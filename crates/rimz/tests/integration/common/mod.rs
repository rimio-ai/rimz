//! Shared harness for integration tests. Real tempdir, real store files —
//! no in-memory stubs.
//!
//! Two entry points, one module each:
//! - [`Env`] (`env`) drives the `rimz` binary out of process (the CLI tier):
//!   host state and both mux namespaces scoped to tempdirs, the workspace
//!   resolved from the project root, and repeated hook round-trip helpers.
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
mod zellij;

#[cfg(unix)]
use std::time::Duration;

pub use command::{CommandTimeoutExt, ROOM_WORKFLOW_TIMEOUT, ScrubSessionEnvExt};
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
pub use zellij::ZellijNamespace;

#[cfg(unix)]
pub fn daemon_test_guard() -> rimz::store::lock::WorkspaceLock {
    // Nextest gives every test its own process, so an in-process mutex leaves
    // the machine-wide listener and ttyd's ephemeral stock-index listener
    // contended. `cargo xtask test` gives the whole run one shared TMPDIR,
    // making this lock run-wide and isolated across runs.
    let path = std::env::temp_dir().join("rimz-web-daemon-tests.lock");
    rimz::store::lock::WorkspaceLock::acquire_with_timeout(&path, Duration::from_secs(120))
        .unwrap_or_else(|err| panic!("acquire web daemon test lock {}: {err}", path.display()))
}

pub fn exec_args(request: &rimz::harness::launch::ExecRequest) -> Vec<String> {
    rimz::harness::launch::exec_argv(std::path::Path::new("rimz"), request)
        .expect("encode hidden exec request")
        .into_iter()
        .skip(1)
        .collect()
}
