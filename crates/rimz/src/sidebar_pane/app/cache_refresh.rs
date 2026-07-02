//! Producer cache refresher: the elder-owned heavy enrich lanes off the fetch worker.
//!
//! The fetch worker still publishes panes and projects caches each data tick.
//! This thread owns the TTL-gated spending, account, usage, auto-continue, loop
//! task firing, and diff-stats cache refreshes so a status flip never waits
//! behind them.

use std::thread::JoinHandle;

use tracing::{debug, error};

use crate::agents::spending::SpendingWalker;
use crate::schema::diag::TickLoop;
use crate::sidebar::consumer::RollupCursor;
use crate::sidebar::meter::TickMeter;
use crate::{RuntimePaths, StatePaths};

use super::{ServeConfig, tick_for};

pub(super) fn spawn(
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
) -> JoinHandle<()> {
    std::thread::spawn(move || refresh_loop(config, runtime, diag))
}

fn refresh_loop(config: ServeConfig, runtime: RuntimePaths, diag: crate::diag::DiagSink) {
    crate::lane::set(crate::lane::WorkLane::CacheRefresh);
    let mut cursor = RollupCursor::new();
    let mut spending_walker = SpendingWalker::new();
    let mut meter = TickMeter::new(TickLoop::CacheRefresh, tick_for(config.tick_seconds));
    loop {
        std::thread::sleep(tick_for(config.tick_seconds));
        if crate::sidebar::elder_sidebar_instance(&runtime, &config.instance_id).is_some() {
            continue;
        }
        let state = match StatePaths::for_workspace(config.workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                debug!(error = %err, "sidebar cache refresh state paths unavailable");
                let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
                crate::harness::schedule::fire::fire_due_tasks(&runtime, &now);
                crate::message::fire::wake_due_messages(&runtime, &now);
                continue;
            }
        };
        let tick = meter.begin();
        let result = refresh_guarded(&mut cursor, &mut spending_walker, |cursor, walker| {
            crate::sidebar::produce::refresh_producer_caches(
                cursor,
                walker,
                &state,
                &runtime,
                &config.session_name,
                config.own_pane.as_ref(),
            )
        });
        if let Some(event) = meter.finish(tick, crate::sidebar::timing::unix_now_ms()) {
            crate::sidebar::meter::report(&diag, event);
        }
        if let Err(err) = result {
            debug!(error = %err, "sidebar cache refresh failed");
        }
        let now = jiff::Timestamp::now().to_zoned(config.timezone.clone());
        crate::harness::schedule::fire::fire_due_tasks(&runtime, &now);
        crate::message::fire::wake_due_messages(&runtime, &now);
    }
}

fn refresh_guarded(
    cursor: &mut RollupCursor,
    spending_walker: &mut SpendingWalker,
    refresh: impl FnOnce(&mut RollupCursor, &mut SpendingWalker) -> crate::sidebar::produce::Result<()>,
) -> std::result::Result<(), String> {
    let result = super::with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            refresh(cursor, spending_walker)
        }))
    });
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(payload) => {
            error!(
                panic = %super::panic_payload_message(payload.as_ref()),
                "sidebar spending cache refresh panicked"
            );
            *cursor = RollupCursor::new();
            *spending_walker = SpendingWalker::new();
            Err(format!(
                "sidebar cache refresh panicked: {}",
                super::panic_payload_message(payload.as_ref())
            ))
        }
    }
}
