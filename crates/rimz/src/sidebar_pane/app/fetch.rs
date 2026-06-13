//! The off-thread fetch machinery: the two-speed fetch cycle (the in-process
//! consumer fast lane plus the elder's in-process produce, sharing one warm
//! [`RollupCursor`]), and its single-flight request coalescing. Everything
//! here runs on a worker thread so the render/input loop never blocks on the
//! produce's `list-panes`/git round-trips.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::config::NotificationsPrefs;
use crate::ids::{PaneId, SidebarInstanceId};
use crate::schema::sidebar_event::SidebarEvent;
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::notify::{LinkAlert, LinkNotificationState, Notification, NotificationState};
use crate::{RuntimePaths, SidebarSnapshot, StatePaths};

use super::input::SNAPSHOT_WAKEUP;
use super::{ServeConfig, tick_for};

/// Run one in-process produce behind a panic guard. The produce pipeline
/// folds ledger truth, runtime caches, and `/proc` on this worker thread; a
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
fn run_produce_guarded(
    cursor: &mut RollupCursor,
    produce: impl FnOnce(&mut RollupCursor) -> crate::sidebar::produce::Result<SidebarSnapshot>,
) -> std::result::Result<SidebarSnapshot, String> {
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
                panic_payload_message(&payload)
            ))
        }
    }
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

/// One refresh cycle's result: the snapshot fetch outcome. A cycle can post
/// two — the in-process fast frame, then the produce that reconciles it.
/// `final_for_request` marks the cycle's last outcome: the loop completes the
/// dispatcher's in-flight request (and releases any deferred refetch) only on
/// it, so the single-flight discipline still counts whole cycles, not posts.
pub(super) struct FetchOutcome {
    pub(super) snapshot: std::result::Result<SidebarSnapshot, String>,
    pub(super) final_for_request: bool,
    pub(super) fresh_pane_frame: bool,
}

pub(super) fn apply_refresh_override(config: &ServeConfig, snapshot: &mut SidebarSnapshot) {
    if let Some(refresh_ms) = config.refresh_ms_override {
        snapshot.sidebar.refresh_ms = refresh_ms;
    }
}

/// One fetch cycle, posting one or two outcomes. Runs on the fetch worker
/// thread, keeping the produce's `list-panes` + git round-trips off the
/// render/input loop so animation never stalls on them. `state` is resolved
/// per cycle by the worker loop, so a `workspace migrate` lands without a
/// restart.
///
/// **Fast lane (every cycle, producer and consumer alike):** fold the
/// event-fresh ledger rollup over the published pane frame entirely in process
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
/// construction. It refreshes pane truth, git, spending, and accounts, and
/// publishes the shared caches every other tab reads. One producer per
/// workspace — the eldest live instance — and on it the produce is gated to
/// the data tick: a ledger-delta storm paints per delta but produces at most
/// once per tick. Topology freshness is producer-only: consumers wait for the
/// producer's `PaneFramePublished` event and fold the new cache without
/// locally producing. Only a hard refresh (reload/manual recovery) lets a
/// consumer produce. Stale-frame recovery belongs to the election, not the
/// consumers — a dead elder's heartbeat ages out within one TTL and the
/// next-eldest *becomes* the producer, so a wedged producer costs one handoff,
/// never an every-consumer produce storm (the old self-heal, whose
/// single-flight loser wait was shorter than a `list-panes`, so every loser
/// timed out into its own uncached produce). A lone renderer is its own
/// next-eldest, so it still self-heals through the producer branch.
struct FetchCycle<'a> {
    config: &'a ServeConfig,
    runtime: &'a RuntimePaths,
    state: &'a StatePaths,
    notification_prefs: &'a NotificationsPrefs,
    notifications: &'a mut NotificationState,
    link_notifications: &'a mut LinkNotificationState,
    diag: Option<&'a crate::diag::DiagSink>,
    last_election: &'a mut Option<ProducerElection>,
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

fn run_fetch_cycle(
    ctx: FetchCycle<'_>,
    request: FetchRequest,
    cursor: &mut RollupCursor,
    post: &mut dyn FnMut(FetchOutcome),
) {
    let FetchCycle {
        config,
        runtime,
        state,
        notification_prefs,
        notifications,
        link_notifications,
        diag,
        last_election,
    } = ctx;
    let election = ProducerElection {
        elder: crate::sidebar::elder_sidebar_instance(runtime, &config.instance_id),
    };
    let is_producer = election.is_producer();
    emit_producer_transition(diag, last_election, election);
    let exclude = config.own_pane.clone();
    let now_ms = crate::sidebar::cache::unix_now_ms();
    let published_frame_produced_at_ms =
        crate::sidebar::cache::published_frame_produced_at_ms(runtime, &config.session_name);
    let published_frame_observed_at_ms =
        crate::sidebar::cache::published_frame_observed_at_ms(runtime, &config.session_name);
    let fast = crate::sidebar::consumer::read_published_snapshot(
        cursor,
        state,
        runtime,
        &config.session_name,
        exclude.as_ref(),
    );
    // The published frame's age decides whether the producer pays the produce
    // this cycle: a frame younger than one data tick still carries the truth
    // the produce would re-derive (pane TTL, git TTL, spend caches all outlive
    // it). Read from the published pane-frame stamp, and only when the fast
    // lane actually folded that frame — an unreadable ledger must produce,
    // never coast on a young stamp it could not use.
    let frame_age_ms = fast
        .as_ref()
        .ok()
        .and(published_frame_produced_at_ms)
        .map(|produced_at_ms| now_ms.saturating_sub(produced_at_ms));
    let produce = produce_this_cycle(
        is_producer,
        request.mode,
        frame_age_ms,
        tick_for(config.tick_seconds).as_millis() as u64,
    );
    let fast_has_request_fresh_frame = fast.as_ref().is_ok_and(|snapshot| {
        snapshot.panes_produced_at_ms.is_some()
            && (request.published_frame_hint
                || request.min_pane_cache_ms.is_some_and(|min| {
                    published_frame_observed_at_ms
                        .is_some_and(|observed_at_ms| observed_at_ms >= min)
                }))
    });
    match fast {
        Ok(snapshot) => {
            if is_producer && !produce {
                evaluate_and_deliver_notifications(
                    config,
                    runtime,
                    notification_prefs,
                    notifications,
                    link_notifications,
                    diag,
                    &snapshot,
                );
            }
            post(FetchOutcome {
                snapshot: Ok(snapshot),
                final_for_request: !produce,
                fresh_pane_frame: fast_has_request_fresh_frame,
            });
        }
        // The consumer lane only misses when the ledger rollup itself could
        // not be read — a missing pane frame is a successful frameless fold.
        // With no produce to deliver the cycle's verdict, the miss is final
        // and carries the rollup error so the health line names the cause.
        Err(err) if !produce => post(FetchOutcome {
            snapshot: Err(err.to_string()),
            final_for_request: true,
            fresh_pane_frame: false,
        }),
        // An unreadable rollup on a producing cycle defers to the produce
        // below, which folds the same ledger and reports its own error.
        Err(_) => {}
    }
    if produce {
        let opts = crate::sidebar::produce::ProduceOptions {
            mux: config.mux,
            session_name: config.session_name.clone(),
            exclude,
            min_pane_cache_ms: request.min_pane_cache_ms,
            diag: diag.cloned(),
        };
        let produced = run_produce_guarded(cursor, |cursor| {
            crate::sidebar::produce::produce_snapshot(cursor, state, runtime, &opts)
        });
        if is_producer && let Ok(snapshot) = &produced {
            evaluate_and_deliver_notifications(
                config,
                runtime,
                notification_prefs,
                notifications,
                link_notifications,
                diag,
                snapshot,
            );
        }
        post(FetchOutcome {
            snapshot: produced,
            final_for_request: true,
            fresh_pane_frame: request.mode.produces_fresh_panes(),
        });
    }
}

fn emit_producer_transition(
    diag: Option<&crate::diag::DiagSink>,
    last_election: &mut Option<ProducerElection>,
    election: ProducerElection,
) {
    let Some(prior) = last_election.replace(election.clone()) else {
        return;
    };
    let Some(diag) = diag else {
        return;
    };
    match (prior.elder, election.elder) {
        (Some(prior_elder), None) => {
            diag.emit_unlimited(crate::schema::diag::DiagEvent::ProducerElected { prior_elder })
        }
        (None, Some(new_elder)) => {
            diag.emit_unlimited(crate::schema::diag::DiagEvent::ProducerDemoted { new_elder })
        }
        _ => {}
    }
}

fn evaluate_and_deliver_notifications(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    prefs: &NotificationsPrefs,
    state: &mut NotificationState,
    link_state: &mut LinkNotificationState,
    diag: Option<&crate::diag::DiagSink>,
    snapshot: &SidebarSnapshot,
) {
    let now_ms = crate::sidebar::cache::unix_now_ms();
    let mut notifications = state.evaluate(snapshot, prefs, now_ms);
    let link = link_state.evaluate(snapshot, prefs, now_ms);
    if let Some(alert) = link.alert {
        emit_link_alert(diag, alert);
    }
    if let Some(notification) = link.notification {
        notifications.push(notification);
    }
    for notification in notifications {
        if let Some(command) = prefs.command()
            && let Err(err) = crate::sidebar::notify::spawn_notify_command(command, &notification)
        {
            tracing::debug!(error = %err, "notify-command spawn failed");
        }
        let panes = notification_panes(snapshot, &notification);
        let notification_kind = notification.kind_env().to_owned();
        // Agent notifications ring the sticky tab bell only while the targeted
        // row is still unread; link reachability alerts ring directly.
        let recheck_unread = !matches!(
            notification.notification_kind,
            crate::sidebar::notify::NotificationKind::LinkDegraded
                | crate::sidebar::notify::NotificationKind::LinkRecovered
        );
        if let Some(diag) = diag {
            diag.trace_notify(notification_emitted_trace(&notification, &panes));
        }
        if let Err(err) = crate::ledger::wakeup::broadcast_sidebar_event(
            runtime,
            Some(&config.session_name),
            SidebarEvent::Notify {
                title: notification.title,
                body: notification.body,
                panes,
                recheck_unread,
                notification_kind: Some(notification_kind),
            },
        ) {
            tracing::debug!(error = %err, "notification event broadcast failed");
        }
    }
}

fn notification_emitted_trace(
    notification: &Notification,
    panes: &[PaneId],
) -> crate::schema::notify_trace::NotifyTraceEvent {
    use crate::schema::notify_trace::{NotifyTraceEvent, TraceAgent};
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
                prev_status: agent.prev_status.map(|status| status.as_str().to_owned()),
                new_status: agent.new_status.map(|status| status.as_str().to_owned()),
            })
            .collect(),
        panes: panes.to_vec(),
        unread_count: notification.unread_count,
    }
}

fn emit_link_alert(diag: Option<&crate::diag::DiagSink>, alert: LinkAlert) {
    let Some(diag) = diag else {
        return;
    };
    diag.emit(crate::schema::diag::DiagEvent::LinkAlert {
        tier: alert.tier,
        rtt_ms: alert.rtt_ms,
        miss_pct: alert.miss_pct,
        since_ms: alert.since_ms,
        recovered_after_ms: alert.recovered_after_ms,
    });
}

fn notification_panes(snapshot: &SidebarSnapshot, notification: &Notification) -> Vec<PaneId> {
    let panes = notification
        .agents
        .iter()
        .filter_map(|agent| agent.pane_id.clone())
        .collect::<Vec<_>>();
    if !panes.is_empty() {
        return panes;
    }
    if matches!(
        notification.notification_kind,
        crate::sidebar::notify::NotificationKind::LinkDegraded
            | crate::sidebar::notify::NotificationKind::LinkRecovered
    ) {
        return snapshot
            .own_view
            .as_ref()
            .map(|view| view.working_pane_ids.clone())
            .unwrap_or_default();
    }
    Vec::new()
}

/// Whether this cycle pays the produce, decided from cheap pre-reads. Pure, so
/// the produce-gating contract is unit-testable: the producer runs on a hard
/// refresh, on a producer-only topology refresh, or when the published frame
/// outlived one data tick (`None` age = no usable frame — cold start). A
/// consumer produces only for a hard refresh; it never produces for topology
/// freshness or a stale frame. Staleness recovery is delegated to the election:
/// once the dead elder's heartbeat ages out (≤ one TTL) the next-eldest renderer
/// *is* the producer and recovers through the branch above, while everyone else
/// keeps folding the held panes with the event-fresh rollup. Exactly one
/// producer at any moment, never a per-consumer produce storm; the lone renderer
/// is its own next-eldest.
fn produce_this_cycle(
    is_producer: bool,
    mode: FetchMode,
    frame_age_ms: Option<u64>,
    tick_ms: u64,
) -> bool {
    match mode {
        FetchMode::Normal if is_producer => frame_age_ms.is_none_or(|age| age >= tick_ms),
        FetchMode::Normal => false,
        FetchMode::ProducerFreshPanes => is_producer,
        FetchMode::HardRefresh => true,
    }
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
            min_pane_cache_ms: Some(crate::sidebar::cache::unix_now_ms()),
            published_frame_hint: false,
        }
    }

    pub(super) fn hard_refresh() -> Self {
        Self {
            mode: FetchMode::HardRefresh,
            min_pane_cache_ms: Some(crate::sidebar::cache::unix_now_ms()),
            published_frame_hint: false,
        }
    }

    pub(super) fn pane_frame_published() -> Self {
        Self {
            mode: FetchMode::Normal,
            min_pane_cache_ms: None,
            published_frame_hint: true,
        }
    }

    fn merge(&mut self, other: Self) {
        self.mode = self.mode.strongest(other.mode);
        self.published_frame_hint |= other.published_frame_hint;
        self.min_pane_cache_ms = match (self.min_pane_cache_ms, other.min_pane_cache_ms) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (Some(current), None) => Some(current),
            (None, Some(next)) => Some(next),
            (None, None) => None,
        };
    }
}

/// Spawn the background fetch worker. It blocks for a request, coalesces any
/// that piled up (a ledger-delta storm collapses to one fetch), runs one
/// [`run_fetch_cycle`], hands the result back over `result_tx`, and pokes the
/// loop's wakeup socket so it folds the result without polling. The thread ends
/// when the loop drops `request_tx`.
pub(super) fn spawn_fetch_worker(
    config: ServeConfig,
    runtime: RuntimePaths,
    socket_path: PathBuf,
    notification_prefs: NotificationsPrefs,
    diag: Option<crate::diag::DiagSink>,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    result_tx: std::sync::mpsc::Sender<FetchOutcome>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        // The worker's in-memory fold base, shared by the fast lane and the
        // produce: each cycle's rollup read folds only the log bytes appended
        // since the last one, instead of re-parsing the persisted base per
        // delta — and promotion to producer inherits the warm base.
        let mut cursor = RollupCursor::new();
        let mut notifications = NotificationState::default();
        let mut link_notifications = LinkNotificationState::default();
        let mut last_election = None;
        while let Ok(first) = request_rx.recv() {
            // Coalesce any requests that piled up into one run, keeping the
            // strongest intent and the newest pane-freshness floor.
            let mut request = first;
            while let Ok(extra) = request_rx.try_recv() {
                request.merge(extra);
            }
            // Post each outcome as it lands and poke the loop per post, so the
            // fast in-process frame paints while the produce (if any) still
            // runs.
            let mut disconnected = false;
            let mut post = |mut outcome: FetchOutcome| {
                if let Ok(snapshot) = &mut outcome.snapshot {
                    apply_refresh_override(&config, snapshot);
                }
                if result_tx.send(outcome).is_err() {
                    disconnected = true;
                    return;
                }
                if let Some(waker) = &waker {
                    let _ = waker.send_to(SNAPSHOT_WAKEUP, &socket_path);
                }
            };
            // Re-resolved every cycle (not cached at spawn), so a
            // `workspace migrate` repoints the ledger without a restart.
            match StatePaths::for_workspace(config.workspace_id.clone()) {
                Ok(state) => {
                    run_fetch_cycle(
                        FetchCycle {
                            config: &config,
                            runtime: &runtime,
                            state: &state,
                            notification_prefs: &notification_prefs,
                            notifications: &mut notifications,
                            link_notifications: &mut link_notifications,
                            diag: diag.as_ref(),
                            last_election: &mut last_election,
                        },
                        request,
                        &mut cursor,
                        &mut post,
                    );
                }
                Err(err) => post(FetchOutcome {
                    snapshot: Err(format!("resolving workspace state paths: {err}")),
                    final_for_request: true,
                    fresh_pane_frame: false,
                }),
            }
            if disconnected {
                return;
            }
        }
    })
}

/// The render loop's handle to the fetch worker. It owns the single-flight
/// accounting: at most one cycle is in flight, and at most one merged request
/// waits behind it when a forced event races that cycle.
pub(super) struct FetchDispatcher {
    tx: Sender<FetchRequest>,
    in_flight: bool,
    pending_refetch: Option<FetchRequest>,
}

impl FetchDispatcher {
    pub(super) fn new(tx: Sender<FetchRequest>) -> Self {
        Self {
            tx,
            in_flight: false,
            pending_refetch: None,
        }
    }

    /// Ask the fetch worker for a fresh snapshot. `in_flight` collapses
    /// redundant requests while one is already running; `force_after` (set by a
    /// ledger delta, i.e. new committed data) guarantees one more fetch once
    /// the in-flight one returns, so a delta that races an in-flight fetch is
    /// never lost. `request` carries the strongest freshness requirement
    /// currently known.
    pub(super) fn request(&mut self, request: FetchRequest, force_after: bool) {
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

    pub(super) fn mark_request_complete(&mut self) {
        self.in_flight = false;
    }

    pub(super) fn take_pending(&mut self) -> Option<FetchRequest> {
        self.pending_refetch.take()
    }
}

#[cfg(test)]
mod tests;
