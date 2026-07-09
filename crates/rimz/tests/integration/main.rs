//! Single integration-test binary for `rimz`.
//!
//! Per `docs/contributing/rust-conventions.md`, every integration test is a
//! module of this one binary rather than a separate `tests/*.rs` target: the
//! shared harness in `common` is declared once, and the workspace links a
//! single test executable instead of one per file. Related suites group under
//! a subdirectory module (`backend`, `examples`, `store`).

mod common;

mod agent_launch;
mod asks;
mod backend;
mod channel;
mod codex_broker;
mod config;
mod coverage;
#[cfg(unix)]
mod daemon_content;
mod doctor;
mod examples;
mod gc;
mod hooks;
mod journey;
mod list;
mod list_pets;
mod list_themes;
mod loop_schedule;
mod message;
mod oauth_usage;
mod performance;
mod presence_wake;
mod proc;
mod reload;
mod remote_attach;
mod reset;
mod resume;
mod run;
mod sidebar_launch;
mod sidebar_snapshot;
mod sidebar_supervisor;
mod sidebar_unread;
mod start;
mod store;
mod transcript;
mod transcript_watch;
mod trust;
mod uninstall;
mod wakeup_pipe;
mod web;
mod workspace;
mod worktree;
mod zellij_health;
mod zellij_socket;
