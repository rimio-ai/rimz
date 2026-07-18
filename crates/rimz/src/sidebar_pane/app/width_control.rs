//! Pure renderer-local sidebar width controller.

use std::collections::VecDeque;
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

pub(super) const FEEDBACK_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_STEPS: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WidthTarget {
    Override(NonZeroU16),
    CapOnly(u16),
}

impl WidthTarget {
    pub(super) fn from_override(width: Option<NonZeroU16>, cap: NonZeroU16) -> Self {
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
pub(super) enum WidthIdleReason {
    ReachedTolerance,
    CrossedNearest,
    NoProgress,
    StepBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WidthTransition {
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
pub(super) struct WidthControl {
    target: WidthTarget,
    steps_issued: u8,
    in_flight: Option<IssuedStep>,
    learned_step: Option<u16>,
    retried_no_progress: bool,
    idle_at: Option<u16>,
    traces: VecDeque<WidthTransition>,
}

impl WidthControl {
    pub(super) fn new(target: WidthTarget) -> Self {
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

    pub(super) fn retarget(&mut self, target: WidthTarget) {
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

    pub(super) fn target(&self) -> WidthTarget {
        self.target
    }

    pub(super) fn override_target(&self) -> Option<NonZeroU16> {
        match self.target {
            WidthTarget::Override(cols) => Some(cols),
            WidthTarget::CapOnly(_) => None,
        }
    }

    pub(super) fn feedback_deadline(&self) -> Option<Instant> {
        self.in_flight.map(|step| step.at + FEEDBACK_TIMEOUT)
    }

    pub(super) fn take_trace(&mut self) -> Option<WidthTransition> {
        self.traces.pop_front()
    }

    /// Return one `(current, target)` actuator request, recording it as the
    /// sole in-flight step until a changed measurement or timeout arrives.
    pub(super) fn decide(&mut self, own_cols: u16, now: Instant) -> Option<(u16, u16)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn override_target(cols: u16) -> WidthTarget {
        WidthTarget::Override(NonZeroU16::new(cols).expect("nonzero target"))
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
