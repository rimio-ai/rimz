//! The money count-up animation — the cockpit today-spend headline and each
//! agent card's session cost.
//!
//! The animated figure is a [`Roll`]: it remembers where it is painted on
//! screen and where the latest snapshot says it should be, then sweeps between
//! the two along an ease-out cubic inside a fixed window — every jump, three
//! cents or five dollars, lands within [`CLIMB_CLICKS`] clicks on a 200ms
//! beat. The curve front-loads the motion and flattens into the landing, so
//! big first steps decay into penny-sized last clicks onto the exact
//! two-decimal value, and a small jump simply lands early once rounding
//! reaches the target. Motion is driven purely by the wall-clock animation
//! phase ([`super::UiState::animation_phase`]) — never by the age of the
//! fetched data — so a roll plays smoothly even when the data behind it is
//! stale, per the render-thread performance contract.
//!
//! A roll fires only on an *increase*: a decrease (today's UTC-midnight reset)
//! and the first observed value both snap, so a figure never plays a sad
//! count-down or a dramatic `0 → today` roll on boot, and an *unchanged*
//! target is a no-op — refolds land on every ledger wakeup, and re-anchoring
//! the roll would snap a climb in flight and erase the settle flash
//! mid-window. The provider dashboard's W/M ledger rows are deliberately
//! static — only the cockpit headline and the per-card costs climb.

use std::collections::{HashMap, HashSet};

/// Animation phases per roll click. The wall-clock phase advances on the
/// configured base render grid; the roll clicks on every second phase, so the
/// default 100ms grid yields a 200ms click — and a room where only money moves
/// rides the serve loop's matching money grid rather than the fast one.
pub(crate) const CLICK_PHASES: u64 = 2;

/// The fixed climb window: every jump completes within this many clicks
/// (1.2s), the bounded-duration contract that keeps a $5 turn from crawling
/// and a 3¢ nudge from churning. The ease-out curve spends the window —
/// rounding lets a small gap land early.
const CLIMB_CLICKS: u64 = 6;

/// Clicks the figure stays brightened just after it lands — the quiet 200ms
/// "ka-chunk" that makes the climb satisfying without any glyph burst.
const FLASH_CLICKS: u64 = 1;

/// Dollars to integer cents, mirroring `dollars2`'s rounding exactly so the
/// eased sweep and the formatter always agree on the painted figure.
fn to_cents(usd: f64) -> u64 {
    (usd.max(0.0) * 100.0).round() as u64
}

/// The painted cents `clicks` clicks into a climb from `from` toward
/// `target`: an ease-out cubic (`1 − (1−t)³`) over the [`CLIMB_CLICKS`]
/// window, quantized to whole cents. Monotone non-decreasing in `clicks` and
/// clamped to land exactly — at or past the window's end it is `target`
/// itself.
fn eased_cents(from: u64, target: u64, clicks: u64) -> u64 {
    if clicks >= CLIMB_CLICKS {
        return target;
    }
    let t = clicks as f64 / CLIMB_CLICKS as f64;
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (target.saturating_sub(from) as f64 * eased).round() as u64
}

/// One animated scalar — where it is painted versus where the data points.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Roll {
    /// The painted value in cents when the current roll began — the eased
    /// sweep's start.
    from_cents: u64,
    /// The value in cents the data says we should reach.
    target_cents: u64,
    /// Animation phase the current roll began at; `None` until the first value.
    start_phase: Option<u64>,
}

impl Roll {
    /// Fold in the latest `target`. An increase starts an eased sweep from the
    /// value painted right now (so an interrupted climb continues, never jumps);
    /// an *unchanged* target is a no-op — refolds land on every ledger wakeup,
    /// and re-anchoring would snap a climb in flight and erase the settle flash
    /// mid-window; a decrease or the first-ever value snaps.
    fn observe(&mut self, target: f64, phase: u64) {
        let target_cents = to_cents(target);
        if self.start_phase.is_some() && target_cents == self.target_cents {
            return;
        }
        let snap = match self.start_phase {
            None => true,
            Some(_) => target_cents < self.target_cents,
        };
        self.from_cents = if snap {
            target_cents
        } else {
            self.value_at_cents(phase)
        };
        self.target_cents = target_cents;
        self.start_phase = Some(phase);
    }

    /// The value to paint at `phase`, sweeping toward the authoritative
    /// `target` from the snapshot. Mid-climb it reads the ease-out curve from
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
                let clicks = phase.saturating_sub(start) / CLICK_PHASES;
                eased_cents(self.from_cents, target_cents, clicks) as f64 / 100.0
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
            Some(start) => eased_cents(
                self.from_cents,
                self.target_cents,
                phase.saturating_sub(start) / CLICK_PHASES,
            ),
            None => self.target_cents,
        }
    }

    /// Clicks until the quantized sweep first paints the target — at most
    /// [`CLIMB_CLICKS`]; a small gap lands earlier because rounding reaches the
    /// target before the curve does. Times the flash and the tick-gate.
    fn clicks_to_settle(&self) -> u64 {
        (0..CLIMB_CLICKS)
            .find(|&clicks| {
                eased_cents(self.from_cents, self.target_cents, clicks) == self.target_cents
            })
            .unwrap_or(CLIMB_CLICKS)
    }

    /// Whether this roll still needs the animation tick — through the climb,
    /// the brief flash, and one trailing clean frame, so the last frame painted
    /// is the settled value rather than a stuck brighten. A snap (`from ==
    /// target`) has no motion, so it never holds the tick.
    fn rolling(&self, phase: u64) -> bool {
        self.from_cents < self.target_cents
            && self
                .elapsed(phase)
                .is_some_and(|e| e <= self.clicks_to_settle() + FLASH_CLICKS)
    }

    /// Within the brief brighten window just after the figure lands. A snap never
    /// flashes — only a genuine climb earns the "ka-chunk".
    pub(crate) fn flashing(&self, phase: u64) -> bool {
        if self.from_cents >= self.target_cents {
            return false;
        }
        let settle = self.clicks_to_settle();
        self.elapsed(phase)
            .is_some_and(|e| (settle..settle + FLASH_CLICKS).contains(&e))
    }

    /// Clicks elapsed since the climb began — the phase delta on the
    /// [`CLICK_PHASES`] grid, so `rolling`/`flashing` reason in the same units
    /// as `clicks_to_settle`.
    fn elapsed(&self, phase: u64) -> Option<u64> {
        self.start_phase
            .map(|start| phase.saturating_sub(start) / CLICK_PHASES)
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
    /// Fold the latest today-spend target into the roll — the snapshot's live
    /// overlay figure when it carries one, else the walked tally's. Called on
    /// each data refresh that carries a figure; a refresh without one leaves
    /// the roll untouched, so a transient missing snapshot never snaps the
    /// figure to zero.
    pub(crate) fn observe(&mut self, today_usd: f64, phase: u64) {
        self.today_usd.observe(today_usd, phase);
    }

    /// Whether the figure is still mid-roll — the serve loop ORs this into its
    /// animation gate so a finished-turn climb plays even when no agent is
    /// running, then lets the loop fall back to the slow data tick once settled.
    pub(crate) fn any_rolling(&self, phase: u64) -> bool {
        self.today_usd.rolling(phase)
    }
}

/// One eased roll per agent card's session `$cost`, keyed by the row's
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

    /// The value to paint for `id` at `phase` — the eased sweep toward the
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
    fn roll_snaps_first_and_decrease_but_sweeps_increases() {
        let mut r = Roll::default();
        r.observe(100.0, 0);
        assert_eq!(r.value_at_cents(0), 10_000);
        assert!(!r.rolling(0));

        r.observe(200.0, 10);
        assert_eq!(r.value_at_cents(10), 10_000, "starts at the prior value");
        let settle = r.clicks_to_settle();
        let mid = r.value_at_cents(10 + CLICK_PHASES);
        assert!(mid > 10_000 && mid < 20_000, "mid-climb");
        for click in 0..=settle + 1 {
            assert!(
                r.value_at_cents(10 + click * CLICK_PHASES) <= 20_000,
                "never overshoots"
            );
        }
        assert_eq!(
            r.value_at_cents(10 + settle * CLICK_PHASES),
            20_000,
            "lands exactly"
        );

        r.observe(40.0, 0);
        r.observe(0.5, 5); // UTC-midnight today reset
        assert_eq!(r.value_at_cents(5), 50, "snaps down, never rolls backward");
        assert!(!r.rolling(5));
    }

    #[test]
    fn roll_curve_is_front_loaded_bounded_and_click_quantized() {
        let mut r = Roll::default();
        r.observe(0.0, 0);
        r.observe(10.0, 0);
        let settle = r.clicks_to_settle();
        let values: Vec<u64> = (0..=settle)
            .map(|click| r.value_at_cents(click * CLICK_PHASES))
            .collect();
        let steps: Vec<u64> = values.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(steps[0], 421, "the first click covers the ease-out's bulk");
        assert!(
            steps.windows(2).all(|w| w[1] <= w[0]),
            "steps only ever decay"
        );
        assert!(
            *steps.last().expect("a climb has steps") < steps[0] / 10,
            "the landing click is gentle"
        );
        assert_eq!(values[settle as usize], 1_000, "lands exactly on target");

        for target in [0.01, 0.05, 0.20, 1.0, 5.0, 123.45] {
            let mut r = Roll::default();
            r.observe(0.0, 0);
            r.observe(target, 0);
            let settle = r.clicks_to_settle();
            assert!(
                settle <= CLIMB_CLICKS,
                "${target} settles within the window"
            );
            assert_eq!(
                r.value_at_cents(settle * CLICK_PHASES),
                to_cents(target),
                "${target} lands exactly"
            );
        }

        let mut r = Roll::default();
        r.observe(0.0, 0);
        r.observe(0.01, 0);
        assert!(r.clicks_to_settle() < CLIMB_CLICKS, "a penny lands early");

        let mut r = Roll::default();
        r.observe(0.0, 0);
        r.observe(1.0, 0);
        for phase in 0..CLICK_PHASES {
            assert_eq!(r.value_at_cents(phase), 0, "value holds within a click");
        }
        assert_eq!(
            r.value_at_cents(CLICK_PHASES),
            42,
            "sweeps on the click boundary"
        );
    }

    #[test]
    fn flash_reobserve_and_retarget_keep_the_roll_continuous() {
        let mut r = Roll::default();
        r.observe(1.0, 0);
        r.observe(2.0, 0); // increase at phase 0
        let settle = r.clicks_to_settle();
        assert!(!r.flashing(0), "no flash mid-climb");
        assert!(r.flashing(settle * CLICK_PHASES), "brightens once it lands");
        assert!(
            r.rolling((settle + FLASH_CLICKS) * CLICK_PHASES),
            "one clean trailing frame"
        );
        assert!(
            !r.rolling((settle + FLASH_CLICKS + 1) * CLICK_PHASES),
            "then releases the tick"
        );

        let painted = r.value_at_cents(CLICK_PHASES);
        assert!(painted > 100 && painted < 200, "mid-climb");

        r.observe(2.0, CLICK_PHASES);
        assert_eq!(r.value_at_cents(CLICK_PHASES), painted, "climb unbroken");
        assert!(r.rolling(CLICK_PHASES), "still holds the tick");

        let flash_phase = settle * CLICK_PHASES;
        r.observe(2.0, flash_phase);
        assert!(
            r.flashing(flash_phase),
            "flash survives an equal-target refold"
        );
        assert_eq!(r.value_at_cents(flash_phase), 200, "settled on target");

        let mut r = Roll::default();
        r.observe(1.0, 0);
        r.observe(5.0, 0);
        let mid_phase = 3 * CLICK_PHASES;
        let painted = r.value_at_cents(mid_phase);
        assert!(painted > 100 && painted < 500, "mid-climb when retargeted");
        r.observe(9.0, mid_phase);
        assert_eq!(
            r.value_at_cents(mid_phase),
            painted,
            "continues from the painted value, never jumps"
        );
        let settle = r.clicks_to_settle();
        assert_eq!(
            r.value_at_cents(mid_phase + settle * CLICK_PHASES),
            900,
            "lands on the new target"
        );
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
        let mid = rolls.display("a", 3.0, 10 + CLICK_PHASES);
        assert!(mid > 1.0 && mid < 3.0, "mid-climb between start and target");

        // The fold pruned the departed "b": display falls back to the target.
        assert_eq!(rolls.display("b", 9.99, 12), 9.99);
    }
}
