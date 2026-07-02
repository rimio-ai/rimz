//! Producer tick budget meter.
//!
//! The meter observes producer work and records diagnostics only; producer ticks
//! proceed unchanged. The budgets live beside the detector and are maintained
//! with the cost map in `docs/internals/health/performance.md`.
//! Counter deltas are process-global, so concurrent thread work can co-attribute
//! to the observing loop's tick.

use std::time::{Duration, Instant};

use crate::diag::DiagSink;
use crate::schema::diag::{DiagEvent, TickLoop};

/// Above the degraded Zellij `list-panes` ceiling plus fold/enrich work. A
/// producer sustaining past one default data tick cannot hold cadence.
pub(crate) const TICK_WALL_BUDGET: Duration = Duration::from_secs(1);
/// Lifecycle frames are pinned under 1KiB; a 100-agent burst folds about 100KiB,
/// and warm unchanged logs fold zero bytes. Cold catch-up trips one tick only.
pub(crate) const TICK_FOLD_BYTES_BUDGET: u64 = 256 * 1024;
/// Warm produce with fresh inputs is pinned at zero spawns; hot git sweeps burst
/// once per diff-stats TTL rather than on consecutive ticks.
pub(crate) const TICK_SPAWN_BUDGET: u64 = 32;
/// Consecutive over-budget ticks required before a diagnostic. The streak window
/// filters one-off IO stalls like the health-alert and observer windows do.
pub(crate) const TICK_BUDGET_BREACH_TICKS: u32 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TickSample {
    wall_ms: u64,
    fold_bytes: u64,
    spawns: u64,
}

impl TickSample {
    fn exceeds_budget(self) -> bool {
        self.wall_ms > budget_wall_ms()
            || self.fold_bytes > TICK_FOLD_BYTES_BUDGET
            || self.spawns > TICK_SPAWN_BUDGET
    }

    fn max(self, other: Self) -> Self {
        Self {
            wall_ms: self.wall_ms.max(other.wall_ms),
            fold_bytes: self.fold_bytes.max(other.fold_bytes),
            spawns: self.spawns.max(other.spawns),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TickStart {
    started_at: Instant,
    bytes_read: u64,
    spawns: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TickMeter {
    tick_loop: TickLoop,
    streak: u32,
    since_ms: u64,
    worst: TickSample,
}

impl TickMeter {
    pub(crate) fn new(tick_loop: TickLoop) -> Self {
        Self {
            tick_loop,
            streak: 0,
            since_ms: 0,
            worst: TickSample::default(),
        }
    }

    pub(crate) fn begin(&self) -> TickStart {
        TickStart {
            started_at: Instant::now(),
            bytes_read: crate::ledger::event_log::testkit::bytes_read(),
            spawns: crate::proc::testkit::spawn_count(),
        }
    }

    pub(crate) fn finish(&mut self, start: TickStart, at_ms: u64) -> Option<DiagEvent> {
        let sample = TickSample {
            wall_ms: duration_ms(start.started_at.elapsed()),
            fold_bytes: crate::ledger::event_log::testkit::bytes_read()
                .saturating_sub(start.bytes_read),
            spawns: crate::proc::testkit::spawn_count().saturating_sub(start.spawns),
        };
        self.finish_sample(sample, at_ms)
    }

    fn finish_sample(&mut self, sample: TickSample, at_ms: u64) -> Option<DiagEvent> {
        if !sample.exceeds_budget() {
            return self.finish_under_budget(at_ms);
        }

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
        let event = (self.streak >= TICK_BUDGET_BREACH_TICKS)
            .then(|| self.event(Some(at_ms.saturating_sub(self.since_ms))));
        self.reset();
        event
    }

    fn event(&self, recovered_after_ms: Option<u64>) -> DiagEvent {
        DiagEvent::TickBudgetBreach {
            tick_loop: self.tick_loop,
            over_ticks: self.streak,
            wall_ms: self.worst.wall_ms,
            fold_bytes: self.worst.fold_bytes,
            spawns: self.worst.spawns,
            budget_wall_ms: budget_wall_ms(),
            budget_fold_bytes: TICK_FOLD_BYTES_BUDGET,
            budget_spawns: TICK_SPAWN_BUDGET,
            since_ms: self.since_ms,
            recovered_after_ms,
        }
    }

    fn reset(&mut self) {
        self.streak = 0;
        self.since_ms = 0;
        self.worst = TickSample::default();
    }
}

pub(crate) fn report(diag: Option<&DiagSink>, event: DiagEvent) {
    match &event {
        DiagEvent::TickBudgetBreach {
            tick_loop,
            over_ticks,
            wall_ms,
            fold_bytes,
            spawns,
            recovered_after_ms: None,
            ..
        } if *over_ticks == TICK_BUDGET_BREACH_TICKS => {
            tracing::warn!(
                tick_loop = ?tick_loop,
                wall_ms = *wall_ms,
                fold_bytes = *fold_bytes,
                spawns = *spawns,
                "sidebar tick budget breached"
            );
        }
        _ => {}
    }
    if let Some(diag) = diag {
        diag.emit(event);
    }
}

fn budget_wall_ms() -> u64 {
    duration_ms(TICK_WALL_BUDGET)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(wall_ms: u64, fold_bytes: u64, spawns: u64) -> TickSample {
        TickSample {
            wall_ms,
            fold_bytes,
            spawns,
        }
    }

    fn over_wall() -> TickSample {
        sample(budget_wall_ms() + 1, 0, 0)
    }

    fn under() -> TickSample {
        sample(1, 0, 0)
    }

    fn event_fields(event: DiagEvent) -> (TickLoop, u32, u64, u64, u64, u64, Option<u64>) {
        match event {
            DiagEvent::TickBudgetBreach {
                tick_loop,
                over_ticks,
                wall_ms,
                fold_bytes,
                spawns,
                since_ms,
                recovered_after_ms,
                ..
            } => (
                tick_loop,
                over_ticks,
                wall_ms,
                fold_bytes,
                spawns,
                since_ms,
                recovered_after_ms,
            ),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn under_budget_stream_stays_silent() {
        let mut meter = TickMeter::new(TickLoop::Fetch);

        for at_ms in 0..10 {
            assert_eq!(meter.finish_sample(under(), at_ms), None);
        }

        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn short_over_budget_streak_resets_without_record() {
        let mut meter = TickMeter::new(TickLoop::Fetch);

        for at_ms in 0..TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(meter.finish_sample(over_wall(), u64::from(at_ms)), None);
        }

        assert_eq!(meter.finish_sample(under(), 99), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn threshold_consecutive_ticks_emit_active_breach_with_worst_values() {
        let mut meter = TickMeter::new(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS - 1 {
            assert_eq!(
                meter.finish_sample(
                    sample(budget_wall_ms() + u64::from(at_ms), 0, 1),
                    u64::from(at_ms)
                ),
                None
            );
        }

        let event = meter
            .finish_sample(
                sample(
                    budget_wall_ms() + 1,
                    TICK_FOLD_BYTES_BUDGET + 5,
                    TICK_SPAWN_BUDGET + 2,
                ),
                14,
            )
            .expect("threshold breach");

        assert_eq!(
            event_fields(event),
            (
                TickLoop::Fetch,
                TICK_BUDGET_BREACH_TICKS,
                budget_wall_ms() + 13,
                TICK_FOLD_BYTES_BUDGET + 5,
                TICK_SPAWN_BUDGET + 2,
                10,
                None
            )
        );
    }

    #[test]
    fn persistent_breach_keeps_emitting_active_records() {
        let mut meter = TickMeter::new(TickLoop::Fetch);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(over_wall(), u64::from(at_ms));
        }

        let event = meter
            .finish_sample(sample(budget_wall_ms() + 50, 0, 0), 20)
            .expect("persistent breach");

        assert_eq!(
            event_fields(event),
            (
                TickLoop::Fetch,
                TICK_BUDGET_BREACH_TICKS + 1,
                budget_wall_ms() + 50,
                0,
                0,
                10,
                None
            )
        );
    }

    #[test]
    fn recovery_emits_info_once_and_resets() {
        let mut meter = TickMeter::new(TickLoop::CacheRefresh);
        for at_ms in 10..10 + TICK_BUDGET_BREACH_TICKS {
            let _ = meter.finish_sample(over_wall(), u64::from(at_ms));
        }

        let event = meter
            .finish_sample(under(), 30)
            .expect("recovery after active breach");

        assert_eq!(
            event_fields(event),
            (
                TickLoop::CacheRefresh,
                TICK_BUDGET_BREACH_TICKS,
                budget_wall_ms() + 1,
                0,
                0,
                10,
                Some(20)
            )
        );
        assert_eq!(meter.finish_sample(under(), 31), None);
        assert_eq!(meter.streak, 0);
    }

    #[test]
    fn each_metric_can_trip_the_budget() {
        for sample in [
            sample(budget_wall_ms() + 1, 0, 0),
            sample(0, TICK_FOLD_BYTES_BUDGET + 1, 0),
            sample(0, 0, TICK_SPAWN_BUDGET + 1),
        ] {
            let mut meter = TickMeter::new(TickLoop::Fetch);
            let mut event = None;
            for at_ms in 0..TICK_BUDGET_BREACH_TICKS {
                event = meter.finish_sample(sample, u64::from(at_ms));
            }
            assert!(event.is_some());
        }
    }
}
