//! Multiplexer abstraction.
//!
//! Everything correctness-critical (ledger, bridge, hooks, schemas) sits
//! above this trait and is identical across backends. Raw pane IDs live
//! only inside the adapter — see [`crate::ids::PaneId`] for the normalized
//! form that travels everywhere else.

mod selection;
pub mod tmux;
pub mod zellij;

pub use selection::auto_detect_backend;
pub use tmux::TmuxBackend;
pub use zellij::ZellijBackend;

use std::collections::BTreeMap;
use std::io;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, WorkspaceId};

#[derive(Debug, thiserror::Error)]
pub enum MuxErr {
    #[error("multiplexer command `{program}` not found on PATH")]
    NotInstalled { program: String },
    #[error("no multiplexer found: install zellij or tmux")]
    NoMuxFound,
    #[error("multiplexer command failed: {program} {args}: {stderr}")]
    Command {
        program: String,
        args: String,
        stderr: String,
    },
    #[error("pane id `{pane_id}` belongs to `{actual}`, but `{expected}` backend was selected")]
    PaneBackendMismatch {
        pane_id: PaneId,
        expected: MuxName,
        actual: MuxName,
    },
    #[error("could not parse mux output from `{program}`: {reason}")]
    Output { program: String, reason: String },
    #[error("multiplexer command `{program} {args}` did not finish within {seconds}s; killed")]
    Timeout {
        program: String,
        args: String,
        seconds: u64,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, MuxErr>;

/// Upper bound on a single control-command round-trip ([`CommandSpec::run`]).
/// Generous — a real `zellij`/`tmux` control command answers in milliseconds, so
/// this only ever fires on a wedged child (a Zellij action client spinning
/// against a dead server), bounding the hang instead of letting it run forever.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll granularity while waiting for a control command to exit — the most
/// latency [`CommandSpec::run_bounded`] adds on the common (fast) path.
const POLL_STEP: Duration = Duration::from_millis(10);

/// A built-up command we can run or hand back to `exec(3)`.
#[derive(Clone, Debug, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(&self.env);
        command
    }

    /// Run the command to completion and capture its output, bounded by
    /// [`COMMAND_TIMEOUT`]. A control command (`zellij action …`, `tmux …`)
    /// finishes in milliseconds; exceeding the bound means it wedged — a Zellij
    /// action client busy-loops at 100% CPU when its session server dies, which
    /// would otherwise hang the caller (and `rimz start`) forever. On the bound
    /// the child is SIGKILLed and a [`MuxErr::Timeout`] returned, so callers — all
    /// of which treat these best-effort — degrade instead of blocking. The
    /// interactive attach never comes through here (it `exec`s).
    pub fn run(&self) -> Result<Output> {
        let output = self.run_bounded(COMMAND_TIMEOUT)?;
        if !output.status.success() {
            return Err(MuxErr::Command {
                program: self.program.clone(),
                args: self.args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(output)
    }

    /// Spawn the child and wait at most `timeout` for it. Its stdout/stderr are
    /// drained on threads so a full pipe never deadlocks the wait, while the child
    /// handle stays here so a deadline can `kill()` it. Polling `try_wait` adds at
    /// most [`POLL_STEP`] of latency on the common (fast) path; on the deadline the
    /// child is killed and reaped and a [`MuxErr::Timeout`] returned.
    fn run_bounded(&self, timeout: Duration) -> Result<Output> {
        let mut child = self
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => MuxErr::NotInstalled {
                    program: self.program.clone(),
                },
                _ => MuxErr::Io(err),
            })?;
        let drain = |pipe: Option<Box<dyn io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn io::Read + Send>),
        );
        let stderr = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn io::Read + Send>),
        );
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let stdout = stdout.join().unwrap_or_default();
                    let stderr = stderr.join().unwrap_or_default();
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout.join();
                    let _ = stderr.join();
                    return Err(MuxErr::Timeout {
                        program: self.program.clone(),
                        args: self.args.join(" "),
                        seconds: timeout.as_secs(),
                    });
                }
                None => std::thread::sleep(POLL_STEP),
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneCapture {
    pub pane_id: PaneId,
    pub raw_text: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PaneListOptions {
    pub session_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub session_name: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SidebarPaneOptions {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub width_percent: u16,
    pub rimz_bin: PathBuf,
    pub replace_existing: bool,
}

/// Tally of one in-place sidebar recovery pass ([`MuxBackend::recover_sidebars`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views (Zellij tabs / tmux windows) that had lost their sidebar and got
    /// one re-added in place this pass.
    pub recovered: usize,
    /// Views that needed a sidebar but whose in-place add failed — logged and
    /// skipped, never retried.
    pub failed: usize,
}

/// Group `(view_id, is_sidebar)` pane classifications and return the view ids
/// that hold at least one working (non-sidebar) pane but no sidebar pane — the
/// views whose sidebar was lost and should gain one in place. First-seen order,
/// each view once. Shared by both backends so the "which views lost a sidebar"
/// rule lives in exactly one place.
pub(crate) fn views_missing_sidebar(classified: &[(String, bool)]) -> Vec<String> {
    use std::collections::HashMap;

    let mut order: Vec<String> = Vec::new();
    // view -> (has_working_pane, has_sidebar_pane)
    let mut state: HashMap<String, (bool, bool)> = HashMap::new();
    for (view, is_sidebar) in classified {
        let entry = state.entry(view.clone()).or_insert_with(|| {
            order.push(view.clone());
            (false, false)
        });
        if *is_sidebar {
            entry.1 = true;
        } else {
            entry.0 = true;
        }
    }
    order
        .into_iter()
        .filter(|view| {
            let (has_work, has_sidebar) = state[view];
            has_work && !has_sidebar
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct SplitPaneOptions {
    pub target_pane_id: Option<PaneId>,
    pub cwd: Option<String>,
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
}

/// One managed long-lived process the daemon view hosts beside the sidebar — the
/// Claude remote-control host, or the Codex app-server broker. The view stacks
/// every host to the right of the global sidebar.
#[derive(Clone, Debug)]
pub struct HostPane {
    /// Host argv, program first.
    pub argv: Vec<String>,
    /// Working directory the host runs in. The Claude host runs from the project
    /// root so `--spawn=worktree` carves new sessions off the canonical repo (not
    /// the current worktree); the broker runs from the worktree — so each pane
    /// carries its own cwd.
    pub cwd: PathBuf,
}

/// Options for launching the managed daemon hosts into a single dedicated, named
/// *view* of a session — a tmux window or a Zellij tab — forced to the first
/// position and out of the user's focus. The view is born with the global
/// sidebar docked on its left and the hosts on its right, mirroring the working
/// tab's `sidebar | shell` shape, so every host is reachable and never traps the
/// user in a bare pane. It hosts the Claude remote-control host and/or the
/// per-session Codex app-server broker.
#[derive(Clone, Debug)]
pub struct BackgroundViewOptions {
    /// View name. Doubles as the idempotency key: a live view by this name in
    /// the session suppresses a relaunch.
    pub name: String,
    /// The hosts the view runs, left to right beside the sidebar. Must be
    /// non-empty; the first host takes focus within the view. The caller decides
    /// whether to open the view at all (it skips an empty host list).
    pub hosts: Vec<HostPane>,
    /// The global sidebar docked on the view's left. Carries the session name
    /// (which is also the view's session), the workspace identity, the width, and
    /// the `rimz` bin the sidebar renderer runs.
    pub sidebar: SidebarPaneOptions,
}

/// The daemon view (the `rimzd` tab/window) to birth *ahead* of the working
/// view, in the same session-creation step. On Zellij this is the only way the
/// view can lead — Zellij can't reorder tabs after birth, so the lead position
/// is owned by the birth layout, not a later move. tmux can reorder freely, so
/// it ignores this and leads via [`MuxBackend::open_background_view`] instead.
/// Only `rimz start` supplies one; every other sidebar launch passes `None`, and
/// the working view leads as before.
#[derive(Clone, Debug)]
pub struct DaemonView {
    /// View name — the idempotency key, matching [`BackgroundViewOptions::name`].
    pub name: String,
    /// The hosts the view runs, left to right beside the sidebar; the first takes
    /// focus within the view. Non-empty (the caller skips an empty host list).
    pub hosts: Vec<HostPane>,
}

/// Outcome of [`MuxBackend::open_background_view`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundViewLaunch {
    /// A view by this name was already present; nothing was launched.
    AlreadyRunning,
    /// A fresh view was launched.
    Launched,
}

/// Backend-neutral mux operations. Every Zellij/tmux command lives behind
/// one of these methods.
pub trait MuxBackend: Send + Sync {
    fn name(&self) -> MuxName;
    fn ensure_session(&self, opts: &SessionOptions) -> Result<()>;
    fn attach_command(&self, name: &str) -> CommandSpec;
    fn detach(&self, name: &str) -> Result<()>;
    /// Force-remove a session by name. A missing session is success — the goal
    /// state is "no session by that name", so callers can retire a stale or
    /// renamed session idempotently.
    fn kill_session(&self, name: &str) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<String>>;
    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>>;
    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()>;
    fn focus_pane(&self, pane: &PaneId) -> Result<()>;
    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture>;
    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()>;
    /// Birth (or heal) the session's working view with its sidebar. When `daemon`
    /// is `Some`, the session is born with that view leading and the working view
    /// focused second — on Zellij the lead order is fixed here, at birth, since
    /// tabs can't be reordered afterwards. tmux ignores `daemon` (it leads its
    /// window via [`Self::open_background_view`]). Only `rimz start` passes a
    /// `daemon`; other launches pass `None` and birth the working view alone.
    fn open_sidebar(&self, opts: &SidebarPaneOptions, daemon: Option<&DaemonView>) -> Result<()>;
    /// Re-add a sidebar to every view (Zellij tab / tmux window) that holds
    /// working panes but lost its sidebar, in place and without disturbing
    /// existing panes. One best-effort pass: a view whose add fails is logged
    /// and skipped — never retried, never a session rebirth. Unlike
    /// [`Self::open_sidebar`], this never deletes or recreates the session.
    fn recover_sidebars(&self, opts: &SidebarPaneOptions) -> Result<SidebarRecovery>;
    /// Launch the `opts.hosts` in one dedicated, named background view (tmux
    /// window / Zellij tab) of an existing session, born `sidebar | hosts…`,
    /// forced to the first position, and out of the user's focus. Idempotent: a
    /// second call while a view of that name is present launches nothing, but
    /// still re-asserts its first position and returns focus to the working view
    /// so a relaunch never strands the user on the daemon view. The view never
    /// gates correctness — a failure here leaves the room intact.
    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch>;
    /// Best-effort wakeup; sockets are the channel of record per the docs.
    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()>;
    fn version(&self) -> Result<String>;
}

/// Construct a boxed backend for the named multiplexer.
pub fn backend_for(mux: MuxName) -> Box<dyn MuxBackend> {
    match mux {
        MuxName::Zellij => Box::new(ZellijBackend::new()),
        MuxName::Tmux => Box::new(TmuxBackend::new()),
    }
}

pub(crate) fn ensure_pane_backend(pane: &PaneId, expected: MuxName) -> Result<()> {
    let actual = pane.mux();
    if actual != expected {
        return Err(MuxErr::PaneBackendMismatch {
            pane_id: pane.clone(),
            expected,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_missing_sidebar_flags_only_worked_views_without_one() {
        let classified = vec![
            // tab 12: a working pane, no sidebar -> needs recovery.
            ("12".to_owned(), false),
            // tab 15: sidebar + working pane -> healthy.
            ("15".to_owned(), true),
            ("15".to_owned(), false),
            // tab 16: sidebar only (no working pane) -> nothing to serve, skip.
            ("16".to_owned(), true),
            // tab 17: two working panes, no sidebar -> needs recovery, listed once.
            ("17".to_owned(), false),
            ("17".to_owned(), false),
        ];
        assert_eq!(
            views_missing_sidebar(&classified),
            vec!["12".to_owned(), "17".to_owned()],
            "only views with work but no sidebar, in first-seen order, deduped"
        );
    }

    #[test]
    fn views_missing_sidebar_is_empty_when_every_view_is_healthy() {
        let classified = vec![
            ("a".to_owned(), true),
            ("a".to_owned(), false),
            ("b".to_owned(), false),
            ("b".to_owned(), true),
        ];
        assert!(views_missing_sidebar(&classified).is_empty());
    }

    #[test]
    fn pane_backend_mismatch_is_rejected_before_running_mux_command() {
        let pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let err = ensure_pane_backend(&pane, MuxName::Tmux).unwrap_err();
        assert!(matches!(
            err,
            MuxErr::PaneBackendMismatch {
                expected: MuxName::Tmux,
                actual: MuxName::Zellij,
                ..
            }
        ));
    }
}
