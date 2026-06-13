//! Single integration-test binary for `rimz`.
//!
//! Per `docs/contributing/rust-conventions.md`, every integration test is a
//! module of this one binary rather than a separate `tests/*.rs` target: the
//! shared harness in `common` is declared once, and the workspace links a
//! single test executable instead of one per file. Related suites group under
//! a subdirectory module (`backend`, `examples`, `ledger`).

mod common;

mod agent_launch;
mod backend;
mod chain_advance;
mod codex_broker;
mod config;
mod doctor;
mod examples;
mod feed_runtime;
mod gc;
mod hooks;
mod journey;
mod ledger;
mod list;
mod list_themes;
mod message_queue;
mod oauth_usage;
mod performance;
mod presence_wake;
mod proc;
mod reload;
mod remote_attach;
mod reset;
mod resolver;
mod run;
mod sidebar_launch;
mod sidebar_snapshot;
mod start;
mod transcript_watch;
mod trust;
mod wakeup_pipe;
mod workspace;
mod worktree;
mod zellij_health;
mod zellij_socket;
