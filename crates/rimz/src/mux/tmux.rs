//! tmux `MuxBackend` implementation.
//!
//! Every command runs `tmux [-S <socket>] <verb> ...`. The optional socket
//! lives on the struct so integration tests can isolate each test's server
//! from the user's running tmux. Production code constructs the unit form
//! (`TmuxBackend::default()`) and inherits the system default socket.
//!
//! Caveats live in `docs/internals/sidebar/multiplexers.md` under "tmux backend
//! caveats" — namely that the managed sidebar pane is the channel of record.

mod backend;
mod options;
mod parse;
mod presence;
mod window;

pub(crate) use presence::PresenceRoster;
pub use presence::{ControlLine, PresenceWatch, control_socket_from_env};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use options::{
    tmux_server_append_options, tmux_server_options, tmux_session_options,
    tmux_soft_newline_bindings, tmux_window_options,
};

use super::{CommandSpec, MuxBackend, Result};
use crate::config::TmuxConfig;

/// Minimum tmux version that supports the room options Rimz applies across all
/// supported hosts: `extended-keys-format` (3.5) and `allow-passthrough` (3.3).
pub const MIN_TMUX_VERSION: (u32, u32, u32) = (3, 5, 0);

/// The tmux server socket Rimz addresses by default:
/// `${TMUX_TMPDIR:-/tmp}/tmux-<uid>/default`.
///
/// Reports the default socket the local backend uses; a `-S` override
/// (test-only today) is not reflected.
pub fn default_server_socket_path() -> PathBuf {
    let tmpdir = std::env::var_os("TMUX_TMPDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    default_server_socket_path_from(&tmpdir, nix::unistd::Uid::current().as_raw())
}

fn default_server_socket_path_from(tmpdir: &Path, uid: u32) -> PathBuf {
    tmpdir.join(format!("tmux-{uid}")).join("default")
}

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
        popup_supported: meets_min_version,
    })
}

/// Parse `"tmux 3.5a"` (and tolerant of leading/trailing whitespace and the
/// alphabetic patch-letter suffix tmux uses for point releases).
pub(crate) fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
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
    /// Override for the tmux server socket.
    socket: Option<PathBuf>,
    /// Memoized `tmux -V` stdout ([`MuxBackend::version`]).
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
    pub(super) fn cmd(&self) -> CommandSpec {
        let mut spec = CommandSpec::new("tmux");
        if let Some(socket) = &self.socket {
            spec = spec.args(["-S".to_owned(), socket.to_string_lossy().into_owned()]);
        }
        spec
    }

    /// Run several tmux commands in one client invocation.
    pub(super) fn batch(&self, commands: &[Vec<String>]) -> Result<()> {
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

    /// The session's first window index (`base-index`, default 0).
    pub(super) fn base_index(&self) -> String {
        self.cmd()
            .args(["show-options", "-gv", "base-index"])
            .run()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "0".to_owned())
    }

    /// Apply Rimz's tmux room options.
    pub(super) fn apply_room_options(&self, session: &str, config: &TmuxConfig) -> Result<()> {
        let mut commands: Vec<Vec<String>> = Vec::new();
        for (key, value) in tmux_server_options(config) {
            commands.push(vec![
                "set-option".to_owned(),
                "-s".to_owned(),
                key.to_owned(),
                value,
            ]);
        }
        // Re-appending `*:extkeys` is idempotent for tmux and preserves user entries.
        for (key, value) in tmux_server_append_options(config) {
            commands.push(vec![
                "set-option".to_owned(),
                "-as".to_owned(),
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
        commands.extend(tmux_soft_newline_bindings(config));
        self.batch(&commands)
    }
}

#[cfg(test)]
mod tests;
