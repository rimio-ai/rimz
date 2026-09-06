//! Durable file mechanics shared across RimZ modules.
//!
//! This module imports only `ids` and `sock`; every fsync lives in
//! `atomic.rs`. Rotating JSONL appends are best-effort and never fsynced.

pub mod atomic;
pub mod lock;
pub(crate) mod parse_cache;
pub mod paths;
pub(crate) mod rotating;
pub mod single_flight;
pub mod usage;
