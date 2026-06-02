//! tmux `MuxBackend` implementation.
//!
//! Every command runs `tmux [-S <socket>] <verb> ...`. The optional socket
//! lives on the struct so integration tests can isolate each test's server
//! from the user's running tmux. Production code constructs the unit form
//! (`TmuxBackend::default()`) and inherits the system default socket.
//!
//! Caveats live in `docs/internals/multiplexers.md` under "tmux backend
//! caveats" — namely that `wake_sidebar` is a no-op (tmux has no pipe
//! equivalent) and that the managed sidebar pane is the channel of record.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{
    BackgroundViewLaunch, BackgroundViewOptions, CommandSpec, DaemonView, MuxBackend, MuxErr,
    PaneCapture, PaneListOptions, Result, SessionOptions, SidebarLiveness, SidebarPaneOptions,
    SidebarRecovery, SplitPaneOptions, ViewSidebars, ensure_pane_backend,
};
use crate::config::TmuxConfig;
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};

/// Minimum tmux version that supports the features Rimz relies on:
/// `split-window -e KEY=VAL` for `RIMZ_*` injection (3.2),
/// `display-popup` for the optional popup integration (3.2).
pub const MIN_TMUX_VERSION: (u32, u32, u32) = (3, 2, 0);

/// Bundle reported by `rimz doctor` when the active backend is tmux.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TmuxCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
    pub popup_supported: bool,
}

/// Probe the installed tmux. Cheap: one `tmux -V` call.
pub fn capabilities() -> Result<TmuxCapabilities> {
    let raw = TmuxBackend::default().version()?;
    let parsed = parse_version(&raw);
    let meets_min_version = parsed.is_some_and(|v| v >= MIN_TMUX_VERSION);
    Ok(TmuxCapabilities {
        binary_version: raw,
        parsed_version: parsed,
        meets_min_version,
        // Popup landed in the same release as `-e`, so the same gate
        // controls both. Keeping the flag distinct lets future tmux
        // capabilities split off without rewriting callers.
        popup_supported: meets_min_version,
    })
}

/// Parse `"tmux 3.5a"` (and tolerant of leading/trailing whitespace and the
/// alphabetic patch-letter suffix tmux uses for point releases). Returns
/// None when the shape is unexpected so `doctor` can render the raw string
/// verbatim.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    let after_prefix = trimmed.strip_prefix("tmux ").unwrap_or(trimmed);
    let head = after_prefix
        .split(|c: char| c.is_whitespace())
        .next()?
        .trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let mut parts = head.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[derive(Clone, Debug, Default)]
pub struct TmuxBackend {
    /// Override for the tmux server socket. When `None` tmux picks the
    /// default (`$TMUX_TMPDIR/tmux-<uid>/default`). Integration tests set
    /// this to a tempdir path so they never touch the user's sessions.
    socket: Option<PathBuf>,
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(socket.into()),
        }
    }

    /// Base `CommandSpec` with the `-S <socket>` prefix applied when set.
    fn cmd(&self) -> CommandSpec {
        let mut spec = CommandSpec::new("tmux");
        if let Some(socket) = &self.socket {
            spec = spec.args(["-S".to_owned(), socket.to_string_lossy().into_owned()]);
        }
        spec
    }

    /// Close a single pane by id (`kill-pane -t %N`), terminating its process.
    /// Reconcile uses this to drop a duplicate or unresponsive sidebar pane
    /// without touching the rest of the window.
    fn kill_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args([
                "kill-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Split a left sidebar into a specific window in place, mirroring the
    /// initial-window split: `-b` (before/left), `-l <pct>%` (width), `-d`
    /// (keep the caller's focus). The `-t <window_id>` target leaves every other
    /// window untouched.
    fn add_sidebar_to_window(&self, opts: &SidebarPaneOptions, window_id: &str) -> Result<()> {
        self.cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                "-h".to_owned(),
                "-b".to_owned(),
                "-l".to_owned(),
                format!("{}%", opts.width_percent),
                "-t".to_owned(),
                window_id.to_owned(),
            ])
            .args(sidebar_serve_command(opts))
            .run()
            .map(|_| ())
    }

    /// Whether `session` already holds a window named `name`. A Rimz background
    /// view is idempotent on its window name, so a relaunch into a session that
    /// already carries it is skipped.
    fn session_has_window(&self, session: &str, name: &str) -> Result<bool> {
        let output = self
            .cmd()
            .args(["list-windows", "-t", session, "-F", "#{window_name}"])
            .run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == name))
    }

    /// Force the named window to the session's first slot. tmux opens the daemon
    /// window last (`-d`, no focus change), so swap it with the base-index window
    /// — `swap-window` always succeeds even when that slot is occupied, and `-d`
    /// keeps the user on their working window, so no focus-return is needed.
    /// Best-effort: a reorder hiccup never sinks an otherwise-launched view.
    fn lead_window(&self, session: &str, name: &str) {
        let base = self.base_index();
        if let Err(err) = self
            .cmd()
            .args([
                "swap-window".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                format!("{session}:{name}"),
                "-t".to_owned(),
                format!("{session}:{base}"),
            ])
            .run()
        {
            tracing::warn!(
                session = %session,
                error = %err,
                "could not move the daemon window to the front",
            );
        }
    }

    /// The session's first window index (`base-index`, default 0 — almost always
    /// a global option).
    fn base_index(&self) -> String {
        self.cmd()
            .args(["show-options", "-gv", "base-index"])
            .run()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "0".to_owned())
    }

    /// Apply Rimz's tmux room options. tmux splits these across server,
    /// session, and window scopes; server options affect every session in this
    /// tmux server because tmux offers no per-session equivalent for clipboard
    /// and rich-key handling.
    fn apply_room_options(&self, session: &str, config: &TmuxConfig) -> Result<()> {
        for (key, value) in tmux_server_options(config) {
            self.cmd()
                .args([
                    "set-option".to_owned(),
                    "-s".to_owned(),
                    key.to_owned(),
                    value,
                ])
                .run()
                .map(|_| ())?;
        }
        for (key, value) in tmux_session_options(config) {
            self.cmd()
                .args([
                    "set-option".to_owned(),
                    "-t".to_owned(),
                    session.to_owned(),
                    key.to_owned(),
                    value,
                ])
                .run()
                .map(|_| ())?;
        }
        for (key, value) in tmux_window_options(config) {
            self.cmd()
                .args([
                    "set-window-option".to_owned(),
                    "-t".to_owned(),
                    session.to_owned(),
                    key.to_owned(),
                    value,
                ])
                .run()
                .map(|_| ())?;
        }
        Ok(())
    }
}

/// Binary name a tmux sidebar pane runs in the foreground. The launching `rimz
/// sidebar serve` parent waits on the `rimz-sidebar` child, so that child is
/// what `pane_current_command` reports.
const SIDEBAR_BIN_NAME: &str = "rimz-sidebar";

/// The `rimz sidebar serve …` argv a tmux sidebar pane runs. Shared by initial
/// launch and in-place recovery so the two cannot drift.
fn sidebar_serve_command(opts: &SidebarPaneOptions) -> Vec<String> {
    vec![
        opts.rimz_bin.to_string_lossy().into_owned(),
        "sidebar".to_owned(),
        "serve".to_owned(),
        "--mux".to_owned(),
        "tmux".to_owned(),
        "--workspace-id".to_owned(),
        opts.workspace_id.as_str().to_owned(),
        "--session-name".to_owned(),
        opts.session_name.clone(),
    ]
}

fn is_tmux_sidebar(pane: &PaneRef) -> bool {
    pane.command.as_deref() == Some(SIDEBAR_BIN_NAME)
}

/// Group a pane list into per-window [`ViewSidebars`] for the reconcile planner:
/// each window's sidebar panes and whether it holds a working (non-sidebar) pane.
/// Panes with no window id are skipped. First-seen window order.
fn tmux_views_with_sidebars(panes: &[PaneRef]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for pane in panes {
        let Some(view) = pane.view_id.as_deref() else {
            continue;
        };
        let slot = *index.entry(view.to_owned()).or_insert_with(|| {
            views.push(ViewSidebars {
                view: view.to_owned(),
                sidebar_panes: Vec::new(),
                has_working: false,
            });
            views.len() - 1
        });
        if is_tmux_sidebar(pane) {
            views[slot].sidebar_panes.push(pane.pane_id.clone());
        } else {
            views[slot].has_working = true;
        }
    }
    views
}

fn tmux_bool(value: bool) -> String {
    if value { "on" } else { "off" }.to_owned()
}

fn tmux_server_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("focus-events", tmux_bool(config.focus_events)),
        ("set-clipboard", config.set_clipboard.as_str().to_owned()),
        ("extended-keys", tmux_bool(config.extended_keys)),
        (
            "extended-keys-format",
            config.extended_keys_format.as_str().to_owned(),
        ),
        ("escape-time", config.escape_time_ms.to_string()),
    ]
}

fn tmux_session_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("mouse", tmux_bool(config.mouse)),
        ("history-limit", config.history_limit.to_string()),
        ("renumber-windows", tmux_bool(config.renumber_windows)),
    ]
}

fn tmux_window_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("allow-passthrough", tmux_bool(config.allow_passthrough)),
        ("aggressive-resize", tmux_bool(config.aggressive_resize)),
        (
            "pane-border-status",
            config.pane_border_status.as_str().to_owned(),
        ),
        (
            "pane-border-lines",
            config.pane_border_lines.as_str().to_owned(),
        ),
    ]
}

impl MuxBackend for TmuxBackend {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, opts: &SessionOptions) -> Result<()> {
        // `-A` attaches if the session exists; `-d` keeps us from grabbing
        // the terminal in the background.
        self.cmd()
            .args([
                "new-session".to_owned(),
                "-A".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                opts.session_name.clone(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .run()
            .map(|_| ())?;
        self.apply_room_options(&opts.session_name, &opts.config.tmux)
    }

    fn attach_command(
        &self,
        name: &str,
        _config: &crate::config::MultiplexerConfig,
    ) -> CommandSpec {
        self.cmd().args(["attach", "-t", name])
    }

    fn detach(&self, name: &str) -> Result<()> {
        self.cmd()
            .args(["detach-client", "-s", name])
            .run()
            .map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        // A session that is already gone (or no server at all) is the goal
        // state, so the "can't find session" / "no server" errors are success.
        match self.cmd().args(["kill-session", "-t", name]).run() {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if {
                    let lower = stderr.to_ascii_lowercase();
                    lower.contains("can't find session")
                        || lower.contains("no server running")
                        || lower.contains("error connecting")
                } =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        // tmux exits 1 with `error connecting to ...` (or `no server
        // running`) on stderr when no server has been started yet. That is
        // an empty list of sessions, not an error condition; the Zellij
        // backend mirrors this shape (exit 0, empty stdout).
        let output = self
            .cmd()
            .args(["list-sessions", "-F", "#{session_name}"])
            .to_command()
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                    program: "tmux".to_owned(),
                },
                _ => MuxErr::Io(err),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("error connecting") {
                return Ok(Vec::new());
            }
            return Err(MuxErr::Command {
                program: "tmux".to_owned(),
                args: "list-sessions -F #{session_name}".to_owned(),
                stderr: stderr.into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>> {
        let mut spec = self.cmd().args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_pid}\t#{pane_start_time}\t#{pane_active}\t#{window_name}",
        ]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run()?;
        let panes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_pane_line)
            .collect();
        Ok(panes)
    }

    /// `list-clients -t <session>` lists the clients attached to that session;
    /// `#{pane_id}` resolves per-client to the pane that client is viewing (the
    /// active pane of its current window). One row per attached client → the
    /// per-client focus set. No clients (headless) → empty.
    fn client_focused_panes(&self, session: &str) -> Result<Vec<PaneId>> {
        let output = self
            .cmd()
            .args(["list-clients", "-t", session, "-F", "#{pane_id}"])
            .run()?;
        Ok(parse_client_pane_ids(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let mut spec = self.cmd().args(["split-window", "-d", "-h"]);
        for (key, value) in &opts.env {
            spec = spec.args(["-e".to_owned(), format!("{key}={value}")]);
        }
        if let Some(target) = opts.target_pane_id {
            ensure_pane_backend(&target, MuxName::Tmux)?;
            spec = spec.args(["-t".to_owned(), target.raw().to_owned()]);
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["-c".to_owned(), cwd]);
        }
        if let Some(command) = opts.command {
            spec = spec.args(command);
        }
        spec.run().map(|_| ())
    }

    fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["select-pane", "-t", pane.raw()])
            .run()
            .map(|_| ())
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let mut spec = self.cmd().args(["capture-pane", "-p", "-t", pane.raw()]);
        if let Some(n) = lines {
            spec = spec.args(["-S".to_owned(), format!("-{n}")]);
        }
        if ansi {
            spec = spec.arg("-e");
        }
        let output = spec.run()?;
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let lines = text.lines().map(ToOwned::to_owned).collect();
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text: text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["send-keys", "-t", pane.raw(), "--", text])
            .run()
            .map(|_| ())
    }

    fn open_sidebar(&self, opts: &SidebarPaneOptions, _daemon: Option<&DaemonView>) -> Result<()> {
        // tmux can reorder windows freely, so the daemon view leads via
        // `open_background_view` (`swap-window`) rather than a birth layout; the
        // `daemon` hint is Zellij's concern and ignored here.
        // Managed sidebar pane per docs/internals/multiplexers.md:
        //   tmux split-window -d -h -l <width>% -b -t <session> 'rimz sidebar serve ...'
        // `-d` keeps the spawning client focused on its existing pane;
        // `-b` places the new pane before the target so the sidebar sits
        // on the left. Workspace identity is passed directly to the spawned
        // renderer command.
        let command = sidebar_serve_command(opts);
        self.cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                "-h".to_owned(),
                "-l".to_owned(),
                format!("{}%", opts.width_percent),
                "-b".to_owned(),
                "-t".to_owned(),
                opts.session_name.clone(),
            ])
            .args(command.clone())
            .run()?;

        // Cross-backend parity (DESIGN.md): a Zellij session's layout doubles
        // as its tab template, so every new tab is born with the same
        // sidebar+terminal split. tmux has no tab template, so we install a
        // session-scoped `after-new-window` hook that re-runs the same left
        // split in each new window. `-b -d` keep the sidebar left and focus on
        // the new window's terminal, exactly as the initial window.
        let serve = command.join(" ");
        let hook = format!(
            "split-window -h -b -d -l {pct}% '{serve}'",
            pct = opts.width_percent,
        );
        self.cmd()
            .args([
                "set-hook".to_owned(),
                "-t".to_owned(),
                opts.session_name.clone(),
                "after-new-window".to_owned(),
                hook,
            ])
            .run()
            .map(|_| ())
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // tmux re-adds a sidebar in place with the same left split the initial
        // window got — `-d` keeps the user's focus, `-l <pct>%` sets the width —
        // and drops a stray sidebar with `kill-pane -t`; no move/resize/refocus
        // dance and no session teardown is needed.
        let panes = self.list_panes(PaneListOptions {
            session_name: Some(opts.session_name.clone()),
        })?;
        let views = tmux_views_with_sidebars(&panes);
        let plan = super::plan_reconcile(&views, live);
        let mut report = SidebarRecovery::default();
        for pane in &plan.close {
            match self.kill_pane(pane) {
                Ok(()) => report.closed += 1,
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    pane = %pane.as_str(),
                    error = %err,
                    "sidebar reconcile: closing a stray sidebar pane failed; leaving it",
                ),
            }
        }
        for window in &plan.add {
            match self.add_sidebar_to_window(opts, window) {
                Ok(()) => report.recovered += 1,
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        window = %window,
                        error = %err,
                        "sidebar reconcile: in-place add failed; leaving the window without a sidebar",
                    );
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the window name; a relaunch into a session already
        // carrying the view launches nothing, but still re-asserts its first
        // position. A failed query propagates rather than risk a duplicate window.
        if self.session_has_window(session, &opts.name)? {
            self.lead_window(session, &opts.name);
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        let Some((first, rest)) = opts.hosts.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "background view has no host panes".to_owned(),
            });
        };
        // `-d` opens the window without pulling the user's focus to it; `-P -F`
        // prints the host pane id so extra hosts split beside it, never the
        // sidebar. The session's `after-new-window` hook (installed by
        // `open_sidebar`) docks the global sidebar on its left, so the window is
        // born `sidebar | host0` — the host is always reachable, never a bare
        // trap. Each host closes with its process, so no `remain-on-exit`.
        let output = self
            .cmd()
            .args([
                "new-window".to_owned(),
                "-d".to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{pane_id}".to_owned(),
                "-t".to_owned(),
                session.clone(),
                "-n".to_owned(),
                opts.name.clone(),
                "-c".to_owned(),
                first.cwd.to_string_lossy().into_owned(),
            ])
            .args(first.argv.clone())
            .run()?;
        let host0 = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // Extra hosts (typically just the Codex broker) split beside host0,
        // stacked left-to-right; `-d` keeps host0 the window's active pane.
        for host in rest {
            self.cmd()
                .args([
                    "split-window".to_owned(),
                    "-d".to_owned(),
                    "-h".to_owned(),
                    "-t".to_owned(),
                    host0.clone(),
                    "-c".to_owned(),
                    host.cwd.to_string_lossy().into_owned(),
                ])
                .args(host.argv.clone())
                .run()?;
        }
        self.lead_window(session, &opts.name);
        Ok(BackgroundViewLaunch::Launched)
    }

    fn wake_sidebar(&self, _session_name: &str, _bytes: &[u8]) -> Result<()> {
        // tmux has no pipe equivalent; the sidebar wakeup socket is the
        // only channel. Socket fanout lives above this trait in the ledger
        // module.
        Ok(())
    }

    fn version(&self) -> Result<String> {
        let output =
            self.cmd()
                .arg("-V")
                .to_command()
                .output()
                .map_err(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                        program: "tmux".to_owned(),
                    },
                    _ => MuxErr::Io(err),
                })?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// Parse one tab-separated `list-panes -F` row into a [`PaneRef`]. Returns
/// `None` for a row missing the three load-bearing leading columns (session,
/// window, pane id) — a degraded answer the caller skips rather than surfaces.
///
/// Trailing columns are read with `.get(i)`, so a short row (an older tmux, or a
/// mid-tick race that truncated the line) yields `None`/default for the missing
/// field rather than erroring the whole read.
fn parse_pane_line(line: &str) -> Option<PaneRef> {
    let cols: Vec<_> = line.split('\t').collect();
    if cols.len() < 3 {
        return None;
    }
    let trimmed_nonempty = |i: usize| {
        cols.get(i)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    Some(PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, cols[2]),
        session_name: cols[0].to_owned(),
        view_id: Some(cols[1].to_owned()),
        view_kind: Some(ViewKind::Window),
        view_name: trimmed_nonempty(8),
        is_focused: cols.get(7).is_some_and(|value| value.trim() == "1"),
        // Stamped later by the producer from `client_focused_panes`, never here:
        // `list-panes` reports the per-window active pane, not per-client focus.
        client_focused: false,
        command: trimmed_nonempty(3),
        cwd: trimmed_nonempty(4),
        pane_pid: cols
            .get(5)
            .and_then(|value| value.trim().parse::<u32>().ok()),
        pane_process_start: cols
            .get(6)
            .and_then(|value| value.trim().parse::<i64>().ok())
            .and_then(|seconds| Timestamp::from_second(seconds).ok()),
    })
}

/// Parse `list-clients -F "#{pane_id}"` stdout into the per-client focused-pane
/// set: one pane id per line. Blank lines (a short or empty read) are skipped;
/// no clients → empty.
fn parse_client_pane_ids(stdout: &str) -> Vec<PaneId> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|raw| PaneId::from_parts(MuxName::Tmux, raw))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmux_pane(id: &str, view: &str, command: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, id),
            session_name: "room".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: None,
            view_name: None,
            is_focused: false,
            client_focused: false,
            command: Some(command.to_owned()),
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn views_with_sidebars_groups_by_window_and_flags_working() {
        let panes = vec![
            tmux_pane("%1", "@0", "sh"),             // working pane
            tmux_pane("%2", "@0", SIDEBAR_BIN_NAME), // its sidebar
            tmux_pane("%3", "@0", SIDEBAR_BIN_NAME), // a duplicate sidebar
            tmux_pane("%4", "@1", SIDEBAR_BIN_NAME), // a sidebar-only window
        ];
        let views = tmux_views_with_sidebars(&panes);
        assert_eq!(views.len(), 2, "two windows, in first-seen order");

        assert_eq!(views[0].view, "@0");
        assert!(views[0].has_working);
        assert_eq!(
            views[0].sidebar_panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%2"),
                PaneId::from_parts(MuxName::Tmux, "%3"),
            ],
            "both sidebar panes, in order",
        );

        assert_eq!(views[1].view, "@1");
        assert!(
            !views[1].has_working,
            "a sidebar-only window holds no working pane",
        );
        assert_eq!(views[1].sidebar_panes.len(), 1);
    }

    #[test]
    fn version_parser_strips_letter_suffix() {
        assert_eq!(parse_version("tmux 3.5a"), Some((3, 5, 0)));
        assert_eq!(parse_version("tmux 3.2"), Some((3, 2, 0)));
        assert_eq!(parse_version("  tmux 3.4  \n"), Some((3, 4, 0)));
        assert_eq!(parse_version("tmux 2.9a"), Some((2, 9, 0)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn min_version_threshold_holds() {
        assert!((3, 2, 0) >= MIN_TMUX_VERSION);
        assert!((3, 5, 0) >= MIN_TMUX_VERSION);
        assert!((3, 1, 9) < MIN_TMUX_VERSION);
    }

    #[test]
    fn tmux_options_render_room_defaults() {
        let config = TmuxConfig::default();
        assert_eq!(
            tmux_server_options(&config),
            vec![
                ("focus-events", "on".to_owned()),
                ("set-clipboard", "on".to_owned()),
                ("extended-keys", "on".to_owned()),
                ("extended-keys-format", "csi-u".to_owned()),
                ("escape-time", "0".to_owned()),
            ],
        );
        assert_eq!(
            tmux_session_options(&config),
            vec![
                ("mouse", "on".to_owned()),
                ("history-limit", "100000".to_owned()),
                ("renumber-windows", "on".to_owned()),
            ],
        );
        assert_eq!(
            tmux_window_options(&config),
            vec![
                ("allow-passthrough", "on".to_owned()),
                ("aggressive-resize", "on".to_owned()),
                ("pane-border-status", "off".to_owned()),
                ("pane-border-lines", "simple".to_owned()),
            ],
        );
    }

    #[test]
    fn parse_pane_line_reads_core_fields() {
        // session, window_id, pane_id, command, cwd, pid, start, pane_active,
        // window_name.
        let row = "rimz-qe\t@1\t%3\tnvim\t/home/u/qe\t4242\t1700000000\t1\tqe";
        let pane = parse_pane_line(row).expect("full row parses");
        assert_eq!(pane.pane_id.raw(), "%3");
        assert_eq!(pane.session_name, "rimz-qe");
        assert_eq!(pane.view_id.as_deref(), Some("@1"));
        assert_eq!(pane.view_name.as_deref(), Some("qe"));
        assert_eq!(pane.command.as_deref(), Some("nvim"));
        assert_eq!(pane.cwd.as_deref(), Some("/home/u/qe"));
        assert_eq!(pane.pane_pid, Some(4242));
        assert!(pane.is_focused, "pane_active=1 is focused");

        // A pane_active=0 row is not focused.
        let other = "rimz-qe\t@1\t%4\tzsh\t/home/u/qe\t4243\t1700000000\t0\tqe";
        assert!(!parse_pane_line(other).expect("row parses").is_focused);
    }

    #[test]
    fn parse_pane_line_tolerates_a_short_trailing_row() {
        // A truncated row that still carries the three load-bearing columns
        // parses; the absent optional fields read as `None`/default.
        let short = "rimz-qe\t@1\t%3";
        let pane = parse_pane_line(short).expect("the leading columns still parse");
        assert_eq!(pane.pane_id.raw(), "%3");
        assert_eq!(pane.command, None);
        assert_eq!(pane.view_name, None);
        assert!(!pane.is_focused);
        assert!(!pane.client_focused);
    }

    #[test]
    fn parse_client_pane_ids_reads_one_pane_per_client() {
        // Two clients attached to the session, each viewing a different pane.
        let ids = parse_client_pane_ids("%3\n%7\n");
        assert_eq!(
            ids,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%3"),
                PaneId::from_parts(MuxName::Tmux, "%7"),
            ]
        );
    }

    #[test]
    fn parse_client_pane_ids_is_empty_with_no_clients() {
        assert!(parse_client_pane_ids("").is_empty());
        assert!(parse_client_pane_ids("\n  \n").is_empty());
    }

    #[test]
    fn parse_pane_line_skips_rows_missing_core_columns() {
        assert!(
            parse_pane_line("rimz-qe\t@1").is_none(),
            "needs session+window+pane"
        );
        assert!(parse_pane_line("").is_none());
    }
}
