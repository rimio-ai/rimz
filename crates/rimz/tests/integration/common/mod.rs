//! Shared harness for integration tests. Real tempdir, real store files —
//! no in-memory stubs.
//!
//! Two entry points, one module each:
//! - [`Env`] (`env`) drives the `rimz` binary out of process (the CLI tier):
//!   XDG roots scoped to a tempdir, the workspace resolved from the project
//!   root, and helpers for hook round trips every CLI test repeats.
//! - [`Harness`] (`harness`) opens a real [`rimz::Store`] in process (the
//!   library tier) for tests that drive store APIs directly.
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
    lifecycle_event, permission_payload, pi_tool_call_payload,
};
#[cfg(unix)]
pub use shim::cargo_bin;
#[cfg(unix)]
pub use shim::{
    path_with_front, write_env_dump_shim, write_failing_agent_shim, write_fake_bash_shell,
    write_fake_login_shell, write_hook_firing_agent,
};

pub fn exec_args(request: &rimz::harness::launch::ExecRequest) -> Vec<String> {
    rimz::harness::launch::exec_argv(std::path::Path::new("rimz"), request)
        .expect("encode hidden exec request")
        .into_iter()
        .skip(1)
        .collect()
}
