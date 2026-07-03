//! Zellij presence-plugin materialization and wakeup pipe helpers.

use std::path::PathBuf;
use std::{env, fs};

use super::{
    PRESENCE_BOOT_PIPE, PRESENCE_PIPE_TIMEOUT, PRESENCE_PLUGIN_MIN_ZELLIJ, PRESENCE_SHARE_PIPE,
    ZellijBackend, parse_version,
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
    configuration.push_str(",focus_follows_mouse=");
    configuration.push_str(if opts.focus_follows_mouse {
        "true"
    } else {
        "false"
    });
    configuration.push_str(",mouse_click_through=");
    configuration.push_str(if opts.mouse_click_through {
        "true"
    } else {
        "false"
    });
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
        if !self.presence_plugin_supported(opts, PRESENCE_BOOT_PIPE) {
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
        match self.pipe_to_presence_plugin(opts, PRESENCE_BOOT_PIPE, "load") {
            Ok(()) => Ok(()),
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

    pub(super) fn share_web_session_for(
        &self,
        opts: &super::super::PresencePluginOptions,
    ) -> Result<()> {
        // Target the same (url, configuration) as the boot path: it launches the
        // plugin if absent and reaches the running instance otherwise.
        self.pipe_to_presence_plugin(opts, PRESENCE_SHARE_PIPE, "share")
    }

    fn pipe_to_presence_plugin(
        &self,
        opts: &super::super::PresencePluginOptions,
        pipe_name: &str,
        payload: &str,
    ) -> Result<()> {
        if !self.presence_plugin_supported(opts, pipe_name) {
            return Ok(());
        }
        // `zellij pipe --plugin` launches the plugin if absent — the one load
        // verb that works on a clientless session (`start-or-reload-plugin`
        // refuses without a connected client) — and routes a no-op message to
        // it when running, so the call is idempotent per (url, configuration).
        let url = format!("file:{}", opts.wasm.display());
        let configuration = presence_plugin_configuration(opts);
        self.cmd()
            .args([
                "--session",
                &opts.session_name,
                "pipe",
                "--plugin",
                &url,
                "--plugin-configuration",
                &configuration,
                "--name",
                pipe_name,
                "--",
                payload,
            ])
            .run_with_timeout(PRESENCE_PIPE_TIMEOUT)
            .map(|_| ())
    }

    fn presence_plugin_supported(
        &self,
        opts: &super::super::PresencePluginOptions,
        pipe_name: &str,
    ) -> bool {
        let parsed = self.version().ok().as_deref().and_then(parse_version);
        if parsed.is_none_or(|v| v < PRESENCE_PLUGIN_MIN_ZELLIJ) {
            tracing::debug!(
                session = %opts.session_name,
                version = ?parsed,
                pipe_name,
                "zellij below the presence-plugin floor; skipping presence pipe",
            );
            return false;
        }
        true
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
            focus_follows_mouse: false,
            mouse_click_through: true,
        }
    }

    #[cfg(unix)]
    fn zellij_shim(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let shim = temp.path().join("zellij");
        let mut file = std::fs::File::create(&shim).expect("create shim");
        file.write_all(script.as_bytes()).expect("write shim");
        let mut perms = file.metadata().expect("shim metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim, perms).expect("chmod shim");
        drop(file);
        (temp, shim)
    }

    #[test]
    fn embedded_presence_plugin_is_present() {
        assert!(!EMBEDDED_PRESENCE_PLUGIN.is_empty());
        assert!(EMBEDDED_PRESENCE_PLUGIN.starts_with(b"\0asm"));
    }

    #[cfg(unix)]
    #[test]
    fn share_web_session_pipes_share_payload_to_presence_plugin() {
        let (temp, shim) = zellij_shim(
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
fi
"#,
        );
        let backend = ZellijBackend::with_program_for_test(&shim);
        let opts = presence_opts("rimz-test", "/home/user/.cargo/bin/rimz");

        backend
            .share_web_session_for(&opts)
            .expect("share session pipe");

        let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read log");
        assert!(
            log.contains("--session rimz-test pipe --plugin file:/tmp/rimz-presence-zellij.wasm"),
            "share should target the presence plugin by session and wasm URL:\n{log}",
        );
        assert!(
            log.contains("--name rimz:share_session -- share"),
            "share should send the runtime web-sharing pipe and payload:\n{log}",
        );
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
    fn presence_plugin_configuration_renders_expressible_fields() {
        type PresenceOpts = crate::mux::PresencePluginOptions;
        type MutatePresence = fn(&mut PresenceOpts);
        struct Case {
            session: &'static str,
            rimz_bin: &'static str,
            mutate: MutatePresence,
            expected: &'static str,
        }

        let cases = [
            Case {
                session: "rimz-test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |_| {},
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
            },
            Case {
                session: "rimz-test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |opts| {
                    opts.focus_follows_mouse = true;
                    opts.mouse_click_through = false;
                },
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=true,mouse_click_through=false",
            },
            Case {
                session: "rimz-test",
                rimz_bin: "/tmp/a,b/rimz",
                mutate: |_| {},
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,focus_follows_mouse=false,mouse_click_through=true",
            },
            Case {
                session: "rimz-test",
                rimz_bin: "/tmp/a=b/rimz",
                mutate: |_| {},
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,focus_follows_mouse=false,mouse_click_through=true",
            },
            Case {
                session: "rimz,test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |_| {},
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
            },
            Case {
                session: "rimz=test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |_| {},
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
            },
            Case {
                session: "rimz-test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |opts| opts.focus_key = Some("Alt+p".to_owned()),
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true,focus_key=Alt+p",
            },
            Case {
                session: "rimz-test",
                rimz_bin: "/home/user/.cargo/bin/rimz",
                mutate: |opts| opts.focus_key = Some("Alt=p".to_owned()),
                expected: "workspace_id=ws_0123456789abcdef01234567,plugin_url=file:/tmp/rimz-presence-zellij.wasm,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz,focus_follows_mouse=false,mouse_click_through=true",
            },
        ];

        for case in cases {
            let mut opts = presence_opts(case.session, case.rimz_bin);
            (case.mutate)(&mut opts);
            assert_eq!(presence_plugin_configuration(&opts), case.expected);
        }
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
