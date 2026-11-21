//! Compact arena storage tree, per-category byte aggregation, culprit summaries, and pruning.

use std::ffi::OsStr;
use std::mem::size_of;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use disk_space_platform::EntryKind;
use disk_space_platform::JunkKind;
use disk_space_platform::StorageCategory;
use disk_space_platform::VolumeInfo;

use crate::jail::ScanRoot;

/// Sentinel stored in [`CompactNode::parent`] when a node has no parent (root).
const NONE_NODE: u32 = u32::MAX;

/// Mask for the 2-bit [`EntryKind`] field inside [`CompactNode::flags`].
const KIND_MASK: u16 = 0b11;
/// Bit offset of [`StorageCategory`] inside [`CompactNode::flags`].
const CATEGORY_SHIFT: u32 = 2;
/// Mask for the 4-bit [`StorageCategory`] field (unshifted).
const CATEGORY_MASK: u16 = 0b1111;
/// Bit offset of [`JunkKind`] inside [`CompactNode::flags`].
const JUNK_SHIFT: u32 = 6;
/// Mask for the 3-bit [`JunkKind`] field (unshifted).
const JUNK_MASK: u16 = 0b111;
/// Bit flag for `review_first` inside [`CompactNode::flags`].
const REVIEW_FIRST_BIT: u16 = 1 << 9;
/// Bit flag for soft-deleted (`removed`) inside [`CompactNode::flags`].
const REMOVED_BIT: u16 = 1 << 10;
/// Bit offset of the upper 5 bits of `name_start` inside [`CompactNode::flags`].
const NAME_HI_SHIFT: u32 = 11;
/// Mask for the upper 5 bits of `name_start` (unshifted).
const NAME_HI_MASK: u16 = 0b1_1111;

/// Compact node index inside [`StorageTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// Creates a [`NodeId`] from a vector index.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "node arena holds fewer than u32::MAX entries"
    )]
    pub(crate) const fn new(index: usize) -> Self {
        Self(index as u32)
    }

    /// Creates a [`NodeId`] from a raw `u32` index.
    #[must_use]
    pub(crate) const fn from_u32(raw: u32) -> Self {
        Self(raw)
    }

    /// Raw index into the arena.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Raw underlying `u32` identifier.
    #[must_use]
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
}

/// Aggregated byte count across the 9 [`StorageCategory`] buckets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CategoryTotals([u64; 9]);

impl CategoryTotals {
    /// Adds `bytes` to `category`.
    pub const fn add(&mut self, category: StorageCategory, bytes: u64) {
        let idx = category.index();
        self.0[idx] = self.0[idx].saturating_add(bytes);
    }

    /// Accumulates another [`CategoryTotals`] into `self`.
    pub fn add_all(&mut self, other: &Self) {
        for (dst, src) in self.0.iter_mut().zip(other.0.iter()) {
            *dst = dst.saturating_add(*src);
        }
    }

    /// Returns bytes attributed to `category`.
    #[must_use]
    pub const fn get(&self, category: StorageCategory) -> u64 {
        self.0[category.index()]
    }

    /// Returns the total bytes summed across all categories.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.iter().copied().sum()
    }

    /// Returns the dominant [`StorageCategory`] by highest byte share.
    #[must_use]
    pub fn dominant(&self) -> StorageCategory {
        let mut best_cat = StorageCategory::Documents;
        let mut best_bytes = 0_u64;
        for category in StorageCategory::ALL {
            let val = self.get(category);
            if val > best_bytes {
                best_bytes = val;
                best_cat = category;
            }
        }
        best_cat
    }
}

/// Summary of the heaviest descendant inside a folder (`mostly in ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CulpritSummary {
    /// Target node responsible for the majority of space inside the parent.
    pub target_id: NodeId,
    /// Relative subpath beneath the parent folder (e.g. `Default/Service Worker/CacheStorage/`).
    pub relative_subpath: String,
    /// Bytes contained by the culprit node.
    pub bytes: u64,
    /// Share of the parent folder (`0..=100`).
    pub share_percent: u8,
}

impl CulpritSummary {
    /// Constructs a [`CulpritSummary`].
    #[must_use]
    pub const fn new(
        target_id: NodeId,
        relative_subpath: String,
        bytes: u64,
        share_percent: u8,
    ) -> Self {
        Self {
            target_id,
            relative_subpath,
            bytes,
            share_percent,
        }
    }
}

/// Fixed 24-byte arena entry for any file, directory, symlink, or special entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompactNode {
    /// `own_bytes` for non-directories; recursive `total_bytes` for directories.
    bytes: u64,
    /// Parent [`NodeId`] raw index, or [`NONE_NODE`] for root.
    parent: u32,
    /// Low 32 bits of the UTF-8 byte offset inside [`StorageTree::name_arena`].
    name_start_lo: u32,
    /// Index into [`StorageTree::dirs`] for directories; encoded mtime seconds otherwise.
    aux: u32,
    /// UTF-8 byte length of the basename inside `name_arena`.
    name_len: u16,
    /// Bit-packed kind, category, junk kind, review/removed flags, and high 5 bits of `name_start`.
    flags: u16,
}

impl CompactNode {
    /// Constructs a [`CompactNode`] from its decoded components.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        clippy::cast_possible_truncation,
        reason = "compact bitfield constructor maps scanner fields directly into 24 bytes"
    )]
    pub(crate) fn new(
        bytes: u64,
        parent: Option<NodeId>,
        name_start: usize,
        name_len: u16,
        aux: u32,
        kind: EntryKind,
        category: StorageCategory,
        junk_kind: Option<JunkKind>,
        review_first: bool,
    ) -> Self {
        let start_u64 = name_start as u64;
        let name_start_lo = (start_u64 & 0xFFFF_FFFF) as u32;
        let name_start_hi = ((start_u64 >> 32) as u16) & NAME_HI_MASK;
        let flags = encode_kind(kind)
            | ((encode_category(category) & CATEGORY_MASK) << CATEGORY_SHIFT)
            | ((encode_junk(junk_kind) & JUNK_MASK) << JUNK_SHIFT)
            | if review_first { REVIEW_FIRST_BIT } else { 0 }
            | (name_start_hi << NAME_HI_SHIFT);

        Self {
            bytes,
            parent: parent.map_or(NONE_NODE, NodeId::raw),
            name_start_lo,
            aux,
            name_len,
            flags,
        }
    }

    #[must_use]
    pub(crate) const fn kind(self) -> EntryKind {
        decode_kind(self.flags & KIND_MASK)
    }

    #[must_use]
    pub(crate) const fn category(self) -> StorageCategory {
        decode_category((self.flags >> CATEGORY_SHIFT) & CATEGORY_MASK)
    }

    pub(crate) const fn set_category(&mut self, category: StorageCategory) {
        let cleared = self.flags & !(CATEGORY_MASK << CATEGORY_SHIFT);
        self.flags = cleared | ((encode_category(category) & CATEGORY_MASK) << CATEGORY_SHIFT);
    }

    #[must_use]
    pub(crate) const fn junk_kind(self) -> Option<JunkKind> {
        decode_junk((self.flags >> JUNK_SHIFT) & JUNK_MASK)
    }

    #[must_use]
    pub(crate) const fn review_first(self) -> bool {
        (self.flags & REVIEW_FIRST_BIT) != 0
    }

    pub(crate) const fn set_review_first(&mut self, review_first: bool) {
        if review_first {
            self.flags |= REVIEW_FIRST_BIT;
        } else {
            self.flags &= !REVIEW_FIRST_BIT;
        }
    }

    #[must_use]
    pub(crate) const fn is_removed(self) -> bool {
        (self.flags & REMOVED_BIT) != 0
    }

    pub(crate) const fn set_removed(&mut self, removed: bool) {
        if removed {
            self.flags |= REMOVED_BIT;
        } else {
            self.flags &= !REMOVED_BIT;
        }
    }

    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "encoded name byte offsets fit platform address space"
    )]
    pub(crate) const fn name_range(self) -> (usize, usize) {
        let hi = ((self.flags >> NAME_HI_SHIFT) & NAME_HI_MASK) as u64;
        let start = ((hi << 32) | (self.name_start_lo as u64)) as usize;
        let end = start + (self.name_len as usize);
        (start, end)
    }

    #[must_use]
    pub(crate) const fn parent(self) -> Option<NodeId> {
        if self.parent == NONE_NODE {
            None
        } else {
            Some(NodeId::from_u32(self.parent))
        }
    }

    #[must_use]
    pub(crate) const fn dir_idx(self) -> Option<usize> {
        if matches!(self.kind(), EntryKind::Directory) {
            Some(self.aux as usize)
        } else {
            None
        }
    }

    #[must_use]
    pub(crate) const fn modified_secs_leaf(self) -> u32 {
        self.aux
    }
}

/// Directory-only metadata (~10% of filesystem entries) stored separately from leaf files.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DirData {
    pub(crate) children_start: u32,
    pub(crate) children_len: u32,
    pub(crate) modified_secs: u32,
    pub(crate) file_count: u32,
    pub(crate) dir_count: u32,
    pub(crate) unused_bytes: u64,
    pub(crate) category_totals: CategoryTotals,
}

const fn encode_kind(kind: EntryKind) -> u16 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other | _ => 3,
    }
}

const fn decode_kind(bits: u16) -> EntryKind {
    match bits {
        0 => EntryKind::File,
        1 => EntryKind::Directory,
        2 => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "StorageCategory has 9 variants cleanly fitting u16"
)]
const fn encode_category(category: StorageCategory) -> u16 {
    category.index() as u16
}

const fn decode_category(bits: u16) -> StorageCategory {
    let idx = bits as usize;
    if idx < StorageCategory::ALL.len() {
        StorageCategory::ALL[idx]
    } else {
        StorageCategory::Documents
    }
}

const fn encode_junk(junk: Option<JunkKind>) -> u16 {
    match junk {
        None => 0,
        Some(JunkKind::Cache) => 1,
        Some(JunkKind::BuildArtifact) => 2,
        Some(JunkKind::StaleUnused) => 3,
        Some(JunkKind::TrashItem) => 4,
        Some(JunkKind::InstallerPackage | _) => 5,
    }
}

const fn decode_junk(bits: u16) -> Option<JunkKind> {
    match bits {
        1 => Some(JunkKind::Cache),
        2 => Some(JunkKind::BuildArtifact),
        3 => Some(JunkKind::StaleUnused),
        4 => Some(JunkKind::TrashItem),
        5 => Some(JunkKind::InstallerPackage),
        _ => None,
    }
}

/// Encodes `SystemTime` as nonzero seconds since `UNIX_EPOCH` (`0` represents `None`).
#[must_use]
pub(crate) fn encode_modified_secs(time: Option<SystemTime>) -> u32 {
    let Some(t) = time else {
        return 0;
    };
    let Ok(dur) = t.duration_since(UNIX_EPOCH) else {
        return 1;
    };
    u32::try_from(dur.as_secs()).unwrap_or(u32::MAX).max(1)
}

#[must_use]
fn decode_modified_secs(secs: u32) -> Option<SystemTime> {
    if secs == 0 {
        None
    } else {
        Some(UNIX_EPOCH + Duration::from_secs(u64::from(secs)))
    }
}

/// Zero-copy borrowed handle to a single active entry in a [`StorageTree`].
#[derive(Debug, Clone, Copy)]
pub struct NodeRef<'a> {
    tree: &'a StorageTree,
    id: NodeId,
    compact: CompactNode,
}

impl<'a> NodeRef<'a> {
    /// Arena identifier.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }

    /// Parent directory (`None` for the root).
    #[must_use]
    pub const fn parent(self) -> Option<NodeId> {
        self.compact.parent()
    }

    /// Base filename or directory name borrowed directly from the packed arena.
    #[must_use]
    pub fn name(self) -> &'a str {
        let (start, end) = self.compact.name_range();
        self.tree.name_arena.get(start..end).unwrap_or("")
    }

    /// File extension without leading dot, if any (not applicable to directories without dots).
    #[must_use]
    pub fn extension(self) -> Option<&'a str> {
        Path::new(self.name()).extension().and_then(OsStr::to_str)
    }

    /// Entry kind (`File`, `Directory`, `Symlink`, `Other`).
    #[must_use]
    pub const fn kind(self) -> EntryKind {
        self.compact.kind()
    }

    /// Allocated bytes directly belonging to this node (`0` for directories).
    #[must_use]
    pub const fn own_bytes(self) -> u64 {
        if matches!(self.compact.kind(), EntryKind::Directory) {
            0
        } else {
            self.compact.bytes
        }
    }

    /// Total recursive allocated bytes under this node.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.compact.bytes
    }

    /// Total recursive file count under this node.
    #[must_use]
    pub fn file_count(self) -> u64 {
        self.dir_data().map_or_else(
            || u64::from(matches!(self.compact.kind(), EntryKind::File)),
            |dir| u64::from(dir.file_count),
        )
    }

    /// Total recursive directory count under this node.
    #[must_use]
    pub fn dir_count(self) -> u64 {
        self.dir_data().map_or(0, |dir| u64::from(dir.dir_count))
    }

    /// Dominant [`StorageCategory`] for this node.
    #[must_use]
    pub const fn category(self) -> StorageCategory {
        self.compact.category()
    }

    /// Bytes attributed to `category` under this node.
    #[must_use]
    pub fn category_bytes(self, category: StorageCategory) -> u64 {
        match self.dir_data() {
            Some(dir) => dir.category_totals.get(category),
            None if self.compact.category() == category => self.compact.bytes,
            None => 0,
        }
    }

    /// Breakdown across all 9 categories beneath this node.
    #[must_use]
    pub fn category_totals(self) -> CategoryTotals {
        self.dir_data().map_or_else(
            || {
                let mut totals = CategoryTotals::default();
                if self.compact.bytes > 0 {
                    totals.add(self.compact.category(), self.compact.bytes);
                }
                totals
            },
            |dir| dir.category_totals,
        )
    }

    /// Last modification timestamp.
    #[must_use]
    pub fn modified(self) -> Option<SystemTime> {
        decode_modified_secs(self.modified_secs())
    }

    /// Compact encoded modification timestamp seconds (`0` for `None`).
    #[must_use]
    pub(crate) fn modified_secs(self) -> u32 {
        self.dir_data().map_or_else(
            || self.compact.modified_secs_leaf(),
            |dir| dir.modified_secs,
        )
    }

    /// Sum of bytes flagged as junk or stale/unused beneath this node.
    #[must_use]
    pub fn unused_bytes(self) -> u64 {
        self.dir_data().map_or_else(
            || {
                if self.compact.junk_kind().is_some() {
                    self.compact.bytes
                } else {
                    0
                }
            },
            |dir| dir.unused_bytes,
        )
    }

    /// Primary junk classification if flagged.
    #[must_use]
    pub const fn junk_kind(self) -> Option<JunkKind> {
        self.compact.junk_kind()
    }

    /// Whether manual review is recommended before batch deletion.
    #[must_use]
    pub const fn review_first(self) -> bool {
        self.compact.review_first()
    }

    /// Direct active child ids sorted descending by `total_bytes`.
    #[must_use]
    pub fn children(self) -> &'a [NodeId] {
        let Some(dir) = self.dir_data() else {
            return &[];
        };
        let start = dir.children_start as usize;
        let end = start.saturating_add(dir.children_len as usize);
        self.tree.child_ids.get(start..end).unwrap_or(&[])
    }

    /// Dominant deep culprit inside a folder (`mostly in ...`), derived on demand.
    #[must_use]
    pub fn largest_culprit(self) -> Option<CulpritSummary> {
        let first_id = *self.children().first()?;
        let parent_bytes = self.total_bytes();
        if parent_bytes == 0 {
            return None;
        }

        let mut cursor = first_id;
        let mut subpath = String::new();
        let mut depth = 0_usize;

        while depth < 4 {
            let node = self.tree.node(cursor)?;
            if node.total_bytes() == 0 {
                break;
            }
            subpath.push_str(node.name());
            if node.kind() == EntryKind::Directory {
                subpath.push('/');
            }

            let Some(&next_child) = node.children().first() else {
                break;
            };
            let next_node = self.tree.node(next_child)?;
            if next_node.total_bytes().saturating_mul(100) < node.total_bytes().saturating_mul(35) {
                break;
            }
            cursor = next_child;
            depth += 1;
        }

        if cursor == self.id || subpath.is_empty() {
            return None;
        }

        let target_bytes = self.tree.node(cursor)?.total_bytes();
        let share = compute_share_percent(target_bytes, parent_bytes);
        Some(CulpritSummary::new(cursor, subpath, target_bytes, share))
    }

    /// Reconstructs the path of this entry relative to [`StorageTree::root`].
    #[must_use]
    pub fn relative_path(self) -> PathBuf {
        self.tree.relative_path(self.id)
    }

    fn dir_data(self) -> Option<&'a DirData> {
        self.compact
            .dir_idx()
            .and_then(|idx| self.tree.dirs.get(idx))
    }
}

/// Depth-first iterator yielding active descendants strictly beneath a scope directory.
#[derive(Debug)]
pub struct SubtreeIter<'a> {
    tree: &'a StorageTree,
    stack: Vec<NodeId>,
}

impl<'a> Iterator for SubtreeIter<'a> {
    type Item = NodeRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(id) = self.stack.pop() {
            let Some(node) = self.tree.node(id) else {
                continue;
            };
            if node.kind() == EntryKind::Directory {
                for &child in node.children().iter().rev() {
                    self.stack.push(child);
                }
            }
            return Some(node);
        }
        None
    }
}

/// Complete scanned storage hierarchy rooted at a [`ScanRoot`].
#[derive(Debug, Clone)]
pub struct StorageTree {
    root: ScanRoot,
    volume: VolumeInfo,
    nodes: Vec<CompactNode>,
    dirs: Vec<DirData>,
    child_ids: Vec<NodeId>,
    name_arena: Arc<str>,
    unreadable_dirs: usize,
    skipped_symlinks: usize,
    skipped_cross_mount: usize,
    hardlink_savings_bytes: u64,
    scanned_at: SystemTime,
}

impl StorageTree {
    /// Builds a [`StorageTree`], links CSR child slices, aggregates totals bottom-up, and sorts.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "tree assembly consumes scanner output arenas and counters directly"
    )]
    pub(crate) fn from_raw(
        root: ScanRoot,
        volume: VolumeInfo,
        mut nodes: Vec<CompactNode>,
        mut dirs: Vec<DirData>,
        name_arena: String,
        unreadable_dirs: usize,
        skipped_symlinks: usize,
        skipped_cross_mount: usize,
        hardlink_savings_bytes: u64,
        scanned_at: SystemTime,
    ) -> Self {
        nodes.shrink_to_fit();
        dirs.shrink_to_fit();
        let arena_arc: Arc<str> = Arc::from(name_arena.into_boxed_str());

        for dir in &mut dirs {
            dir.children_len = 0;
        }
        for node in &nodes {
            if !node.is_removed()
                && let Some(parent_id) = node.parent()
                && let Some(pnode) = nodes.get(parent_id.index())
                && let Some(dir_idx) = pnode.dir_idx()
                && let Some(dir) = dirs.get_mut(dir_idx)
            {
                dir.children_len = dir.children_len.saturating_add(1);
            }
        }

        let mut total_children = 0_u32;
        for dir in &mut dirs {
            dir.children_start = total_children;
            total_children = total_children.saturating_add(dir.children_len);
        }

        let mut write_cursors: Vec<u32> = dirs.iter().map(|d| d.children_start).collect();
        let mut child_ids = vec![NodeId::new(0); total_children as usize];

        for (idx, node) in nodes.iter().enumerate() {
            if node.is_removed() {
                continue;
            }
            if let Some(parent_id) = node.parent()
                && let Some(pnode) = nodes.get(parent_id.index())
                && let Some(dir_idx) = pnode.dir_idx()
                && let Some(cursor) = write_cursors.get_mut(dir_idx)
            {
                let slot = *cursor as usize;
                if let Some(target) = child_ids.get_mut(slot) {
                    *target = NodeId::new(idx);
                }
                *cursor = cursor.saturating_add(1);
            }
        }
        drop(write_cursors);

        let mut tree = Self {
            root,
            volume,
            nodes,
            dirs,
            child_ids,
            name_arena: arena_arc,
            unreadable_dirs,
            skipped_symlinks,
            skipped_cross_mount,
            hardlink_savings_bytes,
            scanned_at,
        };
        tree.recompute_aggregates();
        tree
    }

    /// Active [`ScanRoot`] of this tree.
    #[must_use]
    pub const fn root(&self) -> &ScanRoot {
        &self.root
    }

    /// Host volume capacity snapshot captured with this scan.
    #[must_use]
    pub const fn volume(&self) -> &VolumeInfo {
        &self.volume
    }

    /// Updates the cached volume capacity snapshot (e.g. after deletions).
    pub fn set_volume(&mut self, volume: VolumeInfo) {
        self.volume = volume;
    }

    /// Root node id (`NodeId(0)`).
    #[must_use]
    pub const fn root_id(&self) -> NodeId {
        NodeId::new(0)
    }

    /// Returns a zero-copy handle for `id` if valid and not pruned.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<NodeRef<'_>> {
        self.nodes
            .get(id.index())
            .copied()
            .filter(|node| !node.is_removed())
            .map(|compact| NodeRef {
                tree: self,
                id,
                compact,
            })
    }

    /// Iterates over all active (non-pruned) nodes in the tree.
    pub fn active_nodes(&self) -> impl Iterator<Item = NodeRef<'_>> {
        self.nodes.iter().enumerate().filter_map(|(idx, &compact)| {
            if compact.is_removed() {
                None
            } else {
                Some(NodeRef {
                    tree: self,
                    id: NodeId::new(idx),
                    compact,
                })
            }
        })
    }

    /// Depth-first iterator yielding active descendants strictly beneath `scope`.
    #[must_use]
    pub fn subtree_iter(&self, scope: NodeId) -> SubtreeIter<'_> {
        let stack = self.children_slice(scope).iter().copied().rev().collect();
        SubtreeIter { tree: self, stack }
    }

    /// Zero-allocation slice of direct active child ids sorted descending by bytes.
    #[must_use]
    pub fn children_slice(&self, id: NodeId) -> &[NodeId] {
        self.node(id).map_or(&[], NodeRef::children)
    }

    /// Returns direct non-removed child ids of `id`, already sorted descending by bytes.
    #[must_use]
    pub fn children_sorted(&self, id: NodeId) -> Vec<NodeId> {
        self.children_slice(id).to_vec()
    }

    /// Returns the parent directory of `id`, if `id` is valid and non-root.
    #[must_use]
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.nodes
            .get(id.index())
            .filter(|n| !n.is_removed())
            .and_then(|n| n.parent())
    }

    /// Returns `true` if `node_id` is strictly beneath `ancestor_id` in the directory hierarchy.
    #[must_use]
    pub fn is_descendant_of(&self, node_id: NodeId, ancestor_id: NodeId) -> bool {
        let mut cursor = self.parent_of(node_id);
        while let Some(curr) = cursor {
            if curr == ancestor_id {
                return true;
            }
            cursor = self.parent_of(curr);
        }
        false
    }

    /// Reconstructs the relative path from [`StorageTree::root`] to `id`.
    #[must_use]
    pub fn relative_path(&self, id: NodeId) -> PathBuf {
        let segs = self.relative_segments(id);
        let mut out = PathBuf::new();
        for seg_id in segs.into_iter().rev() {
            if let Some(node) = self.nodes.get(seg_id.index()) {
                let (s, e) = node.name_range();
                if let Some(name) = self.name_arena.get(s..e) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// Resolves the canonical absolute path for `id`.
    #[must_use]
    pub fn absolute_path(&self, id: NodeId) -> PathBuf {
        let segs = self.relative_segments(id);
        let mut out = self.root.canonical_path().to_path_buf();
        for seg_id in segs.into_iter().rev() {
            if let Some(node) = self.nodes.get(seg_id.index()) {
                let (s, e) = node.name_range();
                if let Some(name) = self.name_arena.get(s..e) {
                    out.push(name);
                }
            }
        }
        out
    }

    fn relative_segments(&self, id: NodeId) -> Vec<NodeId> {
        let mut segs = Vec::new();
        let root = self.root_id();
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            if curr == root {
                break;
            }
            let Some(node) = self.nodes.get(curr.index()) else {
                break;
            };
            segs.push(curr);
            cursor = node.parent();
        }
        segs
    }

    /// Looks up a descendant node by its path relative to [`StorageTree::root`].
    #[must_use]
    pub fn find_by_relative_path(&self, rel: &Path) -> Option<NodeId> {
        let mut curr = self.root_id();
        for comp in rel.components() {
            match comp {
                Component::CurDir => {}
                Component::Normal(seg) => {
                    let seg_str = seg.to_string_lossy();
                    curr = self.children_slice(curr).iter().copied().find(|&cid| {
                        self.node(cid).is_some_and(|n| n.name() == seg_str.as_ref())
                    })?;
                }
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        Some(curr)
    }

    /// Returns the ancestor chain from `root_id` down to `id` inclusive.
    #[must_use]
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            chain.push(curr);
            cursor = self.nodes.get(curr.index()).and_then(|node| node.parent());
        }
        chain.reverse();
        chain
    }

    /// Returns top nodes attributing bytes to `category` for category-focused browsing.
    #[must_use]
    pub fn category_entries(&self, category: StorageCategory) -> Vec<NodeId> {
        let mut matches = Vec::new();
        self.collect_category_entries(self.root_id(), category, &mut matches);
        matches.sort_by(|a, b| {
            let bytes_a = self
                .node(*a)
                .map_or(0, |node| node.category_bytes(category));
            let bytes_b = self
                .node(*b)
                .map_or(0, |node| node.category_bytes(category));
            bytes_b.cmp(&bytes_a)
        });
        matches
    }

    fn collect_category_entries(
        &self,
        id: NodeId,
        category: StorageCategory,
        out: &mut Vec<NodeId>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        if id != self.root_id()
            && node.category() == category
            && (node.kind() == EntryKind::Directory || node.own_bytes() > 0)
        {
            out.push(id);
            return;
        }
        for &child in node.children() {
            if let Some(child_node) = self.node(child)
                && child_node.category_bytes(category) > 0
            {
                self.collect_category_entries(child, category, out);
            }
        }
    }

    /// Number of directories skipped due to permission errors.
    #[must_use]
    pub const fn unreadable_dirs(&self) -> usize {
        self.unreadable_dirs
    }

    /// Number of symbolic links skipped during scan.
    #[must_use]
    pub const fn skipped_symlinks(&self) -> usize {
        self.skipped_symlinks
    }

    /// Number of mount points skipped to respect the root device lock.
    #[must_use]
    pub const fn skipped_cross_mount(&self) -> usize {
        self.skipped_cross_mount
    }

    /// Bytes deduplicated because multiple hardlinks pointed to the same inode.
    #[must_use]
    pub const fn hardlink_savings_bytes(&self) -> u64 {
        self.hardlink_savings_bytes
    }

    /// Timestamp when the scan finished.
    #[must_use]
    pub const fn scanned_at(&self) -> SystemTime {
        self.scanned_at
    }

    /// Approximate resident arena heap bytes used by this [`StorageTree`].
    #[must_use]
    pub fn memory_bytes_estimate(&self) -> usize {
        self.nodes.capacity() * size_of::<CompactNode>()
            + self.dirs.capacity() * size_of::<DirData>()
            + self.child_ids.capacity() * size_of::<NodeId>()
            + self.name_arena.len()
    }

    /// Prunes deleted relative paths from the tree and updates all aggregates in place.
    pub fn prune_deleted(&mut self, deleted_relatives: &[PathBuf]) {
        if deleted_relatives.is_empty() {
            return;
        }
        let target_ids: Vec<NodeId> = deleted_relatives
            .iter()
            .filter_map(|rel| {
                if rel.as_os_str().is_empty() {
                    None
                } else {
                    self.find_by_relative_path(rel)
                }
            })
            .collect();
        for id in target_ids {
            self.mark_subtree_removed(id);
        }
        self.recompute_aggregates();
    }

    fn mark_subtree_removed(&mut self, root_id: NodeId) {
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            let Some(node) = self.nodes.get_mut(id.index()) else {
                continue;
            };
            if node.is_removed() {
                continue;
            }
            node.set_removed(true);
            node.bytes = 0;
            if let Some(dir_idx) = node.dir_idx()
                && let Some(dir) = self.dirs.get_mut(dir_idx)
            {
                let start = dir.children_start as usize;
                let end = start
                    .saturating_add(dir.children_len as usize)
                    .min(self.child_ids.len());
                if let Some(slice) = self.child_ids.get(start..end) {
                    stack.extend_from_slice(slice);
                }
                dir.children_len = 0;
                dir.file_count = 0;
                dir.dir_count = 0;
                dir.unused_bytes = 0;
                dir.category_totals = CategoryTotals::default();
            }
        }
    }

    fn recompute_aggregates(&mut self) {
        // Parents are always pushed into the arena strictly before their children,
        // so reverse index iteration visits every child before its parent with zero allocation.
        for idx in (0..self.nodes.len()).rev() {
            self.aggregate_single_node(NodeId::new(idx));
        }
    }

    fn aggregate_single_node(&mut self, id: NodeId) {
        let Some(compact) = self.nodes.get(id.index()).copied() else {
            return;
        };
        if compact.is_removed() {
            return;
        }
        let Some(dir_idx) = compact.dir_idx() else {
            return;
        };

        let start = self.dirs[dir_idx].children_start as usize;
        let len = self.dirs[dir_idx].children_len as usize;
        let end = start.saturating_add(len).min(self.child_ids.len());

        let mut write = start;
        for read in start..end {
            let child_id = self.child_ids[read];
            if self
                .nodes
                .get(child_id.index())
                .is_some_and(|c| !c.is_removed())
            {
                self.child_ids[write] = child_id;
                write += 1;
            }
        }
        let active_len = write.saturating_sub(start);

        let mut total_bytes = 0_u64;
        let mut file_count = 0_u64;
        let mut dir_count = 1_u64;
        let mut unused_bytes = 0_u64;
        let mut category_totals = CategoryTotals::default();
        let mut newest_mod = self.dirs[dir_idx].modified_secs;
        let mut review_first = compact.review_first();

        for pos in start..write {
            let child_id = self.child_ids[pos];
            let child = self.nodes[child_id.index()];
            total_bytes = total_bytes.saturating_add(child.bytes);
            newest_mod = newest_mod.max(
                child
                    .dir_idx()
                    .and_then(|cidx| self.dirs.get(cidx))
                    .map_or_else(|| child.modified_secs_leaf(), |cdir| cdir.modified_secs),
            );
            review_first = review_first || child.review_first();

            if let Some(cdir_idx) = child.dir_idx() {
                let cdir = self.dirs[cdir_idx];
                file_count = file_count.saturating_add(u64::from(cdir.file_count));
                dir_count = dir_count.saturating_add(u64::from(cdir.dir_count));
                unused_bytes = unused_bytes.saturating_add(cdir.unused_bytes);
                category_totals.add_all(&cdir.category_totals);
            } else {
                file_count =
                    file_count.saturating_add(u64::from(matches!(child.kind(), EntryKind::File)));
                if child.junk_kind().is_some() {
                    unused_bytes = unused_bytes.saturating_add(child.bytes);
                }
                if child.bytes > 0 {
                    category_totals.add(child.category(), child.bytes);
                }
            }
        }

        let nodes_ref = &self.nodes;
        let arena_ref = self.name_arena.as_ref();
        if let Some(children_mut) = self.child_ids.get_mut(start..write) {
            children_mut.sort_by(|&a, &b| {
                let na = nodes_ref[a.index()];
                let nb = nodes_ref[b.index()];
                nb.bytes.cmp(&na.bytes).then_with(|| {
                    let (sa, ea) = na.name_range();
                    let (sb, eb) = nb.name_range();
                    arena_ref
                        .get(sa..ea)
                        .unwrap_or("")
                        .cmp(arena_ref.get(sb..eb).unwrap_or(""))
                })
            });
        }

        let dominant_cat = if total_bytes > 0 {
            category_totals.dominant()
        } else {
            compact.category()
        };

        let node_mut = &mut self.nodes[id.index()];
        node_mut.bytes = total_bytes;
        node_mut.set_category(dominant_cat);
        node_mut.set_review_first(review_first);

        let dir_mut = &mut self.dirs[dir_idx];
        dir_mut.children_len = u32::try_from(active_len).unwrap_or(u32::MAX);
        dir_mut.modified_secs = newest_mod;
        dir_mut.file_count = u32::try_from(file_count).unwrap_or(u32::MAX);
        dir_mut.dir_count = u32::try_from(dir_count).unwrap_or(u32::MAX);
        dir_mut.unused_bytes = unused_bytes;
        dir_mut.category_totals = category_totals;
    }
}

/// Computes an integer percentage `0..=100` for `part / whole`.
#[must_use]
pub(crate) fn compute_share_percent(part: u64, whole: u64) -> u8 {
    if whole == 0 || part == 0 {
        return 0;
    }
    let pct = part.saturating_mul(100) / whole;
    u8::try_from(pct.min(100)).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disk_space_platform::FakePlatform;

    use super::*;
    use crate::scanner::ScanCancelHandle;
    use crate::scanner::ScanOptions;
    use crate::scanner::scan_root;

    #[test]
    fn compact_node_size_and_bitfields_fit_strictly() {
        assert_eq!(size_of::<NodeId>(), 4);
        assert_eq!(size_of::<CompactNode>(), 24);
        assert!(size_of::<DirData>() <= 104);

        let high_offset: usize = (5_usize << 32) | 0x1234_5678;
        let mut node = CompactNode::new(
            999,
            Some(NodeId::new(42)),
            high_offset,
            17,
            7,
            EntryKind::Directory,
            StorageCategory::Caches,
            Some(JunkKind::BuildArtifact),
            true,
        );
        assert_eq!(node.parent(), Some(NodeId::new(42)));
        assert_eq!(node.name_range(), (high_offset, high_offset + 17));
        assert_eq!(node.dir_idx(), Some(7));
        assert_eq!(node.kind(), EntryKind::Directory);
        assert_eq!(node.category(), StorageCategory::Caches);
        assert_eq!(node.junk_kind(), Some(JunkKind::BuildArtifact));
        assert!(node.review_first());
        assert!(!node.is_removed());

        node.set_category(StorageCategory::Developer);
        node.set_removed(true);
        assert_eq!(node.category(), StorageCategory::Developer);
        assert!(node.is_removed());
        assert_eq!(node.name_range(), (high_offset, high_offset + 17));
    }

    #[test]
    fn compact_tree_paths_descendants_culprits_and_memory_estimate() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("disk");
        let nested = root_dir.join("Caches/Browser/CodeCache");
        fs::create_dir_all(&nested).expect("mkdir");
        fs::write(nested.join("huge.bin"), vec![0_u8; 32_768]).expect("write huge");
        fs::write(root_dir.join("notes.txt"), vec![0_u8; 512]).expect("write notes");

        let host = FakePlatform::new(root_dir.clone());
        let scan_root_val = ScanRoot::new(&root_dir, &host).expect("scan root");
        let tree = scan_root(
            scan_root_val,
            &host,
            &ScanOptions::default(),
            &ScanCancelHandle::new(),
            &mut |_| {},
        )
        .expect("scan");

        let caches_id = tree
            .find_by_relative_path(Path::new("Caches"))
            .expect("find Caches");
        let huge_id = tree
            .find_by_relative_path(Path::new("Caches/Browser/CodeCache/huge.bin"))
            .expect("find huge.bin");
        assert!(tree.is_descendant_of(huge_id, caches_id));
        assert!(!tree.is_descendant_of(caches_id, huge_id));
        assert_eq!(
            tree.relative_path(huge_id),
            PathBuf::from("Caches/Browser/CodeCache/huge.bin")
        );

        let root_ref = tree.node(tree.root_id()).expect("root node");
        let culprit = root_ref.largest_culprit().expect("culprit");
        assert_eq!(culprit.target_id, huge_id);
        assert_eq!(
            culprit.relative_subpath,
            "Caches/Browser/CodeCache/huge.bin"
        );

        let descendants: Vec<NodeId> = tree.subtree_iter(caches_id).map(NodeRef::id).collect();
        assert_eq!(descendants.len(), 3);
        assert!(descendants.contains(&huge_id));
        assert!(tree.memory_bytes_estimate() > 0);
    }
}
