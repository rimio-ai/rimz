//! Off-thread fast folds, pane production, and single-flight request coalescing.
//! The host shares one warm rollup cursor across both lanes. Heavy cache
//! refreshes run separately so render and input never block on production.
//! Every admitted request folds; acknowledged equal content delivers only clocks.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::diag::record::TickLoop;
use crate::ids::{PaneId, SidebarInstanceId};
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::enrich::{WorkspaceSnapshot, project_local};
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::meter::TickMeter;
use crate::sidebar::notify::{LinkAlert, LinkNotificationState, Notification, NotificationState};
use crate::sidebar::produce::{HostInputs, PaneProducer};
use crate::sidebar::read_marks::{ReadMarks, prune_read_marks};
use crate::sidebar::unread::{ClearedUnread, OpenedUnread, UnreadEpisodes};
use crate::store::snapshot::SidebarSnapshot;
use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
use crate::{RuntimePaths, StatePaths};

use super::ServeConfig;
use super::input::Wakeup;
use super::timing::tick_for;

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
    Unchanged {
        clock: SnapshotClock,
        phase: FetchPhase,
        source: SnapshotSource,
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
    pub(super) workspace: WorkspaceSnapshot,
    pub(super) frame: Option<Arc<PaneFrame>>,
}

#[derive(Default)]
pub(super) struct HostData {
    pub(super) last_fold: Option<WorkspaceFold>,
    pub(super) inputs: Option<HostInputs>,
}

struct SnapshotPublication {
    snapshot: SidebarSnapshot,
    phase: FetchPhase,
    source: SnapshotSource,
}

#[derive(Clone, Copy)]
pub(super) struct SnapshotClock {
    now: jiff::Timestamp,
    generated_at: jiff::Timestamp,
    panes_produced_at_ms: Option<u64>,
    panes_observed_at_ms: Option<u64>,
}

impl SnapshotClock {
    fn of(snapshot: &SidebarSnapshot) -> Self {
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
            } | Self::Unchanged {
                phase: FetchPhase::Interim,
                ..
            }
        )
    }

    fn snapshot_mut(&mut self) -> Option<&mut SidebarSnapshot> {
        match self {
            Self::Shared { update, .. } => update.snapshot_mut(),
            Self::Snapshot { snapshot, .. } => Some(snapshot),
            Self::Unchanged { .. } | Self::Failed { .. } => None,
        }
    }

    fn is_success(&self) -> bool {
        match self {
            Self::Shared { update, .. } => update.is_success(),
            Self::Failed { .. } => false,
            _ => true,
        }
    }

    fn is_unchanged(&self) -> bool {
        match self {
            Self::Shared { update, .. } => update.is_unchanged(),
            Self::Unchanged { .. } => true,
            _ => false,
        }
    }

    fn same_content(&self, previous: &Self) -> bool {
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

    fn into_unchanged(self) -> Self {
        match self {
            Self::Shared { update, context } => Self::Shared {
                update: Box::new(update.into_unchanged()),
                context,
            },
            Self::Snapshot {
                snapshot,
                phase,
                source,
            } => Self::Unchanged {
                clock: SnapshotClock::of(&snapshot),
                phase,
                source,
            },
            update => update,
        }
    }

    fn restore_content(&mut self, previous: &Self) {
        match (self, previous) {
            (Self::Shared { update, .. }, Self::Shared { update: prior, .. }) => {
                update.restore_content(prior)
            }
            (update @ Self::Unchanged { .. }, Self::Snapshot { snapshot, .. }) => {
                let Self::Unchanged {
                    clock,
                    phase,
                    source,
                } = *update
                else {
                    unreachable!()
                };
                let mut snapshot = snapshot.clone();
                clock.apply(&mut snapshot);
                *update = Self::Snapshot {
                    snapshot,
                    phase,
                    source,
                };
            }
            _ => {}
        }
    }

    pub(super) fn into_parts(self) -> (Self, Option<Arc<FoldShared>>) {
        match self {
            Self::Shared { update, context } => (*update, Some(context)),
            update => (update, None),
        }
    }
}

fn shared_eq<T: PartialEq + ?Sized>(left: &Arc<T>, right: &Arc<T>) -> bool {
    Arc::ptr_eq(left, right) || left == right
}

fn reuse_shared<T: PartialEq + ?Sized>(shared: &mut Arc<T>, local: &mut Arc<T>, prior: &Arc<T>) {
    if Arc::ptr_eq(shared, prior) || shared != prior {
        return;
    }
    let local_shares = Arc::ptr_eq(shared, local);
    *shared = prior.clone();
    if local_shares {
        *local = prior.clone();
    }
}

fn same_snapshot_content(left: &SidebarSnapshot, right: &SidebarSnapshot) -> bool {
    // Exhaustive: a new snapshot field must join equality or explicitly be a clock.
    let SidebarSnapshot {
        snapshot_version,
        workspace_id,
        display_name,
        generated_at: _,
        panes_produced_at_ms: _,
        panes_observed_at_ms: _,
        now: _,
        viewed_panes,
        client_views,
        pane_session_name,
        focused_pane,
        presence,
        truth_degraded,
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
        && workspace_id == &right.workspace_id
        && display_name == &right.display_name
        && viewed_panes == &right.viewed_panes
        && client_views == &right.client_views
        && pane_session_name == &right.pane_session_name
        && focused_pane == &right.focused_pane
        && presence == &right.presence
        && truth_degraded == &right.truth_degraded
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

/// Owns all state that persists between fetch requests.
struct FetchWorker {
    observer: Option<crate::sidebar::observe::RoomObserver>,
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    cursor: RollupCursor,
    producer: PaneProducer,
    last_projection: Option<crate::sidebar::agent_projection::AgentProjectionPublication>,
    producer_cadence: ProducerCadence,
    notifications: NotificationState,
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

/// A retained fetch worker and subscriber mailboxes, without a live multiplexer.
#[cfg(feature = "testkit")]
pub struct IdleCycleFixture {
    worker: FetchWorker,
    sink: ResultSink,
    state: StatePaths,
    receivers: Vec<ResultReceiver>,
    renderers: Vec<(super::loop_state::LoopState, FetchDispatcher)>,
}

#[cfg(feature = "testkit")]
impl IdleCycleFixture {
    pub fn new(
        state: StatePaths,
        runtime: RuntimePaths,
        frame: PaneFrame,
        subscribers: usize,
    ) -> Self {
        let config = ServeConfig {
            workspace_id: state.workspace_id.clone(),
            mux: crate::MuxName::Zellij,
            session_name: frame.session_name.clone(),
            instance_id: SidebarInstanceId::new(),
            tick_seconds: 1,
            refresh_ms_override: None,
            timezone: jiff::tz::TimeZone::UTC,
            notification_prefs: NotificationsPrefs::default(),
            own_pane: None,
        };
        let producer = PaneProducer::seeded(&runtime, &config.session_name);
        let worker = FetchWorker::new(
            config,
            runtime,
            crate::diag::DiagSink::disabled(),
            producer,
            None,
        );
        let host_data = Arc::new(Mutex::new(HostData {
            inputs: Some(HostInputs::from_frame(frame)),
            ..Default::default()
        }));
        let mut receivers = Vec::new();
        let mut subscriptions = BTreeMap::new();
        for index in 0..subscribers {
            let (tx, rx) = result_channel();
            let (wake, _) = std::sync::mpsc::sync_channel(1);
            let instance_id = SidebarInstanceId::new();
            subscriptions.insert(
                instance_id.as_str().to_owned(),
                Subscriber {
                    instance_id,
                    own_pane: Some(PaneId::from_parts(
                        crate::MuxName::Zellij,
                        format!("sidebar_{index}"),
                    )),
                    tick_seconds: 1,
                    refresh_override: None,
                    tx,
                    wake,
                },
            );
            receivers.push(rx);
        }
        Self {
            worker,
            sink: ResultSink::shared(
                Arc::new(Mutex::new(subscriptions)),
                Covered::default(),
                host_data,
            ),
            state,
            receivers,
            renderers: Vec::new(),
        }
    }

    /// Include each subscriber's real renderer application, but no painting.
    pub fn enable_renderer_apply(&mut self) {
        self.sink.begin_cycle();
        self.renderers = self
            .receivers
            .drain(..)
            .map(|rx| {
                let subscriber = self
                    .sink
                    .cycle
                    .iter()
                    .find(|subscriber| subscriber.tx.0.ptr_eq(&Arc::downgrade(&rx.0)))
                    .expect("every receiver has a subscriber");
                let mut config = self.worker.config.clone();
                config.instance_id = subscriber.instance_id.clone();
                config.own_pane = subscriber.own_pane.clone();
                (
                    super::loop_state::LoopState::new(
                        config,
                        None,
                        self.worker.runtime.clone(),
                        std::path::PathBuf::new(),
                        crate::diag::DiagSink::disabled(),
                        rx,
                        None,
                        std::sync::mpsc::sync_channel(1).0,
                        Default::default(),
                        None,
                    ),
                    FetchDispatcher::for_plane(std::sync::mpsc::channel().0, Covered::default()),
                )
            })
            .collect();
    }

    /// Paint every consumer once and verify the single-view benchmark topology.
    pub fn paint_fanout(&mut self) {
        let mut watched = 0;
        let mut prior = None;
        for (renderer, _) in &mut self.renderers {
            assert!(renderer.current.own_view.is_some());
            if renderer.bench_frame_timing(true) {
                watched += 1;
                prior = Some(renderer.current.clone());
            }
        }
        assert_eq!(watched, 1);
        self.run_content_cycle();
        let prior = prior.expect("one watched renderer");
        for (renderer, _) in &mut self.renderers {
            if renderer.bench_frame_timing(false) {
                assert_ne!(prior.worktree_groups, renderer.current.worktree_groups);
                assert_eq!(prior.own_view, renderer.current.own_view);
            }
        }
    }

    /// Route a room event through every consumer before the next cycle.
    pub fn deliver_event(&mut self, event: SidebarEvent) {
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
        for (renderer, dispatcher) in &mut self.renderers {
            renderer.on_event(dispatcher, &mut terminal, envelope.clone());
        }
    }

    /// Change row content without changing any consumer's own view, then fold and time frames.
    pub fn run_content_cycle(&mut self) {
        {
            let mut data = self.sink.host_data.lock().expect("host data");
            let inputs = data.inputs.as_mut().expect("seeded inputs");
            let frame = Arc::make_mut(&mut inputs.frame);
            frame.metrics_stamp_ms = Some(frame.metrics_stamp_ms.unwrap_or_default() + 1);
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
        for (renderer, _) in &mut self.renderers {
            renderer.bench_frame_timing(false);
            renderer.bench_frame_timing(false);
        }
    }

    /// Fold once without production; return clock-only mailbox deliveries unless renderer application is enabled.
    pub fn run_cycle(&mut self) -> usize {
        self.worker.producer_cadence.last_attempt = Some(Instant::now());
        self.worker
            .run_cycle(&self.state, FetchRequest::default(), &mut self.sink);
        let mut unchanged = 0;
        for receiver in &self.receivers {
            let update = receiver
                .take()
                .pop()
                .expect("cycle delivers to every subscriber");
            assert!(update.is_final(), "idle cycle completes without production");
            assert!(update.is_success(), "idle fold succeeds");
            unchanged += usize::from(update.is_unchanged());
            std::hint::black_box(&update);
            receiver.acknowledge(update, true);
        }
        for (renderer, dispatcher) in &mut self.renderers {
            let before = renderer.current.now;
            renderer.on_snapshot(dispatcher);
            assert!(renderer.current.now > before, "renderer applies the cycle");
        }
        unchanged
    }
}

impl FetchWorker {
    fn new(
        config: ServeConfig,
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
        if sink.cycle.is_empty() {
            return;
        }
        self.window.cycles += 1;
        if request.force_fold {
            for subscriber in &sink.cycle {
                if let Some(mailbox) = subscriber.tx.0.upgrade() {
                    mailbox
                        .delivery
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .reusable = false;
                }
            }
        }
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
                workspace,
                frame: inputs.as_ref().map(|inputs| Arc::clone(&inputs.frame)),
            },
        );
        self.window.fold_time += fold_started.elapsed();
        let tick = self.stand(sink).tick;
        let produce = self.start_produce_if_due(request, frame_stamps, &fast, now_ms, tick);
        self.publish_fast_fold(
            state,
            FastFold {
                result: fast,
                produce,
            },
            sink,
        );
        if produce {
            self.publish_produced_fold(state, request, sink);
        }
        if let Some(event) = self.window.finish(&sink.deliveries, Instant::now()) {
            self.diag.emit_unlimited(event);
        }
    }

    /// The eldest renderer of the cycle in hand, which stands for the worker
    /// in every decision made once per fold.
    fn stand(&self, sink: &ResultSink) -> Stand {
        Stand::of(sink.cycle.first(), &self.config)
    }

    /// Project one fold for the eldest renderer, which stands for the worker
    /// in every decision made once per fold, and stage it so the publication
    /// projects it again for each other renderer.
    fn project_fold(&mut self, fold: WorkspaceFold, sink: &mut ResultSink) -> SidebarSnapshot {
        sink.context = Some(Arc::new(FoldShared {
            observation: self.observer.as_mut().map(|observer| {
                observer.extract(fold.workspace.snapshot(), sink.cycle[0].instance_id.clone())
            }),
            inputs: sink.inputs.clone(),
        }));
        let own_pane = self.stand(sink).own_pane;
        let snapshot = project_local(
            fold.workspace.clone(),
            fold.frame.as_deref(),
            own_pane.as_ref(),
        );
        sink.staged = Some((fold, self.config.own_pane.clone()));
        snapshot
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
                let snapshot = self.project_fold(fast, sink);
                let phase = if fold.produce {
                    FetchPhase::Interim
                } else {
                    FetchPhase::Final
                };
                self.publish_snapshot(
                    state,
                    SnapshotPublication {
                        snapshot,
                        phase,
                        source: SnapshotSource::Cached,
                    },
                    sink,
                );
            }
            Err(err) if !fold.produce => {
                sink.publish(FetchUpdate::Failed {
                    error: err.to_string(),
                });
            }
            // Producing cycle reports the produce fold's own error.
            Err(_) => {}
        }
    }

    fn publish_produced_fold(
        &mut self,
        state: &StatePaths,
        request: FetchRequest,
        sink: &mut ResultSink,
    ) {
        let opts = crate::sidebar::produce::ProduceOptions {
            mux: self.config.mux,
            session_name: self.config.session_name.clone(),
            exclude: self.stand(sink).own_pane,
            min_pane_cache_ms: request.min_pane_cache_ms,
            diag: self.diag.clone(),
        };
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
                let snapshot = self.project_fold(
                    WorkspaceFold {
                        workspace: produced.workspace,
                        frame: Some(frame.clone()),
                    },
                    sink,
                );
                self.update_tab_names(&snapshot, &frame);
                self.publish_snapshot(
                    state,
                    SnapshotPublication {
                        snapshot,
                        phase: FetchPhase::Final,
                        source: SnapshotSource::Produced,
                    },
                    sink,
                );
            }
            Err(error) => sink.publish(FetchUpdate::Failed { error }),
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

    fn publish_snapshot(
        &mut self,
        state: &StatePaths,
        publication: SnapshotPublication,
        sink: &mut ResultSink,
    ) {
        let SnapshotPublication {
            mut snapshot,
            phase,
            source,
        } = publication;
        let final_snapshot = phase == FetchPhase::Final;
        let roster = (final_snapshot && source == SnapshotSource::Produced)
            .then(|| crate::sidebar::produce::live_roster_from_snapshot(&snapshot));
        let deliveries = if final_snapshot {
            evaluate_notifications(
                &self.runtime,
                &self.config.notification_prefs,
                &mut self.notifications,
                &mut self.link_notifications,
                &self.diag,
                &mut snapshot,
            )
        } else {
            Vec::new()
        };
        if final_snapshot && let Some((fold, _)) = &mut sink.staged {
            let unread: HashMap<_, _> = snapshot
                .rows()
                .map(|row| (row.id.as_str(), row.unread))
                .collect();
            for index in 0..fold.workspace.0.worktree_groups.len() {
                let group = &fold.workspace.0.worktree_groups[index];
                if !group.rows.iter().any(|row| {
                    unread
                        .get(row.id.as_str())
                        .is_some_and(|unread| row.unread != *unread)
                }) {
                    continue;
                }
                let group = &mut Arc::make_mut(&mut fold.workspace.0.worktree_groups)[index];
                for row in Arc::make_mut(&mut group.rows) {
                    if let Some(unread) = unread.get(row.id.as_str()) {
                        row.unread = *unread;
                    }
                }
            }
        }
        sink.publish(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase,
            source,
        });
        // After the send: a narrowing write waits on a mux listing, which must
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
    let mut episodes = UnreadEpisodes::load(runtime);
    let silent_opens = episodes.was_absent_on_load();
    let marks = ReadMarks::load(runtime);
    if snapshot.panes_produced_at_ms.is_some() {
        prune_read_marks(
            runtime,
            &snapshot.rows().map(|row| row.id.clone()).collect(),
        );
    }
    let unread = episodes.reconcile(snapshot, &marks, silent_opens);
    emit_unread_reconcile_trace(diag, &unread.opened, &unread.cleared);
    if (episodes.was_absent_on_load() || unread.changed)
        && let Err(err) = episodes.persist(runtime)
    {
        tracing::debug!(error = %err, "unread episodes persist failed");
    }

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
                recheck_unread: true,
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
/// from manual recovery. When a
/// request carries `min_pane_cache_ms`, any producing lane ignores a pane cache
/// older than the signal that asked for fresh topology.
#[derive(Clone, Copy, Debug)]
pub(super) struct FetchRequest {
    mode: FetchMode,
    min_pane_cache_ms: Option<u64>,
    force_fold: bool,
    /// When the renderer learned of what this request asks to fold. A cycle
    /// that started later already read it.
    observed_at: Instant,
}

impl Default for FetchRequest {
    fn default() -> Self {
        Self {
            mode: FetchMode::Normal,
            min_pane_cache_ms: None,
            force_fold: false,
            observed_at: Instant::now(),
        }
    }
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
            ..Self::default()
        }
    }

    pub(super) fn hard_refresh() -> Self {
        Self {
            mode: FetchMode::HardRefresh,
            min_pane_cache_ms: Some(crate::utils::time::unix_now_ms()),
            ..Self::default()
        }
    }

    /// Request a new fold for renderer-local state that a prior cycle could not observe.
    pub(super) fn force_fold() -> Self {
        Self {
            force_fold: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn is_fresh_panes(self) -> bool {
        matches!(self.mode, FetchMode::FreshPanes)
    }

    #[cfg(test)]
    pub(super) fn forces_fold(self) -> bool {
        self.force_fold
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.mode = self.mode.strongest(other.mode);
        self.force_fold |= other.force_fold;
        self.observed_at = self.observed_at.max(other.observed_at);
        self.min_pane_cache_ms = match (self.min_pane_cache_ms, other.min_pane_cache_ms) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (Some(current), None) => Some(current),
            (None, Some(next)) => Some(next),
            (None, None) => None,
        };
    }

    /// A deferred request asks for a fold after its deadline, so a cycle that
    /// started before the deadline does not answer it.
    fn observed_now(self) -> Self {
        Self {
            observed_at: Instant::now(),
            ..self
        }
    }
}

#[derive(Default)]
pub(super) struct PendingResults {
    pub(super) snapshot: Option<FetchUpdate>,
    pub(super) outcome: Option<FetchUpdate>,
    pub(super) completed: bool,
}

impl PendingResults {
    fn push(&mut self, update: FetchUpdate) {
        let is_final = update.is_final();
        self.completed |= is_final;
        if !update.is_success() {
            self.outcome = Some(update);
            return;
        }
        // An unfinished cycle cannot displace a completed one the pane has
        // not seen.
        if !is_final && self.snapshot.as_ref().is_some_and(FetchUpdate::is_final) {
            return;
        }
        self.snapshot = Some(update);
        if is_final {
            self.outcome = None;
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    fn pop(&mut self) -> Option<FetchUpdate> {
        self.snapshot.take().or_else(|| self.outcome.take())
    }
}

#[derive(Default)]
struct ResultMailbox {
    pending: Mutex<PendingResults>,
    delivery: Mutex<DeliveryMemo>,
    #[cfg(test)]
    ready: std::sync::Condvar,
}

#[derive(Default)]
struct DeliveryMemo {
    accepted: Option<Arc<FetchUpdate>>,
    reusable: bool,
    counts: Option<Arc<Mutex<DeliveryCounts>>>,
}

#[derive(Default)]
struct DeliveryCounts {
    content: u64,
    unchanged: u64,
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
        counts: &Mutex<DeliveryCounts>,
        now: Instant,
    ) -> Option<crate::diag::record::DiagEvent> {
        let interval = now.saturating_duration_since(self.started);
        if interval < Duration::from_secs(60) {
            return None;
        }
        let counts = std::mem::take(
            &mut *counts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        self.started = now;
        Some(crate::diag::record::DiagEvent::FetchDeliveryStats {
            interval_ms: interval.as_millis() as u64,
            cycles: std::mem::take(&mut self.cycles),
            content_deliveries: counts.content,
            unchanged_deliveries: counts.unchanged,
            fold_ms: std::mem::take(&mut self.fold_time).as_millis() as u64,
        })
    }
}

#[derive(Clone)]
pub(super) struct ResultSender(Weak<ResultMailbox>);

pub(super) struct ResultReceiver(Arc<ResultMailbox>);

pub(super) fn result_channel() -> (ResultSender, ResultReceiver) {
    let mailbox = Arc::new(ResultMailbox::default());
    (
        ResultSender(Arc::downgrade(&mailbox)),
        ResultReceiver(mailbox),
    )
}

impl ResultSender {
    #[cfg(test)]
    pub(super) fn send(
        &self,
        update: FetchUpdate,
    ) -> Result<(), std::sync::mpsc::SendError<FetchUpdate>> {
        self.send_tracked(update, None)
    }

    fn send_tracked(
        &self,
        mut update: FetchUpdate,
        counts: Option<Arc<Mutex<DeliveryCounts>>>,
    ) -> Result<(), std::sync::mpsc::SendError<FetchUpdate>> {
        let Some(mailbox) = self.0.upgrade() else {
            return Err(std::sync::mpsc::SendError(update));
        };
        let prior = {
            let delivery = mailbox
                .delivery
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            delivery
                .accepted
                .as_ref()
                .filter(|_| delivery.reusable)
                .cloned()
        };
        let unchanged = prior
            .as_ref()
            .is_some_and(|prior| update.same_content(prior));
        let mut pending = mailbox
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut delivery = mailbox
            .delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        delivery.counts = counts;
        // Fence the off-lock comparison against acknowledgement or invalidation.
        if unchanged
            && delivery.reusable
            && prior
                .as_ref()
                .zip(delivery.accepted.as_ref())
                .is_some_and(|(prior, accepted)| Arc::ptr_eq(prior, accepted))
        {
            update = update.into_unchanged();
        }
        pending.push(update);
        #[cfg(test)]
        mailbox.ready.notify_one();
        Ok(())
    }
}

impl ResultReceiver {
    pub(super) fn take(&self) -> PendingResults {
        let mut pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.snapshot.is_some() || pending.outcome.is_some() {
            self.0
                .delivery
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .reusable = false;
        }
        std::mem::take(&mut *pending)
    }

    pub(super) fn acknowledge(&self, mut update: FetchUpdate, reusable: bool) {
        let mut delivery = self
            .0
            .delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if update.is_success()
            && let Some(counts) = &delivery.counts
        {
            let mut counts = counts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if update.is_unchanged() {
                counts.unchanged += 1;
            } else {
                counts.content += 1;
            }
        }
        if update.snapshot_mut().is_some() || !update.is_success() {
            delivery.accepted = update.is_success().then(|| Arc::new(update));
        }
        delivery.reusable = reusable;
    }

    pub(super) fn allow_unchanged(&self, reusable: bool) {
        self.0
            .delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reusable = reusable;
    }

    pub(super) fn invalidate(&self) {
        let mut pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut delivery = self
            .0
            .delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        delivery.reusable = false;
        // Local input or fusion may have changed the consumer after this send.
        // Keep the queued clock, but give it content to reconcile again.
        if let (Some(update), Some(prior)) = (&mut pending.snapshot, &delivery.accepted) {
            update.restore_content(prior);
        }
    }

    #[cfg(test)]
    fn try_recv(&self) -> Result<FetchUpdate, std::sync::mpsc::TryRecvError> {
        self.0
            .pending
            .lock()
            .unwrap()
            .pop()
            .ok_or(std::sync::mpsc::TryRecvError::Empty)
    }

    #[cfg(test)]
    fn try_iter(&self) -> impl Iterator<Item = FetchUpdate> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }

    #[cfg(test)]
    fn recv(&self) -> Result<FetchUpdate, std::sync::mpsc::RecvError> {
        let pending = self.0.pending.lock().unwrap();
        self.0
            .ready
            .wait_while(pending, |pending| {
                pending.snapshot.is_none() && pending.outcome.is_none()
            })
            .unwrap()
            .pop()
            .ok_or(std::sync::mpsc::RecvError)
    }
}

/// One renderer a fetch worker feeds: where its folds go and how they are
/// projected for it.
#[derive(Clone)]
pub(super) struct Subscriber {
    pub(super) instance_id: SidebarInstanceId,
    pub(super) own_pane: Option<PaneId>,
    pub(super) tick_seconds: u64,
    pub(super) refresh_override: Option<u16>,
    pub(super) tx: ResultSender,
    pub(super) wake: SyncSender<Wakeup>,
}

/// What the eldest renderer decides for every choice the worker makes once
/// per fold: the pane the shared fold excludes and the data tick.
pub(super) struct Stand {
    pub(super) own_pane: Option<PaneId>,
    pub(super) tick: Duration,
}

impl Stand {
    /// `config` answers for a worker with no renderer, or one naming no pane.
    pub(super) fn of(eldest: Option<&Subscriber>, config: &ServeConfig) -> Self {
        Self {
            own_pane: eldest
                .and_then(|eldest| eldest.own_pane.clone())
                .or_else(|| config.own_pane.clone()),
            tick: tick_for(eldest.map_or(config.tick_seconds, |eldest| eldest.tick_seconds)),
        }
    }
}

/// The renderers one fetch worker feeds, keyed by instance id so they iterate
/// oldest subscriber first, providing a stable representative for each cycle.
pub(super) type Subscribers = Arc<Mutex<BTreeMap<String, Subscriber>>>;

struct ResultSink {
    subscribers: Subscribers,
    host_data: Arc<Mutex<HostData>>,
    /// The renderers this cycle serves, taken once as it starts.
    cycle: Vec<Subscriber>,
    covered: Covered,
    /// The cycle in hand, recorded as covered once its answer goes out.
    answering: Option<Coverage>,
    /// The fold behind the next snapshot publication and the worker's
    /// configured pane, kept so every renderer after the eldest gets its own
    /// projection of it.
    staged: Option<(WorkspaceFold, Option<PaneId>)>,
    context: Option<Arc<FoldShared>>,
    inputs: Arc<FoldInputs>,
    deliveries: Arc<Mutex<DeliveryCounts>>,
}

impl ResultSink {
    /// A sink feeding one renderer that folds as the worker's configured pane.
    #[cfg(test)]
    fn new(tx: ResultSender, wake: SyncSender<Wakeup>, refresh_override: Option<u16>) -> Self {
        let subscriber = Subscriber {
            instance_id: SidebarInstanceId::new(),
            own_pane: None,
            tick_seconds: 1,
            refresh_override,
            tx,
            wake,
        };
        Self::shared(
            Arc::new(Mutex::new(BTreeMap::from([(
                subscriber.instance_id.as_str().to_owned(),
                subscriber,
            )]))),
            Covered::default(),
            Arc::default(),
        )
    }

    fn shared(subscribers: Subscribers, covered: Covered, host_data: Arc<Mutex<HostData>>) -> Self {
        let mut sink = Self {
            subscribers,
            host_data,
            cycle: Vec::new(),
            covered,
            answering: None,
            staged: None,
            context: None,
            inputs: Arc::default(),
            deliveries: Arc::default(),
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

    /// Take the renderers one cycle serves. A pane that attaches later is
    /// answered by its own first fetch, and one that leaves is still sent the
    /// view it was folded for.
    fn begin_cycle(&mut self) {
        self.cycle = self.subscribers();
        self.context = None;
    }

    /// Deliver one update: to the eldest renderer as given, and to every
    /// other renderer as its own projection of the staged fold (a snapshot)
    /// or as a copy (anything else).
    fn publish(&mut self, mut update: FetchUpdate) {
        let mut staged = self.staged.take();
        if let (Some((fold, _)), Some(local)) = (&mut staged, update.snapshot_mut()) {
            let prior = self
                .host_data
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_fold
                .as_ref()
                .map(|fold| fold.workspace.clone());
            if let Some(prior) = prior {
                let shared = &mut fold.workspace.0;
                reuse_shared(
                    &mut shared.worktree_groups,
                    &mut local.worktree_groups,
                    &prior.0.worktree_groups,
                );
                reuse_shared(&mut shared.agents, &mut local.agents, &prior.0.agents);
                reuse_shared(
                    &mut shared.agent_panes,
                    &mut local.agent_panes,
                    &prior.0.agent_panes,
                );
                reuse_shared(
                    &mut shared.fenced_sessions,
                    &mut local.fenced_sessions,
                    &prior.0.fenced_sessions,
                );
                if let (Some(shared), Some(local), Some(prior)) = (
                    &mut shared.resume_outcomes,
                    &mut local.resume_outcomes,
                    &prior.0.resume_outcomes,
                ) {
                    reuse_shared(shared, local, prior);
                }
            }
        }
        let is_snapshot = matches!(update, FetchUpdate::Snapshot { .. });
        // Before the first send: a pane that reads this answer and asks again
        // must find its next request unanswered, or the worker would drop it.
        if matches!(
            update,
            FetchUpdate::Snapshot {
                phase: FetchPhase::Final,
                ..
            }
        ) && let Some(cycle) = self.answering.take()
        {
            self.covered.record(cycle);
        }
        let Some((eldest, others)) = self.cycle.split_first() else {
            return;
        };
        for subscriber in others {
            match (&update, &staged) {
                (FetchUpdate::Snapshot { phase, source, .. }, Some((fold, default_pane))) => {
                    let snapshot = project_local(
                        fold.workspace.clone(),
                        fold.frame.as_deref(),
                        subscriber.own_pane.as_ref().or(default_pane.as_ref()),
                    );
                    self.send(
                        subscriber,
                        FetchUpdate::Snapshot {
                            snapshot: Box::new(snapshot),
                            phase: *phase,
                            source: *source,
                        },
                    );
                }
                // A snapshot with no fold behind it is the eldest renderer's alone.
                (FetchUpdate::Snapshot { .. }, None) => {}
                _ => self.send(subscriber, update.clone()),
            }
        }
        self.send(eldest, update);
        if is_snapshot && let Some((fold, _)) = staged {
            self.host_data
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_fold = Some(fold);
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

    fn send(&self, subscriber: &Subscriber, mut update: FetchUpdate) {
        if let (Some(refresh_ms), Some(snapshot)) =
            (subscriber.refresh_override, update.snapshot_mut())
        {
            snapshot.theme.display.refresh_ms = refresh_ms;
        }
        if let Some(context) = &self.context {
            update = FetchUpdate::Shared {
                update: Box::new(update),
                context: context.clone(),
            };
        }
        // A renderer that already closed removes itself from the set. A full
        // inbox already holds a wake, so a dropped one loses nothing.
        if subscriber
            .tx
            .send_tracked(update, Some(self.deliveries.clone()))
            .is_ok()
        {
            let _ = subscriber.wake.try_send(Wakeup::Snapshot);
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
        // Several renderers share this worker: one cycle answers every
        // request already waiting, and none a published cycle answered.
        let covered = sink.covered.clone();
        drive(&request_rx, &covered, |cycle| {
            sink.answering = Some(cycle);
            // Re-resolved every cycle so `workspace migrate` repoints reads
            // without restarting the renderer.
            match resolve_state(self.config.workspace_id.clone()) {
                Ok(state) => {
                    let tick = self.meter.begin();
                    self.run_cycle(&state, cycle.request, &mut sink);
                    if let Some(event) = self.meter.finish(tick, crate::utils::time::unix_now_ms())
                    {
                        crate::sidebar::meter::report(&self.diag, event);
                    }
                }
                Err(err) => {
                    sink.begin_cycle();
                    sink.publish(FetchUpdate::Failed {
                        error: format!("resolving workspace state paths: {err}"),
                    });
                }
            }
            sink.answering = None;
        });
    }
}

/// Spawn the background fetch owner for every renderer in `subscribers`. It
/// runs until every request sender is gone, finishing the cycle in hand so its
/// durable and external side effects still land.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_fetch_worker(
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    producer: PaneProducer,
    last_projection: Option<crate::sidebar::agent_projection::AgentProjectionPublication>,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    subscribers: Subscribers,
    covered: Covered,
    host_data: Arc<Mutex<HostData>>,
    observer: Option<crate::sidebar::observe::RoomObserver>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        crate::lane::set(crate::lane::WorkLane::Fetch);
        let mut worker = FetchWorker::new(config, runtime, diag, producer, last_projection);
        worker.observer = observer;
        worker.run(
            request_rx,
            ResultSink::shared(subscribers, covered, host_data),
        );
    })
}

/// A cycle: when it started and the merged request it runs.
#[derive(Clone, Copy, Debug)]
struct Coverage {
    started: Instant,
    request: FetchRequest,
}

impl Coverage {
    /// Whether this cycle already did all `request` asks: it started after the
    /// request's cause was observed, and folded at least as strongly. Hard
    /// refreshes and forced folds are never answered for another request.
    fn answers(&self, request: &FetchRequest) -> bool {
        let cycle = self.request;
        let cache_floor_met = match (request.min_pane_cache_ms, cycle.min_pane_cache_ms) {
            (None, _) => true,
            (Some(asked), Some(ran)) => asked <= ran,
            (Some(_), None) => false,
        };
        request.observed_at < self.started
            && request.mode != FetchMode::HardRefresh
            && !request.force_fold
            && request.mode.strength() <= cycle.mode.strength()
            && cache_floor_met
    }
}

/// The plane's last answered cycle, shared by its fetch worker and every
/// pane's dispatcher. The worker records a cycle as it publishes the cycle's
/// final outcome, before sending it, so every renderer the cycle serves either
/// finds its next request answered here or receives that outcome after asking.
/// A request observed while a cycle runs costs one more cycle, never a lost
/// change.
#[derive(Clone, Default)]
pub(super) struct Covered(Arc<Mutex<Option<Coverage>>>);

impl Covered {
    fn answers(&self, request: &FetchRequest) -> bool {
        self.last().is_some_and(|cycle| cycle.answers(request))
    }

    fn record(&self, cycle: Coverage) {
        *self.lock() = Some(cycle);
    }

    fn last(&self) -> Option<Coverage> {
        *self.lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Coverage>> {
        // A plain value replaced whole, so a panic while held tears nothing.
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Run one cycle per batch of waiting requests, dropping every request the
/// last answered cycle covers. `cycle` records itself in `covered` once its
/// final outcome goes out.
fn drive(
    requests: &std::sync::mpsc::Receiver<FetchRequest>,
    covered: &Covered,
    mut cycle: impl FnMut(Coverage),
) {
    while let Ok(first) = requests.recv() {
        let last = covered.last();
        let mut batch: Option<FetchRequest> = None;
        let waiting = std::iter::once(first).chain(std::iter::from_fn(|| requests.try_recv().ok()));
        for request in waiting.filter(|request| !last.is_some_and(|cycle| cycle.answers(request))) {
            match &mut batch {
                Some(batch) => batch.merge(request),
                None => batch = Some(request),
            }
        }
        if let Some(request) = batch {
            cycle(Coverage {
                started: Instant::now(),
                request,
            });
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DeferredFetch {
    due_at: Instant,
    request: FetchRequest,
}

/// The render loop's handle to the fetch worker. It owns immediate, deferred,
/// in-flight, and follow-up scheduling so request strength and deadlines merge
/// in one place.
pub(super) struct FetchDispatcher {
    tx: Sender<FetchRequest>,
    covered: Covered,
    in_flight: bool,
    pending_refetch: Option<FetchRequest>,
    deferred: Option<DeferredFetch>,
}

impl FetchDispatcher {
    /// A dispatcher whose worker serves it alone.
    #[cfg(test)]
    pub(super) fn new(tx: Sender<FetchRequest>) -> Self {
        Self::for_plane(tx, Covered::default())
    }

    /// A pane's dispatcher on a shared plane, which skips a request one of the
    /// plane's finished cycles already answered.
    pub(super) fn for_plane(tx: Sender<FetchRequest>, covered: Covered) -> Self {
        Self {
            tx,
            covered,
            in_flight: false,
            pending_refetch: None,
            deferred: None,
        }
    }

    /// Ask the fetch worker for a fresh snapshot. `in_flight` collapses
    /// redundant requests while one is already running; `force_after` (set by a
    /// store delta, i.e. new committed data) guarantees one more fetch once
    /// the in-flight one returns, so a delta that races an in-flight fetch is
    /// never lost. `request` carries the strongest freshness requirement
    /// currently known.
    pub(super) fn request(&mut self, mut request: FetchRequest, force_after: bool) {
        let absorbed = self.deferred.take();
        if let Some(deferred) = absorbed {
            request.merge(deferred.request.observed_now());
        }
        self.dispatch(request, force_after || absorbed.is_some());
    }

    fn dispatch(&mut self, request: FetchRequest, force_after: bool) {
        if !self.in_flight {
            if self.covered.answers(&request) {
                return;
            }
            if self.tx.send(request).is_ok() {
                self.in_flight = true;
            }
        } else if force_after {
            match &mut self.pending_refetch {
                Some(pending) => pending.merge(request),
                None => self.pending_refetch = Some(request),
            }
        }
    }

    pub(super) fn defer_until(&mut self, request: FetchRequest, due_at: Instant) {
        if let Some(deferred) = &mut self.deferred {
            deferred.request.merge(request);
            deferred.due_at = deferred.due_at.min(due_at);
        } else {
            self.deferred = Some(DeferredFetch { due_at, request });
        }
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.deferred.map(|deferred| deferred.due_at)
    }

    pub(super) fn fire_due(&mut self, now: Instant) {
        let Some(deferred) = self.deferred.filter(|deferred| now >= deferred.due_at) else {
            return;
        };
        self.deferred = None;
        self.dispatch(deferred.request.observed_now(), true);
    }

    pub(super) fn complete(&mut self, dispatch_follow_up: bool) {
        self.in_flight = false;
        if dispatch_follow_up && let Some(request) = self.pending_refetch.take() {
            self.request(request, false);
        }
    }
}

#[cfg(test)]
mod tests;
