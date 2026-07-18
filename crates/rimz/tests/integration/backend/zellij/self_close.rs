use std::path::Path;
use std::time::Duration;

use rimz::ids::WorkspaceId;
use rimz::mux::{MuxBackend, ZellijBackend, zellij};
use tempfile::TempDir;

use crate::common::CommandTimeoutExt;

use super::presence::{presence_wasm_artifact, seed_presence_permissions};
use super::support::*;

fn wait_for_no_serve_processes(session: &str, timeout: Duration) {
    poll_until(
        timeout,
        || serve_processes_for(session),
        |count| *count == 0,
        &format!("sidebar serve process exit for {session}"),
    );
}

#[test]
fn sidebar_self_closes_when_its_tab_empties() {
    require_zellij!();

    let rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    if !rimz.exists() {
        eprintln!("rimz binary not built; skipping self-close test");
        return;
    }
    let Some(wasm) = presence_wasm_artifact() else {
        eprintln!("presence wasm not built (run `cargo xtask build-plugin`); skipping test");
        return;
    };
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|v| v >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    let name = unique_session_name("selfclose");
    let cwd = TempDir::new().expect("cwd tempdir");
    let xdg = scoped_runtime_dir();
    seed_presence_permissions(xdg.path(), &wasm);
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    let layout = self_close_layout(&name, &rimz, xdg.path());
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");

    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout_path)
        .bounded_status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    let session = ZellijSession::attach_existing(xdg, name.clone());
    let xdg = session.xdg.path();
    wait_for_attached_client(xdg, &name);
    ZellijBackend::with_runtime_dir(xdg)
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id"),
            wasm,
            rimz_bin: rimz,
            converge: false,
            seed_permissions: false,
            focus_key: None,
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("load presence plugin for self-close topology");

    wait_for_nonplugin_panes(xdg, &name, 2, Duration::from_secs(15));
    wait_for_no_serve_processes(&name, Duration::from_secs(15));
    wait_for_nonplugin_panes(xdg, &name, 0, Duration::from_secs(20));

    let heartbeat_dir = xdg
        .join("rimz")
        .join("ws_0123456789abcdef01234567")
        .join("heartbeat");
    wait_for_no_sidebar_heartbeat(&heartbeat_dir, Duration::from_secs(15));
}

fn wait_for_no_sidebar_heartbeat(dir: &Path, timeout: Duration) {
    poll_until(
        timeout,
        || match std::fs::read_dir(dir) {
            Ok(entries) => Ok(entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.starts_with("sidebar.") && name.ends_with(".json"))
                .collect::<Vec<_>>()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(format!("read {}: {err}", dir.display())),
        },
        Vec::is_empty,
        "sidebar heartbeat removal",
    );
}

fn self_close_layout(session: &str, rimz: &Path, xdg: &Path) -> String {
    let q = |s: String| serde_json::to_string(&s).expect("kdl escape");
    let serve = sidebar_serve_command_with_tick(session, rimz, xdg, 2);
    format!(
        r#"layout {{
    default_tab_template split_direction="vertical" {{
        pane size="30%" name="rimz-sidebar" {{
            command "sh"
            args "-c" {serve}
            close_on_exit true
        }}
        children
    }}
        tab name="rimz" {{
        pane focus=true {{
            command "sleep"
            args "6"
            close_on_exit true
        }}
    }}
}}
"#,
        serve = q(serve),
    )
}

fn sidebar_serve_command_with_tick(
    session: &str,
    rimz: &Path,
    xdg: &Path,
    tick_seconds: u64,
) -> String {
    format!(
        "HOME={xdg} XDG_CONFIG_HOME={xdg} XDG_STATE_HOME={xdg} XDG_RUNTIME_DIR={xdg} \
         RIMZ_BIN={rimz} \
         exec {rimz} sidebar serve --mux zellij --workspace-id ws_0123456789abcdef01234567 \
         --session-name {session} --tick-seconds {tick_seconds}",
        xdg = xdg.display(),
        rimz = rimz.display(),
    )
}

fn session_nonplugin_count(xdg: &Path, name: &str) -> Result<usize, String> {
    let output = scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .map_err(|err| format!("list panes for {name}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("There is no active session") {
            return Ok(0);
        }
        return Err(format!(
            "list panes for {name} exited {}: {}",
            output.status, stderr,
        ));
    }
    let panes: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("parse panes for {name}: {err}"))?;
    let panes = panes
        .as_array()
        .ok_or_else(|| format!("pane listing for {name} was not an array"))?;
    Ok(panes
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|b| b.as_bool()) == Some(false)
                && pane.get("is_suppressed").and_then(|b| b.as_bool()) != Some(true)
        })
        .count())
}

fn wait_for_nonplugin_panes(xdg: &Path, name: &str, target: usize, timeout: Duration) {
    poll_until(
        timeout,
        || session_nonplugin_count(xdg, name),
        |count| *count == target,
        &format!("{target} non-plugin panes in {name}"),
    );
}
