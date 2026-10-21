//! Producer tick budget meter.
//!
//! The meter observes producer work and records diagnostics only; producer ticks
//! proceed unchanged. The budgets live beside the detector and are maintained
//! with the cost map in `docs/internals/performance.md`.
//! Counter deltas are scoped by producer lane so concurrent fetch and cache
//! refresh work attribute their forks and store reads to the loop that caused
//! them. The same consecutive-tick window filters both breach start and
//! recovery, so one cheap tick inside a saturated episode does not flap the
//! diagnostic identity.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::diag::record::{DiagEvent, TickLoop};
use crate::lane::WorkLane;

/// Lifecycle frames are pinned under 1KiB; a 100-agent burst folds about 100KiB,
/// and warm unchanged logs fold zero bytes. Cold catch-up trips one tick only.
const TICK_FOLD_BYTES_BUDGET: u64 = 256 * 1024;
/// Warm produce with fresh inputs is pinned at zero spawns; a host git pass
/// refreshes at most one wave of worktrees, so a backlog spreads over passes
/// rather than bursting past this in one.
const TICK_SPAWN_BUDGET: u64 = 32;
/// Mux subprocess wait budget. Mux waits are accounted separately from
/// in-process work.
const TICK_MUX_WAIT_BUDGET_MS: u64 = 5_000;
/// Consecutive over-budget ticks required before a diagnostic. The streak window
/// filters one-off IO stalls like the health-alert and observer windows do.
const TICK_BUDGET_BREACH_TICKS: u32 = 5;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TickSample {
    wall_ms: u64,
    mux_wait_ms: u64,
    fold_bytes: u64,
    spawns: u64,
}

impl TickSample {
    fn exceeds_budget(self, budget_wall_ms: u64) -> bool {
        self.wall_ms.saturating_sub(self.mux_wait_ms) > budget_wall_ms
            || self.mux_wait_ms > TICK_MUX_WAIT_BUDGET_MS
            || self.fold_bytes > TICK_FOLD_BYTES_BUDGET
            || self.spawns > TICK_SPAWN_BUDGET
    }

    fn max(self, other: Self) -> Self {
        Self {
            wall_ms: self.wall_ms.max(other.wall_ms),
            mux_wait_ms: self.mux_wait_ms.max(other.mux_wait_ms),
            fold_bytes: self.fold_bytes.max(other.fold_bytes),
            spawns: self.spawns.max(other.spawns),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TickStart {
    started_at: Instant,
    bytes_read: u64,
    spawns: u64,
    mux_wait_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct TickMeter {
    tick_loop: TickLoop,
    work_lane: WorkLane,
    budget_wall_ms: u64,
    streak: u32,
    under_streak: u32,
    since_ms: u64,
    last: TickSample,
    worst: TickSample,
    /// Per-step spawns of the streak's pass with the most spawns, so they sum
    /// to `worst.spawns` rather than mixing per-step maxima across passes.
    worst_steps: BTreeMap<String, u64>,
}

/// Spawns per named step of one cache-refresh pass, each measured as the lane's
/// spawn-count delta around the step.
#[derive(Debug, Default)]
pub(crate) struct PassSpawns {
    steps: BTreeMap<String, u64>,
}

impl PassSpawns {
    pub(crate) fn step<T>(&mut self, name: &str, run: impl FnOnce() -> T) -> T {
        let lane = crate::lane::current();
        let before = crate::lane::spawn_count(lane);
        let value = run();
        let spawned = crate::lane::spawn_count(lane).saturating_sub(before);
        if spawned > 0 {
            *self.steps.entry(name.to_owned()).or_default() += spawned;
        }
        value
    }

    pub(crate) fn into_steps(self) -> BTreeMap<String, u64> {
        self.steps
    }
}

impl TickMeter {
    /// The wall budget is one configured data tick. At the default one-second
    /// tick this bounds RimZ in-process work. Mux subprocess wait has its own
    /// fixed budget so mux stalls do not hide real fold/enrich regressions.
    pub(crate) fn new(tick_loop: TickLoop, tick: Duration) -> Self {
        Self {
            tick_loop,
            work_lane: work_lane(tick_loop),
            budget_wall_ms: duration_ms(tick),
            streak: 0,
            under_streak: 0,
            since_ms: 0,
            last: TickSample::default(),
            worst: TickSample::default(),
            worst_steps: BTreeMap::new(),
        }
    }

    pub(crate) fn begin(&self) -> TickStart {
        TickStart {
            started_at: Instant::now(),
            bytes_read: crate::lane::event_log_bytes_read(self.work_lane),
            spawns: crate::lane::spawn_count(self.work_lane),
            mux_wait_ms: crate::lane::mux_wait_ms(self.work_lane),
        }
    }

    /// `steps` is the pass's named-step spawn breakdown; only the cache-refresh
    /// loop keeps it, and the work outside any named step joins it as `other`.
    pub(crate) fn finish(
        &mut self,
        start: TickStart,
        at_ms: u64,
        steps: BTreeMap<String, u64>,
    ) -> Option<DiagEvent> {
        let sample = TickSample {
            wall_ms: duration_ms(start.started_at.elapsed()),
            mux_wait_ms: crate::lane::mux_wait_ms(self.work_lane).saturating_sub(start.mux_wait_ms),
            fold_bytes: crate::lane::event_log_bytes_read(self.work_lane)
                .saturating_sub(start.bytes_read),
            spawns: crate::lane::spawn_count(self.work_lane).saturating_sub(start.spawns),
        };
        let steps = self.complete_steps(sample.spawns, steps);
        self.finish_sample(sample, steps, at_ms)
    }

    fn complete_steps(
        &self,
        spawns: u64,
        mut steps: BTreeMap<String, u64>,
    ) -> BTreeMap<String, u64> {
        if self.tick_loop != TickLoop::CacheRefresh {
            return BTreeMap::new();
        }
        let other = spawns.saturating_sub(steps.values().sum());
        if other > 0 {
            steps.insert("other".to_owned(), other);
        }
        steps
    }

    fn finish_sample(
        &mut self,
        sample: TickSample,
        steps: BTreeMap<String, u64>,
        at_ms: u64,
    ) -> Option<DiagEvent> {
        self.last = sample;
        if !sample.exceeds_budget(self.budget_wall_ms) {
            return self.finish_under_budget(at_ms);
        }

        self.under_streak = 0;
        if self.streak == 0 {
            self.since_ms = at_ms;
            self.worst = sample;
            self.worst_steps = steps;
        } else {
            if sample.spawns > self.worst.spawns {
                self.worst_steps = steps;
            }
            self.worst = self.worst.max(sample);
        }
        self.streak = self.streak.saturating_add(1);

        (self.streak >= TICK_BUDGET_BREACH_TICKS).then(|| self.event(None))
    }

    fn finish_under_budget(&mut self, at_ms: u64) -> Option<DiagEvent> {
        if self.streak < TICK_BUDGET_BREACH_TICKS {
            self.reset();
            return None;
        }
        self.under_streak = self.under_streak.saturating_add(1);
        if self.under_streak < TICK_BUDGET_BREACH_TICKS {
            return None;
        }
        let event = self.event(Some(at_ms.saturating_sub(self.since_ms)));
        self.reset();
        Some(event)
    }

    fn event(&self, recovered_after_ms: Option<u64>) -> DiagEvent {
        DiagEvent::TickBudgetBreach {
            tick_loop: self.tick_loop,
            over_ticks: self.streak,
            last_wall_ms: self.last.wall_ms,
            last_mux_wait_ms: self.last.mux_wait_ms,
            last_fold_bytes: self.last.fold_bytes,
            last_spawns: self.last.spawns,
            wall_ms: self.worst.wall_ms,
            mux_wait_ms: self.worst.mux_wait_ms,
            fold_bytes: self.worst.fold_bytes,
            spawns: self.worst.spawns,
            budget_wall_ms: self.budget_wall_ms,
            budget_mux_wait_ms: TICK_MUX_WAIT_BUDGET_MS,
            budget_fold_bytes: TICK_FOLD_BYTES_BUDGET,
            budget_spawns: TICK_SPAWN_BUDGET,
            since_ms: self.since_ms,
            recovered_after_ms,
            spawns_by_step: self.worst_steps.clone(),
        }
    }

    fn reset(&mut self) {
        self.streak = 0;
        self.under_streak = 0;
        self.since_ms = 0;
        self.last = TickSample::default();
        self.worst = TickSample::default();
        self.worst_steps.clear();
    }
}

pub(crate) fn report(diag: &DiagSink, event: DiagEvent) {
    match &event {
        DiagEvent::TickBudgetBreach {
            tick_loop,
            over_ticks,
            last_wall_ms,
            last_mux_wait_ms,
            last_fold_bytes,
            last_spawns,
            wall_ms,
            mux_wait_ms,
            fold_bytes,
            spawns,
            recovered_after_ms: None,
            ..
        } if *over_ticks == TICK_BUDGET_BREACH_TICKS => {
            tracing::warn!(
                tick_loop = ?tick_loop,
                last_wall_ms = *last_wall_ms,
                last_mux_wait_ms = *last_mux_wait_ms,
                last_fold_bytes = *last_fold_bytes,
                last_spawns = *last_spawns,
                wall_ms = *wall_ms,
                mux_wait_ms = *mux_wait_ms,
                fold_bytes = *fold_bytes,
                spawns = *spawns,
                "sidebar tick budget breached"
            );
        }
        _ => {}
    }
    diag.emit(event);
}

fn work_lane(tick_loop: TickLoop) -> WorkLane {
    match tick_loop {
        TickLoop::Fetch => WorkLane::Fetch,
        TickLoop::CacheRefresh => WorkLane::CacheRefresh,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(wall_ms: u64, fold_bytes: u64, spawns: u64) -> TickSample {
        sample_with_mux(wall_ms, 0, fold_bytes, spawns)
    }

    fn sample_with_mux(wall_ms: u64, mux_wait_ms: u64, fold_bytes: u64, spawns: u64) -> TickSample {
        TickSample {
            wall_ms,
            mux_wait_ms,
            fold_bytes,
            spawns,
        }
    }

    const DEFAULT_WALL_MS: u64 = 1_000;

    fn meter(tick_loop: TickLoop) -> TickMeter {
        TickMeter::new(tick_loop, Duration::from_secs(1))
    }

    fn over_wall() -> TickSample {
        sample(DEFAULT_WALL_MS + 1, 0, 0)
    }

    fn under() -> TickSample {
        sample(1, 0, 0)
    }

    #[derive(Debug, PartialEq)]
    struct EventFields {
        tick_loop: TickLoop,
        over_ticks: u32,
        last_wall_ms: u64,
        last_mux_wait_ms: u64,
        last_fold_bytes: u64,
        last_spawns: u64,
        wall_ms: u64,
        mux_wait_ms: u64,
        fold_bytes: u64,
        spawns: u64,
        since_ms: u64,
        recovered_after_ms: Option<u64>,
    }

    fn event_fields(event: DiagEvent) -> EventFields {
        match event {
            DiagEvent::TickBudgetBreach {
                tick_loop,
                over_ticks,
                last_wall_ms,
                last_mux_wait_ms,
                last_fold_bytes,
                last_spawns,
                wall_ms,
                mux_wait_ms,
                fold_bytes,
                spawns,
                since_ms,
                recovered_after_ms,
                ..
            } => EventFields {
                tick_loop,
                over_ticks,
                last_wall_ms,
                last_mux_wait_ms,
                last_fold_bytes,
                last_spawns,
                wall_ms,
                mux_wait_ms,
                fold_bytes,
                spawns,
                since_ms,
                recovered_after_ms,
            },
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn under_budget_stream_stays_silent() {
        let mut meter = meter(TickLoop::Fetch);

        for at_ms in 0..10 {
            assert_eq!(meter.finish_sample(under(), BTreeMap::new(), at_ms), None);
        }

        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn short_over_budget_streak_resets_without_record() {
        let mut meter = meter(TickLoop::Fetch);

        for at_ms in 0..TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(
                meter.finish_sample(over_wall(), BTreeMap::new(), u64::from(at_ms)),
                None
            );
        }

        assert_eq!(meter.finish_sample(under(), BTreeMap::new(), 99), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn threshold_consecutive_ticks_emit_active_breach_with_worst_values() {
        let mut meter = meter(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(
                meter.finish_sample(
                    sample(DEFAULT_WALL_MS + u64::from(at_ms), 0, 1),
                    BTreeMap::new(),
                    u64::from(at_ms)
                ),
                None
            );
        }

        let event = meter
            .finish_sample(
                sample(
                    DEFAULT_WALL_MS + 1,
                    TICK_FOLD_BYTES_BUDGET + 5,
                    TICK_SPAWN_BUDGET + 2,
                ),
                BTreeMap::new(),
                14,
            )
            .expect("threshold breach");

        assert_eq!(
            event_fields(event),
            EventFields {
                tick_loop: TickLoop::Fetch,
                over_ticks: TICK_BUDGET_BREACH_TICKS,
                last_wall_ms: DEFAULT_WALL_MS + 1,
                last_mux_wait_ms: 0,
                last_fold_bytes: TICK_FOLD_BYTES_BUDGET + 5,
                last_spawns: TICK_SPAWN_BUDGET + 2,
                wall_ms: DEFAULT_WALL_MS + 13,
                mux_wait_ms: 0,
                fold_bytes: TICK_FOLD_BYTES_BUDGET + 5,
                spawns: TICK_SPAWN_BUDGET + 2,
                since_ms: 10,
                recovered_after_ms: None,
            }
        );
    }

    #[test]
    fn persistent_breach_keeps_emitting_active_records() {
        let mut meter = meter(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(over_wall(), BTreeMap::new(), u64::from(at_ms));
        }

        let event = meter
            .finish_sample(sample(DEFAULT_WALL_MS + 50, 0, 0), BTreeMap::new(), 20)
            .expect("persistent breach");

        assert_eq!(
            event_fields(event),
            EventFields {
                tick_loop: TickLoop::Fetch,
                over_ticks: TICK_BUDGET_BREACH_TICKS + 1,
                last_wall_ms: DEFAULT_WALL_MS + 50,
                last_mux_wait_ms: 0,
                last_fold_bytes: 0,
                last_spawns: 0,
                wall_ms: DEFAULT_WALL_MS + 50,
                mux_wait_ms: 0,
                fold_bytes: 0,
                spawns: 0,
                since_ms: 10,
                recovered_after_ms: None,
            }
        );
    }

    #[test]
    fn recovery_emits_info_once_and_resets() {
        let mut meter = meter(TickLoop::CacheRefresh);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(over_wall(), BTreeMap::new(), u64::from(at_ms));
        }

        for at_ms in 30..30 + TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(
                meter.finish_sample(under(), BTreeMap::new(), u64::from(at_ms)),
                None
            );
        }
        let event = meter
            .finish_sample(
                under(),
                BTreeMap::new(),
                30 + u64::from(TICK_BUDGET_BREACH_TICKS - 1),
            )
            .expect("recovery after active breach");

        assert_eq!(
            event_fields(event),
            EventFields {
                tick_loop: TickLoop::CacheRefresh,
                over_ticks: TICK_BUDGET_BREACH_TICKS,
                last_wall_ms: 1,
                last_mux_wait_ms: 0,
                last_fold_bytes: 0,
                last_spawns: 0,
                wall_ms: DEFAULT_WALL_MS + 1,
                mux_wait_ms: 0,
                fold_bytes: 0,
                spawns: 0,
                since_ms: 10,
                recovered_after_ms: Some(24),
            }
        );
        assert_eq!(meter.finish_sample(under(), BTreeMap::new(), 35), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn active_breach_requires_symmetric_under_budget_recovery_streak() {
        let mut meter = meter(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(
                sample(DEFAULT_WALL_MS + 1, TICK_FOLD_BYTES_BUDGET + 9, 0),
                BTreeMap::new(),
                u64::from(at_ms),
            );
        }

        assert_eq!(meter.finish_sample(under(), BTreeMap::new(), 20), None);

        let mut event = None;
        for at_ms in 21..21 + TICK_BUDGET_BREACH_TICKS {
            event = meter.finish_sample(
                sample(DEFAULT_WALL_MS + 50, 0, 0),
                BTreeMap::new(),
                u64::from(at_ms),
            );
        }
        let event = event.expect("active breach continues after one under-budget tick");

        assert_eq!(
            event_fields(event),
            EventFields {
                tick_loop: TickLoop::Fetch,
                over_ticks: TICK_BUDGET_BREACH_TICKS * 2,
                last_wall_ms: DEFAULT_WALL_MS + 50,
                last_mux_wait_ms: 0,
                last_fold_bytes: 0,
                last_spawns: 0,
                wall_ms: DEFAULT_WALL_MS + 50,
                mux_wait_ms: 0,
                fold_bytes: TICK_FOLD_BYTES_BUDGET + 9,
                spawns: 0,
                since_ms: 10,
                recovered_after_ms: None,
            }
        );
    }

    #[test]
    fn each_metric_can_trip_the_budget() {
        for sample in [
            sample(DEFAULT_WALL_MS + 1, 0, 0),
            sample_with_mux(
                TICK_MUX_WAIT_BUDGET_MS + 1,
                TICK_MUX_WAIT_BUDGET_MS + 1,
                0,
                0,
            ),
            sample(0, TICK_FOLD_BYTES_BUDGET + 1, 0),
            sample(0, 0, TICK_SPAWN_BUDGET + 1),
        ] {
            let mut meter = meter(TickLoop::Fetch);
            let mut event = None;
            for at_ms in 0..TICK_BUDGET_BREACH_TICKS {
                event = meter.finish_sample(sample, BTreeMap::new(), u64::from(at_ms));
            }
            assert!(event.is_some());
        }
    }

    #[test]
    fn mux_wait_under_its_budget_does_not_trip_wall_budget() {
        let mut meter = meter(TickLoop::Fetch);
        let sample = sample_with_mux(DEFAULT_WALL_MS + 1_000, 1_500, 0, 0);
        for at_ms in 0..TICK_BUDGET_BREACH_TICKS {
            assert_eq!(
                meter.finish_sample(sample, BTreeMap::new(), u64::from(at_ms)),
                None
            );
        }
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn wall_budget_tracks_configured_tick_duration() {
        let mut default_tick = TickMeter::new(TickLoop::Fetch, Duration::from_secs(1));
        let mut long_tick = TickMeter::new(TickLoop::Fetch, Duration::from_secs(10));
        let sample = sample(1_500, 0, 0);

        let mut default_event = None;
        let mut long_event = None;
        for at_ms in 0..TICK_BUDGET_BREACH_TICKS {
            default_event = default_tick.finish_sample(sample, BTreeMap::new(), u64::from(at_ms));
            long_event = long_tick.finish_sample(sample, BTreeMap::new(), u64::from(at_ms));
        }

        let default_event = default_event.expect("1.5s exceeds a 1s tick");
        assert!(long_event.is_none());
        match default_event {
            DiagEvent::TickBudgetBreach { budget_wall_ms, .. } => {
                assert_eq!(budget_wall_ms, DEFAULT_WALL_MS);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(long_tick.streak, 0);
    }

    fn steps(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(name, spawns)| ((*name).to_owned(), *spawns))
            .collect()
    }

    fn breakdown(event: DiagEvent) -> BTreeMap<String, u64> {
        match event {
            DiagEvent::TickBudgetBreach { spawns_by_step, .. } => spawns_by_step,
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn breach_and_recovery_carry_the_breakdown_of_the_max_spawn_pass() {
        let mut meter = meter(TickLoop::CacheRefresh);
        let passes = [
            (10, steps(&[("diff_stats", 10)])),
            (40, steps(&[("diff_stats", 30), ("pr_states", 10)])),
            (20, steps(&[("accounts", 20)])),
            (40, steps(&[("pipeline", 40)])),
            (5, steps(&[("budget", 5)])),
        ];
        let mut event = None;
        for (at_ms, (spawns, pass)) in (10..).zip(passes) {
            event = meter.finish_sample(sample(DEFAULT_WALL_MS + 1, 0, spawns), pass, at_ms);
        }
        let event = event.expect("threshold breach");
        let by_step = breakdown(event.clone());
        assert_eq!(
            by_step,
            steps(&[("diff_stats", 30), ("pr_states", 10)]),
            "the first pass to reach the worst spawn count keeps its breakdown"
        );
        let DiagEvent::TickBudgetBreach { spawns, .. } = event else {
            unreachable!();
        };
        assert_eq!(by_step.values().sum::<u64>(), spawns);

        let mut recovery = None;
        for at_ms in 30..30 + TICK_BUDGET_BREACH_TICKS {
            recovery = meter.finish_sample(under(), BTreeMap::new(), u64::from(at_ms));
        }
        assert_eq!(breakdown(recovery.expect("recovery")), by_step);
        assert!(
            meter.worst_steps.is_empty(),
            "recovery resets the breakdown"
        );
    }

    #[test]
    fn only_the_cache_refresh_loop_keeps_a_breakdown_and_names_the_remainder() {
        let named = steps(&[("diff_stats", 30)]);
        let cache = meter(TickLoop::CacheRefresh);
        assert_eq!(
            cache.complete_steps(34, named.clone()),
            steps(&[("diff_stats", 30), ("other", 4)])
        );
        assert_eq!(cache.complete_steps(30, named.clone()), named);
        assert!(meter(TickLoop::Fetch).complete_steps(34, named).is_empty());
    }

    #[test]
    fn pass_spawns_attribute_lane_spawns_to_the_step_that_made_them() {
        crate::lane::set(WorkLane::CacheRefresh);
        let mut pass = PassSpawns::default();
        let value = pass.step("git", || {
            crate::lane::count_spawn();
            crate::lane::count_spawn();
            7
        });
        pass.step("quiet", || ());
        pass.step("git", crate::lane::count_spawn);
        assert_eq!(value, 7);
        assert_eq!(pass.into_steps(), steps(&[("git", 3)]));
    }
}
