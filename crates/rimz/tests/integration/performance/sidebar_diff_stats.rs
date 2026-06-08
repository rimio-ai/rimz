//! Cross-process single-flight proof for the sidebar's per-worktree git probes.
//!
//! A multi-tab session runs one `rimz sidebar serve` per tab, each forking
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
/// over the shared [`Env`] (XDG roots + workspace id). Shared with the
/// enrichment-cadence guards (`performance/enrichment_cadence.rs`), which
/// drive the same produce path against the same shim.
pub(crate) struct Fixture {
    pub(crate) env: Env,
    panes_path: PathBuf,
    git_log: PathBuf,
    real_git: PathBuf,
    patched_path: OsString,
}

impl Fixture {
    /// Build the fixture, or `None` when git is unavailable (the test self-skips
    /// like the mux-binary suites).
    pub(crate) fn new() -> Option<Self> {
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
            spawn_command: None,
            cwd: Some(worktree.to_string_lossy().into_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
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
    pub(crate) fn snapshot_command(&self) -> Command {
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd
    }

    pub(crate) fn run_snapshot(&self) -> Output {
        self.snapshot_command()
            .output()
            .expect("spawn rimz sidebar snapshot")
    }

    /// Make the room root itself a git repo, so a subsequent
    /// [`Env::record`](crate::common::Env::record) classifies the workspace as
    /// a repo room and the produce enumerates checkouts with `git worktree
    /// list`. The bare root records as a directory room, whose enumeration is
    /// one `read_dir` and forks no git at all.
    pub(crate) fn init_repo_room(&self) -> bool {
        Command::new(&self.real_git)
            .arg("-C")
            .arg(&self.env.project_root)
            .args(["init", "-q", "-b", "main"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
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
    pub(crate) fn git_forks(&self, marker: &str) -> usize {
        std::fs::read_to_string(&self.git_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(marker))
            .count()
    }

    pub(crate) fn git_log_len(&self) -> usize {
        self.git_log_contents()
            .lines()
            .filter(|line| !line.is_empty())
            .count()
    }

    /// The raw trace log, for assertion diagnostics.
    pub(crate) fn git_log_contents(&self) -> String {
        std::fs::read_to_string(&self.git_log).unwrap_or_default()
    }
}

/// Across a burst of concurrent snapshot processes, one producer forks git and
/// publishes the cache. Warm repeat snapshots and `--no-produce` consumers read
/// that cache without new git forks.
#[test]
fn diff_stats_single_flights_and_serves_warm_consumers() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    const TABS: usize = 4;
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
    assert_feature_group(&outputs[0].stdout, "cold concurrent snapshot");

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
    assert_eq!(
        fixture.git_forks("--untracked-files=all"),
        1,
        "the status probe must fork once across {TABS} sidebars, not {TABS}×:\n{}",
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
    assert_feature_group(&warm.stdout, "warm snapshot");

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
        after_cold,
        "a --no-produce consumer must fork zero git (log unchanged):\n{}",
        std::fs::read_to_string(&fixture.git_log).unwrap_or_default(),
    );
    assert_feature_group(&consumer.stdout, "no-produce snapshot");
}

fn assert_feature_group(stdout: &[u8], context: &str) {
    let parsed: Value = serde_json::from_slice(stdout).expect("snapshot json");
    let group = parsed["worktree_groups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["label"] == "feature-migration")
        })
        .unwrap_or_else(|| {
            panic!("expected a feature-migration worktree group in {context}:\n{parsed:#}")
        });
    assert_eq!(
        group["diff_added"], 2,
        "the {context} group carries the worktree's +2 over trunk"
    );
    assert_eq!(
        group["clean"],
        Value::Bool(true),
        "the {context} group carries the fully-committed tree's clean verdict"
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
