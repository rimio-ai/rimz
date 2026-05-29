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
    BackgroundViewLaunch, BackgroundViewOptions, CommandSpec, MuxBackend, MuxErr, PaneCapture,
    PaneListOptions, Result, SessionOptions, SidebarPaneOptions, SidebarRecovery, SplitPaneOptions,
    ensure_pane_backend,
};
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
            .map(|_| ())
    }

    fn attach_command(&self, name: &str) -> CommandSpec {
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
        let mut panes = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let cols: Vec<_> = line.split('\t').collect();
            if cols.len() < 3 {
                continue;
            }
            let session_name = cols[0].to_owned();
            let view_id = Some(cols[1].to_owned());
            let raw = cols[2].to_owned();
            let command = cols
                .get(3)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let cwd = cols
                .get(4)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let pane_pid = cols
                .get(5)
                .and_then(|value| value.trim().parse::<u32>().ok());
            let pane_process_start = cols
                .get(6)
                .and_then(|value| value.trim().parse::<i64>().ok())
                .and_then(|seconds| Timestamp::from_second(seconds).ok());
            let is_focused = cols.get(7).is_some_and(|value| value.trim() == "1");
            let view_name = cols
                .get(8)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            panes.push(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Tmux, &raw),
                session_name,
                view_id,
                view_kind: Some(ViewKind::Window),
                view_name,
                is_focused,
                command,
                cwd,
                pane_pid,
                pane_process_start,
            });
        }
        Ok(panes)
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

    fn open_sidebar(&self, opts: &SidebarPaneOptions) -> Result<()> {
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

    fn recover_sidebars(&self, opts: &SidebarPaneOptions) -> Result<SidebarRecovery> {
        // tmux re-adds a sidebar in place with the same left split the initial
        // window got — `-d` keeps the user's focus, `-l <pct>%` sets the width —
        // so no move/resize/refocus dance and no session teardown is needed.
        let panes = self.list_panes(PaneListOptions {
            session_name: Some(opts.session_name.clone()),
        })?;
        let classified: Vec<(String, bool)> = panes
            .iter()
            .filter_map(|pane| {
                pane.view_id
                    .clone()
                    .map(|view| (view, is_tmux_sidebar(pane)))
            })
            .collect();
        let mut report = SidebarRecovery::default();
        for window in &super::views_missing_sidebar(&classified) {
            match self.add_sidebar_to_window(opts, window) {
                Ok(()) => report.recovered += 1,
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        window = %window,
                        error = %err,
                        "sidebar recovery: in-place add failed; leaving the window without a sidebar",
                    );
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        // Idempotent on the window name; a relaunch into a session already
        // carrying the view is a no-op. A failed query propagates rather than
        // risk a duplicate window.
        if self.session_has_window(&opts.session_name, &opts.name)? {
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // `-d` opens the window without pulling the user's focus to it; the
        // command runs as the window's process.
        self.cmd()
            .args([
                "new-window".to_owned(),
                "-d".to_owned(),
                "-t".to_owned(),
                opts.session_name.clone(),
                "-n".to_owned(),
                opts.name.clone(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .args(opts.command.clone())
            .run()
            .map(|_| BackgroundViewLaunch::Launched)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
