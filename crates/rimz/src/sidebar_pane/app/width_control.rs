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
enum WidthTarget {
    Override(NonZeroU16),
    CapOnly(u16),
}

impl WidthTarget {
    fn from_override(width: Option<NonZeroU16>, cap: NonZeroU16) -> Self {
        width.map_or(Self::CapOnly(cap.get()), Self::Override)
    }

    fn cols(self) -> u16 {
        match self {
            Self::Override(cols) => cols.get(),
            Self::CapOnly(cols) => cols,
        }
    }

    fn needs_adjustment(self, own_cols: u16, tolerance: u16) -> bool {
        match self {
            Self::Override(cols) => own_cols.abs_diff(cols.get()) > tolerance,
            Self::CapOnly(cap) => own_cols > cap && own_cols.abs_diff(cap) > tolerance,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Narrower,
    Wider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthIdleReason {
    ReachedTolerance,
    CrossedNearest,
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
    target: WidthTarget,
    steps_issued: u8,
    in_flight: Option<IssuedStep>,
    learned_step: Option<u16>,
    retried_no_progress: bool,
    idle_at: Option<u16>,
    traces: VecDeque<WidthTransition>,
}

impl WidthControl {
    fn new(target: WidthTarget) -> Self {
        Self {
            target,
            steps_issued: 0,
            in_flight: None,
            learned_step: None,
            retried_no_progress: false,
            idle_at: None,
            traces: VecDeque::new(),
        }
    }

    fn retarget(&mut self, target: WidthTarget) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.steps_issued = 0;
        self.learned_step = None;
        self.retried_no_progress = false;
        self.idle_at = None;
        self.traces.clear();
    }

    fn target(&self) -> WidthTarget {
        self.target
    }

    fn override_target(&self) -> Option<NonZeroU16> {
        match self.target {
            WidthTarget::Override(cols) => Some(cols),
            WidthTarget::CapOnly(_) => None,
        }
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

        if let Some(idle_at) = self.idle_at {
            if idle_at == own_cols {
                return None;
            }
            self.steps_issued = 0;
            self.in_flight = None;
            self.retried_no_progress = false;
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
                if crossed_target(step, own_cols, self.target.cols()) {
                    self.idle_at = Some(own_cols);
                    self.traces.push_back(WidthTransition::Idle {
                        at: own_cols,
                        reason: WidthIdleReason::CrossedNearest,
                    });
                    return None;
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

        let tolerance = self.learned_step.map_or(1, |step| (step / 2).max(1));
        if !self.target.needs_adjustment(own_cols, tolerance) {
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

        let target_cols = self.target.cols();
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
    width_cap: NonZeroU16,
    convergence: WidthControl,
}

impl WidthController {
    pub(super) fn new(
        runtime: RuntimePaths,
        session_name: String,
        own_pane: Option<PaneId>,
        mux: MuxName,
        width_cap: NonZeroU16,
    ) -> Self {
        let target =
            WidthTarget::from_override(crate::sidebar::width_override::load(&runtime), width_cap);
        Self {
            runtime,
            session_name,
            own_pane,
            mux,
            width_cap,
            convergence: WidthControl::new(target),
        }
    }

    pub(super) fn feedback_deadline(&self) -> Option<Instant> {
        self.convergence.feedback_deadline()
    }

    pub(super) fn max_legit_cols(&self) -> u16 {
        match self.convergence.target() {
            WidthTarget::Override(cols) => self.width_cap.get().max(cols.get()),
            WidthTarget::CapOnly(_) => self.width_cap.get(),
        }
    }

    pub(super) fn reload_target(&mut self, measured_cols: Option<u16>, diag: &DiagSink) {
        self.convergence.retarget(WidthTarget::from_override(
            crate::sidebar::width_override::load(&self.runtime),
            self.width_cap,
        ));
        if let Some(cols) = measured_cols {
            self.observe(cols, SidebarWidthControlTrigger::Retarget, diag);
        }
    }

    pub(super) fn adjust(&mut self, own_cols: u16, dir: WidthAdjust, diag: &DiagSink) {
        let Some(pane) = self.own_pane.as_ref() else {
            return;
        };
        let pending_cols = self.convergence.override_target().map(NonZeroU16::get);
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
            Err(err) if dir == WidthAdjust::Wider && self.mux == MuxName::Zellij => {
                let cols = u16::try_from(crate::mux::width::zellij_resize_step_cols(
                    u64::from(own_cols) * 4,
                ))
                .unwrap_or(u16::MAX);
                debug!(error = %err, own_cols, step_cols = cols, "sidebar wider intent using conservative topology fallback");
                crate::mux::WidthStep { cols, exact: false }
            }
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
        if let Err(err) = crate::sidebar::width_override::write(&self.runtime, target) {
            warn!(error = %err, "sidebar width override write failed");
            return;
        }
        spawn_width_default_record(self.mux, &self.session_name, target.get());
        if let Err(err) = crate::store::wakeup::broadcast_sidebar_event(
            &self.runtime,
            Some(&self.session_name),
            crate::sidebar::events::SidebarEvent::WidthTargetChanged,
        ) {
            debug!(error = %err, "sidebar width target broadcast failed");
        }
        self.convergence.retarget(WidthTarget::Override(target));
        self.observe(own_cols, SidebarWidthControlTrigger::Retarget, diag);
    }

    pub(super) fn observe(
        &mut self,
        measured_cols: u16,
        trigger: SidebarWidthControlTrigger,
        diag: &DiagSink,
    ) {
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

    pub(super) fn backstop(&mut self, measured_cols: Option<u16>, diag: &DiagSink) {
        if self
            .feedback_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
            && let Some(cols) = measured_cols
        {
            self.observe(cols, SidebarWidthControlTrigger::Backstop, diag);
        }
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

    fn override_target(cols: u16) -> WidthTarget {
        WidthTarget::Override(NonZeroU16::new(cols).expect("nonzero target"))
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
            NonZeroU16::new(72).expect("width cap"),
        );
        (dir, runtime, controller)
    }

    fn write_zellij_topology(runtime: &RuntimePaths) {
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
                panes: vec![pane(1, 0, 80, "rimz-sidebar"), pane(2, 80, 120, "work")],
            },
        )
        .expect("write pane topology");
    }

    #[test]
    fn cap_only_shrinks_wide_panes_and_leaves_narrow_panes_alone() {
        let now = Instant::now();
        let mut control = WidthControl::new(WidthTarget::CapOnly(72));
        assert_eq!(control.decide(80, now), Some((80, 72)));

        let mut control = WidthControl::new(WidthTarget::CapOnly(72));
        assert_eq!(control.decide(60, now), None);
    }

    #[test]
    fn tmux_adjustments_persist_exact_compounded_intent_and_broadcast() {
        let (dir, runtime, mut controller) = controller(MuxName::Tmux);
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

        let diag = crate::diag::DiagSink::disabled();
        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(
            crate::sidebar::width_override::load(&runtime),
            NonZeroU16::new(82),
        );
        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(
            crate::sidebar::width_override::load(&runtime),
            NonZeroU16::new(84),
            "repeated keys compound on persisted pending intent",
        );
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
            crate::sidebar::width_override::load(&runtime),
            NonZeroU16::new(90),
        );
        let prior = NonZeroU16::new(30).expect("prior target");
        crate::sidebar::width_override::write(&runtime, prior).expect("write prior override");
        controller.reload_target(None, &diag);
        controller.adjust(30, WidthAdjust::Narrower, &diag);
        assert_eq!(crate::sidebar::width_override::load(&runtime), Some(prior));
    }

    #[test]
    fn zellij_wider_falls_back_without_topology_while_narrower_rejects() {
        let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
        let diag = crate::diag::DiagSink::disabled();

        controller.adjust(80, WidthAdjust::Narrower, &diag);
        assert_eq!(crate::sidebar::width_override::load(&runtime), None);
        controller.adjust(80, WidthAdjust::Wider, &diag);
        assert_eq!(
            crate::sidebar::width_override::load(&runtime),
            NonZeroU16::new(96),
        );
    }

    #[test]
    fn observed_step_sets_the_reachable_tolerance() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(
            control.decide(60, now + Duration::from_millis(10)),
            Some((60, 72))
        );
        assert_eq!(control.decide(68, now + Duration::from_millis(20)), None);
    }

    #[test]
    fn sign_flip_stops_at_the_nearest_reachable_width() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(68, now), Some((68, 72)));
        assert_eq!(control.decide(76, now + Duration::from_millis(10)), None);
        assert_eq!(control.decide(76, now + FEEDBACK_TIMEOUT * 2), None);
    }

    #[test]
    fn unchanged_measurement_retries_once_then_stops() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT / 2), None);
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT), Some((50, 72)));
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 2), None);
        assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 3), None);
    }

    #[test]
    fn one_step_stays_in_flight_until_feedback() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(50, now + Duration::from_millis(999)), None);
        assert_eq!(control.feedback_deadline(), Some(now + FEEDBACK_TIMEOUT));
    }

    #[test]
    fn retarget_resets_progress_guards() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        assert_eq!(control.decide(80, now + Duration::from_millis(10)), None);
        control.retarget(override_target(60));
        assert_eq!(
            control.decide(50, now + Duration::from_millis(20)),
            Some((50, 60))
        );
    }

    #[test]
    fn retarget_keeps_an_issued_step_in_flight() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        control.retarget(override_target(60));
        assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
    }

    #[test]
    fn unchanged_retarget_preserves_progress() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
        assert_eq!(control.decide(50, now), Some((50, 72)));
        control.retarget(override_target(72));
        assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
        assert_eq!(control.steps_issued, 1);
    }

    #[test]
    fn transitions_cover_issue_feedback_and_idle_outcomes() {
        let now = Instant::now();
        let mut control = WidthControl::new(override_target(72));
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
        let mut control = WidthControl::new(override_target(200));
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
