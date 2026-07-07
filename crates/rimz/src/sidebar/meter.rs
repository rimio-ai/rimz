//! Producer tick budget meter.
//!
//! The meter observes producer work and records diagnostics only; producer ticks
//! proceed unchanged. The budgets live beside the detector and are maintained
//! with the cost map in `docs/internals/health/performance.md`.
//! Counter deltas are scoped by producer lane so concurrent fetch and cache
//! refresh work attribute their forks and store reads to the loop that caused
//! them. The same consecutive-tick window filters both breach start and
//! recovery, so one cheap tick inside a saturated episode does not flap the
//! diagnostic identity.

use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::diag::record::{DiagEvent, TickLoop};
use crate::lane::WorkLane;

/// Lifecycle frames are pinned under 1KiB; a 100-agent burst folds about 100KiB,
/// and warm unchanged logs fold zero bytes. Cold catch-up trips one tick only.
pub(crate) const TICK_FOLD_BYTES_BUDGET: u64 = 256 * 1024;
/// Warm produce with fresh inputs is pinned at zero spawns; hot git sweeps burst
/// once per diff-stats TTL rather than on consecutive ticks.
pub(crate) const TICK_SPAWN_BUDGET: u64 = 32;
/// Mux subprocess wait budget. Mux waits are accounted separately from
/// in-process work.
pub(crate) const TICK_MUX_WAIT_BUDGET_MS: u64 = 5_000;
/// Consecutive over-budget ticks required before a diagnostic. The streak window
/// filters one-off IO stalls like the health-alert and observer windows do.
pub(crate) const TICK_BUDGET_BREACH_TICKS: u32 = 5;

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
}

impl TickMeter {
    /// The wall budget is one configured data tick. At the default one-second
    /// tick this bounds Rimz in-process work. Mux subprocess wait has its own
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

    pub(crate) fn finish(&mut self, start: TickStart, at_ms: u64) -> Option<DiagEvent> {
        let sample = TickSample {
            wall_ms: duration_ms(start.started_at.elapsed()),
            mux_wait_ms: crate::lane::mux_wait_ms(self.work_lane).saturating_sub(start.mux_wait_ms),
            fold_bytes: crate::lane::event_log_bytes_read(self.work_lane)
                .saturating_sub(start.bytes_read),
            spawns: crate::lane::spawn_count(self.work_lane).saturating_sub(start.spawns),
        };
        self.finish_sample(sample, at_ms)
    }

    fn finish_sample(&mut self, sample: TickSample, at_ms: u64) -> Option<DiagEvent> {
        self.last = sample;
        if !sample.exceeds_budget(self.budget_wall_ms) {
            return self.finish_under_budget(at_ms);
        }

        self.under_streak = 0;
        if self.streak == 0 {
            self.since_ms = at_ms;
            self.worst = sample;
        } else {
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
        }
    }

    fn reset(&mut self) {
        self.streak = 0;
        self.under_streak = 0;
        self.since_ms = 0;
        self.last = TickSample::default();
        self.worst = TickSample::default();
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
            assert_eq!(meter.finish_sample(under(), at_ms), None);
        }

        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn short_over_budget_streak_resets_without_record() {
        let mut meter = meter(TickLoop::Fetch);

        for at_ms in 0..TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(meter.finish_sample(over_wall(), u64::from(at_ms)), None);
        }

        assert_eq!(meter.finish_sample(under(), 99), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn threshold_consecutive_ticks_emit_active_breach_with_worst_values() {
        let mut meter = meter(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(
                meter.finish_sample(
                    sample(DEFAULT_WALL_MS + u64::from(at_ms), 0, 1),
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
            let _ = meter.finish_sample(over_wall(), u64::from(at_ms));
        }

        let event = meter
            .finish_sample(sample(DEFAULT_WALL_MS + 50, 0, 0), 20)
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
            let _ = meter.finish_sample(over_wall(), u64::from(at_ms));
        }

        for at_ms in 30..30 + TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(meter.finish_sample(under(), u64::from(at_ms)), None);
        }
        let event = meter
            .finish_sample(under(), 30 + u64::from(TICK_BUDGET_BREACH_TICKS - 1))
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
        assert_eq!(meter.finish_sample(under(), 35), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn active_breach_requires_symmetric_under_budget_recovery_streak() {
        let mut meter = meter(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(
                sample(DEFAULT_WALL_MS + 1, TICK_FOLD_BYTES_BUDGET + 9, 0),
                u64::from(at_ms),
            );
        }

        assert_eq!(meter.finish_sample(under(), 20), None);

        let mut event = None;
        for at_ms in 21..21 + TICK_BUDGET_BREACH_TICKS {
            event = meter.finish_sample(sample(DEFAULT_WALL_MS + 50, 0, 0), u64::from(at_ms));
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
                event = meter.finish_sample(sample, u64::from(at_ms));
            }
            assert!(event.is_some());
        }
    }

    #[test]
    fn mux_wait_under_its_budget_does_not_trip_wall_budget() {
        let mut meter = meter(TickLoop::Fetch);
        let sample = sample_with_mux(DEFAULT_WALL_MS + 1_000, 1_500, 0, 0);
        for at_ms in 0..TICK_BUDGET_BREACH_TICKS {
            assert_eq!(meter.finish_sample(sample, u64::from(at_ms)), None);
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
            default_event = default_tick.finish_sample(sample, u64::from(at_ms));
            long_event = long_tick.finish_sample(sample, u64::from(at_ms));
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
}
