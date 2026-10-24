//! The loop driver's time domain and permission to execute external effects.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use jiff::Timestamp;

#[derive(Clone)]
pub(super) struct LoopClock {
    base: Instant,
    epoch: Timestamp,
    offset_ms: Option<Arc<AtomicU64>>,
    #[cfg(test)]
    pub(super) focus_calls: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    pub(super) skip_focus: bool,
}

impl LoopClock {
    pub(super) fn wall() -> Self {
        Self {
            base: Instant::now(),
            epoch: Timestamp::now(),
            offset_ms: None,
            #[cfg(test)]
            focus_calls: Arc::default(),
            #[cfg(test)]
            skip_focus: false,
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(super) fn play(epoch: Timestamp) -> Self {
        Self {
            epoch,
            offset_ms: Some(Arc::default()),
            ..Self::wall()
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(super) fn advance(&self, elapsed: Duration) {
        if let Some(offset) = &self.offset_ms {
            offset.fetch_add(super::timing::duration_millis(elapsed), Ordering::Relaxed);
        }
    }

    pub(super) fn now(&self) -> Instant {
        self.offset_ms.as_ref().map_or_else(Instant::now, |offset| {
            self.base + Duration::from_millis(offset.load(Ordering::Relaxed))
        })
    }

    pub(super) fn timestamp(&self) -> Timestamp {
        if self.effects_enabled() {
            Timestamp::now()
        } else {
            self.timestamp_at(self.now())
        }
    }

    pub(super) fn now_ms(&self) -> u64 {
        self.timestamp().as_millisecond().max(0) as u64
    }

    pub(super) fn timestamp_at(&self, now: Instant) -> Timestamp {
        let (base, epoch) = if self.effects_enabled() {
            (Instant::now(), Timestamp::now())
        } else {
            (self.base, self.epoch)
        };
        if now >= base {
            epoch + now.duration_since(base)
        } else {
            epoch - base.duration_since(now)
        }
    }

    pub(super) fn instant_at(&self, now: Timestamp) -> Instant {
        let (base, epoch) = if self.effects_enabled() {
            (Instant::now(), Timestamp::now())
        } else {
            (self.base, self.epoch)
        };
        let delta = now.duration_since(epoch);
        if delta.is_negative() {
            base - delta.unsigned_abs()
        } else {
            base + delta.unsigned_abs()
        }
    }

    pub(super) fn effects_enabled(&self) -> bool {
        self.offset_ms.is_none()
    }

    pub(super) fn focus(&self, execute: impl FnOnce()) {
        if !self.effects_enabled() {
            return;
        }
        #[cfg(test)]
        self.focus_calls.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if self.skip_focus {
            return;
        }
        execute();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::{snapshot, workspace};
    use crate::sidebar_pane::app::layout_freeze::{apply_freeze, arm_freeze};
    use crate::sidebar_pane::render::ViewState;

    #[test]
    fn skewed_wall_epoch_arms_and_expires_the_same_freeze() {
        for skew in [-3_600i64, 3_600] {
            let mut clock = LoopClock::wall();
            clock.epoch += Duration::from_secs(skew.unsigned_abs());
            if skew < 0 {
                clock.epoch -= Duration::from_secs(7_200);
            }
            let mut ui = ViewState::default();
            let mut snapshot = snapshot(&workspace());
            arm_freeze(&mut ui, clock.timestamp_at(clock.now()).as_millisecond());
            apply_freeze(&mut ui, &mut snapshot, false, clock.now_ms() as i64, false);
            assert!(ui.freeze.is_some(), "fresh freeze survives a skewed epoch");
            apply_freeze(
                &mut ui,
                &mut snapshot,
                false,
                clock.now_ms() as i64 + 10_000,
                false,
            );
            assert!(
                ui.freeze.is_none(),
                "freeze expires on the wall-time deadline"
            );
            let converted = clock.instant_at(clock.timestamp());
            let present = clock.now();
            assert!(
                converted.saturating_duration_since(present)
                    + present.saturating_duration_since(converted)
                    < Duration::from_secs(1)
            );
        }
    }
}
