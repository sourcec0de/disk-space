//! Snag queue ("Collector") and root-jailed destruction / multi-pass secure erasure.

use std::fs;
use std::io::Seek as _;
use std::io::SeekFrom;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use disk_space_platform::EntryKind;
use disk_space_platform::PlatformHost;
use disk_space_platform::StorageCategory;

use crate::jail::JailError;
use crate::jail::JailedPath;
use crate::jail::ScanRoot;
use crate::tree::NodeId;
use crate::tree::NodeRef;
use crate::tree::StorageTree;

/// Chunk buffer size (64 KiB) used for overwriting regular files during secure erasure.
const WIPE_CHUNK_BYTES: usize = 65_536;

/// Counter ensuring unique randomized filenames before unlinking during secure erasure.
static RENAME_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Number of overwrite passes applied during [`DestructionMode::SecureErase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ErasePasses {
    /// Single zero-fill overwrite pass.
    #[default]
    SingleZero,
    /// Three-pass overwrite (0x00, 0xFF, cryptographic random).
    ThreePassDod,
    /// Seven-pass alternating pattern and random overwrite.
    SevenPass,
}

impl ErasePasses {
    /// Number of overwrite passes performed.
    #[must_use]
    pub const fn count(self) -> u8 {
        match self {
            Self::SingleZero => 1,
            Self::ThreePassDod => 3,
            Self::SevenPass => 7,
        }
    }

    /// Short display label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SingleZero => "1x Zero",
            Self::ThreePassDod => "3x DoD",
            Self::SevenPass => "7x Wipe",
        }
    }
}

/// Destruction strategy selected for the snag queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum DestructionMode {
    /// Move items to the operating system Trash / Recycle Bin.
    #[default]
    MoveToTrash,
    /// Immediately unlink files and directories without moving to Trash.
    PermanentDelete,
    /// Overwrite regular file contents, scrub filenames, and unlink without following symlinks.
    SecureErase {
        /// Number of overwrite passes.
        passes: ErasePasses,
    },
}

/// Single staged target inside [`SnagQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SnagItem {
    /// Node id in the active [`crate::StorageTree`].
    pub node_id: NodeId,
    /// Jailed path proven inside the active [`ScanRoot`].
    pub jailed_path: JailedPath,
    /// Estimated reclaimable allocated bytes.
    pub bytes: u64,
    /// Filesystem entry kind (`File`, `Directory`, etc.).
    pub kind: EntryKind,
    /// Storage category of the item.
    pub category: StorageCategory,
}

/// Result of attempting to add an item to [`SnagQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnagOutcome {
    /// Item was staged; any previously staged descendants were consolidated into it.
    Added {
        /// Number of child entries merged under this ancestor.
        consolidated_children: usize,
    },
    /// Item is already covered by a staged parent directory.
    AlreadyCoveredByAncestor {
        /// Relative path of the covering staged ancestor.
        ancestor: PathBuf,
    },
    /// Exact path was already staged.
    AlreadyPresent,
}

/// Deduplicated list of validated items staged for destruction or secure erasure.
#[derive(Debug, Clone, Default)]
pub struct SnagQueue {
    items: Vec<SnagItem>,
}

impl SnagQueue {
    /// Creates an empty snag queue.
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Stages `node` for destruction after validating it against `root`.
    ///
    /// # Errors
    ///
    /// Returns [`JailError`] if `node` escapes `root`, traverses symlinks, is the root
    /// itself, or hits an OS-protected path.
    pub fn snag(
        &mut self,
        root: &ScanRoot,
        node: NodeRef<'_>,
        host: &dyn PlatformHost,
    ) -> Result<SnagOutcome, JailError> {
        let rel_path = node.relative_path();
        let candidate = if rel_path.as_os_str().is_empty() {
            root.canonical_path().to_path_buf()
        } else {
            root.canonical_path().join(&rel_path)
        };
        let jailed = root.jail_for_destruction(&candidate, host)?;
        let rel = jailed.relative_path();

        for existing in &self.items {
            let existing_rel = existing.jailed_path.relative_path();
            if existing_rel == rel {
                return Ok(SnagOutcome::AlreadyPresent);
            }
            if rel.starts_with(existing_rel) {
                return Ok(SnagOutcome::AlreadyCoveredByAncestor {
                    ancestor: existing_rel.to_path_buf(),
                });
            }
        }

        let before_len = self.items.len();
        self.items
            .retain(|existing| !existing.jailed_path.relative_path().starts_with(rel));
        let consolidated_children = before_len.saturating_sub(self.items.len());

        self.items.push(SnagItem {
            node_id: node.id(),
            jailed_path: jailed,
            bytes: node.total_bytes(),
            kind: node.kind(),
            category: node.category(),
        });

        Ok(SnagOutcome::Added {
            consolidated_children,
        })
    }

    /// Removes the staged item matching `relative_path`, returning `true` if removed.
    pub fn remove(&mut self, relative_path: &Path) -> bool {
        let before = self.items.len();
        self.items
            .retain(|item| item.jailed_path.relative_path() != relative_path);
        self.items.len() != before
    }

    /// Removes all staged items.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Read-only slice of staged items.
    #[must_use]
    pub fn items(&self) -> &[SnagItem] {
        &self.items
    }

    /// Sum of estimated reclaimable bytes across all staged items.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|item| item.bytes).sum()
    }

    /// Returns `true` if `relative_path` itself is staged.
    #[must_use]
    pub fn is_snagged(&self, relative_path: &Path) -> bool {
        self.items
            .iter()
            .any(|item| item.jailed_path.relative_path() == relative_path)
    }

    /// Returns `true` if `relative_path` is staged directly or covered by a staged ancestor.
    #[must_use]
    pub fn is_covered(&self, relative_path: &Path) -> bool {
        self.items
            .iter()
            .any(|item| relative_path.starts_with(item.jailed_path.relative_path()))
    }

    /// Zero-allocation check returning `true` if `node_id` or any ancestor in `tree` is staged.
    #[must_use]
    pub fn is_node_covered(&self, tree: &StorageTree, node_id: NodeId) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let mut cursor = Some(node_id);
        while let Some(curr) = cursor {
            if self.items.iter().any(|item| item.node_id == curr) {
                return true;
            }
            cursor = tree.parent_of(curr);
        }
        false
    }
}

/// Live progress update emitted during [`execute_destruction`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DestructionProgress {
    /// Number of staged top-level items processed so far.
    pub completed_items: usize,
    /// Total staged top-level items.
    pub total_items: usize,
    /// Relative path currently being destroyed.
    pub current_path: String,
    /// Cumulative bytes reclaimed so far.
    pub reclaimed_bytes: u64,
}

/// Outcome report returned by [`execute_destruction`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DestructionReport {
    /// Relative paths successfully destroyed or trashed.
    pub succeeded: Vec<PathBuf>,
    /// Relative paths that failed paired with failure reasons.
    pub failed: Vec<(PathBuf, String)>,
    /// Total allocated bytes reclaimed.
    pub reclaimed_bytes: u64,
}

/// Executes `mode` across `items`, re-validating every item against `root` right before acting.
pub fn execute_destruction(
    root: &ScanRoot,
    items: &[SnagItem],
    mode: DestructionMode,
    host: &dyn PlatformHost,
    on_progress: &mut dyn FnMut(DestructionProgress),
) -> DestructionReport {
    let mut report = DestructionReport::default();
    let total_items = items.len();

    for (idx, item) in items.iter().enumerate() {
        let rel = item.jailed_path.relative_path().to_path_buf();
        on_progress(DestructionProgress {
            completed_items: idx,
            total_items,
            current_path: rel.display().to_string(),
            reclaimed_bytes: report.reclaimed_bytes,
        });

        let revalidated = match root.jail_for_destruction(item.jailed_path.as_path(), host) {
            Ok(jailed) => jailed,
            Err(err) => {
                report.failed.push((rel, err.to_string()));
                continue;
            }
        };

        let result = match mode {
            DestructionMode::MoveToTrash => host
                .move_to_trash(revalidated.as_path())
                .map_err(|err| err.to_string()),
            DestructionMode::PermanentDelete => {
                permanent_delete_no_follow(revalidated.as_path(), host)
            }
            DestructionMode::SecureErase { passes } => {
                secure_erase_no_follow(revalidated.as_path(), passes, host)
            }
        };

        match result {
            Ok(()) => {
                report.succeeded.push(rel);
                report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(item.bytes);
            }
            Err(err) => report.failed.push((rel, err)),
        }
    }

    on_progress(DestructionProgress {
        completed_items: total_items,
        total_items,
        current_path: String::new(),
        reclaimed_bytes: report.reclaimed_bytes,
    });

    report
}

fn permanent_delete_no_follow(path: &Path, host: &dyn PlatformHost) -> Result<(), String> {
    let meta = host.lstat(path).map_err(|err| err.to_string())?;
    match meta.kind {
        EntryKind::Symlink | EntryKind::File | EntryKind::Other => {
            fs::remove_file(path).map_err(|err| err.to_string())
        }
        EntryKind::Directory => {
            let entries = fs::read_dir(path).map_err(|err| err.to_string())?;
            for entry_res in entries {
                let entry = entry_res.map_err(|err| err.to_string())?;
                permanent_delete_no_follow(&entry.path(), host)?;
            }
            fs::remove_dir(path).map_err(|err| err.to_string())
        }
        _ => Err(format!("unsupported entry kind at {}", path.display())),
    }
}

fn secure_erase_no_follow(
    path: &Path,
    passes: ErasePasses,
    host: &dyn PlatformHost,
) -> Result<(), String> {
    let meta = host.lstat(path).map_err(|err| err.to_string())?;
    match meta.kind {
        // Never follow symlinks: unlink the symlink itself without touching its target.
        EntryKind::Symlink | EntryKind::Other => {
            fs::remove_file(path).map_err(|err| err.to_string())
        }
        EntryKind::File => secure_wipe_regular_file(path, passes, host),
        EntryKind::Directory => {
            let entries = fs::read_dir(path).map_err(|err| err.to_string())?;
            for entry_res in entries {
                let entry = entry_res.map_err(|err| err.to_string())?;
                secure_erase_no_follow(&entry.path(), passes, host)?;
            }
            let scrubbed_dir = rename_to_random_sibling(path)?;
            fs::remove_dir(&scrubbed_dir).map_err(|err| err.to_string())
        }
        _ => Err(format!("unsupported entry kind at {}", path.display())),
    }
}

fn secure_wipe_regular_file(
    path: &Path,
    passes: ErasePasses,
    host: &dyn PlatformHost,
) -> Result<(), String> {
    let mut file = host
        .open_regular_nofollow_rw(path)
        .map_err(|err| err.to_string())?;
    let len = file.metadata().map_err(|err| err.to_string())?.len();
    let mut buffer = vec![0_u8; WIPE_CHUNK_BYTES];

    let patterns = pass_schedule(passes);
    for pattern in patterns {
        file.seek(SeekFrom::Start(0))
            .map_err(|err| err.to_string())?;
        let mut remaining = len;
        while remaining > 0 {
            let chunk_len =
                usize::try_from(remaining.min(WIPE_CHUNK_BYTES as u64)).unwrap_or(WIPE_CHUNK_BYTES);
            let slice = &mut buffer[..chunk_len];
            fill_pattern(slice, *pattern)?;
            file.write_all(slice).map_err(|err| err.to_string())?;
            remaining = remaining.saturating_sub(chunk_len as u64);
        }
        file.sync_all().map_err(|err| err.to_string())?;
    }

    file.set_len(0).map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())?;
    drop(file);

    let scrubbed_path = rename_to_random_sibling(path)?;
    fs::remove_file(&scrubbed_path).map_err(|err| err.to_string())
}

#[derive(Clone, Copy)]
enum PassPattern {
    Fixed(u8),
    Random,
}

const fn pass_schedule(passes: ErasePasses) -> &'static [PassPattern] {
    match passes {
        ErasePasses::SingleZero => &[PassPattern::Fixed(0x00)],
        ErasePasses::ThreePassDod => &[
            PassPattern::Fixed(0x00),
            PassPattern::Fixed(0xff),
            PassPattern::Random,
        ],
        ErasePasses::SevenPass => &[
            PassPattern::Fixed(0xf6),
            PassPattern::Fixed(0x00),
            PassPattern::Fixed(0xff),
            PassPattern::Random,
            PassPattern::Fixed(0x00),
            PassPattern::Fixed(0xff),
            PassPattern::Random,
        ],
    }
}

fn fill_pattern(slice: &mut [u8], pattern: PassPattern) -> Result<(), String> {
    match pattern {
        PassPattern::Fixed(byte) => {
            slice.fill(byte);
            Ok(())
        }
        PassPattern::Random => getrandom::fill(slice).map_err(|err| err.to_string()),
    }
}

fn rename_to_random_sibling(path: &Path) -> Result<PathBuf, String> {
    let Some(parent) = path.parent() else {
        return Ok(path.to_path_buf());
    };
    let mut rand_bytes = [0_u8; 8];
    let _ = getrandom::fill(&mut rand_bytes);
    let seq = RENAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    let random_name = format!(
        ".wipe_{:02x}{:02x}{:02x}{:02x}_{seq}",
        rand_bytes[0], rand_bytes[1], rand_bytes[2], rand_bytes[3]
    );
    let dest = parent.join(random_name);
    fs::rename(path, &dest).map_err(|err| err.to_string())?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use disk_space_platform::FakePlatform;

    use super::*;
    use crate::scanner::ScanCancelHandle;
    use crate::scanner::ScanOptions;
    use crate::scanner::scan_root;

    #[test]
    fn snag_consolidates_descendants_and_prevents_redundant_children() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("root");
        let child_dir = root_dir.join("Caches/App");
        fs::create_dir_all(&child_dir).expect("mkdir");
        fs::write(child_dir.join("a.bin"), vec![1_u8; 4096]).expect("write a");

        let host = FakePlatform::new(root_dir.clone());
        let scan_root_val = ScanRoot::new(&root_dir, &host).expect("scan root");
        let tree = scan_root(
            scan_root_val.clone(),
            &host,
            &ScanOptions::default(),
            &ScanCancelHandle::new(),
            &mut |_| {},
        )
        .expect("scan");

        let caches_id = tree.children_sorted(tree.root_id())[0];
        let app_id = tree.children_sorted(caches_id)[0];
        let file_id = tree.children_sorted(app_id)[0];

        let mut queue = SnagQueue::new();
        let file_node = tree.node(file_id).expect("file node");
        assert!(matches!(
            queue
                .snag(&scan_root_val, file_node, &host)
                .expect("snag file"),
            SnagOutcome::Added {
                consolidated_children: 0
            }
        ));

        let caches_node = tree.node(caches_id).expect("caches node");
        assert!(matches!(
            queue
                .snag(&scan_root_val, caches_node, &host)
                .expect("snag parent"),
            SnagOutcome::Added {
                consolidated_children: 1
            }
        ));
        assert_eq!(queue.items().len(), 1);

        let app_node = tree.node(app_id).expect("app node");
        assert!(matches!(
            queue
                .snag(&scan_root_val, app_node, &host)
                .expect("snag child under ancestor"),
            SnagOutcome::AlreadyCoveredByAncestor { .. }
        ));
    }

    #[test]
    fn secure_erase_wipes_files_and_never_touches_symlink_targets() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("jail");
        let victim_dir = root_dir.join("junk");
        let outside_dir = outer.path().join("outside");
        fs::create_dir_all(&victim_dir).expect("mkdir victim");
        fs::create_dir_all(&outside_dir).expect("mkdir outside");

        let secret = victim_dir.join("secret.dat");
        fs::write(&secret, vec![0xaa_u8; 4096]).expect("write secret");
        let outside_file = outside_dir.join("preserved.txt");
        fs::write(&outside_file, b"untouched").expect("write outside");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, victim_dir.join("ext_ref")).expect("symlink");

        let host = FakePlatform::new(root_dir.clone());
        let scan_root_val = ScanRoot::new(&root_dir, &host).expect("scan root");
        let tree = scan_root(
            scan_root_val.clone(),
            &host,
            &ScanOptions::default(),
            &ScanCancelHandle::new(),
            &mut |_| {},
        )
        .expect("scan");

        let junk_id = tree.children_sorted(tree.root_id())[0];
        let mut queue = SnagQueue::new();
        queue
            .snag(&scan_root_val, tree.node(junk_id).expect("junk"), &host)
            .expect("snag junk");

        let report = execute_destruction(
            &scan_root_val,
            queue.items(),
            DestructionMode::SecureErase {
                passes: ErasePasses::ThreePassDod,
            },
            &host,
            &mut |_| {},
        );

        assert_eq!(report.succeeded.len(), 1);
        assert!(report.failed.is_empty());
        assert!(!victim_dir.exists());
        assert_eq!(
            fs::read(&outside_file).expect("outside preserved"),
            b"untouched"
        );
    }
}
