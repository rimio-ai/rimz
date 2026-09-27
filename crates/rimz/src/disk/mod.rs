//! Durable file mechanics shared across RimZ modules.
//!
//! This module imports only `ids`, `sock` and `utils`; every fsync lives in
//! `atomic.rs`. Rotating JSONL appends are best-effort, skip the per-record
//! fsync, and sync the parent directory only when they create the file.

pub mod atomic;
pub(crate) mod buckets;
pub mod lock;
pub(crate) mod parse_cache;
pub mod paths;
pub mod retention;
pub(crate) mod rotating;
pub(crate) mod single_flight;
pub mod summary;
pub mod usage;
