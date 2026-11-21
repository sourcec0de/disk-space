//! Reactive per-surface state, background scan execution, and destruction orchestration.

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
use disk_space_platform::PlatformHost;
use disk_space_platform::StorageCategory;
use disk_space_platform::native_platform;
use nsh_core::LayoutAnimation;
use nsh_core::LayoutCurve;
use nsh_core::LayoutTransition;
use nsh_core::prelude::Owner;
use nsh_core::prelude::Signal;
use nsh_core::prelude::animate_layout;
use nsh_core::prelude::batch;
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
    pub(crate) selected_row: usize,
    pub(crate) search_text: String,
    pub(crate) filter_mode: FilterMode,
    pub(crate) sort_order: SortOrder,
    pub(crate) view_tab: ViewTab,
    pub(crate) aggregate_kind: AggregateKind,
    pub(crate) hover_category: Option<StorageCategory>,
    pub(crate) hover_sunburst: Option<NodeId>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SnagState {
    pub(crate) queue: SnagQueue,
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

    browse_signal().update(|browse| {
        browse.current_node = None;
        browse.selected_row = 0;
        browse.hover_sunburst = None;
    });

    batch(|| {
        snag_signal().update(|snag| {
            snag.queue.clear();
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
    raw: Vec<NodeId>,
    queue: &SnagQueue,
    exiting: Option<&HashSet<NodeId>>,
) -> Vec<NodeId> {
    raw.into_iter()
        .filter(|&id| {
            if exiting.is_some_and(|set| set.contains(&id)) {
                return true;
            }
            tree.node(id)
                .is_some_and(|node| !queue.is_covered(&node.relative_path))
        })
        .collect()
}

/// Computes active [`NodeId`]s in the explorer list (excluding snagged items).
pub(crate) fn visible_items(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get();
    let raw = raw_scope_items(tree, &browse);
    snag_signal().with(|snag| filter_uncovered(tree, raw, &snag.queue, None))
}

fn visible_items_untracked(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get_untracked();
    let raw = raw_scope_items(tree, &browse);
    snag_signal().with_untracked(|snag| filter_uncovered(tree, raw, &snag.queue, None))
}

/// Computes [`NodeId`]s mounted in the explorer list, including rows mid pop-out animation.
pub(crate) fn explorer_display_items(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get();
    let raw = raw_scope_items(tree, &browse);
    exiting_signal().with(|exiting| {
        snag_signal().with(|snag| filter_uncovered(tree, raw, &snag.queue, Some(&exiting.nodes)))
    })
}

fn explorer_display_items_untracked(tree: &StorageTree) -> Vec<NodeId> {
    let browse = browse_signal().get_untracked();
    let raw = raw_scope_items(tree, &browse);
    exiting_signal().with_untracked(|exiting| {
        snag_signal()
            .with_untracked(|snag| filter_uncovered(tree, raw, &snag.queue, Some(&exiting.nodes)))
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

/// Returns `true` when `node_id` is the actively selected row in `visible_items`.
pub(crate) fn is_row_selected(tree: &StorageTree, node_id: NodeId) -> bool {
    let browse = browse_signal().get();
    let visible = snag_signal().with(|snag| {
        let raw = raw_scope_items(tree, &browse);
        filter_uncovered(tree, raw, &snag.queue, None)
    });
    if visible.is_empty() {
        return false;
    }
    let idx = browse.selected_row.min(visible.len() - 1);
    visible.get(idx).copied() == Some(node_id)
}

/// Untracked check used in click handlers to determine whether `node_id` is currently selected.
pub(crate) fn is_row_selected_untracked(tree: &StorageTree, node_id: NodeId) -> bool {
    let browse = browse_signal().get_untracked();
    let visible = snag_signal().with_untracked(|snag| {
        let raw = raw_scope_items(tree, &browse);
        filter_uncovered(tree, raw, &snag.queue, None)
    });
    if visible.is_empty() {
        return false;
    }
    let idx = browse.selected_row.min(visible.len() - 1);
    visible.get(idx).copied() == Some(node_id)
}

/// Selects `node_id` by updating `selected_row` to its index inside `visible_items`.
pub(crate) fn select_visible_node(tree: &StorageTree, node_id: NodeId) {
    let visible = visible_items_untracked(tree);
    if let Some(pos) = visible.iter().position(|&id| id == node_id) {
        browse_signal().update(|browse| browse.selected_row = pos);
    }
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
    browse_signal().update(|browse| {
        browse.selected_category = category;
        browse.current_node = None;
        browse.selected_row = 0;
        browse.hover_sunburst = None;
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
        browse_signal().update(|browse| {
            browse.current_node = Some(node_id);
            browse.selected_row = 0;
            browse.search_text.clear();
            browse.hover_sunburst = None;
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
            browse.selected_row = 0;
        }
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
        browse_signal().update(|browse| {
            browse.current_node = Some(target_id);
            browse.selected_row = 0;
        });
    } else if let Some(parent) = target.parent {
        let sibling_idx = tree
            .children_sorted(parent)
            .iter()
            .position(|&id| id == target_id)
            .unwrap_or(0);
        browse_signal().update(|browse| {
            browse.current_node = Some(parent);
            browse.selected_row = sibling_idx;
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

/// Stages `node_id` for destruction and triggers its pop-out exit animation from the explorer.
pub(crate) fn snag_node(node_id: NodeId) {
    let Some(tree) = current_tree_untracked() else {
        return;
    };
    let Some(node) = tree.node(node_id) else {
        return;
    };
    let rel = node.relative_path.clone();

    let already_snagged_or_exiting = snag_signal()
        .with_untracked(|snag| snag.queue.is_covered(&rel))
        || exiting_signal().with_untracked(|exiting| exiting.nodes.contains(&node_id));
    if already_snagged_or_exiting {
        return;
    }

    let host = create_host();
    let mut queue_copy = snag_signal().with_untracked(|snag| snag.queue.clone());
    match queue_copy.snag(tree.root(), node, host.as_ref()) {
        Ok(SnagOutcome::Added {
            consolidated_children,
        }) => {
            let mut newly_exiting: Vec<NodeId> = explorer_display_items_untracked(&tree)
                .into_iter()
                .filter(|&id| {
                    tree.node(id)
                        .is_some_and(|candidate| candidate.relative_path.starts_with(&rel))
                })
                .collect();
            if !newly_exiting.contains(&node_id) {
                newly_exiting.push(node_id);
            }

            let exiting_batch = newly_exiting.clone();
            batch(|| {
                exiting_signal().update(|exiting| {
                    exiting.nodes.extend(exiting_batch);
                });
                snag_signal().update(|snag| {
                    snag.queue = queue_copy;
                    snag.drawer_open = true;
                });
            });

            let msg = if consolidated_children > 0 {
                format!(
                    "Snagged `{}` (consolidated {consolidated_children} child item(s))",
                    node.name
                )
            } else {
                format!(
                    "Snagged `{}` ({})",
                    node.name,
                    format_bytes(node.total_bytes)
                )
            };
            set_notice(NoticeTone::Info, msg);

            spawn_app_task(async move {
                timer(Duration::from_millis(270)).await;
                let still_exiting = exiting_signal().with_untracked(|exiting| {
                    newly_exiting.iter().any(|id| exiting.nodes.contains(id))
                });
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
        Ok(SnagOutcome::AlreadyCoveredByAncestor { ancestor }) => {
            set_notice(
                NoticeTone::Warn,
                format!(
                    "Already covered by staged ancestor `{}`",
                    ancestor.display()
                ),
            );
        }
        Ok(SnagOutcome::AlreadyPresent | _) => {}
        Err(err) => {
            set_notice(NoticeTone::Danger, format!("Blocked by safety rail: {err}"));
        }
    }
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
            snag.queue.remove(relative_path);
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
        snag_signal().update(|snag| snag.queue.clear());
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
            for rel in &succeeded_relatives {
                snag.queue.remove(rel);
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
    let row_idx = tree
        .children_sorted(parent)
        .iter()
        .position(|&id| id == node_id)
        .unwrap_or(0);
    browse_signal().update(|browse| {
        browse.current_node = Some(parent);
        browse.selected_row = row_idx;
        browse.view_tab = ViewTab::DotMatrix;
    });
}

/// Formats a relative path display string for an item inside the tree.
pub(crate) fn display_abs_path(path: &Path) -> String {
    disk_space_core::format_display_path(path, home_dir().as_deref())
}
