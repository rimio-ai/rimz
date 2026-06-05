//! The money count-up animation — the cockpit today-spend headline and each
//! agent card's session cost.
//!
//! The animated figure is a [`Roll`]: it remembers where it is painted on
//! screen and where the latest snapshot says it should be, then ticks between
//! the two like an odometer. Each animation tick steps a tenth of the
//! remaining gap — never less than one cent — so big decaying steps rush the
//! figure toward the target and the final stretch counts penny by penny onto
//! the exact two-decimal value. Motion is driven purely by the wall-clock
//! animation phase ([`super::UiState::animation_phase`]) — never by the age of
//! the fetched data — so a roll plays smoothly even when the data behind it is
//! stale, per the render-thread performance contract.
//!
//! A roll fires only on an *increase*: a decrease (today's UTC-midnight reset)
//! and the first observed value both snap, so a figure never plays a sad
//! count-down or a dramatic `0 → today` roll on boot. The provider dashboard's
//! W/M ledger rows are deliberately static — only the cockpit headline and the
//! per-card costs climb.

use std::collections::{HashMap, HashSet};

use rimz::SpendTally;

/// Frames the figure stays brightened just after it lands — the quiet
/// "ka-chunk" that makes the climb satisfying without any glyph burst.
const FLASH_FRAMES: u64 = 2;

/// Hard cap on the ticks a climb may walk — a climb past the cap snaps to the
/// target, so a pathological gap can neither spin the per-frame walk nor
/// strand a figure short of the truth. The decaying step settles any realistic
/// gap in well under a hundred ticks, so the cap never bites in practice.
const MAX_TICKS: u64 = 512;

/// Dollars to integer cents, mirroring `dollars2`'s rounding exactly so the
/// stepped walk and the formatter always agree on the painted figure.
fn to_cents(usd: f64) -> u64 {
    (usd.max(0.0) * 100.0).round() as u64
}

/// One animation tick's increment in cents: a tenth of the remaining gap (so
/// steps decay as the figure closes in), floored at one cent so the last dime
/// ticks penny by penny onto the exact target, and clamped so the walk never
/// overshoots. The gap is recomputed from the live `current` each tick — the
/// decelerating, slot-machine feel; an equal-steps variant would capture the
/// gap once at observe time instead.
fn step_cents(current: u64, target: u64) -> u64 {
    let gap = target.saturating_sub(current);
    (gap / 10).max(1).min(gap)
}

/// Walk the stepped climb `ticks` ticks from `from` toward `target`, in cents.
/// Past [`MAX_TICKS`] the walk snaps to the target (see the cap's contract).
fn walk_cents(from: u64, target: u64, ticks: u64) -> u64 {
    if ticks >= MAX_TICKS {
        return target;
    }
    let mut current = from;
    for _ in 0..ticks {
        if current >= target {
            break;
        }
        current += step_cents(current, target);
    }
    current
}

/// One animated scalar — where it is painted versus where the data points.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Roll {
    /// The painted value in cents when the current roll began — the stepped
    /// climb's start.
    from_cents: u64,
    /// The value in cents the data says we should reach.
    target_cents: u64,
    /// Animation phase the current roll began at; `None` until the first value.
    start_phase: Option<u64>,
}

impl Roll {
    /// Fold in the latest `target`. An increase starts a stepped climb from the
    /// value painted right now (so an interrupted climb continues, never jumps);
    /// a decrease or the first-ever value snaps.
    fn observe(&mut self, target: f64, phase: u64) {
        let target_cents = to_cents(target);
        let snap = match self.start_phase {
            None => true,
            Some(_) => target_cents <= self.target_cents,
        };
        self.from_cents = if snap {
            target_cents
        } else {
            self.value_at_cents(phase)
        };
        self.target_cents = target_cents;
        self.start_phase = Some(phase);
    }

    /// The value to paint at `phase`, stepping toward the authoritative
    /// `target` from the snapshot. Mid-climb it walks the decaying steps from
    /// where the figure was painted when the climb began; unseeded (no
    /// `observe` has run — a one-off `draw`, or a test), snapped, or settled,
    /// it is `target` itself. Reading the live target here keeps the corner
    /// correct even on a render path that never folded a roll, with the roll
    /// supplying only the transition. Pure: render reads it without mutating,
    /// so a frame recomputes at any phase.
    pub(crate) fn display(&self, target: f64, phase: u64) -> f64 {
        let target_cents = to_cents(target);
        match self.start_phase {
            Some(start) if self.from_cents < target_cents => {
                let walked = walk_cents(self.from_cents, target_cents, phase.saturating_sub(start));
                walked as f64 / 100.0
            }
            // Unseeded, a snap, or a roll long settled: paint the true figure.
            _ => target,
        }
    }

    /// The painted cents at `phase` against the roll's own stored target — the
    /// start point `observe` captures when a fresh climb interrupts one already
    /// in flight.
    fn value_at_cents(&self, phase: u64) -> u64 {
        match self.start_phase {
            Some(start) => walk_cents(
                self.from_cents,
                self.target_cents,
                phase.saturating_sub(start),
            ),
            None => self.target_cents,
        }
    }

    /// Ticks the stepped climb needs to land the roll's start exactly on its
    /// target — the derived settle point that times the flash and the
    /// tick-gate, where the eased model had a fixed frame count.
    fn ticks_to_settle(&self) -> u64 {
        let mut current = self.from_cents;
        let mut ticks = 0;
        while current < self.target_cents && ticks < MAX_TICKS {
            current += step_cents(current, self.target_cents);
            ticks += 1;
        }
        ticks
    }

    /// Whether this roll still needs the fast animation tick — through the climb,
    /// the brief flash, and one trailing clean frame, so the last frame painted
    /// is the settled value rather than a stuck brighten. A snap (`from ==
    /// target`) has no motion, so it never holds the tick.
    fn rolling(&self, phase: u64) -> bool {
        self.from_cents < self.target_cents
            && self
                .elapsed(phase)
                .is_some_and(|e| e <= self.ticks_to_settle() + FLASH_FRAMES)
    }

    /// Within the brief brighten window just after the figure lands. A snap never
    /// flashes — only a genuine climb earns the "ka-chunk".
    pub(crate) fn flashing(&self, phase: u64) -> bool {
        if self.from_cents >= self.target_cents {
            return false;
        }
        let settle = self.ticks_to_settle();
        self.elapsed(phase)
            .is_some_and(|e| (settle..settle + FLASH_FRAMES).contains(&e))
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

/// One stepped roll per agent card's session `$cost`, keyed by the row's
/// durable id (the agent id), so a status-churn reorder or a refresh
/// re-anchors a climb to the same agent. Folded on each data refresh next to
/// the cockpit tally; an id no fold has observed displays the live target
/// as-is, so a one-off draw or a test paints the corner correctly with no roll
/// seeded.
#[derive(Clone, Debug, Default)]
pub(crate) struct CostRolls {
    rolls: HashMap<String, Roll>,
}

impl CostRolls {
    /// Fold the snapshot's per-row costs: observe each agent row's session cost
    /// under its row id, then drop the ids the snapshot no longer carries, so
    /// the map tracks the live rows and never grows across a long session.
    pub(crate) fn observe(&mut self, costs: impl Iterator<Item = (String, f64)>, phase: u64) {
        let mut seen = HashSet::new();
        for (id, usd) in costs {
            self.rolls
                .entry(id.clone())
                .or_default()
                .observe(usd, phase);
            seen.insert(id);
        }
        self.rolls.retain(|id, _| seen.contains(id));
    }

    /// The value to paint for `id` at `phase` — the stepped climb toward the
    /// authoritative `target`, or `target` itself for an unobserved id.
    pub(crate) fn display(&self, id: &str, target: f64, phase: u64) -> f64 {
        self.rolls
            .get(id)
            .map_or(target, |roll| roll.display(target, phase))
    }

    /// Whether `id`'s figure is inside its brief settle brighten.
    pub(crate) fn flashing(&self, id: &str, phase: u64) -> bool {
        self.rolls.get(id).is_some_and(|roll| roll.flashing(phase))
    }

    /// Whether any card figure is still mid-roll — ORed into the serve loop's
    /// animation gate beside the cockpit tally's.
    pub(crate) fn any_rolling(&self, phase: u64) -> bool {
        self.rolls.values().any(|roll| roll.rolling(phase))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_value_snaps_then_increase_steps() {
        let mut r = Roll::default();
        r.observe(100.0, 0);
        // First observation snaps — no boot roll from zero.
        assert_eq!(r.value_at_cents(0), 10_000);
        assert!(!r.rolling(0));

        // A genuine increase steps from the painted value to the new target.
        r.observe(200.0, 10);
        assert_eq!(r.value_at_cents(10), 10_000, "starts at the prior value");
        let settle = r.ticks_to_settle();
        let mid = r.value_at_cents(11);
        assert!(mid > 10_000 && mid < 20_000, "mid-climb");
        for offset in 0..=settle + 1 {
            assert!(r.value_at_cents(10 + offset) <= 20_000, "never overshoots");
        }
        assert_eq!(r.value_at_cents(10 + settle), 20_000, "lands exactly");
    }

    #[test]
    fn decrease_snaps_without_a_countdown() {
        let mut r = Roll::default();
        r.observe(40.0, 0);
        r.observe(0.5, 5); // UTC-midnight today reset
        assert_eq!(r.value_at_cents(5), 50, "snaps down, never rolls backward");
        assert!(!r.rolling(5));
    }

    #[test]
    fn big_steps_then_pennies() {
        let mut r = Roll::default();
        r.observe(0.0, 0);
        r.observe(10.0, 0);
        let settle = r.ticks_to_settle();
        let values: Vec<u64> = (0..=settle).map(|p| r.value_at_cents(p)).collect();
        let steps: Vec<u64> = values.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(steps[0], 100, "first step is a tenth of the gap");
        assert!(
            steps.windows(2).all(|w| w[1] <= w[0]),
            "steps only ever decay"
        );
        assert!(
            steps[steps.len() - 9..].iter().all(|step| *step == 1),
            "the final stretch ticks penny by penny"
        );
        assert_eq!(values[settle as usize], 1_000, "lands exactly on target");
    }

    #[test]
    fn flash_lands_after_the_climb_then_clears() {
        let mut r = Roll::default();
        r.observe(1.0, 0);
        r.observe(2.0, 0); // increase at phase 0
        let settle = r.ticks_to_settle();
        assert!(!r.flashing(0), "no flash mid-climb");
        assert!(r.flashing(settle), "brightens once it lands");
        assert!(r.rolling(settle + FLASH_FRAMES), "one clean trailing frame");
        assert!(
            !r.rolling(settle + FLASH_FRAMES + 1),
            "then releases the tick"
        );
    }

    #[test]
    fn retarget_mid_climb_continues_from_painted() {
        let mut r = Roll::default();
        r.observe(1.0, 0);
        r.observe(5.0, 0);
        let painted = r.value_at_cents(3);
        assert!(painted > 100 && painted < 500, "mid-climb when retargeted");
        r.observe(9.0, 3);
        assert_eq!(
            r.value_at_cents(3),
            painted,
            "continues from the painted value, never jumps"
        );
        let settle = r.ticks_to_settle();
        assert_eq!(r.value_at_cents(3 + settle), 900, "lands on the new target");
    }

    #[test]
    fn cost_rolls_observe_prune_and_fall_back() {
        let mut rolls = CostRolls::default();
        let seed = vec![("a".to_owned(), 1.0), ("b".to_owned(), 2.0)];
        rolls.observe(seed.into_iter(), 0);
        // First observation snaps each id; nothing holds the fast tick.
        assert_eq!(rolls.display("a", 1.0, 0), 1.0);
        assert!(!rolls.any_rolling(0));

        // An increase rolls: the painted value sits strictly mid-climb.
        rolls.observe(vec![("a".to_owned(), 3.0)].into_iter(), 10);
        assert!(rolls.any_rolling(10));
        let mid = rolls.display("a", 3.0, 12);
        assert!(mid > 1.0 && mid < 3.0, "mid-climb between start and target");

        // The fold pruned the departed "b": display falls back to the target.
        assert_eq!(rolls.display("b", 9.99, 12), 9.99);
    }
}
