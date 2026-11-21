//! In-memory storage tree, per-category byte aggregation, culprit summaries, and pruning.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use disk_space_platform::EntryKind;
use disk_space_platform::JunkKind;
use disk_space_platform::StorageCategory;
use disk_space_platform::VolumeInfo;

use crate::jail::ScanRoot;

/// Compact node index inside [`StorageTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// Creates a [`NodeId`] from a vector index.
    #[must_use]
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Raw index into the arena.
    #[must_use]
    pub const fn index(self) -> usize {
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

/// Single file, folder, or symlink entry inside a [`StorageTree`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DiskNode {
    /// Arena identifier.
    pub id: NodeId,
    /// Parent directory (`None` for the root).
    pub parent: Option<NodeId>,
    /// Direct child nodes sorted descending by `total_bytes`.
    pub children: Vec<NodeId>,
    /// Base filename or directory name.
    pub name: String,
    /// Relative path from [`StorageTree::root`].
    pub relative_path: PathBuf,
    /// Entry kind (`File`, `Directory`, `Symlink`, `Other`).
    pub kind: EntryKind,
    /// Allocated bytes directly belonging to this node (0 for directories).
    pub own_bytes: u64,
    /// Total recursive allocated bytes under this node.
    pub total_bytes: u64,
    /// Total recursive logical bytes under this node.
    pub logical_bytes: u64,
    /// Total recursive file count under this node.
    pub file_count: u64,
    /// Total recursive directory count under this node.
    pub dir_count: u64,
    /// Dominant [`StorageCategory`] for this node.
    pub category: StorageCategory,
    /// Breakdown across all 9 categories beneath this node.
    pub category_totals: CategoryTotals,
    /// Last modification timestamp.
    pub modified: Option<SystemTime>,
    /// Last access timestamp.
    pub accessed: Option<SystemTime>,
    /// Sum of bytes flagged as junk or stale/unused beneath this node.
    pub unused_bytes: u64,
    /// Primary junk classification if flagged.
    pub junk_kind: Option<JunkKind>,
    /// Whether manual review is recommended before batch deletion.
    pub review_first: bool,
    /// Dominant deep culprit inside a folder (`mostly in ...`).
    pub largest_culprit: Option<CulpritSummary>,
    /// Soft-deleted flag set after in-place pruning.
    pub(crate) removed: bool,
}

/// Complete scanned storage hierarchy rooted at a [`ScanRoot`].
#[derive(Debug, Clone)]
pub struct StorageTree {
    root: ScanRoot,
    volume: VolumeInfo,
    nodes: Vec<DiskNode>,
    unreadable_dirs: usize,
    skipped_symlinks: usize,
    skipped_cross_mount: usize,
    hardlink_savings_bytes: u64,
    scanned_at: SystemTime,
}

impl StorageTree {
    /// Builds a [`StorageTree`], aggregates totals bottom-up, sorts children, and derives culprits.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "tree assembly consumes scanner output counters directly"
    )]
    pub(crate) fn from_raw(
        root: ScanRoot,
        volume: VolumeInfo,
        nodes: Vec<DiskNode>,
        unreadable_dirs: usize,
        skipped_symlinks: usize,
        skipped_cross_mount: usize,
        hardlink_savings_bytes: u64,
        scanned_at: SystemTime,
    ) -> Self {
        let mut tree = Self {
            root,
            volume,
            nodes,
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

    /// Returns a node by id if valid and not pruned.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&DiskNode> {
        self.nodes.get(id.0).filter(|node| !node.removed)
    }

    /// Iterates over all active (non-pruned) nodes in the tree.
    pub fn active_nodes(&self) -> impl Iterator<Item = &DiskNode> {
        self.nodes.iter().filter(|node| !node.removed)
    }

    /// Resolves the canonical absolute path for `id`.
    #[must_use]
    pub fn absolute_path(&self, id: NodeId) -> PathBuf {
        match self.nodes.get(id.0) {
            Some(node) if !node.relative_path.as_os_str().is_empty() => {
                self.root.canonical_path().join(&node.relative_path)
            }
            _ => self.root.canonical_path().to_path_buf(),
        }
    }

    /// Returns the ancestor chain from `root_id` down to `id` inclusive.
    #[must_use]
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            chain.push(curr);
            cursor = self.nodes.get(curr.0).and_then(|node| node.parent);
        }
        chain.reverse();
        chain
    }

    /// Returns direct non-removed child ids of `id`, already sorted descending by bytes.
    #[must_use]
    pub fn children_sorted(&self, id: NodeId) -> Vec<NodeId> {
        self.node(id)
            .map(|node| {
                node.children
                    .iter()
                    .copied()
                    .filter(|child_id| self.node(*child_id).is_some())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns top nodes attributing bytes to `category` for category-focused browsing.
    #[must_use]
    pub fn category_entries(&self, category: StorageCategory) -> Vec<NodeId> {
        let mut matches = Vec::new();
        self.collect_category_entries(self.root_id(), category, &mut matches);
        matches.sort_by(|a, b| {
            let bytes_a = self
                .node(*a)
                .map_or(0, |node| node.category_totals.get(category));
            let bytes_b = self
                .node(*b)
                .map_or(0, |node| node.category_totals.get(category));
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
            && node.category == category
            && (node.kind == EntryKind::Directory || node.own_bytes > 0)
        {
            out.push(id);
            return;
        }
        for &child in &node.children {
            if let Some(child_node) = self.node(child)
                && child_node.category_totals.get(category) > 0
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

    /// Prunes deleted relative paths from the tree and updates all aggregates in place.
    pub fn prune_deleted(&mut self, deleted_relatives: &[PathBuf]) {
        if deleted_relatives.is_empty() {
            return;
        }
        let targets: HashSet<&Path> = deleted_relatives.iter().map(PathBuf::as_path).collect();
        for idx in 1..self.nodes.len() {
            if targets.contains(self.nodes[idx].relative_path.as_path()) {
                self.mark_subtree_removed(NodeId::new(idx));
            }
        }
        self.recompute_aggregates();
    }

    fn mark_subtree_removed(&mut self, root_id: NodeId) {
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            if let Some(node) = self.nodes.get_mut(id.0) {
                if node.removed {
                    continue;
                }
                node.removed = true;
                node.own_bytes = 0;
                node.total_bytes = 0;
                node.logical_bytes = 0;
                node.unused_bytes = 0;
                node.category_totals = CategoryTotals::default();
                node.largest_culprit = None;
                stack.extend(node.children.iter().copied());
                node.children.clear();
            }
        }
    }

    fn recompute_aggregates(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        // Bottom-up pass: nodes are allocated in BFS/parent-before-child or arbitrary order,
        // so build an explicit topological post-order starting from root.
        let mut post_order = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(self.root_id(), false)];
        while let Some((id, visited)) = stack.pop() {
            let Some(node) = self.nodes.get(id.0) else {
                continue;
            };
            if node.removed {
                continue;
            }
            if visited {
                post_order.push(id);
            } else {
                stack.push((id, true));
                for &child in &node.children {
                    stack.push((child, false));
                }
            }
        }

        for id in post_order {
            self.aggregate_single_node(id);
        }
    }

    fn aggregate_single_node(&mut self, id: NodeId) {
        let children: Vec<NodeId> = self.nodes[id.0]
            .children
            .iter()
            .copied()
            .filter(|child_id| self.nodes.get(child_id.0).is_some_and(|c| !c.removed))
            .collect();

        if children.is_empty() && self.nodes[id.0].kind != EntryKind::Directory {
            let own = self.nodes[id.0].own_bytes;
            let cat = self.nodes[id.0].category;
            let mut totals = CategoryTotals::default();
            totals.add(cat, own);
            let node = &mut self.nodes[id.0];
            node.total_bytes = own;
            node.category_totals = totals;
            node.largest_culprit = None;
            return;
        }

        let mut total_bytes = self.nodes[id.0].own_bytes;
        let mut logical_bytes = if self.nodes[id.0].kind == EntryKind::Directory {
            0
        } else {
            self.nodes[id.0].logical_bytes
        };
        let mut file_count = u64::from(self.nodes[id.0].kind == EntryKind::File);
        let mut dir_count = u64::from(self.nodes[id.0].kind == EntryKind::Directory);
        let mut unused_bytes = if self.nodes[id.0].junk_kind.is_some() {
            self.nodes[id.0].own_bytes
        } else {
            0
        };
        let mut category_totals = CategoryTotals::default();
        if self.nodes[id.0].own_bytes > 0 {
            category_totals.add(self.nodes[id.0].category, self.nodes[id.0].own_bytes);
        }
        let mut newest_mod = self.nodes[id.0].modified;
        let mut newest_acc = self.nodes[id.0].accessed;
        let mut review_first = self.nodes[id.0].review_first;

        for &child_id in &children {
            let child = &self.nodes[child_id.0];
            total_bytes = total_bytes.saturating_add(child.total_bytes);
            logical_bytes = logical_bytes.saturating_add(child.logical_bytes);
            file_count = file_count.saturating_add(child.file_count);
            dir_count = dir_count.saturating_add(child.dir_count);
            unused_bytes = unused_bytes.saturating_add(child.unused_bytes);
            category_totals.add_all(&child.category_totals);
            newest_mod = max_time(newest_mod, child.modified);
            newest_acc = max_time(newest_acc, child.accessed);
            review_first = review_first || child.review_first;
        }

        let mut sorted_children = children;
        sorted_children.sort_by(|a, b| {
            self.nodes[b.0]
                .total_bytes
                .cmp(&self.nodes[a.0].total_bytes)
                .then_with(|| self.nodes[a.0].name.cmp(&self.nodes[b.0].name))
        });

        let culprit = self.compute_culprit(id, &sorted_children, total_bytes);
        let dominant_cat = if total_bytes > 0 {
            category_totals.dominant()
        } else {
            self.nodes[id.0].category
        };

        let node = &mut self.nodes[id.0];
        node.children = sorted_children;
        node.total_bytes = total_bytes;
        node.logical_bytes = logical_bytes;
        node.file_count = file_count;
        node.dir_count = dir_count;
        node.unused_bytes = unused_bytes;
        node.category_totals = category_totals;
        node.category = dominant_cat;
        node.modified = newest_mod;
        node.accessed = newest_acc;
        node.review_first = review_first;
        node.largest_culprit = culprit;
    }

    fn compute_culprit(
        &self,
        parent_id: NodeId,
        sorted_children: &[NodeId],
        parent_bytes: u64,
    ) -> Option<CulpritSummary> {
        let first_id = *sorted_children.first()?;
        if parent_bytes == 0 {
            return None;
        }
        // Walk down the heaviest child chain up to 4 levels while the child remains significant.
        let mut cursor = first_id;
        let mut segments = Vec::new();
        let mut depth = 0_usize;

        while depth < 4 {
            let node = self.nodes.get(cursor.0)?;
            if node.total_bytes == 0 {
                break;
            }
            let suffix = if node.kind == EntryKind::Directory {
                format!("{}/", node.name)
            } else {
                node.name.clone()
            };
            segments.push(suffix);

            let Some(&next_child) = node.children.first() else {
                break;
            };
            let next_node = self.nodes.get(next_child.0)?;
            if next_node.total_bytes.saturating_mul(100) < node.total_bytes.saturating_mul(35) {
                break;
            }
            cursor = next_child;
            depth += 1;
        }

        if cursor == parent_id || segments.is_empty() {
            return None;
        }

        let target_bytes = self.nodes.get(cursor.0)?.total_bytes;
        let share = compute_share_percent(target_bytes, parent_bytes);
        Some(CulpritSummary::new(
            cursor,
            segments.concat(),
            target_bytes,
            share,
        ))
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

fn max_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(ta), Some(tb)) => Some(if ta >= tb { ta } else { tb }),
        (Some(ta), None) => Some(ta),
        (None, Some(tb)) => Some(tb),
        (None, None) => None,
    }
}
