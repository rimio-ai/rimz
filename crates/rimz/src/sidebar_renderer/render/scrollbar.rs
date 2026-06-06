//! The agent-cards scrollbar's auto-hide fade.
//!
//! In the default `auto` mode ([`crate::config::ScrollbarMode`]) the bar shows
//! only while the viewport is moving — a wheel scroll, the selection-driven
//! auto-follow, a clamp shift — then hides once the view settles. Like the
//! odometer's [`super::odometer::Roll`], motion is driven purely by the
//! wall-clock animation phase ([`super::UiState::animation_phase`]): every
//! draw [`observe`](ScrollbarFade::observe)s the resolved viewport offset, a
//! change stamps the phase, and visibility is a pure read over that stamp —
//! so golden tests pin the bar deterministically and render never touches the
//! wall clock.
//!
//! The baseline lives here rather than in a `prev != ui.scroll_offset`
//! compare because a wheel tick mutates `ui.scroll_offset` *before* the draw
//! ([`crate::sidebar_renderer::app::selection`]), which would hide every wheel move from a
//! write-back comparison. `last_offset` advances only at `observe`, so the
//! next draw — synchronous for input — sees every kind of move through one
//! mechanism, stamped at the freshly-set phase.

/// Phases the bar lingers after the viewport last moved — about a second at
/// the 100ms animation tick.
const FADE_FRAMES: u64 = 10;

/// The settle-window state behind the `auto` scrollbar mode: the offset the
/// last draw resolved, and the phase the viewport last moved at.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScrollbarFade {
    /// Viewport offset at the last draw; `None` before the first draw, so the
    /// first frame establishes a baseline rather than reading as a move.
    last_offset: Option<usize>,
    /// Animation phase the viewport last moved at; `None` until the first move.
    last_activity: Option<u64>,
}

impl ScrollbarFade {
    /// Fold in the offset a draw resolved: a change from the last draw stamps
    /// scroll activity at `phase`. Called at the draw write-back, beside
    /// `scroll_offset` itself.
    pub(crate) fn observe(&mut self, offset: usize, phase: u64) {
        if self.moved_from(offset) {
            self.last_activity = Some(phase);
        }
        self.last_offset = Some(offset);
    }

    /// Whether `offset` is a move against the last draw's baseline — the
    /// same-frame signal that paints the bar on the very frame the viewport
    /// moves, before `observe` has stamped it.
    pub(crate) fn moved_from(&self, offset: usize) -> bool {
        self.last_offset.is_some_and(|prev| prev != offset)
    }

    /// Whether the bar shows at `phase`: within the settle window after the
    /// last move. Pure — render reads it without mutating.
    pub(crate) fn visible(&self, phase: u64) -> bool {
        self.last_activity
            .is_some_and(|stamp| phase.saturating_sub(stamp) <= FADE_FRAMES)
    }

    /// Whether the fade still needs the fast animation tick — through the
    /// visible window plus one trailing clean frame, so the frame that hides
    /// the bar is actually painted rather than waiting on the slow data tick.
    pub(crate) fn fading(&self, phase: u64) -> bool {
        self.last_activity
            .is_some_and(|stamp| phase.saturating_sub(stamp) <= FADE_FRAMES + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observe_establishes_baseline_without_showing() {
        let mut fade = ScrollbarFade::default();
        assert!(!fade.moved_from(3), "no baseline yet — never a move");
        fade.observe(3, 0);
        assert!(!fade.visible(0), "the first draw is a baseline, not a move");
        assert!(!fade.fading(0));
    }

    #[test]
    fn offset_change_shows_then_settles() {
        let mut fade = ScrollbarFade::default();
        fade.observe(0, 0);
        assert!(fade.moved_from(2), "a changed offset reads as a move");
        fade.observe(2, 5);
        assert!(fade.visible(5), "visible on the move frame");
        assert!(fade.visible(5 + FADE_FRAMES), "through the settle window");
        assert!(!fade.visible(5 + FADE_FRAMES + 1), "then hides");
        assert!(
            fade.fading(5 + FADE_FRAMES + 1),
            "one clean trailing frame paints the hide"
        );
        assert!(!fade.fading(5 + FADE_FRAMES + 2), "then releases the tick");
    }

    #[test]
    fn re_stamp_resets_the_window() {
        let mut fade = ScrollbarFade::default();
        fade.observe(0, 0);
        fade.observe(1, 2);
        fade.observe(2, 8);
        assert!(
            fade.visible(8 + FADE_FRAMES),
            "the newest move sets the window"
        );
        assert!(!fade.visible(8 + FADE_FRAMES + 1));
    }

    #[test]
    fn unchanged_offset_never_stamps() {
        let mut fade = ScrollbarFade::default();
        fade.observe(4, 0);
        fade.observe(4, 1);
        assert!(!fade.visible(1), "a held viewport stays bar-less");
    }
}
