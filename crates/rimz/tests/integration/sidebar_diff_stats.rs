//! Cross-process single-flight proof for the sidebar's per-worktree git probes.
//!
//! A multi-tab session runs one `rimz-sidebar serve` per tab, each forking
//! `rimz sidebar snapshot --json`, which runs git per worktree (trunk ref →
//! merge-base → numstat → branch). Those probes are single-flighted across the
//! fleet: one elected producer forks git and writes the shared `diff-stats.json`
//! cache; the rest read it back.
//!
//! No live mux needed. We point `RIMZ_TEST_PANE_LIST` at a one-pane fixture
//! whose `cwd` is a real on-disk git worktree (bypassing `list-panes` while
//! keeping the git path), and put a `git`-shaped trace shim
//! (`tests/fixtures/git-trace`) first on the snapshot process's PATH. The shim
//! logs each `git` argv then execs the real git, so the log line count is the
//! true cross-process git fork rate.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rimz::feed::PaneRef;
use rimz::ids::{MuxName, PaneId};
use serde_json::Value;

use crate::common::Env;

const SESSION: &str = "rimz-diff-stats";

/// One snapshot process's worktree, pane fixture, and git-shim wiring, layered
/// over the shared [`Env`] (XDG roots + workspace id).
struct Fixture {
    env: Env,
    panes_path: PathBuf,
    git_log: PathBuf,
    real_git: PathBuf,
    patched_path: OsString,
}

impl Fixture {
    /// Build the fixture, or `None` when git is unavailable (the test self-skips
    /// like the mux-binary suites).
    fn new() -> Option<Self> {
        let env = Env::new();
        let real_git = find_git()?;

        let worktree = env.project_root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("mkdir worktree");
        if !init_git_worktree(&worktree, &real_git) {
            eprintln!("git init failed; skipping diff-stats single-flight test");
            return None;
        }

        // One pane whose cwd is the worktree. With no recorded workspace, the
        // snapshot's project root is `None`, so the cwd groups as a `Worktree`
        // and the git probes run against it.
        let pane = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_0"),
            session_name: SESSION.to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: Some("bash".to_owned()),
            cwd: Some(worktree.to_string_lossy().into_owned()),
            pane_pid: None,
            pane_process_start: None,
            view_active: None,
            session_attached: None,
        };
        let panes_path = env.project_root.join("panes.json");
        std::fs::write(
            &panes_path,
            serde_json::to_vec(&[pane]).expect("serialize panes"),
        )
        .expect("write panes fixture");

        // A `git`-named link to the trace shim, first on PATH.
        let bin_dir = env.project_root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
        let git_link = bin_dir.join("git");
        std::os::unix::fs::symlink(git_trace_shim(), &git_link).expect("symlink git shim");
        let patched_path = prepend_path(&bin_dir);
        let git_log = env.project_root.join("git-trace.log");

        Some(Self {
            env,
            panes_path,
            git_log,
            real_git,
            patched_path,
        })
    }

    /// A `rimz sidebar snapshot --json` command wired to the fixture: the pane
    /// list, the git shim on PATH, and the trace log.
    fn snapshot_command(&self) -> Command {
        let mut cmd = self.env.rimz();
        cmd.args([
            "sidebar",
            "snapshot",
            "--json",
            "--workspace-id",
            self.env.workspace_id.as_str(),
            "--session-name",
            SESSION,
        ])
        .env("RIMZ_TEST_PANE_LIST", &self.panes_path)
        .env("RIMZ_TEST_GIT_LOG", &self.git_log)
        .env("RIMZ_TEST_REAL_GIT", &self.real_git)
        .env("PATH", &self.patched_path)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd
    }

    fn run_snapshot(&self) -> Output {
        self.snapshot_command()
            .output()
            .expect("spawn rimz sidebar snapshot")
    }

    /// A read-only consumer snapshot: the same command plus `--no-produce`, the
    /// flag a younger per-tab renderer passes so it renders from the elder's
    /// published cache and never forks `list-panes`/git.
    fn run_no_produce_snapshot(&self) -> Output {
        let mut cmd = self.snapshot_command();
        cmd.arg("--no-produce");
        cmd.output()
            .expect("spawn rimz sidebar snapshot --no-produce")
    }

    /// Trace-log lines mentioning the per-worktree `git` forks, by marker.
    fn git_forks(&self, marker: &str) -> usize {
        std::fs::read_to_string(&self.git_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(marker))
            .count()
    }

    fn git_log_len(&self) -> usize {
        std::fs::read_to_string(&self.git_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .count()
    }
}

/// Across a burst of concurrent snapshot processes — one per tab, all on a cold
/// cache — the per-worktree `merge-base`/`numstat` forks happen once, not once
/// per process. One producer forks git and publishes the cache; the rest read
/// it back.
#[test]
fn concurrent_snapshots_single_flight_the_git_probes() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    const TABS: usize = 6;
    let children: Vec<_> = (0..TABS)
        .map(|_| fixture.snapshot_command().spawn().expect("spawn snapshot"))
        .collect();
    let outputs: Vec<Output> = children
        .into_iter()
        .map(|child| child.wait_with_output().expect("wait snapshot"))
        .collect();
    for output in &outputs {
        assert!(
            output.status.success(),
            "sidebar snapshot failed:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Sanity: every process emitted the same worktree group, enriched from the
    // one shared git read (+2 over the merge-base, live branch label).
    let parsed: Value = serde_json::from_slice(&outputs[0].stdout).expect("snapshot json");
    let group = parsed["worktree_groups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["label"] == "feature-migration")
        })
        .unwrap_or_else(|| panic!("expected a feature-migration worktree group:\n{parsed:#}"));
    assert_eq!(
        group["diff_added"], 2,
        "the group carries the worktree's +2 over trunk"
    );

    // The whole point: the merge-base and numstat forks ran once across all
    // tabs, not once each.
    assert_eq!(
        fixture.git_forks("merge-base"),
        1,
        "merge-base must fork once across {TABS} sidebars, not {TABS}×:\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
    assert_eq!(
        fixture.git_forks("numstat"),
        1,
        "numstat must fork once across {TABS} sidebars, not {TABS}×:\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
}

/// A second snapshot inside the diff-stats TTL window reuses the cache and forks
/// zero git — the log is unchanged from the cold run.
#[test]
fn repeat_snapshot_within_ttl_forks_no_git() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    let cold = fixture.run_snapshot();
    assert!(
        cold.status.success(),
        "cold snapshot failed:\n{}",
        String::from_utf8_lossy(&cold.stderr),
    );
    assert!(
        fixture.git_forks("merge-base") >= 1,
        "the cold run must fork git for the worktree:\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
    let after_cold = fixture.git_log_len();

    let warm = fixture.run_snapshot();
    assert!(
        warm.status.success(),
        "warm snapshot failed:\n{}",
        String::from_utf8_lossy(&warm.stderr),
    );
    assert_eq!(
        fixture.git_log_len(),
        after_cold,
        "a repeat snapshot within the TTL must fork zero git (log unchanged):\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
}

/// A `--no-produce` consumer (a younger per-tab renderer) forks zero git: it
/// projects the elder's published `diff-stats.json` and never runs the
/// per-worktree probes. Warm the cache with one producing snapshot, then assert
/// a `--no-produce` run adds no git-trace lines yet still carries the cached +2.
/// This is the read-side of "one producer per workspace, one renderer per tab":
/// every extra tab costs a cache read, not a `list-panes`/git round-trip.
#[test]
fn no_produce_consumer_forks_no_git_and_reads_cache() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    // Warm: a producing snapshot forks git and publishes the diff-stats cache.
    let warm = fixture.run_snapshot();
    assert!(
        warm.status.success(),
        "warm snapshot failed:\n{}",
        String::from_utf8_lossy(&warm.stderr),
    );
    assert!(
        fixture.git_forks("merge-base") >= 1,
        "the warm run must fork git to publish the cache:\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
    let after_warm = fixture.git_log_len();

    // Consumer: `--no-produce` must fork zero git (log unchanged) and still
    // render the cached worktree group with its +2 over trunk.
    let consumer = fixture.run_no_produce_snapshot();
    assert!(
        consumer.status.success(),
        "no-produce snapshot failed:\n{}",
        String::from_utf8_lossy(&consumer.stderr),
    );
    assert_eq!(
        fixture.git_log_len(),
        after_warm,
        "a --no-produce consumer must fork zero git (log unchanged):\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
    let parsed: Value = serde_json::from_slice(&consumer.stdout).expect("snapshot json");
    let group = parsed["worktree_groups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["label"] == "feature-migration")
        })
        .unwrap_or_else(|| panic!("expected a feature-migration worktree group:\n{parsed:#}"));
    assert_eq!(
        group["diff_added"], 2,
        "the consumer renders the cached +2 without forking git"
    );
}

/// Initialize a git worktree on `main` with a `feature-migration` branch checked
/// out carrying a +2 diff over the trunk merge-base. Returns `false` if git is
/// too old / unavailable, so the caller self-skips.
fn init_git_worktree(dir: &Path, git_bin: &Path) -> bool {
    let git = |args: &[&str]| {
        Command::new(git_bin)
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    // `-b main` needs Git >= 2.28; an older git fails here and the test skips.
    if !git(&["init", "-q", "-b", "main"]) {
        return false;
    }
    let _ = git(&["config", "user.email", "t@example.com"]);
    let _ = git(&["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "a\nb\nc\n").expect("write base");
    let _ = git(&["add", "base.txt"]);
    let _ = git(&["commit", "-q", "-m", "base"]);
    if !git(&["checkout", "-q", "-b", "feature-migration"]) {
        return false;
    }
    std::fs::write(dir.join("feat.txt"), "x\ny\n").expect("write feat");
    let _ = git(&["add", "feat.txt"]);
    git(&["commit", "-q", "-m", "feature work"])
}

/// The built `git-trace` shim binary (Cargo exports the path to every declared
/// `[[bin]]`).
fn git_trace_shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-trace"))
}

/// First `git` on the test process's own (unpatched) PATH — the real binary the
/// shim execs. `None` when git is not installed.
fn find_git() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
}

/// PATH with `dir` prepended, so a bare `git` resolves to the shim first.
fn prepend_path(dir: &Path) -> OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut prepended = OsString::from(dir);
    prepended.push(":");
    prepended.push(&original);
    prepended
}
