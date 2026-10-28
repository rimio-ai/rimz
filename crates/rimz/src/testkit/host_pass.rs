//! Testkit-only work counters at host I/O and attachment decision boundaries.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Counter {
    ProcStat,
    ProcCmdline,
    ProcComm,
    ProcChildren,
    ProcTaskDir,
    ProcCwd,
    ProcIo,
    ProcStatus,
    ProcBootStat,
    WakePublication,
    WakeRoomEvent,
    WakeInput,
    WakeClose,
    WakeHint,
    WakeFocus,
    WakeDisconnected,
    TimeoutIdle,
    TimeoutFrame,
    TimeoutWatchdog,
    TimeoutFreeze,
    TimeoutFocusDwell,
    TimeoutTabRead,
    TimeoutWidth,
    TimeoutCaps,
    TimeoutGate,
    DrainClone,
    DrainPreview,
    HeartbeatWrite,
    HeartbeatRenewal,
    HeartbeatFailure,
    ParkRead,
    HostFold,
}

const COUNTERS: usize = Counter::HostFold as usize + 1;

/// Monotonic event counts, either process-wide or belonging to one thread.
pub struct Counters([AtomicU64; COUNTERS]);

impl Counters {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; COUNTERS])
    }

    pub fn get(&self, counter: Counter) -> u64 {
        self.0[counter as usize].load(Ordering::Relaxed)
    }

    /// Reset only while the measured work is quiescent; use deltas for a running thread.
    pub fn reset(&self) {
        for counter in &self.0 {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

pub static TOTAL: Counters = Counters::new();

thread_local! {
    static THREAD: Arc<Counters> = Arc::new(Counters::new());
    static TIMEOUT: Cell<Counter> = const { Cell::new(Counter::TimeoutIdle) };
}

/// Keep this handle to inspect a worker from another thread, even after it exits.
pub fn thread_counters() -> Arc<Counters> {
    THREAD.with(Arc::clone)
}

pub(crate) fn count(counter: Counter) {
    add(counter, 1);
}

pub(crate) fn add(counter: Counter, amount: u64) {
    TOTAL.0[counter as usize].fetch_add(amount, Ordering::Relaxed);
    THREAD.with(|counts| counts.0[counter as usize].fetch_add(amount, Ordering::Relaxed));
}

pub(crate) fn timeout_source() -> Counter {
    TIMEOUT.with(Cell::get)
}

pub(crate) fn set_timeout_source(source: Counter) {
    TIMEOUT.with(|timeout| timeout.set(source));
}

pub(crate) fn cap_timeout(current: std::time::Duration, cap: std::time::Duration, source: Counter) {
    if cap < current {
        set_timeout_source(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_counts_are_isolated_and_totals_include_exited_threads() {
        let local = thread_counters();
        local.reset();
        let total = TOTAL.get(Counter::HostFold);
        let other = std::thread::spawn(|| {
            count(Counter::HostFold);
            thread_counters()
        })
        .join()
        .unwrap();
        assert_eq!(local.get(Counter::HostFold), 0);
        assert_eq!(other.get(Counter::HostFold), 1);
        assert_eq!(TOTAL.get(Counter::HostFold), total + 1);
        other.reset();
        assert_eq!(other.get(Counter::HostFold), 0);
    }
}
