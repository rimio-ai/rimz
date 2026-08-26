//! Deep renderer-local sidebar width controller.

use std::collections::VecDeque;
use std::num::NonZeroU16;
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
const STRUCTURAL_GUARD_MS: u64 = 2_000;
const MAX_STEPS: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthIdleReason {
    ReachedTolerance,
    ReverseParked,
    NoProgress,
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
    Idle { at: u16, reason: WidthIdleReason },
}

#[derive(Clone, Copy, Debug)]
struct IssuedStep {
    width_before: u16,
    at: Instant,
}

#[derive(Debug)]
struct WidthControl {
    target: Option<NonZeroU16>,
    steps_issued: u8,
    in_flight: Option<IssuedStep>,
    learned_step: Option<u16>,
    /// Backend-native step estimate that seeds nearest-width tolerance and survives retargeting.
    native_step: Option<NonZeroU16>,
    retried_no_progress: bool,
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
        self.reverse_max_distance = None;
        if !self.is_fullscreen_held() {
            self.idle = None;
        }
        self.traces.clear();
    }

    fn seed_native_step(&mut self, step_cols: u16) {
        self.native_step = NonZeroU16::new(step_cols).or(self.native_step);
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
        self.reverse_max_distance = None;
        self.idle = None;
    }

    fn observe_fullscreen(&mut self, active: bool, own_cols: u16) -> bool {
        if active {
            self.steps_issued = 0;
            self.in_flight = None;
            self.retried_no_progress = false;
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
        self.learned_step
            .max(self.native_step.map(NonZeroU16::get))
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
            self.reverse_max_distance = None;
            self.idle = None;
        }

        if let Some(step) = self.in_flight {
            if own_cols != step.width_before {
                let learned_step = own_cols.abs_diff(step.width_before);
                self.learned_step = Some(learned_step);
                self.traces.push_back(WidthTransition::FeedbackLearned {
                    settled: own_cols,
                    learned_step,
                });
                self.in_flight = None;
                self.retried_no_progress = false;
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
                self.idle = Some(WidthIdle {
                    at: own_cols,
                    reason: WidthIdleReason::NoProgress,
                });
                self.traces.push_back(WidthTransition::Idle {
                    at: own_cols,
                    reason: WidthIdleReason::NoProgress,
                });
                return None;
            } else {
                self.in_flight = None;
                self.retried_no_progress = true;
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
pub(super) struct WidthController {
    runtime: RuntimePaths,
    session_name: String,
    own_pane: Option<PaneId>,
    mux: MuxName,
    width: crate::mux::SidebarWidth,
    convergence: WidthControl,
    started_at_ms: u64,
    current_view_cols: Option<u16>,
    last_siblings: Option<usize>,
    structural_at_ms: Option<u64>,
    fullscreen_observed_at_ms: Option<u64>,
    idle_retry_deadline: Option<Instant>,
    baseline_probe_deadline: Option<Instant>,
    classification_deadline: Option<Instant>,
    classification_resize_at_ms: Option<u64>,
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
            runtime,
            session_name,
            own_pane,
            mux,
            width,
            convergence: WidthControl::new(None),
            started_at_ms: crate::sidebar::timing::unix_now_ms(),
            current_view_cols: None,
            last_siblings: None,
            structural_at_ms: None,
            fullscreen_observed_at_ms: None,
            idle_retry_deadline: None,
            baseline_probe_deadline,
            classification_deadline: None,
            classification_resize_at_ms: None,
        }
    }

    pub(super) fn feedback_deadline(&self) -> Option<Instant> {
        [
            self.convergence.feedback_deadline(),
            self.baseline_probe_deadline,
            self.classification_deadline,
            self.idle_retry_deadline,
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
        if self.refresh_target(None).is_some() {
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

    pub(super) fn adjust(&mut self, own_cols: u16, dir: WidthAdjust, diag: &DiagSink) {
        let Some(pane) = self.own_pane.as_ref() else {
            return;
        };
        let pending_cols = self.convergence.target().map(NonZeroU16::get);
        let base_cols = match dir {
            WidthAdjust::Narrower => pending_cols.map_or(own_cols, |target| target.min(own_cols)),
            WidthAdjust::Wider => pending_cols.map_or(own_cols, |target| target.max(own_cols)),
        };
        let trigger = match dir {
            WidthAdjust::Narrower => SidebarWidthIntentTrigger::Narrower,
            WidthAdjust::Wider => SidebarWidthIntentTrigger::Wider,
        };
        let step = match crate::mux::backend_for(self.mux).sidebar_width_step(
            &self.runtime,
            &self.session_name,
            pane,
            None,
        ) {
            Ok(step) => step,
            Err(err) => {
                diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                    trigger,
                    own_cols,
                    base_cols,
                    view_cols: 0,
                    step_cols: None,
                    step_exact: false,
                    target_cols: None,
                    verdict: SidebarWidthIntentVerdict::RejectedNoStep,
                });
                debug!(pane = %pane, error = %err, "sidebar width intent dropped without backend step");
                return;
            }
        };
        let adjustment_cols = step.adjustment_cols(dir);
        self.convergence.seed_native_step(step.stop_step_cols);
        let Some(view_cols) = NonZeroU16::new(step.view_cols) else {
            diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                trigger,
                own_cols,
                base_cols,
                view_cols: step.view_cols,
                step_cols: Some(adjustment_cols),
                step_exact: step.exact,
                target_cols: None,
                verdict: SidebarWidthIntentVerdict::RejectedNoStep,
            });
            debug!(pane = %pane, "sidebar width intent dropped without backend geometry");
            return;
        };
        self.current_view_cols = Some(view_cols.get());
        let Some(target) = crate::mux::width::adjust_target_cols(
            base_cols,
            dir,
            step,
            crate::mux::width::MIN_ADJUSTABLE_WIDTH,
        ) else {
            diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                trigger,
                own_cols,
                base_cols,
                view_cols: view_cols.get(),
                step_cols: Some(adjustment_cols),
                step_exact: step.exact,
                target_cols: None,
                verdict: SidebarWidthIntentVerdict::RejectedFloor,
            });
            debug!(pane = %pane, base_cols, step_cols = adjustment_cols, "sidebar width intent rejected at minimum width");
            return;
        };
        diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
            trigger,
            own_cols,
            base_cols,
            view_cols: view_cols.get(),
            step_cols: Some(adjustment_cols),
            step_exact: step.exact,
            target_cols: Some(target.get()),
            verdict: SidebarWidthIntentVerdict::Accepted,
        });
        let target = match crate::sidebar::width_target::pin(&self.runtime, target, view_cols.get())
        {
            Ok(permille) => permille.cols(view_cols),
            Err(err) => {
                warn!(error = %err, "sidebar width target pin failed");
                return;
            }
        };
        spawn_width_default_record(self.mux, &self.session_name, target.get());
        self.convergence.retarget(Some(target));
        self.observe(own_cols, SidebarWidthControlTrigger::Retarget, diag);
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
        if trigger == SidebarWidthControlTrigger::ResizeFeedback && !self.convergence.in_flight() {
            if self.convergence.needs_adjustment(measured_cols) {
                self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                self.classification_resize_at_ms = Some(crate::sidebar::timing::unix_now_ms());
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
                WidthTransition::Idle { at, reason } => {
                    let outcome = match reason {
                        WidthIdleReason::ReachedTolerance => {
                            SidebarWidthSettleOutcome::ReachedTolerance
                        }
                        WidthIdleReason::ReverseParked => SidebarWidthSettleOutcome::ReverseParked,
                        WidthIdleReason::NoProgress => SidebarWidthSettleOutcome::NoProgress,
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
        if self.refresh_target(Some(at_ms)).is_none() {
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
        let now = Instant::now();
        if let (Some(cols), Some(observed_at_ms)) = (measured_cols, panes_observed_at_ms) {
            if self.sync_fullscreen_hold(cols, observed_at_ms) {
                self.observe(cols, SidebarWidthControlTrigger::Backstop, diag);
            }
        }
        if let Some(siblings) = sibling_count {
            let previous = self.last_siblings.replace(siblings);
            if previous.is_some_and(|previous| previous != siblings)
                && !self.note_structural(
                    panes_observed_at_ms.unwrap_or_else(crate::sidebar::timing::unix_now_ms),
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
            if self.classification_deadline.is_some() {
                self.idle_retry_deadline = None;
            } else if self.convergence.is_fullscreen_held() {
                self.idle_retry_deadline = None;
            } else if self.convergence.is_idle() {
                let deadline = self.idle_retry_deadline.get_or_insert(now + IDLE_RETRY);
                if now >= *deadline {
                    let _ = self.refresh_target(None);
                    if self.convergence.needs_adjustment(cols) {
                        self.convergence.rearm();
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
        let Ok(step) = crate::mux::backend_for(self.mux).sidebar_width_step(
            &self.runtime,
            &self.session_name,
            pane,
            None,
        ) else {
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
        if self.refresh_target(Some(floor)).is_some() {
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
        let (step, view_cols) = match self.refresh_target(None) {
            Some(proven) => proven,
            None => {
                debug!("sidebar settled resize lacks backend geometry");
                self.observe(
                    measured_cols,
                    SidebarWidthControlTrigger::Classification,
                    diag,
                );
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
        if view_changed {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            return;
        }
        if structurally_changed {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            self.convergence.rearm();
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            return;
        }
        if !panes_observed_at_ms.is_some_and(|observed_at_ms| {
            observed_at_ms >= resize_at_ms.saturating_add(STRUCTURAL_GUARD_MS)
        }) {
            self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            return;
        }
        self.classification_deadline = None;
        self.classification_resize_at_ms = None;

        let Some(measured) = NonZeroU16::new(measured_cols) else {
            return;
        };
        let base_cols = self
            .convergence
            .target()
            .map_or(measured_cols, NonZeroU16::get);
        let permille =
            match crate::sidebar::width_target::pin(&self.runtime, measured, view_cols.get()) {
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

    /// Re-derive the target from a proven viewport. `floor` is the event this
    /// decision reacts to: an older topology observation cannot describe the
    /// geometry that event produced. A failed proof leaves the target untouched.
    fn refresh_target(
        &mut self,
        floor: Option<u64>,
    ) -> Option<(crate::mux::WidthStep, NonZeroU16)> {
        let pane = self.own_pane.as_ref()?;
        let step = crate::mux::backend_for(self.mux)
            .sidebar_width_step(&self.runtime, &self.session_name, pane, floor)
            .ok()?;
        let view_cols = NonZeroU16::new(step.view_cols)?;
        self.convergence.seed_native_step(step.stop_step_cols);
        self.current_view_cols = Some(view_cols.get());
        let target = match floor {
            Some(_) => crate::sidebar::width_target::adopt(&self.runtime, self.width, view_cols),
            None => crate::sidebar::width_target::resolve(
                &self.runtime,
                self.width,
                Some(view_cols.get()),
            ),
        }
        .cols(Some(view_cols.get()));
        let changed = self.convergence.target() != Some(target);
        self.convergence.retarget(Some(target));
        if changed {
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
