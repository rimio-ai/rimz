//! Zellij presence-plugin materialization and wakeup pipe helpers.

use std::path::PathBuf;
use std::{env, fs};

use super::{
    PRESENCE_BOOT_PIPE, PRESENCE_PIPE_TIMEOUT, PRESENCE_PLUGIN_MIN_ZELLIJ, ZellijBackend,
    parse_version,
};
use crate::ledger::{atomic, paths};
use crate::mux::{MuxBackend, MuxErr, Result};

const EMBEDDED_PRESENCE_PLUGIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/rimz-presence-zellij.wasm"));
const PRESENCE_PLUGIN_FILE: &str = "rimz-presence-zellij.wasm";

/// Locate the presence-plugin wasm: the `RIMZ_PRESENCE_PLUGIN` override, else
/// the embedded plugin materialized under `$XDG_DATA_HOME/rimz/plugins/`, else
/// a development fallback beside the running executable. `None` leaves the
/// session in poll mode; `rimz doctor` names the missing artifact and the fix.
///
/// Canonical, because Zellij keys the user's one-time permission grant on the
/// exact path string it is handed: one real artifact must read as one string
/// however rimz was invoked (symlinked bin dir, relative exe), or the user is
/// re-prompted per spelling.
pub fn presence_plugin_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RIMZ_PRESENCE_PLUGIN") {
        return PathBuf::from(path)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    if let Some(path) = embedded_presence_plugin_path() {
        return Some(path);
    }
    env::current_exe()
        .ok()?
        .parent()?
        .join(PRESENCE_PLUGIN_FILE)
        .canonicalize()
        .ok()
        .filter(|path| path.is_file())
}

fn embedded_presence_plugin_path() -> Option<PathBuf> {
    match materialize_presence_plugin_bytes(EMBEDDED_PRESENCE_PLUGIN, &paths::data_home()) {
        Ok(Some(path)) => path.canonicalize().ok().filter(|path| path.is_file()),
        Ok(None) => None,
        Err(err) => {
            tracing::debug!(error = %err, "materializing embedded presence plugin failed");
            None
        }
    }
}

pub(super) fn materialized_presence_plugin_path_under(data_root: &std::path::Path) -> PathBuf {
    data_root
        .join("rimz")
        .join("plugins")
        .join(PRESENCE_PLUGIN_FILE)
}

pub(super) fn materialize_presence_plugin_bytes(
    bytes: &[u8],
    data_root: &std::path::Path,
) -> std::result::Result<Option<PathBuf>, atomic::AtomicErr> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let path = materialized_presence_plugin_path_under(data_root);
    if fs::read(&path).is_ok_and(|existing| existing == bytes) {
        return Ok(Some(path));
    }
    atomic::write_bytes_atomically(&path, bytes)?;
    Ok(Some(path))
}

/// The `key=value,key=value` configuration the plugin reads at load. The
/// parse is Zellij's — split on `,` then `=` — so a `rimz` path containing
/// either separator cannot be expressed; `rimz_bin` is omitted and the plugin
/// falls back to `rimz` on the host PATH, while an inexpressible plugin URL
/// disables the runtime focus keybind rather than register a mis-targeted pipe.
/// Workspace ids are `ws_` + hex by construction, always expressible.
pub(super) fn presence_plugin_configuration(opts: &super::super::PresencePluginOptions) -> String {
    let mut configuration = format!("workspace_id={}", opts.workspace_id.as_str());
    let plugin_url = format!("file:{}", opts.wasm.display());
    let focus_destination_expressible = !plugin_url.contains([',', '=']);
    if focus_destination_expressible {
        configuration.push_str(",plugin_url=");
        configuration.push_str(&plugin_url);
    } else {
        tracing::debug!(
            plugin_url,
            "presence plugin URL contains a plugin-configuration separator; the Zellij focus keybind is disabled",
        );
    }
    if opts.session_name.contains([',', '=']) {
        tracing::debug!(
            session = %opts.session_name,
            "session name contains a plugin-configuration separator; command-change shortcut is disabled",
        );
    } else {
        configuration.push_str(",session_name=");
        configuration.push_str(&opts.session_name);
    }
    let bin = opts.rimz_bin.to_string_lossy();
    if bin.contains([',', '=']) {
        tracing::debug!(
            rimz_bin = %bin,
            "rimz path contains a plugin-configuration separator; the plugin resolves `rimz` from PATH instead",
        );
    } else {
        configuration.push_str(",rimz_bin=");
        configuration.push_str(&bin);
    }
    // The focus chord the plugin binds at load. Grammar validation and the
    // user-facing warning live in `cli::register_focus_key` (it runs for both
    // backends); here we only guard the plugin-config separators and let the
    // plugin's own parser skip anything malformed.
    if let Some(focus_key) = opts.focus_key.as_deref() {
        if !focus_destination_expressible {
            tracing::debug!(
                focus_key,
                "the Zellij focus keybind is disabled because the plugin URL is not expressible",
            );
        } else if focus_key.contains([',', '=']) {
            tracing::debug!(
                focus_key,
                "focus_key contains a plugin-configuration separator; the Zellij focus keybind is disabled",
            );
        } else {
            configuration.push_str(",focus_key=");
            configuration.push_str(focus_key);
        }
    }
    configuration
}

impl ZellijBackend {
    pub(super) fn ensure_presence_plugin_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        let parsed = self.version().ok().as_deref().and_then(parse_version);
        if parsed.is_none_or(|v| v < PRESENCE_PLUGIN_MIN_ZELLIJ) {
            tracing::debug!(
                session = %opts.session_name,
                version = ?parsed,
                "zellij below the presence-plugin floor; the producer keeps its pane poll",
            );
            return Ok(());
        }
        let url = format!("file:{}", opts.wasm.display());
        let configuration = presence_plugin_configuration(opts);
        if opts.converge {
            // Reload a *running* plugin in place onto the current wasm —
            // `start-or-reload-plugin` converges a pipe-launched instance
            // (verified on 0.44.3: one instance throughout). It needs a
            // connected client; with none the server drops it silently (exit
            // 0 regardless), and the pipe below still ensures a plugin is
            // loaded — the upgrade then lands on the next attached reload.
            self.zellij_action(&opts.session_name)
                .args([
                    "start-or-reload-plugin".to_owned(),
                    url.clone(),
                    "--configuration".to_owned(),
                    configuration.clone(),
                ])
                .run()?;
        }
        // `zellij pipe --plugin` launches the plugin if absent — the one load
        // verb that works on a clientless session (`start-or-reload-plugin`
        // refuses without a connected client) — and routes a no-op message to
        // it when running, so the call is idempotent per (url, configuration).
        let result = self
            .cmd()
            .args([
                "--session",
                &opts.session_name,
                "pipe",
                "--plugin",
                &url,
                "--plugin-configuration",
                &configuration,
                "--name",
                PRESENCE_BOOT_PIPE,
                "--",
                "load",
            ])
            .run_with_timeout(PRESENCE_PIPE_TIMEOUT);
        match result {
            Ok(_) => Ok(()),
            // The held-CLI kill: the launch is delivered, the plugin is
            // waiting on the user's one-time permission answer (or the
            // session has no client yet to surface it). Expected, not an
            // error — pokes start once the grant lands.
            Err(MuxErr::Timeout { .. }) => {
                tracing::debug!(
                    session = %opts.session_name,
                    "presence boot pipe held past its deadline (permission pending); launch delivered",
                );
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MIN_ZELLIJ_VERSION, PRESENCE_PLUGIN_MIN_ZELLIJ};
    use super::*;

    fn presence_opts(session_name: &str, rimz_bin: &str) -> crate::mux::PresencePluginOptions {
        crate::mux::PresencePluginOptions {
            session_name: session_name.to_owned(),
            workspace_id: crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            wasm: std::path::PathBuf::from("/tmp/rimz-presence-zellij.wasm"),
            rimz_bin: std::path::PathBuf::from(rimz_bin),
            converge: false,
            focus_key: None,
        }
    }

    #[test]
    fn presence_plugin_floor_admits_the_tile_line_only() {
        // The floor is the `zellij-tile` pin: 0.44.x loads, anything older keeps
        // the pane poll (and stays above MIN_ZELLIJ_VERSION for everything else).
        assert!((0, 44, 0) >= PRESENCE_PLUGIN_MIN_ZELLIJ);
        assert!((0, 44, 3) >= PRESENCE_PLUGIN_MIN_ZELLIJ);
        assert!((0, 43, 9) < PRESENCE_PLUGIN_MIN_ZELLIJ);
        assert!(PRESENCE_PLUGIN_MIN_ZELLIJ >= MIN_ZELLIJ_VERSION);
    }
    #[test]
    fn presence_plugin_configuration_pins_workspace_and_rimz() {
        let configuration = presence_plugin_configuration(&presence_opts(
            "rimz-test",
            "/home/user/.cargo/bin/rimz",
        ));
        assert_eq!(
            configuration,
            "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz",
        );
    }

    #[test]
    fn presence_plugin_configuration_omits_inexpressible_fields() {
        for weird in ["/tmp/a,b/rimz", "/tmp/a=b/rimz"] {
            let configuration = presence_plugin_configuration(&presence_opts("rimz-test", weird));
            assert_eq!(
                configuration,
                "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test",
                "{weird} must be omitted, not shipped mis-parsable",
            );
        }
        for weird in ["rimz,test", "rimz=test"] {
            let configuration =
                presence_plugin_configuration(&presence_opts(weird, "/home/user/.cargo/bin/rimz"));
            assert_eq!(
                configuration,
                "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,rimz_bin=/home/user/.cargo/bin/rimz",
                "{weird} must be omitted, not shipped mis-parsable",
            );
        }
    }

    #[test]
    fn presence_plugin_configuration_appends_focus_key() {
        let mut opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");
        opts.focus_key = Some("Alt+p".to_owned());
        assert_eq!(
            presence_plugin_configuration(&opts),
            "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_key=Alt+p",
        );

        // A chord carrying a plugin-config separator is dropped rather than
        // shipped mis-parsable; the plugin keeps poll-only focus behaviour.
        opts.focus_key = Some("Alt=p".to_owned());
        assert_eq!(
            presence_plugin_configuration(&opts),
            "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz",
        );
    }

    #[test]
    fn materialize_presence_plugin_bytes_writes_stable_artifact_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            materialize_presence_plugin_bytes(b"", dir.path())
                .unwrap()
                .is_none()
        );

        let path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
            .unwrap()
            .unwrap();
        assert!(path.ends_with("rimz/plugins/rimz-presence-zellij.wasm"));
        assert_eq!(std::fs::read(&path).unwrap(), b"wasm-bytes");

        let same_path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(same_path, path);

        materialize_presence_plugin_bytes(b"new-bytes", dir.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new-bytes");
    }
}
