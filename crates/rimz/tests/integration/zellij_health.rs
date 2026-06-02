//! Regression coverage for Zellij rooms that are live but cannot be inspected.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rimz::workspace::WorkspaceResolver;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn uninspectable_live_zellij_room_is_not_auto_deleted() {
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
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .bounded_output()
        .expect("run rimz start");

    assert!(
        !output.status.success(),
        "a noninteractive stuck room should fail fast before attach"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No terminal is available to confirm"),
        "stderr should explain the reset precondition, got: {stderr}"
    );

    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("action\tlist-panes\t-j\t-a")),
        "the health gate should probe panes, got: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("delete-session")),
        "an uninspectable live room must be preserved until reset confirmation: {lines:?}"
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
        let mut perms = fs::metadata(&bin)
            .expect("fake zellij metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).expect("chmod fake zellij");
        fs::write(&log, "").expect("create fake zellij log");
        Self {
            _home: home,
            bin_dir,
            bin,
            log,
        }
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
