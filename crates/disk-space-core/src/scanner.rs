//! Bounded parallel LIFO DFS root-jailed filesystem scanner.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use disk_space_platform::DeviceId;
use disk_space_platform::EntryKind;
use disk_space_platform::JunkAssessment;
use disk_space_platform::JunkKind;
use disk_space_platform::PlatformHost;
use disk_space_platform::StorageCategory;

use crate::jail::CrossMountPolicy;
use crate::jail::JailError;
use crate::jail::ScanRoot;
use crate::tree::CategoryTotals;
use crate::tree::CompactNode;
use crate::tree::DirData;
use crate::tree::NodeId;
use crate::tree::StorageTree;
use crate::tree::encode_modified_secs;

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

struct ScanArena {
    nodes: Vec<CompactNode>,
    dirs: Vec<DirData>,
    name_arena: String,
}

impl ScanArena {
    #[expect(
        clippy::too_many_arguments,
        reason = "internal helper writes a single node directly into the compact arena"
    )]
    fn push_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        bytes: u64,
        modified: Option<SystemTime>,
        kind: EntryKind,
        category: StorageCategory,
        junk_kind: Option<JunkKind>,
        review_first: bool,
    ) -> NodeId {
        let id = NodeId::new(self.nodes.len());
        let name_slice = clamp_name_u16(name);
        let name_start = self.name_arena.len();
        self.name_arena.push_str(name_slice);
        let name_len = u16::try_from(name_slice.len()).unwrap_or(u16::MAX);
        let mod_secs = encode_modified_secs(modified);

        let aux = if kind == EntryKind::Directory {
            let dir_idx = u32::try_from(self.dirs.len()).unwrap_or(u32::MAX);
            self.dirs.push(DirData {
                modified_secs: mod_secs,
                ..DirData::default()
            });
            dir_idx
        } else {
            mod_secs
        };

        self.nodes.push(CompactNode::new(
            bytes,
            parent,
            name_start,
            name_len,
            aux,
            kind,
            category,
            junk_kind,
            review_first,
        ));
        id
    }

    fn commit_batch(
        &mut self,
        parent_id: NodeId,
        staged_children: Vec<StagedChild>,
    ) -> Vec<NodeId> {
        let mut subdir_ids = Vec::new();
        for child in staged_children {
            let is_dir = child.kind == EntryKind::Directory;
            let child_id = self.push_node(
                Some(parent_id),
                &child.name,
                child.own_bytes,
                child.modified,
                child.kind,
                child.category,
                child.junk_kind,
                child.review_first,
            );
            if is_dir {
                subdir_ids.push(child_id);
            }
        }
        subdir_ids
    }

    fn build_abs_path(&self, root_abs: &Path, id: NodeId) -> PathBuf {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            if curr == NodeId::new(0) {
                break;
            }
            let Some(node) = self.nodes.get(curr.index()) else {
                break;
            };
            chain.push(node.name_range());
            cursor = node.parent();
        }
        let mut out = root_abs.to_path_buf();
        for (start, end) in chain.into_iter().rev() {
            if let Some(seg) = self.name_arena.get(start..end) {
                out.push(seg);
            }
        }
        out
    }
}

fn clamp_name_u16(name: &str) -> &str {
    let max_len = usize::from(u16::MAX);
    if name.len() <= max_len {
        return name;
    }
    let mut end = max_len;
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &name[..end]
}

struct StagedChild {
    name: String,
    kind: EntryKind,
    own_bytes: u64,
    modified: Option<SystemTime>,
    category: StorageCategory,
    junk_kind: Option<JunkKind>,
    review_first: bool,
}

struct SharedScanState<'a> {
    root: &'a ScanRoot,
    host: &'a dyn PlatformHost,
    options: &'a ScanOptions,
    cancel: &'a ScanCancelHandle,
    now: SystemTime,
    arena: Mutex<ScanArena>,
    work_stack: Mutex<Vec<NodeId>>,
    work_condvar: Condvar,
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

    let mut initial_arena = ScanArena {
        nodes: Vec::with_capacity(4096),
        dirs: Vec::with_capacity(512),
        name_arena: String::with_capacity(64 * 1024),
    };
    let root_id = initial_arena.push_node(
        None,
        root.display_name(),
        0,
        root_meta.modified,
        EntryKind::Directory,
        root_cat,
        root_junk.kind,
        root_junk.review_first,
    );

    let state = SharedScanState {
        root: &root,
        host,
        options,
        cancel,
        now,
        arena: Mutex::new(initial_arena),
        work_stack: Mutex::new(vec![root_id]),
        work_condvar: Condvar::new(),
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

    let worker_count = options.worker_threads.max(1);
    let interval = Duration::from_millis(options.progress_interval_ms.max(20));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let state_ref = &state;
            scope.spawn(move || worker_loop(state_ref));
        }

        let mut last_emit = Instant::now();
        while state.pending_tasks.load(Ordering::Acquire) > 0 && !cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(25));
            if last_emit.elapsed() >= interval {
                on_progress(snapshot_progress(&state));
                last_emit = Instant::now();
            }
        }
        if cancel.is_cancelled() {
            state.work_condvar.notify_all();
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
    let SharedScanState {
        arena,
        work_stack,
        hardlinks,
        category_totals,
        current_path,
        ..
    } = state;
    drop(work_stack);
    drop(hardlinks);
    drop(category_totals);
    drop(current_path);

    let extracted = arena
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    Ok(StorageTree::from_raw(
        root,
        volume,
        extracted.nodes,
        extracted.dirs,
        extracted.name_arena,
        unreadable_dirs,
        skipped_symlinks,
        skipped_cross_mount,
        hardlink_savings,
        SystemTime::now(),
    ))
}

fn worker_loop(state: &SharedScanState<'_>) {
    loop {
        if state.cancel.is_cancelled() {
            break;
        }
        let next_dir = {
            let mut stack = state
                .work_stack
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(dir_id) = stack.pop() {
                    break Some(dir_id);
                }
                if state.cancel.is_cancelled() || state.pending_tasks.load(Ordering::Acquire) == 0 {
                    break None;
                }
                let (guard, _) = state
                    .work_condvar
                    .wait_timeout(stack, Duration::from_millis(25))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                stack = guard;
            }
        };

        let Some(dir_id) = next_dir else {
            break;
        };
        process_directory(state, dir_id);
        if state.pending_tasks.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _guard = state.work_stack.lock();
            state.work_condvar.notify_all();
            break;
        }
    }
}

fn process_directory(state: &SharedScanState<'_>, dir_id: NodeId) {
    let abs_path = {
        let arena = state
            .arena
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        arena.build_abs_path(state.root.canonical_path(), dir_id)
    };

    if let Ok(mut current) = state.current_path.try_lock() {
        *current = abs_path.display().to_string();
    }

    let Ok(entries) = fs::read_dir(&abs_path) else {
        state.unreadable_dirs.fetch_add(1, Ordering::Relaxed);
        return;
    };

    let mut staged_children: Vec<StagedChild> = Vec::new();
    let mut local_files = 0_u64;
    let mut local_dirs = 0_u64;
    let mut local_bytes = 0_u64;
    let mut local_cat_totals = CategoryTotals::default();

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
            &entry,
            &mut staged_children,
            &mut local_files,
            &mut local_dirs,
            &mut local_bytes,
            &mut local_cat_totals,
        );
    }

    if local_files > 0 {
        state
            .scanned_files
            .fetch_add(local_files, Ordering::Relaxed);
    }
    if local_dirs > 0 {
        state.scanned_dirs.fetch_add(local_dirs, Ordering::Relaxed);
    }
    if local_bytes > 0 {
        state
            .scanned_bytes
            .fetch_add(local_bytes, Ordering::Relaxed);
        if let Ok(mut totals) = state.category_totals.lock() {
            totals.add_all(&local_cat_totals);
        }
    }

    commit_directory_children(state, dir_id, staged_children);
}

fn inspect_dir_entry(
    state: &SharedScanState<'_>,
    entry: &fs::DirEntry,
    staged_children: &mut Vec<StagedChild>,
    local_files: &mut u64,
    local_dirs: &mut u64,
    local_bytes: &mut u64,
    local_cat_totals: &mut CategoryTotals,
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
    let (cat, junk): (StorageCategory, JunkAssessment) =
        state.host.classify(&child_abs, meta.kind, &meta, state.now);
    let own_bytes = compute_allocated_bytes(state, &meta);

    if meta.kind == EntryKind::Directory {
        *local_dirs = local_dirs.saturating_add(1);
    } else {
        *local_files = local_files.saturating_add(1);
        *local_bytes = local_bytes.saturating_add(own_bytes);
        if own_bytes > 0 {
            local_cat_totals.add(cat, own_bytes);
        }
    }

    staged_children.push(StagedChild {
        name: child_name,
        kind: meta.kind,
        own_bytes,
        modified: meta.modified,
        category: cat,
        junk_kind: junk.kind,
        review_first: junk.review_first,
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
    parent_id: NodeId,
    staged_children: Vec<StagedChild>,
) {
    if staged_children.is_empty() {
        return;
    }

    let subdir_ids = state
        .arena
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .commit_batch(parent_id, staged_children);

    if !subdir_ids.is_empty() {
        state
            .pending_tasks
            .fetch_add(subdir_ids.len(), Ordering::AcqRel);
        state
            .work_stack
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(subdir_ids.into_iter().rev());
        state.work_condvar.notify_all();
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
        assert!(root_node.total_bytes() < 65_536);
        assert!(root_node.largest_culprit().is_some());

        // Pruning removes cache entry and drops root bytes cleanly.
        let before = root_node.total_bytes();
        let rel = PathBuf::from("Library/Caches/TestApp/blob_hardlink.cache");
        let rel_orig = PathBuf::from("Library/Caches/TestApp/blob.cache");
        tree.prune_deleted(&[rel, rel_orig]);
        let after = tree.node(tree.root_id()).expect("root node").total_bytes();
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
