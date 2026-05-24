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
    CommandSpec, MuxBackend, MuxErr, PaneCapture, PaneListOptions, Result, SessionOptions,
    SidebarPaneOptions, SplitPaneOptions, ensure_pane_backend,
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
            "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_current_path}",
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
            panes.push(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Tmux, &raw),
                session_name,
                view_id,
                view_kind: Some(ViewKind::Window),
                pane_process_start: None,
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
        let command = vec![
            opts.rimz_bin.to_string_lossy().into_owned(),
            "sidebar".to_owned(),
            "serve".to_owned(),
            "--mux".to_owned(),
            "tmux".to_owned(),
            "--workspace-id".to_owned(),
            opts.workspace_id.as_str().to_owned(),
            "--session-name".to_owned(),
            opts.session_name.clone(),
        ];
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
            .args(command)
            .run()
            .map(|_| ())
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
