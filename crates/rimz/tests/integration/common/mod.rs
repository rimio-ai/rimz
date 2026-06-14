//! Shared harness for integration tests. Real tempdir, real ledger files —
//! no in-memory stubs.
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
//! shared across tiers.

mod command;
mod env;
mod harness;
mod payloads;
mod shim;

pub use command::{CommandTimeoutExt, ScrubSessionEnvExt};
pub use env::{Env, af_unix_bind_sandboxed, canonical, tmux_pane};
pub use harness::Harness;
pub use payloads::{
    claude_pre_tool_use_payload, codex_permission_payload, codex_pre_tool_use_payload,
    lifecycle_event, permission_payload, pi_tool_call_payload, skip_preconditions,
    spawn_example_resolver, wait_for_heartbeat,
};
#[cfg(unix)]
pub use shim::{
    path_with_front, write_env_dump_shim, write_fake_bash_shell, write_fake_login_shell,
};
