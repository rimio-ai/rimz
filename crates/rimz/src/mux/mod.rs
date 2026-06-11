//! Multiplexer abstraction.
//!
//! Everything correctness-critical (ledger, bridge, hooks, schemas) sits
//! above this trait and is identical across backends. Raw pane IDs live
//! only inside the adapter — see [`crate::ids::PaneId`] for the normalized
//! form that travels everywhere else.

mod command;
mod keys;
mod reconcile;
pub mod recovery;
mod selection;
pub mod tmux;
mod width;
pub mod zellij;

pub(crate) use command::COMMAND_TIMEOUT;
pub use command::CommandSpec;
pub use keys::{NamedKey, UnknownKey};
pub use reconcile::{SidebarLiveness, SidebarRecovery};
pub(crate) use reconcile::{ViewSidebars, plan_reconcile};
pub use selection::auto_detect_backend;
pub use tmux::TmuxBackend;
pub use width::{BirthSize, SidebarWidth, detect_terminal_size};
pub use zellij::ZellijBackend;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewId, WorkspaceId};

#[derive(Debug, thiserror::Error)]
pub enum MuxErr {
    #[error("multiplexer command `{program}` not found on PATH")]
    NotInstalled { program: String },
    #[error("no multiplexer found: install zellij or tmux")]
    NoMuxFound,
    #[error(
        "multiplexer command failed: {program} {}: {}",
        args_summary(args),
        stderr_summary(stderr)
    )]
    Command {
        program: String,
        args: String,
        stderr: String,
    },
    #[error(
        "Zellij can't create this room's IPC socket: the path is {len} bytes and the AF_UNIX limit here is {limit}.\n    {}\nPoint Zellij at a shorter socket directory and re-run rimz:\n\n    export ZELLIJ_SOCKET_DIR=/tmp/zellij\n\nAdd the export to your shell profile to make it permanent. `rimz doctor` reports the socket headroom.",
        path.display()
    )]
    SocketPathTooLong {
        path: PathBuf,
        len: usize,
        limit: usize,
    },
    #[error(
        "Zellij reported that this room's IPC socket path is too long:\n    {}\nPoint Zellij at a shorter socket directory and re-run rimz:\n\n    export ZELLIJ_SOCKET_DIR=/tmp/zellij\n\nAdd the export to your shell profile to make it permanent. `rimz doctor` reports the socket headroom.",
        stderr_summary(stderr)
    )]
    SocketPathReportedTooLong { stderr: String },
    #[error("pane id `{pane_id}` belongs to `{actual}`, but `{expected}` backend was selected")]
    PaneBackendMismatch {
        pane_id: PaneId,
        expected: MuxName,
        actual: MuxName,
    },
    #[error("could not parse mux output from `{program}`: {reason}")]
    Output { program: String, reason: String },
    #[error(
        "multiplexer command `{program} {}` did not finish within {seconds}s; killed",
        args_summary(args)
    )]
    Timeout {
        program: String,
        args: String,
        seconds: u64,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, MuxErr>;

fn args_summary(args: &str) -> String {
    let mut tokens = args.split_whitespace();
    let total = tokens.clone().count();
    let mut kept = Vec::new();
    let mut skip_value_for_flag = false;
    for token in &mut tokens {
        if skip_value_for_flag {
            skip_value_for_flag = false;
            continue;
        }
        if token.starts_with('-') {
            skip_value_for_flag = matches!(token, "--session");
            continue;
        }
        kept.push(token);
        if kept.len() == 2 {
            break;
        }
    }
    if kept.is_empty() {
        kept.extend(args.split_whitespace().take(1));
    }
    if total > kept.len() {
        kept.push("...");
    }
    kept.join(" ")
}

fn stderr_summary(stderr: &str) -> String {
    let mut lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .collect();
    let truncated_lines = lines.len() > 4;
    if truncated_lines {
        lines.truncate(4);
    }
    let mut summary = if lines.is_empty() {
        "no stderr".to_owned()
    } else {
        lines.join("\n")
    };
    if summary.len() > 400 {
        summary.truncate(last_char_boundary_at_or_before(&summary, 400));
        summary.push_str("...");
    } else if truncated_lines {
        summary.push_str("\n...");
    }
    summary
}

fn last_char_boundary_at_or_before(value: &str, max: usize) -> usize {
    if value.len() <= max {
        return value.len();
    }
    value
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= max)
        .last()
        .unwrap_or(0)
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
    /// Workspace runtime root to consult for backend-specific latency hints.
    /// Backends that do not have such a hint ignore it.
    pub workspace_id: Option<WorkspaceId>,
    /// Minimum acceptable `produced_at_ms` for backend-specific topology
    /// caches. Backends without such a cache ignore it.
    pub min_topology_produced_at_ms: Option<u64>,
    /// Override the backend's default subprocess timeout. `None` uses the
    /// backend's default (30s). Set to a shorter value for latency-sensitive
    /// probes (e.g. the self-close watchdog) where a hung Zellij should not
    /// block the caller for the full timeout.
    pub command_timeout: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
pub struct PaneListing {
    pub panes: Vec<PaneRef>,
    /// Wall-clock millisecond when the pane source observed this topology.
    /// For a topology-cache hit this is the cache's `produced_at_ms`; for a
    /// live mux CLI read it is stamped before the command starts.
    pub observed_at_ms: u64,
    /// Backend/source-resolved active panes by view. Zellij's presence plugin
    /// fills this from transition-derived focus; normal CLI reads leave it
    /// empty and frame assembly arbitrates from raw marks.
    pub source_active: BTreeMap<ViewId, PaneId>,
}

#[derive(Clone, Debug, Default)]
pub struct ClientFocusOptions {
    pub session_name: Option<String>,
    /// Override the backend's default subprocess timeout. The hook ingestion
    /// path uses a short bound because this is a best-effort pane recovery
    /// probe, never a precondition for recording the lifecycle event.
    pub command_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub session_name: String,
    /// The room's identity, stamped into the session environment at birth
    /// ([`crate::workspace::pin_env`]) so every pane — and so every agent and
    /// its in-pane hook children — inherits the workspace it lives in. A
    /// daemon-routed hook child inherits its daemon's env instead; resolution
    /// recovers the pin from the in-pane agent process
    /// ([`crate::workspace::WorkspaceResolver::resolve_participant_with_pin_recovery`]).
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    pub config: crate::config::MultiplexerConfig,
    /// The invoking terminal's `(cols, rows)`, when launch ran in one
    /// ([`detect_terminal_size`]). tmux sizes a detached birth with `-x`/`-y`
    /// so a fixed sidebar width is correct before the client attaches; `None`
    /// leaves the backend's default geometry. Zellij ignores it (a background
    /// session adopts the client size on attach).
    pub detected_size: Option<(u16, u16)>,
}

/// Normalize a raw per-pane mux env value into a [`PaneId`]: Zellij exposes a
/// bare integer in `ZELLIJ_PANE_ID` (normalized as `terminal_<id>`), tmux the
/// full raw id (`%<n>`) in `TMUX_PANE`. The one place the env→id mapping lives —
/// the renderer and reload both resolve through here.
pub fn pane_from_env_value(mux: MuxName, raw_env: &str) -> PaneId {
    let raw = match mux {
        MuxName::Zellij => format!("terminal_{raw_env}"),
        MuxName::Tmux => raw_env.to_owned(),
    };
    PaneId::from_parts(mux, raw)
}

/// The multiplexer's per-pane env var — the one place the key mapping lives.
fn pane_env_key(mux: MuxName) -> &'static str {
    match mux {
        MuxName::Zellij => "ZELLIJ_PANE_ID",
        MuxName::Tmux => "TMUX_PANE",
    }
}

/// This process's normalized pane id, read from the multiplexer's per-pane env
/// var via [`pane_from_env_value`]. `None` outside a pane.
pub fn own_pane_id(mux: MuxName) -> Option<PaneId> {
    let raw = std::env::var(pane_env_key(mux))
        .ok()
        .filter(|raw| !raw.is_empty())?;
    Some(pane_from_env_value(mux, &raw))
}

/// This process's normalized pane id probed from whichever multiplexer's
/// per-pane env var is present — Zellij first, so a tmux nested inside a
/// Zellij pane still stamps the outer room's pane. The ambient stamp agent
/// hooks and script asks share; `None` outside any mux pane (CI, cron, a
/// detached shell).
pub fn ambient_pane_id() -> Option<PaneId> {
    own_pane_id(MuxName::Zellij).or_else(|| own_pane_id(MuxName::Tmux))
}

#[derive(Clone, Debug)]
pub struct SidebarPaneOptions {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
    /// The workspace root behind `workspace_id` — with it, the identity pin a
    /// Zellij birth stamps on the spawned server so every pane inherits it
    /// (tmux pins through [`SessionOptions`] at `new-session` instead).
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    /// The configured width — the reconcile heal path still steps a recovered
    /// pane toward [`SidebarWidth::target_cols`] from live geometry.
    pub width: SidebarWidth,
    /// The width verdict freshly-born panes are spelled with in layouts,
    /// splits, and hooks — resolved once per command by
    /// [`SidebarWidth::birth_size`].
    pub birth_size: BirthSize,
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
    /// One-shot render-cadence override passed to newly spawned sidebars. This
    /// is intentionally not persisted; crash recovery rebuilds argv from
    /// workspace state and returns to `[sidebar].refresh_ms`.
    pub refresh_ms: Option<u16>,
}

/// One prior agent the reborn session re-seeds: a fresh pane running the
/// agent's resume CLI in its worktree, restoring the conversation idle (no
/// auto-prompt, no new token spend until the user types). Pure data — the
/// backend seeds `{command, cwd}` and knows nothing of agents or the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePane {
    /// Resume argv, program first — the supervised exec wrapper, e.g.
    /// `["<rimz>", "agents", "exec", "claude", "--resume", "<uuid>"]`, so the
    /// resumed agent gets the same launch-env injection as a fresh launch.
    pub command: Vec<String>,
    /// The agent's worktree: the cwd the resumed pane runs in.
    pub cwd: PathBuf,
    /// Short display and view label, e.g. `claude:feature-migration`. Doubles
    /// as the Zellij tab / tmux window name and the seed's idempotency key.
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct SplitPaneOptions {
    pub target_pane_id: Option<PaneId>,
    pub cwd: Option<String>,
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
}

/// Inputs for [`MuxBackend::ensure_presence_plugin`] — one session's presence
/// push channel. The caller resolves the artifact
/// ([`zellij::presence_plugin_path`]) and the `rimz` the plugin pokes; the
/// backend owns the load verbs and the version gate.
#[derive(Clone, Debug)]
pub struct PresencePluginOptions {
    pub session_name: String,
    /// The workspace the plugin pokes (`rimz sidebar wake --workspace-id …`),
    /// pinned at load so the poke never depends on the plugin host's cwd.
    pub workspace_id: WorkspaceId,
    /// The presence-plugin wasm to load.
    pub wasm: PathBuf,
    /// Absolute `rimz` the plugin runs, insulating the poke from the host
    /// PATH.
    pub rimz_bin: PathBuf,
    /// Also converge a *running* plugin onto the current wasm — the explicit
    /// upgrade verb `rimz reload` passes; routine loads leave a healthy
    /// running plugin untouched.
    pub converge: bool,
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

/// One command pane in a caller-built tab layout. The caller owns semantics
/// (agent exec wrapper vs shell); backends only run argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneCmd {
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPanes {
    pub columns: Vec<Vec<PaneCmd>>,
}

#[derive(Clone, Debug)]
pub struct TabOptions {
    pub session_name: String,
    pub title: String,
    pub cwd: PathBuf,
    pub panes: LayoutPanes,
    pub focus: bool,
    pub sidebar: SidebarPaneOptions,
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
    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing>;
    fn focused_client_panes(&self, opts: ClientFocusOptions) -> Result<Vec<PaneId>> {
        let _ = opts;
        Ok(Vec::new())
    }
    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()>;
    fn focus_pane(&self, pane: &PaneId) -> Result<()>;
    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture>;
    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn send_key(&self, pane: &PaneId, key: NamedKey) -> Result<()>;
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
    /// Open one user-facing tab/window with a caller-built pane layout. The
    /// global sidebar is included by backend convention: tmux relies on the
    /// session's `after-new-window` hook; Zellij renders it into the tab layout.
    fn open_tab(&self, opts: &TabOptions) -> Result<()>;
    /// Close one pane by normalized id. Used by supervised one-shot launches
    /// after their terminal result is recorded; callers treat failure as
    /// best-effort cleanup.
    fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()>;
    /// Close any floating panes sharing `anchor`'s view, returning the closed
    /// pane ids. A self-closing sidebar uses this to tear down Zellij overlays
    /// that would otherwise keep an empty tab alive; tmux has no floating panes,
    /// so the default is empty.
    fn close_view_floating_panes(&self, session: &str, anchor: &PaneId) -> Result<Vec<PaneId>> {
        let _ = (session, anchor);
        Ok(Vec::new())
    }
    /// Best-effort wakeup; sockets are the channel of record per the docs.
    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()>;
    /// Load the session's presence plugin — the push channel that nudges the
    /// sidebar producer off its pane poll. A latency hint layered over the
    /// poll truth, so failure costs freshness only and callers never block on
    /// it. Zellij implements it; the default no-op covers tmux, whose
    /// control-mode `PresenceWatch` already pushes.
    fn ensure_presence_plugin(&self, _opts: &PresencePluginOptions) -> Result<()> {
        Ok(())
    }
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
    fn pane_from_env_value_normalizes_per_mux() {
        assert_eq!(
            pane_from_env_value(MuxName::Zellij, "3"),
            PaneId::from_parts(MuxName::Zellij, "terminal_3"),
        );
        assert_eq!(
            pane_from_env_value(MuxName::Tmux, "%5"),
            PaneId::from_parts(MuxName::Tmux, "%5"),
        );
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

    #[test]
    fn command_display_summarizes_args_and_stderr() {
        let args = [
            "attach",
            "--create-background",
            "rimz-room",
            "options",
            "--default-mode",
            "locked",
            "--show-startup-tips",
            "false",
        ]
        .join(" ");
        let err = MuxErr::Command {
            program: "zellij".to_owned(),
            args: args.clone(),
            stderr: "one\ntwo\nthree\nfour\nfive\n".to_owned(),
        };

        let rendered = err.to_string();

        assert!(rendered.contains("zellij attach rimz-room ..."));
        assert!(!rendered.contains("--default-mode"));
        assert!(rendered.contains("one\ntwo\nthree\nfour\n..."));
        assert!(matches!(err, MuxErr::Command { args: stored, .. } if stored == args));
    }

    #[test]
    fn timeout_display_summarizes_args() {
        let err = MuxErr::Timeout {
            program: "zellij".to_owned(),
            args: "attach --create rimz-room options --default-mode locked".to_owned(),
            seconds: 8,
        };

        let rendered = err.to_string();

        assert!(rendered.contains("zellij attach rimz-room ..."));
        assert!(!rendered.contains("--default-mode"));
    }

    #[test]
    fn command_display_truncates_stderr_on_char_boundaries() {
        let err = MuxErr::Command {
            program: "zellij".to_owned(),
            args: "action list-panes".to_owned(),
            stderr: "€".repeat(200),
        };

        let rendered = err.to_string();

        assert!(rendered.contains("..."));
    }

    #[test]
    fn command_display_skips_session_flag_value_in_summary() {
        let err = MuxErr::Command {
            program: "zellij".to_owned(),
            args: "--session rimz-room action list-panes --all".to_owned(),
            stderr: "failed".to_owned(),
        };

        let rendered = err.to_string();

        assert!(rendered.contains("zellij action list-panes ..."));
    }
}
