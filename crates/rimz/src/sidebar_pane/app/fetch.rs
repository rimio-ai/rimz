//! Off-thread fast folds, pane production, and room-owned request cadence.
//! The host shares one warm rollup cursor across both lanes. Heavy cache
//! refreshes run separately so render and input never block on production.
//! Every admitted request publishes a room fold; equal reductions apply only clocks.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::diag::record::TickLoop;
use crate::ids::{PaneId, SidebarInstanceId};
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::enrich::WorkspaceSnapshot;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::meter::TickMeter;
use crate::sidebar::notify::{LinkAlert, LinkNotificationState, Notification, NotificationState};
use crate::sidebar::produce::{HostInputs, PaneProducer};
#[cfg(test)]
use crate::sidebar::unread::UnreadEpisodes;
use crate::sidebar::unread::{ClearedUnread, OpenedUnread, reconcile_snapshot};
use crate::store::snapshot::{SidebarOwnView, SidebarSnapshot};
use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
use crate::{RuntimePaths, StatePaths};

use super::RoomConfig;
#[cfg(feature = "testkit")]
use super::ServeConfig;
use super::input::Wakeup;
use super::timing::tick_for;

#[cfg(feature = "testkit")]
mod concurrent;
#[cfg(feature = "testkit")]
pub use concurrent::{ConcurrentFanout, FanoutThread};

/// Run one produce behind a panic guard. An unwind keeps the renderer alive on its last good frame and degrades health; `panic = "abort"` falls back to host death and supervisor recovery.
///
/// The cursor may be torn and is reset. Producer attempts and accepted inputs remain committed in memory; metrics replace their history only after a completed sample, and projection publication records success only after the atomic write.
fn run_produce_guarded<T>(
    cursor: &mut RollupCursor,
    produce: impl FnOnce(&mut RollupCursor) -> crate::sidebar::produce::Result<T>,
) -> std::result::Result<T, String> {
    let result = super::with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| produce(cursor)))
    });
    match result {
        Ok(Ok(snapshot)) => Ok(snapshot),
        Ok(Err(err)) => Err(err.to_string()),
        Err(payload) => {
            *cursor = RollupCursor::new();
            Err(format!(
                "sidebar produce panicked: {}",
                super::panic_payload_message(payload.as_ref(), "unknown panic payload")
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FetchPhase {
    Interim,
    Final,
}

/// Typed publication from one fetch cycle. Variant shape rules out an
/// unchanged error, an interim failure, or conflicting protocol flags.
#[derive(Clone)]
pub(super) enum FetchUpdate {
    Shared {
        update: Box<FetchUpdate>,
        context: Arc<FoldShared>,
    },
    Snapshot {
        snapshot: Box<SidebarSnapshot>,
        phase: FetchPhase,
        source: SnapshotSource,
    },
    Fold {
        fold: Arc<WorkspaceFold>,
        phase: FetchPhase,
        source: SnapshotSource,
    },
    Unchanged {
        clock: SnapshotClock,
        phase: FetchPhase,
        full: Arc<FetchUpdate>,
    },
    Failed {
        error: String,
    },
}

#[derive(Default)]
pub(super) struct FoldShared {
    pub observation: Option<Arc<crate::sidebar::observe::FrameSig>>,
    pub inputs: Arc<FoldInputs>,
}

pub(super) struct FoldInputs {
    pub sampled_at: Instant,
    pub anchor: Option<crate::mux::focus_anchor::FocusAnchor>,
    pub filter: crate::sidebar::body_filter::BodyLens,
}

impl Default for FoldInputs {
    fn default() -> Self {
        Self {
            sampled_at: Instant::now(),
            anchor: None,
            filter: Default::default(),
        }
    }
}

impl FoldInputs {
    fn read(runtime: &RuntimePaths) -> Self {
        Self {
            sampled_at: Instant::now(),
            anchor: crate::mux::focus_anchor::load(runtime),
            filter: crate::sidebar::body_filter::load(runtime),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotSource {
    Cached,
    Produced,
}

/// One fold before its per-renderer projection.
#[derive(Clone)]
pub(super) struct WorkspaceFold {
    /// Taken before this fold's first input read, not at publication.
    pub(super) sampled_at: Instant,
    pub(super) workspace: WorkspaceSnapshot,
    pub(super) frame: Option<Arc<PaneFrame>>,
}

#[derive(Default)]
pub(super) struct HostData {
    pub(super) views: Arc<super::views::ViewsHandle>,
    pub(super) inputs: Option<HostInputs>,
}

#[derive(Clone, Copy)]
pub(super) struct SnapshotClock {
    now: jiff::Timestamp,
    generated_at: jiff::Timestamp,
    panes_produced_at_ms: Option<u64>,
    panes_observed_at_ms: Option<u64>,
}

impl SnapshotClock {
    pub(super) fn of(snapshot: &SidebarSnapshot) -> Self {
        Self {
            now: snapshot.now,
            generated_at: snapshot.generated_at,
            panes_produced_at_ms: snapshot.panes_produced_at_ms,
            panes_observed_at_ms: snapshot.panes_observed_at_ms,
        }
    }

    pub(super) fn apply(self, snapshot: &mut SidebarSnapshot) {
        snapshot.now = self.now;
        snapshot.generated_at = self.generated_at;
        snapshot.panes_produced_at_ms = self.panes_produced_at_ms;
        snapshot.panes_observed_at_ms = self.panes_observed_at_ms;
    }
}

impl FetchUpdate {
    pub(super) fn is_final(&self) -> bool {
        if let Self::Shared { update, .. } = self {
            return update.is_final();
        }
        !matches!(
            self,
            Self::Snapshot {
                phase: FetchPhase::Interim,
                ..
            } | Self::Fold {
                phase: FetchPhase::Interim,
                ..
            } | Self::Unchanged {
                phase: FetchPhase::Interim,
                ..
            }
        )
    }

    pub(super) fn is_success(&self) -> bool {
        match self {
            Self::Shared { update, .. } => update.is_success(),
            Self::Failed { .. } => false,
            _ => true,
        }
    }

    pub(super) fn is_unchanged(&self) -> bool {
        match self {
            Self::Shared { update, .. } => update.is_unchanged(),
            Self::Unchanged { .. } => true,
            _ => false,
        }
    }

    pub(super) fn same_content(&self, previous: &Self) -> bool {
        match (self, previous) {
            (
                Self::Shared { update, context },
                Self::Shared {
                    update: prior,
                    context: prior_context,
                },
            ) => {
                context.inputs.anchor == prior_context.inputs.anchor
                    && context.inputs.filter == prior_context.inputs.filter
                    && update.same_content(prior)
            }
            (
                Self::Snapshot { snapshot, .. },
                Self::Snapshot {
                    snapshot: prior, ..
                },
            ) => same_snapshot_content(snapshot, prior),
            _ => false,
        }
    }

    pub(super) fn unchanged(full: Arc<Self>) -> Self {
        let update = match full.as_ref() {
            Self::Shared { update, .. } => update.as_ref(),
            update => update,
        };
        match update {
            Self::Snapshot {
                snapshot, phase, ..
            } => Self::Unchanged {
                clock: SnapshotClock::of(snapshot),
                phase: *phase,
                full,
            },
            _ => (*full).clone(),
        }
    }

    /// The own view and viewed panes this update shows `pane`. A `Snapshot`
    /// is already local to its pane, a scenario's deliberately absent
    /// ownership included, so it answers as it stands; only a `Fold` derives
    /// ownership for the pane.
    pub(super) fn preview_for(&self, pane: &PaneId) -> Option<(Option<SidebarOwnView>, &[PaneId])> {
        match self {
            Self::Shared { update, .. } => update.preview_for(pane),
            Self::Snapshot { snapshot, .. } => {
                Some((snapshot.own_view.clone(), &snapshot.viewed_panes))
            }
            Self::Fold { fold, .. } => Some((
                fold.frame
                    .as_deref()
                    .and_then(|frame| SidebarOwnView::from_frame(pane, frame)),
                &fold.workspace.0.viewed_panes,
            )),
            _ => None,
        }
    }

    pub(super) fn into_parts(self) -> (Self, Option<Arc<FoldShared>>) {
        match self {
            Self::Shared { update, context } => (*update, Some(context)),
            Self::Unchanged { ref full, .. } => {
                let context = match full.as_ref() {
                    Self::Shared { context, .. } => Some(context.clone()),
                    _ => None,
                };
                (self, context)
            }
            update => (update, None),
        }
    }
}

fn shared_eq<T: PartialEq + ?Sized>(left: &Arc<T>, right: &Arc<T>) -> bool {
    Arc::ptr_eq(left, right) || left == right
}

fn reuse_shared<T: PartialEq + ?Sized>(shared: &mut Arc<T>, prior: &Arc<T>) {
    if !Arc::ptr_eq(shared, prior) && shared == prior {
        *shared = prior.clone();
    }
}

pub(super) fn same_snapshot_content(left: &SidebarSnapshot, right: &SidebarSnapshot) -> bool {
    // Exhaustive: a new snapshot field must join equality or explicitly be a clock.
    let SidebarSnapshot {
        snapshot_version,
        workspace_id,
        display_name,
        generated_at: _,
        skipped_settings,
        panes_produced_at_ms: _,
        panes_observed_at_ms: _,
        now: _,
        viewed_panes,
        client_views,
        pane_session_name,
        focused_pane,
        presence,
        truth_degraded,
        hook_drainer_outage,
        worktree_groups,
        agents,
        wired_kinds,
        wired_default_models,
        agent_panes,
        own_view,
        only_daemon_view_remains,
        project_root,
        worktree_roots,
        worktree_home,
        root_class,
        sidebar,
        theme,
        attention,
        providers,
        value_tally,
        workspace_value_tally,
        today_spend_live_usd,
        today_spend_epoch_secs,
        fleet_day_spend_usd,
        fleet_day_spend_epoch_secs,
        fleet_budget,
        link,
        reflects_log,
        fenced_sessions,
        resume_outcomes,
        parked_workflows,
        capacity_wait,
    } = left;
    shared_eq(worktree_groups, &right.worktree_groups)
        && shared_eq(agents, &right.agents)
        && shared_eq(agent_panes, &right.agent_panes)
        && shared_eq(fenced_sessions, &right.fenced_sessions)
        && match (resume_outcomes, &right.resume_outcomes) {
            (Some(left), Some(right)) => shared_eq(left, right),
            (None, None) => true,
            _ => false,
        }
        && snapshot_version == &right.snapshot_version
        && skipped_settings == &right.skipped_settings
        && workspace_id == &right.workspace_id
        && display_name == &right.display_name
        && viewed_panes == &right.viewed_panes
        && client_views == &right.client_views
        && pane_session_name == &right.pane_session_name
        && focused_pane == &right.focused_pane
        && presence == &right.presence
        && truth_degraded == &right.truth_degraded
        && hook_drainer_outage == &right.hook_drainer_outage
        && wired_kinds == &right.wired_kinds
        && wired_default_models == &right.wired_default_models
        && own_view == &right.own_view
        && only_daemon_view_remains == &right.only_daemon_view_remains
        && project_root == &right.project_root
        && worktree_roots == &right.worktree_roots
        && worktree_home == &right.worktree_home
        && root_class == &right.root_class
        && sidebar == &right.sidebar
        && theme == &right.theme
        && attention == &right.attention
        && providers == &right.providers
        && match (value_tally, &right.value_tally) {
            (Some(left), Some(right)) => shared_eq(left, right),
            (None, None) => true,
            _ => false,
        }
        && match (workspace_value_tally, &right.workspace_value_tally) {
            (Some(left), Some(right)) => shared_eq(left, right),
            (None, None) => true,
            _ => false,
        }
        && today_spend_live_usd == &right.today_spend_live_usd
        && today_spend_epoch_secs == &right.today_spend_epoch_secs
        && fleet_day_spend_usd == &right.fleet_day_spend_usd
        && fleet_day_spend_epoch_secs == &right.fleet_day_spend_epoch_secs
        && fleet_budget == &right.fleet_budget
        && link == &right.link
        && reflects_log == &right.reflects_log
        && parked_workflows == &right.parked_workflows
        && capacity_wait == &right.capacity_wait
}

/// Record every production attempt so errors and forced refreshes bound the next ordinary attempt.
#[derive(Default)]
struct ProducerCadence {
    last_attempt: Option<Instant>,
}

impl ProducerCadence {
    fn start_attempt_if_due(
        &mut self,
        mode: FetchMode,
        frame_age_ms: Option<u64>,
        tick: Duration,
        now: Instant,
    ) -> bool {
        let normal_attempt_due = self
            .last_attempt
            .is_none_or(|last| now.saturating_duration_since(last) >= tick);
        let produce = match mode {
            FetchMode::Normal => {
                normal_attempt_due && frame_age_ms.is_none_or(|age| age >= tick.as_millis() as u64)
            }
            FetchMode::FreshPanes => true,
            FetchMode::HardRefresh => true,
        };
        if produce {
            // Record before the produce path so forced and failed attempts
            // bound the next ordinary attempt too.
            self.last_attempt = Some(now);
        }
        produce
    }
}

#[derive(Default)]
struct TabNameMemo {
    frame_generation: Option<(Option<u64>, u64)>,
    attempted: HashMap<PaneId, String>,
}

impl TabNameMemo {
    fn pending(
        &mut self,
        frame: &crate::sidebar::frame::PaneFrame,
        renames: Vec<crate::sidebar::produce::tab_status::TabRename>,
    ) -> Vec<crate::sidebar::produce::tab_status::TabRename> {
        let generation = (frame.topology_stamp_ms, frame.observed_at_ms);
        if self.frame_generation != Some(generation) {
            self.frame_generation = Some(generation);
            self.attempted.clear();
        }
        renames
            .into_iter()
            .filter(|rename| {
                if self
                    .attempted
                    .get(&rename.anchor)
                    .is_some_and(|attempted| attempted == &rename.desired_name)
                {
                    return false;
                }
                self.attempted
                    .insert(rename.anchor.clone(), rename.desired_name.clone());
                true
            })
            .collect()
    }
}

/// Owns all state that persists between fetch cycles.
struct FetchWorker {
    observer: Option<crate::sidebar::observe::RoomObserver>,
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    cursor: RollupCursor,
    producer: PaneProducer,
    last_projection: Option<crate::sidebar::agent_projection::AgentProjectionPublication>,
    producer_cadence: ProducerCadence,
    notifications: NotificationState,
    remind: super::remind::RemindState,
    clock: super::clock::LoopClock,
    link_notifications: LinkNotificationState,
    meter: TickMeter,
    window: FetchWindow,
    tab_name_memo: TabNameMemo,
    session_listed: fn(crate::MuxName, &str) -> bool,
    #[cfg(test)]
    on_read: Option<Box<dyn FnMut() + Send>>,
}

struct FastFold {
    result: crate::sidebar::produce::Result<WorkspaceFold>,
    produce: bool,
}

/// A retained fetch worker and consumer cursors, without a live multiplexer.
#[cfg(feature = "testkit")]
pub struct IdleCycleFixture {
    worker: FetchWorker,
    sink: ResultSink,
    state: StatePaths,
    cursors: Vec<super::views::Cursor>,
    renderers: Vec<super::loop_state::LoopState>,
    receivers: Vec<Receiver<Wakeup>>,
}

#[cfg(feature = "testkit")]
impl IdleCycleFixture {
    pub fn new(
        state: StatePaths,
        runtime: RuntimePaths,
        mut frame: PaneFrame,
        subscribers: usize,
    ) -> Self {
        let config = RoomConfig {
            workspace_id: state.workspace_id.clone(),
            mux: crate::MuxName::Zellij,
            session_name: frame.session_name.clone(),
            tick_seconds: 1,
            timezone: jiff::tz::TimeZone::UTC,
            notification_prefs: NotificationsPrefs::default(),
        };
        let producer = PaneProducer::seeded(&runtime, &config.session_name);
        let worker = FetchWorker::new(
            config,
            runtime,
            crate::diag::DiagSink::disabled(),
            producer,
            None,
        );
        if !frame.client_views.is_empty() {
            frame.client_view_fresh = true;
        }
        let host_data = Arc::new(Mutex::new(HostData {
            inputs: Some(HostInputs::from_frame(frame)),
            ..Default::default()
        }));
        let mut subscriptions = BTreeMap::new();
        let mut receivers = Vec::new();
        for index in 0..subscribers {
            let (wake, receiver) = std::sync::mpsc::sync_channel(1);
            receivers.push(receiver);
            let instance_id = SidebarInstanceId::new();
            subscriptions.insert(
                instance_id.as_str().to_owned(),
                Subscriber {
                    instance_id,
                    own_pane: PaneId::from_parts(
                        crate::MuxName::Zellij,
                        format!("sidebar_{index}"),
                    ),
                    tick_seconds: 1,
                    completion: Arc::default(),
                    wake,
                },
            );
        }
        Self {
            worker,
            sink: ResultSink::shared(Arc::new(Mutex::new(subscriptions)), host_data),
            state,
            cursors: vec![super::views::Cursor::default(); subscribers],
            renderers: Vec::new(),
            receivers,
        }
    }

    /// Include each subscriber's real renderer application, but no painting.
    pub fn enable_renderer_apply(&mut self) {
        self.sink.begin_cycle();
        let views = self.sink.host_data.lock().expect("host data").views.clone();
        self.renderers = self
            .sink
            .cycle
            .iter()
            .map(|subscriber| {
                fixture_renderer(
                    self.worker.config.clone(),
                    self.worker.runtime.clone(),
                    subscriber,
                    views.clone(),
                    super::clock::LoopClock::wall(),
                )
            })
            .collect();
        self.cursors.clear();
    }

    /// Paint the watched consumer and verify the single-view benchmark topology.
    pub fn paint_fanout(&mut self) {
        let mut watched = 0;
        let mut prior = None;
        for renderer in &mut self.renderers {
            assert!(renderer.views.mirror().own_view.is_some());
            if renderer.bench_frame_timing(true) {
                watched += 1;
                prior = Some(renderer.views.mirror().clone());
            }
        }
        assert_eq!(watched, 1);
        self.run_content_cycle();
        let prior = prior.expect("one watched renderer");
        for renderer in &mut self.renderers {
            if renderer.bench_frame_timing(false) {
                assert_ne!(
                    prior.worktree_groups,
                    renderer.views.mirror().worktree_groups
                );
                assert_eq!(prior.own_view, renderer.views.mirror().own_view);
            }
        }
    }

    /// Route an identity-free nudge through every consumer before the next cycle. The fixture
    /// has no plane to record events, so an overlay or a focus change would reach the
    /// consumers unrecorded.
    pub fn deliver_event(&mut self, event: SidebarEvent) {
        assert!(
            !event.is_overlay() && !matches!(event, SidebarEvent::FocusChanged { .. }),
            "the fixture records nothing: deliver only nudges"
        );
        let backend = super::backend::PaneBackend::ambient(
            std::fs::File::options()
                .write(true)
                .open("/dev/null")
                .expect("null output")
                .into(),
            || {
                Ok(ratatui::backend::WindowSize {
                    columns_rows: ratatui::layout::Size::new(40, 60),
                    pixels: ratatui::layout::Size::new(400, 1200),
                })
            },
        )
        .expect("benchmark backend");
        let mut terminal = ratatui::Terminal::new(backend).expect("benchmark terminal");
        let envelope = SidebarEventEnvelope::new(
            self.state.workspace_id.clone(),
            None,
            crate::utils::time::unix_now_ms(),
            event,
        );
        for renderer in &mut self.renderers {
            renderer.on_event(&mut terminal, envelope.clone());
        }
    }

    /// Change row content without changing any consumer's own view, then fold and time frames.
    pub fn run_content_cycle(&mut self) {
        {
            let mut data = self.sink.host_data.lock().expect("host data");
            let inputs = data.inputs.as_mut().expect("seeded inputs");
            let frame = Arc::make_mut(&mut inputs.frame);
            let pane = frame
                .tabs
                .iter_mut()
                .flat_map(|tab| &mut tab.panes)
                .find(|pane| pane.pane_id.as_str() == "zellij:terminal_56")
                .expect("fanout fixture's unbound process pane");
            pane.metrics.cpu_pct = Some(if pane.metrics.cpu_pct == Some(10) {
                20
            } else {
                10
            });
            pane.metrics.rss_kb = Some(1024);
            pane.metrics.io_bps = Some(0);
        }
        self.run_cycle();
        for renderer in &mut self.renderers {
            renderer.bench_frame_timing(false);
            renderer.bench_frame_timing(false);
        }
    }

    /// Fold without production; count clock-only replays unless renderer application is enabled.
    pub fn run_cycle(&mut self) -> usize {
        // A stamp ahead of the clock keeps production not-due however long the fold takes.
        self.worker.producer_cadence.last_attempt =
            Some(Instant::now() + Duration::from_secs(3600));
        self.worker
            .run_cycle(&self.state, FetchRequest::default(), &mut self.sink);
        let mut unchanged = 0;
        let views = self.sink.host_data.lock().expect("host data").views.clone();
        for (cursor, subscriber) in self.cursors.iter_mut().zip(&self.sink.cycle) {
            let views = views.lock();
            for update in views.latest.since(cursor.revision) {
                let update = cursor.replay(
                    update,
                    |fold| {
                        super::loop_state::LoopState::project_fold(
                            fold,
                            Some(&subscriber.own_pane),
                            None,
                        )
                    },
                    true,
                );
                assert!(update.is_final(), "idle cycle completes without production");
                assert!(update.is_success(), "idle fold succeeds");
                unchanged += usize::from(update.is_unchanged());
                std::hint::black_box(update);
            }
            cursor.revision = views.revision;
        }
        for renderer in &mut self.renderers {
            let before = renderer.views.mirror().now;
            renderer.on_snapshot();
            assert!(
                renderer.views.mirror().now >= before,
                "renderer clock never regresses"
            );
        }
        unchanged
    }
}

#[cfg(feature = "testkit")]
fn fixture_renderer(
    room: RoomConfig,
    runtime: RuntimePaths,
    subscriber: &Subscriber,
    views: Arc<super::views::ViewsHandle>,
    clock: super::clock::LoopClock,
) -> super::loop_state::LoopState {
    let config = ServeConfig {
        tick_seconds: room.tick_seconds,
        room,
        instance_id: subscriber.instance_id.clone(),
        own_pane: subscriber.own_pane.clone(),
        refresh_ms_override: None,
    };
    let mut renderer = super::loop_state::LoopState::new(
        clock,
        config,
        None,
        runtime,
        std::path::PathBuf::new(),
        crate::diag::DiagSink::disabled(),
        FetchHandle::new(std::sync::mpsc::channel().0, subscriber.completion.clone()),
        None,
        std::sync::mpsc::sync_channel(1).0,
        Default::default(),
        None,
    );
    renderer.share_views(views, 0);
    renderer
}

impl FetchWorker {
    fn new(
        config: RoomConfig,
        runtime: RuntimePaths,
        diag: crate::diag::DiagSink,
        producer: PaneProducer,
        last_projection: Option<crate::sidebar::agent_projection::AgentProjectionPublication>,
    ) -> Self {
        let meter = TickMeter::new(TickLoop::Fetch, tick_for(config.tick_seconds));
        Self {
            observer: None,
            config,
            runtime,
            diag,
            cursor: RollupCursor::new(),
            producer,
            last_projection,
            producer_cadence: ProducerCadence::default(),
            notifications: NotificationState::default(),
            remind: super::remind::RemindState::default(),
            clock: super::clock::LoopClock::wall(),
            link_notifications: LinkNotificationState::default(),
            meter,
            window: FetchWindow::default(),
            tab_name_memo: TabNameMemo::default(),
            session_listed: |mux, session| {
                crate::mux::backend_for(mux).session_accepts_agent_close(session)
            },
            #[cfg(test)]
            on_read: None,
        }
    }

    /// Fold event-fresh truth every cycle and reconcile panes at the data tick.
    /// Both lanes share the warm cursor; forced topology and hard refreshes
    /// bypass cadence without blocking any renderer.
    fn run_cycle(&mut self, state: &StatePaths, request: FetchRequest, sink: &mut ResultSink) {
        sink.begin_cycle();
        let Some(eldest) = sink.cycle.first() else {
            return;
        };
        self.window.cycles += 1;
        let sampled_at = Instant::now();
        let now_ms = crate::utils::time::unix_now_ms();
        let inputs = sink
            .host_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inputs
            .clone();
        let frame_stamps = inputs
            .as_ref()
            .map(|inputs| (inputs.frame.produced_at_ms, inputs.frame.observed_at_ms));
        tracing::debug!(target: "rimz::sidebar::fold", "sidebar fold");
        #[cfg(test)]
        if let Some(on_read) = &mut self.on_read {
            on_read();
        }
        sink.inputs = Arc::new(FoldInputs::read(&self.runtime));
        sink.context = Some(Arc::new(FoldShared {
            observation: None,
            inputs: sink.inputs.clone(),
        }));
        let fold_started = Instant::now();
        let fast = HostInputs::fold(inputs.as_ref(), &mut self.cursor, state, &self.runtime).map(
            |workspace| WorkspaceFold {
                sampled_at,
                workspace,
                frame: inputs.as_ref().map(|inputs| Arc::clone(&inputs.frame)),
            },
        );
        self.window.fold_time += fold_started.elapsed();
        let tick = data_tick(eldest);
        let produce = self.start_produce_if_due(request, frame_stamps, &fast, now_ms, tick);
        let hibernated_panes = fast.as_ref().ok().map_or_else(Default::default, |fold| {
            fold.workspace
                .snapshot()
                .agents
                .iter()
                .filter(|agent| agent.hibernation.is_some())
                .filter_map(|agent| agent.pane.as_ref().map(|pane| pane.pane_id.clone()))
                .collect()
        });
        let agent_panes = fast.as_ref().ok().map_or_else(Default::default, |fold| {
            fold.workspace
                .snapshot()
                .agent_panes
                .iter()
                .map(|pane| pane.pane_id.clone())
                .collect()
        });
        self.publish_fast_fold(
            state,
            FastFold {
                result: fast,
                produce,
            },
            sink,
        );
        if produce {
            self.publish_produced_fold(state, request, hibernated_panes, agent_panes, sink);
        }
        let views = sink
            .host_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .views
            .clone();
        if let Some(event) = self.window.finish(&views, Instant::now()) {
            self.diag.emit_unlimited(event);
        }
    }

    fn start_produce_if_due(
        &mut self,
        request: FetchRequest,
        frame_stamps: Option<(u64, u64)>,
        fast: &crate::sidebar::produce::Result<WorkspaceFold>,
        now_ms: u64,
        tick: Duration,
    ) -> bool {
        // Pane-frame age gates only the producer reconciliation. An unreadable
        // store cannot coast on an otherwise-young frame.
        let frame_age_ms = fast
            .as_ref()
            .ok()
            .and(frame_stamps.map(|(produced_at_ms, _)| produced_at_ms))
            .map(|produced_at_ms| now_ms.saturating_sub(produced_at_ms));
        self.producer_cadence
            .start_attempt_if_due(request.mode, frame_age_ms, tick, Instant::now())
    }

    fn publish_fast_fold(&mut self, state: &StatePaths, fold: FastFold, sink: &mut ResultSink) {
        match fold.result {
            Ok(fast) => {
                let phase = if fold.produce {
                    FetchPhase::Interim
                } else {
                    FetchPhase::Final
                };
                self.publish_fold(state, fast, phase, SnapshotSource::Cached, sink);
            }
            Err(err) if !fold.produce => {
                sink.publish(Err(err.to_string()));
            }
            // Producing cycle reports the produce fold's own error.
            Err(_) => {}
        }
    }

    fn publish_produced_fold(
        &mut self,
        state: &StatePaths,
        request: FetchRequest,
        hibernated_panes: std::collections::HashSet<PaneId>,
        agent_panes: std::collections::HashSet<PaneId>,
        sink: &mut ResultSink,
    ) {
        let opts = crate::sidebar::produce::ProduceOptions {
            mux: self.config.mux,
            session_name: self.config.session_name.clone(),
            exclude: sink.witness(),
            min_pane_cache_ms: request.min_pane_cache_ms,
            hibernated_panes,
            agent_panes,
            diag: self.diag.clone(),
        };
        let sampled_at = Instant::now();
        let mut inputs = sink
            .host_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inputs
            .clone();
        let fold_started = Instant::now();
        let result = run_produce_guarded(&mut self.cursor, |cursor| {
            crate::sidebar::produce::produce_workspace_snapshot(
                cursor,
                state,
                &self.runtime,
                &opts,
                &mut self.producer,
                &mut self.last_projection,
                &mut inputs,
            )
        });
        self.window.fold_time += fold_started.elapsed();
        if let Some(inputs) = inputs {
            sink.host_data
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .inputs = Some(inputs);
        }
        match result {
            Ok(produced) => {
                let frame = produced.inputs.frame;
                let fold = WorkspaceFold {
                    sampled_at,
                    workspace: produced.workspace,
                    frame: Some(frame.clone()),
                };
                self.update_tab_names(fold.workspace.snapshot(), &frame);
                self.publish_fold(
                    state,
                    fold,
                    FetchPhase::Final,
                    SnapshotSource::Produced,
                    sink,
                );
            }
            Err(error) => sink.publish(Err(error)),
        }
    }

    fn update_tab_names(
        &mut self,
        snapshot: &SidebarSnapshot,
        frame: &crate::sidebar::frame::PaneFrame,
    ) {
        let renames = crate::sidebar::produce::tab_status::desired_tab_renames(
            snapshot,
            frame,
            &crate::proc::shell_pane_name(),
        );
        let pending = self.tab_name_memo.pending(frame, renames);
        if pending.is_empty() {
            return;
        }
        let mux = self.config.mux;
        let session = self.config.session_name.clone();
        std::thread::spawn(move || {
            let backend = crate::mux::backend_for(mux);
            for rename in pending {
                let result = backend.rename_tab(
                    &session,
                    &rename.anchor,
                    &rename.desired_name,
                    rename.intent.clone(),
                );
                if let Err(err) = result {
                    tracing::debug!(
                        session = %session,
                        pane = %rename.anchor,
                        intent = ?rename.intent,
                        desired_name = %rename.desired_name,
                        tags.operation = "sidebar.tab_status.rename",
                        error = &err as &dyn std::error::Error,
                        "could not update mux tab status",
                    );
                }
            }
        });
    }

    fn publish_fold(
        &mut self,
        state: &StatePaths,
        mut fold: WorkspaceFold,
        phase: FetchPhase,
        source: SnapshotSource,
        sink: &mut ResultSink,
    ) {
        sink.context = Some(Arc::new(FoldShared {
            observation: self.observer.as_mut().map(|observer| {
                observer.extract(fold.workspace.snapshot(), sink.cycle[0].instance_id.clone())
            }),
            inputs: sink.inputs.clone(),
        }));
        let final_snapshot = phase == FetchPhase::Final;
        let roster = (final_snapshot && source == SnapshotSource::Produced)
            .then(|| crate::sidebar::produce::live_roster_from_snapshot(fold.workspace.snapshot()));
        let mut deliveries = if final_snapshot {
            evaluate_notifications(
                &self.runtime,
                &self.config.notification_prefs,
                &mut self.notifications,
                &mut self.link_notifications,
                &self.diag,
                &mut fold.workspace.0,
            )
        } else {
            Vec::new()
        };
        if final_snapshot {
            let now_ms = self.clock.now_ms();
            if !deliveries.is_empty() {
                self.remind.note_ring(now_ms);
            }
            if let Some((notification, panes)) = self.remind.evaluate(
                now_ms,
                fold.workspace.snapshot(),
                &self.config.notification_prefs,
            ) {
                deliveries.push(NotificationDelivery {
                    notification_kind: notification.kind_env().to_owned(),
                    notification,
                    panes,
                });
            }
        }
        sink.publish(Ok((Arc::new(fold), phase, source)));
        // After publication: a narrowing write waits on a mux listing, which must
        // not hold the frame.
        if let Some(roster) = roster {
            self.publish_live_roster(state, roster);
        }
        deliver_notifications(
            sink,
            &self.config.workspace_id,
            &self.config.session_name,
            &self.config.notification_prefs,
            &self.diag,
            deliveries,
        );
    }

    /// Removing an agent from the roster claims it left a living room, so a
    /// narrowing write needs the mux to still list this session: a renderer
    /// that outlives its session reads an agent-less room from the pane cache
    /// and would otherwise empty the set the next birth recovers from.
    fn publish_live_roster(
        &self,
        state: &StatePaths,
        roster: std::collections::BTreeSet<(crate::ids::AgentKind, crate::ids::AgentSessionId)>,
    ) {
        let dropped: Vec<_> = crate::store::live_roster::read(&state.live_roster)
            .map(|prior| prior.agents.difference(&roster).cloned().collect())
            .unwrap_or_default();
        if !dropped.is_empty() && !(self.session_listed)(self.config.mux, &self.config.session_name)
        {
            self.diag
                .emit(crate::diag::record::DiagEvent::LiveRosterHeld { dropped });
            return;
        }
        if let Err(err) = crate::store::live_roster::publish(&state.live_roster, roster) {
            tracing::debug!(
                path = %state.live_roster.display(),
                error = %err,
                "live roster publish failed",
            );
        }
    }
}

fn evaluate_notifications(
    runtime: &RuntimePaths,
    prefs: &NotificationsPrefs,
    state: &mut NotificationState,
    link_state: &mut LinkNotificationState,
    diag: &crate::diag::DiagSink,
    snapshot: &mut SidebarSnapshot,
) -> Vec<NotificationDelivery> {
    let now_ms = crate::utils::time::unix_now_ms();
    let unread = reconcile_snapshot(runtime, snapshot);
    emit_unread_reconcile_trace(diag, &unread.opened, &unread.cleared);

    let notifications = state.evaluate(snapshot, &unread.opened, prefs, now_ms);
    if let Some(alert) = link_state.evaluate(snapshot, now_ms) {
        emit_link_alert(diag, alert);
    }
    notifications
        .into_iter()
        .map(|notification| {
            let panes = notification_panes(&notification);
            let notification_kind = notification.kind_env().to_owned();
            NotificationDelivery {
                notification,
                panes,
                notification_kind,
            }
        })
        .collect()
}

fn deliver_notifications(
    sink: &ResultSink,
    workspace_id: &crate::WorkspaceId,
    session_name: &str,
    prefs: &NotificationsPrefs,
    diag: &crate::diag::DiagSink,
    deliveries: Vec<NotificationDelivery>,
) {
    for delivery in deliveries {
        let notification = delivery.notification;
        if prefs.has_handlers() {
            crate::sidebar::notify::spawn_notify_handlers(prefs, &notification);
        }
        diag.trace_notify(notification_emitted_trace(&notification, &delivery.panes));
        sink.notify(
            workspace_id,
            session_name,
            SidebarEvent::Notify {
                title: notification.title,
                body: notification.body,
                panes: delivery.panes,
                recheck_unread: notification.notification_kind
                    != crate::sidebar::notify::NotificationKind::Reminder,
                notification_kind: Some(delivery.notification_kind),
            },
        );
    }
}

#[derive(Clone, Debug)]
struct NotificationDelivery {
    notification: Notification,
    panes: Vec<PaneId>,
    notification_kind: String,
}

fn emit_unread_reconcile_trace(
    diag: &crate::diag::DiagSink,
    opened: &[OpenedUnread],
    cleared: &[ClearedUnread],
) {
    for item in opened {
        diag.trace_notify(item.trace_event());
    }
    for item in cleared {
        diag.trace_notify(item.trace_event());
    }
}

fn notification_emitted_trace(
    notification: &Notification,
    panes: &[PaneId],
) -> crate::diag::notify::NotifyTraceEvent {
    use crate::diag::notify::{NotifyTraceEvent, TraceAgent};
    NotifyTraceEvent::NotificationEmitted {
        notification_kind: notification.kind_env().to_owned(),
        agents: notification
            .agents
            .iter()
            .map(|agent| TraceAgent {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                label: agent.label.clone(),
                pane_id: agent.pane_id.clone(),
                new_status: agent.new_status.map(|status| status.as_str().to_owned()),
            })
            .collect(),
        panes: panes.to_vec(),
        unread_count: notification.unread_count,
    }
}

fn emit_link_alert(diag: &crate::diag::DiagSink, alert: LinkAlert) {
    diag.emit(crate::diag::record::DiagEvent::LinkAlert {
        tier: alert.tier,
        rtt_ms: alert.rtt_ms,
        miss_pct: alert.miss_pct,
        since_ms: alert.since_ms,
        recovered_after_ms: alert.recovered_after_ms,
    });
}

fn notification_panes(notification: &Notification) -> Vec<PaneId> {
    notification
        .agents
        .iter()
        .filter_map(|agent| agent.pane_id.clone())
        .collect()
}

/// One request to the fetch worker. The mode distinguishes topology freshness
/// from manual recovery. A producing lane ignores pane caches older than a
/// supplied `min_pane_cache_ms` floor.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FetchRequest {
    mode: FetchMode,
    min_pane_cache_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FetchMode {
    #[default]
    Normal,
    FreshPanes,
    HardRefresh,
}

impl FetchMode {
    fn strength(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::FreshPanes => 1,
            Self::HardRefresh => 2,
        }
    }

    fn strongest(self, other: Self) -> Self {
        if self.strength() >= other.strength() {
            self
        } else {
            other
        }
    }
}

impl FetchRequest {
    pub(super) fn fresh_panes() -> Self {
        Self {
            mode: FetchMode::FreshPanes,
            min_pane_cache_ms: Some(crate::utils::time::unix_now_ms()),
        }
    }

    pub(super) fn hard_refresh() -> Self {
        Self {
            mode: FetchMode::HardRefresh,
            min_pane_cache_ms: Some(crate::utils::time::unix_now_ms()),
        }
    }

    #[cfg(test)]
    pub(super) fn is_fresh_panes(self) -> bool {
        matches!(self.mode, FetchMode::FreshPanes)
    }

    #[cfg(test)]
    pub(super) fn mode(self) -> FetchMode {
        self.mode
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.mode = self.mode.strongest(other.mode);
        self.min_pane_cache_ms = self.min_pane_cache_ms.max(other.min_pane_cache_ms);
    }
}

struct FetchWindow {
    started: Instant,
    cycles: u64,
    fold_time: Duration,
}

impl Default for FetchWindow {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            cycles: 0,
            fold_time: Duration::ZERO,
        }
    }
}

impl FetchWindow {
    fn finish(
        &mut self,
        views: &super::views::ViewsHandle,
        now: Instant,
    ) -> Option<crate::diag::record::DiagEvent> {
        let interval = now.saturating_duration_since(self.started);
        if interval < Duration::from_secs(60) {
            return None;
        }
        let counts = std::mem::take(&mut views.lock().reductions);
        self.started = now;
        Some(crate::diag::record::DiagEvent::FetchReductionStats {
            interval_ms: interval.as_millis() as u64,
            cycles: std::mem::take(&mut self.cycles),
            content_reductions: counts.content,
            unchanged_reductions: counts.unchanged,
            fold_ms: std::mem::take(&mut self.fold_time).as_millis() as u64,
        })
    }
}

/// A pane's lifecycle and production preferences; fetch content lives in the views.
#[derive(Clone)]
pub(super) struct Subscriber {
    pub(super) instance_id: SidebarInstanceId,
    pub(super) own_pane: PaneId,
    pub(super) tick_seconds: u64,
    pub(super) completion: Arc<AtomicBool>,
    pub(super) wake: SyncSender<Wakeup>,
}

pub(super) fn data_tick(eldest: &Subscriber) -> Duration {
    tick_for(eldest.tick_seconds)
}

/// The renderers one fetch worker feeds, keyed by instance id so they iterate
/// oldest subscriber first, fixing cadence, frame witness and observation origin.
pub(super) type Subscribers = Arc<Mutex<BTreeMap<String, Subscriber>>>;

struct ResultSink {
    subscribers: Subscribers,
    host_data: Arc<Mutex<HostData>>,
    /// The renderers this cycle serves, taken once as it starts.
    cycle: Vec<Subscriber>,
    context: Option<Arc<FoldShared>>,
    inputs: Arc<FoldInputs>,
    #[cfg(feature = "testkit")]
    publication: Option<Arc<Mutex<(u64, Instant)>>>,
}

impl ResultSink {
    fn shared(subscribers: Subscribers, host_data: Arc<Mutex<HostData>>) -> Self {
        let mut sink = Self {
            subscribers,
            host_data,
            cycle: Vec::new(),
            context: None,
            inputs: Arc::default(),
            #[cfg(feature = "testkit")]
            publication: None,
        };
        sink.begin_cycle();
        sink
    }

    fn subscribers(&self) -> Vec<Subscriber> {
        let subscribers = match self.subscribers.lock() {
            Ok(subscribers) => subscribers,
            Err(poisoned) => poisoned.into_inner(),
        };
        subscribers.values().cloned().collect()
    }

    /// Freeze completion and wake membership; later arrivals can still read room content.
    fn begin_cycle(&mut self) {
        self.cycle = self.subscribers();
        self.context = None;
    }

    fn witness(&self) -> Option<PaneId> {
        self.cycle.first().map(|eldest| eldest.own_pane.clone())
    }

    /// Publish content and completion atomically, then wake the frozen cycle members.
    fn publish(
        &mut self,
        outcome: Result<(Arc<WorkspaceFold>, FetchPhase, SnapshotSource), String>,
    ) {
        let views = self
            .host_data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .views
            .clone();
        let mut update = match outcome {
            Ok((mut fold, phase, source)) => {
                // Compare unlocked: panes replay and paint under the views lock, and
                // this worker is the only writer of the retained fold.
                let prior = views.lock().fold.clone();
                if let Some(prior) = prior {
                    let shared = &mut Arc::make_mut(&mut fold).workspace.0;
                    let prior = prior.workspace.snapshot();
                    reuse_shared(&mut shared.worktree_groups, &prior.worktree_groups);
                    reuse_shared(&mut shared.agents, &prior.agents);
                    reuse_shared(&mut shared.agent_panes, &prior.agent_panes);
                    reuse_shared(&mut shared.fenced_sessions, &prior.fenced_sessions);
                    if let (Some(shared), Some(prior)) =
                        (&mut shared.resume_outcomes, &prior.resume_outcomes)
                    {
                        reuse_shared(shared, prior);
                    }
                }
                FetchUpdate::Fold {
                    fold,
                    phase,
                    source,
                }
            }
            Err(error) => FetchUpdate::Failed { error },
        };
        let completed = update.is_final();
        if let Some(context) = &self.context {
            update = FetchUpdate::Shared {
                update: Box::new(update),
                context: context.clone(),
            };
        }
        let mut views = views.lock();
        #[cfg(feature = "testkit")]
        let published_at = self.publication.as_ref().map(|_| Instant::now());
        views.publish(update);
        #[cfg(feature = "testkit")]
        if let (Some(publication), Some(stamp)) = (&self.publication, published_at) {
            *publication.lock().expect("publication stamp") = (views.revision, stamp);
        }
        if completed {
            for subscriber in &self.cycle {
                subscriber.completion.store(true, Ordering::Relaxed);
            }
        }
        drop(views);
        for subscriber in &self.cycle {
            let _ = subscriber.wake.try_send(Wakeup::Snapshot);
        }
    }

    fn notify(&self, workspace_id: &crate::WorkspaceId, session_name: &str, event: SidebarEvent) {
        let envelope = SidebarEventEnvelope::new(
            workspace_id.clone(),
            Some(session_name.to_owned()),
            crate::utils::time::unix_now_ms(),
            event,
        );
        for subscriber in self.subscribers() {
            let _ = subscriber.wake.try_send(Wakeup::Event(envelope.clone()));
        }
    }
}

impl FetchWorker {
    fn run(self, request_rx: std::sync::mpsc::Receiver<FetchRequest>, sink: ResultSink) {
        self.run_resolving(request_rx, sink, StatePaths::for_workspace);
    }

    fn run_resolving(
        mut self,
        request_rx: std::sync::mpsc::Receiver<FetchRequest>,
        mut sink: ResultSink,
        mut resolve_state: impl FnMut(
            crate::ids::WorkspaceId,
        )
            -> std::result::Result<StatePaths, crate::disk::paths::PathErr>,
    ) {
        let subscribers = sink.subscribers.clone();
        drive(
            &request_rx,
            FOLD_FLOOR,
            || {
                subscribers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .values()
                    .map(|subscriber| tick_for(subscriber.tick_seconds))
                    .min()
            },
            |request| {
                // Re-resolved every cycle so `workspace migrate` repoints reads
                // without restarting the renderer.
                match resolve_state(self.config.workspace_id.clone()) {
                    Ok(state) => {
                        let tick = self.meter.begin();
                        self.run_cycle(&state, request, &mut sink);
                        if let Some(event) = self.meter.finish(
                            tick,
                            crate::utils::time::unix_now_ms(),
                            Default::default(),
                        ) {
                            crate::sidebar::meter::report(&self.diag, event);
                        }
                    }
                    Err(err) => {
                        sink.begin_cycle();
                        sink.publish(Err(format!("resolving workspace state paths: {err}")));
                    }
                }
            },
        );
    }
}

/// Spawn the background fetch owner for every renderer in `subscribers`. It
/// runs until every request sender is gone, finishing the cycle in hand so its
/// durable and external side effects still land.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_fetch_worker(
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    producer: PaneProducer,
    last_projection: Option<crate::sidebar::agent_projection::AgentProjectionPublication>,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    subscribers: Subscribers,
    host_data: Arc<Mutex<HostData>>,
    observer: Option<crate::sidebar::observe::RoomObserver>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        crate::lane::set(crate::lane::WorkLane::Fetch);
        let mut worker = FetchWorker::new(config, runtime, diag, producer, last_projection);
        worker.observer = observer;
        worker.run(request_rx, ResultSink::shared(subscribers, host_data));
    })
}

/// The worker's idle gap after each cycle returns: an end-to-start gap bounds
/// its duty cycle even when one cycle takes longer than the gap.
const FOLD_FLOOR: Duration = Duration::from_millis(250);

/// Run one cycle per batch, starting none sooner than `fold_floor` after the
/// previous one returned, whatever the request's mode. Every request waiting
/// at the drain joins the batch; later arrivals run the next cycle. A batch
/// in hand still runs after every sender disconnects. After the first cycle,
/// an idle backstop tick runs an ordinary cycle. No subscribers (`None`), or a
/// tick no `Instant` can reach, means blocking for a request; timeouts re-read
/// membership before firing.
fn drive(
    requests: &Receiver<FetchRequest>,
    fold_floor: Duration,
    mut backstop: impl FnMut() -> Option<Duration>,
    mut cycle: impl FnMut(FetchRequest),
) {
    let mut returned_at: Option<Instant> = None;
    loop {
        let received =
            match returned_at.and_then(|last| backstop().and_then(|tick| last.checked_add(tick))) {
                Some(deadline) => {
                    requests.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                None => requests.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
        let mut request = match received {
            Ok(request) => request,
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) if backstop().is_none() => continue,
            Err(RecvTimeoutError::Timeout) => FetchRequest::default(),
        };
        if let Some(rest) =
            returned_at.and_then(|last| (last + fold_floor).checked_duration_since(Instant::now()))
        {
            std::thread::sleep(rest);
        }
        for waiting in requests.try_iter() {
            request.merge(waiting);
        }
        cycle(request);
        returned_at = Some(Instant::now());
    }
}

/// A pane's send end and completion latch. Pane observations send immediately;
/// level-triggered deadlines wait while this pane awaits a completed cycle.
pub(super) struct FetchHandle {
    tx: Sender<FetchRequest>,
    completion: Arc<AtomicBool>,
    awaiting: bool,
}

impl FetchHandle {
    pub(super) fn new(tx: Sender<FetchRequest>, completion: Arc<AtomicBool>) -> Self {
        Self {
            tx,
            completion,
            awaiting: false,
        }
    }

    /// Send now; the worker merges it into its next cycle.
    pub(super) fn request(&mut self, request: FetchRequest) {
        if self.tx.send(request).is_ok() {
            self.awaiting = true;
        }
    }

    /// A due deadline: send unless a request of this pane still awaits a cycle.
    pub(super) fn request_if_idle(&mut self, request: FetchRequest) {
        if !self.awaiting {
            self.request(request);
        }
    }

    /// Whether a cycle this pane belonged to completed since the last call;
    /// clears the awaiting latch when true.
    pub(super) fn completed(&mut self) -> bool {
        let completed = self.completion.swap(false, Ordering::Relaxed);
        if completed {
            self.awaiting = false;
        }
        completed
    }
}

#[cfg(test)]
mod tests;
