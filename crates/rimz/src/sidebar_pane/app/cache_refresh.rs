//! Producer cache refresher: the elder-owned heavy enrich lanes off the fetch worker.
//!
//! The fetch worker still publishes panes and projects caches each data tick.
//! This thread owns the TTL-gated spending, account, usage, auto-continue, loop
//! task firing, and diff-stats cache refreshes so a status flip never waits
//! behind them.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, error};

use crate::diag::record::TickLoop;
use crate::sidebar::ProducerElectionTracker;
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::meter::TickMeter;
use crate::{RuntimePaths, StatePaths};

use super::{ServeConfig, tick_for};

const DAEMON_VIEW_REPAIR_TTL: Duration = Duration::from_secs(30);

pub(super) fn spawn(
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    election: ProducerElectionTracker,
) -> JoinHandle<()> {
    std::thread::spawn(move || refresh_loop(config, runtime, diag, election))
}

fn refresh_loop(
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    election: ProducerElectionTracker,
) {
    crate::lane::set(crate::lane::WorkLane::CacheRefresh);
    let mut cursor = RollupCursor::new();
    let mut meter = TickMeter::new(TickLoop::CacheRefresh, tick_for(config.tick_seconds));
    let daemon_backend = crate::mux::backend_for(config.mux);
    let mut daemon_tracker = crate::daemon_view::DaemonRepairTracker::new(
        config.workspace_id.clone(),
        config.session_name.clone(),
    );
    let mut daemon_checked_at = Instant::now() - DAEMON_VIEW_REPAIR_TTL;
    let mut refresh_state = crate::sidebar::refresh::ProducerRefreshState::default();
    loop {
        std::thread::sleep(tick_for(config.tick_seconds));
        if election.elder_instance().is_some() {
            continue;
        }
        let state = match StatePaths::for_workspace(config.workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                debug!(error = %err, "sidebar cache refresh state paths unavailable");
                let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
                fire_elder_timers(&runtime, &now);
                continue;
            }
        };
        let tick = meter.begin();
        let result = refresh_guarded(&mut cursor, |cursor| {
            crate::sidebar::produce::refresh_producer_caches_with_state(
                cursor,
                &state,
                &runtime,
                &config.session_name,
                config.own_pane.as_ref(),
                &mut refresh_state,
            )
        });
        if let Some(event) = meter.finish(tick, crate::sidebar::timing::unix_now_ms()) {
            crate::sidebar::meter::report(&diag, event);
        }
        if let Err(err) = result {
            debug!(error = %err, "sidebar cache refresh failed");
        }
        let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
        fire_elder_timers(&runtime, &now);
        if daemon_checked_at.elapsed() >= DAEMON_VIEW_REPAIR_TTL {
            daemon_checked_at = Instant::now();
            daemon_tracker.maintain(daemon_backend.as_ref(), &runtime);
        }
    }
}

fn fire_elder_timers(runtime: &RuntimePaths, now: &jiff::Zoned) {
    crate::harness::schedule::fire::fire_due_tasks_for_room(runtime, now);
    crate::message::fire::wake_due_messages(runtime, now);
}

fn refresh_guarded(
    cursor: &mut RollupCursor,
    refresh: impl FnOnce(&mut RollupCursor) -> crate::sidebar::produce::Result<()>,
) -> std::result::Result<(), String> {
    let result = super::with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| refresh(cursor)))
    });
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(payload) => {
            error!(
                panic = %super::panic_payload_message(payload.as_ref(), "unknown panic payload"),
                "sidebar spending cache refresh panicked"
            );
            *cursor = RollupCursor::new();
            Err(format!(
                "sidebar cache refresh panicked: {}",
                super::panic_payload_message(payload.as_ref(), "unknown panic payload")
            ))
        }
    }
}
