//! Store-tier suites: the synthetic round-trip over `Store` and the off-lock
//! write-path contract (group-commit publish, debounced sweep, lock-free recovery).

mod round_trip;
mod write_path;
