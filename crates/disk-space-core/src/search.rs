//! Search, filtering, sorting, and storage aggregation across a [`StorageTree`].

use std::collections::HashMap;
use std::path::Path;

use disk_space_platform::EntryKind;
use disk_space_platform::StorageCategory;

use crate::tree::NodeId;
use crate::tree::StorageTree;

/// Maximum search hits returned to keep the UI responsive on massive trees.
const MAX_SEARCH_RESULTS: usize = 400;

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

/// Searches `tree` according to `query` and returns matching [`NodeId`]s.
#[must_use]
pub fn search_tree(tree: &StorageTree, query: &SearchQuery) -> Vec<NodeId> {
    let trimmed = query.text.trim().to_ascii_lowercase();
    let scope_prefix = query
        .scope_node
        .and_then(|id| tree.node(id))
        .map(|node| node.relative_path.as_path());

    let mut matches: Vec<NodeId> = tree
        .active_nodes()
        .filter(|node| node.id != tree.root_id())
        .filter(|node| match scope_prefix {
            Some(prefix) if !prefix.as_os_str().is_empty() => {
                node.relative_path.starts_with(prefix) && node.relative_path.as_path() != prefix
            }
            _ => true,
        })
        .filter(|node| {
            query
                .category
                .is_none_or(|cat| node.category == cat || node.category_totals.get(cat) > 0)
        })
        .filter(|node| match query.filter_mode {
            FilterMode::All => true,
            FilterMode::JunkAndUnusedOnly => node.junk_kind.is_some() || node.unused_bytes > 0,
        })
        .filter(|node| query.min_bytes.is_none_or(|min| node.total_bytes >= min))
        .filter(|node| matches_text(node.name.as_str(), &node.relative_path, &trimmed))
        .map(|node| node.id)
        .collect();

    sort_nodes(tree, &mut matches, query.sort);
    matches.truncate(MAX_SEARCH_RESULTS);
    matches
}

/// Sorts a slice of [`NodeId`]s according to `order`.
pub fn sort_nodes(tree: &StorageTree, ids: &mut [NodeId], order: SortOrder) {
    ids.sort_by(|a_id, b_id| {
        let Some(a) = tree.node(*a_id) else {
            return std::cmp::Ordering::Greater;
        };
        let Some(b) = tree.node(*b_id) else {
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
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
        }
    });
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
            let ext = Path::new(&node.name).extension().map_or_else(
                || "(no ext)".to_owned(),
                |e| e.to_string_lossy().to_ascii_lowercase(),
            );
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

fn matches_text(name: &str, relative_path: &Path, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if let Some(ext_query) = needle.strip_prefix('.')
        && !ext_query.is_empty()
        && !ext_query.contains('/')
    {
        return Path::new(name)
            .extension()
            .is_some_and(|ext| ext.to_string_lossy().to_ascii_lowercase() == ext_query);
    }
    name.to_ascii_lowercase().contains(needle)
        || relative_path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(needle)
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
}
