//! Debounced, paced hidden-view geometry repair owned by the room host.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::diag::record::{DiagEvent, ViewFitTrigger};
use crate::ids::{PaneId, ViewId};
use crate::mux::focus_anchor::FocusAnchor;
use crate::mux::{LaggingViews, ViewSize};

const SWEEP_SETTLE: Duration = Duration::from_millis(500);
const SWEEP_STEP: Duration = Duration::from_millis(200);

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Read,
    Fit(ViewId, ViewSize),
}

#[derive(Debug, PartialEq, Eq)]
struct Summary {
    target: ViewSize,
    fitted: usize,
    failed: usize,
    skipped: usize,
}

#[derive(Default)]
struct SweepPlan {
    settle: Option<Instant>,
    next_fit: Option<Instant>,
    batch: Option<Batch>,
}

struct Batch {
    summary: Summary,
    queue: VecDeque<ViewId>,
}

impl SweepPlan {
    fn command(&mut self, command: Command) {
        match command {
            Command::Kick(now) => self.kick(now),
            Command::Fitted(view) => self.remove(&view),
        }
    }

    fn kick(&mut self, now: Instant) {
        self.settle = Some(now + SWEEP_SETTLE);
    }

    fn action(&mut self, now: Instant) -> Option<Action> {
        if let Some(settle) = self.settle {
            if now < settle {
                return None;
            }
            self.settle = None;
            return Some(Action::Read);
        }
        if self.next_fit.is_some_and(|next| now < next) {
            return None;
        }
        let batch = self.batch.as_ref()?;
        batch
            .queue
            .front()
            .map(|view| Action::Fit(view.clone(), batch.summary.target))
    }

    fn replace(&mut self, lag: Option<LaggingViews>) -> Option<Summary> {
        let previous = self.batch.take().map(|batch| batch.summary);
        self.batch = lag.map(|lag| Batch {
            summary: Summary {
                target: lag.target,
                fitted: 0,
                failed: 0,
                skipped: 0,
            },
            queue: lag.views.into(),
        });
        previous
    }

    fn record_fit(&mut self, view: &ViewId, now: Instant, success: Option<bool>) {
        self.remove(view);
        self.next_fit = Some(now + SWEEP_STEP);
        if let Some(batch) = &mut self.batch {
            match success {
                Some(true) => batch.summary.fitted += 1,
                Some(false) => batch.summary.failed += 1,
                None => batch.summary.skipped += 1,
            }
        }
    }

    fn remove(&mut self, view: &ViewId) {
        if let Some(batch) = &mut self.batch {
            batch.queue.retain(|queued| queued != view);
        }
    }

    fn completed(&mut self) -> Option<Summary> {
        if self.settle.is_some() || !self.batch.as_ref()?.queue.is_empty() {
            return None;
        }
        self.batch.take().map(|batch| batch.summary)
    }

    fn delay(&self, now: Instant) -> Option<Duration> {
        if let Some(settle) = self.settle {
            return Some(settle.saturating_duration_since(now));
        }
        let batch = self.batch.as_ref()?;
        (!batch.queue.is_empty())
            .then(|| self.next_fit.unwrap_or(now).saturating_duration_since(now))
    }
}

#[derive(Clone)]
pub(super) struct Sweep(Sender<Command>);

pub(super) enum Command {
    Kick(Instant),
    Fitted(ViewId),
}

pub(super) fn channel() -> (Sweep, Receiver<Command>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Sweep(tx), rx)
}

impl Sweep {
    pub(super) fn kick(&self) {
        let _ = self.0.send(Command::Kick(Instant::now()));
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
        let result = backend.fit_view(&lag.view, lag.target, sidebar_cols(runtime, lag.target));
        emit(
            diag,
            Summary {
                target: lag.target,
                fitted: usize::from(matches!(&result, Ok(Some(_)))),
                failed: usize::from(result.is_err()),
                skipped: usize::from(matches!(&result, Ok(None))),
            },
            ViewFitTrigger::Jump,
        );
        match result {
            Ok(Some(rows)) => {
                let _ = self.0.send(Command::Fitted(lag.view));
                Some(rows)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::debug!(view = %lag.view, error = %err, "hidden-view jump fit failed");
                None
            }
        }
    }
}

pub(super) fn spawn(
    rx: Receiver<Command>,
    session: String,
    diag: DiagSink,
    runtime: crate::RuntimePaths,
) {
    std::thread::spawn(move || {
        let backend = crate::mux::backend_for(crate::ids::MuxName::Tmux);
        let mut plan = SweepPlan::default();
        loop {
            while let Ok(command) = rx.try_recv() {
                plan.command(command);
            }
            if let Some(summary) = plan.completed() {
                emit(&diag, summary, ViewFitTrigger::Sweep);
            }
            let now = Instant::now();
            match plan.action(now) {
                Some(Action::Read) => {
                    let lag = match backend.lagging_views(&session) {
                        Ok(lag) => lag,
                        Err(err) => {
                            tracing::debug!(error = %err, "hidden-view sweep read failed");
                            None
                        }
                    };
                    if let Some(summary) = plan.replace(lag) {
                        emit(&diag, summary, ViewFitTrigger::Sweep);
                    }
                }
                Some(Action::Fit(view, target)) => {
                    let result = backend.fit_view(&view, target, sidebar_cols(&runtime, target));
                    if let Err(err) = &result {
                        tracing::debug!(%view, error = %err, "hidden-view sweep fit failed");
                    }
                    let success = match result {
                        Ok(Some(_)) => Some(true),
                        Ok(None) => None,
                        Err(_) => Some(false),
                    };
                    plan.record_fit(&view, Instant::now(), success);
                }
                None => {
                    let received = match plan.delay(now) {
                        Some(delay) => rx.recv_timeout(delay),
                        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    };
                    match received {
                        Ok(command) => plan.command(command),
                        Err(RecvTimeoutError::Timeout) => (),
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
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

    fn lag(views: &[&str]) -> Option<LaggingViews> {
        Some(LaggingViews {
            target: ViewSize {
                cols: 240,
                rows: 70,
            },
            views: views
                .iter()
                .map(|view| ViewId::new_unchecked(*view))
                .collect(),
        })
    }

    fn fit(view: &str) -> Option<Action> {
        Some(Action::Fit(
            ViewId::new_unchecked(view),
            ViewSize {
                cols: 240,
                rows: 70,
            },
        ))
    }

    #[test]
    fn kick_burst_reads_once_after_the_last_settle() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        plan.kick(now);
        plan.kick(now + Duration::from_millis(300));
        assert_eq!(plan.action(now + Duration::from_millis(799)), None);
        assert_eq!(
            plan.action(now + Duration::from_millis(800)),
            Some(Action::Read)
        );
        plan.replace(lag(&[]));
        assert_eq!(plan.action(now + Duration::from_secs(2)), None);
        assert!(plan.completed().is_some());
        assert_eq!(plan.completed(), None);
    }

    #[test]
    fn mru_steps_and_a_restart_keep_the_resize_gap_and_replace_the_queue() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        plan.replace(lag(&["recent", "slow", "old"]));
        assert_eq!(plan.action(now), fit("recent"));
        plan.record_fit(&ViewId::new_unchecked("recent"), now, Some(true));
        assert_eq!(
            plan.action(now + SWEEP_STEP - Duration::from_nanos(1)),
            None
        );
        assert_eq!(plan.action(now + SWEEP_STEP), fit("slow"));
        plan.kick(now + Duration::from_millis(210));
        // A slow fit returns just before a pending restart settles.
        plan.record_fit(
            &ViewId::new_unchecked("slow"),
            now + Duration::from_millis(700),
            Some(true),
        );
        assert_eq!(plan.action(now + Duration::from_millis(709)), None);
        assert_eq!(
            plan.action(now + Duration::from_millis(710)),
            Some(Action::Read)
        );
        plan.replace(lag(&["new", "replacement"]));
        assert_eq!(plan.action(now + Duration::from_millis(710)), None);
        assert_eq!(plan.action(now + Duration::from_millis(900)), fit("new"));
        plan.record_fit(
            &ViewId::new_unchecked("new"),
            now + Duration::from_millis(900),
            Some(true),
        );
        assert_eq!(
            plan.action(now + Duration::from_millis(1100)),
            fit("replacement")
        );
    }

    #[test]
    fn jump_removes_a_queued_view_and_a_failed_fit_does_not_stop_the_sweep() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        plan.replace(lag(&["first", "jump", "skipped", "last"]));
        plan.remove(&ViewId::new_unchecked("jump"));
        assert_eq!(plan.action(now), fit("first"));
        plan.record_fit(&ViewId::new_unchecked("first"), now, Some(false));
        assert_eq!(plan.action(now + SWEEP_STEP), fit("skipped"));
        plan.record_fit(&ViewId::new_unchecked("skipped"), now + SWEEP_STEP, None);
        assert_eq!(plan.action(now + SWEEP_STEP * 2), fit("last"));
        plan.record_fit(
            &ViewId::new_unchecked("last"),
            now + SWEEP_STEP * 2,
            Some(true),
        );
        assert_eq!(
            plan.completed(),
            Some(Summary {
                target: ViewSize {
                    cols: 240,
                    rows: 70
                },
                fitted: 1,
                failed: 1,
                skipped: 1,
            })
        );
        assert_eq!(plan.action(now + Duration::from_secs(1)), None);
    }
}
