//! Ledger-tier suites: the synthetic round-trip over `Ledger`, the
//! per-request script decision bridge, and the off-lock write-path
//! contract (group-commit publish, debounced sweep, lock-free recovery).

mod bridge;
mod round_trip;
mod write_path;
