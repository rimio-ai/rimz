//! Debounced hidden-view geometry repair owned by the room host.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::diag::record::{DiagEvent, ViewFitTrigger};
use crate::ids::PaneId;
use crate::mux::focus_anchor::FocusAnchor;
use crate::mux::{ViewFit, ViewSize};

const SWEEP_SETTLE: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
struct Summary {
    target: ViewSize,
    fitted: usize,
    failed: usize,
    skipped: usize,
}

impl Summary {
    fn from_fit(target: ViewSize, count: usize, result: &crate::mux::Result<Vec<ViewFit>>) -> Self {
        let mut summary = Self {
            target,
            fitted: 0,
            failed: 0,
            skipped: 0,
        };
        match result {
            Ok(outcomes) => {
                for outcome in outcomes {
                    match outcome {
                        ViewFit::Fitted(_) => summary.fitted += 1,
                        ViewFit::Failed => summary.failed += 1,
                        ViewFit::Skipped => summary.skipped += 1,
                    }
                }
            }
            Err(_) => summary.failed = count,
        }
        summary
    }
}

#[derive(Default)]
struct SweepPlan {
    settle: Option<Instant>,
}

impl SweepPlan {
    fn kick(&mut self, now: Instant) {
        self.settle = Some(now + SWEEP_SETTLE);
    }

    fn ready(&mut self, now: Instant) -> bool {
        if !self.settle.is_some_and(|settle| now >= settle) {
            return false;
        }
        self.settle = None;
        true
    }

    fn delay(&self, now: Instant) -> Option<Duration> {
        self.settle
            .map(|settle| settle.saturating_duration_since(now))
    }
}

#[derive(Clone)]
pub(super) struct Sweep(Sender<Instant>);

pub(super) fn channel() -> (Sweep, Receiver<Instant>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Sweep(tx), rx)
}

impl Sweep {
    pub(super) fn kick(&self) {
        let _ = self.0.send(Instant::now());
    }

    pub(super) fn fit_jump(
        &self,
        anchor: &FocusAnchor,
        diag: &DiagSink,
        runtime: &crate::RuntimePaths,
    ) -> Option<HashMap<PaneId, crate::mux::FittedPane>> {
        let backend = crate::mux::backend_for(crate::ids::MuxName::Tmux);
        let lag = match backend.lagging_view(&anchor.session_name, &anchor.pane_id) {
            Ok(lag) => lag?,
            Err(err) => {
                tracing::debug!(error = %err, "hidden-view jump read failed");
                return None;
            }
        };
        let result = backend.fit_views(
            std::slice::from_ref(&lag.view),
            lag.target,
            sidebar_cols(runtime, lag.target),
        );
        emit(
            diag,
            Summary::from_fit(lag.target, 1, &result),
            ViewFitTrigger::Jump,
        );
        match result {
            Ok(outcomes) => outcomes.into_iter().find_map(|outcome| match outcome {
                ViewFit::Fitted(rows) => Some(rows),
                ViewFit::Skipped | ViewFit::Failed => None,
            }),
            Err(err) => {
                tracing::debug!(view = %lag.view, error = %err, "hidden-view jump fit failed");
                None
            }
        }
    }
}

pub(super) fn spawn(
    rx: Receiver<Instant>,
    session: String,
    diag: DiagSink,
    runtime: crate::RuntimePaths,
) {
    std::thread::spawn(move || {
        let backend = crate::mux::backend_for(crate::ids::MuxName::Tmux);
        let mut plan = SweepPlan::default();
        loop {
            while let Ok(kick) = rx.try_recv() {
                plan.kick(kick);
            }
            let now = Instant::now();
            if plan.ready(now) {
                let lag = match backend.lagging_views(&session) {
                    Ok(Some(lag)) => lag,
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::debug!(error = %err, "hidden-view sweep read failed");
                        continue;
                    }
                };
                let result =
                    backend.fit_views(&lag.views, lag.target, sidebar_cols(&runtime, lag.target));
                if let Err(err) = &result {
                    tracing::debug!(error = %err, "hidden-view sweep fit failed");
                }
                emit(
                    &diag,
                    Summary::from_fit(lag.target, lag.views.len(), &result),
                    ViewFitTrigger::Sweep,
                );
                continue;
            }
            let received = match plan.delay(now) {
                Some(delay) => rx.recv_timeout(delay),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(kick) => plan.kick(kick),
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

fn sidebar_cols(runtime: &crate::RuntimePaths, target: ViewSize) -> std::num::NonZeroU16 {
    let config = crate::config::MachineConfig::load_lenient();
    let width = crate::mux::SidebarWidth::from_config(&config.theme);
    crate::mux::width_target::resolve(runtime, width, Some(target.cols)).cols(Some(target.cols))
}

fn emit(diag: &DiagSink, summary: Summary, trigger: ViewFitTrigger) {
    if summary.fitted + summary.failed + summary.skipped == 0 {
        return;
    }
    diag.emit_unlimited(DiagEvent::HiddenViewsFitted {
        trigger,
        target_cols: summary.target.cols,
        target_rows: summary.target.rows,
        fitted: summary.fitted,
        failed: summary.failed,
        skipped: summary.skipped,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_burst_reads_once_after_the_last_settle() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        plan.kick(now);
        plan.kick(now + Duration::from_millis(300));
        assert!(!plan.ready(now + Duration::from_millis(799)));
        assert!(plan.ready(now + Duration::from_millis(800)));
        assert!(!plan.ready(now + Duration::from_secs(2)));
    }
}
