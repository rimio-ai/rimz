//! Host cache refresher: heavy enrichment lanes off the fetch worker.
//!
//! The fetch worker still publishes room folds each data tick.
//! This thread owns the TTL-gated spending, account, usage, auto-continue, loop
//! task firing, and diff-stats cache refreshes so a status flip never waits
//! behind them.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, error};

use crate::diag::record::TickLoop;
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::meter::TickMeter;
use crate::{RuntimePaths, StatePaths};

use super::RoomConfig;
use super::fetch::HostData;
use super::timing::tick_for;

const DAEMON_VIEW_REPAIR_TTL: Duration = Duration::from_secs(30);

/// Read the room's live data tick, or skip work while no pane subscribes.
pub(super) fn spawn(
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    data: Arc<Mutex<HostData>>,
    data_tick: impl Fn() -> Option<Duration> + Send + 'static,
) -> JoinHandle<()> {
    std::thread::spawn(move || refresh_loop(config, runtime, diag, data, data_tick))
}

fn refresh_loop(
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    data: Arc<Mutex<HostData>>,
    data_tick: impl Fn() -> Option<Duration>,
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
        std::thread::sleep(data_tick().unwrap_or_else(|| tick_for(config.tick_seconds)));
        if data_tick().is_none() {
            continue;
        }
        let state = match StatePaths::for_workspace(config.workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                debug!(error = %err, "sidebar cache refresh state paths unavailable");
                let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
                fire_timers(&runtime, config.mux, &now);
                continue;
            }
        };
        let tick = meter.begin();
        let inputs = data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .inputs
            .clone();
        let result = refresh_guarded(&mut cursor, |cursor| {
            crate::sidebar::produce::refresh_producer_caches(
                cursor,
                &state,
                &runtime,
                inputs.as_ref(),
                &mut refresh_state,
            )
        });
        let steps = refresh_state.take_pass_spawns();
        if let Some(event) = meter.finish(tick, crate::utils::time::unix_now_ms(), steps) {
            crate::sidebar::meter::report(&diag, event);
        }
        if let Err(err) = result {
            debug!(error = %err, "sidebar cache refresh failed");
        }
        let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
        fire_timers(&runtime, config.mux, &now);
        if daemon_checked_at.elapsed() >= DAEMON_VIEW_REPAIR_TTL {
            daemon_checked_at = Instant::now();
            daemon_tracker.maintain(
                daemon_backend.as_ref(),
                inputs.as_ref().map(|inputs| inputs.frame.as_ref()),
            );
        }
    }
}

fn fire_timers(runtime: &RuntimePaths, mux: crate::ids::MuxName, now: &jiff::Zoned) {
    let ci_source = crate::harness::schedule::when::CiSource::read(runtime);
    crate::harness::schedule::fire::fire_due_tasks(
        runtime,
        None,
        now,
        crate::harness::schedule::fire::LoopRunHost::Detached,
        Some(&ci_source),
    );
    crate::message::fire::wake_due_messages(runtime, mux, now);
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
