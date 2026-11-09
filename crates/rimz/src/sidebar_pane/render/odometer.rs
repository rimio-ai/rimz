//! The money count-up animation — the cockpit headline spend and each
//! agent card's session cost.
//!
//! The animated figure is a [`Roll`]: it remembers where it is painted on
//! screen and where the latest snapshot says it should be, then sweeps between
//! the two along an ease-out cubic inside a fixed window — every jump, three
//! cents or five dollars, lands within [`CLIMB_CLICKS`] clicks on the default
//! 200ms beat. The curve front-loads the motion and flattens into the landing, so
//! big first steps decay into penny-sized last clicks onto the exact
//! two-decimal value, and a small jump simply lands early once rounding
//! reaches the target. Motion stamps the driver-clock milliseconds
//! ([`super::ViewState::frame_ms`]) and counts base frames on the current grid,
//! never the age of fetched data, so a roll plays smoothly even when the data
//! behind it is stale, per the render-thread performance contract.
//!
//! A roll fires only on an *increase*: a decrease (today's UTC-midnight reset)
//! and the first observed value both snap, so a figure never plays a sad
//! count-down or a dramatic `0 → today` roll on boot, and an *unchanged*
//! target is a no-op — refolds land on every store wakeup, and re-anchoring
//! the roll would snap a climb in flight and erase the settle flash
//! mid-window. The provider dashboard's W/M store rows are deliberately
//! static — only the cockpit headline and the per-card costs climb.

use std::collections::{HashMap, HashSet};

use super::FrameTime;

/// Base frames per roll click on the current refresh grid. The roll clicks on
/// every second base frame, so the default 100ms grid yields a 200ms click —
/// and a room where only money moves rides the serve loop's matching money
/// grid rather than the fast one.
pub(super) const CLICK_PHASES: u64 = 2;

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
pub(in crate::sidebar_pane) struct Roll {
    /// The painted value in cents when the current roll began — the eased
    /// sweep's start.
    from_cents: u64,
    /// The value in cents the data says we should reach.
    target_cents: u64,
    /// Frame time the current roll began at; `None` until the first value.
    started_ms: Option<FrameTime>,
}

impl Roll {
    /// Fold in the latest `target`. An increase starts an eased sweep from the
    /// value painted right now (so an interrupted climb continues, never jumps);
    /// an *unchanged* target is a no-op — refolds land on every store wakeup,
    /// and re-anchoring would snap a climb in flight and erase the settle flash
    /// mid-window; a decrease or the first-ever value snaps.
    fn observe(&mut self, target: f64, now: FrameTime, refresh_ms: u16) {
        let target_cents = to_cents(target);
        if self.started_ms.is_some() && target_cents == self.target_cents {
            return;
        }
        let snap = match self.started_ms {
            None => true,
            Some(_) => target_cents < self.target_cents,
        };
        self.from_cents = if snap {
            target_cents
        } else {
            self.value_at_cents(now, refresh_ms)
        };
        self.target_cents = target_cents;
        self.started_ms = Some(now);
    }

    /// The value to paint at `now`, sweeping toward the authoritative
    /// `target` from the snapshot. Mid-climb it reads the ease-out curve from
    /// where the figure was painted when the climb began; unseeded (no
    /// `observe` has run — a one-off `draw`, or a test), snapped, or settled,
    /// it is `target` itself. Reading the live target here keeps the corner
    /// correct even on a render path that never folded a roll, with the roll
    /// supplying only the transition. Pure: render reads it without mutating,
    /// so a frame recomputes at any sample.
    pub(in crate::sidebar_pane) fn display(
        &self,
        target: f64,
        now: FrameTime,
        refresh_ms: u16,
    ) -> f64 {
        let target_cents = to_cents(target);
        match self.started_ms {
            Some(start) if self.from_cents < target_cents => {
                let clicks = now.elapsed_frames_since(start, refresh_ms) / CLICK_PHASES;
                eased_cents(self.from_cents, target_cents, clicks) as f64 / 100.0
            }
            // Unseeded, a snap, or a roll long settled: paint the true figure.
            _ => target,
        }
    }

    /// The painted cents at `now` against the roll's own stored target — the
    /// start point `observe` captures when a fresh climb interrupts one already
    /// in flight.
    fn value_at_cents(&self, now: FrameTime, refresh_ms: u16) -> u64 {
        match self.started_ms {
            Some(start) => eased_cents(
                self.from_cents,
                self.target_cents,
                now.elapsed_frames_since(start, refresh_ms) / CLICK_PHASES,
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
    fn rolling(&self, now: FrameTime, refresh_ms: u16) -> bool {
        self.from_cents < self.target_cents
            && self
                .elapsed(now, refresh_ms)
                .is_some_and(|e| e <= self.clicks_to_settle() + FLASH_CLICKS)
    }

    /// Within the brief brighten window just after the figure lands. A snap never
    /// flashes — only a genuine climb earns the "ka-chunk".
    pub(in crate::sidebar_pane) fn flashing(&self, now: FrameTime, refresh_ms: u16) -> bool {
        if self.from_cents >= self.target_cents {
            return false;
        }
        let settle = self.clicks_to_settle();
        self.elapsed(now, refresh_ms)
            .is_some_and(|e| (settle..settle + FLASH_CLICKS).contains(&e))
    }

    /// Clicks elapsed since the climb began — base frames on the current
    /// [`CLICK_PHASES`] grid, so `rolling`/`flashing` reason in the same units
    /// as `clicks_to_settle`.
    fn elapsed(&self, now: FrameTime, refresh_ms: u16) -> Option<u64> {
        self.started_ms
            .map(|start| now.elapsed_frames_since(start, refresh_ms) / CLICK_PHASES)
    }
}

/// The cockpit's one animated figure: today's fleet spend, the headline that
/// climbs as a turn lands. The W/M store rows below read straight from the
/// tally with no roll.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::sidebar_pane) struct TallyAnim {
    pub(in crate::sidebar_pane) today_usd: Roll,
}

impl TallyAnim {
    /// Fold the latest headline-spend target into the roll — the snapshot's live
    /// overlay figure when it carries one, else the walked tally's. Called on
    /// each data refresh that carries a figure; a refresh without one leaves
    /// the roll untouched, so a transient missing snapshot never snaps the
    /// figure to zero.
    pub(in crate::sidebar_pane) fn observe(
        &mut self,
        today_usd: f64,
        now: FrameTime,
        refresh_ms: u16,
    ) {
        self.today_usd.observe(today_usd, now, refresh_ms);
    }

    /// Whether the figure is still mid-roll — the serve loop ORs this into its
    /// animation gate so a finished-turn climb plays even when no agent is
    /// running, then lets the loop fall back to the slow data tick once settled.
    pub(in crate::sidebar_pane) fn any_rolling(&self, now: FrameTime, refresh_ms: u16) -> bool {
        self.today_usd.rolling(now, refresh_ms)
    }
}

/// One eased roll per agent card's resolved `$cost`, keyed by the row's durable
/// id (the agent id), so a status-churn reorder or a refresh re-anchors a climb
/// to the same agent. Folded on each data refresh next to the cockpit tally; an
/// id no fold has observed displays the target as-is, so a one-off draw or a
/// test paints the corner correctly with no roll seeded.
#[derive(Clone, Debug, Default)]
pub(in crate::sidebar_pane) struct CostRolls {
    rolls: HashMap<String, Roll>,
}

impl CostRolls {
    /// Fold the snapshot's resolved per-row costs under each row id, then drop
    /// ids the snapshot no longer carries, so the map tracks the live rows and
    /// never grows across a long session.
    pub(in crate::sidebar_pane) fn observe(
        &mut self,
        costs: impl Iterator<Item = (String, f64)>,
        now: FrameTime,
        refresh_ms: u16,
    ) {
        let mut seen = HashSet::new();
        for (id, usd) in costs {
            self.rolls
                .entry(id.clone())
                .or_default()
                .observe(usd, now, refresh_ms);
            seen.insert(id);
        }
        self.rolls.retain(|id, _| seen.contains(id));
    }

    /// The value to paint for `id` at `now` — the eased sweep toward the
    /// authoritative `target`, or `target` itself for an unobserved id.
    pub(in crate::sidebar_pane) fn display(
        &self,
        id: &str,
        target: f64,
        now: FrameTime,
        refresh_ms: u16,
    ) -> f64 {
        self.rolls
            .get(id)
            .map_or(target, |roll| roll.display(target, now, refresh_ms))
    }

    /// Whether `id`'s figure is inside its brief settle brighten.
    pub(in crate::sidebar_pane) fn flashing(
        &self,
        id: &str,
        now: FrameTime,
        refresh_ms: u16,
    ) -> bool {
        self.rolls
            .get(id)
            .is_some_and(|roll| roll.flashing(now, refresh_ms))
    }

    /// Whether any card figure is still mid-roll — ORed into the serve loop's
    /// animation gate beside the cockpit tally's.
    pub(in crate::sidebar_pane) fn any_rolling(&self, now: FrameTime, refresh_ms: u16) -> bool {
        self.rolls
            .values()
            .any(|roll| roll.rolling(now, refresh_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(elapsed_ms: u64) -> FrameTime {
        FrameTime::from_millis(elapsed_ms)
    }

    #[test]
    fn roll_snaps_first_and_decrease_but_sweeps_increases() {
        let mut r = Roll::default();
        r.observe(100.0, ms(0), 100);
        assert_eq!(r.value_at_cents(ms(0), 100), 10_000);
        assert!(!r.rolling(ms(0), 100));

        r.observe(200.0, ms(1000), 100);
        assert_eq!(
            r.value_at_cents(ms(1000), 100),
            10_000,
            "starts at the prior value"
        );
        let settle = r.clicks_to_settle();
        let mid = r.value_at_cents(ms((10 + CLICK_PHASES) * 100), 100);
        assert!(mid > 10_000 && mid < 20_000, "mid-climb");
        for click in 0..=settle + 1 {
            assert!(
                r.value_at_cents(ms((10 + click * CLICK_PHASES) * 100), 100) <= 20_000,
                "never overshoots"
            );
        }
        assert_eq!(
            r.value_at_cents(ms((10 + settle * CLICK_PHASES) * 100), 100),
            20_000,
            "lands exactly"
        );

        r.observe(40.0, ms(0), 100);
        r.observe(0.5, ms(500), 100); // UTC-midnight today reset
        assert_eq!(
            r.value_at_cents(ms(500), 100),
            50,
            "snaps down, never rolls backward"
        );
        assert!(!r.rolling(ms(500), 100));
    }

    #[test]
    fn roll_curve_is_front_loaded_bounded_and_click_quantized() {
        let mut r = Roll::default();
        r.observe(0.0, ms(0), 100);
        r.observe(10.0, ms(0), 100);
        let settle = r.clicks_to_settle();
        let values: Vec<u64> = (0..=settle)
            .map(|click| r.value_at_cents(ms((click * CLICK_PHASES) * 100), 100))
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
            r.observe(0.0, ms(0), 100);
            r.observe(target, ms(0), 100);
            let settle = r.clicks_to_settle();
            assert!(
                settle <= CLIMB_CLICKS,
                "${target} settles within the window"
            );
            assert_eq!(
                r.value_at_cents(ms((settle * CLICK_PHASES) * 100), 100),
                to_cents(target),
                "${target} lands exactly"
            );
        }

        let mut r = Roll::default();
        r.observe(0.0, ms(0), 100);
        r.observe(0.01, ms(0), 100);
        assert!(r.clicks_to_settle() < CLIMB_CLICKS, "a penny lands early");

        let mut r = Roll::default();
        r.observe(0.0, ms(0), 100);
        r.observe(1.0, ms(0), 100);
        for phase in 0..CLICK_PHASES {
            assert_eq!(
                r.value_at_cents(ms(phase * 100), 100),
                0,
                "value holds within a click"
            );
        }
        assert_eq!(
            r.value_at_cents(ms(CLICK_PHASES * 100), 100),
            42,
            "sweeps on the click boundary"
        );
    }

    #[test]
    fn flash_reobserve_and_retarget_keep_the_roll_continuous() {
        let mut r = Roll::default();
        r.observe(1.0, ms(0), 100);
        r.observe(2.0, ms(0), 100); // increase at phase 0
        let settle = r.clicks_to_settle();
        assert!(!r.flashing(ms(0), 100), "no flash mid-climb");
        assert!(
            r.flashing(ms((settle * CLICK_PHASES) * 100), 100),
            "brightens once it lands"
        );
        assert!(
            r.rolling(ms(((settle + FLASH_CLICKS) * CLICK_PHASES) * 100), 100),
            "one clean trailing frame"
        );
        assert!(
            !r.rolling(ms(((settle + FLASH_CLICKS + 1) * CLICK_PHASES) * 100), 100),
            "then releases the tick"
        );

        let painted = r.value_at_cents(ms(CLICK_PHASES * 100), 100);
        assert!(painted > 100 && painted < 200, "mid-climb");

        r.observe(2.0, ms(CLICK_PHASES * 100), 100);
        assert_eq!(
            r.value_at_cents(ms(CLICK_PHASES * 100), 100),
            painted,
            "climb unbroken"
        );
        assert!(
            r.rolling(ms(CLICK_PHASES * 100), 100),
            "still holds the tick"
        );

        let flash_phase = settle * CLICK_PHASES;
        r.observe(2.0, ms(flash_phase * 100), 100);
        assert!(
            r.flashing(ms(flash_phase * 100), 100),
            "flash survives an equal-target refold"
        );
        assert_eq!(
            r.value_at_cents(ms(flash_phase * 100), 100),
            200,
            "settled on target"
        );

        let mut r = Roll::default();
        r.observe(1.0, ms(0), 100);
        r.observe(5.0, ms(0), 100);
        let mid_phase = 3 * CLICK_PHASES;
        let painted = r.value_at_cents(ms(mid_phase * 100), 100);
        assert!(painted > 100 && painted < 500, "mid-climb when retargeted");
        r.observe(9.0, ms(mid_phase * 100), 100);
        assert_eq!(
            r.value_at_cents(ms(mid_phase * 100), 100),
            painted,
            "continues from the painted value, never jumps"
        );
        let settle = r.clicks_to_settle();
        assert_eq!(
            r.value_at_cents(ms((mid_phase + settle * CLICK_PHASES) * 100), 100),
            900,
            "lands on the new target"
        );
    }

    #[test]
    fn cost_rolls_observe_prune_and_fall_back() {
        let mut rolls = CostRolls::default();
        let seed = vec![("a".to_owned(), 1.0), ("b".to_owned(), 2.0)];
        rolls.observe(seed.into_iter(), ms(0), 100);
        // First observation snaps each id; nothing holds the fast tick.
        assert_eq!(rolls.display("a", 1.0, ms(0), 100), 1.0);
        assert!(!rolls.any_rolling(ms(0), 100));

        // An increase rolls: the painted value sits strictly mid-climb.
        rolls.observe(vec![("a".to_owned(), 3.0)].into_iter(), ms(1000), 100);
        assert!(rolls.any_rolling(ms(1000), 100));
        let mid = rolls.display("a", 3.0, ms((10 + CLICK_PHASES) * 100), 100);
        assert!(mid > 1.0 && mid < 3.0, "mid-climb between start and target");

        // The fold pruned the departed "b": display falls back to the target.
        assert_eq!(rolls.display("b", 9.99, ms(1200), 100), 9.99);
    }
}
