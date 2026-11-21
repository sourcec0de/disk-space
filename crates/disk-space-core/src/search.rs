//! Search, filtering, sorting, and storage aggregation across a [`StorageTree`].

use std::cmp::Ordering;
use std::collections::HashMap;

use disk_space_platform::EntryKind;
use disk_space_platform::StorageCategory;

use crate::tree::NodeId;
use crate::tree::NodeRef;
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
    search_tree_bounded(tree, query, None, MAX_SEARCH_RESULTS, 0).0
}

/// Searches `tree` (or `candidates`) keeping bounded `O(limit)` heap memory for top hits,
/// plus an optional candidate cache capped at `max_cache` entries (`0` disables caching).
#[must_use]
pub fn search_tree_bounded(
    tree: &StorageTree,
    query: &SearchQuery,
    candidates: Option<&[NodeId]>,
    limit: usize,
    max_cache: usize,
) -> (Vec<NodeId>, Option<Vec<NodeId>>) {
    let mut top = Vec::new();
    let watermark = limit.saturating_mul(2).max(limit).saturating_add(64);
    let mut cache_all = (max_cache > 0).then(Vec::new);

    visit_matching_nodes(tree, query, candidates, |node| {
        let id = node.id();
        if let Some(cache) = cache_all.as_mut() {
            if cache.len() < max_cache {
                cache.push(id);
            } else {
                cache_all = None;
            }
        }
        if limit > 0 {
            top.push(id);
            if top.len() >= watermark {
                top.select_nth_unstable_by(limit, |&a, &b| compare_nodes(tree, a, b, query.sort));
                top.truncate(limit);
            }
        }
    });

    truncate_and_sort_nodes(tree, &mut top, query.sort, limit);
    (top, cache_all)
}

/// Searches `tree` (or an explicit slice of `candidates`) according to `query` and returns all
/// matching [`NodeId`]s without sorting or truncation.
#[must_use]
pub fn search_tree_all(
    tree: &StorageTree,
    query: &SearchQuery,
    candidates: Option<&[NodeId]>,
) -> Vec<NodeId> {
    let mut out = Vec::new();
    visit_matching_nodes(tree, query, candidates, |node| out.push(node.id()));
    out
}

fn visit_matching_nodes<'a>(
    tree: &'a StorageTree,
    query: &SearchQuery,
    candidates: Option<&[NodeId]>,
    mut on_match: impl FnMut(NodeRef<'a>),
) {
    let normalized = NormalizedQuery::new(&query.text);
    let filter = normalized.filter();
    let root_id = tree.root_id();
    let scope_id = query
        .scope_node
        .filter(|&id| id != root_id && tree.node(id).is_some());

    if let Some(ids) = candidates {
        for &id in ids {
            if let Some(node) = tree.node(id)
                && node.id() != root_id
                && scope_id.is_none_or(|scope| tree.is_descendant_of(node.id(), scope))
                && node_matches_query(tree, node, query, filter)
            {
                on_match(node);
            }
        }
    } else if let Some(scope) = scope_id {
        for node in tree.subtree_iter(scope) {
            if node.id() != root_id && node_matches_query(tree, node, query, filter) {
                on_match(node);
            }
        }
    } else {
        for node in tree.active_nodes() {
            if node.id() != root_id && node_matches_query(tree, node, query, filter) {
                on_match(node);
            }
        }
    }
}

fn node_matches_query(
    tree: &StorageTree,
    node: NodeRef<'_>,
    query: &SearchQuery,
    filter: QueryFilter<'_>,
) -> bool {
    if query
        .category
        .is_some_and(|cat| node.category() != cat && node.category_bytes(cat) == 0)
    {
        return false;
    }
    match query.filter_mode {
        FilterMode::All => {}
        FilterMode::JunkAndUnusedOnly => {
            if node.junk_kind().is_none() && node.unused_bytes() == 0 {
                return false;
            }
        }
    }
    if query.min_bytes.is_some_and(|min| node.total_bytes() < min) {
        return false;
    }
    matches_parsed(tree, node, filter)
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

fn compare_nodes(tree: &StorageTree, a_id: NodeId, b_id: NodeId, order: SortOrder) -> Ordering {
    let Some(a) = tree.node(a_id) else {
        return Ordering::Greater;
    };
    let Some(b) = tree.node(b_id) else {
        return Ordering::Less;
    };
    match order {
        SortOrder::SizeDesc => b
            .total_bytes()
            .cmp(&a.total_bytes())
            .then_with(|| a.name().cmp(b.name())),
        SortOrder::AgeOldestFirst => a
            .modified_secs()
            .cmp(&b.modified_secs())
            .then_with(|| b.total_bytes().cmp(&a.total_bytes())),
        SortOrder::NameAsc => cmp_ascii_ci(a.name(), b.name()),
    }
}

/// Computes extension, top-folder, and junk-cluster aggregations within `scope`.
#[must_use]
pub fn compute_aggregates(tree: &StorageTree, scope: NodeId, limit: usize) -> AggregateReport {
    let mut ext_map: HashMap<String, (u64, u64, NodeId, u64)> = HashMap::new();
    let mut folders = Vec::new();
    let mut junk = Vec::new();
    let watermark = limit.saturating_mul(2).max(limit).saturating_add(64);
    let root_id = tree.root_id();

    let mut accumulate = |node: NodeRef<'_>| {
        if node.kind() == EntryKind::Directory {
            if node.total_bytes() > 0 && limit > 0 {
                folders.push(node.id());
                if folders.len() >= watermark {
                    folders.select_nth_unstable_by(limit, |&a, &b| {
                        compare_nodes(tree, a, b, SortOrder::SizeDesc)
                    });
                    folders.truncate(limit);
                }
            }
            if node.junk_kind().is_some() && node.total_bytes() > 0 && limit > 0 {
                junk.push(node.id());
                if junk.len() >= watermark {
                    junk.select_nth_unstable_by(limit, |&a, &b| {
                        compare_nodes(tree, a, b, SortOrder::SizeDesc)
                    });
                    junk.truncate(limit);
                }
            }
        } else if node.kind() == EntryKind::File {
            let own = node.own_bytes();
            update_extension_aggregate(&mut ext_map, node.extension(), node.id(), own);
            if node.junk_kind().is_some() && own > 0 && limit > 0 {
                junk.push(node.id());
                if junk.len() >= watermark {
                    junk.select_nth_unstable_by(limit, |&a, &b| {
                        compare_nodes(tree, a, b, SortOrder::SizeDesc)
                    });
                    junk.truncate(limit);
                }
            }
        }
    };

    if scope == root_id {
        for node in tree.active_nodes() {
            if node.id() != root_id {
                accumulate(node);
            }
        }
    } else {
        for node in tree.subtree_iter(scope) {
            accumulate(node);
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
    if by_extension.len() > limit && limit > 0 {
        by_extension.select_nth_unstable_by(limit, |a, b| b.total_bytes.cmp(&a.total_bytes));
        by_extension.truncate(limit);
    } else if limit == 0 {
        by_extension.clear();
    }
    by_extension.sort_by_key(|ext| std::cmp::Reverse(ext.total_bytes));

    truncate_and_sort_nodes(tree, &mut folders, SortOrder::SizeDesc, limit);
    truncate_and_sort_nodes(tree, &mut junk, SortOrder::SizeDesc, limit);

    AggregateReport {
        by_extension,
        top_folders: folders,
        junk_clusters: junk,
    }
}

fn update_extension_aggregate(
    ext_map: &mut HashMap<String, (u64, u64, NodeId, u64)>,
    raw_ext: Option<&str>,
    node_id: NodeId,
    own_bytes: u64,
) {
    let mut stack_buf = [0_u8; 32];
    let owned_fallback;
    let key: &str = match raw_ext {
        None => "(no ext)",
        Some(raw) if raw.len() <= stack_buf.len() => {
            for (dst, &src) in stack_buf.iter_mut().zip(raw.as_bytes()) {
                *dst = src.to_ascii_lowercase();
            }
            std::str::from_utf8(&stack_buf[..raw.len()]).unwrap_or(raw)
        }
        Some(raw) => {
            owned_fallback = raw.to_ascii_lowercase();
            &owned_fallback
        }
    };

    if let Some(entry) = ext_map.get_mut(key) {
        entry.0 = entry.0.saturating_add(own_bytes);
        entry.1 = entry.1.saturating_add(1);
        if own_bytes > entry.3 {
            entry.2 = node_id;
            entry.3 = own_bytes;
        }
    } else {
        ext_map.insert(key.to_owned(), (own_bytes, 1, node_id, own_bytes));
    }
}

#[derive(Clone, Copy)]
enum QueryFilter<'a> {
    Empty,
    Extension(&'a str),
    Name(&'a str),
    Subpath(&'a str, &'a str),
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
        } else if let Some((parent_part, name_part)) = clean.rsplit_once('/') {
            QueryFilter::Subpath(parent_part, name_part)
        } else {
            QueryFilter::Name(clean)
        }
    }
}

fn matches_parsed(tree: &StorageTree, node: NodeRef<'_>, filter: QueryFilter<'_>) -> bool {
    match filter {
        QueryFilter::Empty => true,
        QueryFilter::Extension(ext_query) => node
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(ext_query)),
        QueryFilter::Name(clean) => contains_ascii_ci(node.name(), clean),
        QueryFilter::Subpath(parent_part, name_part) => {
            let Some(parent_id) = node.parent() else {
                return false;
            };
            if parent_id == tree.root_id() {
                return false;
            }
            starts_with_ascii_ci(node.name(), name_part)
                && ancestors_end_with_ascii_ci(tree, parent_id, parent_part.as_bytes())
        }
    }
}

fn contains_ascii_ci(haystack: &str, needle_lower: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    let Some(&first) = n.first() else {
        return true;
    };
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len())
        .any(|w| w[0].to_ascii_lowercase() == first && w.eq_ignore_ascii_case(n))
}

fn starts_with_ascii_ci(haystack: &str, prefix_lower: &str) -> bool {
    let h = haystack.as_bytes();
    let p = prefix_lower.as_bytes();
    h.get(..p.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(p))
}

fn ancestors_end_with_ascii_ci(
    tree: &StorageTree,
    mut parent_id: NodeId,
    mut remaining: &[u8],
) -> bool {
    let root_id = tree.root_id();
    while !remaining.is_empty() {
        if parent_id == root_id {
            return false;
        }
        let Some(parent) = tree.node(parent_id) else {
            return false;
        };
        let pname = parent.name().as_bytes();
        if let Some(slash_pos) = remaining.iter().rposition(|&b| b == b'/') {
            let seg = &remaining[slash_pos + 1..];
            if !pname.eq_ignore_ascii_case(seg) {
                return false;
            }
            remaining = &remaining[..slash_pos];
            let Some(next_parent) = parent.parent() else {
                return false;
            };
            parent_id = next_parent;
        } else {
            return pname
                .len()
                .checked_sub(remaining.len())
                .is_some_and(|start| pname[start..].eq_ignore_ascii_case(remaining));
        }
    }
    true
}

fn cmp_ascii_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()))
        .then_with(|| a.cmp(b))
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
        assert_eq!(tree.node(log_hits[0]).expect("hit").name(), "old.log");

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
                .any(|id| tree.node(*id).is_some_and(|n| n.name() == "old.log"))
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
            .filter_map(|id| tree.node(*id).map(NodeRef::name))
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
            .filter_map(|id| tree.node(*id).map(NodeRef::name))
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
            .filter_map(|id| tree.node(*id).map(NodeRef::name))
            .collect();
        assert_eq!(subpath_names, vec!["data"]);

        let deep_subpath = search_tree(
            &tree,
            &SearchQuery {
                text: "REPORT-CORE/Data/8a4f".to_owned(),
                ..SearchQuery::default()
            },
        );
        let deep_names: Vec<&str> = deep_subpath
            .iter()
            .filter_map(|id| tree.node(*id).map(NodeRef::name))
            .collect();
        assert_eq!(deep_names, vec!["8a4f9c.webm"]);

        let ancestor_only = search_tree(
            &tree,
            &SearchQuery {
                text: "playwright-report-core/other".to_owned(),
                ..SearchQuery::default()
            },
        );
        assert!(ancestor_only.is_empty());
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
    fn truncate_and_sort_nodes_keeps_exact_top_n_and_bounded_search_respects_cache_cap() {
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
            .filter_map(|id| tree.node(*id).map(NodeRef::name))
            .collect();
        assert_eq!(
            names,
            vec!["item_12.dat", "item_11.dat", "item_10.dat", "item_09.dat"]
        );

        let (bounded_top, small_cache) =
            search_tree_bounded(&tree, &SearchQuery::default(), None, 4, 20);
        assert_eq!(bounded_top, ids);
        assert_eq!(small_cache.as_ref().map(Vec::len), Some(12));

        let (bounded_uncached, overflow_cache) =
            search_tree_bounded(&tree, &SearchQuery::default(), None, 4, 5);
        assert_eq!(bounded_uncached, ids);
        assert!(overflow_cache.is_none());
    }
}
