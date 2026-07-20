use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common::{CommandTimeoutExt, Env};

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn success_json(output: &Output, action: &str) -> serde_json::Value {
    assert_success(output, action);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "{action} emits JSON: {err}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn zellij_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

fn zellij_command(env: &Env, log: &Path) -> Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", log);
    command
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
# RimZ addresses its own server, so every argv leads with `-S <socket>`.
if [ "$1" = "-S" ]; then shift 2; fi
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

struct ZellijOpenFixture {
    env: Env,
    workspace: rimz::ResolvedWorkspace,
    log: PathBuf,
    output: Output,
}

fn zellij_open_json_fixture() -> ZellijOpenFixture {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open.log");
    let output = zellij_command(&env, &log)
        .args(["web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
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
    ZellijOpenFixture {
        env,
        workspace,
        log,
        output,
    }
}

fn assert_zellij_open_json(fixture: &ZellijOpenFixture) {
    let json = success_json(&fixture.output, "Zellij web open");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(json["session"], fixture.workspace.session_name);
    assert_eq!(
        json["url"],
        format!("http://127.0.0.1:8082/{}", fixture.workspace.session_name)
    );
    let stdout = String::from_utf8_lossy(&fixture.output.stdout);
    let stderr = String::from_utf8_lossy(&fixture.output.stderr);
    assert!(!stdout.contains("Web Server started"), "{stdout}");
    assert!(!stderr.contains("Web Server started"), "{stderr}");

    let log = std::fs::read_to_string(&fixture.log).expect("read zellij log");
    assert!(
        !log.contains("\t--web-sharing\ton\t"),
        "runtime plugin sharing is authoritative: {log}"
    );
    assert!(
        log.contains(&format!(
            "--session\t{}\taction\tpipe\t--plugin",
            fixture.workspace.session_name
        )),
        "web open should pipe the presence plugin: {log}"
    );
    assert!(
        log.contains("\t--skip-plugin-cache\t"),
        "web open should bypass the path-keyed plugin cache: {log}"
    );
    assert!(
        log.contains("\t--name\trimz:share_session\t--\tshare"),
        "web open should request runtime sharing: {log}"
    );
}

#[cfg(unix)]
struct TmuxWebFixture {
    env: Env,
    workspace: rimz::ResolvedWorkspace,
    bin_dir: PathBuf,
    tmux_log: PathBuf,
    ttyd_log: PathBuf,
}

#[cfg(unix)]
impl TmuxWebFixture {
    fn new(log_name: &str) -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        let workspace =
            rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let (bin_dir, tmux_log) = tmux_shim(&env);
        let ttyd_log = env.project_root.join(log_name);
        Self {
            env,
            workspace,
            bin_dir,
            tmux_log,
            ttyd_log,
        }
    }

    fn command(&self) -> Command {
        let mut command = self.env.rimz();
        command
            .env("PATH", &self.bin_dir)
            .env("RIMZ_TTYD_BIN", ttyd_shim())
            .env("RIMZ_TEST_TTYD_LOG", &self.ttyd_log)
            .env("RIMZ_TEST_TMUX_LOG", &self.tmux_log)
            .env("RIMZ_TEST_TMUX_SESSION", &self.workspace.session_name)
            .env("RIMZ_TEST_TMUX_CWD", &self.env.project_root);
        command
    }
}

#[test]
fn zellij_status_and_offline_url_report_machine_contracts() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");

    let status_log = env.project_root.join("zellij-web-status.log");
    let status = zellij_command(&env, &status_log)
        .args(["--mux", "zellij", "web", "status", "--json"])
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env("RIMZ_TEST_ZELLIJ_WEB_TOKENS", "default 2026-07-03\n")
        .bounded_output()
        .expect("run rimz web status");
    let status = success_json(&status, "Zellij web status");
    assert_eq!(status["version"], "rimz.web.v1");
    assert_eq!(status["online"], true);
    assert_eq!(status["base_url"], "http://127.0.0.1:8082");
    assert_eq!(status["token_count"], 1);

    let url_log = env.project_root.join("zellij-web-url.log");
    let url = zellij_command(&env, &url_log)
        .args(["--mux", "zellij", "web", "url", "--session"])
        .arg(&workspace.session_name)
        .arg("--json")
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS",
            "Web server is offline, checked: http://127.0.0.1:8082\n",
        )
        .bounded_output()
        .expect("run rimz web url");
    let url = success_json(&url, "offline Zellij web URL");
    assert_eq!(url["version"], "rimz.web.v1");
    assert_eq!(
        url["url"],
        format!("http://127.0.0.1:8082/{}", workspace.session_name)
    );
    let log = std::fs::read_to_string(url_log).expect("read URL zellij log");
    assert!(
        !log.contains("web\t--start"),
        "web url must not start: {log}"
    );
}

#[test]
fn zellij_failure_preserves_stderr_and_clean_stdout() {
    let env = Env::new();
    let log = env.project_root.join("zellij-web-status-failure.log");
    let output = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "status", "--json"])
        .env("RIMZ_TEST_ZELLIJ_WEB_FAIL", "status")
        .bounded_output()
        .expect("run failing zellij web status");

    assert!(!output.status.success(), "status should fail");
    assert!(
        output.stdout.is_empty(),
        "failure polluted stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("simulated zellij web status failure"),
        "failure lost subprocess stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn web_open_disabled_fails_before_room_side_effects() {
    let env = Env::new();
    env.record(&env.project_root);
    write_machine_config(&env, "[web]\nenabled = false\n");
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-disabled.log");
    let output = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("run rimz web open");

    assert!(!output.status.success(), "disabled web should fail");
    assert!(output.stdout.is_empty(), "disabled web printed a URL");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Browser access is disabled")
            && stderr.contains("machine serving this room"),
        "stderr should name the serving-machine fix: {stderr}"
    );
    assert!(!log.exists(), "disabled web should not invoke the backend");
}

#[test]
fn zellij_existing_room_open_uses_default_mux_and_runtime_sharing() {
    let fixture = zellij_open_json_fixture();
    assert_zellij_open_json(&fixture);

    let permissions_path = fixture.env.home_root.join(".cache/zellij/permissions.kdl");
    let permissions = std::fs::read_to_string(&permissions_path).unwrap_or_else(|err| {
        panic!(
            "read seeded Zellij permission cache at {}: {err}",
            permissions_path.display()
        )
    });
    let document: kdl::KdlDocument = permissions.parse().expect("permissions KDL parses");
    let plugin_key = fixture
        .env
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
fn zellij_fresh_room_open_materializes_and_shares() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-fresh.log");
    let output = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
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

    let json = success_json(&output, "fresh Zellij web open");
    assert_eq!(json["session"], workspace.session_name);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("could not confirm Zellij web sharing"),
        "{stderr}"
    );
    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("attach\t--create-background"), "{log}");
    assert!(!log.contains("--web-sharing\ton"), "{log}");
    assert!(log.contains("rimz:share_session"), "{log}");
}

#[test]
fn zellij_human_open_manages_cached_login_token() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-token-cache.log");
    let open = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .arg("--print")
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
    assert_success(&open, "human Zellij web open");
    assert_eq!(
        String::from_utf8_lossy(&open.stdout),
        format!("http://127.0.0.1:8082/{}\n", workspace.session_name)
    );
    let stderr = String::from_utf8_lossy(&open.stderr);
    assert!(stderr.contains("Zellij web login token (paste into the browser's"));
    assert!(stderr.contains("rimz-tok-123"), "{stderr}");

    let second = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "token", "ensure"])
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_2: rimz-tok-456\n",
        )
        .bounded_output()
        .expect("ensure cached Zellij token");
    assert_success(&second, "cached Zellij token ensure");
    assert_eq!(String::from_utf8_lossy(&second.stdout), "rimz-tok-123\n");

    let revoke = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "token", "revoke", "token_1"])
        .bounded_output()
        .expect("revoke Zellij token");
    assert_success(&revoke, "Zellij token revoke");
    assert!(!env.state_root().join("rimz/web-login-token.json").exists());

    let third = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "token", "ensure"])
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_CREATE_TOKEN",
            "Created token successfully\n\ntoken_2: rimz-tok-456\n",
        )
        .bounded_output()
        .expect("ensure replacement Zellij token");
    assert_success(&third, "replacement Zellij token ensure");
    assert_eq!(String::from_utf8_lossy(&third.stdout), "rimz-tok-456\n");
}

#[test]
fn zellij_open_refuses_unaddressable_prepared_session() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("zellij-web-open-dead-session.log");
    let output = zellij_command(&env, &log)
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
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
        "stdout must not contain a stale URL"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not addressable after web preparation"),
        "{stderr}"
    );
    assert!(stderr.contains("Run `rimz reset`"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn tmux_open_missing_ttyd_reports_install_fix() {
    let fixture = TmuxWebFixture::new("ttyd-missing.log");
    let output = fixture
        .command()
        .env_remove("RIMZ_TTYD_BIN")
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .arg("--print")
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
fn tmux_offline_url_is_deterministic_and_does_not_spawn() {
    let fixture = TmuxWebFixture::new("ttyd-web-url.log");
    let output = fixture
        .command()
        .args(["--mux", "tmux", "web", "url", "--session"])
        .arg(&fixture.workspace.session_name)
        .arg("--json")
        .bounded_output()
        .expect("print tmux web url");

    let json = success_json(&output, "offline tmux web URL");
    let expected_port =
        rimz::web::derive_port(&fixture.workspace.session_name, &(8200_u16..=8299_u16));
    assert_eq!(json["engine"], "ttyd");
    assert_eq!(json["port"], expected_port);
    assert_eq!(json["token_count"], 0);
    assert_eq!(
        json["url"],
        format!(
            "http://127.0.0.1:{expected_port}/{}",
            fixture.workspace.session_name
        )
    );
    assert!(!fixture.ttyd_log.exists(), "web url must not spawn ttyd");
}

#[cfg(unix)]
#[test]
fn tmux_open_rotate_revoke_reopen_and_stop() {
    let fixture = TmuxWebFixture::new("ttyd-web.log");
    let open = fixture
        .command()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("open tmux web");
    let open = success_json(&open, "tmux web open");
    assert_eq!(open["engine"], "ttyd");
    assert_eq!(open["session"], fixture.workspace.session_name);
    assert_eq!(
        open["port"]
            .as_u64()
            .map(|port| (8200..=8299).contains(&port)),
        Some(true)
    );

    let status = fixture
        .command()
        .args(["web", "status", "--json"])
        .bounded_output()
        .expect("status tmux web");
    let status = success_json(&status, "tmux web status");
    assert_eq!(
        status["tmux_instances"][0]["session"],
        fixture.workspace.session_name
    );
    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read ttyd log");
    assert_eq!(log.lines().count(), 1, "{log}");

    let rotate = fixture
        .command()
        .args(["--mux", "tmux", "web", "token", "create"])
        .bounded_output()
        .expect("rotate ttyd credential");
    assert_success(&rotate, "ttyd credential rotation");
    assert!(
        String::from_utf8_lossy(&rotate.stdout)
            .contains("rotated ttyd credential and restarted 1 instance(s)")
    );
    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read rotated ttyd log");
    assert_eq!(log.lines().count(), 2, "{log}");

    let revoke = fixture
        .command()
        .args(["--mux", "tmux", "web", "token", "revoke-all"])
        .bounded_output()
        .expect("revoke ttyd credential");
    assert_success(&revoke, "ttyd credential revoke");
    assert!(
        String::from_utf8_lossy(&revoke.stdout)
            .contains("revoked ttyd credential and stopped 1 instance(s)")
    );
    assert!(
        !fixture
            .env
            .state_root()
            .join("rimz/web-ttyd-credential.json")
            .exists(),
        "revoke-all clears the ttyd credential"
    );
    let status = fixture
        .command()
        .args(["web", "status", "--json"])
        .bounded_output()
        .expect("status revoked tmux web");
    let status = success_json(&status, "revoked tmux web status");
    assert!(status.get("tmux_instances").is_none());

    let reopen = fixture
        .command()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("reopen tmux web after revoke");
    let reopen = success_json(&reopen, "tmux web reopen");
    assert_eq!(reopen["session"], fixture.workspace.session_name);
    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read reopened ttyd log");
    assert_eq!(log.lines().count(), 3, "{log}");

    let stop = fixture
        .command()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop ttyd");
    assert_success(&stop, "ttyd web stop");
    assert!(String::from_utf8_lossy(&stop.stdout).contains("1 ttyd instance"));
}

#[cfg(unix)]
#[test]
fn tmux_no_start_refuses_without_instance() {
    let fixture = TmuxWebFixture::new("ttyd-no-start.log");
    let output = fixture
        .command()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--no-start"])
        .bounded_output()
        .expect("open tmux web without start");

    assert!(!output.status.success(), "offline ttyd should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not serving tmux session"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.ttyd_log.exists(), "--no-start must not spawn ttyd");
}
