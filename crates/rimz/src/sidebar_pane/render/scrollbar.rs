//! The agent-cards scrollbar's auto-hide fade.
//!
//! In the default `auto` mode ([`crate::config::ScrollbarMode`]) the bar shows
//! only while the viewport is moving — a wheel scroll, the selection-driven
//! reveal, or jump — then hides once the view settles. Anchor corrections
//! translate the baseline without stamping activity. Like the
//! odometer's [`super::odometer::Roll`], motion is driven purely by the
//! driver-clock milliseconds ([`super::ViewState::frame_ms`]): every
//! draw [`observe`](ScrollbarFade::observe)s the visible viewport offset, a
//! change stamps that time, and visibility counts base frames on the current grid —
//! so golden tests pin the bar deterministically and render never touches the
//! wall clock.
//!
//! The baseline lives here rather than in a `prev != view.scroll_offset`
//! compare because a wheel tick mutates the target `view.scroll_offset`
//! *before* the draw ([`crate::sidebar_pane::app::selection`]), and
//! `view.scroll_ease` then moves the visible row over the following frames.
//! `last_offset` advances only at `observe`, which takes the visible row, so
//! the fade and thumb follow the cards rather than the input target.

use super::FrameTime;

/// Base frames on the current refresh grid that the bar lingers after the viewport
/// last moved — about a second at the default 100ms animation tick.
const FADE_FRAMES: u64 = 10;

/// The settle-window state behind the `auto` scrollbar mode: the offset the
/// last draw resolved, and the frame time the viewport last moved at.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::sidebar_pane) struct ScrollbarFade {
    /// Viewport offset at the last draw; `None` before the first draw, so the
    /// first frame establishes a baseline rather than reading as a move.
    last_offset: Option<usize>,
    /// Frame time the viewport last moved at; `None` until the first move.
    last_activity: Option<FrameTime>,
}

impl ScrollbarFade {
    pub(super) fn shift(&mut self, correction: isize) {
        self.last_offset = self
            .last_offset
            .map(|offset| offset.saturating_add_signed(correction));
    }

    /// Fold in the visible offset a draw resolved: a change from the last draw
    /// stamps scroll activity at `now`. Called at the draw write-back, beside
    /// `scroll_offset` and `scroll_ease`.
    pub(in crate::sidebar_pane) fn observe(&mut self, offset: usize, now: FrameTime) {
        if self.moved_from(offset) {
            self.last_activity = Some(now);
        }
        self.last_offset = Some(offset);
    }

    /// Whether `offset` is a move against the last draw's baseline — the
    /// same-frame signal that paints the bar on the very frame the viewport
    /// moves, before `observe` has stamped it.
    pub(in crate::sidebar_pane) fn moved_from(&self, offset: usize) -> bool {
        self.last_offset.is_some_and(|prev| prev != offset)
    }

    /// Whether the bar shows at `now`: within the settle window after the
    /// last move. Pure — render reads it without mutating.
    pub(in crate::sidebar_pane) fn visible(&self, now: FrameTime, refresh_ms: u16) -> bool {
        self.last_activity
            .is_some_and(|stamp| now.elapsed_frames_since(stamp, refresh_ms) <= FADE_FRAMES)
    }

    /// Whether the fade still needs the fast animation tick — through the
    /// visible window plus one trailing clean frame, so the frame that hides
    /// the bar is actually painted rather than waiting on the slow data tick.
    pub(in crate::sidebar_pane) fn fading(&self, now: FrameTime, refresh_ms: u16) -> bool {
        self.last_activity
            .is_some_and(|stamp| now.elapsed_frames_since(stamp, refresh_ms) <= FADE_FRAMES + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(elapsed_ms: u64) -> FrameTime {
        FrameTime::from_millis(elapsed_ms)
    }

    #[test]
    fn first_observe_establishes_baseline_without_showing() {
        let mut fade = ScrollbarFade::default();
        assert!(!fade.moved_from(3), "no baseline yet — never a move");
        fade.observe(3, ms(0));
        assert!(
            !fade.visible(ms(0), 100),
            "the first draw is a baseline, not a move"
        );
        assert!(!fade.fading(ms(0), 100));
    }

    #[test]
    fn offset_change_shows_then_settles() {
        let mut fade = ScrollbarFade::default();
        fade.observe(0, ms(0));
        assert!(fade.moved_from(2), "a changed offset reads as a move");
        fade.observe(2, ms(500));
        assert!(fade.visible(ms(500), 100), "visible on the move frame");
        assert!(
            fade.visible(ms((5 + FADE_FRAMES) * 100), 100),
            "through the settle window"
        );
        assert!(
            !fade.visible(ms((5 + FADE_FRAMES + 1) * 100), 100),
            "then hides"
        );
        assert!(
            fade.fading(ms((5 + FADE_FRAMES + 1) * 100), 100),
            "one clean trailing frame paints the hide"
        );
        assert!(
            !fade.fading(ms((5 + FADE_FRAMES + 2) * 100), 100),
            "then releases the tick"
        );
    }

    #[test]
    fn re_stamp_resets_the_window() {
        let mut fade = ScrollbarFade::default();
        fade.observe(0, ms(0));
        fade.observe(1, ms(200));
        fade.observe(2, ms(800));
        assert!(
            fade.visible(ms((8 + FADE_FRAMES) * 100), 100),
            "the newest move sets the window"
        );
        assert!(!fade.visible(ms((8 + FADE_FRAMES + 1) * 100), 100));
    }

    #[test]
    fn unchanged_offset_never_stamps() {
        let mut fade = ScrollbarFade::default();
        fade.observe(4, ms(0));
        fade.observe(4, ms(100));
        assert!(
            !fade.visible(ms(100), 100),
            "a held viewport stays bar-less"
        );
    }
}
