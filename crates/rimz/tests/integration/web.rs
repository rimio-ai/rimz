use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::common::{CommandTimeoutExt, Env};

#[cfg(unix)]
static DAEMON_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
fn daemon_test_guard() -> std::sync::MutexGuard<'static, ()> {
    DAEMON_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

fn ttyd_shim() -> PathBuf {
    crate::common::cargo_bin("ttyd-trace", env!("CARGO_BIN_EXE_ttyd-trace"))
}

fn zellij_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

fn materialized_room_panes_json() -> &'static str {
    r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#
}

#[cfg(unix)]
fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind test web port")
        .local_addr()
        .expect("test web address")
        .port()
}

fn write_machine_config(env: &Env, text: &str) {
    let path = env.config_root().join("rimz").join("config.toml");
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config parent");
    std::fs::write(path, text).expect("write machine config");
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
if [ "$1" = "-S" ]; then shift 2; fi
if [ "$1" = "-V" ]; then
  printf 'tmux 3.5\n'
elif [ "$1" = "list-sessions" ]; then
  printf '%b\n' "$RIMZ_TEST_TMUX_SESSIONS"
elif [ "$1" = "list-panes" ]; then
  session=$(printf '%b' "$RIMZ_TEST_TMUX_SESSIONS" | head -n 1)
  printf '%s,@1,%%1,sh,%s,%s,1,main,rimz-sidebar,0\n' "$session" "$RIMZ_TEST_TMUX_CWD" "$$"
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

#[cfg(unix)]
struct WebFixture {
    env: Env,
    workspace: rimz::ResolvedWorkspace,
    bin_dir: PathBuf,
    ttyd_bin: PathBuf,
    web_port: u16,
    tmux_log: PathBuf,
    ttyd_log: PathBuf,
}

#[cfg(unix)]
impl WebFixture {
    fn new(log_name: &str) -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        let workspace =
            rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let (bin_dir, tmux_log) = tmux_shim(&env);
        let ttyd_bin = bin_dir.join("ttyd");
        std::os::unix::fs::symlink(ttyd_shim(), &ttyd_bin).expect("link named ttyd fixture");
        let ttyd_log = env.project_root.join(log_name);
        let web_port = free_loopback_port();
        write_machine_config(&env, &format!("[web]\nport = {web_port}\n"));
        Self {
            env,
            workspace,
            bin_dir,
            ttyd_bin,
            web_port,
            tmux_log,
            ttyd_log,
        }
    }

    fn command(&self) -> Command {
        self.command_with_sessions(&self.workspace.session_name)
    }

    fn command_with_sessions(&self, sessions: &str) -> Command {
        let mut command = self.env.rimz();
        command
            .env("PATH", &self.bin_dir)
            .env("RIMZ_TTYD_BIN", &self.ttyd_bin)
            .env("RIMZ_TEST_TTYD_LOG", &self.ttyd_log)
            .env("RIMZ_WEB_FONTS_OFFLINE", "1")
            .env("RIMZ_TEST_TMUX_LOG", &self.tmux_log)
            .env("RIMZ_TEST_TMUX_SESSIONS", sessions)
            .env("RIMZ_TEST_TMUX_CWD", &self.env.project_root);
        command
    }
}

#[test]
fn web_open_disabled_fails_before_room_side_effects() {
    let env = Env::new();
    env.record(&env.project_root);
    write_machine_config(&env, "[web]\nenabled = false\n");
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let log = env.project_root.join("ttyd-disabled.log");
    let output = env
        .rimz()
        .env("RIMZ_TTYD_BIN", ttyd_shim())
        .env("RIMZ_TEST_TTYD_LOG", &log)
        .args(["web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("run rimz web open");

    assert!(!output.status.success(), "disabled web should fail");
    assert!(output.stdout.is_empty(), "disabled web printed a URL");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Browser access is disabled"));
    assert!(!log.exists(), "disabled web should not invoke ttyd");
}

#[cfg(unix)]
#[test]
fn offline_url_and_status_use_configured_shared_port_without_spawning() {
    let fixture = WebFixture::new("ttyd-offline.log");
    write_machine_config(
        &fixture.env,
        "[web]\nport = 9123\nbase_url = \"https://devbox.example/rimz\"\n",
    );
    let url = fixture
        .command()
        .args(["--mux", "tmux", "web", "url", "--session"])
        .arg(&fixture.workspace.session_name)
        .arg("--json")
        .bounded_output()
        .expect("print web URL");
    let url = success_json(&url, "offline web URL");
    assert_eq!(url["version"], "rimz.web.v2");
    assert_eq!(url["port"], 9123);
    assert_eq!(
        url["url"],
        format!(
            "https://devbox.example/rimz/?arg={}",
            fixture.workspace.session_name
        )
    );
    assert!(url.get("credential").is_none());
    assert!(
        !fixture
            .env
            .state_root()
            .join("rimz/web-ttyd-credential.json")
            .exists(),
        "URL inspection persisted a credential"
    );

    let status = fixture
        .command()
        .args(["web", "status", "--json"])
        .bounded_output()
        .expect("status web daemon");
    let status = success_json(&status, "offline web status");
    assert_eq!(status["version"], "rimz.web.v2");
    assert_eq!(status["online"], false);
    assert_eq!(status["port"], 9123);
    assert!(!fixture.ttyd_log.exists(), "url/status must not spawn ttyd");
}

#[cfg(unix)]
#[test]
fn offline_token_operations_keep_one_singular_credential() {
    let fixture = WebFixture::new("ttyd-offline-token.log");
    let credential_path = fixture
        .env
        .state_root()
        .join("rimz/web-ttyd-credential.json");
    let daemon_path = fixture.env.state_root().join("rimz/web-ttyd.json");

    let empty = fixture
        .command()
        .args(["web", "token", "list"])
        .bounded_output()
        .expect("list offline token");
    assert_success(&empty, "empty offline token list");
    assert!(empty.stdout.is_empty());
    assert!(!credential_path.exists());
    assert!(!daemon_path.exists());

    let create = fixture
        .command()
        .args(["web", "token", "create"])
        .bounded_output()
        .expect("create offline token");
    assert_success(&create, "offline token creation");
    assert_eq!(
        String::from_utf8_lossy(&create.stdout),
        "rotated ttyd credential and restarted 0 daemon(s)\n"
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credential_path).expect("saved credential"))
            .expect("credential JSON");
    assert_eq!(saved["name"], "rimz");
    assert!(!daemon_path.exists());

    let list = fixture
        .command()
        .args(["web", "token", "list"])
        .bounded_output()
        .expect("list saved offline token");
    assert_success(&list, "saved offline token list");
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        format!(
            "rimz: {}\n",
            saved["created_at"].as_str().expect("timestamp")
        )
    );

    let revoke_all = fixture
        .command()
        .args(["web", "token", "revoke-all"])
        .bounded_output()
        .expect("revoke all offline tokens");
    assert_success(&revoke_all, "offline revoke-all");
    assert_eq!(
        String::from_utf8_lossy(&revoke_all.stdout),
        "revoked ttyd credential and stopped 0 daemon(s)\n"
    );

    assert_success(
        &fixture
            .command()
            .args(["web", "token", "create"])
            .bounded_output()
            .expect("recreate offline token"),
        "offline token recreation",
    );
    let revoke_named = fixture
        .command()
        .args(["web", "token", "revoke", "rimz"])
        .bounded_output()
        .expect("revoke named offline token");
    assert_success(&revoke_named, "offline named revoke");
    assert_eq!(revoke_named.stdout, revoke_all.stdout);
    assert!(!credential_path.exists());
    assert!(!daemon_path.exists());
}

#[cfg(unix)]
#[test]
fn two_rooms_reuse_one_shared_daemon_and_rotate_restarts_it() {
    let _guard = daemon_test_guard();
    let fixture = WebFixture::new("ttyd-shared.log");
    write_machine_config(
        &fixture.env,
        &format!("[web]\nport = {}\nstyle_client = false\n", fixture.web_port),
    );
    let second_root = fixture.env.project_root.join("second-room");
    std::fs::create_dir_all(&second_root).expect("mkdir second room");
    fixture.env.record(&second_root);
    let second = rimz::WorkspaceResolver::resolve(&second_root, None).expect("resolve second room");
    let sessions = format!(
        "{}\\n{}",
        fixture.workspace.session_name, second.session_name
    );

    let mut shared_secret = None;
    for workspace in [&fixture.workspace, &second] {
        let open = fixture
            .command_with_sessions(&sessions)
            .args(["--mux", "tmux", "web", "open", "--session"])
            .arg(&workspace.session_name)
            .args(["--print", "--json"])
            .bounded_output()
            .expect("open shared web daemon");
        let payload = success_json(&open, "shared web open");
        assert_eq!(payload["version"], "rimz.web.v2");
        assert_eq!(payload["session"], workspace.session_name);
        assert_eq!(payload["port"], fixture.web_port);
        assert_eq!(
            payload["url"],
            format!(
                "http://127.0.0.1:{}/?arg={}",
                fixture.web_port, workspace.session_name
            )
        );
        assert_eq!(payload["credential"]["username"], "rimz");
        let secret = payload["credential"]["secret"]
            .as_str()
            .expect("shared credential secret");
        if let Some(first) = &shared_secret {
            assert_eq!(secret, first);
        } else {
            shared_secret = Some(secret.to_owned());
        }
    }

    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read ttyd log");
    assert_eq!(
        log.lines().count(),
        2,
        "one stock-index fetch plus one shared daemon: {log}"
    );
    let daemon_argv = log
        .lines()
        .find(|line| line.contains("\tweb\texec"))
        .expect("shared daemon argv");
    assert!(
        daemon_argv.contains("-W\t-O\t-a\t-c\trimz:"),
        "{daemon_argv}"
    );
    assert!(!daemon_argv.contains("\t-b\t"), "{daemon_argv}");
    assert!(daemon_argv.contains("\t-I\t"), "{daemon_argv}");
    assert!(!daemon_argv.contains("fontFamily="), "{daemon_argv}");
    assert!(!daemon_argv.contains("theme="), "{daemon_argv}");
    assert!(
        daemon_argv.contains(&format!("\t-p\t{}\t", fixture.web_port)),
        "{daemon_argv}"
    );

    let status = fixture
        .command_with_sessions(&sessions)
        .args(["web", "status", "--json"])
        .bounded_output()
        .expect("status shared daemon");
    let status = success_json(&status, "shared daemon status");
    assert_eq!(status["online"], true);
    assert_eq!(status["port"], fixture.web_port);
    assert!(status["pid"].as_u64().is_some());

    let credential_path = fixture
        .env
        .state_root()
        .join("rimz/web-ttyd-credential.json");
    let daemon_path = fixture.env.state_root().join("rimz/web-ttyd.json");
    let credential_before = std::fs::read(&credential_path).expect("credential before bad revoke");
    let daemon_before = std::fs::read(&daemon_path).expect("daemon before bad revoke");
    let bad_revoke = fixture
        .command_with_sessions(&sessions)
        .args(["web", "token", "revoke", "other"])
        .bounded_output()
        .expect("reject unknown credential name");
    assert!(!bad_revoke.status.success());
    assert!(
        String::from_utf8_lossy(&bad_revoke.stderr)
            .contains("ttyd credential `other` does not exist")
    );
    assert_eq!(
        std::fs::read(&credential_path).expect("credential after bad revoke"),
        credential_before
    );
    assert_eq!(
        std::fs::read(&daemon_path).expect("daemon after bad revoke"),
        daemon_before
    );

    write_machine_config(&fixture.env, "[web]\nport = 9123\n");
    let url = fixture
        .command_with_sessions(&sessions)
        .args(["--mux", "tmux", "web", "url", "--session"])
        .arg(&fixture.workspace.session_name)
        .arg("--json")
        .bounded_output()
        .expect("inspect live shared daemon");
    let url = success_json(&url, "live shared URL");
    assert_eq!(
        url["port"], fixture.web_port,
        "live port wins over changed config"
    );
    assert_eq!(
        url["credential"]["secret"],
        shared_secret.expect("first shared credential")
    );
    write_machine_config(
        &fixture.env,
        &format!("[web]\nport = {}\n", fixture.web_port),
    );

    std::fs::remove_file(
        fixture
            .env
            .state_root()
            .join("rimz/web-ttyd-credential.json"),
    )
    .expect("remove shared credential");
    let no_start = fixture
        .command_with_sessions(&sessions)
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--no-start"])
        .bounded_output()
        .expect("open live daemon without credential");
    assert!(!no_start.status.success());
    assert!(
        String::from_utf8_lossy(&no_start.stderr).contains("daemon credential is missing"),
        "{}",
        String::from_utf8_lossy(&no_start.stderr)
    );

    let rotate = fixture
        .command_with_sessions(&sessions)
        .args(["web", "token", "create"])
        .bounded_output()
        .expect("rotate shared credential");
    assert_success(&rotate, "shared credential rotation");
    assert!(
        String::from_utf8_lossy(&rotate.stdout)
            .contains("rotated ttyd credential and restarted 1 daemon(s)")
    );
    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read rotated log");
    assert_eq!(
        log.lines().count(),
        3,
        "rotation starts one replacement: {log}"
    );

    let stop = fixture
        .command_with_sessions(&sessions)
        .args(["web", "token", "revoke", "rimz"])
        .bounded_output()
        .expect("revoke shared credential");
    assert_success(&stop, "revoke shared credential");
    assert_eq!(
        String::from_utf8_lossy(&stop.stdout),
        "revoked ttyd credential and stopped 1 daemon(s)\n"
    );
}

#[cfg(unix)]
#[test]
fn concurrent_start_calls_create_one_shared_daemon() {
    let _guard = daemon_test_guard();
    let fixture = WebFixture::new("ttyd-concurrent.log");
    let mut first = fixture.command();
    first.args(["web", "start"]);
    let mut second = fixture.command();
    second.args(["web", "start"]);

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(move || first.bounded_output().expect("first web start"));
        let second = scope.spawn(move || second.bounded_output().expect("second web start"));
        (
            first.join().expect("first start thread"),
            second.join().expect("second start thread"),
        )
    });
    assert_success(&first, "first concurrent start");
    assert_success(&second, "second concurrent start");

    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read ttyd log");
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("\tweb\texec"))
            .count(),
        1,
        "concurrent starts must share one daemon: {log}"
    );

    let stop = fixture
        .command()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop concurrent daemon");
    assert_success(&stop, "stop concurrent daemon");
}

#[cfg(unix)]
#[test]
fn first_shared_start_reaps_legacy_ttyd_before_binding_its_port() {
    let _guard = daemon_test_guard();
    let fixture = WebFixture::new("ttyd-legacy.log");
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve legacy port")
        .local_addr()
        .expect("legacy port address")
        .port();
    write_machine_config(&fixture.env, &format!("[web]\nport = {port}\n"));
    let mut legacy = Command::new(&fixture.ttyd_bin)
        .args(["-p", &port.to_string(), "sh"])
        .env("RIMZ_TEST_TTYD_LOG", &fixture.ttyd_log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start legacy ttyd fixture");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline
        && std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "legacy ttyd fixture did not bind"
    );

    let legacy_dir = fixture.env.state_root().join("rimz/web-ttyd");
    std::fs::create_dir_all(&legacy_dir).expect("mkdir legacy state");
    std::fs::write(
        legacy_dir.join("legacy.json"),
        serde_json::to_vec(&serde_json::json!({
            "session": fixture.workspace.session_name,
            "pid": legacy.id(),
            "port": port
        }))
        .expect("serialize legacy state"),
    )
    .expect("write legacy state");

    let start = fixture
        .command()
        .args(["web", "start"])
        .bounded_output()
        .expect("start shared daemon after legacy cleanup");
    assert_success(&start, "shared start after legacy cleanup");
    assert!(!legacy_dir.exists(), "legacy state directory remains");
    assert!(
        legacy.try_wait().expect("query legacy ttyd").is_some(),
        "legacy ttyd still runs"
    );

    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read ttyd log");
    assert!(
        log.lines().any(|line| line.contains("\tweb\texec")),
        "shared daemon did not replace legacy instance: {log}"
    );
    let stop = fixture
        .command()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop shared daemon");
    assert_success(&stop, "stop shared daemon after legacy cleanup");
}

#[cfg(unix)]
#[test]
fn no_start_requires_an_online_daemon() {
    let fixture = WebFixture::new("ttyd-no-start.log");
    let output = fixture
        .command()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--no-start"])
        .bounded_output()
        .expect("open web without start");

    assert!(!output.status.success(), "offline ttyd should fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("shared ttyd daemon is offline"));
    assert!(!fixture.ttyd_log.exists(), "--no-start must not spawn ttyd");
}

#[cfg(unix)]
#[test]
fn stale_daemon_record_never_signals_a_reused_non_ttyd_pid() {
    let env = Env::new();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind foreign listener");
    let port = listener
        .local_addr()
        .expect("foreign listener address")
        .port();
    let mut unrelated = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn unrelated process");
    let state = env.state_root().join("rimz");
    std::fs::create_dir_all(&state).expect("mkdir web state");
    std::fs::write(
        state.join("web-ttyd.json"),
        serde_json::to_vec(&serde_json::json!({
            "pid": unrelated.id(),
            "port": port
        }))
        .expect("serialize stale daemon state"),
    )
    .expect("write stale daemon state");

    let stop = env
        .rimz()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop stale daemon record");
    assert_success(&stop, "discard stale daemon record");
    assert!(String::from_utf8_lossy(&stop.stdout).contains("stopped 0 ttyd daemons"));
    assert!(
        unrelated
            .try_wait()
            .expect("query unrelated process")
            .is_none(),
        "stale daemon record signalled unrelated process"
    );
    unrelated.kill().expect("kill unrelated fixture");
    unrelated.wait().expect("reap unrelated fixture");
}

#[cfg(unix)]
#[test]
fn web_exec_rejects_unknown_session_and_lists_live_rimz_rooms() {
    let fixture = WebFixture::new("ttyd-exec-reject.log");
    let hostile = "not-a-rimz-room;--create";
    let output = fixture
        .command()
        .args(["--mux", "tmux", "web", "exec", hostile])
        .bounded_output()
        .expect("reject unknown web session");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("session `{hostile}` is not a live RimZ room")),
        "{stderr}"
    );
    assert!(stderr.contains("Live RimZ sessions:"), "{stderr}");
    assert!(stderr.contains(&fixture.workspace.session_name), "{stderr}");
    let log = std::fs::read_to_string(&fixture.tmux_log).expect("read tmux trace");
    assert!(!log.lines().any(|line| line.contains("attach")), "{log}");
}

#[cfg(unix)]
#[test]
fn web_exec_rejects_known_stopped_session_before_attach() {
    let fixture = WebFixture::new("ttyd-exec-stopped.log");
    let stopped_root = fixture.env.project_root.join("stopped-room");
    std::fs::create_dir_all(&stopped_root).expect("mkdir stopped room");
    fixture.env.record(&stopped_root);
    let stopped =
        rimz::WorkspaceResolver::resolve(&stopped_root, None).expect("resolve stopped workspace");
    let second_live_root = fixture.env.project_root.join("second-live-room");
    std::fs::create_dir_all(&second_live_root).expect("mkdir second live room");
    fixture.env.record(&second_live_root);
    let second_live = rimz::WorkspaceResolver::resolve(&second_live_root, None)
        .expect("resolve second live workspace");
    let live_sessions = format!(
        "{}\\n{}",
        second_live.session_name, fixture.workspace.session_name
    );

    let output = fixture
        .command_with_sessions(&live_sessions)
        .args(["--mux", "tmux", "web", "exec"])
        .arg(&stopped.session_name)
        .bounded_output()
        .expect("reject stopped web session");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "session `{}` is not a live RimZ room",
            stopped.session_name
        )),
        "{stderr}"
    );
    assert!(stderr.contains(&fixture.workspace.session_name), "{stderr}");
    assert!(stderr.contains(&second_live.session_name), "{stderr}");
    assert!(
        !stderr.contains(&format!("  {} (", stopped.session_name)),
        "{stderr}"
    );
    let mut sorted = [
        fixture.workspace.session_name.as_str(),
        second_live.session_name.as_str(),
    ];
    sorted.sort_unstable();
    assert!(
        stderr.find(sorted[0]).expect("first live session")
            < stderr.find(sorted[1]).expect("second live session"),
        "{stderr}"
    );
    let log = std::fs::read_to_string(&fixture.tmux_log).expect("read tmux trace");
    assert!(!log.lines().any(|line| line.contains("attach")), "{log}");
}

#[cfg(unix)]
#[test]
fn custom_font_is_inlined_into_the_shared_client_index() {
    let _guard = daemon_test_guard();
    let fixture = WebFixture::new("ttyd-custom-font.log");
    let font = fixture.env.home_root.join("custom-font.woff2");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dummy-font.woff2"),
        &font,
    )
    .expect("copy dummy font under sandbox HOME");
    write_machine_config(
        &fixture.env,
        &format!(
            "[web]\nport = {}\nfont = \"RimZ Test Font\"\nfont_source = \"~/custom-font.woff2\"\n",
            fixture.web_port
        ),
    );

    let open = fixture
        .command()
        .args(["--mux", "tmux", "web", "open", "--session"])
        .arg(&fixture.workspace.session_name)
        .args(["--print", "--json"])
        .bounded_output()
        .expect("open web with custom font");
    success_json(&open, "custom-font web open");

    let log = std::fs::read_to_string(&fixture.ttyd_log).expect("read ttyd log");
    let daemon_argv = log
        .lines()
        .find(|line| line.contains("\tweb\texec"))
        .expect("shared daemon argv");
    assert!(
        daemon_argv.contains("\t-t\tfontFamily=RimZ Test Font,monospace"),
        "{daemon_argv}"
    );
    let args = daemon_argv.split('\t').collect::<Vec<_>>();
    let index = args
        .windows(2)
        .find(|pair| pair[0] == "-I")
        .map(|pair| PathBuf::from(pair[1]))
        .expect("generated -I path");
    let index = std::fs::read_to_string(index).expect("read generated ttyd index");
    assert!(index.contains("font-family:\"RimZ Test Font\""), "{index}");
    assert!(
        index.contains("data:font/woff2;base64,cmlteiBicm93c2VyIGZvbnQgZml4dHVyZQo="),
        "{index}"
    );

    let stop = fixture
        .command()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop custom-font daemon");
    assert_success(&stop, "stop custom-font daemon");
}

#[cfg(unix)]
#[test]
fn zellij_room_uses_the_same_shared_ttyd_daemon() {
    let _guard = daemon_test_guard();
    let env = Env::new();
    env.record(&env.project_root);
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let ttyd_log = env.project_root.join("ttyd-zellij.log");
    let zellij_log = env.project_root.join("zellij-web.log");
    let ttyd_bin = env.home_root.join("zellij-web-bin/ttyd");
    std::fs::create_dir_all(ttyd_bin.parent().expect("ttyd fixture parent"))
        .expect("mkdir ttyd fixture parent");
    std::os::unix::fs::symlink(ttyd_shim(), &ttyd_bin).expect("link named ttyd fixture");
    let web_port = free_loopback_port();
    write_machine_config(&env, &format!("[web]\nport = {web_port}\n"));
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "open", "--session"])
        .arg(&workspace.session_name)
        .args(["--print", "--json"])
        .env("RIMZ_TTYD_BIN", &ttyd_bin)
        .env("RIMZ_TEST_TTYD_LOG", &ttyd_log)
        .env("RIMZ_WEB_FONTS_OFFLINE", "1")
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &zellij_log)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 0s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            materialized_room_panes_json(),
        )
        .bounded_output()
        .expect("open Zellij room through ttyd");
    let payload = success_json(&output, "Zellij ttyd web open");
    assert_eq!(payload["version"], "rimz.web.v2");
    assert_eq!(payload["session"], workspace.session_name);
    assert_eq!(
        payload["url"],
        format!(
            "http://127.0.0.1:{web_port}/?arg={}",
            workspace.session_name
        )
    );

    let ttyd_log = std::fs::read_to_string(&ttyd_log).expect("read ttyd log");
    assert!(ttyd_log.lines().any(|line| line.contains("\tweb\texec")));
    let zellij_log = std::fs::read_to_string(&zellij_log).expect("read zellij log");
    assert!(!zellij_log.contains("share_session"), "{zellij_log}");
    assert!(!zellij_log.contains("web-sharing"), "{zellij_log}");

    let stop = env
        .rimz()
        .args(["web", "stop"])
        .bounded_output()
        .expect("stop shared daemon after Zellij open");
    assert_success(&stop, "stop shared daemon after Zellij open");
}

#[cfg(unix)]
#[test]
fn configured_port_owned_by_another_process_names_the_fix() {
    let env = Env::new();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve loopback port");
    let port = listener.local_addr().expect("listener address").port();
    write_machine_config(&env, &format!("[web]\nport = {port}\n"));
    let output = env
        .rimz()
        .args(["web", "start"])
        .env("RIMZ_TTYD_BIN", ttyd_shim())
        .env(
            "RIMZ_TEST_TTYD_LOG",
            env.project_root.join("foreign-port.log"),
        )
        .env("RIMZ_WEB_FONTS_OFFLINE", "1")
        .bounded_output()
        .expect("start shared daemon on occupied port");

    assert!(!output.status.success(), "occupied port should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("[web] port {port} is already in use")),
        "{stderr}"
    );
    assert!(stderr.contains("choose a free port"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn room_start_warns_when_browser_daemon_cannot_start() {
    let fixture = WebFixture::new("ttyd-best-effort.log");
    let output = fixture
        .command()
        .args(["--mux", "tmux", "start", "--no-attach"])
        .env("RIMZ_TTYD_BIN", fixture.env.home_root.join("missing-ttyd"))
        .bounded_output()
        .expect("start room without ttyd");

    assert_success(&output, "room start without ttyd");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("browser daemon was not started"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
