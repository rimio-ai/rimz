use std::path::Path;
use std::time::Duration;

use rimz::ids::WorkspaceId;
use rimz::mux::{MuxBackend, SplitPaneOptions, SplitTarget, ZellijBackend, zellij};
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

    let room = LiveZellijSession::new("selfclose");
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let xdg = room.path();
    seed_presence_permissions(xdg, &wasm);

    let layout = self_close_layout(&name, &rimz, xdg);
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");

    let created = room
        .command()
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout_path)
        .bounded_status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");
    room.wait_until_ready();

    let _client = AttachedClient::attach(&room, 80, 24);
    ZellijBackend::with_runtime_dir(xdg)
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id"),
            wasm,
            rimz_bin: rimz,
            converge: false,
            focus_key: None,
            zoom_key: None,
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("load presence plugin for self-close topology");

    wait_for_nonplugin_panes(&room, 2, Duration::from_secs(15));
    wait_for_no_serve_processes(&name, Duration::from_secs(15));
    wait_for_nonplugin_panes(&room, 0, Duration::from_secs(20));

    let heartbeat_dir = xdg
        .join("rimz")
        .join("ws_0123456789abcdef01234567")
        .join("heartbeat");
    wait_for_no_sidebar_heartbeat(&heartbeat_dir, Duration::from_secs(15));
}

#[test]
fn split_pane_close_on_exit_removes_a_failed_command() {
    require_zellij!();

    let room = LiveZellijSession::new("paneclose");
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let xdg = room.path();
    let target = wait_for_pane_count(xdg, room.name(), 1)[0].pane_id.clone();
    let marker_dir = TempDir::new().expect("marker tempdir");
    let marker = marker_dir.path().join("started");

    ZellijBackend::with_runtime_dir(xdg)
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: room.name().to_owned(),
                pane_id: target,
            },
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf started > \"$1\"; exit 7".to_owned(),
                "rimz-close-on-exit".to_owned(),
                marker.to_string_lossy().into_owned(),
            ]),
            title: Some("rimz-close-on-exit".to_owned()),
            close_on_exit: true,
            focus: false,
            ..Default::default()
        })
        .expect("split self-closing pane");

    poll_until(
        Duration::from_secs(10),
        || Ok::<_, String>(marker.exists()),
        |exists| *exists,
        "self-closing command start",
    );
    poll_until(
        Duration::from_secs(10),
        || {
            list_panes(xdg, room.name())
                .map(|snapshot| snapshot.panes.iter().filter(|pane| !pane.is_plugin).count())
        },
        |count| *count == 1,
        "failed split pane removal",
    );
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

fn session_nonplugin_count(session: &LiveZellijSession) -> Result<usize, String> {
    let name = session.name();
    let output = session
        .command()
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

fn wait_for_nonplugin_panes(session: &LiveZellijSession, target: usize, timeout: Duration) {
    poll_until(
        timeout,
        || session_nonplugin_count(session),
        |count| *count == target,
        &format!("{target} non-plugin panes in {}", session.name()),
    );
}
