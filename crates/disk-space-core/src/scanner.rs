//! Bounded parallel root-jailed filesystem scanner.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use disk_space_platform::DeviceId;
use disk_space_platform::EntryKind;
use disk_space_platform::JunkAssessment;
use disk_space_platform::PlatformHost;
use disk_space_platform::StorageCategory;

use crate::jail::CrossMountPolicy;
use crate::jail::JailError;
use crate::jail::ScanRoot;
use crate::tree::CategoryTotals;
use crate::tree::DiskNode;
use crate::tree::NodeId;
use crate::tree::StorageTree;

/// Configuration options for [`scan_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanOptions {
    /// Device boundary enforcement mode.
    pub cross_mount: CrossMountPolicy,
    /// Number of worker threads used for parallel metadata inspection.
    pub worker_threads: usize,
    /// Minimum milliseconds between progress callbacks.
    pub progress_interval_ms: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        let threads = thread::available_parallelism().map_or(4, |count| count.get().min(8));
        Self {
            cross_mount: CrossMountPolicy::StayOnRootDevice,
            worker_threads: threads,
            progress_interval_ms: 80,
        }
    }
}

/// Live progress snapshot emitted while scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanProgress {
    /// Number of files inspected so far.
    pub scanned_files: u64,
    /// Number of directories inspected so far.
    pub scanned_dirs: u64,
    /// Total allocated bytes encountered so far.
    pub scanned_bytes: u64,
    /// Count of unreadable directories encountered so far.
    pub unreadable_dirs: usize,
    /// Count of symlinks skipped so far.
    pub skipped_symlinks: usize,
    /// Most recently visited path label.
    pub current_path: String,
    /// Category breakdown accumulated so far.
    pub category_totals: CategoryTotals,
}

/// Cooperative cancellation handle for an active scan.
#[derive(Debug, Clone, Default)]
pub struct ScanCancelHandle {
    cancelled: Arc<AtomicBool>,
}

impl ScanCancelHandle {
    /// Creates a fresh non-cancelled handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation of the scan associated with this handle.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Returns `true` if cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

struct DirTask {
    node_id: NodeId,
    abs_path: PathBuf,
    rel_path: PathBuf,
}

struct SharedScanState<'a> {
    root: &'a ScanRoot,
    host: &'a dyn PlatformHost,
    options: &'a ScanOptions,
    cancel: &'a ScanCancelHandle,
    now: SystemTime,
    nodes: Mutex<Vec<DiskNode>>,
    hardlinks: Mutex<HashSet<(DeviceId, u64)>>,
    category_totals: Mutex<CategoryTotals>,
    current_path: Mutex<String>,
    scanned_files: AtomicU64,
    scanned_dirs: AtomicU64,
    scanned_bytes: AtomicU64,
    unreadable_dirs: AtomicUsize,
    skipped_symlinks: AtomicUsize,
    skipped_cross_mount: AtomicUsize,
    hardlink_savings: AtomicU64,
    pending_tasks: AtomicUsize,
}

/// Scans `root` strictly within its directory and mount boundaries.
///
/// # Errors
///
/// Returns [`JailError::Cancelled`] if cancelled via `cancel`, or [`JailError::Platform`]
/// if querying volume metadata fails.
pub fn scan_root(
    root: ScanRoot,
    host: &dyn PlatformHost,
    options: &ScanOptions,
    cancel: &ScanCancelHandle,
    on_progress: &mut dyn FnMut(ScanProgress),
) -> Result<StorageTree, JailError> {
    let volume = host.volume_for_path(root.canonical_path())?;
    let now = SystemTime::now();
    let root_meta = host.lstat(root.canonical_path())?;
    let (root_cat, root_junk) =
        host.classify(root.canonical_path(), EntryKind::Directory, &root_meta, now);

    let root_node = DiskNode {
        id: NodeId::new(0),
        parent: None,
        children: Vec::new(),
        name: root.display_name().to_owned(),
        relative_path: PathBuf::new(),
        kind: EntryKind::Directory,
        own_bytes: 0,
        total_bytes: 0,
        logical_bytes: 0,
        file_count: 0,
        dir_count: 1,
        category: root_cat,
        category_totals: CategoryTotals::default(),
        modified: root_meta.modified,
        accessed: root_meta.accessed,
        unused_bytes: 0,
        junk_kind: root_junk.kind,
        review_first: root_junk.review_first,
        largest_culprit: None,
        removed: false,
    };

    let state = SharedScanState {
        root: &root,
        host,
        options,
        cancel,
        now,
        nodes: Mutex::new(vec![root_node]),
        hardlinks: Mutex::new(HashSet::new()),
        category_totals: Mutex::new(CategoryTotals::default()),
        current_path: Mutex::new(root.canonical_path().display().to_string()),
        scanned_files: AtomicU64::new(0),
        scanned_dirs: AtomicU64::new(1),
        scanned_bytes: AtomicU64::new(0),
        unreadable_dirs: AtomicUsize::new(0),
        skipped_symlinks: AtomicUsize::new(0),
        skipped_cross_mount: AtomicUsize::new(0),
        hardlink_savings: AtomicU64::new(0),
        pending_tasks: AtomicUsize::new(1),
    };

    let (tx, rx) = crossbeam_channel::unbounded::<DirTask>();
    let _ = tx.send(DirTask {
        node_id: NodeId::new(0),
        abs_path: root.canonical_path().to_path_buf(),
        rel_path: PathBuf::new(),
    });

    let worker_count = options.worker_threads.max(1);
    let interval = Duration::from_millis(options.progress_interval_ms.max(20));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let worker_tx = tx.clone();
            let worker_rx = rx.clone();
            let state_ref = &state;
            scope.spawn(move || worker_loop(state_ref, &worker_tx, &worker_rx));
        }

        drop(tx);
        drop(rx);

        let mut last_emit = Instant::now();
        while state.pending_tasks.load(Ordering::Acquire) > 0 && !cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(25));
            if last_emit.elapsed() >= interval {
                on_progress(snapshot_progress(&state));
                last_emit = Instant::now();
            }
        }
    });

    if cancel.is_cancelled() {
        return Err(JailError::Cancelled);
    }

    on_progress(snapshot_progress(&state));

    let unreadable_dirs = state.unreadable_dirs.load(Ordering::Relaxed);
    let skipped_symlinks = state.skipped_symlinks.load(Ordering::Relaxed);
    let skipped_cross_mount = state.skipped_cross_mount.load(Ordering::Relaxed);
    let hardlink_savings = state.hardlink_savings.load(Ordering::Relaxed);
    let nodes = state
        .nodes
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    Ok(StorageTree::from_raw(
        root,
        volume,
        nodes,
        unreadable_dirs,
        skipped_symlinks,
        skipped_cross_mount,
        hardlink_savings,
        SystemTime::now(),
    ))
}

fn worker_loop(state: &SharedScanState<'_>, tx: &Sender<DirTask>, rx: &Receiver<DirTask>) {
    loop {
        if state.cancel.is_cancelled() || state.pending_tasks.load(Ordering::Acquire) == 0 {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(task) => {
                process_directory(state, tx, &task);
                if state.pending_tasks.fetch_sub(1, Ordering::AcqRel) == 1 {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn process_directory(state: &SharedScanState<'_>, tx: &Sender<DirTask>, task: &DirTask) {
    if let Ok(mut current) = state.current_path.lock() {
        *current = task.abs_path.display().to_string();
    }

    let Ok(entries) = fs::read_dir(&task.abs_path) else {
        state.unreadable_dirs.fetch_add(1, Ordering::Relaxed);
        return;
    };

    let mut staged_children: Vec<DiskNode> = Vec::new();
    let mut subdirs_to_queue: Vec<(usize, PathBuf, PathBuf)> = Vec::new();

    for entry_result in entries {
        if state.cancel.is_cancelled() {
            return;
        }
        let Ok(entry) = entry_result else {
            state.unreadable_dirs.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        inspect_dir_entry(
            state,
            task,
            &entry,
            &mut staged_children,
            &mut subdirs_to_queue,
        );
    }

    commit_directory_children(state, tx, task.node_id, staged_children, subdirs_to_queue);
}

fn inspect_dir_entry(
    state: &SharedScanState<'_>,
    parent_task: &DirTask,
    entry: &fs::DirEntry,
    staged_children: &mut Vec<DiskNode>,
    subdirs_to_queue: &mut Vec<(usize, PathBuf, PathBuf)>,
) {
    let child_abs = entry.path();
    if state
        .host
        .should_skip_scan_entry(&child_abs, state.root.canonical_path())
    {
        return;
    }

    let Ok(meta) = state.host.lstat(&child_abs) else {
        state.unreadable_dirs.fetch_add(1, Ordering::Relaxed);
        return;
    };

    if meta.kind == EntryKind::Symlink {
        state.skipped_symlinks.fetch_add(1, Ordering::Relaxed);
        return;
    }

    if state.options.cross_mount == CrossMountPolicy::StayOnRootDevice
        && meta.device_id != state.root.device_id()
    {
        state.skipped_cross_mount.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let child_name = entry.file_name().to_string_lossy().into_owned();
    let child_rel = parent_task.rel_path.join(&child_name);

    let (cat, junk): (StorageCategory, JunkAssessment) =
        state.host.classify(&child_abs, meta.kind, &meta, state.now);

    let own_bytes = compute_allocated_bytes(state, &meta);

    if meta.kind == EntryKind::Directory {
        state.scanned_dirs.fetch_add(1, Ordering::Relaxed);
        let local_idx = staged_children.len();
        subdirs_to_queue.push((local_idx, child_abs, child_rel.clone()));
    } else {
        state.scanned_files.fetch_add(1, Ordering::Relaxed);
        state.scanned_bytes.fetch_add(own_bytes, Ordering::Relaxed);
        if let Ok(mut totals) = state.category_totals.lock() {
            totals.add(cat, own_bytes);
        }
    }

    staged_children.push(DiskNode {
        id: NodeId::new(0),
        parent: Some(parent_task.node_id),
        children: Vec::new(),
        name: child_name,
        relative_path: child_rel,
        kind: meta.kind,
        own_bytes,
        total_bytes: own_bytes,
        logical_bytes: meta.logical_bytes,
        file_count: u64::from(meta.kind == EntryKind::File),
        dir_count: u64::from(meta.kind == EntryKind::Directory),
        category: cat,
        category_totals: CategoryTotals::default(),
        modified: meta.modified,
        accessed: meta.accessed,
        unused_bytes: if junk.kind.is_some() { own_bytes } else { 0 },
        junk_kind: junk.kind,
        review_first: junk.review_first,
        largest_culprit: None,
        removed: false,
    });
}

fn compute_allocated_bytes(
    state: &SharedScanState<'_>,
    meta: &disk_space_platform::EntryMetadata,
) -> u64 {
    if meta.kind == EntryKind::Directory {
        return 0;
    }
    if meta.link_count > 1 && meta.inode != 0 {
        let is_first_seen = state
            .hardlinks
            .lock()
            .map_or(true, |mut set| set.insert((meta.device_id, meta.inode)));
        if is_first_seen {
            meta.allocated_bytes
        } else {
            state
                .hardlink_savings
                .fetch_add(meta.allocated_bytes, Ordering::Relaxed);
            0
        }
    } else {
        meta.allocated_bytes
    }
}

fn commit_directory_children(
    state: &SharedScanState<'_>,
    tx: &Sender<DirTask>,
    parent_id: NodeId,
    staged_children: Vec<DiskNode>,
    subdirs_to_queue: Vec<(usize, PathBuf, PathBuf)>,
) {
    if staged_children.is_empty() {
        return;
    }

    let mut queued_tasks = Vec::with_capacity(subdirs_to_queue.len());
    let mut nodes = state
        .nodes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let base_idx = nodes.len();
    let mut child_ids = Vec::with_capacity(staged_children.len());

    for (offset, mut child_node) in staged_children.into_iter().enumerate() {
        let new_id = NodeId::new(base_idx + offset);
        child_node.id = new_id;
        child_ids.push(new_id);
        nodes.push(child_node);
    }

    nodes[parent_id.index()].children.extend(child_ids);
    drop(nodes);

    for (local_idx, abs_path, rel_path) in subdirs_to_queue {
        queued_tasks.push(DirTask {
            node_id: NodeId::new(base_idx + local_idx),
            abs_path,
            rel_path,
        });
    }

    if !queued_tasks.is_empty() {
        state
            .pending_tasks
            .fetch_add(queued_tasks.len(), Ordering::AcqRel);
        for subdir in queued_tasks {
            let _ = tx.send(subdir);
        }
    }
}

fn snapshot_progress(state: &SharedScanState<'_>) -> ScanProgress {
    let totals = state
        .category_totals
        .lock()
        .map(|guard| *guard)
        .unwrap_or_default();
    let current_path = state
        .current_path
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    ScanProgress {
        scanned_files: state.scanned_files.load(Ordering::Relaxed),
        scanned_dirs: state.scanned_dirs.load(Ordering::Relaxed),
        scanned_bytes: state.scanned_bytes.load(Ordering::Relaxed),
        unreadable_dirs: state.unreadable_dirs.load(Ordering::Relaxed),
        skipped_symlinks: state.skipped_symlinks.load(Ordering::Relaxed),
        current_path,
        category_totals: totals,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use disk_space_platform::FakePlatform;

    use super::*;

    struct RestorePermGuard {
        path: PathBuf,
    }

    impl Drop for RestorePermGuard {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
        }
    }

    #[test]
    fn scans_hierarchy_dedups_hardlinks_skips_symlinks_and_counts_unreadable() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("scan");
        let cache_dir = root_dir.join("Library/Caches/TestApp");
        let unreadable_dir = root_dir.join("locked");
        let outside_dir = outer.path().join("outside");
        fs::create_dir_all(&cache_dir).expect("create cache");
        fs::create_dir_all(&unreadable_dir).expect("create locked");
        fs::create_dir_all(&outside_dir).expect("create outside");

        let big_cache = cache_dir.join("blob.cache");
        fs::write(&big_cache, vec![0xab; 16_384]).expect("write cache");
        let hardlink = cache_dir.join("blob_hardlink.cache");
        fs::hard_link(&big_cache, &hardlink).expect("hardlink");

        let outside_big = outside_dir.join("huge.bin");
        fs::write(&outside_big, vec![0xcd; 65_536]).expect("write outside");
        std::os::unix::fs::symlink(&outside_dir, root_dir.join("ext_link")).expect("symlink");

        fs::set_permissions(&unreadable_dir, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let _guard = RestorePermGuard {
            path: unreadable_dir,
        };

        let host = FakePlatform::new(root_dir.clone());
        let scan_root = ScanRoot::new(&root_dir, &host).expect("scan root");
        let cancel = ScanCancelHandle::new();

        let mut tree = scan_root_run(scan_root, &host, &cancel);
        assert_eq!(tree.skipped_symlinks(), 1);
        assert_eq!(tree.unreadable_dirs(), 1);
        assert!(tree.hardlink_savings_bytes() > 0);

        let root_node = tree.node(tree.root_id()).expect("root node");
        assert!(root_node.total_bytes < 65_536);
        assert!(root_node.largest_culprit.is_some());

        // Pruning removes cache entry and drops root bytes cleanly.
        let before = root_node.total_bytes;
        let rel = PathBuf::from("Library/Caches/TestApp/blob_hardlink.cache");
        let rel_orig = PathBuf::from("Library/Caches/TestApp/blob.cache");
        tree.prune_deleted(&[rel, rel_orig]);
        let after = tree.node(tree.root_id()).expect("root node").total_bytes;
        assert!(after < before);
    }

    fn scan_root_run(
        root: ScanRoot,
        host: &FakePlatform,
        cancel: &ScanCancelHandle,
    ) -> StorageTree {
        scan_root(root, host, &ScanOptions::default(), cancel, &mut |_| {}).expect("scan ok")
    }
}
