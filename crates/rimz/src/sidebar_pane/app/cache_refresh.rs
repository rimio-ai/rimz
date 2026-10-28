//! Host cache refresher: heavy enrichment lanes off the fetch worker.
//!
//! The fetch worker still publishes room folds each data tick.
//! This thread consumes those sampled folds for TTL-gated spending, account,
//! usage, auto-continue and diff-stats refreshes. Timers and daemon repair run
//! even when no fresh fold arrives, so a status flip never waits behind them.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, error};

use crate::diag::record::TickLoop;
use crate::sidebar::meter::TickMeter;
use crate::{RuntimePaths, StatePaths};

use super::RoomConfig;
use super::fetch::{FetchRequest, HostData, WorkspaceFold};
use super::timing::tick_for;

const DAEMON_VIEW_REPAIR_TTL: Duration = Duration::from_secs(30);

fn acquire_fold(
    data: &Mutex<HostData>,
    requests: &Sender<FetchRequest>,
    tick: Duration,
) -> std::result::Result<Arc<WorkspaceFold>, String> {
    let views = data
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .views
        .clone();
    let (receiver, requested_at) = {
        let mut views = views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(fold) = views.fresh_fold(Instant::now(), tick) {
            return Ok(fold);
        }
        let requested_at = Instant::now();
        requests
            .send(FetchRequest::default())
            .map_err(|error| error.to_string())?;
        let receiver = views.wait_for_fold(requested_at);
        (receiver, requested_at)
    };
    receiver
        .recv_timeout(tick.saturating_sub(requested_at.elapsed()))
        .map_err(|error| format!("waiting for sidebar fold: {error}"))?
}

fn refresh_pass(
    data: &Mutex<HostData>,
    requests: &Sender<FetchRequest>,
    tick: Duration,
    refresh: impl FnOnce(&WorkspaceFold) -> crate::sidebar::produce::Result<()>,
    maintain: impl FnOnce(),
) -> std::result::Result<(), String> {
    let result =
        acquire_fold(data, requests, tick).and_then(|fold| refresh_guarded(|| refresh(&fold)));
    maintain();
    result
}

/// Read the room's live data tick, or skip work while no pane subscribes.
pub(super) fn spawn(
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    data: Arc<Mutex<HostData>>,
    requests: Sender<FetchRequest>,
    data_tick: impl Fn() -> Option<Duration> + Send + 'static,
) -> JoinHandle<()> {
    std::thread::spawn(move || refresh_loop(config, runtime, diag, data, requests, data_tick))
}

fn refresh_loop(
    config: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    data: Arc<Mutex<HostData>>,
    requests: Sender<FetchRequest>,
    data_tick: impl Fn() -> Option<Duration>,
) {
    crate::lane::set(crate::lane::WorkLane::CacheRefresh);
    let mut meter = TickMeter::new(TickLoop::CacheRefresh, tick_for(config.tick_seconds));
    let daemon_backend = crate::mux::backend_for(config.mux);
    let mut daemon_tracker = crate::daemon_view::DaemonRepairTracker::new(
        config.workspace_id.clone(),
        config.session_name.clone(),
    );
    let mut daemon_checked_at = Instant::now() - DAEMON_VIEW_REPAIR_TTL;
    let mut refresh_state = crate::sidebar::refresh::ProducerRefreshState::default();
    let mut last_message_sweep = None;
    loop {
        let interval = data_tick().unwrap_or_else(|| tick_for(config.tick_seconds));
        std::thread::sleep(interval);
        if data_tick().is_none() {
            continue;
        }
        let state = StatePaths::for_workspace(config.workspace_id.clone());
        let result = refresh_pass(
            &data,
            &requests,
            interval,
            |fold| {
                let state = state?;
                let tick = meter.begin();
                let result = crate::sidebar::produce::refresh_producer_caches(
                    fold.workspace.clone(),
                    fold.frame.as_deref(),
                    &state,
                    &runtime,
                    &mut refresh_state,
                );
                let steps = refresh_state.take_pass_spawns();
                if let Some(event) = meter.finish(tick, crate::utils::time::unix_now_ms(), steps) {
                    crate::sidebar::meter::report(&diag, event);
                }
                result
            },
            || {
                let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
                fire_timers(&runtime, config.mux, &now, &mut last_message_sweep);
                if daemon_checked_at.elapsed() >= DAEMON_VIEW_REPAIR_TTL {
                    daemon_checked_at = Instant::now();
                    let inputs = data
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .inputs
                        .clone();
                    daemon_tracker.maintain(
                        daemon_backend.as_ref(),
                        inputs.as_ref().map(|inputs| inputs.frame.as_ref()),
                    );
                }
            },
        );
        if let Err(err) = result {
            debug!(error = %err, "sidebar cache refresh skipped or failed");
        }
    }
}

fn fire_timers(
    runtime: &RuntimePaths,
    mux: crate::ids::MuxName,
    now: &jiff::Zoned,
    last_message_sweep: &mut Option<(jiff::Timestamp, jiff::Timestamp)>,
) {
    let ci_source = crate::harness::schedule::when::CiSource::read(runtime);
    crate::harness::schedule::fire::fire_due_tasks(
        runtime,
        None,
        now,
        crate::harness::schedule::fire::LoopRunHost::Detached,
        Some(&ci_source),
    );
    crate::message::fire::wake_due_messages(runtime, mux, now, last_message_sweep);
}

fn refresh_guarded(
    refresh: impl FnOnce() -> crate::sidebar::produce::Result<()>,
) -> std::result::Result<(), String> {
    let result = super::with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(refresh))
    });
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(payload) => {
            error!(
                panic = %super::panic_payload_message(payload.as_ref(), "unknown panic payload"),
                "sidebar spending cache refresh panicked"
            );
            Err(format!(
                "sidebar cache refresh panicked: {}",
                super::panic_payload_message(payload.as_ref(), "unknown panic payload")
            ))
        }
    }
}

#[cfg(test)]
mod tests;
