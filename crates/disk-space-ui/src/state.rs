//! Reactive per-surface state, background scan execution, and destruction orchestration.

use std::cell::RefCell;
use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::thread;
use std::time::Duration;

use crossbeam_channel::TryRecvError;
use disk_space_core::DestructionMode;
use disk_space_core::DestructionProgress;
use disk_space_core::DestructionReport;
use disk_space_core::FilterMode;
use disk_space_core::MAX_SEARCH_RESULTS;
use disk_space_core::NodeId;
use disk_space_core::ScanCancelHandle;
use disk_space_core::ScanOptions;
use disk_space_core::ScanProgress;
use disk_space_core::ScanRoot;
use disk_space_core::SearchQuery;
use disk_space_core::SnagOutcome;
use disk_space_core::SnagQueue;
use disk_space_core::SortOrder;
use disk_space_core::StorageTree;
use disk_space_core::execute_destruction;
use disk_space_core::format_bytes;
use disk_space_core::scan_root;
use disk_space_core::search_tree;
use disk_space_core::search_tree_all;
use disk_space_core::truncate_and_sort_nodes;
use disk_space_platform::PlatformHost;
use disk_space_platform::StorageCategory;
use disk_space_platform::native_platform;
use nsh_core::LayoutAnimation;
use nsh_core::LayoutCurve;
use nsh_core::LayoutTransition;
use nsh_core::prelude::Memo;
use nsh_core::prelude::Owner;
use nsh_core::prelude::Signal;
use nsh_core::prelude::animate_layout;
use nsh_core::prelude::batch;
use nsh_core::prelude::computed;
use nsh_core::prelude::global;
use nsh_core::prelude::spawn;
use nsh_core::prelude::timer;

/// Process-level startup configuration injected by the binary or headless tests.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LaunchConfig {
    /// Optional initial directory to scan instead of the user's home directory.
    pub initial_root: Option<PathBuf>,
    /// Platform host factory used for OS operations (or [`disk_space_platform::FakePlatform`] in tests).
    pub host_factory: fn() -> Box<dyn PlatformHost>,
}

impl LaunchConfig {
    /// Constructs a [`LaunchConfig`] for launching the desktop or headless app.
    #[must_use]
    pub const fn new(
        initial_root: Option<PathBuf>,
        host_factory: fn() -> Box<dyn PlatformHost>,
    ) -> Self {
        Self {
            initial_root,
            host_factory,
        }
    }
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            initial_root: None,
            host_factory: native_platform,
        }
    }
}

static LAUNCH_CONFIG: OnceLock<RwLock<LaunchConfig>> = OnceLock::new();

fn config_lock() -> &'static RwLock<LaunchConfig> {
    LAUNCH_CONFIG.get_or_init(|| RwLock::new(LaunchConfig::default()))
}

/// Configures the initial root and platform host factory prior to mounting [`crate::App`].
pub fn configure_launch(config: LaunchConfig) {
    if let Ok(mut guard) = config_lock().write() {
        *guard = config;
    }
}

pub(crate) fn create_host() -> Box<dyn PlatformHost> {
    let default_factory: fn() -> Box<dyn PlatformHost> = native_platform;
    let factory = config_lock()
        .read()
        .map_or(default_factory, |cfg| cfg.host_factory);
    factory()
}

fn default_initial_root() -> PathBuf {
    if let Ok(guard) = config_lock().read()
        && let Some(ref custom) = guard.initial_root
    {
        return custom.clone();
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Active primary visualization tab in the top workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ViewTab {
    #[default]
    DotMatrix,
    Sunburst,
    Aggregates,
}

/// Sub-tab shown when [`ViewTab::Aggregates`] is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AggregateKind {
    #[default]
    ByExtension,
    HeavyFolders,
    JunkClusters,
}

/// Scan lifecycle state.
#[derive(Debug, Clone, Default)]
pub(crate) enum ScanPhase {
    #[default]
    Idle,
    Scanning(ScanProgress),
    Ready(Arc<StorageTree>),
    Failed(String),
}

/// Severity tone for transient status notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    Info,
    Warn,
    Danger,
}

/// Transient top-bar notification banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusNotice {
    pub(crate) tone: NoticeTone,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScanState {
    pub(crate) started: bool,
    pub(crate) app_owner: Option<Owner>,
    pub(crate) phase: ScanPhase,
    pub(crate) root_path: Option<PathBuf>,
    pub(crate) generation: u64,
    pub(crate) cancel: Option<ScanCancelHandle>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BrowseState {
    pub(crate) selected_category: Option<StorageCategory>,
    pub(crate) current_node: Option<NodeId>,
    pub(crate) search_text: String,
    pub(crate) filter_mode: FilterMode,
    pub(crate) sort_order: SortOrder,
    pub(crate) view_tab: ViewTab,
    pub(crate) aggregate_kind: AggregateKind,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HoverState {
    pub(crate) category: Option<StorageCategory>,
    pub(crate) sunburst: Option<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListMove {
    Up,
    Down,
    PageUp(usize),
    PageDown(usize),
    First,
    Last,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SelectionState {
    pub(crate) cursor: Option<NodeId>,
    pub(crate) anchor: Option<NodeId>,
    pub(crate) selected: HashSet<NodeId>,
}

impl SelectionState {
    pub(crate) fn clear(&mut self) {
        self.cursor = None;
        self.anchor = None;
        self.selected.clear();
    }

    pub(crate) fn select_single(&mut self, id: NodeId) {
        self.cursor = Some(id);
        self.anchor = Some(id);
        self.selected.clear();
        self.selected.insert(id);
    }

    pub(crate) fn reconcile(&mut self, visible: &[NodeId]) {
        if visible.is_empty() {
            self.clear();
            return;
        }
        let visible_set: HashSet<NodeId> = visible.iter().copied().collect();
        self.selected.retain(|id| visible_set.contains(id));

        if self.cursor.is_none_or(|id| !visible_set.contains(&id)) {
            let next = visible
                .iter()
                .copied()
                .find(|id| self.selected.contains(id))
                .unwrap_or(visible[0]);
            self.cursor = Some(next);
            if self.selected.is_empty() {
                self.selected.insert(next);
            }
        }
        if self.anchor.is_none_or(|id| !visible_set.contains(&id)) {
            self.anchor = self.cursor;
        }
        if self.selected.is_empty()
            && let Some(cur) = self.cursor
        {
            self.selected.insert(cur);
        }
    }

    pub(crate) fn move_cursor(
        &mut self,
        visible: &[NodeId],
        step: ListMove,
        extend: bool,
    ) -> Option<usize> {
        let last = visible.len().checked_sub(1)?;
        let cur_idx = self
            .cursor
            .and_then(|id| visible.iter().position(|&v| v == id));

        let next_idx = match (step, cur_idx) {
            (ListMove::First, _) | (ListMove::Down | ListMove::PageDown(_), None) => 0,
            (ListMove::Last, _) | (ListMove::Up | ListMove::PageUp(_), None) => last,
            (ListMove::Up, Some(idx)) => idx.saturating_sub(1),
            (ListMove::Down, Some(idx)) => (idx + 1).min(last),
            (ListMove::PageUp(page), Some(idx)) => idx.saturating_sub(page.max(1)),
            (ListMove::PageDown(page), Some(idx)) => (idx + page.max(1)).min(last),
        };

        if !extend {
            self.select_single(visible[next_idx]);
            return Some(next_idx);
        }

        let anchor_idx = self
            .anchor
            .and_then(|id| visible.iter().position(|&v| v == id))
            .or(cur_idx)
            .unwrap_or(next_idx);
        let anchor_id = visible[anchor_idx];
        let cursor_id = visible[next_idx];
        let (lo, hi) = if anchor_idx <= next_idx {
            (anchor_idx, next_idx)
        } else {
            (next_idx, anchor_idx)
        };

        self.anchor = Some(anchor_id);
        self.cursor = Some(cursor_id);
        self.selected.clear();
        self.selected.extend(visible[lo..=hi].iter().copied());
        Some(next_idx)
    }

    pub(crate) fn select_range_to(&mut self, visible: &[NodeId], target: NodeId) -> Option<usize> {
        let target_idx = visible.iter().position(|&v| v == target)?;
        let anchor_idx = self
            .anchor
            .and_then(|id| visible.iter().position(|&v| v == id))
            .or_else(|| {
                self.cursor
                    .and_then(|id| visible.iter().position(|&v| v == id))
            })
            .unwrap_or(target_idx);
        let (lo, hi) = if anchor_idx <= target_idx {
            (anchor_idx, target_idx)
        } else {
            (target_idx, anchor_idx)
        };
        self.anchor = Some(visible[anchor_idx]);
        self.cursor = Some(target);
        self.selected.clear();
        self.selected.extend(visible[lo..=hi].iter().copied());
        Some(target_idx)
    }

    pub(crate) fn toggle_item(&mut self, id: NodeId) {
        if self.selected.remove(&id) {
            if self.cursor == Some(id) {
                self.cursor = self.selected.iter().copied().next();
            }
            if self.anchor == Some(id) {
                self.anchor = self.cursor;
            }
        } else {
            self.selected.insert(id);
            self.cursor = Some(id);
            self.anchor = Some(id);
        }
    }

    pub(crate) fn select_all(&mut self, visible: &[NodeId]) {
        let Some(&first) = visible.first() else {
            return;
        };
        self.anchor = self
            .anchor
            .filter(|id| visible.contains(id))
            .or(Some(first));
        self.cursor = self
            .cursor
            .filter(|id| visible.contains(id))
            .or(Some(first));
        self.selected.clear();
        self.selected.extend(visible.iter().copied());
    }

    pub(crate) fn collapse_to_cursor(&mut self) {
        if let Some(cur) = self.cursor {
            self.select_single(cur);
        } else {
            self.clear();
        }
    }

    pub(crate) fn selected_in_order(&self, visible: &[NodeId]) -> Vec<NodeId> {
        visible
            .iter()
            .copied()
            .filter(|id| self.selected.contains(id))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SnagState {
    pub(crate) queue: SnagQueue,
    pub(crate) queue_version: u64,
    pub(crate) drawer_open: bool,
    pub(crate) mode: DestructionMode,
    pub(crate) confirm_open: bool,
    pub(crate) erase_input: String,
    pub(crate) running: bool,
    pub(crate) progress: Option<DestructionProgress>,
    pub(crate) last_report: Option<DestructionReport>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExitingState {
    pub(crate) nodes: HashSet<NodeId>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NoticeState {
    pub(crate) notice: Option<StatusNotice>,
}

pub(crate) fn scan_signal() -> Signal<ScanState> {
    global::<ScanState>()
}

pub(crate) fn browse_signal() -> Signal<BrowseState> {
    global::<BrowseState>()
}

pub(crate) fn hover_signal() -> Signal<HoverState> {
    global::<HoverState>()
}

pub(crate) fn selection_signal() -> Signal<SelectionState> {
    global::<SelectionState>()
}

pub(crate) fn snag_signal() -> Signal<SnagState> {
    global::<SnagState>()
}

pub(crate) fn exiting_signal() -> Signal<ExitingState> {
    global::<ExitingState>()
}

pub(crate) fn notice_signal() -> Signal<NoticeState> {
    global::<NoticeState>()
}

pub(crate) fn ensure_started() {
    let should_start = scan_signal().with_untracked(|state| !state.started);
    if should_start {
        let root_owner = Owner::current();
        scan_signal().update(|state| state.app_owner = Some(root_owner));
        let initial = default_initial_root();
        start_scan(initial);
    }
}

/// Spawns a future on the long-lived root `App` owner so `dynamic()` rebuilds never cancel it.
pub(crate) fn spawn_app_task(future: impl Future<Output = ()> + 'static) {
    let owner = scan_signal()
        .with_untracked(|state| state.app_owner)
        .unwrap_or_else(Owner::current);
    owner.run(|| {
        spawn(future);
    });
}

enum ScanWorkerMsg {
    Progress(ScanProgress),
    Done(Result<StorageTree, String>),
}

/// Cancels any active scan and starts scanning `root_path` in a background thread.
pub(crate) fn start_scan(root_path: PathBuf) {
    let cancel = ScanCancelHandle::new();
    let generation = scan_signal().with_untracked(|state| state.generation.saturating_add(1));

    if let Some(prev) = scan_signal().with_untracked(|state| state.cancel.clone()) {
        prev.cancel();
    }

    scan_signal().update(|state| {
        state.started = true;
        state.generation = generation;
        state.root_path = Some(root_path.clone());
        state.cancel = Some(cancel.clone());
        state.phase = ScanPhase::Idle;
    });

    batch(|| {
        browse_signal().update(|browse| {
            browse.current_node = None;
        });
        hover_signal().update(|hover| {
            hover.sunburst = None;
        });
        selection_signal().update(SelectionState::clear);
        snag_signal().update(|snag| {
            snag.queue.clear();
            snag.queue_version = snag.queue_version.saturating_add(1);
            snag.confirm_open = false;
            snag.erase_input.clear();
            snag.last_report = None;
        });
        exiting_signal().update(|exiting| exiting.nodes.clear());
    });

    let (tx, rx) = crossbeam_channel::unbounded::<ScanWorkerMsg>();
    let worker_cancel = cancel;
    thread::spawn(move || {
        let host = create_host();
        let outcome = match ScanRoot::new(&root_path, host.as_ref()) {
            Ok(root) => scan_root(
                root,
                host.as_ref(),
                &ScanOptions::default(),
                &worker_cancel,
                &mut |progress| {
                    let _ = tx.send(ScanWorkerMsg::Progress(progress));
                },
            )
            .map_err(|err| err.to_string()),
            Err(err) => Err(err.to_string()),
        };
        let _ = tx.send(ScanWorkerMsg::Done(outcome));
    });

    spawn_app_task(async move {
        loop {
            let current_gen = scan_signal().with_untracked(|state| state.generation);
            if current_gen != generation {
                break;
            }

            let mut latest_progress = None;
            let mut finished = None;

            loop {
                match rx.try_recv() {
                    Ok(ScanWorkerMsg::Progress(prog)) => latest_progress = Some(prog),
                    Ok(ScanWorkerMsg::Done(res)) => {
                        finished = Some(res);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some(Err("scan worker terminated unexpectedly".to_owned()));
                        break;
                    }
                }
            }

            if let Some(res) = finished {
                scan_signal().update(|state| {
                    if state.generation == generation {
                        state.phase = match res {
                            Ok(tree) => ScanPhase::Ready(Arc::new(tree)),
                            Err(msg) => ScanPhase::Failed(msg),
                        };
                        state.cancel = None;
                    }
                });
                break;
            }

            if let Some(prog) = latest_progress {
                scan_signal().update(|state| {
                    if state.generation == generation {
                        state.phase = ScanPhase::Scanning(prog);
                    }
                });
            }

            timer(Duration::from_millis(50)).await;
        }
    });
}

/// Returns the currently loaded [`StorageTree`] snapshot, if ready.
pub(crate) fn current_tree() -> Option<Arc<StorageTree>> {
    scan_signal().with(|state| match &state.phase {
        ScanPhase::Ready(tree) => Some(Arc::clone(tree)),
        ScanPhase::Idle | ScanPhase::Scanning(_) | ScanPhase::Failed(_) => None,
    })
}

/// Returns the active [`StorageTree`] without subscribing to signal updates.
pub(crate) fn current_tree_untracked() -> Option<Arc<StorageTree>> {
    scan_signal().with_untracked(|state| match &state.phase {
        ScanPhase::Ready(tree) => Some(Arc::clone(tree)),
        ScanPhase::Idle | ScanPhase::Scanning(_) | ScanPhase::Failed(_) => None,
    })
}

#[derive(Clone, PartialEq, Eq)]
struct ListQueryKey {
    current_node: Option<NodeId>,
    selected_category: Option<StorageCategory>,
    search_text: String,
    filter_mode: FilterMode,
    sort_order: SortOrder,
}

struct NarrowCache {
    scope_node: Option<NodeId>,
    category: Option<StorageCategory>,
    filter_mode: FilterMode,
    needle: String,
    all_matches: Vec<NodeId>,
}

impl NarrowCache {
    fn can_narrow(
        &self,
        scope_node: Option<NodeId>,
        category: Option<StorageCategory>,
        filter_mode: FilterMode,
        next_needle: &str,
    ) -> bool {
        self.scope_node == scope_node
            && self.category == category
            && self.filter_mode == filter_mode
            && is_plain_name_query(&self.needle)
            && is_plain_name_query(next_needle)
            && next_needle.contains(self.needle.as_str())
    }
}

fn is_plain_name_query(trimmed: &str) -> bool {
    !trimmed.is_empty()
        && !trimmed.starts_with('.')
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
}

/// Memoized explorer list state built once per [`StorageTree`] snapshot.
#[derive(Clone, Copy)]
pub(crate) struct ExplorerListData {
    pub(crate) visible_items: Memo<Arc<Vec<NodeId>>>,
    pub(crate) display_items: Memo<Arc<Vec<NodeId>>>,
}

fn compute_raw_list_items(
    tree: &StorageTree,
    key: &ListQueryKey,
    narrow_cache: &RefCell<Option<NarrowCache>>,
) -> Arc<Vec<NodeId>> {
    let trimmed_search = key.search_text.trim();
    if !trimmed_search.is_empty() || key.filter_mode == FilterMode::JunkAndUnusedOnly {
        let needle = trimmed_search.to_ascii_lowercase();
        let mut query = SearchQuery::default();
        trimmed_search.clone_into(&mut query.text);
        query.category = key.selected_category;
        query.filter_mode = key.filter_mode;
        query.sort = key.sort_order;
        query.scope_node = key.current_node;

        let all_matches = {
            let cached = narrow_cache.borrow();
            cached
                .as_ref()
                .filter(|c| {
                    c.can_narrow(
                        key.current_node,
                        key.selected_category,
                        key.filter_mode,
                        &needle,
                    )
                })
                .map_or_else(
                    || search_tree_all(tree, &query, None),
                    |cache| {
                        if cache.needle == needle {
                            cache.all_matches.clone()
                        } else {
                            search_tree_all(tree, &query, Some(&cache.all_matches))
                        }
                    },
                )
        };

        *narrow_cache.borrow_mut() = Some(NarrowCache {
            scope_node: key.current_node,
            category: key.selected_category,
            filter_mode: key.filter_mode,
            needle,
            all_matches: all_matches.clone(),
        });

        let mut top = all_matches;
        truncate_and_sort_nodes(tree, &mut top, key.sort_order, MAX_SEARCH_RESULTS);
        return Arc::new(top);
    }

    *narrow_cache.borrow_mut() = None;
    let mut ids = match (key.current_node, key.selected_category) {
        (Some(node_id), _) => tree.children_sorted(node_id),
        (None, Some(category)) => tree.category_entries(category),
        (None, None) => tree.children_sorted(tree.root_id()),
    };
    if key.sort_order != SortOrder::SizeDesc {
        disk_space_core::search::sort_nodes(tree, &mut ids, key.sort_order);
    }
    Arc::new(ids)
}

impl ExplorerListData {
    pub(crate) fn new(tree: Arc<StorageTree>) -> Self {
        let query_key = computed(move || {
            browse_signal().with(|b| ListQueryKey {
                current_node: b.current_node,
                selected_category: b.selected_category,
                search_text: b.search_text.clone(),
                filter_mode: b.filter_mode,
                sort_order: b.sort_order,
            })
        });

        let narrow_cache: RefCell<Option<NarrowCache>> = RefCell::new(None);
        let raw_tree = Arc::clone(&tree);
        let raw_items = computed(move || {
            let key = query_key.get();
            compute_raw_list_items(&raw_tree, &key, &narrow_cache)
        });

        let queue_ver = computed(move || snag_signal().with(|s| s.queue_version));

        let vis_tree = Arc::clone(&tree);
        let visible_items = computed(move || {
            let _ver = queue_ver.get();
            raw_items.with(|raw| {
                snag_signal().with_untracked(|snag| {
                    if snag.queue.items().is_empty() {
                        return Arc::clone(raw);
                    }
                    Arc::new(filter_uncovered(&vis_tree, raw, &snag.queue, None))
                })
            })
        });

        let display_items = computed(move || {
            let _ver = queue_ver.get();
            exiting_signal().with(|exiting| {
                if exiting.nodes.is_empty() {
                    return visible_items.get();
                }
                raw_items.with(|raw| {
                    snag_signal().with_untracked(|snag| {
                        Arc::new(filter_uncovered(
                            &tree,
                            raw,
                            &snag.queue,
                            Some(&exiting.nodes),
                        ))
                    })
                })
            })
        });

        Self {
            visible_items,
            display_items,
        }
    }
}

/// Computes raw unsorted/sorted [`NodeId`]s matching the current scope and query.
fn raw_scope_items(tree: &StorageTree, browse: &BrowseState) -> Vec<NodeId> {
    let trimmed_search = browse.search_text.trim();
    let has_search = !trimmed_search.is_empty();

    if has_search || browse.filter_mode == FilterMode::JunkAndUnusedOnly {
        let mut query = SearchQuery::default();
        trimmed_search.clone_into(&mut query.text);
        query.category = browse.selected_category;
        query.filter_mode = browse.filter_mode;
        query.sort = browse.sort_order;
        query.scope_node = browse.current_node;
        return search_tree(tree, &query);
    }

    let mut ids = match (browse.current_node, browse.selected_category) {
        (Some(node_id), _) => tree.children_sorted(node_id),
        (None, Some(category)) => tree.category_entries(category),
        (None, None) => tree.children_sorted(tree.root_id()),
    };

    if browse.sort_order != SortOrder::SizeDesc {
        disk_space_core::search::sort_nodes(tree, &mut ids, browse.sort_order);
    }
    ids
}

fn filter_uncovered(
    tree: &StorageTree,
    raw: &[NodeId],
    queue: &SnagQueue,
    exiting: Option<&HashSet<NodeId>>,
) -> Vec<NodeId> {
    raw.iter()
        .copied()
        .filter(|&id| {
            if exiting.is_some_and(|set| set.contains(&id)) {
                return true;
            }
            tree.node(id)
                .is_some_and(|node| !queue.is_covered(&node.relative_path))
        })
        .collect()
}

fn visible_items_untracked(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get_untracked();
    let raw = raw_scope_items(tree, &browse);
    snag_signal().with_untracked(|snag| filter_uncovered(tree, &raw, &snag.queue, None))
}

fn explorer_display_items_untracked(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get_untracked();
    let raw = raw_scope_items(tree, &browse);
    exiting_signal().with_untracked(|exiting| {
        snag_signal()
            .with_untracked(|snag| filter_uncovered(tree, &raw, &snag.queue, Some(&exiting.nodes)))
    })
}

/// Returns total bytes in the active scope (selected directory, category, or root).
pub(crate) fn current_scope_bytes(tree: &StorageTree) -> u64 {
    browse_signal().with(|browse| {
        let header_node_id = browse.current_node.unwrap_or_else(|| tree.root_id());
        match (browse.current_node, browse.selected_category) {
            (None, Some(cat)) => tree
                .node(tree.root_id())
                .map_or(0, |n| n.category_totals.get(cat)),
            _ => tree.node(header_node_id).map_or(0, |n| n.total_bytes),
        }
    })
}

pub(crate) fn set_notice(tone: NoticeTone, text: impl Into<String>) {
    notice_signal().set(NoticeState {
        notice: Some(StatusNotice {
            tone,
            text: text.into(),
        }),
    });
}

pub(crate) fn clear_notice() {
    notice_signal().set(NoticeState { notice: None });
}

pub(crate) fn select_category(category: Option<StorageCategory>) {
    batch(|| {
        browse_signal().update(|browse| {
            browse.selected_category = category;
            browse.current_node = None;
        });
        hover_signal().update(|hover| hover.sunburst = None);
        selection_signal().update(SelectionState::clear);
    });
}

pub(crate) fn open_node(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let Some(node) = tree.node(node_id) else {
        return;
    };
    if node.kind == disk_space_platform::EntryKind::Directory {
        batch(|| {
            browse_signal().update(|browse| {
                browse.current_node = Some(node_id);
                browse.search_text.clear();
            });
            hover_signal().update(|hover| hover.sunburst = None);
            selection_signal().update(SelectionState::clear);
        });
    } else {
        let abs = tree.absolute_path(node_id);
        let host = create_host();
        if let Err(err) = host.open_item(&abs) {
            set_notice(NoticeTone::Warn, err.to_string());
        }
    }
}

pub(crate) fn ascend_directory() {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    batch(|| {
        browse_signal().update(|browse| {
            if let Some(current) = browse.current_node {
                let parent = tree.node(current).and_then(|node| node.parent);
                if current == tree.root_id()
                    || (parent == Some(tree.root_id()) && browse.selected_category.is_some())
                {
                    browse.current_node = None;
                } else {
                    browse.current_node = parent;
                }
            }
        });
        hover_signal().update(|hover| hover.sunburst = None);
        selection_signal().update(SelectionState::clear);
    });
}

pub(crate) fn jump_to_culprit(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let Some(node) = tree.node(node_id) else {
        return;
    };
    let Some(culprit) = &node.largest_culprit else {
        return;
    };
    let target_id = culprit.target_id;
    let Some(target) = tree.node(target_id) else {
        return;
    };

    if target.kind == disk_space_platform::EntryKind::Directory {
        batch(|| {
            browse_signal().update(|browse| {
                browse.current_node = Some(target_id);
            });
            selection_signal().update(SelectionState::clear);
        });
    } else if let Some(parent) = target.parent {
        batch(|| {
            browse_signal().update(|browse| {
                browse.current_node = Some(parent);
            });
            selection_signal().update(|sel| sel.select_single(target_id));
        });
    }
}

pub(crate) fn reveal_node(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let abs = tree.absolute_path(node_id);
    let host = create_host();
    if let Err(err) = host.reveal_in_file_manager(&abs) {
        set_notice(NoticeTone::Warn, err.to_string());
    }
}

/// Stages `node_id` for destruction and triggers its pop-out exit animation.
pub(crate) fn snag_node(node_id: NodeId) {
    snag_nodes_inner(&[node_id], None);
}

/// Stages a batch of selected items using the cached `display` slice to drive exit animation.
pub(crate) fn snag_nodes_in_display(node_ids: &[NodeId], display: &[NodeId]) {
    snag_nodes_inner(node_ids, Some(display));
}

struct SnagBatchResult {
    added: Vec<(NodeId, PathBuf, String, u64)>,
    consolidated_total: usize,
    already_covered: Option<PathBuf>,
    first_error: Option<String>,
}

fn collect_snag_batch(
    tree: &StorageTree,
    queue_copy: &mut SnagQueue,
    node_ids: &[NodeId],
    host: &dyn PlatformHost,
) -> SnagBatchResult {
    let mut result = SnagBatchResult {
        added: Vec::new(),
        consolidated_total: 0,
        already_covered: None,
        first_error: None,
    };

    for &node_id in node_ids {
        let Some(node) = tree.node(node_id) else {
            continue;
        };
        let rel = node.relative_path.clone();
        let already_staged_or_exiting = queue_copy.is_covered(&rel)
            || exiting_signal().with_untracked(|exiting| exiting.nodes.contains(&node_id));
        if already_staged_or_exiting {
            continue;
        }
        match queue_copy.snag(tree.root(), node, host) {
            Ok(SnagOutcome::Added {
                consolidated_children,
            }) => {
                result.consolidated_total = result
                    .consolidated_total
                    .saturating_add(consolidated_children);
                result
                    .added
                    .push((node_id, rel, node.name.clone(), node.total_bytes));
            }
            Ok(SnagOutcome::AlreadyCoveredByAncestor { ancestor }) => {
                if result.already_covered.is_none() {
                    result.already_covered = Some(ancestor);
                }
            }
            Ok(SnagOutcome::AlreadyPresent | _) => {}
            Err(err) => {
                if result.first_error.is_none() {
                    result.first_error = Some(err.to_string());
                }
            }
        }
    }

    result
}

fn next_surviving_cursor(
    tree: &StorageTree,
    queue: &SnagQueue,
    visible: &[NodeId],
    snagged_ids: &[NodeId],
) -> Option<NodeId> {
    let max_pos = snagged_ids
        .iter()
        .filter_map(|id| visible.iter().position(|v| v == id))
        .max()?;
    let after = visible.iter().skip(max_pos + 1).copied().find(|&id| {
        tree.node(id)
            .is_some_and(|n| !queue.is_covered(&n.relative_path))
    });
    after.or_else(|| {
        visible.iter().take(max_pos).rev().copied().find(|&id| {
            tree.node(id)
                .is_some_and(|n| !queue.is_covered(&n.relative_path))
        })
    })
}

fn schedule_exit_cleanup(newly_exiting: Vec<NodeId>) {
    spawn_app_task(async move {
        timer(Duration::from_millis(270)).await;
        let still_exiting = exiting_signal()
            .with_untracked(|exiting| newly_exiting.iter().any(|id| exiting.nodes.contains(id)));
        if !still_exiting {
            return;
        }
        animate_layout(LayoutAnimation {
            create: None,
            update: Some(LayoutTransition::new(LayoutCurve::EaseOut, 180.0)),
            delete: None,
        });
        exiting_signal().update(|exiting| {
            for id in &newly_exiting {
                exiting.nodes.remove(id);
            }
        });
    });
}

fn emit_snag_notice(outcome: &SnagBatchResult) {
    if let Some(err) = &outcome.first_error {
        set_notice(
            NoticeTone::Warn,
            format!(
                "Snagged {} item(s); blocked one: {err}",
                outcome.added.len()
            ),
        );
    } else if outcome.added.len() == 1 {
        let (_, _, name, bytes) = &outcome.added[0];
        let msg = if outcome.consolidated_total > 0 {
            format!(
                "Snagged `{name}` (consolidated {} child item(s))",
                outcome.consolidated_total
            )
        } else {
            format!("Snagged `{name}` ({})", format_bytes(*bytes))
        };
        set_notice(NoticeTone::Info, msg);
    } else {
        let total_bytes: u64 = outcome.added.iter().map(|(_, _, _, bytes)| *bytes).sum();
        set_notice(
            NoticeTone::Info,
            format!(
                "Snagged {} items ({})",
                outcome.added.len(),
                format_bytes(total_bytes)
            ),
        );
    }
}

fn snag_nodes_inner(node_ids: &[NodeId], display_snapshot: Option<&[NodeId]>) {
    if node_ids.is_empty() {
        return;
    }
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let host = create_host();
    let mut queue_copy = snag_signal().with_untracked(|snag| snag.queue.clone());
    let outcome = collect_snag_batch(&tree, &mut queue_copy, node_ids, host.as_ref());

    if outcome.added.is_empty() {
        if let Some(err) = outcome.first_error {
            set_notice(NoticeTone::Danger, format!("Blocked by safety rail: {err}"));
        } else if let Some(ancestor) = outcome.already_covered {
            set_notice(
                NoticeTone::Warn,
                format!(
                    "Already covered by staged ancestor `{}`",
                    ancestor.display()
                ),
            );
        }
        return;
    }

    let mut newly_exiting: Vec<NodeId> = display_snapshot
        .map_or_else(
            || explorer_display_items_untracked(&tree),
            <[NodeId]>::to_vec,
        )
        .into_iter()
        .filter(|&id| {
            tree.node(id).is_some_and(|candidate| {
                outcome
                    .added
                    .iter()
                    .any(|(_, rel, _, _)| candidate.relative_path.starts_with(rel))
            })
        })
        .collect();
    for &(id, _, _, _) in &outcome.added {
        if !newly_exiting.contains(&id) {
            newly_exiting.push(id);
        }
    }

    let next_cursor = display_snapshot.map_or_else(
        || {
            let visible_before = visible_items_untracked(&tree);
            next_surviving_cursor(&tree, &queue_copy, &visible_before, node_ids)
        },
        |slice| {
            let visible_before: Vec<NodeId> = slice
                .iter()
                .copied()
                .filter(|id| {
                    tree.node(*id).is_some_and(|n| {
                        !snag_signal().with_untracked(|s| s.queue.is_covered(&n.relative_path))
                    })
                })
                .collect();
            next_surviving_cursor(&tree, &queue_copy, &visible_before, node_ids)
        },
    );

    let exiting_batch = newly_exiting.clone();
    batch(|| {
        exiting_signal().update(|exiting| {
            exiting.nodes.extend(exiting_batch);
        });
        snag_signal().update(|snag| {
            snag.queue = queue_copy;
            snag.queue_version = snag.queue_version.saturating_add(1);
            snag.drawer_open = true;
        });
        selection_signal().update(|sel| {
            if let Some(next) = next_cursor {
                sel.select_single(next);
            } else {
                sel.clear();
            }
        });
    });

    emit_snag_notice(&outcome);
    schedule_exit_cleanup(newly_exiting);
}

/// Removes one item from the destruction queue and restores it smoothly into the explorer.
pub(crate) fn unstage_item(relative_path: &Path) {
    animate_layout(LayoutAnimation {
        create: Some(LayoutTransition::new(LayoutCurve::EaseOut, 180.0)),
        update: Some(LayoutTransition::new(LayoutCurve::EaseOut, 180.0)),
        delete: None,
    });
    let active_tree = current_tree_untracked();
    batch(|| {
        if let Some(ref tree) = active_tree {
            exiting_signal().update(|exiting| {
                exiting.nodes.retain(|&id| {
                    tree.node(id)
                        .is_none_or(|node| !node.relative_path.starts_with(relative_path))
                });
            });
        }
        snag_signal().update(|snag| {
            if snag.queue.remove(relative_path) {
                snag.queue_version = snag.queue_version.saturating_add(1);
            }
        });
    });
}

/// Clears all items from the destruction queue and restores them into the explorer.
pub(crate) fn clear_snag_queue() {
    animate_layout(LayoutAnimation {
        create: Some(LayoutTransition::new(LayoutCurve::EaseOut, 180.0)),
        update: Some(LayoutTransition::new(LayoutCurve::EaseOut, 180.0)),
        delete: None,
    });
    batch(|| {
        exiting_signal().update(|exiting| exiting.nodes.clear());
        snag_signal().update(|snag| {
            if !snag.queue.items().is_empty() {
                snag.queue.clear();
                snag.queue_version = snag.queue_version.saturating_add(1);
            }
        });
    });
}

pub(crate) fn quick_trash_node(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let Some(node) = tree.node(node_id) else {
        return;
    };
    let host = create_host();
    let mut queue = SnagQueue::new();
    match queue.snag(tree.root(), node, host.as_ref()) {
        Ok(_) => {
            snag_signal().update(|snag| {
                snag.queue = queue;
                snag.queue_version = snag.queue_version.saturating_add(1);
                snag.mode = DestructionMode::MoveToTrash;
                snag.drawer_open = true;
                snag.confirm_open = true;
                snag.erase_input.clear();
            });
        }
        Err(err) => {
            set_notice(NoticeTone::Danger, format!("Blocked by safety rail: {err}"));
        }
    }
}

enum DestroyWorkerMsg {
    Progress(DestructionProgress),
    Done(DestructionReport, Option<disk_space_platform::VolumeInfo>),
}

/// Runs the currently staged destruction queue asynchronously and prunes deleted items.
pub(crate) fn run_staged_destruction() {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let (items, mode) =
        snag_signal().with_untracked(|snag| (snag.queue.items().to_vec(), snag.mode));
    if items.is_empty() {
        return;
    }

    snag_signal().update(|snag| {
        snag.running = true;
        snag.progress = None;
        snag.last_report = None;
    });

    let root = tree.root().clone();
    let (tx, rx) = crossbeam_channel::unbounded::<DestroyWorkerMsg>();

    thread::spawn(move || {
        let host = create_host();
        let report = execute_destruction(&root, &items, mode, host.as_ref(), &mut |progress| {
            let _ = tx.send(DestroyWorkerMsg::Progress(progress));
        });
        let updated_volume = host.volume_for_path(root.canonical_path()).ok();
        let _ = tx.send(DestroyWorkerMsg::Done(report, updated_volume));
    });

    spawn_app_task(async move {
        loop {
            let mut latest_progress = None;
            let mut finished = None;

            loop {
                match rx.try_recv() {
                    Ok(DestroyWorkerMsg::Progress(prog)) => latest_progress = Some(prog),
                    Ok(DestroyWorkerMsg::Done(report, vol)) => {
                        finished = Some((report, vol));
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some((DestructionReport::default(), None));
                        break;
                    }
                }
            }

            if let Some((report, updated_volume)) = finished {
                apply_destruction_report(report, updated_volume);
                break;
            }

            if let Some(prog) = latest_progress {
                snag_signal().update(|snag| snag.progress = Some(prog));
            }

            timer(Duration::from_millis(40)).await;
        }
    });
}

fn apply_destruction_report(
    report: DestructionReport,
    updated_volume: Option<disk_space_platform::VolumeInfo>,
) {
    let succeeded_relatives = report.succeeded.clone();
    let failed_count = report.failed.len();
    let reclaimed = report.reclaimed_bytes;
    let first_error = report
        .failed
        .first()
        .map(|(p, err)| format!("{}: {err}", p.display()));

    scan_signal().update(|state| {
        if let ScanPhase::Ready(ref tree) = state.phase {
            let mut updated = (**tree).clone();
            updated.prune_deleted(&succeeded_relatives);
            if let Some(vol) = updated_volume {
                updated.set_volume(vol);
            }
            state.phase = ScanPhase::Ready(Arc::new(updated));
        }
    });

    batch(|| {
        exiting_signal().update(|exiting| exiting.nodes.clear());
        snag_signal().update(|snag| {
            let mut removed_any = false;
            for rel in &succeeded_relatives {
                removed_any |= snag.queue.remove(rel);
            }
            if removed_any {
                snag.queue_version = snag.queue_version.saturating_add(1);
            }
            snag.running = false;
            snag.confirm_open = false;
            snag.erase_input.clear();
            snag.progress = None;
            snag.last_report = Some(report);
        });
    });

    if failed_count == 0 {
        set_notice(
            NoticeTone::Info,
            format!(
                "Destroyed {} item(s) · reclaimed {}",
                succeeded_relatives.len(),
                format_bytes(reclaimed)
            ),
        );
    } else {
        let tail = first_error.unwrap_or_default();
        set_notice(
            NoticeTone::Warn,
            format!(
                "Reclaimed {} ({} succeeded, {failed_count} failed: {tail})",
                format_bytes(reclaimed),
                succeeded_relatives.len(),
            ),
        );
    }
}

/// Computes the home directory for `~` display shortening if set.
pub(crate) fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

/// Navigates to the nearest folder containing `path` inside the active scan tree.
pub(crate) fn select_node_in_parent(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let Some(node) = tree.node(node_id) else {
        return;
    };
    let parent = node.parent.unwrap_or_else(|| tree.root_id());
    batch(|| {
        browse_signal().update(|browse| {
            browse.current_node = Some(parent);
            browse.view_tab = ViewTab::DotMatrix;
        });
        hover_signal().update(|hover| hover.sunburst = None);
        selection_signal().update(|sel| sel.select_single(node_id));
    });
}

/// Formats a relative path display string for an item inside the tree.
pub(crate) fn display_abs_path(path: &Path) -> String {
    disk_space_core::format_display_path(path, home_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ids() -> Vec<NodeId> {
        // Build a tiny tree just to obtain distinct valid NodeIds.
        let outer = tempfile::tempdir().expect("tempdir");
        let root_dir = outer.path().join("sel");
        std::fs::create_dir_all(&root_dir).expect("mkdir");
        for idx in 0..5 {
            std::fs::write(root_dir.join(format!("f{idx}.txt")), vec![1_u8; 64]).expect("write");
        }
        let host = disk_space_platform::FakePlatform::new(root_dir.clone());
        let scan_root_val = ScanRoot::new(&root_dir, &host).expect("scan root");
        let tree = scan_root(
            scan_root_val,
            &host,
            &ScanOptions::default(),
            &ScanCancelHandle::new(),
            &mut |_| {},
        )
        .expect("scan");
        tree.children_sorted(tree.root_id())
    }

    #[test]
    fn selection_state_shift_extends_and_retracts_both_directions() {
        let ids = test_ids();
        assert_eq!(ids.len(), 5);
        let mut sel = SelectionState::default();
        sel.reconcile(&ids);
        assert_eq!(sel.cursor, Some(ids[0]));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[0]]);

        // Shift+Down twice -> [0, 1, 2]
        assert_eq!(sel.move_cursor(&ids, ListMove::Down, true), Some(1));
        assert_eq!(sel.move_cursor(&ids, ListMove::Down, true), Some(2));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[0], ids[1], ids[2]]);

        // Shift+Up once -> retracts back to [0, 1]
        assert_eq!(sel.move_cursor(&ids, ListMove::Up, true), Some(1));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[0], ids[1]]);

        // Plain Down moves single selection to row 2
        assert_eq!(sel.move_cursor(&ids, ListMove::Down, false), Some(2));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[2]]);
        assert_eq!(sel.anchor, Some(ids[2]));

        // Shift+Up twice from row 2 -> [0, 1, 2]
        assert_eq!(sel.move_cursor(&ids, ListMove::Up, true), Some(1));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[1], ids[2]]);
        assert_eq!(sel.move_cursor(&ids, ListMove::Up, true), Some(0));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[0], ids[1], ids[2]]);

        // Home/End clamping and PageUp/PageDown
        assert_eq!(sel.move_cursor(&ids, ListMove::Last, false), Some(4));
        assert_eq!(sel.move_cursor(&ids, ListMove::Down, false), Some(4));
        assert_eq!(sel.move_cursor(&ids, ListMove::PageUp(2), true), Some(2));
        assert_eq!(sel.selected_in_order(&ids), vec![ids[2], ids[3], ids[4]]);

        // Reconcile drops removed ids and picks surviving item
        let remaining = vec![ids[0], ids[1]];
        sel.reconcile(&remaining);
        assert_eq!(sel.cursor, Some(ids[0]));
        assert_eq!(sel.selected_in_order(&remaining), vec![ids[0]]);
    }
}
