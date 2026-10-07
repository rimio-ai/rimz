//! Deep renderer-local sidebar width controller.

use std::collections::VecDeque;
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::diag::record::{
    SidebarWidthControlTrigger, SidebarWidthIntentTrigger, SidebarWidthIntentVerdict,
    SidebarWidthSettleOutcome,
};
use crate::ids::{MuxName, PaneId};
use crate::mux::WidthAdjust;
use crate::mux::width::{sidebar_width_off_spec, width_step_regressed};
use crate::{RuntimePaths, diag::DiagSink};
use tracing::{debug, warn};

const FEEDBACK_TIMEOUT: Duration = Duration::from_secs(1);
const IDLE_RETRY: Duration = Duration::from_secs(5);
const KEY_SETTLE: Duration = Duration::from_millis(300);
const STRUCTURAL_GUARD_MS: u64 = 2_000;
const MAX_STEPS: u8 = 32;
// One cycle is an issued step plus its single no-progress retry.
const MAX_NO_PROGRESS_CYCLES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthIdleReason {
    ReachedTolerance,
    ReverseParked,
    NoProgress,
    Unacknowledged,
    StepBudget,
    FullscreenHeld,
}

#[derive(Clone, Copy, Debug)]
struct WidthIdle {
    at: u16,
    reason: WidthIdleReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthTransition {
    StepIssued { from: u16, target: u16 },
    FeedbackLearned { settled: u16, learned_step: u16 },
    LateFeedback { settled: u16 },
    Idle { at: u16, reason: WidthIdleReason },
}

#[derive(Clone, Copy, Debug)]
struct IssuedStep {
    width_before: u16,
    target: u16,
    at: Instant,
}

#[derive(Debug)]
struct WidthControl {
    target: Option<NonZeroU16>,
    steps_issued: u8,
    in_flight: Option<IssuedStep>,
    learned_step: Option<u16>,
    /// Backend-native step estimate that seeds nearest-width tolerance and survives retargeting.
    native_step: Option<crate::mux::WidthStep>,
    retried_no_progress: bool,
    no_progress_cycles: u8,
    unacknowledged: Option<IssuedStep>,
    reverse_max_distance: Option<u16>,
    idle: Option<WidthIdle>,
    traces: VecDeque<WidthTransition>,
}

impl WidthControl {
    fn new(target: Option<NonZeroU16>) -> Self {
        Self {
            target,
            steps_issued: 0,
            in_flight: None,
            learned_step: None,
            native_step: None,
            retried_no_progress: false,
            no_progress_cycles: 0,
            unacknowledged: None,
            reverse_max_distance: None,
            idle: None,
            traces: VecDeque::new(),
        }
    }

    fn retarget(&mut self, target: Option<NonZeroU16>) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.steps_issued = 0;
        self.learned_step = None;
        self.retried_no_progress = false;
        self.no_progress_cycles = 0;
        self.reverse_max_distance = None;
        if !self.is_fullscreen_held() {
            self.idle = None;
        }
        self.traces.clear();
    }

    fn seed_native_step(&mut self, step: crate::mux::WidthStep) {
        if step.stop_step_cols != 0 {
            self.native_step = Some(step);
        }
    }

    fn target(&self) -> Option<NonZeroU16> {
        self.target
    }

    fn is_idle(&self) -> bool {
        self.idle.is_some()
    }

    fn is_fullscreen_held(&self) -> bool {
        self.idle
            .is_some_and(|idle| idle.reason == WidthIdleReason::FullscreenHeld)
    }

    fn is_unacknowledged(&self) -> bool {
        self.idle
            .is_some_and(|idle| idle.reason == WidthIdleReason::Unacknowledged)
    }

    fn retries_when_idle(&self) -> bool {
        self.is_idle() && !self.is_fullscreen_held() && !self.is_unacknowledged()
    }

    fn settle_unacknowledged(&mut self, own_cols: u16) -> bool {
        let Some(step) = self.unacknowledged else {
            return false;
        };
        if own_cols == step.width_before {
            return false;
        }
        self.unacknowledged = None;
        if (own_cols > step.width_before) != (step.target > step.width_before) {
            return false;
        }
        self.no_progress_cycles = 0;
        self.traces
            .push_back(WidthTransition::LateFeedback { settled: own_cols });
        true
    }

    fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    fn rearm(&mut self) {
        if self.is_fullscreen_held() {
            return;
        }
        self.steps_issued = 0;
        self.in_flight = None;
        self.retried_no_progress = false;
        self.no_progress_cycles = 0;
        self.reverse_max_distance = None;
        self.idle = None;
    }

    fn retry_idle(&mut self) {
        if !self.retries_when_idle() {
            return;
        }
        let no_progress_cycles = self.no_progress_cycles;
        self.rearm();
        self.no_progress_cycles = no_progress_cycles;
    }

    fn observe_fullscreen(&mut self, active: bool, own_cols: u16) -> bool {
        if active {
            self.steps_issued = 0;
            self.in_flight = None;
            self.retried_no_progress = false;
            self.no_progress_cycles = 0;
            self.unacknowledged = None;
            self.reverse_max_distance = None;
            self.idle = Some(WidthIdle {
                at: own_cols,
                reason: WidthIdleReason::FullscreenHeld,
            });
            return false;
        }
        if self.is_fullscreen_held() {
            self.idle = None;
            return true;
        }
        false
    }

    fn stop_step(&self) -> u16 {
        if let Some(step) = self.native_step
            && step.exact
        {
            return step.stop_step_cols;
        }
        self.learned_step
            .max(self.native_step.map(|step| step.stop_step_cols))
            .unwrap_or(1)
    }

    fn needs_adjustment(&self, own_cols: u16) -> bool {
        if self.is_fullscreen_held() {
            return false;
        }
        self.target.is_some_and(|target| {
            sidebar_width_off_spec(
                u64::from(own_cols),
                u64::from(target.get()),
                u64::from(self.stop_step()),
            )
        })
    }

    fn feedback_deadline(&self) -> Option<Instant> {
        self.in_flight.map(|step| step.at + FEEDBACK_TIMEOUT)
    }

    fn take_trace(&mut self) -> Option<WidthTransition> {
        self.traces.pop_front()
    }

    /// Return one `(current, target)` actuator request, recording it as the
    /// sole in-flight step until a changed measurement or timeout arrives.
    fn decide(&mut self, own_cols: u16, now: Instant) -> Option<(u16, u16)> {
        if own_cols == 0 {
            return None;
        }
        let target_cols = self.target?.get();

        if let Some(idle) = self.idle {
            if idle.reason == WidthIdleReason::FullscreenHeld {
                return None;
            }
            if idle.at == own_cols {
                return None;
            }
            self.steps_issued = 0;
            self.in_flight = None;
            self.retried_no_progress = false;
            self.no_progress_cycles = 0;
            self.reverse_max_distance = None;
            self.idle = None;
        }

        if self.in_flight.is_none() {
            self.settle_unacknowledged(own_cols);
        }

        if let Some(step) = self.in_flight {
            if own_cols != step.width_before {
                if !self.native_step.is_some_and(|step| step.exact) {
                    let learned_step = own_cols.abs_diff(step.width_before);
                    self.learned_step = Some(learned_step);
                    self.traces.push_back(WidthTransition::FeedbackLearned {
                        settled: own_cols,
                        learned_step,
                    });
                }
                self.in_flight = None;
                self.retried_no_progress = false;
                self.no_progress_cycles = 0;
                self.unacknowledged = None;
                if let Some(max_distance) = self.reverse_max_distance {
                    if own_cols.abs_diff(target_cols) <= max_distance {
                        self.idle = Some(WidthIdle {
                            at: own_cols,
                            reason: WidthIdleReason::ReverseParked,
                        });
                        self.traces.push_back(WidthTransition::Idle {
                            at: own_cols,
                            reason: WidthIdleReason::ReverseParked,
                        });
                        return None;
                    }
                    self.reverse_max_distance = None;
                }
                if width_step_regressed(
                    u64::from(step.width_before),
                    u64::from(own_cols),
                    u64::from(target_cols),
                ) {
                    self.reverse_max_distance = Some(step.width_before.abs_diff(target_cols));
                }
            } else if now.saturating_duration_since(step.at) < FEEDBACK_TIMEOUT {
                return None;
            } else if self.retried_no_progress {
                self.in_flight = None;
                self.no_progress_cycles = self.no_progress_cycles.saturating_add(1);
                let reason = if self.no_progress_cycles >= MAX_NO_PROGRESS_CYCLES {
                    WidthIdleReason::Unacknowledged
                } else {
                    WidthIdleReason::NoProgress
                };
                self.idle = Some(WidthIdle {
                    at: own_cols,
                    reason,
                });
                self.traces.push_back(WidthTransition::Idle {
                    at: own_cols,
                    reason,
                });
                return None;
            } else {
                self.in_flight = None;
                self.retried_no_progress = true;
                self.unacknowledged = Some(step);
            }
        }

        if !self.needs_adjustment(own_cols) {
            self.idle = Some(WidthIdle {
                at: own_cols,
                reason: WidthIdleReason::ReachedTolerance,
            });
            self.traces.push_back(WidthTransition::Idle {
                at: own_cols,
                reason: WidthIdleReason::ReachedTolerance,
            });
            return None;
        }
        if self.steps_issued >= MAX_STEPS {
            self.idle = Some(WidthIdle {
                at: own_cols,
                reason: WidthIdleReason::StepBudget,
            });
            self.traces.push_back(WidthTransition::Idle {
                at: own_cols,
                reason: WidthIdleReason::StepBudget,
            });
            return None;
        }

        self.steps_issued += 1;
        self.in_flight = Some(IssuedStep {
            width_before: own_cols,
            target: target_cols,
            at: now,
        });
        self.traces.push_back(WidthTransition::StepIssued {
            from: own_cols,
            target: target_cols,
        });
        Some((own_cols, target_cols))
    }
}

#[derive(Debug)]
struct WidthKeyBurst {
    deadline: Instant,
    base_cols: u16,
    own_cols: u16,
    dir: WidthAdjust,
    target: Option<NonZeroU16>,
    verdict: SidebarWidthIntentVerdict,
}

#[derive(Debug)]
pub(super) struct WidthController {
    pub(super) geometry: Arc<Mutex<crate::mux::zellij::WidthMemo>>,
    runtime: RuntimePaths,
    session_name: String,
    own_pane: Option<PaneId>,
    mux: MuxName,
    width: crate::mux::SidebarWidth,
    convergence: WidthControl,
    started_at_ms: u64,
    current_view_cols: Option<u16>,
    last_siblings: Option<usize>,
    siblings_stable_since_ms: Option<u64>,
    structural_at_ms: Option<u64>,
    fullscreen_observed_at_ms: Option<u64>,
    idle_retry_deadline: Option<Instant>,
    baseline_probe_deadline: Option<Instant>,
    classification_deadline: Option<Instant>,
    classification_resize_at_ms: Option<u64>,
    key_burst: Option<WidthKeyBurst>,
}

impl WidthController {
    pub(super) fn new(
        runtime: RuntimePaths,
        session_name: String,
        own_pane: Option<PaneId>,
        mux: MuxName,
        width: crate::mux::SidebarWidth,
    ) -> Self {
        let baseline_probe_deadline = own_pane.as_ref().map(|_| Instant::now());
        Self {
            geometry: Arc::default(),
            runtime,
            session_name,
            own_pane,
            mux,
            width,
            convergence: WidthControl::new(None),
            started_at_ms: crate::utils::time::unix_now_ms(),
            current_view_cols: None,
            last_siblings: None,
            siblings_stable_since_ms: None,
            structural_at_ms: None,
            fullscreen_observed_at_ms: None,
            idle_retry_deadline: None,
            baseline_probe_deadline,
            classification_deadline: None,
            classification_resize_at_ms: None,
            key_burst: None,
        }
    }

    pub(super) fn feedback_deadline(&self) -> Option<Instant> {
        [
            self.convergence.feedback_deadline(),
            self.baseline_probe_deadline,
            self.classification_deadline,
            self.idle_retry_deadline,
            self.key_burst.as_ref().map(|burst| burst.deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn max_legit_cols(&self) -> u16 {
        self.convergence
            .target()
            .map_or(self.width.max_cols.get(), NonZeroU16::get)
    }

    pub(super) fn reload_target(
        &mut self,
        theme: &crate::config::ThemeConfig,
        measured_cols: Option<u16>,
        diag: &DiagSink,
    ) {
        self.width = crate::mux::SidebarWidth::from_config(theme);
        if self.refresh_target(None, false).is_some() {
            self.baseline_probe_deadline = None;
        } else if self.own_pane.is_some() {
            // A topology broadcast can arrive while the sidebar is the tab's
            // only materialized pane. Keep the proven target and retry once the
            // sibling has made the viewport measurable.
            self.baseline_probe_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
        }
        if let Some(cols) = measured_cols {
            self.observe(cols, SidebarWidthControlTrigger::Retarget, diag);
        }
    }

    pub(super) fn adjust(&mut self, own_cols: u16, dir: WidthAdjust) {
        let Some(pane) = self.own_pane.as_ref() else {
            return;
        };
        // A press must not fork (docs/internals/performance.md, keypress
        // budget). Zellij's probe is a stat-keyed memo read, so every burst
        // opens on the live view and re-resolves the room target; tmux's probe
        // is a subprocess, so it runs only while no view is stored.
        if self.key_burst.is_none() && !self.convergence.is_fullscreen_held() {
            if self.mux == MuxName::Zellij {
                if self.refresh_target(None, false).is_some() {
                    self.baseline_probe_deadline = None;
                }
            } else if self.current_view_cols.is_none()
                && let Ok(step) = self.width_step(pane, None)
                && step.view_cols != 0
            {
                self.current_view_cols = Some(step.view_cols);
                self.convergence.seed_native_step(step);
                self.baseline_probe_deadline = None;
            }
        }
        let burst = self.key_burst.get_or_insert(WidthKeyBurst {
            deadline: Instant::now() + KEY_SETTLE,
            base_cols: own_cols,
            own_cols,
            dir,
            target: None,
            verdict: SidebarWidthIntentVerdict::RejectedNoStep,
        });
        burst.deadline = Instant::now() + KEY_SETTLE;
        burst.own_cols = own_cols;
        self.classification_deadline = None;
        self.classification_resize_at_ms = None;
        if self.convergence.is_fullscreen_held() {
            burst.target = None;
            burst.verdict = SidebarWidthIntentVerdict::RejectedFullscreen;
            return;
        }
        let (Some(step), Some(view_cols)) = (self.convergence.native_step, self.current_view_cols)
        else {
            return;
        };
        let base_cols = if step.exact {
            self.convergence.target().map_or(own_cols, NonZeroU16::get)
        } else {
            own_cols
        };
        let target = crate::mux::width::adjust_target_cols(
            base_cols,
            dir,
            step,
            crate::mux::width::MIN_ADJUSTABLE_WIDTH,
            (u32::from(view_cols) * u32::from(self.width.max_percent.clamp(10, 90))).div_ceil(100)
                as u16,
        )
        .map(|target| match (step.exact, self.convergence.target()) {
            // A relative step from a pane still converging toward a broadcast
            // target must not move the target against the press.
            (false, Some(pending)) => match dir {
                WidthAdjust::Narrower => target.min(pending),
                WidthAdjust::Wider => target.max(pending),
            },
            _ => target,
        });
        if let Some(target) = target {
            burst.target = Some(target);
            burst.dir = dir;
            burst.verdict = SidebarWidthIntentVerdict::Accepted;
            self.convergence.retarget(Some(target));
        } else if burst.target.is_none() {
            burst.dir = dir;
            burst.verdict = match dir {
                WidthAdjust::Narrower => SidebarWidthIntentVerdict::RejectedFloor,
                WidthAdjust::Wider => SidebarWidthIntentVerdict::RejectedCeiling,
            };
        }
    }

    fn commit_key_burst(&mut self, diag: &DiagSink) {
        let _ = self.refresh_target(None, false);
        let Some(mut burst) = self.key_burst.take() else {
            return;
        };
        let step = self.convergence.native_step;
        if let Some(target) = burst.target
            && let Some(view_cols) = self.current_view_cols.and_then(NonZeroU16::new)
        {
            match crate::mux::width_target::pin(&self.runtime, self.width, target, view_cols.get())
            {
                Ok(permille) => {
                    let target = permille.cols(view_cols);
                    self.convergence.retarget(Some(target));
                    burst.target = Some(target);
                    spawn_width_default_record(self.mux, &self.session_name, target.get());
                }
                Err(err) => {
                    warn!(error = %err, "sidebar width target pin failed");
                    return;
                }
            }
        }
        diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
            trigger: match burst.dir {
                WidthAdjust::Narrower => SidebarWidthIntentTrigger::Narrower,
                WidthAdjust::Wider => SidebarWidthIntentTrigger::Wider,
            },
            own_cols: burst.own_cols,
            base_cols: burst.base_cols,
            view_cols: self.current_view_cols.unwrap_or(0),
            step_cols: step.map(|step| step.adjustment_cols(burst.dir)),
            step_exact: step.is_some_and(|step| step.exact),
            target_cols: burst.target.map(NonZeroU16::get),
            verdict: burst.verdict,
        });
    }

    pub(super) fn observe(
        &mut self,
        measured_cols: u16,
        trigger: SidebarWidthControlTrigger,
        diag: &DiagSink,
    ) {
        if self.own_pane.is_none() {
            return;
        }
        if trigger == SidebarWidthControlTrigger::ResizeFeedback
            && self.key_burst.is_none()
            && !self.convergence.in_flight()
            && !self.convergence.settle_unacknowledged(measured_cols)
        {
            if self.convergence.needs_adjustment(measured_cols) {
                self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                self.classification_resize_at_ms = Some(crate::utils::time::unix_now_ms());
            }
            return;
        }
        let nudge = self.convergence.decide(measured_cols, Instant::now());
        while let Some(transition) = self.convergence.take_trace() {
            match transition {
                WidthTransition::StepIssued { from, target } => {
                    diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthNudge {
                        trigger,
                        view_cols: self.current_view_cols.unwrap_or(0),
                        from_cols: from,
                        target_cols: target,
                    });
                }
                WidthTransition::FeedbackLearned {
                    settled,
                    learned_step,
                } => diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthSettle {
                    settled_cols: settled,
                    learned_step: Some(learned_step),
                    outcome: SidebarWidthSettleOutcome::FeedbackLearned,
                }),
                WidthTransition::LateFeedback { settled } => {
                    diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthSettle {
                        settled_cols: settled,
                        learned_step: None,
                        outcome: SidebarWidthSettleOutcome::LateFeedback,
                    });
                }
                WidthTransition::Idle { at, reason } => {
                    let outcome = match reason {
                        WidthIdleReason::ReachedTolerance => {
                            SidebarWidthSettleOutcome::ReachedTolerance
                        }
                        WidthIdleReason::ReverseParked => SidebarWidthSettleOutcome::ReverseParked,
                        WidthIdleReason::NoProgress => SidebarWidthSettleOutcome::NoProgress,
                        WidthIdleReason::Unacknowledged => {
                            SidebarWidthSettleOutcome::Unacknowledged
                        }
                        WidthIdleReason::StepBudget => SidebarWidthSettleOutcome::StepBudget,
                        WidthIdleReason::FullscreenHeld => continue,
                    };
                    diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthSettle {
                        settled_cols: at,
                        learned_step: None,
                        outcome,
                    });
                }
            }
        }
        if let (Some(pane), Some((current, target))) = (self.own_pane.clone(), nudge) {
            spawn_width_nudge(pane, &self.session_name, current, target);
        }
    }

    pub(super) fn note_structural(
        &mut self,
        at_ms: u64,
        measured_cols: Option<u16>,
        diag: &DiagSink,
    ) -> bool {
        self.structural_at_ms = Some(
            self.structural_at_ms
                .map_or(at_ms, |previous| previous.max(at_ms)),
        );
        if self.refresh_target(Some(at_ms), true).is_none() {
            if self.own_pane.is_some() {
                self.baseline_probe_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            }
            return false;
        }
        self.baseline_probe_deadline = None;
        if let Some(cols) = measured_cols
            && self.convergence.needs_adjustment(cols)
        {
            self.convergence.rearm();
            self.observe(cols, SidebarWidthControlTrigger::Structural, diag);
        }
        true
    }

    pub(super) fn backstop(
        &mut self,
        measured_cols: Option<u16>,
        sibling_count: Option<usize>,
        panes_observed_at_ms: Option<u64>,
        diag: &DiagSink,
    ) {
        self.backstop_at(
            measured_cols,
            sibling_count,
            panes_observed_at_ms,
            diag,
            Instant::now(),
        );
    }

    fn backstop_at(
        &mut self,
        measured_cols: Option<u16>,
        sibling_count: Option<usize>,
        panes_observed_at_ms: Option<u64>,
        diag: &DiagSink,
        now: Instant,
    ) {
        if self.key_burst.is_some() {
            if self
                .key_burst
                .as_ref()
                .is_some_and(|burst| now >= burst.deadline)
            {
                self.commit_key_burst(diag);
            }
            if let Some(cols) = measured_cols {
                self.observe(cols, SidebarWidthControlTrigger::Retarget, diag);
            }
            self.idle_retry_deadline = None;
        }
        if let (Some(cols), Some(observed_at_ms)) = (measured_cols, panes_observed_at_ms)
            && self.sync_fullscreen_hold(cols, observed_at_ms)
        {
            self.observe(cols, SidebarWidthControlTrigger::Backstop, diag);
        }
        if let Some(siblings) = sibling_count {
            let previous = self.last_siblings.replace(siblings);
            if previous != Some(siblings) || self.siblings_stable_since_ms.is_none() {
                self.siblings_stable_since_ms =
                    panes_observed_at_ms.map(|at_ms| at_ms.max(crate::utils::time::unix_now_ms()));
            }
            if previous.is_some_and(|previous| previous != siblings)
                && !self.note_structural(
                    panes_observed_at_ms.unwrap_or_else(crate::utils::time::unix_now_ms),
                    measured_cols,
                    diag,
                )
            {
                return;
            }
        }
        if self
            .baseline_probe_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.baseline_probe_deadline = Some(now + FEEDBACK_TIMEOUT);
            if let Some(cols) = measured_cols
                && self.capture_classification_baseline(cols, diag)
            {
                self.baseline_probe_deadline = None;
            }
        }
        if self
            .convergence
            .feedback_deadline()
            .is_some_and(|deadline| now >= deadline)
            && let Some(cols) = measured_cols
        {
            self.observe(cols, SidebarWidthControlTrigger::Backstop, diag);
        }
        if self
            .classification_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            match (measured_cols, sibling_count) {
                (Some(cols), Some(_)) => {
                    self.classify_settled_resize(cols, panes_observed_at_ms, diag);
                }
                (Some(_), None) => {
                    // A sibling count proves this observation located our own
                    // view; do not adopt from a session frame that did not.
                    self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                }
                (None, _) => {
                    self.classification_deadline = None;
                    self.classification_resize_at_ms = None;
                }
            }
        }
        if let Some(cols) = measured_cols {
            if self.key_burst.is_some()
                || self.classification_deadline.is_some()
                || self.convergence.is_fullscreen_held()
            {
                self.idle_retry_deadline = None;
            } else if self.convergence.retries_when_idle() {
                let deadline = self.idle_retry_deadline.get_or_insert(now + IDLE_RETRY);
                if now >= *deadline {
                    let _ = self.refresh_target(None, true);
                    if self.convergence.needs_adjustment(cols) {
                        self.convergence.retry_idle();
                        self.observe(cols, SidebarWidthControlTrigger::IdleRetry, diag);
                    }
                    self.idle_retry_deadline = Some(now + IDLE_RETRY);
                }
            } else {
                self.idle_retry_deadline = None;
            }
        }
    }

    fn sync_fullscreen_hold(&mut self, measured_cols: u16, observed_at_ms: u64) -> bool {
        // tmux width probes spawn a subprocess and currently expose no zoom
        // observation. Revisit this guard if WidthStep gains that signal there.
        if self.mux != MuxName::Zellij
            || self
                .fullscreen_observed_at_ms
                .is_some_and(|current| observed_at_ms <= current)
        {
            return false;
        }
        let Some(pane) = self.own_pane.as_ref() else {
            return false;
        };
        let Ok(step) = self.width_step(pane, None) else {
            return false;
        };
        let Some(active) = step.fullscreen_active else {
            return false;
        };
        self.fullscreen_observed_at_ms = Some(observed_at_ms);
        self.convergence.observe_fullscreen(active, measured_cols)
    }

    fn capture_classification_baseline(&mut self, measured_cols: u16, diag: &DiagSink) -> bool {
        let floor = self
            .structural_at_ms
            .map_or(self.started_at_ms, |at_ms| at_ms.max(self.started_at_ms));
        if self.refresh_target(Some(floor), true).is_some() {
            self.observe(measured_cols, SidebarWidthControlTrigger::Backstop, diag);
            return true;
        }
        false
    }

    fn classify_settled_resize(
        &mut self,
        measured_cols: u16,
        panes_observed_at_ms: Option<u64>,
        diag: &DiagSink,
    ) {
        if !self.convergence.needs_adjustment(measured_cols) {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            return;
        }
        if self.own_pane.is_none() {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            return;
        }
        let previous_view_cols = self.current_view_cols;
        let (step, view_cols) = match self.refresh_target(None, true) {
            Some(proven) => proven,
            None => {
                debug!("sidebar settled resize lacks backend geometry");
                self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                return;
            }
        };
        let Some(resize_at_ms) = self.classification_resize_at_ms else {
            self.classification_deadline = None;
            return;
        };
        let view_changed = previous_view_cols != Some(view_cols.get());
        let structurally_changed = self.structural_at_ms.is_some_and(|structural_at_ms| {
            structural_at_ms >= resize_at_ms.saturating_sub(STRUCTURAL_GUARD_MS)
        });
        if !view_changed
            && !structurally_changed
            && !panes_observed_at_ms.is_some_and(|observed_at_ms| {
                observed_at_ms >= resize_at_ms.saturating_add(STRUCTURAL_GUARD_MS)
            })
        {
            self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            return;
        }
        self.classification_deadline = None;
        self.classification_resize_at_ms = None;
        let base_cols = self
            .convergence
            .target()
            .map_or(measured_cols, NonZeroU16::get);
        let proven_siblings = self
            .siblings_stable_since_ms
            .is_some_and(|at_ms| at_ms < resize_at_ms);
        if view_changed || structurally_changed || !proven_siblings {
            if !proven_siblings {
                diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                    trigger: SidebarWidthIntentTrigger::MouseAdopt,
                    own_cols: measured_cols,
                    base_cols,
                    view_cols: view_cols.get(),
                    step_cols: Some(step.cols),
                    step_exact: step.exact,
                    target_cols: Some(base_cols),
                    verdict: SidebarWidthIntentVerdict::RejectedUnproven,
                });
            }
            self.convergence.rearm();
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            return;
        }
        let Some(measured) = NonZeroU16::new(measured_cols) else {
            return;
        };
        let permille = match crate::mux::width_target::pin(
            &self.runtime,
            self.width,
            measured,
            view_cols.get(),
        ) {
            Ok(permille) => permille,
            Err(err) => {
                warn!(error = %err, "sidebar mouse width target pin failed");
                return;
            }
        };
        let target = permille.cols(view_cols);
        diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
            trigger: SidebarWidthIntentTrigger::MouseAdopt,
            own_cols: measured_cols,
            base_cols,
            view_cols: view_cols.get(),
            step_cols: Some(step.cols),
            step_exact: step.exact,
            target_cols: Some(target.get()),
            verdict: SidebarWidthIntentVerdict::Accepted,
        });
        spawn_width_default_record(self.mux, &self.session_name, target.get());
        self.convergence.retarget(Some(target));
        self.observe(
            measured_cols,
            SidebarWidthControlTrigger::Classification,
            diag,
        );
    }

    fn width_step(
        &self,
        pane: &PaneId,
        floor: Option<u64>,
    ) -> crate::mux::Result<crate::mux::WidthStep> {
        if self.mux != MuxName::Zellij {
            return crate::mux::backend_for(self.mux).sidebar_width_step(
                &self.runtime,
                &self.session_name,
                pane,
                floor,
            );
        }
        self.geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .step(
                &self.runtime,
                &self.session_name,
                pane,
                crate::utils::time::unix_now_ms(),
                floor,
            )
    }

    /// Re-derive the target from a proven viewport. `floor` is the event this
    /// decision reacts to: an older topology observation cannot describe the
    /// geometry that event produced. A failed proof leaves the target untouched.
    fn refresh_target(
        &mut self,
        floor: Option<u64>,
        remember_default: bool,
    ) -> Option<(crate::mux::WidthStep, NonZeroU16)> {
        let pane = self.own_pane.as_ref()?;
        let step = self.width_step(pane, floor).ok()?;
        let view_cols = NonZeroU16::new(step.view_cols)?;
        self.convergence.seed_native_step(step);
        self.current_view_cols = Some(view_cols.get());
        if self.key_burst.is_some() {
            return Some((step, view_cols));
        }
        let target = match floor {
            Some(_) => crate::mux::width_target::adopt(&self.runtime, self.width, view_cols),
            None => {
                crate::mux::width_target::resolve(&self.runtime, self.width, Some(view_cols.get()))
            }
        }
        .cols(Some(view_cols.get()));
        let changed = self.convergence.target() != Some(target);
        self.convergence.retarget(Some(target));
        if changed && remember_default {
            spawn_width_default_record(self.mux, &self.session_name, target.get());
        }
        Some((step, view_cols))
    }
}

fn spawn_width_nudge(pane_id: PaneId, session_name: &str, current_cols: u16, target_cols: u16) {
    let session_name = session_name.to_owned();
    std::thread::spawn(move || {
        if let Err(err) = crate::mux::backend_for(pane_id.mux()).nudge_sidebar_width(
            &session_name,
            &pane_id,
            current_cols,
            target_cols,
        ) {
            debug!(pane = %pane_id, error = %err, "sidebar width nudge failed");
        }
    });
}

fn spawn_width_default_record(mux: MuxName, session_name: &str, cols: u16) {
    if mux == MuxName::Zellij {
        return;
    }
    let session_name = session_name.to_owned();
    std::thread::spawn(move || {
        if let Err(err) =
            crate::mux::backend_for(mux).record_sidebar_width_default(&session_name, cols)
        {
            debug!(error = %err, "sidebar width default record failed");
        }
    });
}

#[cfg(test)]
mod tests;
