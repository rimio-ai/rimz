//! Store-tier suites: the synthetic round-trip over `Store`, the
//! sidebar wakeup fanout, and the off-lock write-path
//! contract (group-commit publish, debounced sweep, lock-free recovery).

mod round_trip;
mod wakeup;
mod write_path;
