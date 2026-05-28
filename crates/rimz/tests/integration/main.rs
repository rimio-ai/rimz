//! Single integration-test binary for `rimz`.
//!
//! Per `docs/contributing/rust-conventions.md`, every integration test is a
//! module of this one binary rather than a separate `tests/*.rs` target: the
//! shared harness in `common` is declared once, and the workspace links a
//! single test executable instead of one per file. Related suites group under
//! a subdirectory module (`backend`, `examples`, `ledger`).

mod common;

mod backend;
mod chain_advance;
mod doctor;
mod examples;
mod gc;
mod hooks;
mod journey;
mod ledger;
mod list;
mod property;
mod resolver;
mod sidebar_launch;
mod trust;
mod wakeup_pipe;
mod workspace;
