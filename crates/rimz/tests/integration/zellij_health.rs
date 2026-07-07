//! Regression coverage for Zellij rooms that are live but cannot be inspected.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rimz::workspace::WorkspaceResolver;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn uninspectable_live_zellij_room_attaches_as_is() {
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");
    let shim = FakeZellij::new();

    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250")
        // The uninspectable room legitimately holds `start` for the full
        // pre-attach topology ceiling before attaching as-is, so keep the outer
        // test bound roomy while shortening the fake-shim probe.
        .bounded_output_within(Duration::from_secs(30))
        .expect("run rimz start");

    assert!(output.status.success(), "live room should attach as-is");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("No terminal is available to confirm"),
        "live room should not reach reset, got: {stderr}"
    );

    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    assert!(
        !lines.iter().any(|line| line.contains("delete-session")),
        "an uninspectable live room must be preserved: {lines:?}"
    );
}

#[test]
fn attach_retries_transient_zellij_session_listing_before_default_mux_fallback() {
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");
    let shim = FakeZellij::new().with_tmux();
    let fail_once = shim.log.with_extension("list-sessions-fail-once");

    let output = env
        .rimz()
        .args(["attach", workspace.session_name.as_str(), "--print"])
        .env("PATH", shim.bin_dir.path())
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_SESSION_NAME", &workspace.session_name)
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
  if [ -n "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE" ] && [ ! -e "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE" ]; then
    : > "$RIMZ_TEST_ZELLIJ_LIST_SESSIONS_FAIL_ONCE"
    printf 'transient list-sessions failure\n' >&2
    exit 5
  fi
  printf '%s [Created 1m ago]\n' "$RIMZ_TEST_SESSION_NAME"
  exit 0
fi

if [ "$1" = "--session" ] && [ "$3" = "action" ] && [ "$4" = "list-panes" ]; then
  printf 'simulated wedged list-panes\n' >&2
  exit 5
fi

if [ "$1" = "--session" ] && [ "$3" = "action" ] && [ "$4" = "query-tab-names" ]; then
  printf 'main\n'
  exit 0
fi

exit 0
	"#
}

fn fake_tmux_script() -> &'static str {
    r#"#!/bin/sh
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
