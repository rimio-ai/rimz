//! The off-thread fetch machinery: the two-speed fetch cycle (the in-process
//! consumer fast lane plus the elder's in-process produce, sharing one warm
//! [`PublishedSnapshotReader`]), and its single-flight request coalescing.
//! `FetchWorker` owns cadence, election, request coalescing, notification state,
//! and typed result publication; the reader owns consumer fold memoization.
//! Everything here runs on a worker thread so the render/input loop never
//! blocks on pane production; heavy git/spend/account refreshes run on the
//! cache refresher.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::diag::record::TickLoop;
use crate::ids::{PaneId, SidebarInstanceId};
use crate::sidebar::ProducerElectionTracker;
use crate::sidebar::consumer::{PublishedSnapshotReader, RollupCursor};
use crate::sidebar::enrich::{WorkspaceSnapshot, project_local};
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::meter::TickMeter;
use crate::sidebar::notify::{LinkAlert, LinkNotificationState, Notification, NotificationState};
use crate::sidebar::read_marks::ReadMarks;
use crate::sidebar::unread::{ClearedUnread, OpenedUnread, UnreadEpisodes};
use crate::store::snapshot::SidebarSnapshot;
use crate::wakeup::events::SidebarEvent;
use crate::{RuntimePaths, StatePaths};

use super::ServeConfig;
use super::input::SNAPSHOT_WAKEUP;
use super::timing::tick_for;

/// Run one in-process produce behind a panic guard. The produce pipeline
/// folds store truth, runtime caches, and `/proc` on this worker thread; a
/// bug anywhere in it must cost one degraded outcome — the loop holds its
/// last good frame and raises the health line — never the renderer. The
/// workspace builds with unwinding panics; under a future `panic = "abort"`
/// this guard degrades to renderer death plus the election handoff, the
/// documented recovery either way.
///
/// `AssertUnwindSafe` is discharged by construction: the only state carried
/// across the unwind boundary is the cursor, and the panic arm replaces it —
/// a panic can interrupt the fold mid-update, so the next cycle refolds cold
/// rather than trusting a torn base. Everything else the closure captures is
/// read-only paths and options.
fn run_produce_guarded<T>(
    reader: &mut PublishedSnapshotReader,
    produce: impl FnOnce(&mut RollupCursor) -> crate::sidebar::produce::Result<T>,
) -> std::result::Result<T, String> {
    let result = super::with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            produce(reader.cursor_mut())
        }))
    });
    match result {
        Ok(Ok(snapshot)) => Ok(snapshot),
        Ok(Err(err)) => Err(err.to_string()),
        Err(payload) => {
            reader.reset_after_unwind();
            Err(format!(
                "sidebar produce panicked: {}",
                super::panic_payload_message(payload.as_ref(), "unknown panic payload")
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FetchRole {
    Producer,
    Consumer,
}

impl FetchRole {
    pub(super) fn is_producer(self) -> bool {
        self == Self::Producer
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
    Unchanged {
        role: FetchRole,
    },
    Snapshot {
        snapshot: Box<SidebarSnapshot>,
        role: FetchRole,
        phase: FetchPhase,
        source: SnapshotSource,
    },
    Failed {
        error: String,
        role: FetchRole,
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
    pub filter: Option<crate::sidebar::body_filter::BodyFilter>,
    pub marks: Arc<ReadMarks>,
}

impl Default for FoldInputs {
    fn default() -> Self {
        Self {
            sampled_at: Instant::now(),
            anchor: None,
            filter: None,
            marks: Arc::default(),
        }
    }
}

impl FoldInputs {
    fn read(runtime: &RuntimePaths) -> Self {
        Self {
            sampled_at: Instant::now(),
            anchor: crate::mux::focus_anchor::load(runtime),
            filter: crate::sidebar::body_filter::load(runtime),
            marks: ReadMarks::load_merged(runtime),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotSource {
    Published,
    Produced,
}

/// One fold before its per-renderer projection.
struct WorkspaceFold {
    workspace: WorkspaceSnapshot,
    frame: Option<Arc<PaneFrame>>,
}

struct SnapshotPublication {
    snapshot: SidebarSnapshot,
    role: FetchRole,
    phase: FetchPhase,
    source: SnapshotSource,
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
            }
        )
    }

    pub(super) fn role(&self) -> FetchRole {
        match self {
            Self::Shared { update, .. } => update.role(),
            Self::Unchanged { role } | Self::Snapshot { role, .. } | Self::Failed { role, .. } => {
                *role
            }
        }
    }

    fn snapshot_mut(&mut self) -> Option<&mut SidebarSnapshot> {
        match self {
            Self::Shared { update, .. } => update.snapshot_mut(),
            Self::Snapshot { snapshot, .. } => Some(snapshot),
            Self::Unchanged { .. } | Self::Failed { .. } => None,
        }
    }

    pub(super) fn into_parts(self) -> (Self, Option<Arc<FoldShared>>) {
        match self {
            Self::Shared { update, context } => (*update, Some(context)),
            update => (update, None),
        }
    }
}

/// Decides whether a cycle pays the produce from cheap pre-reads. The producer
/// runs on a hard refresh, on a producer-only topology refresh, or when the
/// published frame outlived one data tick (`None` age = no usable frame — cold
/// start) and its process-local attempt cadence is due. A consumer produces
/// only for a hard refresh; it never produces for topology freshness or a stale
/// frame. Staleness recovery is delegated to the election: once the dead
/// elder's heartbeat ages out (≤ one TTL) the next-eldest renderer *is* the
/// producer and recovers through the branch above, while everyone else keeps
/// folding the held panes with the event-fresh rollup. Exactly one producer at
/// any moment, never a per-consumer produce storm; the lone renderer is its own
/// next-eldest. This state records every attempt before the produce path, so
/// errors and forced refreshes cannot start an ordinary storm.
#[derive(Default)]
struct ProducerCadence {
    last_attempt: Option<Instant>,
}

impl ProducerCadence {
    fn start_attempt_if_due(
        &mut self,
        is_producer: bool,
        mode: FetchMode,
        frame_age_ms: Option<u64>,
        tick: Duration,
        now: Instant,
    ) -> bool {
        let normal_attempt_due = self
            .last_attempt
            .is_none_or(|last| now.saturating_duration_since(last) >= tick);
        let produce = match mode {
            FetchMode::Normal if is_producer => {
                normal_attempt_due && frame_age_ms.is_none_or(|age| age >= tick.as_millis() as u64)
            }
            FetchMode::Normal => false,
            FetchMode::ProducerFreshPanes => is_producer,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProducerElection {
    elder: Option<SidebarInstanceId>,
}

impl ProducerElection {
    fn is_producer(&self) -> bool {
        self.elder.is_none()
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
    election: ProducerElectionTracker,
    reader: PublishedSnapshotReader,
    producer_cadence: ProducerCadence,
    notifications: NotificationState,
    link_notifications: LinkNotificationState,
    last_election: Option<ProducerElection>,
    meter: TickMeter,
    projection_publisher: crate::sidebar::workspace_projection::WorkspaceProjectionPublisher,
    tab_name_memo: TabNameMemo,
    session_listed: fn(crate::MuxName, &str) -> bool,
    #[cfg(test)]
    on_read: Option<Box<dyn FnMut() + Send>>,
}

struct FastFold {
    result: crate::store::snapshot::Result<WorkspaceFold>,
    role: FetchRole,
    produce: bool,
}

impl FetchWorker {
    fn new(
        config: ServeConfig,
        runtime: RuntimePaths,
        diag: crate::diag::DiagSink,
        election: ProducerElectionTracker,
    ) -> Self {
        let reader = PublishedSnapshotReader::new(
            runtime.clone(),
            config.session_name.clone(),
            config.own_pane.clone(),
        );
        let meter = TickMeter::new(TickLoop::Fetch, tick_for(config.tick_seconds));
        Self {
            observer: None,
            config,
            runtime,
            diag,
            election,
            reader,
            producer_cadence: ProducerCadence::default(),
            notifications: NotificationState::default(),
            link_notifications: LinkNotificationState::default(),
            last_election: None,
            meter,
            projection_publisher: Default::default(),
            tab_name_memo: TabNameMemo::default(),
            session_listed: |mux, session| {
                crate::mux::backend_for(mux).session_accepts_agent_close(session)
            },
            #[cfg(test)]
            on_read: None,
        }
    }

    /// One fetch cycle, posting one or two outcomes. Runs on the fetch worker
    /// thread, keeping the produce's `list-panes` + git round-trips off the
    /// render/input loop so animation never stalls on it. `state` is resolved per
    /// cycle by the worker loop, so a `workspace migrate` lands without a restart.
    ///
    /// **Fast lane (every cycle, producer and consumer alike):** fold the
    /// event-fresh store rollup over the published pane frame entirely in process
    /// ([`crate::sidebar::consumer::read_published_snapshot`]) — no `list-panes`,
    /// no git. This is the paint that lands a status flip or a cost update within
    /// one wakeup, in single-digit milliseconds — and it runs even over an aged
    /// pane frame, so a dead producer stales only pane *presence* while status
    /// keeps flowing. On cold start, before any usable frame exists, this still
    /// returns a frameless rollup snapshot so startup waits do not read as refresh
    /// failures; the produce below recovers the pane frame.
    ///
    /// **Produce lane (the elder's reconciliation):**
    /// [`crate::sidebar::produce::produce_workspace_snapshot`] runs in process on this same
    /// worker — same thread, same warm cursor as the fast lane, so the rollup
    /// fold stays O(new log bytes) and promotion to producer is warm by
    /// construction. It refreshes pane truth and roots, then publishes the shared
    /// frame every other tab reads. One producer per
    /// workspace — the eldest live instance — and on it the produce is gated to
    /// the data tick: a store-delta storm paints per delta but produces at most
    /// once per tick. Heavy git/spend/account lanes are refreshed by the elder's
    /// cache refresher and projected here, so this worker stays responsible for
    /// pane truth, roots, notifications, and publish order. Topology freshness is
    /// producer-only: consumers wait for the
    /// producer's `PaneFramePublished` event and fold the new cache without
    /// locally producing. Only a hard refresh (reload/manual recovery) lets a
    /// consumer produce. Stale-frame recovery belongs to the election, not the
    /// consumers — a dead elder's heartbeat ages out within one TTL and the
    /// next-eldest *becomes* the producer, so a wedged producer costs one handoff,
    /// never an every-consumer produce storm (the old self-heal, whose
    /// single-flight loser wait was shorter than a `list-panes`, so every loser
    /// timed out into its own uncached produce). A lone renderer is its own
    /// next-eldest, so it still self-heals through the producer branch.
    fn run_cycle(&mut self, state: &StatePaths, request: FetchRequest, sink: &mut ResultSink) {
        sink.begin_cycle();
        if sink.cycle.is_empty() {
            return;
        }
        let role = self.observe_role();
        let now_ms = crate::utils::time::unix_now_ms();
        let frame_stamps =
            crate::sidebar::cache::published_frame_stamps(&self.runtime, &self.config.session_name);
        let recordable = consumer_stamp_recordable(request, role.is_producer());
        let unchanged = recordable && self.reader.fold_unchanged(state, now_ms);
        if consumer_stamp_skippable(request, role.is_producer()) && unchanged {
            sink.publish(FetchUpdate::Unchanged { role });
            return;
        }

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
        let fast = if role.is_producer() {
            self.read_and_publish_workspace(state)
        } else {
            self.reader.read_adopting_workspace(state)
        }
        .map(|(workspace, frame)| WorkspaceFold { workspace, frame });
        let tick = self.stand(sink).tick;
        let produce = self.start_produce_if_due(request, role, frame_stamps, &fast, now_ms, tick);
        let fast_fold_ok = self.publish_fast_fold(
            state,
            FastFold {
                result: fast,
                role,
                produce,
            },
            sink,
        );
        if fast_fold_ok && recordable {
            self.reader.record_fold(state, now_ms);
        } else {
            self.reader.clear_fold();
        }
        if produce {
            self.publish_produced_fold(state, request, role, sink);
        }
    }

    fn observe_role(&mut self) -> FetchRole {
        let election = ProducerElection {
            elder: self.election.elder_instance(),
        };
        let role = if election.is_producer() {
            FetchRole::Producer
        } else {
            FetchRole::Consumer
        };
        emit_producer_transition(&self.diag, &mut self.last_election, election);
        role
    }

    fn read_and_publish_workspace(
        &mut self,
        state: &StatePaths,
    ) -> crate::store::snapshot::Result<(WorkspaceSnapshot, Option<Arc<PaneFrame>>)> {
        let (workspace, frame) = self.reader.read_workspace(state)?;
        if let Some(frame) = frame.as_deref()
            && let Err(err) = self.projection_publisher.publish(
                &self.runtime,
                &self.config.session_name,
                &workspace,
                frame,
            )
        {
            tracing::debug!(error = %err, "workspace projection publish failed");
        }
        Ok((workspace, frame))
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
        if sink.cycle.len() < 2 {
            return project_local(fold.workspace, fold.frame.as_deref(), own_pane.as_ref());
        }
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
        role: FetchRole,
        frame_stamps: Option<(u64, u64)>,
        fast: &crate::store::snapshot::Result<WorkspaceFold>,
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
        self.producer_cadence.start_attempt_if_due(
            role.is_producer(),
            request.mode,
            frame_age_ms,
            tick,
            Instant::now(),
        )
    }

    fn publish_fast_fold(
        &mut self,
        state: &StatePaths,
        fold: FastFold,
        sink: &mut ResultSink,
    ) -> bool {
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
                        role: fold.role,
                        phase,
                        source: SnapshotSource::Published,
                    },
                    sink,
                );
                true
            }
            Err(err) if !fold.produce => {
                sink.publish(FetchUpdate::Failed {
                    error: err.to_string(),
                    role: fold.role,
                });
                false
            }
            // Producing cycle reports the produce fold's own error.
            Err(_) => false,
        }
    }

    fn publish_produced_fold(
        &mut self,
        state: &StatePaths,
        request: FetchRequest,
        role: FetchRole,
        sink: &mut ResultSink,
    ) {
        let opts = crate::sidebar::produce::ProduceOptions {
            mux: self.config.mux,
            session_name: self.config.session_name.clone(),
            exclude: self.stand(sink).own_pane,
            min_pane_cache_ms: request.min_pane_cache_ms,
            diag: self.diag.clone(),
        };
        match run_produce_guarded(&mut self.reader, |cursor| {
            crate::sidebar::produce::produce_workspace_snapshot(cursor, state, &self.runtime, &opts)
        }) {
            Ok(produced) => {
                if role.is_producer()
                    && let Err(err) = self.projection_publisher.publish(
                        &self.runtime,
                        &self.config.session_name,
                        &produced.workspace,
                        &produced.frame,
                    )
                {
                    tracing::debug!(error = %err, "workspace projection publish failed");
                }
                let frame = Arc::new(produced.frame);
                let snapshot = self.project_fold(
                    WorkspaceFold {
                        workspace: produced.workspace,
                        frame: Some(frame.clone()),
                    },
                    sink,
                );
                if role.is_producer() {
                    self.update_tab_names(&snapshot, &frame);
                }
                self.publish_snapshot(
                    state,
                    SnapshotPublication {
                        snapshot,
                        role,
                        phase: FetchPhase::Final,
                        source: SnapshotSource::Produced,
                    },
                    sink,
                );
            }
            Err(error) => sink.publish(FetchUpdate::Failed { error, role }),
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
            role,
            phase,
            source,
        } = publication;
        let final_producer = role.is_producer() && phase == FetchPhase::Final;
        let roster = (final_producer && source == SnapshotSource::Produced)
            .then(|| crate::sidebar::produce::live_roster_from_snapshot(&snapshot));
        let deliveries = if final_producer {
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
        if final_producer && let Some((fold, _)) = &mut sink.staged {
            let unread: HashMap<_, _> = snapshot
                .rows()
                .map(|row| (row.id.as_str(), row.unread))
                .collect();
            for row in fold
                .workspace
                .0
                .worktree_groups
                .iter_mut()
                .flat_map(|group| &mut group.rows)
            {
                if let Some(unread) = unread.get(row.id.as_str()) {
                    row.unread = *unread;
                }
            }
        }
        sink.publish(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            role,
            phase,
            source,
        });
        // After the send: a narrowing write waits on a mux listing, which must
        // not hold the frame.
        if let Some(roster) = roster
            && self.election.confirm_producer()
        {
            self.publish_live_roster(state, roster);
        }
        deliver_notifications(
            &self.config,
            &self.runtime,
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

fn consumer_stamp_skippable(request: FetchRequest, is_producer: bool) -> bool {
    !is_producer && request.allows_unchanged_skip()
}

fn consumer_stamp_recordable(request: FetchRequest, is_producer: bool) -> bool {
    !is_producer
        && request.mode == FetchMode::Normal
        && request.min_pane_cache_ms.is_none()
        && !request.force_fold
}

fn emit_producer_transition(
    diag: &crate::diag::DiagSink,
    last_election: &mut Option<ProducerElection>,
    election: ProducerElection,
) {
    let Some(prior) = last_election.replace(election.clone()) else {
        return;
    };
    match (prior.elder, election.elder) {
        (Some(prior_elder), None) => {
            diag.emit_unlimited(crate::diag::record::DiagEvent::ProducerElected { prior_elder })
        }
        (None, Some(new_elder)) => {
            diag.emit_unlimited(crate::diag::record::DiagEvent::ProducerDemoted { new_elder })
        }
        _ => {}
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
    let marks = ReadMarks::load_merged(runtime);
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
    config: &ServeConfig,
    runtime: &RuntimePaths,
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
        if let Err(err) = crate::wakeup::broadcast(
            runtime,
            Some(&config.session_name),
            SidebarEvent::Notify {
                title: notification.title,
                body: notification.body,
                panes: delivery.panes,
                recheck_unread: true,
                notification_kind: Some(delivery.notification_kind),
            },
        ) {
            tracing::debug!(error = %err, "notification event broadcast failed");
        }
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

/// One request to the fetch worker. The mode keeps topology signals producer-
/// only, while a hard refresh remains available for manual recovery. When a
/// request carries `min_pane_cache_ms`, any producing lane ignores a pane cache
/// older than the signal that asked for fresh topology.
#[derive(Clone, Copy, Debug)]
pub(super) struct FetchRequest {
    mode: FetchMode,
    min_pane_cache_ms: Option<u64>,
    published_frame_hint: bool,
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
            published_frame_hint: false,
            force_fold: false,
            observed_at: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FetchMode {
    #[default]
    Normal,
    ProducerFreshPanes,
    HardRefresh,
}

impl FetchMode {
    fn strength(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::ProducerFreshPanes => 1,
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
    pub(super) fn producer_fresh_panes() -> Self {
        Self {
            mode: FetchMode::ProducerFreshPanes,
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

    pub(super) fn pane_frame_published() -> Self {
        Self {
            published_frame_hint: true,
            ..Self::default()
        }
    }

    /// Fold from current caches even when the worker's unchanged-input memo
    /// would skip. Renderer-local timers use this when fold side effects depend
    /// on local state rather than store or pane-frame inputs.
    pub(super) fn force_fold() -> Self {
        Self {
            force_fold: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn is_producer_fresh_panes(self) -> bool {
        matches!(self.mode, FetchMode::ProducerFreshPanes)
    }

    #[cfg(test)]
    pub(super) fn forces_fold(self) -> bool {
        self.force_fold
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.mode = self.mode.strongest(other.mode);
        self.published_frame_hint |= other.published_frame_hint;
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

    fn allows_unchanged_skip(self) -> bool {
        self.mode == FetchMode::Normal
            && self.min_pane_cache_ms.is_none()
            && !self.published_frame_hint
            && !self.force_fold
    }
}

#[derive(Default)]
pub(super) struct PendingResults {
    pub(super) snapshot: Option<FetchUpdate>,
    pub(super) outcome: Option<FetchUpdate>,
    pub(super) completed: bool,
    pub(super) role: Option<FetchRole>,
}

impl PendingResults {
    fn push(&mut self, mut update: FetchUpdate) {
        let is_final = update.is_final();
        self.completed |= is_final;
        self.role = Some(update.role());
        if update.snapshot_mut().is_none() {
            self.outcome = Some(update);
            return;
        }
        // An unfinished cycle cannot displace a completed one the pane has
        // not seen. Its role still advances independently of that snapshot.
        if !is_final && self.snapshot.as_ref().is_some_and(FetchUpdate::is_final) {
            return;
        }
        self.snapshot = Some(update);
        if is_final {
            self.outcome = None;
        }
    }

    #[cfg(test)]
    fn pop(&mut self) -> Option<FetchUpdate> {
        self.snapshot.take().or_else(|| self.outcome.take())
    }
}

#[derive(Default)]
struct ResultMailbox {
    pending: Mutex<PendingResults>,
    #[cfg(test)]
    ready: std::sync::Condvar,
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
    pub(super) fn send(
        &self,
        update: FetchUpdate,
    ) -> Result<(), std::sync::mpsc::SendError<FetchUpdate>> {
        let Some(mailbox) = self.0.upgrade() else {
            return Err(std::sync::mpsc::SendError(update));
        };
        mailbox
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(update);
        #[cfg(test)]
        mailbox.ready.notify_one();
        Ok(())
    }
}

impl ResultReceiver {
    pub(super) fn take(&self) -> PendingResults {
        std::mem::take(
            &mut *self
                .0
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
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
    pub(super) socket_path: PathBuf,
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
/// eldest first, in the order the producer election ranks them.
pub(super) type Subscribers = Arc<Mutex<BTreeMap<String, Subscriber>>>;

struct ResultSink {
    waker: Option<UnixDatagram>,
    subscribers: Subscribers,
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
}

impl ResultSink {
    /// A sink feeding one renderer that folds as the worker's configured pane.
    #[cfg(test)]
    fn new(tx: ResultSender, socket_path: PathBuf, refresh_override: Option<u16>) -> Self {
        let subscriber = Subscriber {
            instance_id: SidebarInstanceId::new(),
            own_pane: None,
            tick_seconds: 1,
            refresh_override,
            tx,
            socket_path,
        };
        Self::shared(
            Arc::new(Mutex::new(BTreeMap::from([(
                subscriber.instance_id.as_str().to_owned(),
                subscriber,
            )]))),
            Covered::default(),
        )
    }

    fn shared(subscribers: Subscribers, covered: Covered) -> Self {
        let mut sink = Self {
            waker: nonblocking_waker(),
            subscribers,
            cycle: Vec::new(),
            covered,
            answering: None,
            staged: None,
            context: None,
            inputs: Arc::default(),
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
    fn publish(&mut self, update: FetchUpdate) {
        let staged = self.staged.take();
        // Before the first send: a pane that reads this answer and asks again
        // must find its next request unanswered, or the worker would drop it.
        if matches!(
            update,
            FetchUpdate::Unchanged { .. }
                | FetchUpdate::Snapshot {
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
                (
                    FetchUpdate::Snapshot {
                        role,
                        phase,
                        source,
                        ..
                    },
                    Some((fold, default_pane)),
                ) => {
                    let snapshot = project_local(
                        fold.workspace.clone(),
                        fold.frame.as_deref(),
                        subscriber.own_pane.as_ref().or(default_pane.as_ref()),
                    );
                    self.send(
                        subscriber,
                        FetchUpdate::Snapshot {
                            snapshot: Box::new(snapshot),
                            role: *role,
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
        if subscriber.tx.send(update).is_ok()
            && let Some(waker) = &self.waker
        {
            let _ = waker.send_to(SNAPSHOT_WAKEUP, &subscriber.socket_path);
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
                        role: FetchRole::Consumer,
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
    election: ProducerElectionTracker,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    subscribers: Subscribers,
    covered: Covered,
    observer: Option<crate::sidebar::observe::RoomObserver>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        crate::lane::set(crate::lane::WorkLane::Fetch);
        let mut worker = FetchWorker::new(config, runtime, diag, election);
        worker.observer = observer;
        worker.run(request_rx, ResultSink::shared(subscribers, covered));
    })
}

/// A sender that drops a wake rather than wait on a renderer's full inbox.
fn nonblocking_waker() -> Option<UnixDatagram> {
    let waker = UnixDatagram::unbound().ok()?;
    waker.set_nonblocking(true).ok()?;
    Some(waker)
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
            && (!request.published_frame_hint || cycle.published_frame_hint)
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

    pub(super) fn request_or_defer(
        &mut self,
        request: FetchRequest,
        immediate: bool,
        defer_for: Duration,
    ) {
        if immediate {
            self.request(request, true);
        } else {
            self.defer_until(request, Instant::now() + defer_for);
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

    #[cfg(test)]
    pub(super) fn deferred_request(&self) -> Option<FetchRequest> {
        self.deferred.map(|deferred| deferred.request)
    }

    pub(super) fn fire_due(&mut self, now: Instant) {
        let Some(deferred) = self.deferred.filter(|deferred| now >= deferred.due_at) else {
            return;
        };
        self.deferred = None;
        self.dispatch(deferred.request.observed_now(), true);
    }

    pub(super) fn clear_deferred(&mut self) {
        self.deferred = None;
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
