use super::*;

fn wait_for_no_serve_processes(session: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if serve_processes_for(session) == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn sidebar_self_closes_when_its_tab_empties() {
    require_zellij!();

    let rimz = assert_cmd::cargo::cargo_bin("rimz");
    if !rimz.exists() {
        eprintln!("rimz binary not built; skipping self-close test");
        return;
    }

    let name = unique_session_name("selfclose");
    let cwd = TempDir::new().expect("cwd tempdir");
    let xdg = scoped_runtime_dir();
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

    assert!(
        wait_for_nonplugin_panes(xdg, &name, 2, Duration::from_secs(15)),
        "expected sidebar + terminal before self-close for {name}",
    );
    assert!(
        wait_for_no_serve_processes(&name, Duration::from_secs(15)),
        "sidebar serve process did not exit after the terminal left its tab for {name}",
    );
    assert!(
        wait_for_nonplugin_panes(xdg, &name, 0, Duration::from_secs(15)),
        "lone sidebar pane did not close after its renderer exited for {name}",
    );

    let heartbeat_dir = xdg
        .join("rimz")
        .join("ws_0123456789abcdef01234567")
        .join("heartbeat");
    assert!(
        wait_for_no_sidebar_heartbeat(&heartbeat_dir, Duration::from_secs(5)),
        "sidebar heartbeat should be removed on self-close, found: {:?}",
        std::fs::read_dir(&heartbeat_dir)
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
}

fn wait_for_no_sidebar_heartbeat(dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let lingering = std::fs::read_dir(dir)
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("sidebar.") && n.ends_with(".json"))
                })
            })
            .unwrap_or(false);
        if !lingering {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
            args "3"
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

fn session_nonplugin_count(xdg: &Path, name: &str) -> usize {
    scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|panes| {
            panes.as_array().map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        pane.get("is_plugin").and_then(|b| b.as_bool()) == Some(false)
                            && pane.get("is_suppressed").and_then(|b| b.as_bool()) != Some(true)
                    })
                    .count()
            })
        })
        .unwrap_or(0)
}

fn wait_for_nonplugin_panes(xdg: &Path, name: &str, target: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if session_nonplugin_count(xdg, name) == target {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
