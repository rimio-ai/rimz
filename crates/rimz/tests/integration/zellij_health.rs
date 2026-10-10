//! Regression coverage for Zellij rooms that are live but cannot be inspected.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn unresponsive_live_zellij_room_fails_fast_and_is_preserved() {
    assert_unresponsive_live_room_is_preserved(None);
}

#[test]
fn timed_out_live_zellij_room_fails_fast_and_is_preserved() {
    assert_unresponsive_live_room_is_preserved(Some("60"));
}

fn assert_unresponsive_live_room_is_preserved(list_panes_sleep: Option<&str>) {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = FakeZellij::new();

    let mut command = env.rimz();
    command
        .args(["--mux", "zellij", "start"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100");
    if let Some(sleep) = list_panes_sleep {
        command.env("RIMZ_TEST_ZELLIJ_LIST_PANES_SLEEP", sleep);
    }
    let output = command.bounded_output().expect("run rimz start");

    assert!(
        !output.status.success(),
        "an unresponsive live room must fail before attach",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&workspace.session_name) && stderr.contains("rimz reset"),
        "error should name the room and recovery command, got: {stderr}",
    );

    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    assert!(
        !lines.iter().any(|line| line.starts_with("attach")),
        "the attach child must not run: {lines:?}",
    );
    assert!(
        !lines.iter().any(|line| line.contains("action\tlist-tabs")),
        "best-effort room launches must not run before the health refusal: {lines:?}",
    );
    assert!(
        !lines.iter().any(|line| line.contains("delete-session")),
        "an unresponsive live room must be preserved: {lines:?}",
    );
    assert!(
        env.read_events().iter().all(|event| !matches!(
            event.kind(),
            rimz::store::event::EventKind::SessionDeath(_)
                | rimz::store::event::EventKind::SessionRebirth
        )),
        "a live room must not be recorded as dead or reborn",
    );
}

#[test]
fn unresponsive_foreign_zellij_session_names_native_recovery_and_bypass() {
    let env = Env::new();
    let shim = FakeZellij::new();
    let session = "someone-elses-session";

    let output = env
        .rimz()
        .args(["--mux", "zellij", "attach", session, "--print"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", session)
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz attach");

    assert!(!output.status.success(), "unresponsive attach must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not RimZ-managed"), "stderr: {stderr}");
    assert!(
        stderr.contains("zellij delete-session --force 'someone-elses-session'"),
        "stderr: {stderr}",
    );
    assert!(
        stderr.contains("zellij attach 'someone-elses-session'") && stderr.contains("bypass RimZ"),
        "stderr: {stderr}",
    );
    assert!(!stderr.contains("rimz reset"), "stderr: {stderr}");
    assert!(!stderr.contains("prompts first"), "stderr: {stderr}");

    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    assert!(
        !lines.iter().any(|line| line.starts_with("attach")),
        "the bypass is advice, not a command RimZ ran: {lines:?}",
    );
    assert!(
        !lines.iter().any(|line| line.contains("delete-session")),
        "RimZ must not destroy a foreign session: {lines:?}",
    );
}

#[test]
fn unresponsive_zellij_session_with_unknown_ownership_avoids_destroy_guidance() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .record_room_bin(
            &workspace,
            std::env::current_exe().expect("current test binary"),
            "test".to_owned(),
        )
        .expect("record managed room");

    let workspaces = env.rimz_home().join("ws");
    let hidden_workspaces = env.rimz_home().join("ws-hidden");
    fs::rename(&workspaces, &hidden_workspaces).expect("hide workspace records");
    fs::write(&workspaces, b"not a directory").expect("block workspace record lookup");

    let shim = FakeZellij::new();
    let output = env
        .rimz()
        .args([
            "--mux",
            "zellij",
            "attach",
            workspace.session_name.as_str(),
            "--print",
        ])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz attach");

    fs::remove_file(&workspaces).expect("remove lookup blocker");
    fs::rename(&hidden_workspaces, &workspaces).expect("restore workspace records");

    assert!(!output.status.success(), "unresponsive attach must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not determine whether this session is managed"),
        "stderr: {stderr}",
    );
    assert!(stderr.contains("rimz doctor"), "stderr: {stderr}");
    assert!(
        stderr.contains(&format!("zellij attach '{}'", workspace.session_name)),
        "stderr: {stderr}",
    );
    assert!(!stderr.contains("delete-session"), "stderr: {stderr}");
    assert!(!stderr.contains("rimz reset"), "stderr: {stderr}");
}

#[test]
fn attach_retries_transient_zellij_session_listing_before_default_mux_fallback() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = FakeZellij::new().with_tmux();
    let fail_once = shim.log.with_extension("list-sessions-fail-once");

    let output = env
        .rimz()
        .args(["attach", workspace.session_name.as_str(), "--print"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE", &fail_once)
        .bounded_output()
        .expect("run rimz attach");

    assert!(
        output.status.success(),
        "attach should print successfully: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("zellij attach") && stdout.contains(&workspace.session_name),
        "attach should target the live zellij room after retry, got: {stdout}",
    );
    assert!(
        !stdout.contains("tmux attach"),
        "attach should not fall back to the default tmux shim, got: {stdout}",
    );
    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    let list_attempts = lines
        .iter()
        .filter(|line| line.contains("list-sessions"))
        .count();
    assert!(
        list_attempts >= 2,
        "zellij list-sessions should be retried after a transient failure: {lines:?}",
    );
}

#[test]
fn named_attach_preserves_recorded_room_owner() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let recorded_owner = env.project_root.join("previous-rimz");
    rimz::disk::atomic::write_executable_bytes_atomically(&recorded_owner, b"recorded build")
        .expect("write recorded room owner");
    let store = env.store();
    store
        .record_room_bin(&workspace, recorded_owner.clone(), "recorded".to_owned())
        .expect("record room owner");
    let shim = FakeZellij::new().with_tmux();

    let output = env
        .rimz()
        .args(["attach", workspace.session_name.as_str(), "--print"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz attach");

    assert!(
        output.status.success(),
        "named attach should succeed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let record = rimz::workspace::record::read(&store.paths().workspace_record)
        .expect("read workspace record");
    assert_eq!(record.rimz_bin, Some(recorded_owner));
}

#[test]
fn cold_cwd_attach_claims_recorded_room_owner() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let recorded_owner = env.project_root.join("previous-rimz");
    rimz::disk::atomic::write_executable_bytes_atomically(&recorded_owner, b"recorded build")
        .expect("write recorded room owner");
    let store = env.store();
    store
        .record_room_bin(&workspace, recorded_owner.clone(), "recorded".to_owned())
        .expect("record room owner");
    let shim = FakeZellij::new().with_tmux();
    let tmux_log = env.project_root.join("tmux.log");
    let _room = crate::common::room::ShimRoom::watch_tmux(
        env.runtime_paths(),
        &workspace.session_name,
        &tmux_log,
    );

    let output = env
        .rimz()
        .args(["attach", "--print"])
        .env("RIMZ_TEST_TMUX_LOG", &tmux_log)
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz attach");

    assert!(
        output.status.success(),
        "cold attach should succeed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let record = rimz::workspace::record::read(&store.paths().workspace_record)
        .expect("read workspace record");
    let claimed = record.rimz_bin.expect("cold attach records a room owner");
    assert_ne!(claimed, recorded_owner);
    assert!(claimed.is_file(), "claimed owner {claimed:?} is staged");
}

#[test]
fn tmux_start_never_probes_the_rival_backend() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = FakeZellij::new().with_tmux();
    let tmux_log = env.project_root.join("tmux.log");
    let _room = crate::common::room::ShimRoom::watch_tmux(
        env.runtime_paths(),
        &workspace.session_name,
        &tmux_log,
    );

    let output = env
        .rimz()
        .args(["--tmux", "start", "--no-attach"])
        .env("RIMZ_TEST_TMUX_LOG", &tmux_log)
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS_SLEEP", "60")
        .env("RIMZ_TEST_SESSION_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz start");

    assert!(
        output.status.success(),
        "tmux start should proceed around wedged rival zellij: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("skipping"),
        "an explicit backend must not need a rival probe notice: {stderr}",
    );
    let lines = fs::read_to_string(&shim.log).unwrap();
    assert!(
        !lines.contains("list-sessions"),
        "rival was probed: {lines}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The printed command must name the RimZ socket: a bare `tmux attach -t`
    // would look for the room on the user's default server and find nothing.
    assert!(
        stdout.contains("tmux -S") && stdout.contains("attach"),
        "start --no-attach should print a socket-scoped tmux attach command, got: {stdout}",
    );
}

#[test]
fn start_refuses_a_held_room_even_when_the_rival_probe_would_wedge() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    env.record(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let record_before = fs::read(&paths.workspace_record).unwrap();
    let _held = rimz::disk::lock::RoomLock::hold(&env.runtime_paths().room_lock()).unwrap();
    let shim = FakeZellij::new().with_tmux();
    let tmux_log = env.project_root.join("tmux.log");

    let output = env
        .rimz()
        .args(["--tmux", "start", "--no-attach"])
        .env("RIMZ_TEST_TMUX_LOG", &tmux_log)
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS_SLEEP", "60")
        .env("RIMZ_TEST_SESSION_PROBE_MS", "100")
        .bounded_output_within(Duration::from_secs(10))
        .expect("held room refusal must beat the wedged rival's deadline");

    assert!(!output.status.success(), "a held room must refuse birth");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&workspace.session_name)
            && stderr.contains(
                "already held by another running room (another multiplexer, or a renamed session)"
            )
            && stderr.contains("rimz attach")
            && stderr.contains("rimz reset --no-start")
            && stderr.contains(&format!(
                "If this project's session is still open in another multiplexer, or you renamed it, close it from that multiplexer (or rename it back to `{}`), then run the command again.",
                workspace.session_name
            )),
        "stderr: {stderr}"
    );
    let lines = fs::read_to_string(&tmux_log).unwrap();
    assert!(!lines.contains("new-session"), "birth ran: {lines}");
    assert!(
        !fs::read_to_string(&shim.log)
            .unwrap()
            .contains("list-sessions")
    );
    assert_eq!(fs::read(&paths.workspace_record).unwrap(), record_before);
}

#[test]
fn zellij_start_fails_fast_when_selected_session_probe_wedges() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = FakeZellij::new().with_tmux();

    let output = env
        .rimz()
        .args(["--zellij", "start", "--no-attach"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        // The shim cannot finish naturally before the harness deadline. The
        // recovery error below is the deterministic evidence that both mux
        // probe deadlines ran and killed their children.
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS_SLEEP", "60")
        .env("RIMZ_TEST_SESSION_PROBE_MS", "100")
        .bounded_output()
        .expect("run rimz start");

    assert!(
        !output.status.success(),
        "zellij start should refuse a wedged selected backend",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("zellij is not responding") && stderr.contains("rimz --tmux"),
        "stderr should explain recovery, got: {stderr}",
    );
}

struct FakeZellij {
    _home: TempDir,
    bin_dir: TempDir,
    bin: PathBuf,
    log: PathBuf,
}

impl FakeZellij {
    fn new() -> Self {
        let home = TempDir::new().expect("fake zellij home");
        let bin_dir = TempDir::new().expect("fake zellij bin dir");
        let bin = bin_dir.path().join("zellij");
        let log = home.path().join("zellij.log");
        fs::write(&bin, fake_zellij_script()).expect("write fake zellij");
        make_executable(&bin);
        fs::write(&log, "").expect("create fake zellij log");
        Self {
            _home: home,
            bin_dir,
            bin,
            log,
        }
    }

    fn with_tmux(self) -> Self {
        let tmux = self.bin_dir.path().join("tmux");
        fs::write(&tmux, fake_tmux_script()).expect("write fake tmux");
        make_executable(&tmux);
        self
    }
}

fn fake_zellij_script() -> &'static str {
    r#"#!/bin/sh
{
  first=1
  for arg in "$@"; do
    if [ "$first" = 1 ]; then
      first=0
    else
      printf '\t'
    fi
    printf '%s' "$arg"
  done
  printf '\n'
} >> "$RIMZ_TEST_ZELLIJ_LOG"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

if [ "$1" = "list-sessions" ]; then
  if [ -n "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_SLEEP" ]; then
    exec /bin/sleep "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_SLEEP"
  fi
  if [ -n "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE" ] && [ ! -e "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE" ]; then
    : > "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE"
    printf 'transient list-sessions failure\n' >&2
    exit 5
  fi
  printf '%s [Created 1m ago]\n' "$RIMZ_TEST_SESSION_NAME"
  exit 0
fi

if [ "$1" = "--session" ] && [ "$3" = "action" ] && [ "$4" = "list-panes" ]; then
  if [ -n "$RIMZ_TEST_ZELLIJ_LIST_PANES_SLEEP" ]; then
    exec /bin/sleep "$RIMZ_TEST_ZELLIJ_LIST_PANES_SLEEP"
  fi
  if [ -n "$RIMZ_TEST_ZELLIJ_LIST_PANES" ]; then
    printf '%s\n' "$RIMZ_TEST_ZELLIJ_LIST_PANES"
    exit 0
  fi
  printf 'simulated wedged list-panes\n' >&2
  exit 5
fi

exit 0
	"#
}

fn fake_tmux_script() -> &'static str {
    r#"#!/bin/sh
if [ "$1" = "-S" ]; then
  shift 3
fi
if [ -n "$RIMZ_TEST_TMUX_LOG" ]; then
  printf '%s\n' "$*" >> "$RIMZ_TEST_TMUX_LOG"
fi
if [ "$1" = "-V" ]; then
  printf 'tmux 3.5\n'
  exit 0
fi
if [ "$1" = "list-sessions" ]; then
  exit 0
fi
exit 0
"#
}

fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path)
        .unwrap_or_else(|err| panic!("fake mux metadata {}: {err}", path.display()))
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .unwrap_or_else(|err| panic!("chmod fake mux {}: {err}", path.display()));
}

fn read_trace_lines(log_path: &Path, timeout: Duration) -> Vec<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(bytes) = fs::read(log_path) {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<String> = text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
            if !lines.is_empty() {
                return lines;
            }
        }
        if std::time::Instant::now() > deadline {
            return Vec::new();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
