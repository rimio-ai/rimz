//! Shared harness for integration tests. Real tempdir, real ledger files —
//! no in-memory stubs per `docs/contributing/testing.md`.
//!
//! Two entry points, one module each:
//! - [`Env`] (`env`) drives the `rimz` binary out of process (the CLI tier):
//!   XDG roots scoped to a tempdir, the workspace resolved from the project
//!   root, and helpers for the hook/feed/resolver round trips every CLI test
//!   repeats.
//! - [`Harness`] (`harness`) opens a real [`rimz::Ledger`] in process (the
//!   library tier) for tests that drive ledger APIs directly.
//!
//! `payloads` holds the agent hook-payload fixtures and environment probes
//! shared across tiers; `redact` normalizes volatile output for snapshots.

#![allow(dead_code)]

pub mod redact;

mod env;
mod harness;
mod payloads;

pub use env::{Env, af_unix_bind_sandboxed, canonical};
pub use harness::Harness;
pub use payloads::{
    claude_pre_tool_use_payload, codex_permission_payload, example_resolver_script,
    permission_payload, python3_present, skip_preconditions, spawn_example_resolver,
    wait_for_heartbeat,
};
