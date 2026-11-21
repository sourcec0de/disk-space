//! Core storage scanning, safety-jail enforcement, aggregation, search, and
//! secure erasure logic for `disk-space`.
//!
//! This crate forbids `unsafe` code entirely and operates exclusively against
//! [`disk_space_platform::PlatformHost`] abstractions.

#![forbid(unsafe_code)]

pub mod format;
pub mod jail;
pub mod scanner;
pub mod search;
pub mod snag;
pub mod tree;

pub use format::AgeFreshness;
pub use format::FormattedAge;
pub use format::format_age;
pub use format::format_bytes;
pub use format::format_display_path;
pub use jail::CrossMountPolicy;
pub use jail::JailError;
pub use jail::JailedPath;
pub use jail::ScanRoot;
pub use scanner::ScanCancelHandle;
pub use scanner::ScanOptions;
pub use scanner::ScanProgress;
pub use scanner::scan_root;
pub use search::AggregateReport;
pub use search::ExtensionAggregate;
pub use search::FilterMode;
pub use search::MAX_SEARCH_RESULTS;
pub use search::SearchQuery;
pub use search::SortOrder;
pub use search::compute_aggregates;
pub use search::search_tree;
pub use search::search_tree_all;
pub use search::search_tree_bounded;
pub use search::truncate_and_sort_nodes;
pub use snag::DestructionMode;
pub use snag::DestructionProgress;
pub use snag::DestructionReport;
pub use snag::ErasePasses;
pub use snag::SnagItem;
pub use snag::SnagOutcome;
pub use snag::SnagQueue;
pub use snag::execute_destruction;
pub use tree::CategoryTotals;
pub use tree::CulpritSummary;
pub use tree::NodeId;
pub use tree::NodeRef;
pub use tree::StorageTree;
pub use tree::SubtreeIter;
