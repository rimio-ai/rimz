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
use crate::{RuntimePaths, diag::DiagSink};
use tracing::{debug, warn};

const FEEDBACK_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_STEPS: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Narrower,
    Wider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthIdleReason {
    ReachedTolerance,
    CrossedNearest,
    ReverseParked,
    NoProgress,
    StepBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthTransition {
    StepIssued { from: u16, target: u16 },
    FeedbackLearned { settled: u16, learned_step: u16 },
    Idle { at: u16, reason: WidthIdleReason },
}

#[derive(Clone, Copy, Debug)]
struct IssuedStep {
    direction: Direction,
    width_before: u16,
    at: Instant,
}

#[derive(Debug)]
struct WidthControl {
    target: Option<NonZeroU16>,
    steps_issued: u8,
    in_flight: Option<IssuedStep>,
    learned_step: Option<u16>,
    /// Backend-native step estimate that seeds the stop band and survives retargeting.
    native_step: Option<NonZeroU16>,
    retried_no_progress: bool,
    reverse_issued: bool,
    idle_at: Option<u16>,
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
            reverse_issued: false,
            idle_at: None,
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
        self.reverse_issued = false;
        self.idle_at = None;
        self.traces.clear();
    }

    fn seed_native_step(&mut self, step_cols: u16) {
        self.native_step = NonZeroU16::new(step_cols).or(self.native_step);
    }

    fn target(&self) -> Option<NonZeroU16> {
        self.target
    }

    fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    fn tolerance(&self) -> u16 {
        self.learned_step
            .or(self.native_step.map(NonZeroU16::get))
            .map_or(1, |step| (step / 2).max(1))
    }

    fn needs_adjustment(&self, own_cols: u16) -> bool {
        self.target
            .is_some_and(|target| own_cols.abs_diff(target.get()) > self.tolerance())
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

        if let Some(idle_at) = self.idle_at {
            if idle_at == own_cols {
                return None;
            }
            self.steps_issued = 0;
            self.in_flight = None;
            self.retried_no_progress = false;
            self.reverse_issued = false;
            self.idle_at = None;
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
                if self.reverse_issued {
                    self.idle_at = Some(own_cols);
                    self.traces.push_back(WidthTransition::Idle {
                        at: own_cols,
                        reason: WidthIdleReason::ReverseParked,
                    });
                    return None;
                }
                if crossed_target(step, own_cols, target_cols) {
                    if own_cols.abs_diff(target_cols) <= step.width_before.abs_diff(target_cols) {
                        self.idle_at = Some(own_cols);
                        self.traces.push_back(WidthTransition::Idle {
                            at: own_cols,
                            reason: WidthIdleReason::CrossedNearest,
                        });
                        return None;
                    }
                    self.reverse_issued = true;
                }
            } else if now.saturating_duration_since(step.at) < FEEDBACK_TIMEOUT {
                return None;
            } else if self.retried_no_progress {
                self.in_flight = None;
                self.idle_at = Some(own_cols);
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
            self.idle_at = Some(own_cols);
            self.traces.push_back(WidthTransition::Idle {
                at: own_cols,
                reason: WidthIdleReason::ReachedTolerance,
            });
            return None;
        }
        if self.steps_issued >= MAX_STEPS {
            self.idle_at = Some(own_cols);
            self.traces.push_back(WidthTransition::Idle {
                at: own_cols,
                reason: WidthIdleReason::StepBudget,
            });
            return None;
        }

        let direction = if own_cols < target_cols {
            Direction::Wider
        } else {
            Direction::Narrower
        };
        self.steps_issued += 1;
        self.in_flight = Some(IssuedStep {
            direction,
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

fn crossed_target(step: IssuedStep, own_cols: u16, target_cols: u16) -> bool {
    match step.direction {
        Direction::Narrower => step.width_before > target_cols && own_cols < target_cols,
        Direction::Wider => step.width_before < target_cols && own_cols > target_cols,
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
    last_classified: Option<(u16, usize)>,
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
            last_classified: None,
            baseline_probe_deadline,
            classification_deadline: None,
            classification_resize_at_ms: None,
        }
    }

    pub(super) fn feedback_deadline(&self) -> Option<Instant> {
        [
            self.convergence.feedback_deadline(),
            self.last_classified
                .is_none()
                .then_some(self.baseline_probe_deadline)
                .flatten(),
            self.classification_deadline,
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
        let target = self.last_classified.map(|(view_cols, _)| {
            crate::sidebar::width_target::resolve(
                &self.runtime,
                self.width,
                self.mux,
                Some(view_cols),
            )
            .cols(Some(view_cols))
        });
        self.convergence.retarget(target);
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
        ) {
            Ok(step) => step,
            Err(err) => {
                diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                    trigger,
                    own_cols,
                    base_cols,
                    step_cols: None,
                    step_exact: false,
                    target_cols: None,
                    verdict: SidebarWidthIntentVerdict::RejectedNoStep,
                });
                debug!(pane = %pane, error = %err, "sidebar width intent dropped without backend step");
                return;
            }
        };
        self.convergence.seed_native_step(step.cols);
        let Some(view_cols) = NonZeroU16::new(step.view_cols) else {
            diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
                trigger,
                own_cols,
                base_cols,
                step_cols: Some(step.cols),
                step_exact: step.exact,
                target_cols: None,
                verdict: SidebarWidthIntentVerdict::RejectedNoStep,
            });
            debug!(pane = %pane, "sidebar width intent dropped without backend geometry");
            return;
        };
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
                step_cols: Some(step.cols),
                step_exact: step.exact,
                target_cols: None,
                verdict: SidebarWidthIntentVerdict::RejectedFloor,
            });
            debug!(pane = %pane, base_cols, step_cols = step.cols, "sidebar width intent rejected at minimum width");
            return;
        };
        diag.emit_unlimited(crate::diag::record::DiagEvent::SidebarWidthIntent {
            trigger,
            own_cols,
            base_cols,
            step_cols: Some(step.cols),
            step_exact: step.exact,
            target_cols: Some(target.get()),
            verdict: SidebarWidthIntentVerdict::Accepted,
        });
        let target = match crate::sidebar::width_target::pin(
            &self.runtime,
            target,
            self.mux,
            view_cols.get(),
        ) {
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
                        WidthIdleReason::CrossedNearest => {
                            SidebarWidthSettleOutcome::CrossedNearest
                        }
                        WidthIdleReason::ReverseParked => SidebarWidthSettleOutcome::ReverseParked,
                        WidthIdleReason::NoProgress => SidebarWidthSettleOutcome::NoProgress,
                        WidthIdleReason::StepBudget => SidebarWidthSettleOutcome::StepBudget,
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

    pub(super) fn backstop(
        &mut self,
        measured_cols: Option<u16>,
        sibling_count: Option<usize>,
        panes_observed_at_ms: Option<u64>,
        diag: &DiagSink,
    ) {
        if self.last_classified.is_none()
            && self
                .baseline_probe_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.baseline_probe_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            if let (Some(cols), Some(siblings)) = (measured_cols, sibling_count)
                && self.capture_classification_baseline(cols, siblings, diag)
            {
                self.baseline_probe_deadline = None;
            }
        }
        if self
            .convergence
            .feedback_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
            && let Some(cols) = measured_cols
        {
            self.observe(cols, SidebarWidthControlTrigger::Backstop, diag);
        }
        if self
            .classification_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            match (measured_cols, sibling_count) {
                (Some(cols), Some(siblings)) => {
                    self.classify_settled_resize(cols, siblings, panes_observed_at_ms, diag);
                }
                (Some(_), None) => {
                    self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                }
                (None, _) => {
                    self.classification_deadline = None;
                    self.classification_resize_at_ms = None;
                }
            }
        }
    }

    fn capture_classification_baseline(
        &mut self,
        measured_cols: u16,
        sibling_count: usize,
        diag: &DiagSink,
    ) -> bool {
        let Some(pane) = self.own_pane.as_ref() else {
            return false;
        };
        if let Ok(step) = crate::mux::backend_for(self.mux).sidebar_width_step(
            &self.runtime,
            &self.session_name,
            pane,
        ) && step.view_cols > 0
        {
            self.convergence.seed_native_step(step.cols);
            self.last_classified = Some((step.view_cols, sibling_count));
            let target = crate::sidebar::width_target::resolve(
                &self.runtime,
                self.width,
                self.mux,
                Some(step.view_cols),
            );
            self.convergence
                .retarget(Some(target.cols(Some(step.view_cols))));
            self.observe(measured_cols, SidebarWidthControlTrigger::Backstop, diag);
            return true;
        }
        false
    }

    fn classify_settled_resize(
        &mut self,
        measured_cols: u16,
        sibling_count: usize,
        panes_observed_at_ms: Option<u64>,
        diag: &DiagSink,
    ) {
        if !self.convergence.needs_adjustment(measured_cols) {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            return;
        }
        let Some(pane) = self.own_pane.as_ref() else {
            self.classification_deadline = None;
            self.classification_resize_at_ms = None;
            return;
        };
        let step = match crate::mux::backend_for(self.mux).sidebar_width_step(
            &self.runtime,
            &self.session_name,
            pane,
        ) {
            Ok(step) => step,
            Err(err) => {
                debug!(pane = %pane, error = %err, "sidebar settled resize lacks backend geometry");
                self.observe(
                    measured_cols,
                    SidebarWidthControlTrigger::Classification,
                    diag,
                );
                self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
                return;
            }
        };
        self.convergence.seed_native_step(step.cols);
        let Some(view_cols) = NonZeroU16::new(step.view_cols) else {
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            return;
        };
        if !self
            .classification_resize_at_ms
            .zip(panes_observed_at_ms)
            .is_some_and(|(resize_at_ms, observed_at_ms)| observed_at_ms > resize_at_ms)
        {
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            self.classification_deadline = Some(Instant::now() + FEEDBACK_TIMEOUT);
            return;
        }
        self.classification_deadline = None;
        self.classification_resize_at_ms = None;

        let previous = self
            .last_classified
            .replace((view_cols.get(), sibling_count));
        let view_changed =
            previous.is_none_or(|(previous_cols, _)| previous_cols != view_cols.get());
        let siblings_changed = previous.is_some_and(|(_, siblings)| siblings != sibling_count);
        if view_changed {
            let target = crate::sidebar::width_target::resolve(
                &self.runtime,
                self.width,
                self.mux,
                Some(view_cols.get()),
            );
            let target_cols = target.cols(Some(view_cols.get()));
            spawn_width_default_record(self.mux, &self.session_name, target_cols.get());
            self.convergence.retarget(Some(target_cols));
            self.observe(
                measured_cols,
                SidebarWidthControlTrigger::Classification,
                diag,
            );
            return;
        }
        if siblings_changed {
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
        let base_cols = self
            .convergence
            .target()
            .map_or(measured_cols, NonZeroU16::get);
        let permille = match crate::sidebar::width_target::pin(
            &self.runtime,
            measured,
            self.mux,
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
mod tests {
    use super::*;
    use crate::ids::{SidebarInstanceId, WorkspaceId};
    use crate::sidebar::events::{SidebarEvent, SidebarEventEnvelope};
    use std::os::unix::net::UnixDatagram;

    fn target(cols: u16) -> NonZeroU16 {
        NonZeroU16::new(cols).expect("nonzero target")
    }

    fn controller(mux: MuxName) -> (tempfile::TempDir, RuntimePaths, WidthController) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        let pane = match mux {
            MuxName::Tmux => PaneId::from_parts(mux, "%1"),
            MuxName::Zellij => PaneId::from_parts(mux, "terminal_1"),
        };
        let controller = WidthController::new(
            runtime.clone(),
            "rimz-test".to_owned(),
            Some(pane),
            mux,
            crate::mux::SidebarWidth::default(),
        );
        (dir, runtime, controller)
    }

    fn write_zellij_topology(runtime: &RuntimePaths) {
        write_zellij_topology_for_view(runtime, 200);
    }

    fn write_zellij_topology_for_view(runtime: &RuntimePaths, view_cols: u16) {
        use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};

        let pane = |id, pane_x, pane_columns, title: &str| PaneTopologyPane {
            id,
            is_plugin: false,
            is_held: false,
            exited: false,
            is_suppressed: false,
            is_floating: false,
            tab_position: 0,
            tab_name: None,
            pane_columns: Some(pane_columns),
            pane_x: Some(pane_x),
            title: Some(title.to_owned()),
            pane_command: None,
            pane_cwd: None,
            pane_pid: None,
            terminal_command: None,
        };
        crate::sidebar::cache::write_pane_topology_cache(
            runtime,
            &PaneTopologyCache {
                session_name: "rimz-test".to_owned(),
                produced_at_ms: crate::sidebar::timing::unix_now_ms(),
                writer: None,
                focused_pane: None,
                clients: None,
                panes: vec![
                    pane(1, 0, 80, "rimz-sidebar"),
                    pane(2, 80, u64::from(view_cols.saturating_sub(80)), "work"),
                ],
            },
        )
        .expect("write pane topology");
    }

    #[test]
    fn one_target_converges_from_both_directions_and_none_stays_idle() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(80, now), Some((80, 72)));

        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(60, now), Some((60, 72)));

        let mut control = WidthControl::new(None);
        assert_eq!(control.decide(60, now), None);
    }

    #[test]
    fn width_target_pin_broadcasts_without_a_producer_fetch() {
        let (dir, runtime, _controller) = controller(MuxName::Tmux);
        let instance = SidebarInstanceId::new();
        let socket_path = runtime.sock_dir.join("width-target-test.sock");
        let socket = UnixDatagram::bind(&socket_path).expect("bind wakeup socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set socket timeout");
        crate::sidebar::write_heartbeat(
            &runtime,
            runtime.workspace_id.clone(),
            &instance,
            MuxName::Tmux,
            "rimz-test",
            &socket_path,
            None,
        )
        .expect("write heartbeat");

        let permille = crate::sidebar::width_target::pin(&runtime, target(82), MuxName::Tmux, 200)
            .expect("pin width target");
        assert_eq!(crate::sidebar::width_target::load(&runtime), Some(permille));
        let mut payload = [0_u8; 1024];
        let received = socket.recv(&mut payload).expect("receive target broadcast");
        let envelope: SidebarEventEnvelope =
            serde_json::from_slice(&payload[..received]).expect("decode target broadcast");
        assert_eq!(envelope.event, SidebarEvent::WidthTargetChanged);
        drop(dir);
    }

    #[test]
    fn zellij_uses_live_step_and_rejects_floor_crossing() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        let diag = crate::diag::DiagSink::disabled();

        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(
            crate::sidebar::width_target::pinned(&runtime),
            Some(crate::mux::WidthPermille::from_percent(45)),
        );
        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(
            crate::sidebar::width_target::pinned(&runtime),
            Some(crate::mux::WidthPermille::from_percent(50)),
            "repeated keys compound on persisted pending intent",
        );
        let prior = NonZeroU16::new(30).expect("prior target");
        let prior_share = crate::sidebar::width_target::pin(&runtime, prior, MuxName::Zellij, 200)
            .expect("pin prior target");
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
        controller.adjust(30, WidthAdjust::Narrower, &diag);
        assert_eq!(
            crate::sidebar::width_target::load(&runtime),
            Some(prior_share),
        );
    }

    #[test]
    fn zellij_intent_without_topology_never_pins_a_phantom_share() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        let diag = crate::diag::DiagSink::disabled();

        controller.adjust(80, WidthAdjust::Narrower, &diag);
        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    }

    #[test]
    fn observation_without_an_owned_pane_stays_idle() {
        let (_dir, runtime, _) = controller(MuxName::Tmux);
        let mut controller = WidthController::new(
            runtime,
            "rimz-test".to_owned(),
            None,
            MuxName::Tmux,
            crate::mux::SidebarWidth::default(),
        );

        controller.observe(
            80,
            SidebarWidthControlTrigger::ResizeFeedback,
            &crate::diag::DiagSink::disabled(),
        );

        assert_eq!(controller.feedback_deadline(), None);
    }

    #[test]
    fn first_backend_geometry_resolves_the_initial_target() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        let diag = crate::diag::DiagSink::disabled();

        assert_eq!(controller.convergence.target(), None);
        controller.backstop(Some(80), Some(1), None, &diag);

        assert_eq!(controller.convergence.target(), Some(target(50)));
        assert_eq!(controller.last_classified, Some((200, 1)));
    }

    #[test]
    fn missing_backend_geometry_retries_the_baseline_at_most_once_per_second() {
        let (_dir, _runtime, mut controller) = controller(MuxName::Zellij);
        let diag = crate::diag::DiagSink::disabled();

        controller.backstop(Some(80), Some(1), None, &diag);
        let retry = controller
            .baseline_probe_deadline
            .expect("failed baseline re-arms the probe");
        assert!(retry > Instant::now());

        controller.backstop(Some(80), Some(1), None, &diag);
        assert_eq!(
            controller.baseline_probe_deadline,
            Some(retry),
            "an immediate render iteration does not probe again",
        );
        assert_eq!(controller.last_classified, None);
    }

    #[test]
    fn legitimate_paint_width_tracks_the_target_or_falls_back_to_the_cap() {
        let (_dir, _runtime, mut controller) = controller(MuxName::Zellij);

        assert_eq!(controller.max_legit_cols(), controller.width.max_cols.get(),);
        controller.convergence.retarget(Some(target(50)));
        assert_eq!(controller.max_legit_cols(), 50);
        controller.convergence.retarget(Some(target(90)));
        assert_eq!(controller.max_legit_cols(), 90);
    }

    #[test]
    fn settled_drag_pins_once_after_the_debounce() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        controller.last_classified = Some((200, 1));
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

        assert_eq!(
            crate::sidebar::width_target::pinned(&runtime),
            Some(crate::mux::WidthPermille::from_percent(40)),
        );
        assert_eq!(controller.classification_deadline, None);
        assert!(
            !controller.convergence.in_flight(),
            "adopting a drag inside the seeded band must not nudge it",
        );
        controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);
        assert_eq!(
            crate::sidebar::width_target::pinned(&runtime),
            Some(crate::mux::WidthPermille::from_percent(40)),
        );
        assert!(
            !controller.convergence.in_flight(),
            "the next backstop must leave the adopted width parked",
        );
    }

    #[test]
    fn broadcast_reload_uses_the_seeded_native_band() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        let diag = crate::diag::DiagSink::disabled();

        controller.backstop(Some(50), Some(1), None, &diag);
        crate::sidebar::width_target::pin(&runtime, target(83), MuxName::Zellij, 200)
            .expect("pin external target");
        controller.reload_target(&crate::config::ThemeConfig::default(), Some(83), &diag);

        assert_eq!(controller.convergence.target(), Some(target(80)));
        assert_eq!(controller.convergence.tolerance(), 5);
        assert!(!controller.convergence.in_flight());
    }

    #[test]
    fn drag_inside_the_native_band_never_arms_classification() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        let diag = crate::diag::DiagSink::disabled();

        controller.backstop(Some(50), Some(1), None, &diag);
        controller.observe(54, SidebarWidthControlTrigger::ResizeFeedback, &diag);

        assert_eq!(controller.classification_deadline, None);
        assert_eq!(controller.classification_resize_at_ms, None);
        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    }

    #[test]
    fn settled_structural_resize_converges_without_adopting() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        controller.last_classified = Some((200, 1));
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(83), Some(2), Some(u64::MAX), &diag);

        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
        assert_eq!(controller.convergence.target(), Some(target(50)));
    }

    #[test]
    fn settled_view_resize_reresolves_an_unpinned_target() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology_for_view(&runtime, 240);
        controller.last_classified = Some((200, 1));
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(80, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(80), Some(1), Some(u64::MAX), &diag);

        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
        assert_eq!(controller.convergence.target(), Some(target(60)));
    }

    #[test]
    fn settled_view_resize_scales_a_pinned_target() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology_for_view(&runtime, 240);
        controller.last_classified = Some((200, 1));
        let share = crate::sidebar::width_target::pin(&runtime, target(80), MuxName::Zellij, 200)
            .expect("pin width target");
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(96, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(96), Some(1), Some(u64::MAX), &diag);

        assert_eq!(crate::sidebar::width_target::pinned(&runtime), Some(share));
        assert_eq!(controller.convergence.target(), Some(target(96)));
    }

    #[test]
    fn settled_resize_without_geometry_never_adopts() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        controller.last_classified = Some((200, 1));
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
        assert!(controller.classification_deadline.is_some());
    }

    #[test]
    fn stale_pane_observation_converges_and_rearms_without_adopting() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        write_zellij_topology(&runtime);
        controller.last_classified = Some((200, 1));
        let diag = crate::diag::DiagSink::disabled();
        controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

        controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
        let resize_at_ms = controller
            .classification_resize_at_ms
            .expect("resize starts classification");
        controller.classification_deadline = Some(Instant::now());
        controller.backstop(Some(83), Some(1), Some(resize_at_ms), &diag);

        assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
        assert!(controller.classification_deadline.is_some());
        assert_eq!(controller.classification_resize_at_ms, Some(resize_at_ms));
    }

    #[test]
    fn observed_step_sets_the_reachable_tolerance() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(
            control.decide(60, now + Duration::from_millis(10)),
            Some((60, 72))
        );
        assert_eq!(control.decide(68, now + Duration::from_millis(20)), None);
    }

    #[test]
    fn seeded_native_step_parks_inside_half_a_step() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        control.seed_native_step(10);

        assert_eq!(control.tolerance(), 5);
        assert_eq!(control.decide(83, now), None);
    }

    #[test]
    fn native_step_seed_survives_retargeting() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        control.seed_native_step(10);

        control.retarget(Some(target(90)));

        assert_eq!(control.tolerance(), 5);
        assert_eq!(control.decide(94, now), None);
    }

    #[test]
    fn learned_step_refines_the_seeded_band() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        control.seed_native_step(10);
        assert_eq!(control.decide(60, now), Some((60, 80)));

        assert_eq!(
            control.decide(66, now + Duration::from_millis(10)),
            Some((66, 80)),
        );
        assert_eq!(control.tolerance(), 3);
    }

    #[test]
    fn exact_backend_seed_keeps_a_one_column_band() {
        let now = Instant::now();
        let mut inside = WidthControl::new(Some(target(80)));
        inside.seed_native_step(2);
        assert_eq!(inside.decide(79, now), None);

        let mut outside = WidthControl::new(Some(target(80)));
        outside.seed_native_step(2);
        assert_eq!(outside.decide(78, now), Some((78, 80)));
    }

    #[test]
    fn sign_flip_stops_at_the_nearest_reachable_width() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(68, now), Some((68, 72)));
        assert_eq!(control.decide(76, now + Duration::from_millis(10)), None);
        assert_eq!(control.decide(76, now + FEEDBACK_TIMEOUT * 2), None);
    }

    #[test]
    fn strictly_nearer_crossing_parks_without_a_reverse() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        assert_eq!(control.decide(60, now), Some((60, 80)));

        assert_eq!(control.decide(85, now + Duration::from_millis(10)), None);
        assert!(!control.reverse_issued);
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::StepIssued {
                from: 60,
                target: 80,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::FeedbackLearned {
                settled: 85,
                learned_step: 25,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::Idle {
                at: 85,
                reason: WidthIdleReason::CrossedNearest,
            }),
        );
    }

    #[test]
    fn farther_crossing_reverses_once_then_parks() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        assert_eq!(control.decide(83, now), Some((83, 80)));
        assert_eq!(
            control.decide(70, now + Duration::from_millis(10)),
            Some((70, 80)),
        );
        assert!(control.reverse_issued);

        assert_eq!(control.decide(83, now + Duration::from_millis(20)), None);
        assert_eq!(control.decide(83, now + FEEDBACK_TIMEOUT * 2), None);
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::StepIssued {
                from: 83,
                target: 80,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::FeedbackLearned {
                settled: 70,
                learned_step: 13,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::StepIssued {
                from: 70,
                target: 80,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::FeedbackLearned {
                settled: 83,
                learned_step: 13,
            }),
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::Idle {
                at: 83,
                reason: WidthIdleReason::ReverseParked,
            }),
        );
    }

    #[test]
    fn reverse_step_parks_even_outside_the_learned_band() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(80)));
        assert_eq!(control.decide(83, now), Some((83, 80)));
        assert_eq!(
            control.decide(70, now + Duration::from_millis(10)),
            Some((70, 80)),
        );

        assert_eq!(control.decide(95, now + Duration::from_millis(20)), None);
        assert_eq!(control.tolerance(), 12);
        assert!(95_u16.abs_diff(80) > control.tolerance());
        assert_eq!(
            control.traces.back(),
            Some(&WidthTransition::Idle {
                at: 95,
                reason: WidthIdleReason::ReverseParked,
            }),
        );
    }

    #[test]
    fn unchanged_measurement_retries_once_then_stops() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT / 2), None);
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT), Some((50, 72)));
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 2), None);
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 3), None);
    }

    #[test]
    fn one_step_stays_in_flight_until_feedback() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(50, now + Duration::from_millis(999)), None);
        assert_eq!(control.feedback_deadline(), Some(now + FEEDBACK_TIMEOUT));
    }

    #[test]
    fn retarget_resets_progress_guards() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(80, now + Duration::from_millis(10)), None);
        control.retarget(Some(target(60)));
        assert_eq!(
            control.decide(50, now + Duration::from_millis(20)),
            Some((50, 60))
        );
    }

    #[test]
    fn retarget_keeps_an_issued_step_in_flight() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        control.retarget(Some(target(60)));
        assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
    }

    #[test]
    fn unchanged_retarget_preserves_progress() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        control.retarget(Some(target(72)));
        assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
        assert_eq!(control.steps_issued, 1);
    }

    #[test]
    fn transitions_cover_issue_feedback_and_idle_outcomes() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(72)));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::StepIssued {
                from: 50,
                target: 72,
            })
        );

        assert_eq!(
            control.decide(60, now + Duration::from_millis(10)),
            Some((60, 72))
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::FeedbackLearned {
                settled: 60,
                learned_step: 10,
            })
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::StepIssued {
                from: 60,
                target: 72,
            })
        );

        assert_eq!(control.decide(68, now + Duration::from_millis(20)), None);
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::FeedbackLearned {
                settled: 68,
                learned_step: 8,
            })
        );
        assert_eq!(
            control.take_trace(),
            Some(WidthTransition::Idle {
                at: 68,
                reason: WidthIdleReason::ReachedTolerance,
            })
        );
    }

    #[test]
    fn step_budget_bounds_continuous_progress() {
        let now = Instant::now();
        let mut control = WidthControl::new(Some(target(200)));
        assert_eq!(control.decide(10, now), Some((10, 200)));
        for step in 1..MAX_STEPS {
            let width = 10 + u16::from(step);
            assert_eq!(
                control.decide(width, now + Duration::from_millis(u64::from(step))),
                Some((width, 200))
            );
        }
        assert_eq!(
            control.decide(10 + u16::from(MAX_STEPS), now + Duration::from_secs(1)),
            None
        );
    }
}
