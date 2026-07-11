use std::path::PathBuf;

use crate::common::{CommandTimeoutExt, Env};

fn zellij_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

fn ttyd_shim() -> PathBuf {
    crate::common::cargo_bin("ttyd-trace", env!("CARGO_BIN_EXE_ttyd-trace"))
}

#[cfg(unix)]
fn tmux_shim(env: &Env) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let bin_dir = env.home_root.join("web-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir web bin");
    let bin = bin_dir.join("tmux");
    std::fs::write(
        &bin,
        r#"#!/bin/sh
if [ "$1" = "-V" ]; then
  printf 'tmux 3.5\n'
elif [ "$1" = "list-sessions" ]; then
  printf '%s\n' "$RIMZ_TEST_TMUX_SESSION"
elif [ "$1" = "list-panes" ]; then
  printf '%s,@1,%%1,sh,%s,%s,1,main,rimz-sidebar,0\n' "$RIMZ_TEST_TMUX_SESSION" "$RIMZ_TEST_TMUX_CWD" "$$"
fi
printf '%s\n' "$*" >> "$RIMZ_TEST_TMUX_LOG"
exit 0
"#,
    )
    .expect("write tmux shim");
    let mut permissions = std::fs::metadata(&bin)
        .expect("tmux metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bin, permissions).expect("chmod tmux shim");
    (bin_dir, env.project_root.join("tmux-web.log"))
}

fn materialized_room_panes_json() -> &'static str {
    r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#
}

fn write_machine_config(env: &Env, text: &str) {
    let path = env.config_root().join("rimz").join("config.toml");
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config parent");
    std::fs::write(path, text).expect("write machine config");
}

fn permission_children(document: &kdl::KdlDocument, key: &str) -> Vec<String> {
    document
        .get(key)
        .expect("presence plugin permission node")
        .children()
        .expect("presence plugin permission children")
        .nodes()
        .iter()
        .map(|node| node.name().value().to_owned())
        .collect()
}

#[test]
fn web_status_json_parses_zellij_status_and_token_count() {
    let env = Env::new();
    let log = env.project_root.join("zellij-web.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "status", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env("RIMZ_TEST_ZELLIJ_WEB_TOKENS", "default 2026-07-03\n")
        .bounded_output()
        .expect("run rimz web status");

    assert!(
        output.status.success(),
        "status succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("status json parses");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(json["online"], true);
    assert_eq!(json["base_url"], "http://127.0.0.1:8082");
    assert_eq!(json["token_count"], 1);

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--help"), "{log}");
    assert!(log.contains("web\t--status"), "{log}");
    assert!(log.contains("web\t--list-tokens"), "{log}");
}

#[test]
fn web_open_disabled_fails_before_room_side_effects() {
    let env = Env::new();
    env.record(&env.project_root);
    write_machine_config(&env, "[web]\nenabled = false\n");
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-disabled.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .bounded_output()
        .expect("run rimz web open");

    assert!(!output.status.success(), "disabled web should fail");
    assert!(
        output.stdout.is_empty(),
        "disabled web should not print a URL: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Browser access is disabled")
            && stderr.contains("machine serving this room"),
        "stderr should name the serving-machine config fix: {stderr}"
    );
    assert!(!log.exists(), "disabled web should not invoke the backend");
}

#[test]
fn web_open_json_keeps_autostart_banner_off_stdout() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 0s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_START_STDOUT",
            "Web Server started on 127.0.0.1 port 8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .bounded_output()
        .expect("run rimz web open");

    assert!(
        output.status.success(),
        "open succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Web Server started"),
        "stdout should contain only JSON: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("open json parses");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(
        json["session"].as_str(),
        Some(workspace.session_name.as_str())
    );
    assert_eq!(
        json["url"],
        format!("http://127.0.0.1:8082/{}", workspace.session_name)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Web Server started"),
        "autostart banner should stay out of human stderr on success: {stderr}"
    );

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--start\t--daemonize"), "{log}");
    assert!(log.contains("web\t--status"), "{log}");
    assert!(log.contains("web\t--list-tokens"), "{log}");
    assert!(!log.contains("web\t--create-token"), "{log}");
    assert!(
        !log.contains("\t--web-sharing\ton\t"),
        "runtime plugin sharing is authoritative; birth-time --web-sharing is dead: {log}"
    );
    assert!(
        log.contains(&format!(
            "--session\t{}\tpipe\t--plugin",
            workspace.session_name
        )),
        "web open should pipe the presence plugin for this session: {log}"
    );
    assert!(
        log.contains("\t--name\trimz:share_session\t--\tshare"),
        "web open should request runtime web sharing through the presence plugin: {log}"
    );

    let permissions_path = env.home_root.join(".cache/zellij/permissions.kdl");
    let permissions = std::fs::read_to_string(&permissions_path).unwrap_or_else(|err| {
        panic!(
            "read seeded Zellij permission cache at {}: {err}",
            permissions_path.display()
        )
    });
    let document: kdl::KdlDocument = permissions.parse().expect("permissions KDL parses");
    let plugin_key = env
        .home_root
        .join(".local/share/rimz/plugins/rimz-presence-zellij.wasm")
        .display()
        .to_string();
    assert_eq!(
        permission_children(&document, &plugin_key),
        [
            "ReadApplicationState",
            "RunCommands",
            "Reconfigure",
            "StartWebServer"
        ]
    );
}

#[test]
fn web_open_assumes_zellij_without_mux_flag() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-default-mux.log");
    let output = env
        .rimz()
        .args(["web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 0s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_START_STDOUT",
            "Web Server started on 127.0.0.1 port 8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .bounded_output()
        .expect("run rimz web open");

    assert!(
        output.status.success(),
        "open succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Web Server started"),
        "stdout should contain only JSON: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("open json parses");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(
        json["session"].as_str(),
        Some(workspace.session_name.as_str())
    );
    assert_eq!(
        json["url"],
        format!("http://127.0.0.1:8082/{}", workspace.session_name)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Web Server started"),
        "autostart banner should stay out of human stderr on success: {stderr}"
    );

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--start\t--daemonize"), "{log}");
    assert!(log.contains("web\t--status"), "{log}");
    assert!(log.contains("web\t--list-tokens"), "{log}");
    assert!(!log.contains("web\t--create-token"), "{log}");
    assert!(
        !log.contains("\t--web-sharing\ton\t"),
        "runtime plugin sharing is authoritative; birth-time --web-sharing is dead: {log}"
    );
    assert!(
        log.contains(&format!(
            "--session\t{}\tpipe\t--plugin",
            workspace.session_name
        )),
        "web open should pipe the presence plugin for this session: {log}"
    );
    assert!(
        log.contains("\t--name\trimz:share_session\t--\tshare"),
        "web open should request runtime web sharing through the presence plugin: {log}"
    );
}

#[test]
fn web_open_fresh_birth_uses_runtime_share_pipe() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-fresh.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .bounded_output()
        .expect("run rimz web open");

    assert!(
        output.status.success(),
        "fresh open succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("could not confirm Zellij web sharing"),
        "fresh birth should confirm the runtime share path: {stderr}"
    );
    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("attach\t--create-background"), "{log}");
    assert!(!log.contains("--web-sharing\ton"), "{log}");
    assert!(log.contains("rimz:share_session"), "{log}");
}

#[test]
fn web_open_human_mints_and_shows_login_token() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-token.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .arg("--print")
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 0s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_1: rimz-tok-123\n",
        )
        .bounded_output()
        .expect("run rimz web open");

    assert!(
        output.status.success(),
        "human open succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("http://127.0.0.1:8082/{}\n", workspace.session_name)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Zellij web login token (paste into the browser's")
            && !stderr.contains("shown once"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz-tok-123"), "{stderr}");
    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--create-token"), "{log}");
    assert!(!log.contains("--token-name"), "{log}");
}

#[test]
fn web_token_ensure_caches_and_revoke_clears_login_token() {
    let env = Env::new();
    let log = env.project_root.join("zellij-web-token-cache.log");
    let first = env
        .rimz()
        .args(["--mux", "zellij", "web", "token", "ensure"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_1: rimz-tok-123\n",
        )
        .bounded_output()
        .expect("run rimz web token ensure");

    assert!(
        first.status.success(),
        "first ensure succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&first.stdout), "rimz-tok-123\n");

    let second = env
        .rimz()
        .args(["--mux", "zellij", "web", "token", "ensure"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_2: rimz-tok-456\n",
        )
        .bounded_output()
        .expect("run rimz web token ensure again");

    assert!(
        second.status.success(),
        "second ensure succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&second.stdout), "rimz-tok-123\n");

    let revoke = env
        .rimz()
        .args(["--mux", "zellij", "web", "token", "revoke", "token_1"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .bounded_output()
        .expect("run rimz web token revoke");

    assert!(
        revoke.status.success(),
        "revoke succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&revoke.stderr)
    );
    assert!(!env.state_root().join("rimz/web-login-token.json").exists());

    let third = env
        .rimz()
        .args(["--mux", "zellij", "web", "token", "ensure"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_2: rimz-tok-456\n",
        )
        .bounded_output()
        .expect("run rimz web token ensure after revoke");

    assert!(
        third.status.success(),
        "third ensure succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&third.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&third.stdout), "rimz-tok-456\n");

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert_eq!(log.matches("web\t--create-token").count(), 2, "{log}");
    assert!(log.contains("web\t--revoke-token\ttoken_1"), "{log}");
    assert!(!log.contains("web\t--list-tokens"), "{log}");
}

#[test]
fn web_open_refuses_url_when_prepared_session_is_not_addressable() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-dead-session.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
        .env("RIMZ_TEST_ZELLIJ_DISABLE_CREATED_SESSIONS", "1")
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250")
        .env("RIMZ_TEST_WEB_ADDRESSABLE_MS", "300")
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .bounded_output()
        .expect("run rimz web open");

    assert!(!output.status.success(), "dead session should fail fast");
    assert!(
        output.stdout.is_empty(),
        "stdout must not contain a stale URL: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not addressable after web preparation")
            || stderr.contains("Run `rimz reset` to rebuild it cleanly"),
        "stderr should explain the failed Zellij session check or reset path: {stderr}"
    );
}

#[test]
fn web_url_json_prints_offline_server_url_without_starting() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-url.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "url", "--session"])
        .arg(&workspace.session_name)
        .arg("--json")
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS",
            "Web server is offline, checked: http://127.0.0.1:8082\n",
        )
        .bounded_output()
        .expect("run rimz web url");

    assert!(
        output.status.success(),
        "url succeeds while server is offline\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("url json parses");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(
        json["url"],
        format!("http://127.0.0.1:8082/{}", workspace.session_name)
    );

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--status"), "{log}");
    assert!(log.contains("web\t--list-tokens"), "{log}");
    assert!(!log.contains("web\t--start"), "{log}");
}

#[test]
#[cfg(unix)]
fn web_tmux_open_fails_fast_when_ttyd_is_missing() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let (bin_dir, tmux_log) = tmux_shim(&env);
    let output = env
        .rimz()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .arg("--print")
        .env("PATH", &bin_dir)
        .env("RIMZ_TEST_TMUX_LOG", tmux_log)
        .env("RIMZ_TEST_TMUX_SESSION", &workspace.session_name)
        .env("RIMZ_TEST_TMUX_CWD", &env.project_root)
        .bounded_output()
        .expect("run rimz web open on tmux");

    assert!(!output.status.success(), "missing ttyd should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ttyd is required")
            && stderr.contains("brew install ttyd")
            && stderr.contains("apt install ttyd"),
        "stderr should carry the ttyd install fix: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn web_tmux_open_status_rotation_and_stop_use_ttyd() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let (bin_dir, tmux_log) = tmux_shim(&env);
    let ttyd_log = env.project_root.join("ttyd-web.log");
    let command = || {
        let mut command = env.rimz();
        command
            .env("PATH", &bin_dir)
            .env("RIMZ_TTYD_BIN", ttyd_shim())
            .env("RIMZ_TEST_TTYD_LOG", &ttyd_log)
            .env("RIMZ_TEST_TMUX_LOG", &tmux_log)
            .env("RIMZ_TEST_TMUX_SESSION", &workspace.session_name)
            .env("RIMZ_TEST_TMUX_CWD", &env.project_root);
        command
    };

    let mut open = command();
    let output = open
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("open tmux web");
    assert!(
        output.status.success(),
        "tmux open succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("open json");
    assert_eq!(json["engine"], "ttyd");
    assert_eq!(json["session"], workspace.session_name);
    assert_eq!(
        json["port"]
            .as_u64()
            .map(|port| (8200..=8299).contains(&port)),
        Some(true)
    );

    let mut status = command();
    let status = status
        .args(["web", "status", "--json"])
        .bounded_output()
        .expect("status tmux web");
    assert!(status.status.success(), "status succeeds");
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status json");
    assert_eq!(
        status_json["tmux_instances"][0]["session"],
        workspace.session_name
    );

    let first_log = std::fs::read_to_string(&ttyd_log).expect("first ttyd log");
    assert!(first_log.contains("-W\t-O\t-c\trimz:"), "{first_log}");
    assert!(
        first_log.contains(&format!(
            "-b\t/{}\ttmux\tattach\t-t",
            workspace.session_name
        )),
        "{first_log}"
    );

    let mut rotate = command();
    let rotate = rotate
        .args(["--mux", "tmux", "web", "token", "create"])
        .bounded_output()
        .expect("rotate ttyd credential");
    assert!(
        rotate.status.success(),
        "rotation succeeds: {}",
        String::from_utf8_lossy(&rotate.stderr)
    );
    let rotated_log = std::fs::read_to_string(&ttyd_log).expect("rotated ttyd log");
    assert_eq!(rotated_log.lines().count(), 2, "{rotated_log}");

    let mut revoke = command();
    let revoke = revoke
        .args(["--mux", "tmux", "web", "token", "revoke-all"])
        .bounded_output()
        .expect("revoke ttyd credential");
    assert!(
        revoke.status.success(),
        "revoke succeeds: {}",
        String::from_utf8_lossy(&revoke.stderr)
    );
    assert!(
        !env.state_root()
            .join("rimz/web-ttyd-credential.json")
            .exists(),
        "revoke-all clears the ttyd credential"
    );

    let mut reopen = command();
    let reopen = reopen
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("reopen tmux web after revoke");
    assert!(
        reopen.status.success(),
        "reopen succeeds: {}",
        String::from_utf8_lossy(&reopen.stderr)
    );

    let mut stop = command();
    let stop = stop
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop ttyd");
    assert!(
        stop.status.success(),
        "stop succeeds: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(String::from_utf8_lossy(&stop.stdout).contains("1 ttyd instance"));
}

#[cfg(unix)]
#[test]
fn web_tmux_no_start_refuses_without_an_instance() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let (bin_dir, tmux_log) = tmux_shim(&env);
    let ttyd_log = env.project_root.join("ttyd-no-start.log");
    let output = env
        .rimz()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--no-start"])
        .env("PATH", &bin_dir)
        .env("RIMZ_TTYD_BIN", ttyd_shim())
        .env("RIMZ_TEST_TTYD_LOG", &ttyd_log)
        .env("RIMZ_TEST_TMUX_LOG", tmux_log)
        .env("RIMZ_TEST_TMUX_SESSION", &workspace.session_name)
        .env("RIMZ_TEST_TMUX_CWD", &env.project_root)
        .bounded_output()
        .expect("open tmux web without start");
    assert!(!output.status.success(), "offline ttyd should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not serving tmux session"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!ttyd_log.exists(), "--no-start must not spawn ttyd");
}
