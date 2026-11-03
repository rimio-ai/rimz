//! Paced, interruptible hidden-view geometry repair owned by the room host.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::diag::record::{DiagEvent, ViewFitTrigger};
use crate::ids::{PaneId, ViewId};
use crate::mux::focus_anchor::FocusAnchor;
use crate::mux::{LaggingViews, ViewFit, ViewSize};

const SWEEP_SETTLE: Duration = Duration::from_millis(500);
const SWEEP_GAP: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
struct Summary {
    target: ViewSize,
    fitted: usize,
    failed: usize,
    skipped: usize,
    remaining: usize,
}

impl Summary {
    fn new(target: ViewSize) -> Self {
        Self {
            target,
            fitted: 0,
            failed: 0,
            skipped: 0,
            remaining: 0,
        }
    }

    fn from_fit(target: ViewSize, count: usize, result: &crate::mux::Result<Vec<ViewFit>>) -> Self {
        let mut summary = Self::new(target);
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
    due: Option<Instant>,
    next_slice: Option<Instant>,
    run: Option<Run>,
}

struct Run {
    summary: Summary,
    attempted: HashSet<ViewId>,
}

impl SweepPlan {
    fn kick(&mut self, now: Instant) -> Option<Summary> {
        let summary = self.finish();
        self.due = Some(now + SWEEP_SETTLE);
        summary
    }

    fn ready(&mut self, now: Instant) -> bool {
        if self.delay(now) != Some(Duration::ZERO) {
            return false;
        }
        self.due = None;
        true
    }

    fn delay(&self, now: Instant) -> Option<Duration> {
        self.due.map(|due| {
            self.next_slice
                .map_or(due, |next| due.max(next))
                .saturating_duration_since(now)
        })
    }

    fn read(&mut self, lag: &mut LaggingViews) -> Option<Summary> {
        let previous = if self
            .run
            .as_ref()
            .is_some_and(|run| run.summary.target != lag.target)
        {
            self.finish()
        } else {
            None
        };
        let run = self.run.get_or_insert_with(|| Run {
            summary: Summary::new(lag.target),
            attempted: HashSet::new(),
        });
        lag.views.retain(|view| !run.attempted.contains(view));
        run.summary.remaining = lag.views.len();
        if lag.views.is_empty() {
            let completed = self.finish();
            return previous.or(completed);
        }
        previous
    }

    fn fit_ready(&mut self, now: Instant, quiet_until: Option<Instant>) -> bool {
        self.run.is_some() && self.due.is_none() && !self.wait_for_quiet(now, quiet_until)
    }

    fn wait_for_quiet(&mut self, now: Instant, quiet_until: Option<Instant>) -> bool {
        let Some(until) = quiet_until.filter(|until| now < *until) else {
            return false;
        };
        self.due = Some(until);
        true
    }

    fn record_fit(
        &mut self,
        views: &[ViewId],
        started: Instant,
        finished: Instant,
        result: &crate::mux::Result<Vec<ViewFit>>,
    ) {
        let Some(run) = &mut self.run else {
            return;
        };
        let slice = Summary::from_fit(run.summary.target, views.len(), result);
        run.summary.fitted += slice.fitted;
        run.summary.failed += slice.failed;
        run.summary.skipped += slice.skipped;
        run.summary.remaining -= views.len();
        run.attempted.extend(views.iter().cloned());
        let gap = SWEEP_GAP.max(
            finished
                .saturating_duration_since(started)
                .saturating_mul(9),
        );
        self.next_slice = Some(finished + gap);
        self.due = self.next_slice;
    }

    fn finish(&mut self) -> Option<Summary> {
        self.due = None;
        self.run.take().map(|run| run.summary)
    }
}

fn slice(lag: &LaggingViews) -> Vec<ViewId> {
    use super::timing::{HIDDEN_FIT_BUDGET, HIDDEN_FIT_WINDOW_CAP, HIDDEN_FIT_WINDOW_COST};

    let mut views = Vec::new();
    let mut cost = 0_u64;
    for view in &lag.views {
        // Every view in the backend's sweep read carries its normal-screen history cost.
        let weight = lag.reflow_lines[view].saturating_add(HIDDEN_FIT_WINDOW_COST);
        if !views.is_empty()
            && (views.len() == HIDDEN_FIT_WINDOW_CAP
                || cost.saturating_add(weight) > HIDDEN_FIT_BUDGET)
        {
            break;
        }
        views.push(view.clone());
        cost = cost.saturating_add(weight);
    }
    views
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
    mirrors: Arc<super::mirror_queue::MirrorQueue>,
    backend: Box<dyn crate::mux::MuxBackend>,
) {
    std::thread::spawn(move || {
        let mut plan = SweepPlan::default();
        loop {
            drain_kicks(&rx, &mut plan, &diag);
            let now = Instant::now();
            if plan.ready(now) && !plan.wait_for_quiet(now, mirrors.quiet_until()) {
                let started = Instant::now();
                let mut lag = match backend.lagging_views(&session) {
                    Ok(Some(lag)) => lag,
                    Ok(None) => {
                        emit_run(&diag, plan.finish());
                        continue;
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "hidden-view sweep read failed");
                        emit_run(&diag, plan.finish());
                        continue;
                    }
                };
                emit_run(&diag, plan.read(&mut lag));
                if lag.views.is_empty() {
                    continue;
                }
                mirrors.rank_views(&mut lag.views);
                let views = slice(&lag);
                let cols = sidebar_cols(&runtime, lag.target);
                let quiet_until = mirrors.quiet_until();
                drain_kicks(&rx, &mut plan, &diag);
                if !plan.fit_ready(Instant::now(), quiet_until) {
                    continue;
                }
                let result = backend.fit_views(&views, lag.target, cols);
                plan.record_fit(&views, started, Instant::now(), &result);
                if let Err(err) = &result {
                    tracing::debug!(error = %err, "hidden-view sweep fit failed");
                }
                continue;
            }
            let received = match plan.delay(now) {
                Some(delay) => rx.recv_timeout(delay),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(kick) => emit_run(&diag, plan.kick(kick)),
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => {
                    emit_run(&diag, plan.finish());
                    return;
                }
            }
        }
    });
}

fn drain_kicks(rx: &Receiver<Instant>, plan: &mut SweepPlan, diag: &DiagSink) {
    for kick in rx.try_iter() {
        emit_run(diag, plan.kick(kick));
    }
}

fn emit_run(diag: &DiagSink, summary: Option<Summary>) {
    if let Some(summary) = summary {
        emit(diag, summary, ViewFitTrigger::Sweep);
    }
}

#[cfg(feature = "testkit")]
pub(super) mod testkit {
    use super::{DiagSink, Sweep};

    /// A production sweep kept alive by its kick sender.
    pub struct HiddenViewSweep(Sweep);

    impl HiddenViewSweep {
        /// Request a run after the last kick settles.
        pub fn kick(&self) {
            self.0.kick();
        }
    }

    /// Start the production worker; run completion is recorded at [`DiagSink::log_path`].
    pub fn start_hidden_view_sweep(
        backend: crate::mux::TmuxBackend,
        session: String,
        diag: DiagSink,
        runtime: crate::RuntimePaths,
    ) -> HiddenViewSweep {
        let (sweep, rx) = super::channel();
        super::spawn(
            rx,
            session,
            diag,
            runtime,
            std::sync::Arc::default(),
            Box::new(backend),
        );
        HiddenViewSweep(sweep)
    }
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
        remaining: summary.remaining,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_burst_reads_once_after_the_last_settle() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        let _ = plan.kick(now);
        let _ = plan.kick(now + Duration::from_millis(300));
        assert!(!plan.ready(now + Duration::from_millis(799)));
        assert!(plan.ready(now + Duration::from_millis(800)));
        assert!(!plan.ready(now + Duration::from_secs(2)));
    }

    fn lag(lines: &[u64]) -> LaggingViews {
        let views = (0..lines.len())
            .map(|index| crate::ids::ViewId::new_unchecked(format!("@{index}")))
            .collect::<Vec<_>>();
        LaggingViews {
            target: ViewSize {
                cols: 240,
                rows: 65,
            },
            reflow_lines: views.iter().cloned().zip(lines.iter().copied()).collect(),
            views,
        }
    }

    fn reading(lag: &mut LaggingViews) -> (SweepPlan, Instant) {
        let mut plan = SweepPlan::default();
        let now = Instant::now();
        let _ = plan.kick(now);
        let now = now + SWEEP_SETTLE;
        assert!(plan.ready(now));
        assert!(plan.read(lag).is_none());
        (plan, now)
    }

    #[test]
    fn packing_stops_at_the_line_budget_in_rank_order() {
        let lag = lag(&[100_000, 47_000, 0]);
        assert_eq!(slice(&lag), lag.views[..2]);
    }

    #[test]
    fn an_over_budget_window_goes_alone_and_never_starves() {
        let lag = lag(&[u64::MAX, 0, 0]);
        assert_eq!(slice(&lag), lag.views[..1]);
    }

    #[test]
    fn empty_history_windows_still_obey_the_window_cap() {
        let lag = lag(&[0; 20]);
        assert_eq!(slice(&lag), lag.views[..8]);
    }

    #[test]
    fn slice_gap_covers_the_whole_read_and_fit_and_survives_kicks() {
        for (wall_ms, gap_ms) in [(10, 250), (60, 540)] {
            let mut lag = lag(&[0, 0]);
            let (mut plan, started) = reading(&mut lag);
            let finished = started + Duration::from_millis(wall_ms);
            plan.record_fit(
                &lag.views[..1],
                started,
                finished,
                &Ok(vec![ViewFit::Failed]),
            );
            assert_eq!(plan.delay(finished), Some(Duration::from_millis(gap_ms)));
            assert!(!plan.ready(finished + Duration::from_millis(gap_ms - 1)));
            assert!(plan.ready(finished + Duration::from_millis(gap_ms)));
            let _ = plan.kick(finished);
            let restart_ms = gap_ms.max(500);
            assert!(!plan.ready(finished + Duration::from_millis(restart_ms - 1)));
            assert!(plan.ready(finished + Duration::from_millis(restart_ms)));
        }
    }

    #[test]
    fn kick_between_slices_ends_the_run_with_its_remainder() {
        let mut lag = lag(&[0; 3]);
        let (mut plan, now) = reading(&mut lag);
        plan.record_fit(&lag.views[..1], now, now, &Ok(vec![ViewFit::Failed]));
        let summary = plan.kick(now + Duration::from_millis(1));
        assert!(summary.is_some(), "an interrupted run must be recorded");
        let summary = summary.unwrap();
        assert_eq!((summary.failed, summary.remaining), (1, 2));
        assert!(!plan.ready(now + Duration::from_millis(500)));
        assert!(plan.ready(now + Duration::from_millis(501)));
    }

    #[test]
    fn kick_during_the_read_prevents_that_slices_fit() {
        let mut lag = lag(&[0; 3]);
        let (mut plan, now) = reading(&mut lag);
        let _ = plan.kick(now + Duration::from_millis(10));
        assert!(!plan.fit_ready(now + Duration::from_millis(11), None));
        assert!(!plan.ready(now + Duration::from_millis(509)));
        assert!(plan.ready(now + Duration::from_millis(510)));
    }

    #[test]
    fn input_quiet_defers_the_fit_without_ending_the_run() {
        let mut lag = lag(&[0; 2]);
        let (mut plan, now) = reading(&mut lag);
        plan.record_fit(&lag.views[..1], now, now, &Ok(vec![ViewFit::Failed]));
        let now = now + Duration::from_millis(250);
        assert!(plan.ready(now));
        assert!(plan.read(&mut lag).is_none());
        let quiet_until = now + super::super::timing::MIRROR_QUIET;
        assert!(!plan.fit_ready(now, Some(quiet_until)));
        assert!(!plan.ready(quiet_until - Duration::from_millis(1)));
        assert!(plan.ready(quiet_until));
        assert!(plan.read(&mut lag).is_none());
        assert!(plan.fit_ready(quiet_until, Some(quiet_until)));
        plan.record_fit(
            &lag.views,
            quiet_until,
            quiet_until,
            &Ok(vec![ViewFit::Skipped]),
        );
        let summary = plan.finish().unwrap();
        assert_eq!(
            (summary.failed, summary.skipped, summary.remaining),
            (1, 1, 0)
        );
    }

    #[test]
    fn input_quiet_defers_the_read_until_the_stamp_expires() {
        let now = Instant::now();
        let mut plan = SweepPlan::default();
        let _ = plan.kick(now);
        let now = now + SWEEP_SETTLE;
        let quiet_until = now + super::super::timing::MIRROR_QUIET;
        assert!(plan.ready(now));
        assert!(plan.wait_for_quiet(now, Some(quiet_until)));
        assert!(!plan.ready(quiet_until - Duration::from_millis(1)));
        assert!(plan.ready(quiet_until));
        assert!(!plan.wait_for_quiet(quiet_until, Some(quiet_until)));
    }

    #[test]
    fn failed_and_skipped_windows_are_attempted_once_per_run() {
        let mut lag = lag(&[0; 2]);
        let original = lag.clone();
        let (mut plan, now) = reading(&mut lag);
        plan.record_fit(&lag.views[..1], now, now, &Ok(vec![ViewFit::Failed]));
        assert!(plan.ready(now + Duration::from_millis(250)));
        assert!(plan.read(&mut lag).is_none());
        assert_eq!(lag.views, original.views[1..]);
        plan.record_fit(&lag.views, now, now, &Ok(vec![ViewFit::Skipped]));
        let summary = plan.read(&mut original.clone());
        assert!(summary.is_some(), "no unattempted views ends the run");
        let summary = summary.unwrap();
        assert_eq!(
            (summary.failed, summary.skipped, summary.remaining),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_changed_target_starts_with_an_empty_attempted_set() {
        let mut lag = lag(&[0; 2]);
        let (mut plan, now) = reading(&mut lag);
        plan.record_fit(&lag.views[..1], now, now, &Ok(vec![ViewFit::Failed]));
        assert!(plan.ready(now + Duration::from_millis(250)));
        lag.target.cols = 260;
        let summary = plan.read(&mut lag);
        assert!(summary.is_some(), "a new target ends the previous run");
        assert_eq!(summary.unwrap().remaining, 1);
        assert_eq!(lag.views.len(), 2, "retry the first window at the new size");
        plan.record_fit(
            &lag.views,
            now,
            now,
            &Ok(vec![ViewFit::Skipped, ViewFit::Skipped]),
        );
        let summary = plan.finish().unwrap();
        assert_eq!(
            (summary.target.cols, summary.skipped, summary.remaining),
            (260, 2, 0)
        );
    }

    #[test]
    fn a_detach_or_read_error_keeps_the_last_known_remainder() {
        let mut lag = lag(&[0; 3]);
        let (mut plan, now) = reading(&mut lag);
        plan.record_fit(&lag.views[..1], now, now, &Ok(vec![ViewFit::Failed]));
        let summary = plan.finish();
        assert!(
            summary.is_some(),
            "losing the read must end and record the run"
        );
        assert_eq!(summary.unwrap().remaining, 2);
        assert!(!plan.ready(now + Duration::from_secs(10)));
    }
}
