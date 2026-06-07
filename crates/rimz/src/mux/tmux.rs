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

use serde::{Deserialize, Serialize};

use super::{
    BackgroundViewLaunch, BackgroundViewOptions, ClientFocusOptions, CommandSpec, DaemonView,
    MuxBackend, MuxErr, PaneCapture, PaneCmd, PaneListOptions, Result, SessionOptions,
    SidebarLiveness, SidebarPaneOptions, SidebarRecovery, SplitPaneOptions, TabOptions,
    ViewSidebars, ensure_pane_backend,
};
use crate::config::TmuxConfig;
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};

/// Minimum tmux version that supports the features Rimz relies on. The floor
/// is set by the room options `ensure_session` applies unconditionally —
/// `extended-keys-format` (3.5) and `allow-passthrough` (3.3) — since the
/// option batch fails at the first unknown option; the command surface alone
/// needs 3.2 (`new-session -e`, `display-popup`). Release floors per feature
/// live in docs/externals/mux-adapter/tmux-reference.md.
pub const MIN_TMUX_VERSION: (u32, u32, u32) = (3, 5, 0);

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
        // Popup landed in 3.2, below the floor, so the floor gate covers it.
        // Keeping the flag distinct lets future tmux capabilities split off
        // without rewriting callers.
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

#[derive(Debug, Default)]
pub struct TmuxBackend {
    /// Override for the tmux server socket. When `None` tmux picks the
    /// default (`$TMUX_TMPDIR/tmux-<uid>/default`). Integration tests set
    /// this to a tempdir path so they never touch the user's sessions.
    socket: Option<PathBuf>,
    /// Memoized `tmux -V` stdout ([`MuxBackend::version`]), probed once per
    /// instance — Zellij parity. An instance lives one CLI command, so an
    /// upgraded binary is seen by the next command. Only a successful probe
    /// is stored, so a transient failure retries.
    version: std::sync::OnceLock<String>,
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(socket.into()),
            ..Self::default()
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

    /// Run several tmux commands in one client invocation, joined by standalone
    /// `;` argv tokens (`tmux <cmd-a…> ; <cmd-b…>`) — one fork and one server
    /// round-trip instead of N. tmux runs the sequence left-to-right and exits
    /// non-zero at the first failure, naming it on stderr; commands before it
    /// stay applied (the same partial application the sequential loop had), and
    /// the returned [`MuxErr::Command`] carries the joined argv with that
    /// stderr. Only for commands whose output is not read back per command —
    /// anything parsed (`-P -F`, `show-options`, `display-message`) stays a
    /// separate `run()`.
    fn batch(&self, commands: &[Vec<String>]) -> Result<()> {
        // Zero commands is a no-op: an argv-less `tmux` client would
        // `new-session` instead.
        if commands.is_empty() {
            return Ok(());
        }
        let mut spec = self.cmd();
        for (index, command) in commands.iter().enumerate() {
            if index > 0 {
                spec = spec.arg(";");
            }
            spec = spec.args(command.iter().cloned());
        }
        spec.run().map(|_| ())
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
    /// initial-window split: `-b` (before/left), `-l <size>` (width), `-d`
    /// (keep the caller's focus). The `-t <window_id>` target leaves every other
    /// window untouched. The heal sizes from the live window — `target_cols`
    /// of `#{window_width}` — never from `opts.birth_size`: a reconcile can run
    /// from a terminal (or no terminal) unrelated to the session's clients.
    /// When the width is unreadable, the percentage is the safe fallback.
    fn add_sidebar_to_window(&self, opts: &SidebarPaneOptions, window_id: &str) -> Result<()> {
        let size = match self.window_width(window_id) {
            Some(total) => opts.width.target_cols(total).to_string(),
            None => format!("{}%", opts.width.percent),
        };
        self.cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                "-h".to_owned(),
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                window_id.to_owned(),
            ])
            .args(sidebar_serve_command(opts))
            .run()
            .map(|_| ())
    }

    /// The live column width of `window_id`, when tmux can report it.
    fn window_width(&self, window_id: &str) -> Option<u64> {
        let output = self
            .cmd()
            .args(["display-message", "-p", "-t", window_id, "#{window_width}"])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    /// Whether `session` already holds a window named `name`. A Rimz background
    /// view is idempotent on its window name, so a relaunch into a session that
    /// already carries it is skipped.
    fn session_has_window(&self, session: &str, name: &str) -> Result<bool> {
        Ok(self
            .window_names(session)?
            .iter()
            .any(|window| window == name))
    }

    /// Every window name in `session` — one `list-windows` probe that callers
    /// checking several names share instead of forking per name.
    fn window_names(&self, session: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args(["list-windows", "-t", session, "-F", "#{window_name}"])
            .run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .collect())
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

    /// Re-seed the reborn session's prior agents, one window each, born
    /// `sidebar | agent` via the `after-new-window` hook. Idempotent on the
    /// window name so a re-run (a heal that re-adds the sidebar) never doubles an
    /// agent window; the freshest agent (the first in the plan) is selected so
    /// attach lands on it, mirroring the Zellij layout's focus. Best-effort:
    /// a failed window is logged and skipped — the room is still usable.
    fn seed_resume_windows(&self, opts: &SidebarPaneOptions) {
        if opts.resume_panes.is_empty() {
            return;
        }
        // One `list-windows` probe covers every pane's idempotency check —
        // this replaces a probe fork per resumed agent. A failed probe means
        // re-seeding cannot be made idempotent, so every agent is left out
        // (the same degradation the per-agent probe had, once instead of N).
        let existing = match self.window_names(&opts.session_name) {
            Ok(names) => names,
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    error = %err,
                    "resume: window probe failed; leaving the agents out",
                );
                return;
            }
        };
        let mut focus_window: Option<String> = None;
        for pane in &opts.resume_panes {
            if existing.iter().any(|window| window == &pane.label) {
                continue; // already seeded by an earlier birth
            }
            // `-d` keeps the user on the working window; `-P -F` prints the new
            // window id so we can land focus on the freshest agent without the
            // `session:name` colon ambiguity a label can carry. The agent argv
            // follows directly, run via execvp (no shell), so it needs no quoting.
            let launched = self
                .cmd()
                .args([
                    "new-window".to_owned(),
                    "-d".to_owned(),
                    "-P".to_owned(),
                    "-F".to_owned(),
                    "#{window_id}".to_owned(),
                    "-t".to_owned(),
                    opts.session_name.clone(),
                    "-n".to_owned(),
                    pane.label.clone(),
                    "-c".to_owned(),
                    pane.cwd.to_string_lossy().into_owned(),
                ])
                .args(pane.command.clone())
                .run();
            match launched {
                Ok(output) => {
                    let window_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    if focus_window.is_none() && !window_id.is_empty() {
                        focus_window = Some(window_id);
                    }
                }
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    agent = %pane.label,
                    error = %err,
                    "resume: launching the agent window failed; leaving it out",
                ),
            }
        }
        if let Some(window_id) = focus_window {
            let _ = self
                .cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run();
        }
    }

    fn split_tab_pane(
        &self,
        opts: &TabOptions,
        direction: &str,
        target: &str,
        pane: &PaneCmd,
    ) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                direction.to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{pane_id}".to_owned(),
                "-t".to_owned(),
                target.to_owned(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .args(pane.argv.clone())
            .run()?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "split-window did not print a pane id".to_owned(),
            });
        }
        Ok(pane_id)
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
    /// and rich-key handling. All twelve sets ride one batched client
    /// invocation — this runs on the `rimz start` birth path, where a fork per
    /// option was the dominant cost.
    fn apply_room_options(&self, session: &str, config: &TmuxConfig) -> Result<()> {
        let mut commands: Vec<Vec<String>> = Vec::new();
        for (key, value) in tmux_server_options(config) {
            commands.push(vec![
                "set-option".to_owned(),
                "-s".to_owned(),
                key.to_owned(),
                value,
            ]);
        }
        for (key, value) in tmux_session_options(config) {
            commands.push(vec![
                "set-option".to_owned(),
                "-t".to_owned(),
                session.to_owned(),
                key.to_owned(),
                value,
            ]);
        }
        for (key, value) in tmux_window_options(config) {
            commands.push(vec![
                "set-window-option".to_owned(),
                "-t".to_owned(),
                session.to_owned(),
                key.to_owned(),
                value,
            ]);
        }
        self.batch(&commands)
    }
}

/// Pane title the sidebar renderer sets through the terminal title escape. The
/// host binary is now `rimz`, so tmux identifies chrome through this title
/// instead of the foreground command name.
const SIDEBAR_PANE_TITLE: &str = "rimz-sidebar";

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
    pane.command.as_deref() == Some(SIDEBAR_PANE_TITLE)
}

/// Group a pane list into per-window [`ViewSidebars`] for the reconcile planner:
/// each window's sidebar panes and whether it holds a user-working pane. Managed
/// daemon hosts in `rimzd` are not work. Panes with no window id are skipped.
/// First-seen window order.
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
                has_daemon_host: false,
            });
            views.len() - 1
        });
        if is_tmux_sidebar(pane) {
            views[slot].sidebar_panes.push(pane.pane_id.clone());
        } else if crate::remote_control::pane_is_host(pane) {
            views[slot].has_daemon_host = true;
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
        let pin = crate::workspace::pin_env(&opts.workspace_id, &opts.project_root);
        // `new-session -d` births detached; an already-live room answers
        // `duplicate session` (exit 1), which is the goal state and treated as
        // success below. `-A` is unusable here: on a live session it switches
        // to the attach path, which ignores `-d`/`-e`/`-x`/`-y` and needs a
        // terminal on stdin — `CommandSpec` nulls stdin, so it exits 1 with
        // `open terminal failed` (docs/externals/mux-adapter/tmux-reference.md).
        let mut spec = self.cmd().args([
            "new-session".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            opts.session_name.clone(),
            "-c".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
        ]);
        // The identity pin lands in the session environment at birth (`-e`),
        // so the first window's panes already inherit it — `set-environment`
        // below would only reach panes created after it runs.
        for (key, value) in &pin {
            spec = spec.args(["-e".to_owned(), format!("{key}={value}")]);
        }
        // Birth the detached session at the launching terminal's geometry
        // (instead of tmux's 80×24 default), so a fixed-column sidebar split
        // is already correct before the client attaches. The duplicate path
        // skips creation entirely, so a re-ensure never resizes a live room.
        if let Some((cols, rows)) = opts.detected_size {
            spec = spec.args([
                "-x".to_owned(),
                cols.to_string(),
                "-y".to_owned(),
                rows.to_string(),
            ]);
        }
        match spec.run() {
            Ok(_) => {}
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("duplicate session") => {}
            Err(err) => return Err(err),
        }
        // The duplicate path never saw `-e`, so the pin is re-asserted
        // idempotently: future panes of a pre-pin room inherit it; existing
        // panes keep the env they were born with and their participants fall
        // back to the static ladder.
        for (key, value) in &pin {
            self.cmd()
                .args(["set-environment", "-t", &opts.session_name, key, value])
                .run()?;
        }
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
        let timeout = opts.command_timeout.unwrap_or(super::COMMAND_TIMEOUT);
        let mut spec = self.cmd().args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_pid}\t#{pane_active}\t#{window_name}\t#{pane_title}",
        ]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run_with_timeout(timeout)?;
        let panes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_pane_line)
            .collect();
        Ok(panes)
    }

    fn focused_client_panes(&self, opts: ClientFocusOptions) -> Result<Vec<PaneId>> {
        let timeout = opts.command_timeout.unwrap_or(super::COMMAND_TIMEOUT);
        let mut spec = self.cmd().args(["list-clients", "-F", "#{pane_id}"]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run_with_timeout(timeout)?;
        Ok(parse_focused_client_panes(&output.stdout))
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
        // `select-pane` activates within its window only — it never switches
        // the session's current window — so a cross-window jump needs
        // `select-window` first. A pane id resolves as a window target to the
        // window holding it, and both verbs ride one batched client call.
        self.batch(&[
            vec![
                "select-window".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
            vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
        ])
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
        //   tmux split-window -d -h -l <cols> -b -t <session> 'rimz sidebar serve ...'
        // `-d` keeps the spawning client focused on its existing pane;
        // `-b` places the new pane before the target so the sidebar sits
        // on the left. Workspace identity is passed directly to the spawned
        // renderer command.
        // The split sizes from the just-born window: `ensure_session` birthed
        // it at the probed `-x`/`-y` geometry (or an existing room sits at its
        // clients' real geometry), so `target_cols` of the live width is the
        // start verdict in columns. The verdict's percentage spelling is the
        // safe fallback when the width is unreadable.
        let size = match self.window_width(&opts.session_name) {
            Some(total) => opts.width.target_cols(total).to_string(),
            None => format!("{}%", opts.birth_size.percent),
        };
        let command = sidebar_serve_command(opts);
        let mut split = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            "-h".to_owned(),
            "-l".to_owned(),
            size,
            "-b".to_owned(),
            "-t".to_owned(),
            opts.session_name.clone(),
        ];
        split.extend(command.iter().cloned());

        // Cross-backend parity (DESIGN.md): a Zellij session's layout doubles
        // as its tab template, so every new tab is born with the same
        // sidebar+terminal split. tmux has no tab template, so we install a
        // session-scoped `after-new-window` hook that re-runs the same left
        // split in each new window. `-b -d` keep the sidebar left and focus on
        // the new window's terminal, exactly as the initial window. The hook
        // pins the verdict's fixed columns: a new window instantiates at the
        // attached client's real geometry, and a raw percentage there would
        // re-evaluate against it — exactly how the cap used to vanish.
        let serve = command.join(" ");
        let cols = opts.birth_size.cols;
        let hook = format!("split-window -h -b -d -l {cols} '{serve}'");
        let set_hook = vec![
            "set-hook".to_owned(),
            "-t".to_owned(),
            opts.session_name.clone(),
            "after-new-window".to_owned(),
            hook,
        ];
        // One client invocation births the sidebar and installs the hook.
        self.batch(&[split, set_hook])?;
        // With the `after-new-window` hook installed, re-seed the reborn
        // session's prior agents: each becomes its own window, born
        // `sidebar | agent` as the hook docks the sidebar on its left.
        self.seed_resume_windows(opts);
        Ok(())
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // tmux re-adds a sidebar in place with the same left split the initial
        // window got — `-d` keeps the user's focus, `-l <pct>%` sets the width —
        // and drops a stray sidebar with `kill-pane -t`; no move/resize/refocus
        // dance and no session teardown is needed. `split-window` mounts fine on
        // a detached session, so tmux never defers an add the way the Zellij
        // backend must (its detached screen thread drops the mount). Geometry
        // convergence is likewise a deliberate no-op here: `-b` births every
        // sidebar left at the layout width synchronously, so the mis-mounted
        // right/50% shape Zellij repairs cannot occur.
        let panes = self.list_panes(PaneListOptions {
            session_name: Some(opts.session_name.clone()),
            ..Default::default()
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

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        let Some((first_column, rest_columns)) = opts.panes.columns.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "tab layout has no columns".to_owned(),
            });
        };
        let Some((first, first_column_rest)) = first_column.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "tab layout has an empty column".to_owned(),
            });
        };
        let output = self
            .cmd()
            .args([
                "new-window".to_owned(),
                "-d".to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{window_id}\t#{pane_id}".to_owned(),
                "-t".to_owned(),
                opts.session_name.clone(),
                "-n".to_owned(),
                opts.title.clone(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .args(first.argv.clone())
            .run()?;
        let (window_id, first_pane) = parse_new_window_ids(&output.stdout)?;

        let mut column_anchors = vec![first_pane.clone()];
        let mut previous_in_column = first_pane;
        for pane in first_column_rest {
            previous_in_column = self.split_tab_pane(opts, "-v", &previous_in_column, pane)?;
        }
        for column in rest_columns {
            let Some((top, rows)) = column.split_first() else {
                return Err(MuxErr::Output {
                    program: "tmux".to_owned(),
                    reason: "tab layout has an empty column".to_owned(),
                });
            };
            let target = column_anchors
                .last()
                .cloned()
                .unwrap_or_else(|| window_id.clone());
            let new_column = self.split_tab_pane(opts, "-h", &target, top)?;
            column_anchors.push(new_column.clone());
            let mut previous = new_column;
            for row in rows {
                previous = self.split_tab_pane(opts, "-v", &previous, row)?;
            }
        }
        if opts.focus {
            self.cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run()?;
        }
        Ok(())
    }

    fn wake_sidebar(&self, _session_name: &str, _bytes: &[u8]) -> Result<()> {
        // tmux has no pipe equivalent; the sidebar wakeup socket is the
        // only channel. Socket fanout lives above this trait in the ledger
        // module.
        Ok(())
    }

    fn version(&self) -> Result<String> {
        if let Some(cached) = self.version.get() {
            return Ok(cached.clone());
        }
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
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // First writer wins on a probe race; both raced probes read one binary.
        Ok(self.version.get_or_init(|| raw).clone())
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
        view_name: trimmed_nonempty(7),
        is_focused: cols.get(6).is_some_and(|value| value.trim() == "1"),
        command: if cols
            .get(8)
            .is_some_and(|value| value.trim() == SIDEBAR_PANE_TITLE)
        {
            Some(SIDEBAR_PANE_TITLE.to_owned())
        } else {
            trimmed_nonempty(3)
        },
        cwd: trimmed_nonempty(4),
        pane_pid: cols
            .get(5)
            .and_then(|value| value.trim().parse::<u32>().ok()),
        // tmux has no per-pane process-start format variable; the sidebar
        // producer derives the stamp from `pane_pid` via `/proc`
        // (`sidebar::produce::panes::stamp_pane_process_starts`).
        pane_process_start: None,
    })
}

fn parse_focused_client_panes(stdout: &[u8]) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for raw in String::from_utf8_lossy(stdout).lines().map(str::trim) {
        if !raw.starts_with('%') {
            continue;
        }
        let pane = PaneId::from_parts(MuxName::Tmux, raw);
        if !panes.iter().any(|known| known == &pane) {
            panes.push(pane);
        }
    }
    panes
}

fn parse_new_window_ids(stdout: &[u8]) -> Result<(String, String)> {
    let raw = String::from_utf8_lossy(stdout);
    let mut cols = raw.trim().split('\t');
    let window = cols.next().unwrap_or_default().trim();
    let pane = cols.next().unwrap_or_default().trim();
    if window.is_empty() || pane.is_empty() {
        return Err(MuxErr::Output {
            program: "tmux".to_owned(),
            reason: format!("new-window did not print window and pane ids: {raw:?}"),
        });
    }
    Ok((window.to_owned(), pane.to_owned()))
}

// ── Control-mode presence stream ──────────────────────────────────────────────

/// A live tmux control-mode presence stream — the tmux fast path for pane
/// topology (docs/internals/multiplexers.md). Attaches a read-only (`-r`),
/// output-suppressed (`-f no-output`) control client to one session and
/// surfaces a nudge per presence-relevant notification: a window opened or
/// closed, a layout change (a split opened/closed inside a window). Poll stays
/// truth — a dropped stream loses only latency, never correctness, and the
/// consumer respawns it.
pub struct PresenceWatch {
    child: std::process::Child,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    /// Held open for the stream's lifetime: a control client exits on stdin
    /// EOF, which doubles as the no-leak guarantee — if this process dies, the
    /// pipe closes and tmux reaps the client.
    _stdin: Option<std::process::ChildStdin>,
}

impl PresenceWatch {
    /// Attach a control client to `session` (on `socket` when given, else the
    /// default server). `$TMUX` is dropped from the child's env so the nested
    /// attach is deliberate rather than refused.
    pub fn attach(socket: Option<&std::path::Path>, session: &str) -> std::io::Result<Self> {
        use std::io::BufRead as _;
        let mut cmd = std::process::Command::new("tmux");
        if let Some(socket) = socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.args([
            "-C",
            "attach-session",
            "-r",
            "-f",
            "no-output",
            "-t",
            session,
        ])
        .env_remove("TMUX")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("tmux control client spawned without a stdout pipe")
        })?;
        Ok(Self {
            child,
            lines: std::io::BufReader::new(stdout).lines(),
            _stdin: stdin,
        })
    }

    /// Block until the next presence-relevant notification. `None` when the
    /// stream ends — the client was detached, the server exited, or the pipe
    /// broke — after which the watch is spent and the caller re-attaches.
    pub fn next_presence(&mut self) -> Option<()> {
        loop {
            let line = self.lines.next()?.ok()?;
            if is_presence_event(&line) {
                return Some(());
            }
        }
    }
}

impl Drop for PresenceWatch {
    fn drop(&mut self) {
        // Best-effort: the stdin pipe closing already detaches the client;
        // the kill only hurries a wedged one along.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The control-mode socket of the server this process is running inside, from
/// `$TMUX` (`<socket>,<pid>,<session-idx>`). `None` outside tmux.
pub fn control_socket_from_env() -> Option<PathBuf> {
    control_socket_from(&std::env::var("TMUX").ok()?)
}

fn control_socket_from(raw: &str) -> Option<PathBuf> {
    let socket = raw.split(',').next()?.trim();
    (!socket.is_empty()).then(|| PathBuf::from(socket))
}

/// Whether a control-mode notification line reports a pane-topology change.
/// Only presence moves the sidebar: window add/close (linked or not) and
/// layout changes (a split opened/closed). Everything else — `%output`
/// (suppressed by `-f no-output` anyway), command replies (`%begin`/`%end`),
/// focus and mode changes — stays silent.
fn is_presence_event(line: &str) -> bool {
    [
        "%window-add",
        "%unlinked-window-add",
        "%window-close",
        "%unlinked-window-close",
        "%layout-change",
        "%sessions-changed",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
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
            command: Some(command.to_owned()),
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn views_with_sidebars_groups_by_window_and_flags_working() {
        let panes = vec![
            tmux_pane("%1", "@0", "sh"),               // working pane
            tmux_pane("%2", "@0", SIDEBAR_PANE_TITLE), // its sidebar
            tmux_pane("%3", "@0", SIDEBAR_PANE_TITLE), // a duplicate sidebar
            tmux_pane("%4", "@1", SIDEBAR_PANE_TITLE), // a sidebar-only window
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

        // window @1 is a sidebar-only orphan: no working pane and no daemon host.
        assert_eq!(views[1].view, "@1");
        assert!(
            !views[1].has_working,
            "a sidebar-only window holds no working pane",
        );
        assert!(!views[1].has_daemon_host);
        assert_eq!(views[1].sidebar_panes.len(), 1);
    }

    #[test]
    fn views_with_sidebars_ignores_daemon_hosts_as_working_panes() {
        let mut host = tmux_pane("%1", "@0", "rimz");
        host.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
        let panes = vec![host];
        let views = tmux_views_with_sidebars(&panes);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].view, "@0");
        assert!(!views[0].has_working);
        assert!(
            views[0].has_daemon_host,
            "a daemon host marks the view so reload never collapses it as an orphan",
        );
        assert!(views[0].sidebar_panes.is_empty());
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
        assert!((3, 5, 0) >= MIN_TMUX_VERSION);
        assert!((3, 6, 0) >= MIN_TMUX_VERSION);
        // 3.4 lacks `extended-keys-format`, which the room options set
        // unconditionally — below the floor.
        assert!((3, 4, 0) < MIN_TMUX_VERSION);
        assert!((3, 2, 0) < MIN_TMUX_VERSION);
    }

    #[test]
    fn version_serves_the_memoized_probe() {
        let backend = TmuxBackend::default();
        backend
            .version
            .set("tmux 9.9".to_owned())
            .expect("a fresh instance has not probed yet");
        // The cache is consulted before any probe: the seeded value comes back
        // verbatim — no `tmux -V` fork, no overwrite by a real binary.
        assert_eq!(backend.version().expect("cached version"), "tmux 9.9");
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
        // session, window_id, pane_id, command, cwd, pid, pane_active,
        // window_name.
        let row = "rimz-qe\t@1\t%3\tnvim\t/home/u/qe\t4242\t1\tqe";
        let pane = parse_pane_line(row).expect("full row parses");
        assert_eq!(pane.pane_id.raw(), "%3");
        assert_eq!(pane.session_name, "rimz-qe");
        assert_eq!(pane.view_id.as_deref(), Some("@1"));
        assert_eq!(pane.view_name.as_deref(), Some("qe"));
        assert_eq!(pane.command.as_deref(), Some("nvim"));
        assert_eq!(pane.cwd.as_deref(), Some("/home/u/qe"));
        assert_eq!(pane.pane_pid, Some(4242));
        assert!(pane.is_focused, "pane_active=1 is focused");
        assert_eq!(
            pane.pane_process_start, None,
            "tmux has no per-pane process-start variable; the /proc stamp owns it",
        );

        // A pane_active=0 row is not focused.
        let other = "rimz-qe\t@1\t%4\tzsh\t/home/u/qe\t4243\t0\tqe";
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
    }

    #[test]
    fn parse_pane_line_skips_rows_missing_core_columns() {
        assert!(
            parse_pane_line("rimz-qe\t@1").is_none(),
            "needs session+window+pane"
        );
        assert!(parse_pane_line("").is_none());
    }

    #[test]
    fn parse_focused_client_panes_reads_unique_tmux_panes() {
        let panes = parse_focused_client_panes(b"%10\n%10\n%11\n");
        assert_eq!(
            panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%10"),
                PaneId::from_parts(MuxName::Tmux, "%11"),
            ]
        );
    }

    #[test]
    fn parse_focused_client_panes_ignores_malformed_rows() {
        assert!(parse_focused_client_panes(b"\nno-pane\n@1\n").is_empty());
    }

    #[test]
    fn presence_filter_accepts_topology_and_skips_noise() {
        for line in [
            "%window-add @5",
            "%unlinked-window-add @6",
            "%window-close @5",
            "%unlinked-window-close @6",
            "%layout-change @1 b25d,208x60,0,0{104x60,0,0,1,103x60,105,0,2}",
            "%sessions-changed",
        ] {
            assert!(is_presence_event(line), "{line}");
        }
        for line in [
            "%begin 1622 0 1",
            "%end 1622 0 1",
            "%output %1 aGVsbG8=",
            "%window-pane-changed @1 %2",
            "%client-session-changed /dev/pts/3 $1 main",
            "%pane-mode-changed %2",
            "%window-renamed @1 build",
            "",
        ] {
            assert!(!is_presence_event(line), "{line}");
        }
    }

    #[test]
    fn control_socket_parses_the_tmux_env_shape() {
        assert_eq!(
            control_socket_from("/tmp/tmux-1000/default,12345,0"),
            Some(PathBuf::from("/tmp/tmux-1000/default"))
        );
        assert_eq!(control_socket_from(""), None);
        assert_eq!(control_socket_from(",12345,0"), None);
    }
}
