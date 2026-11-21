//! Search, filtering, sorting, and storage aggregation across a [`StorageTree`].

use std::collections::HashMap;
use std::path::Path;

use disk_space_platform::EntryKind;
use disk_space_platform::StorageCategory;

use crate::tree::DiskNode;
use crate::tree::NodeId;
use crate::tree::StorageTree;

/// Maximum search hits returned to keep the UI responsive on massive trees.
pub const MAX_SEARCH_RESULTS: usize = 400;

/// Filter toggle for showing all items vs. only junk/unused items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FilterMode {
    /// Include every scanned item.
    #[default]
    All,
    /// Include only caches, build artifacts, stale/unused items, installers, or Trash.
    JunkAndUnusedOnly,
}

/// Ordering applied to directory listings and search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SortOrder {
    /// Largest allocated size first.
    #[default]
    SizeDesc,
    /// Oldest timestamp first (stale items at the top).
    AgeOldestFirst,
    /// Lexicographical name ascending.
    NameAsc,
}

/// Query criteria evaluated against a [`StorageTree`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchQuery {
    /// Substring or `.ext` filter (empty matches all).
    pub text: String,
    /// Optional category filter.
    pub category: Option<StorageCategory>,
    /// All vs. junk/unused-only toggle.
    pub filter_mode: FilterMode,
    /// Minimum allocated bytes required.
    pub min_bytes: Option<u64>,
    /// Result ordering.
    pub sort: SortOrder,
    /// Optional subtree root constraining the search.
    pub scope_node: Option<NodeId>,
}

/// Space aggregation for a single file extension (e.g. `dmg`, `log`, `rlib`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtensionAggregate {
    /// Lowercase extension or `"(no ext)"`.
    pub extension: String,
    /// Total allocated bytes across matching files.
    pub total_bytes: u64,
    /// Number of files with this extension.
    pub file_count: u64,
    /// Largest representative file node for quick inspection/snagging.
    pub sample_node: NodeId,
}

/// Aggregated space hogs under a scope node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AggregateReport {
    /// Extensions ordered descending by total allocated bytes.
    pub by_extension: Vec<ExtensionAggregate>,
    /// Heaviest directories inside the scope.
    pub top_folders: Vec<NodeId>,
    /// Largest junk/unused clusters inside the scope.
    pub junk_clusters: Vec<NodeId>,
}

/// Searches `tree` according to `query` and returns up to [`MAX_SEARCH_RESULTS`] sorted [`NodeId`]s.
#[must_use]
pub fn search_tree(tree: &StorageTree, query: &SearchQuery) -> Vec<NodeId> {
    let mut matches = search_tree_all(tree, query, None);
    truncate_and_sort_nodes(tree, &mut matches, query.sort, MAX_SEARCH_RESULTS);
    matches
}

/// Searches `tree` (or an explicit slice of `candidates`) according to `query` and returns all
/// matching [`NodeId`]s without sorting or truncation.
#[must_use]
pub fn search_tree_all(
    tree: &StorageTree,
    query: &SearchQuery,
    candidates: Option<&[NodeId]>,
) -> Vec<NodeId> {
    let normalized = NormalizedQuery::new(&query.text);
    let filter = normalized.filter();
    let scope_prefix = query
        .scope_node
        .and_then(|id| tree.node(id))
        .map(|node| node.relative_path.as_path());
    let root_id = tree.root_id();

    let matches_criteria = |node: &DiskNode| -> bool {
        if node.id == root_id {
            return false;
        }
        if let Some(prefix) = scope_prefix
            && !prefix.as_os_str().is_empty()
            && !(node.relative_path.starts_with(prefix) && node.relative_path.as_path() != prefix)
        {
            return false;
        }
        if query
            .category
            .is_some_and(|cat| node.category != cat && node.category_totals.get(cat) == 0)
        {
            return false;
        }
        match query.filter_mode {
            FilterMode::All => {}
            FilterMode::JunkAndUnusedOnly => {
                if node.junk_kind.is_none() && node.unused_bytes == 0 {
                    return false;
                }
            }
        }
        if query.min_bytes.is_some_and(|min| node.total_bytes < min) {
            return false;
        }
        matches_parsed(node, filter)
    };

    candidates.map_or_else(
        || {
            tree.active_nodes()
                .filter(|node| matches_criteria(node))
                .map(|node| node.id)
                .collect()
        },
        |ids| {
            ids.iter()
                .filter_map(|id| tree.node(*id))
                .filter(|node| matches_criteria(node))
                .map(|node| node.id)
                .collect()
        },
    )
}

/// Keeps at most `limit` top [`NodeId`]s ordered by `order` using `select_nth_unstable_by`
/// so large hit sets are partially partitioned in linear time before sorting the visible slice.
pub fn truncate_and_sort_nodes(
    tree: &StorageTree,
    ids: &mut Vec<NodeId>,
    order: SortOrder,
    limit: usize,
) {
    if limit == 0 {
        ids.clear();
        return;
    }
    if ids.len() > limit {
        ids.select_nth_unstable_by(limit, |&a, &b| compare_nodes(tree, a, b, order));
        ids.truncate(limit);
    }
    ids.sort_by(|&a, &b| compare_nodes(tree, a, b, order));
}

/// Sorts a slice of [`NodeId`]s according to `order`.
pub fn sort_nodes(tree: &StorageTree, ids: &mut [NodeId], order: SortOrder) {
    ids.sort_by(|&a, &b| compare_nodes(tree, a, b, order));
}

fn compare_nodes(
    tree: &StorageTree,
    a_id: NodeId,
    b_id: NodeId,
    order: SortOrder,
) -> std::cmp::Ordering {
    let Some(a) = tree.node(a_id) else {
        return std::cmp::Ordering::Greater;
    };
    let Some(b) = tree.node(b_id) else {
        return std::cmp::Ordering::Less;
    };
    match order {
        SortOrder::SizeDesc => b
            .total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| a.name.cmp(&b.name)),
        SortOrder::AgeOldestFirst => a
            .modified
            .cmp(&b.modified)
            .then_with(|| b.total_bytes.cmp(&a.total_bytes)),
        SortOrder::NameAsc => a
            .name_lower
            .cmp(&b.name_lower)
            .then_with(|| a.name.cmp(&b.name)),
    }
}

/// Computes extension, top-folder, and junk-cluster aggregations within `scope`.
#[must_use]
pub fn compute_aggregates(tree: &StorageTree, scope: NodeId, limit: usize) -> AggregateReport {
    let scope_prefix = tree
        .node(scope)
        .map_or_else(|| Path::new(""), |node| node.relative_path.as_path());

    let mut ext_map: HashMap<String, (u64, u64, NodeId, u64)> = HashMap::new();
    let mut folders = Vec::new();
    let mut junk = Vec::new();

    for node in tree.active_nodes() {
        if node.id == tree.root_id() || node.id == scope {
            continue;
        }
        if !scope_prefix.as_os_str().is_empty() && !node.relative_path.starts_with(scope_prefix) {
            continue;
        }

        if node.kind == EntryKind::Directory {
            if node.total_bytes > 0 {
                folders.push(node.id);
            }
            if node.junk_kind.is_some() && node.total_bytes > 0 {
                junk.push(node.id);
            }
        } else if node.kind == EntryKind::File {
            let ext = node
                .ext_lower
                .as_deref()
                .map_or_else(|| "(no ext)".to_owned(), str::to_owned);
            let entry = ext_map
                .entry(ext)
                .or_insert((0, 0, node.id, node.own_bytes));
            entry.0 = entry.0.saturating_add(node.own_bytes);
            entry.1 = entry.1.saturating_add(1);
            if node.own_bytes > entry.3 {
                entry.2 = node.id;
                entry.3 = node.own_bytes;
            }
            if node.junk_kind.is_some() && node.own_bytes > 0 {
                junk.push(node.id);
            }
        }
    }

    let mut by_extension: Vec<ExtensionAggregate> = ext_map
        .into_iter()
        .map(
            |(extension, (total_bytes, file_count, sample_node, _))| ExtensionAggregate {
                extension,
                total_bytes,
                file_count,
                sample_node,
            },
        )
        .collect();
    by_extension.sort_by_key(|ext| std::cmp::Reverse(ext.total_bytes));
    by_extension.truncate(limit);

    sort_nodes(tree, &mut folders, SortOrder::SizeDesc);
    folders.truncate(limit);

    sort_nodes(tree, &mut junk, SortOrder::SizeDesc);
    junk.truncate(limit);

    AggregateReport {
        by_extension,
        top_folders: folders,
        junk_clusters: junk,
    }
}

#[derive(Clone, Copy)]
enum QueryFilter<'a> {
    Empty,
    Extension(&'a str),
    Name(&'a str),
    Subpath(&'a str),
}

struct NormalizedQuery {
    normalized: String,
}

impl NormalizedQuery {
    fn new(text: &str) -> Self {
        let trimmed = text.trim();
        let normalized = if trimmed.contains('\\') {
            trimmed.replace('\\', "/").to_ascii_lowercase()
        } else {
            trimmed.to_ascii_lowercase()
        };
        Self { normalized }
    }

    fn filter(&self) -> QueryFilter<'_> {
        let trimmed = self.normalized.trim();
        if trimmed.is_empty() {
            return QueryFilter::Empty;
        }
        if let Some(ext_query) = trimmed.strip_prefix('.')
            && !ext_query.is_empty()
            && !ext_query.contains('/')
        {
            return QueryFilter::Extension(ext_query);
        }
        let clean = trimmed.trim_matches('/');
        if clean.is_empty() {
            QueryFilter::Empty
        } else if clean.contains('/') {
            QueryFilter::Subpath(clean)
        } else {
            QueryFilter::Name(clean)
        }
    }
}

fn matches_parsed(node: &DiskNode, filter: QueryFilter<'_>) -> bool {
    match filter {
        QueryFilter::Empty => true,
        QueryFilter::Extension(ext_query) => node.ext_lower.as_deref() == Some(ext_query),
        QueryFilter::Name(clean) => node.name_lower.contains(clean),
        QueryFilter::Subpath(clean) => {
            // Explicit subpath queries must extend into the entry's own name, never match only an ancestor.
            let name_start = node
                .rel_path_lower
                .len()
                .saturating_sub(node.name_lower.len());
            node.rel_path_lower
                .rfind(clean)
                .is_some_and(|pos| pos.saturating_add(clean.len()) > name_start)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disk_space_platform::FakePlatform;

    use super::*;
    use crate::jail::ScanRoot;
    use crate::scanner::ScanCancelHandle;
    use crate::scanner::ScanOptions;
    use crate::scanner::scan_root;

    #[test]
    fn searches_filters_and_aggregates_by_extension() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("root");
        let caches = root_dir.join("Caches");
        let docs = root_dir.join("Documents");
        fs::create_dir_all(&caches).expect("mkdir caches");
        fs::create_dir_all(&docs).expect("mkdir docs");
        fs::write(caches.join("old.log"), vec![1_u8; 8_192]).expect("write log");
        fs::write(docs.join("archive.dmg"), vec![2_u8; 16_384]).expect("write dmg");

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

        let log_hits = search_tree(
            &tree,
            &SearchQuery {
                text: ".log".to_owned(),
                ..SearchQuery::default()
            },
        );
        assert_eq!(log_hits.len(), 1);
        assert_eq!(tree.node(log_hits[0]).expect("hit").name, "old.log");

        let junk_hits = search_tree(
            &tree,
            &SearchQuery {
                filter_mode: FilterMode::JunkAndUnusedOnly,
                ..SearchQuery::default()
            },
        );
        assert!(
            junk_hits
                .iter()
                .any(|id| tree.node(*id).is_some_and(|n| n.name == "old.log"))
        );

        let agg = compute_aggregates(&tree, tree.root_id(), 10);
        assert!(!agg.by_extension.is_empty());
        assert_eq!(agg.by_extension[0].extension, "dmg");
    }

    #[test]
    fn search_excludes_non_matching_descendants_of_matching_parent_dir() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("downloads");
        let report_data = root_dir.join("playwright-report-core/data");
        let other = root_dir.join("other");
        fs::create_dir_all(&report_data).expect("mkdir report_data");
        fs::create_dir_all(&other).expect("mkdir other");

        fs::write(report_data.join("8a4f9c.webm"), vec![1_u8; 4_096]).expect("write webm");
        fs::write(
            root_dir.join("playwright-report-core/index.html"),
            vec![2_u8; 1_024],
        )
        .expect("write html");
        fs::write(
            root_dir.join("playwright-report-core/playwright-trace.zip"),
            vec![3_u8; 2_048],
        )
        .expect("write trace");
        fs::write(other.join("playwright.config.ts"), vec![4_u8; 512]).expect("write config");

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

        let hits = search_tree(
            &tree,
            &SearchQuery {
                text: "playwright".to_owned(),
                ..SearchQuery::default()
            },
        );
        let hit_names: Vec<&str> = hits
            .iter()
            .filter_map(|id| tree.node(*id).map(|n| n.name.as_str()))
            .collect();
        assert_eq!(
            hit_names,
            vec![
                "playwright-report-core",
                "playwright-trace.zip",
                "playwright.config.ts"
            ]
        );
        assert!(!hit_names.contains(&"8a4f9c.webm"));
        assert!(!hit_names.contains(&"index.html"));

        let webm_hits = search_tree(
            &tree,
            &SearchQuery {
                text: ".webm".to_owned(),
                ..SearchQuery::default()
            },
        );
        let webm_names: Vec<&str> = webm_hits
            .iter()
            .filter_map(|id| tree.node(*id).map(|n| n.name.as_str()))
            .collect();
        assert_eq!(webm_names, vec!["8a4f9c.webm"]);

        let subpath_hits = search_tree(
            &tree,
            &SearchQuery {
                text: "playwright-report-core/data".to_owned(),
                ..SearchQuery::default()
            },
        );
        let subpath_names: Vec<&str> = subpath_hits
            .iter()
            .filter_map(|id| tree.node(*id).map(|n| n.name.as_str()))
            .collect();
        assert_eq!(subpath_names, vec!["data"]);
    }

    #[test]
    fn incremental_candidate_narrowing_matches_full_search() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("downloads");
        let dir = root_dir.join("playwright-report-core");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("playwright-trace.zip"), vec![1_u8; 4_096]).expect("write trace");
        fs::write(root_dir.join("playground.txt"), vec![2_u8; 2_048]).expect("write playground");
        fs::write(root_dir.join("other.bin"), vec![3_u8; 8_192]).expect("write other");

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

        let broad_query = SearchQuery {
            text: "play".to_owned(),
            ..SearchQuery::default()
        };
        let broad = search_tree_all(&tree, &broad_query, None);
        assert_eq!(broad.len(), 3);

        let narrow_query = SearchQuery {
            text: "playwright".to_owned(),
            ..SearchQuery::default()
        };
        let mut from_candidates = search_tree_all(&tree, &narrow_query, Some(&broad));
        let full = search_tree(&tree, &narrow_query);
        truncate_and_sort_nodes(
            &tree,
            &mut from_candidates,
            SortOrder::SizeDesc,
            MAX_SEARCH_RESULTS,
        );
        assert_eq!(from_candidates, full);
    }

    #[test]
    fn truncate_and_sort_nodes_keeps_exact_top_n() {
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("many");
        fs::create_dir_all(&root_dir).expect("mkdir");
        for idx in 1..=12_u64 {
            let size = usize::try_from(idx * 16_384).expect("size fits");
            fs::write(
                root_dir.join(format!("item_{idx:02}.dat")),
                vec![0_u8; size],
            )
            .expect("write file");
        }

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

        let mut ids = search_tree_all(&tree, &SearchQuery::default(), None);
        assert_eq!(ids.len(), 12);
        truncate_and_sort_nodes(&tree, &mut ids, SortOrder::SizeDesc, 4);
        let names: Vec<&str> = ids
            .iter()
            .filter_map(|id| tree.node(*id).map(|n| n.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec!["item_12.dat", "item_11.dat", "item_10.dat", "item_09.dat"]
        );
    }
}
