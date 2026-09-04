//! Durable file mechanics shared across RimZ modules.
//!
//! This module imports only `ids` and `sock`; every fsync lives in
//! `atomic.rs`.

pub mod atomic;
pub mod lock;
pub(crate) mod parse_cache;
pub mod paths;
pub mod single_flight;
