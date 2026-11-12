//! Wheel and birth-reveal viewport motion, sampled purely from the serve loop's millisecond
//! clock.

use std::time::Duration;

use super::FrameTime;

pub(in crate::sidebar_pane) const SCROLL_EASE_DURATION: Duration = Duration::from_millis(100);
pub(in crate::sidebar_pane) const SCROLL_EASE_FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(in crate::sidebar_pane) struct ScrollEase {
    armed: bool,
    /// The flight armed or running is a birth reveal's, a move the user did not make.
    birth: bool,
    from: isize,
    to: isize,
    started_ms: FrameTime,
}

impl ScrollEase {
    pub(in crate::sidebar_pane) fn arm(&mut self) {
        self.armed = true;
        self.birth = false;
    }

    /// Arm the next target change as a birth reveal's glide.
    pub(super) fn arm_birth(&mut self) {
        self.armed = true;
        self.birth = true;
    }

    /// Resolve a draw's target without mutating the flight; an unarmed change snaps.
    pub(in crate::sidebar_pane) fn visible(&self, target: usize, now_ms: FrameTime) -> usize {
        if target as isize != self.to && !self.armed {
            return target;
        }
        self.position(now_ms)
    }

    /// Commit the draw's resolved target and consume its one-shot wheel arm.
    pub(in crate::sidebar_pane) fn observe(&mut self, target: usize, now_ms: FrameTime) {
        let visible = self.visible(target, now_ms);
        if target as isize != self.to {
            self.from = visible as isize;
            self.to = target as isize;
            self.started_ms = now_ms;
        } else if visible as isize == self.to {
            self.from = self.to;
        }
        self.armed = false;
    }

    /// Land the flight on its target now and drop any pending wheel arm.
    pub(in crate::sidebar_pane) fn cancel(&mut self) {
        self.from = self.to;
        self.armed = false;
    }

    /// Translate a correction without changing the flight's distance or clock.
    pub(super) fn shift(&mut self, correction: isize) {
        self.from = self.from.saturating_add(correction);
        self.to = self.to.saturating_add(correction);
    }

    pub(in crate::sidebar_pane) fn in_flight(&self) -> bool {
        self.from != self.to
    }

    /// In flight on the user's own wheel, not on a birth reveal.
    pub(in crate::sidebar_pane) fn wheel_in_flight(&self) -> bool {
        self.in_flight() && !self.birth
    }

    pub(in crate::sidebar_pane) fn birth_in_flight(&self) -> bool {
        self.in_flight() && self.birth
    }

    fn position(&self, now_ms: FrameTime) -> usize {
        let duration = SCROLL_EASE_DURATION.as_millis();
        let elapsed = now_ms
            .elapsed_since(self.started_ms)
            .as_millis()
            .min(duration);
        let denominator = duration * duration;
        let distance = self.from.abs_diff(self.to) as u128;
        let moved = ((distance * elapsed * (2 * duration - elapsed) + denominator / 2)
            / denominator) as isize;
        let position = if self.from <= self.to {
            self.from + moved
        } else {
            self.from - moved
        };
        position.max(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(elapsed_ms: u64) -> FrameTime {
        FrameTime::from_millis(elapsed_ms)
    }

    const START: u64 = 1000;

    fn flight(from: usize, to: usize) -> ScrollEase {
        let mut ease = ScrollEase::default();
        ease.observe(from, ms(START));
        ease.arm();
        ease.observe(to, ms(START));
        ease
    }

    #[test]
    fn armed_target_eases_and_settles() {
        let mut ease = ScrollEase::default();
        ease.arm();
        assert_eq!(ease.visible(9, ms(START)), 0);
        ease.observe(9, ms(START));
        assert!(ease.in_flight());
        let mut previous = 0;
        for elapsed in (0..100).step_by(SCROLL_EASE_FRAME.as_millis() as usize) {
            let row = ease.visible(9, ms(START + elapsed));
            assert!(row >= previous && row <= 9);
            previous = row;
        }
        assert!((1..9).contains(&ease.visible(9, ms(START + 50))));
        assert_eq!(ease.visible(9, ms(START + 100)), 9);
        ease.observe(9, ms(START + 100));
        assert!(!ease.in_flight());
    }

    #[test]
    fn late_frame_uses_elapsed_time_not_frame_count() {
        let late = flight(0, 9);
        assert!((1..9).contains(&late.visible(9, ms(START + 60))));
        let mut playback = late;
        for elapsed in [16, 32, 48, 60] {
            playback.observe(9, ms(START + elapsed));
        }
        assert_eq!(
            late.visible(9, ms(START + 60)),
            playback.visible(9, ms(START + 60))
        );
        assert_eq!(late.visible(9, ms(START + 5100)), 9);
    }

    #[test]
    fn retarget_starts_at_the_current_visible_row() {
        let mut ease = flight(0, 9);
        let before = ease.visible(9, ms(START + 40));
        ease.arm();
        assert_eq!(ease.visible(18, ms(START + 40)), before);
        ease.observe(18, ms(START + 40));
        assert_eq!(ease.visible(18, ms(START + 40)), before);
        assert_eq!(ease.visible(18, ms(START + 140)), 18);
    }

    #[test]
    fn unarmed_target_change_snaps() {
        let mut ease = flight(0, 9);
        assert!(ease.in_flight());
        assert_eq!(ease.visible(4, ms(START + 20)), 4);
        ease.observe(4, ms(START + 20));
        assert_eq!(ease.visible(4, ms(START + 20)), 4);
        assert!(!ease.in_flight());
    }

    #[test]
    fn upward_flight_eases_and_settles() {
        let mut ease = flight(9, 0);
        assert_eq!(ease.visible(0, ms(START)), 9);
        let mut previous = 9;
        for elapsed in (0..100).step_by(SCROLL_EASE_FRAME.as_millis() as usize) {
            let row = ease.visible(0, ms(START + elapsed));
            assert!(row <= previous);
            previous = row;
        }
        assert!((1..9).contains(&ease.visible(0, ms(START + 50))));
        assert_eq!(ease.visible(0, ms(START + 100)), 0);
        ease.observe(0, ms(START + 100));
        assert!(!ease.in_flight());
    }

    #[test]
    fn unchanged_target_consumes_the_arm_without_motion() {
        let mut ease = ScrollEase::default();
        ease.arm();
        ease.observe(0, ms(START));
        assert!(!ease.armed);
        assert!(!ease.in_flight());
        assert_eq!(ease.visible(4, ms(START)), 4);
    }
}
