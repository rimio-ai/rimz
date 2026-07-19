//! Shared fixtures for the Zellij backend unit tests.
//!
//! The shims stand in for the `zellij` binary: each writes every argv it
//! receives to `zellij.log` beside itself, so a test asserts on the commands the
//! backend issued. [`pane_roster_shim`] covers the common case where the only
//! interesting answer is one `list-panes --all --json` payload.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::ids::WorkspaceId;
use crate::mux::PresencePluginOptions;
use crate::mux::zellij::pane_topology::TopologyWriter;
use crate::mux::zellij::{
    presence_plugin_build, presence_plugin_config_hash, presence_plugin_configuration,
};

/// Write `script` as an executable `zellij` beside a fresh temp dir.
#[cfg(unix)]
pub(crate) fn zellij_shim(script: &str) -> (TempDir, PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let shim = temp.path().join("zellij");
    let mut file = std::fs::File::create(&shim).expect("create shim");
    file.write_all(script.as_bytes()).expect("write shim");
    let mut perms = file.metadata().expect("shim metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod shim");
    (temp, shim)
}

/// A shim that logs every argv, reports a supported version, and runs
/// `list_panes` for `action list-panes --all --json`. Every other verb exits 0.
#[cfg(unix)]
fn shim_answering_list_panes(list_panes: &str) -> (TempDir, PathBuf) {
    zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
case " $* " in
  *" action list-panes --all --json "*)
{list_panes} ;;
esac
exit 0
"#
    ))
}

/// A shim whose pane roster is `panes` — a JSON array literal.
#[cfg(unix)]
pub(crate) fn pane_roster_shim(panes: &str) -> (TempDir, PathBuf) {
    shim_answering_list_panes(&format!("    printf '%s\\n' '{panes}'; exit 0"))
}

/// A shim whose pane listing fails, so callers exercise their degrade path.
#[cfg(unix)]
pub(crate) fn failing_roster_shim() -> (TempDir, PathBuf) {
    shim_answering_list_panes("    exit 1")
}

/// A shim that only records argv — for flows that never list panes.
#[cfg(unix)]
pub(crate) fn logging_shim() -> (TempDir, PathBuf) {
    shim_answering_list_panes("    exit 0")
}

#[cfg(unix)]
pub(crate) fn shim_log(temp: &TempDir) -> String {
    std::fs::read_to_string(temp.path().join("zellij.log")).unwrap_or_default()
}

pub(crate) fn command_count(log: &str, command: &str) -> usize {
    log.lines().filter(|line| line.contains(command)).count()
}

pub(crate) fn presence_opts(session_name: &str, rimz_bin: &str) -> PresencePluginOptions {
    PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        wasm: PathBuf::from("/tmp/rimz-presence-zellij.wasm"),
        rimz_bin: PathBuf::from(rimz_bin),
        converge: false,
        seed_permissions: false,
        focus_key: None,
        focus_follows_mouse: false,
        mouse_click_through: true,
    }
}

/// A writer record carrying this host's build and config identity — what the
/// presence retire path accepts as proof that a replacement plugin is live.
pub(crate) fn current_writer(plugin_id: u32, loaded_at_ms: u64) -> TopologyWriter {
    let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
    let configuration = presence_plugin_configuration(&opts);
    TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: Some(presence_plugin_build().to_owned()),
        config: presence_plugin_config_hash(&configuration).map(str::to_owned),
    }
}
