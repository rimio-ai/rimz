//! Thread-local producer lane tags plus lane-scoped observability counters.
//!
//! The global counters in hot-path test seams stay process-wide for perf gates.
//! The sidebar tick meter reads these lane counters so concurrent fetch and
//! cache-refresh work does not attribute one lane's forks or ledger reads to
//! the other's tick.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkLane {
    Fetch,
    CacheRefresh,
    Other,
}

thread_local! {
    static CURRENT: Cell<WorkLane> = const { Cell::new(WorkLane::Other) };
}

struct Counters {
    spawns: AtomicU64,
    event_log_bytes_read: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            spawns: AtomicU64::new(0),
            event_log_bytes_read: AtomicU64::new(0),
        }
    }
}

static FETCH: Counters = Counters::new();
static CACHE_REFRESH: Counters = Counters::new();
static OTHER: Counters = Counters::new();

impl WorkLane {
    fn counters(self) -> &'static Counters {
        match self {
            Self::Fetch => &FETCH,
            Self::CacheRefresh => &CACHE_REFRESH,
            Self::Other => &OTHER,
        }
    }
}

pub(crate) fn set(lane: WorkLane) {
    CURRENT.with(|current| current.set(lane));
}

pub(crate) fn current() -> WorkLane {
    CURRENT.with(Cell::get)
}

pub(crate) fn count_spawn() {
    current().counters().spawns.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn spawn_count(lane: WorkLane) -> u64 {
    lane.counters().spawns.load(Ordering::Relaxed)
}

pub(crate) fn count_event_log_bytes_read(n: u64) {
    current()
        .counters()
        .event_log_bytes_read
        .fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn event_log_bytes_read(lane: WorkLane) -> u64 {
    lane.counters().event_log_bytes_read.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_tag_is_thread_local() {
        set(WorkLane::Fetch);

        let child = std::thread::spawn(|| {
            assert_eq!(current(), WorkLane::Other);
            set(WorkLane::CacheRefresh);
            current()
        });

        assert_eq!(child.join().unwrap(), WorkLane::CacheRefresh);
        assert_eq!(current(), WorkLane::Fetch);
        set(WorkLane::Other);
    }
}
