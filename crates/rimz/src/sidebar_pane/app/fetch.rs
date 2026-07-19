//! The off-thread fetch machinery: the two-speed fetch cycle (the in-process
//! consumer fast lane plus the elder's in-process produce, sharing one warm
//! [`PublishedSnapshotReader`]), and its single-flight request coalescing.
//! `FetchWorker` owns cadence, election, memoization, notification state, and
//! typed result publication. Everything here runs on a worker thread so the
//! render/input loop never blocks on pane production; heavy git/spend/account
//! refreshes run on the cache refresher.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::diag::record::TickLoop;
use crate::ids::{PaneId, SidebarInstanceId};
use crate::sidebar::ProducerElectionTracker;
use crate::sidebar::consumer::{
    ConsumerFoldInputsStamp, ConsumerSnapshotSource, PublishedSnapshotReader, RollupCursor,
};
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::meter::TickMeter;
use crate::sidebar::notify::{LinkAlert, LinkNotificationState, Notification, NotificationState};
use crate::sidebar::read_marks::ReadMarks;
use crate::sidebar::unread::{ClearedUnread, OpenedUnread, UnreadEpisodes};
use crate::{RuntimePaths, SidebarSnapshot, StatePaths};

use super::input::SNAPSHOT_WAKEUP;
use super::{ServeConfig, tick_for};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneFrame {
    Held,
    Fresh,
}

/// Typed publication from one fetch cycle. Variant shape rules out an
/// unchanged error, an interim failure, or conflicting protocol flags.
pub(super) enum FetchUpdate {
    Unchanged {
        role: FetchRole,
    },
    Snapshot {
        snapshot: Box<SidebarSnapshot>,
        role: FetchRole,
        phase: FetchPhase,
        pane_frame: PaneFrame,
    },
    Failed {
        error: String,
        role: FetchRole,
        pane_frame: PaneFrame,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotSource {
    Published,
    Produced,
}

struct SnapshotPublication {
    snapshot: SidebarSnapshot,
    role: FetchRole,
    phase: FetchPhase,
    pane_frame: PaneFrame,
    source: SnapshotSource,
}

impl FetchUpdate {
    pub(super) fn is_final(&self) -> bool {
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
            Self::Unchanged { role } | Self::Snapshot { role, .. } | Self::Failed { role, .. } => {
                *role
            }
        }
    }

    pub(super) fn pane_frame(&self) -> PaneFrame {
        match self {
            Self::Snapshot { pane_frame, .. } | Self::Failed { pane_frame, .. } => *pane_frame,
            Self::Unchanged { .. } => PaneFrame::Held,
        }
    }

    fn snapshot_mut(&mut self) -> Option<&mut SidebarSnapshot> {
        match self {
            Self::Snapshot { snapshot, .. } => Some(snapshot),
            Self::Unchanged { .. } | Self::Failed { .. } => None,
        }
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
/// [`crate::sidebar::produce::produce_snapshot`] runs in process on this same
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
const CONSUMER_UNCHANGED_BACKSTOP_MS: u64 = 30_000;

#[derive(Default)]
struct ConsumerFoldMemo {
    last_ok: Option<(ConsumerFoldInputsStamp, u64, bool)>,
}

impl ConsumerFoldMemo {
    fn should_skip(&self, stamp: &ConsumerFoldInputsStamp, now_ms: u64) -> bool {
        self.last_ok
            .as_ref()
            .is_some_and(|(last, folded_at_ms, _)| {
                last == stamp
                    && now_ms.saturating_sub(*folded_at_ms) < CONSUMER_UNCHANGED_BACKSTOP_MS
            })
    }

    fn record(&mut self, stamp: ConsumerFoldInputsStamp, at_ms: u64, adopted: bool) {
        self.last_ok = Some((stamp, at_ms, adopted));
    }

    fn last_was_adoption(&self) -> bool {
        self.last_ok
            .as_ref()
            .is_some_and(|(_, _, adopted)| *adopted)
    }

    fn clear(&mut self) {
        self.last_ok = None;
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

/// Owns all state that persists between fetch requests.
struct FetchWorker {
    config: ServeConfig,
    runtime: RuntimePaths,
    notification_prefs: NotificationsPrefs,
    diag: crate::diag::DiagSink,
    election: ProducerElectionTracker,
    reader: PublishedSnapshotReader,
    consumer_memo: ConsumerFoldMemo,
    producer_cadence: ProducerCadence,
    notifications: NotificationState,
    link_notifications: LinkNotificationState,
    last_election: Option<ProducerElection>,
    meter: TickMeter,
    projection_publisher: crate::sidebar::workspace_projection::WorkspaceProjectionPublisher,
}

struct FastFold {
    result: crate::store::snapshot::Result<SidebarSnapshot>,
    role: FetchRole,
    produce: bool,
    pane_frame: PaneFrame,
    stamp: Option<ConsumerFoldInputsStamp>,
    now_ms: u64,
    adopted: bool,
}

impl FetchWorker {
    fn new(
        config: ServeConfig,
        runtime: RuntimePaths,
        notification_prefs: NotificationsPrefs,
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
            config,
            runtime,
            notification_prefs,
            diag,
            election,
            reader,
            consumer_memo: ConsumerFoldMemo::default(),
            producer_cadence: ProducerCadence::default(),
            notifications: NotificationState::default(),
            link_notifications: LinkNotificationState::default(),
            last_election: None,
            meter,
            projection_publisher: Default::default(),
        }
    }

    fn run_cycle(&mut self, state: &StatePaths, request: FetchRequest, sink: &mut ResultSink) {
        let role = self.observe_role();
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let frame_stamps =
            crate::sidebar::cache::published_frame_stamps(&self.runtime, &self.config.session_name);
        let last_was_adoption = self.consumer_memo.last_was_adoption();
        let mut fold_stamp = consumer_stamp_recordable(request, role.is_producer()).then(|| {
            if last_was_adoption {
                self.reader.projection_inputs_stamp(state)
            } else {
                self.reader.inputs_stamp(state)
            }
        });
        if self.consumer_fold_unchanged(request, role, fold_stamp.as_ref(), now_ms) {
            sink.publish(FetchUpdate::Unchanged { role });
            return;
        }

        let (fast, adopted) = if role.is_producer() {
            (self.read_and_publish_workspace(state), false)
        } else {
            match self.reader.read_adopting(state) {
                Ok(read) => (
                    Ok(read.snapshot),
                    read.source == ConsumerSnapshotSource::Adoption,
                ),
                Err(err) => (Err(err), false),
            }
        };
        if consumer_stamp_recordable(request, role.is_producer()) && adopted != last_was_adoption {
            fold_stamp = Some(if adopted {
                self.reader.projection_inputs_stamp(state)
            } else {
                self.reader.inputs_stamp(state)
            });
        }
        let produce = self.start_produce_if_due(request, role, frame_stamps, &fast, now_ms);
        let pane_frame = fast_pane_frame(request, frame_stamps, &fast);
        let folded_consumer_ok = self.publish_fast_fold(
            state,
            FastFold {
                result: fast,
                role,
                produce,
                pane_frame,
                stamp: fold_stamp.take(),
                now_ms,
                adopted,
            },
            sink,
        );
        if produce {
            self.publish_produced_fold(state, request, role, sink);
        }
        if !folded_consumer_ok {
            self.consumer_memo.clear();
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
    ) -> crate::store::snapshot::Result<SidebarSnapshot> {
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
        Ok(crate::sidebar::enrich::project_local(
            workspace,
            frame.as_deref(),
            self.config.own_pane.as_ref(),
        ))
    }

    fn consumer_fold_unchanged(
        &self,
        request: FetchRequest,
        role: FetchRole,
        stamp: Option<&ConsumerFoldInputsStamp>,
        now_ms: u64,
    ) -> bool {
        consumer_stamp_skippable(request, role.is_producer())
            && stamp.is_some_and(|stamp| self.consumer_memo.should_skip(stamp, now_ms))
    }

    fn start_produce_if_due(
        &mut self,
        request: FetchRequest,
        role: FetchRole,
        frame_stamps: Option<(u64, u64)>,
        fast: &crate::store::snapshot::Result<SidebarSnapshot>,
        now_ms: u64,
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
            tick_for(self.config.tick_seconds),
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
            Ok(snapshot) => {
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
                        pane_frame: fold.pane_frame,
                        source: SnapshotSource::Published,
                    },
                    sink,
                );
                if let Some(stamp) = fold.stamp {
                    self.consumer_memo.record(stamp, fold.now_ms, fold.adopted);
                    return true;
                }
                false
            }
            Err(err) if !fold.produce => {
                sink.publish(FetchUpdate::Failed {
                    error: err.to_string(),
                    role: fold.role,
                    pane_frame: PaneFrame::Held,
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
        let pane_frame = if request.mode.produces_fresh_panes() {
            PaneFrame::Fresh
        } else {
            PaneFrame::Held
        };
        let opts = crate::sidebar::produce::ProduceOptions {
            mux: self.config.mux,
            session_name: self.config.session_name.clone(),
            exclude: self.config.own_pane.clone(),
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
                let snapshot = crate::sidebar::enrich::project_local(
                    produced.workspace,
                    Some(&produced.frame),
                    self.config.own_pane.as_ref(),
                );
                self.publish_snapshot(
                    state,
                    SnapshotPublication {
                        snapshot,
                        role,
                        phase: FetchPhase::Final,
                        pane_frame,
                        source: SnapshotSource::Produced,
                    },
                    sink,
                );
            }
            Err(error) => sink.publish(FetchUpdate::Failed {
                error,
                role,
                pane_frame,
            }),
        }
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
            pane_frame,
            source,
        } = publication;
        let final_producer = role.is_producer() && phase == FetchPhase::Final;
        if final_producer && source == SnapshotSource::Produced {
            let roster = crate::sidebar::produce::live_roster_from_snapshot(&snapshot);
            if let Err(err) = crate::store::live_roster::publish(&state.live_roster, roster) {
                tracing::debug!(
                    path = %state.live_roster.display(),
                    error = %err,
                    "live roster publish failed",
                );
            }
        }
        let deliveries = if final_producer {
            evaluate_notifications(
                &self.runtime,
                &self.notification_prefs,
                &mut self.notifications,
                &mut self.link_notifications,
                &self.diag,
                &mut snapshot,
            )
        } else {
            Vec::new()
        };
        sink.publish(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            role,
            phase,
            pane_frame,
        });
        deliver_notifications(
            &self.config,
            &self.runtime,
            &self.notification_prefs,
            &self.diag,
            deliveries,
        );
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

fn fast_pane_frame(
    request: FetchRequest,
    frame_stamps: Option<(u64, u64)>,
    fast: &crate::store::snapshot::Result<SidebarSnapshot>,
) -> PaneFrame {
    if fast.as_ref().is_ok_and(|snapshot| {
        snapshot.panes_produced_at_ms.is_some()
            && (request.published_frame_hint
                || request
                    .min_pane_cache_ms
                    .is_some_and(|min| frame_stamps.is_some_and(|(_, observed)| observed >= min)))
    }) {
        PaneFrame::Fresh
    } else {
        PaneFrame::Held
    }
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
    let now_ms = crate::sidebar::timing::unix_now_ms();
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
        if let Err(err) = crate::store::wakeup::broadcast_sidebar_event(
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
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FetchRequest {
    mode: FetchMode,
    min_pane_cache_ms: Option<u64>,
    published_frame_hint: bool,
    force_fold: bool,
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

    fn produces_fresh_panes(self) -> bool {
        matches!(self, Self::ProducerFreshPanes | Self::HardRefresh)
    }
}

impl FetchRequest {
    pub(super) fn producer_fresh_panes() -> Self {
        Self {
            mode: FetchMode::ProducerFreshPanes,
            min_pane_cache_ms: Some(crate::sidebar::timing::unix_now_ms()),
            published_frame_hint: false,
            force_fold: false,
        }
    }

    pub(super) fn hard_refresh() -> Self {
        Self {
            mode: FetchMode::HardRefresh,
            min_pane_cache_ms: Some(crate::sidebar::timing::unix_now_ms()),
            published_frame_hint: false,
            force_fold: false,
        }
    }

    pub(super) fn pane_frame_published() -> Self {
        Self {
            mode: FetchMode::Normal,
            min_pane_cache_ms: None,
            published_frame_hint: true,
            force_fold: false,
        }
    }

    /// Fold from current caches even when the worker's unchanged-input memo
    /// would skip. Renderer-local timers use this when fold side effects depend
    /// on local state rather than store or pane-frame inputs.
    pub(super) fn force_fold() -> Self {
        Self {
            mode: FetchMode::Normal,
            min_pane_cache_ms: None,
            published_frame_hint: false,
            force_fold: true,
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
        self.min_pane_cache_ms = match (self.min_pane_cache_ms, other.min_pane_cache_ms) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (Some(current), None) => Some(current),
            (None, Some(next)) => Some(next),
            (None, None) => None,
        };
    }

    fn allows_unchanged_skip(self) -> bool {
        self.mode == FetchMode::Normal
            && self.min_pane_cache_ms.is_none()
            && !self.published_frame_hint
            && !self.force_fold
    }
}

struct ResultSink {
    tx: Sender<FetchUpdate>,
    waker: Option<UnixDatagram>,
    socket_path: PathBuf,
    refresh_override: Option<u16>,
    disconnected: bool,
}

impl ResultSink {
    fn new(tx: Sender<FetchUpdate>, socket_path: PathBuf, refresh_override: Option<u16>) -> Self {
        Self {
            tx,
            waker: UnixDatagram::unbound().ok(),
            socket_path,
            refresh_override,
            disconnected: false,
        }
    }

    fn publish(&mut self, mut update: FetchUpdate) {
        if let (Some(refresh_ms), Some(snapshot)) = (self.refresh_override, update.snapshot_mut()) {
            snapshot.theme.display.refresh_ms = refresh_ms;
        }
        if self.tx.send(update).is_err() {
            self.disconnected = true;
            return;
        }
        if let Some(waker) = &self.waker {
            let _ = waker.send_to(SNAPSHOT_WAKEUP, &self.socket_path);
        }
    }
}

impl FetchWorker {
    fn run(mut self, request_rx: std::sync::mpsc::Receiver<FetchRequest>, mut sink: ResultSink) {
        while let Ok(first) = request_rx.recv() {
            let mut request = first;
            while let Ok(extra) = request_rx.try_recv() {
                request.merge(extra);
            }
            // Re-resolved every cycle so `workspace migrate` repoints reads
            // without restarting the renderer.
            match StatePaths::for_workspace(self.config.workspace_id.clone()) {
                Ok(state) => {
                    let tick = self.meter.begin();
                    self.run_cycle(&state, request, &mut sink);
                    if let Some(event) = self
                        .meter
                        .finish(tick, crate::sidebar::timing::unix_now_ms())
                    {
                        crate::sidebar::meter::report(&self.diag, event);
                    }
                }
                Err(err) => sink.publish(FetchUpdate::Failed {
                    error: format!("resolving workspace state paths: {err}"),
                    role: FetchRole::Consumer,
                    pane_frame: PaneFrame::Held,
                }),
            }
            // A closed loop still gets all current-cycle durable and external
            // side effects before the worker exits.
            if sink.disconnected {
                return;
            }
        }
    }
}

/// Spawn background fetch owner. Result sender and socket path travel together
/// because every successful send wakes that socket.
pub(super) fn spawn_fetch_worker(
    config: ServeConfig,
    runtime: RuntimePaths,
    notification_prefs: NotificationsPrefs,
    diag: crate::diag::DiagSink,
    election: ProducerElectionTracker,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    result: (std::sync::mpsc::Sender<FetchUpdate>, PathBuf),
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        crate::lane::set(crate::lane::WorkLane::Fetch);
        let refresh_override = config.refresh_ms_override;
        let worker = FetchWorker::new(config, runtime, notification_prefs, diag, election);
        let (result_tx, socket_path) = result;
        worker.run(
            request_rx,
            ResultSink::new(result_tx, socket_path, refresh_override),
        );
    })
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
    in_flight: bool,
    pending_refetch: Option<FetchRequest>,
    deferred: Option<DeferredFetch>,
}

impl FetchDispatcher {
    pub(super) fn new(tx: Sender<FetchRequest>) -> Self {
        Self {
            tx,
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
            request.merge(deferred.request);
        }
        self.dispatch(request, force_after || absorbed.is_some());
    }

    fn dispatch(&mut self, request: FetchRequest, force_after: bool) {
        if !self.in_flight {
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
        self.dispatch(deferred.request, true);
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
