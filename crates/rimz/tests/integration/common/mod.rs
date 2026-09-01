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
#[cfg(unix)]
pub(crate) mod ssh_trace;
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

pub fn osc_titles(bytes: &[u8]) -> Vec<String> {
    let mut titles = Vec::new();
    let mut cursor = 0;
    while cursor + 4 <= bytes.len() {
        if bytes[cursor] != 0x1b
            || bytes[cursor + 1] != b']'
            || !matches!(bytes[cursor + 2], b'0' | b'1' | b'2')
            || bytes[cursor + 3] != b';'
        {
            cursor += 1;
            continue;
        }

        let payload_start = cursor + 4;
        let mut end = payload_start;
        while end < bytes.len() {
            if bytes[end] == 0x07 {
                titles.push(String::from_utf8_lossy(&bytes[payload_start..end]).into_owned());
                cursor = end + 1;
                break;
            }
            if bytes[end] == 0x1b && bytes.get(end + 1) == Some(&b'\\') {
                titles.push(String::from_utf8_lossy(&bytes[payload_start..end]).into_owned());
                cursor = end + 2;
                break;
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
    }
    titles
}

#[cfg(test)]
mod tests {
    use super::osc_titles;

    #[test]
    fn osc_titles_parses_bel_and_st_terminators() {
        assert_eq!(
            osc_titles(b"noise\x1b]0;zero\x07\x1b]1;one\x1b\\\x1b]2;two\x07tail"),
            ["zero", "one", "two"],
        );
    }
}
