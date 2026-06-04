//! Multiplexer abstraction.
//!
//! Everything correctness-critical (ledger, bridge, hooks, schemas) sits
//! above this trait and is identical across backends. Raw pane IDs live
//! only inside the adapter — see [`crate::ids::PaneId`] for the normalized
//! form that travels everywhere else.

pub mod recovery;
mod selection;
pub mod tmux;
pub mod zellij;

pub use selection::auto_detect_backend;
pub use tmux::TmuxBackend;
pub use zellij::ZellijBackend;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::io::Read as _;
use std::num::NonZeroU16;
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
        self.run_with_timeout(COMMAND_TIMEOUT)
    }

    /// Like [`Self::run`], but with a caller-chosen bound. The health probe at
    /// `rimz start` uses a tight one so a wedged action client (spinning against
    /// a dead server) is killed in a few seconds rather than stalling the launch
    /// for the full [`COMMAND_TIMEOUT`].
    pub fn run_with_timeout(&self, timeout: Duration) -> Result<Output> {
        let output = self.run_bounded(timeout)?;
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
    /// Override the backend's default subprocess timeout. `None` uses the
    /// backend's default (30s). Set to a shorter value for latency-sensitive
    /// probes (e.g. the self-close watchdog) where a hung Zellij should not
    /// block the caller for the full timeout.
    pub command_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub session_name: String,
    pub cwd: PathBuf,
    pub config: crate::config::MultiplexerConfig,
}

/// Default sidebar width as a percentage of the view. The single source of
/// truth for both the CLI launch paths and the user-wide reload reconcile.
const DEFAULT_SIDEBAR_WIDTH_PERCENT: u16 = 30;

/// Sidebar pane width: a percentage of the view, capped at `max_cols` columns
/// (`sidebar.max_cols`). Panes are born at the percentage — a Zellij layout
/// spells fixed sizes or percentages but never `min(percent, columns)`, and a
/// detached tmux session births at default geometry where an absolute size
/// would be wrong — so the cap lands as one creation-time
/// [`MuxBackend::resize_sidebar_pane`] fired by the freshly-born renderer.
/// Creation-time only: a manual resize afterwards sticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarWidth {
    /// Percentage of the view width — tracks terminal size below the cap.
    pub percent: u16,
    /// Column cap the percentage never exceeds (`sidebar.max_cols`).
    pub max_cols: NonZeroU16,
}

impl SidebarWidth {
    /// The width a machine config asks for: the default percentage at the
    /// configured column cap.
    pub fn from_config(sidebar: &crate::config::SidebarConfig) -> Self {
        Self {
            percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
            max_cols: sidebar.max_cols,
        }
    }

    /// The capped target in columns for a view `total_cols` wide:
    /// `min(percent, max_cols)`.
    pub fn target_cols(self, total_cols: u64) -> u64 {
        let percent = (total_cols * u64::from(self.percent.clamp(10, 90)) / 100).max(1);
        percent.min(self.cap_cols())
    }

    /// The column cap alone — the threshold above which a freshly-born pane is
    /// shrunk once.
    pub fn cap_cols(self) -> u64 {
        u64::from(self.max_cols.get())
    }
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self::from_config(&crate::config::SidebarConfig::default())
    }
}

#[derive(Clone, Debug)]
pub struct SidebarPaneOptions {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub width: SidebarWidth,
    pub rimz_bin: PathBuf,
    pub replace_existing: bool,
    pub config: crate::config::MultiplexerConfig,
    /// Prior agents the reborn session re-seeds, one running pane each, so a
    /// rebirth comes back where the user left off instead of empty. Empty on
    /// every launch that births nothing to restore (first start, healthy
    /// reattach) — then the birth is exactly the bare working room. Built from
    /// the durable agent rollup by [`crate::resume::plan_resume`]; the backend
    /// seeds the panes and stays ignorant of agents and the ledger.
    pub resume_panes: Vec<ResumePane>,
}

/// One prior agent the reborn session re-seeds: a fresh pane running the
/// agent's resume CLI in its worktree, restoring the conversation idle (no
/// auto-prompt, no new token spend until the user types). Pure data — the
/// backend seeds `{command, cwd}` and knows nothing of agents or the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePane {
    /// Resume argv, program first — e.g. `["claude", "--resume", "<uuid>"]`.
    pub command: Vec<String>,
    /// The agent's worktree: the cwd the resumed pane runs in.
    pub cwd: PathBuf,
    /// Short display and view label, e.g. `claude:feature-migration`. Doubles
    /// as the Zellij tab / tmux window name and the seed's idempotency key.
    pub label: String,
}

/// Tally of one in-place sidebar reconcile pass ([`MuxBackend::reconcile_sidebars`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views (Zellij tabs / tmux windows) that gained a sidebar this pass —
    /// because they had none, or their only sidebar was unresponsive and was
    /// closed first.
    pub recovered: usize,
    /// Duplicate or unresponsive sidebar panes closed so each view keeps exactly
    /// one live sidebar.
    pub closed: usize,
    /// Views that needed a sidebar but whose in-place add failed — logged and
    /// skipped, never retried.
    pub failed: usize,
}

/// The live sidebars the runtime knows about when a reconcile runs: the panes a
/// fresh, current-protocol heartbeat claims, and whether any fresh heartbeat is
/// *unlocated* (carries no pane id — an old/edge renderer with no per-pane env).
/// An unlocated live sidebar is a wildcard for the last physical sidebar in a
/// view: reconcile keeps one possible owner, while duplicate panes still close
/// so one view never carries multiple sidebars.
#[derive(Clone, Debug, Default)]
pub struct SidebarLiveness {
    pub claimed_panes: HashSet<PaneId>,
    pub has_unlocated: bool,
}

/// One view's sidebar panes (in mux order) and how it is otherwise occupied: a
/// user-working pane (neither a sidebar nor a managed daemon host), and/or a
/// managed daemon host. A view with neither is sidebar-only — an orphan to
/// collapse; one with a daemon host is the intentional `rimzd` view.
pub(crate) struct ViewSidebars {
    pub view: String,
    pub sidebar_panes: Vec<PaneId>,
    pub has_working: bool,
    pub has_daemon_host: bool,
}

/// What a reconcile must do to converge one session to a single live sidebar per
/// working view: close these sidebar panes (duplicates + unclaimed/unresponsive),
/// then add a sidebar to these views (none survived, or none existed).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcilePlan {
    pub close: Vec<PaneId>,
    pub add: Vec<String>,
}

/// Plan the reconcile for one session, view by view:
/// - **Working view** — keep exactly one *claimed* (live) sidebar pane, close the
///   rest, and add one if none survived, so duplicates collapse to one and a
///   wedged sidebar is replaced.
/// - **Orphan sidebar-only view** — no working pane and no daemon host, so its
///   working siblings all closed but the sidebar never self-closed (a wedged
///   renderer that stopped ticking). Close every sidebar pane and let the view
///   collapse; reload cannot rely on self-close for a renderer that is no longer
///   ticking.
/// - **Daemon view** — a sidebar beside managed daemon hosts (`rimzd`) is
///   intentional; leave it alone.
///
/// When a live sidebar is unlocated (a fresh heartbeat carrying no pane id), each
/// view is handled conservatively: keep one physical sidebar as the possible
/// owner, close duplicate panes, add only when a working view has none, and leave
/// a single orphan for self-close.
/// First-seen order; shared by both backends so the rule lives in one place and
/// is unit-tested without a mux.
pub(crate) fn plan_reconcile(views: &[ViewSidebars], live: &SidebarLiveness) -> ReconcilePlan {
    let mut plan = ReconcilePlan::default();
    for view in views {
        if view.has_working {
            let keep = sidebar_to_keep(view, live, live.has_unlocated);
            close_unkept_sidebars(view, keep, &mut plan.close);
            if keep.is_none() {
                plan.add.push(view.view.clone());
            }
        } else if view.has_daemon_host {
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            close_unkept_sidebars(view, keep, &mut plan.close);
        } else if live.has_unlocated {
            // Orphan sidebar-only view: keep one possible owner for self-close,
            // but still collapse duplicates so a tab never accumulates chrome.
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            close_unkept_sidebars(view, keep, &mut plan.close);
        } else {
            // Orphan sidebar-only view: close every sidebar pane so the view
            // collapses. Without a wildcard there is no live owner to preserve.
            plan.close.extend(view.sidebar_panes.iter().cloned());
        }
    }
    plan
}

fn sidebar_to_keep(
    view: &ViewSidebars,
    live: &SidebarLiveness,
    keep_unclaimed: bool,
) -> Option<usize> {
    view.sidebar_panes
        .iter()
        .position(|pane| live.claimed_panes.contains(pane))
        .or_else(|| (keep_unclaimed && !view.sidebar_panes.is_empty()).then_some(0))
}

fn close_unkept_sidebars(view: &ViewSidebars, keep: Option<usize>, close: &mut Vec<PaneId>) {
    close.extend(
        view.sidebar_panes
            .iter()
            .enumerate()
            .filter(|(index, _pane)| Some(*index) != keep)
            .map(|(_index, pane)| pane.clone()),
    );
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

/// Health verdict for a backend session. [`MuxBackend::probe_session_health`]
/// returns `Healthy` or `Stuck` (read-only); [`MuxBackend::ensure_clean_session`]
/// adds `Reborn` when it rebirthed a safely-rebuildable room into a clean one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionHealth {
    /// Clean and running — or absent, with nothing to heal.
    Healthy,
    /// Was auto-rebuildable; a rebirth brought it back clean and running.
    Reborn,
    /// Stuck and needs an explicit reset: either a rebirth could not clear it, or
    /// the live room cannot be inspected safely enough to auto-rebirth.
    Stuck,
}

/// Backend-neutral mux operations. Every Zellij/tmux command lives behind
/// one of these methods.
pub trait MuxBackend: Send + Sync {
    fn name(&self) -> MuxName;
    fn ensure_session(&self, opts: &SessionOptions) -> Result<()>;
    fn attach_command(&self, name: &str, config: &crate::config::MultiplexerConfig) -> CommandSpec;
    fn detach(&self, name: &str) -> Result<()>;
    /// Force-remove a session by name. A missing session is success — the goal
    /// state is "no session by that name", so callers can retire a stale or
    /// renamed session idempotently.
    fn kill_session(&self, name: &str) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<String>>;
    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>>;
    /// The pane each attached client is focused on — one entry per client (the
    /// caller dedups). This is the per-client focus set, distinct from
    /// [`PaneRef::is_focused`], which is the per-view active pane the mux marks
    /// once per tab/window. The sidebar focus mirror reads this so it tracks the
    /// user, not every tab's active pane. Best-effort enrichment, never a
    /// precondition: a backend that cannot answer returns an empty set and the
    /// mirror simply holds its current selection.
    fn client_focused_panes(&self, session: &str) -> Result<Vec<PaneId>> {
        let _ = session;
        Ok(Vec::new())
    }
    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()>;
    fn focus_pane(&self, pane: &PaneId) -> Result<()>;
    /// Best-effort resize of a live sidebar pane toward its configured width.
    /// This is how `sidebar.max_cols` lands: panes are born at the percentage
    /// (neither backend can spell `min(percent, columns)` at creation), and the
    /// freshly-born renderer fires this once when its pane exceeds the cap —
    /// session birth, every new tab/window, recovery. Creation-time only: the
    /// renderer never fires it again, so a manual resize sticks.
    fn resize_sidebar_pane(
        &self,
        session_name: &str,
        pane: &PaneId,
        width: SidebarWidth,
    ) -> Result<()>;
    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture>;
    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()>;
    /// Birth (or heal) the session's working view with its sidebar. When `daemon`
    /// is `Some`, the session is born with that view leading and the working view
    /// focused second — on Zellij the lead order is fixed here, at birth, since
    /// tabs can't be reordered afterwards. tmux ignores `daemon` (it leads its
    /// window via [`Self::open_background_view`]). Only `rimz start` passes a
    /// `daemon`; other launches pass `None` and birth the working view alone.
    fn open_sidebar(&self, opts: &SidebarPaneOptions, daemon: Option<&DaemonView>) -> Result<()>;
    /// Read-only health verdict for `name`'s room. Zellij detects a resurrected
    /// or suspended room (every command pane held at a "Waiting to run" prompt
    /// after a server death) and an uninspectable live room; tmux has no
    /// resurrection, so the default is always [`SessionHealth::Healthy`]. `rimz doctor` reports this;
    /// [`Self::ensure_clean_session`] acts on it. Never mutates the session.
    fn probe_session_health(&self, name: &str) -> Result<SessionHealth> {
        let _ = name;
        Ok(SessionHealth::Healthy)
    }
    /// Guarantee the next [`Self::attach_command`] lands on a clean, running
    /// room. Probe `opts.session_name`; a clean live room is left untouched
    /// ([`SessionHealth::Healthy`]); an absent, exited, or inspected-stale one is
    /// (re)birthed from the layout ([`SessionHealth::Reborn`]); a live room that
    /// cannot be inspected, or a room that a rebirth still cannot make clean,
    /// returns [`SessionHealth::Stuck`] so the caller can prompt for, or direct
    /// the user to, `rimz reset`. This is the authoritative pre-attach gate that
    /// the best-effort sidebar launch cannot bypass. tmux has no
    /// resurrection, so the default is a no-op `Healthy`.
    fn ensure_clean_session(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let _ = (opts, daemon);
        Ok(SessionHealth::Healthy)
    }
    /// Remove the backend's on-disk resurrection cache for `name`, returning the
    /// paths removed (for the `rimz reset` report). tmux has no such cache, so the
    /// default removes nothing. Best-effort: a missing or unreadable cache is not
    /// an error.
    fn purge_resurrection_cache(&self, name: &str) -> Vec<PathBuf> {
        let _ = name;
        Vec::new()
    }
    /// Converge every view (Zellij tab / tmux window) to one healthy sidebar per
    /// working view: in a working view close duplicate or unresponsive sidebar
    /// panes (those `live` does not claim) and re-add one if none survived; in an
    /// orphan sidebar-only view (no working pane, no daemon host) close every
    /// sidebar pane so a wedged renderer that never self-closed collapses with its
    /// view; leave the daemon view alone. All in place, without disturbing working
    /// panes. One best-effort pass: a view whose add fails is logged and skipped,
    /// never retried, never a session rebirth. Unlike [`Self::open_sidebar`], this
    /// never deletes or recreates the session.
    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery>;
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

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn view(id: &str, sidebars: &[&str], has_working: bool) -> ViewSidebars {
        ViewSidebars {
            view: id.to_owned(),
            sidebar_panes: sidebars.iter().map(|raw| pane(raw)).collect(),
            has_working,
            has_daemon_host: false,
        }
    }

    fn live(claimed: &[&str]) -> SidebarLiveness {
        SidebarLiveness {
            claimed_panes: claimed.iter().map(|raw| pane(raw)).collect(),
            has_unlocated: false,
        }
    }

    #[test]
    fn sidebar_width_is_the_default_percent_at_the_configured_cap() {
        let mut sidebar = crate::config::SidebarConfig::default();
        assert_eq!(
            SidebarWidth::from_config(&sidebar),
            SidebarWidth {
                percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
                max_cols: NonZeroU16::new(72).expect("nonzero"),
            },
        );
        assert_eq!(SidebarWidth::from_config(&sidebar), SidebarWidth::default());
        let max = NonZeroU16::new(100).expect("nonzero");
        sidebar.max_cols = max;
        assert_eq!(SidebarWidth::from_config(&sidebar).max_cols, max);
    }

    #[test]
    fn width_targets_the_percent_below_the_cap_and_the_cap_above_it() {
        let width = SidebarWidth::default();
        assert_eq!(width.target_cols(120), 36);
        assert_eq!(width.target_cols(300), 72);
        assert_eq!(width.cap_cols(), 72);
    }

    #[test]
    fn reconcile_adds_to_a_working_view_without_a_sidebar() {
        let views = vec![view("12", &[], true)];
        let plan = plan_reconcile(&views, &live(&[]));
        assert_eq!(plan.close, Vec::<PaneId>::new());
        assert_eq!(plan.add, vec!["12".to_owned()]);
    }

    #[test]
    fn reconcile_leaves_a_healthy_view_untouched() {
        // One sidebar pane, claimed live, plus a working pane: nothing to do.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &live(&["terminal_15"]));
        assert_eq!(plan, ReconcilePlan::default());
    }

    #[test]
    fn reconcile_closes_duplicates_keeping_one_live() {
        // Two sidebar panes in one tab; the live one is kept, the other closed.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let plan = plan_reconcile(&views, &live(&["terminal_15"]));
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty(), "a live sidebar already serves the tab");
    }

    #[test]
    fn reconcile_replaces_an_unresponsive_only_sidebar() {
        // The tab's lone sidebar is not claimed (wedged): close it and add fresh.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &live(&[]));
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert_eq!(plan.add, vec!["15".to_owned()]);
    }

    #[test]
    fn reconcile_collapses_an_orphan_sidebar_only_view() {
        // A sidebar-only view (working siblings all closed, no daemon host) is an
        // orphan a wedged renderer never self-closed: close every sidebar pane so
        // the view collapses, and add nothing — there is no working pane to serve.
        let views = vec![view("16", &["terminal_16", "terminal_17"], false)];
        let plan = plan_reconcile(&views, &live(&["terminal_16"]));
        assert_eq!(plan.close, vec![pane("terminal_16"), pane("terminal_17")]);
        assert!(
            plan.add.is_empty(),
            "no working pane means no sidebar to add"
        );
    }

    #[test]
    fn reconcile_leaves_the_daemon_view_alone() {
        // The daemon view (`rimzd`) has a sidebar beside managed hosts but no
        // working pane — intentional, never collapsed.
        let daemon = ViewSidebars {
            view: "0".to_owned(),
            sidebar_panes: vec![pane("terminal_2")],
            has_working: false,
            has_daemon_host: true,
        };
        assert_eq!(
            plan_reconcile(&[daemon], &live(&["terminal_2"])),
            ReconcilePlan::default(),
        );
    }

    #[test]
    fn reconcile_closes_duplicate_sidebars_under_an_unlocated_wildcard() {
        // An unlocated heartbeat might own one of the panes, but the view still
        // keeps only one physical sidebar.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty(), "one possible owner remains in the tab");
    }

    #[test]
    fn reconcile_prefers_a_claimed_sidebar_when_collapsing_unlocated_duplicates() {
        // A claimed pane is the best owner signal; the unlocated wildcard only
        // protects an unclaimed pane when no claimed one exists in the view.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: [pane("terminal_99")].into(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert!(
            plan.add.is_empty(),
            "the claimed sidebar already serves the tab"
        );
    }

    #[test]
    fn reconcile_closes_duplicate_sidebars_in_the_daemon_view() {
        // The daemon view itself is intentional, but duplicate chrome in that
        // view is not.
        let daemon = ViewSidebars {
            view: "0".to_owned(),
            sidebar_panes: vec![pane("terminal_2"), pane("terminal_3")],
            has_working: false,
            has_daemon_host: true,
        };
        let plan = plan_reconcile(&[daemon], &live(&["terminal_2"]));
        assert_eq!(plan.close, vec![pane("terminal_3")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_leaves_an_orphan_view_alone_under_an_unlocated_wildcard() {
        // An unlocated live sidebar might own the orphan's pane, so don't close
        // blind — leave it for self-close.
        let views = vec![view("16", &["terminal_16"], false)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        assert_eq!(plan_reconcile(&views, &unlocated), ReconcilePlan::default());
    }

    #[test]
    fn reconcile_collapses_duplicate_orphan_sidebars_under_an_unlocated_wildcard() {
        // Keep one possible owner for self-close, but close duplicate chrome.
        let views = vec![view("16", &["terminal_16", "terminal_17"], false)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_17")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_with_an_unlocated_live_sidebar_never_closes_blind() {
        // A fresh heartbeat with no pane id is a wildcard for the last physical
        // sidebar in a view; only add to a working view that has none at all.
        let views = vec![view("15", &["terminal_15"], true), view("12", &[], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert!(plan.close.is_empty(), "never close blind under a wildcard");
        assert_eq!(plan.add, vec!["12".to_owned()]);
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
