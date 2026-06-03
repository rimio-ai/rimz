//! The cockpit today-spend count-up animation.
//!
//! The animated figure is a [`Roll`]: it remembers where it is painted on
//! screen and where the latest snapshot says it should be, then eases between
//! the two. Motion is driven purely by the wall-clock animation phase
//! ([`super::UiState::animation_phase`]) — never by the age of the fetched data
//! — so a roll plays smoothly even when the data behind it is stale, per the
//! render-thread performance contract.
//!
//! A roll fires only on an *increase*: a decrease (today's UTC-midnight reset)
//! and the first observed value both snap, so the cockpit never plays a sad
//! count-down or a dramatic `0 → today` roll on boot. The provider dashboard's
//! W/M ledger rows are deliberately static — only today's headline figure climbs.

use rimz::SpendTally;

/// Frames a roll takes to settle — about 700ms at the 100ms animation tick.
const ROLL_FRAMES: u64 = 7;
/// Frames the figure stays brightened just after it lands — the quiet
/// "ka-chunk" that makes the climb satisfying without any glyph burst.
const FLASH_FRAMES: u64 = 2;

/// Cubic ease-out: fast off the mark, gently settling onto the target. `t` is
/// clamped to `0.0..=1.0`; `f(0) = 0`, `f(1) = 1`.
fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// One animated scalar — where it is painted versus where the data points.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Roll {
    /// The painted value when the current roll began — the eased journey's start.
    from: f64,
    /// The value the data says we should reach.
    target: f64,
    /// Animation phase the current roll began at; `None` until the first value.
    start_phase: Option<u64>,
}

impl Roll {
    /// Fold in the latest `target`. An increase starts an eased roll from the
    /// value painted right now (so an interrupted climb continues, never jumps);
    /// a decrease or the first-ever value snaps.
    fn observe(&mut self, target: f64, phase: u64) {
        let snap = match self.start_phase {
            None => true,
            Some(_) => target <= self.target,
        };
        self.from = if snap { target } else { self.value_at(phase) };
        self.target = target;
        self.start_phase = Some(phase);
    }

    /// The value to paint at `phase`, easing toward the authoritative `target`
    /// from the snapshot. Mid-climb it interpolates from where the figure was
    /// painted when the climb began; unseeded (no `observe` has run — a one-off
    /// `draw`, or a test), snapped, or settled, it is `target` itself. Reading
    /// the live target here keeps the corner correct even on a render path that
    /// never folded a roll, with the roll supplying only the transition. Pure:
    /// render reads it without mutating, so a frame recomputes at any phase.
    pub(crate) fn display(&self, target: f64, phase: u64) -> f64 {
        match self.start_phase {
            Some(start) if self.from != self.target => {
                let elapsed = phase.saturating_sub(start);
                if elapsed >= ROLL_FRAMES {
                    target
                } else {
                    let progress = ease_out_cubic(elapsed as f64 / ROLL_FRAMES as f64);
                    self.from + (target - self.from) * progress
                }
            }
            // Unseeded, a snap, or a roll long settled: paint the true figure.
            _ => target,
        }
    }

    /// The painted value against the roll's own stored target — the start point
    /// `observe` captures when a fresh climb interrupts one already in flight.
    fn value_at(&self, phase: u64) -> f64 {
        self.display(self.target, phase)
    }

    /// Whether this roll still needs the fast animation tick — through the climb,
    /// the brief flash, and one trailing clean frame, so the last frame painted
    /// is the settled value rather than a stuck brighten. A snap (`from ==
    /// target`) has no motion, so it never holds the tick.
    fn rolling(&self, phase: u64) -> bool {
        self.from != self.target
            && self
                .elapsed(phase)
                .is_some_and(|e| e <= ROLL_FRAMES + FLASH_FRAMES)
    }

    /// Within the brief brighten window just after the figure lands. A snap never
    /// flashes — only a genuine climb earns the "ka-chunk".
    pub(crate) fn flashing(&self, phase: u64) -> bool {
        self.from != self.target
            && self
                .elapsed(phase)
                .is_some_and(|e| (ROLL_FRAMES..ROLL_FRAMES + FLASH_FRAMES).contains(&e))
    }

    fn elapsed(&self, phase: u64) -> Option<u64> {
        self.start_phase.map(|start| phase.saturating_sub(start))
    }
}

/// The cockpit's one animated figure: today's fleet spend, the headline that
/// climbs as a turn lands. The W/M ledger rows below read straight from the
/// tally with no roll.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TallyAnim {
    pub(crate) today_usd: Roll,
}

impl TallyAnim {
    /// Fold the latest tally's today-spend target into the roll. Called on each
    /// data refresh that carries a tally; a refresh without one leaves the roll
    /// untouched, so a transient missing snapshot never snaps the figure to zero.
    pub(crate) fn observe(&mut self, tally: &SpendTally, phase: u64) {
        self.today_usd.observe(tally.today.usd, phase);
    }

    /// Whether the figure is still mid-roll — the serve loop ORs this into its
    /// animation gate so a finished-turn climb plays even when no agent is
    /// running, then lets the loop fall back to the slow data tick once settled.
    pub(crate) fn any_rolling(&self, phase: u64) -> bool {
        self.today_usd.rolling(phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_pins_endpoints_and_fronts_load() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(-1.0) == 0.0 && ease_out_cubic(2.0) == 1.0);
        // Ease-*out* covers more than half the distance by the halfway point.
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn first_value_snaps_then_increase_rolls() {
        let mut r = Roll::default();
        r.observe(100.0, 0);
        // First observation snaps — no boot roll from zero.
        assert_eq!(r.value_at(0), 100.0);
        assert!(!r.rolling(0));

        // A genuine increase eases from the painted value to the new target.
        r.observe(200.0, 10);
        assert_eq!(r.value_at(10), 100.0, "starts at the prior value");
        assert!(r.value_at(13) > 100.0 && r.value_at(13) < 200.0, "mid-roll");
        assert_eq!(r.value_at(10 + ROLL_FRAMES), 200.0, "settles on target");
    }

    #[test]
    fn decrease_snaps_without_a_countdown() {
        let mut r = Roll::default();
        r.observe(40.0, 0);
        r.observe(0.5, 5); // UTC-midnight today reset
        assert_eq!(r.value_at(5), 0.5, "snaps down, never rolls backward");
        assert!(!r.rolling(5));
    }

    #[test]
    fn flash_lands_after_the_climb_then_clears() {
        let mut r = Roll::default();
        r.observe(1.0, 0);
        r.observe(2.0, 0); // increase at phase 0
        assert!(!r.flashing(0), "no flash mid-climb");
        assert!(r.flashing(ROLL_FRAMES), "brightens once it lands");
        assert!(
            r.rolling(ROLL_FRAMES + FLASH_FRAMES),
            "one clean trailing frame"
        );
        assert!(
            !r.rolling(ROLL_FRAMES + FLASH_FRAMES + 1),
            "then releases the tick"
        );
    }
}
