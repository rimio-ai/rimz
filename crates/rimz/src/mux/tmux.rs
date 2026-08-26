//! tmux `MuxBackend` implementation.
//!
//! Every command runs `tmux -S <socket> <verb> ...` against one RimZ-owned
//! server per runtime domain ([`managed_server_socket_path`]), holding one
//! session per workspace. The socket is always set, so no command can reach
//! the user's default server; integration tests point it at a private path
//! with [`TmuxBackend::with_socket`].
//!
//! Caveats live in `docs/internals/multiplexers.md` under "tmux backend
//! caveats" — namely that the managed sidebar pane is the channel of record.

mod backend;
mod options;
mod parse;
mod presence;
mod window;

pub use presence::{ControlLine, PresenceWatch, TmuxLayoutPane};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use options::{
    tmux_extended_key_bindings, tmux_server_options, tmux_session_options,
    tmux_terminal_features_commands, tmux_window_options,
};
use parse::parse_terminal_features;

use super::{CommandSpec, MuxBackend, MuxErr, Result};
use crate::config::TmuxConfig;

/// Minimum tmux version that supports the room options RimZ applies across all
/// supported hosts: `extended-keys-format` (3.5) and `allow-passthrough` (3.3).
pub const MIN_TMUX_VERSION: (u32, u32, u32) = (3, 5, 0);

/// The tmux server socket RimZ addresses by default:
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

pub fn server_log_file() -> Option<PathBuf> {
    let own_uid = crate::proc::own_uid()?;
    let mut procs = crate::proc::list_processes();
    procs.sort_by_key(|process| process.pid);
    procs
        .into_iter()
        .filter(|process| process.real_uid == own_uid)
        .filter(|process| super::binaries::is_tmux_server_cmdline(&process.cmdline))
        .find_map(|process| {
            let path =
                crate::proc::cwd(process.pid)?.join(format!("tmux-server-{}.log", process.pid));
            path.is_file().then_some(path)
        })
}

pub(crate) fn default_server_socket_path_from(tmpdir: &Path, uid: u32) -> PathBuf {
    tmpdir.join(format!("tmux-{uid}")).join("default")
}

pub(crate) fn managed_server_socket_dir_under(runtime_root: &Path) -> PathBuf {
    runtime_root.join("rimz").join("tmux")
}

/// The one RimZ-owned tmux server endpoint for this runtime domain.
///
/// Every managed tmux command addresses this socket, so any caller
/// reconstructs the same endpoint without a workspace or [`RuntimePaths`]
/// argument. One server holds one session per workspace, which keeps `%pane`
/// ids unambiguous across RimZ and keeps server-global options and root key
/// bindings off the user's own tmux server.
///
/// Deriving it from the resolved runtime root also gives sandboxes and tests
/// their own server for free: a disposable `XDG_RUNTIME_DIR` yields a
/// different socket and therefore a different daemon.
///
/// [`RuntimePaths`]: crate::store::RuntimePaths
pub fn managed_server_socket_path() -> PathBuf {
    managed_server_socket_path_under(&crate::store::paths::runtime_home())
}

/// The managed endpoint for an explicit runtime domain. Socket identity and
/// the environment stamped into managed sessions derive from this one root, so
/// a server's sessions can never disagree with the socket addressing them.
pub fn managed_server_socket_path_under(runtime_root: &Path) -> PathBuf {
    managed_server_socket_dir_under(runtime_root).join("server")
}

/// A RimZ session left on the user's default tmux server by a release that
/// predates the managed endpoint.
///
/// It shares this room's store while its panes are unreachable from the
/// managed server, so it is reported before birth or attach with the exact
/// command that retires it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacySessionConflict {
    pub session: String,
    pub socket: PathBuf,
}

impl LegacySessionConflict {
    /// The one command that resolves this conflict. Scoped to the session, so
    /// unrelated sessions on the default server survive.
    pub fn recovery_command(&self) -> String {
        format!(
            "tmux -S {} kill-session -t {}",
            self.socket.display(),
            self.session
        )
    }
}

/// Look for `session` on the legacy default server, read-only.
///
/// `has-session` against an absent server exits non-zero without starting one,
/// so probing cannot resurrect a default daemon on a host that has none. Only
/// an exact name match counts as a conflict: RimZ owns nothing else there.
pub fn legacy_session_conflict(session: &str) -> Option<LegacySessionConflict> {
    let socket = default_server_socket_path();
    if socket == managed_server_socket_path() {
        return None;
    }
    let status = tmux_cmd(&socket)
        .args(["has-session", "-t", session])
        .output_raw_with_timeout(super::command::LIST_SESSIONS_TIMEOUT)
        .ok()?;
    status.status.success().then(|| LegacySessionConflict {
        session: session.to_owned(),
        socket,
    })
}

/// `tmux -S <socket>` run from a cwd that cannot vanish, with any inherited
/// `$TMUX` cleared. The one place a managed tmux argv is built.
pub(crate) fn tmux_cmd(socket: &Path) -> CommandSpec {
    CommandSpec::new("tmux")
        .args(["-S".to_owned(), socket.to_string_lossy().into_owned()])
        .cwd(MANAGED_SERVER_CWD)
        .env_remove("TMUX")
}

/// Base command addressing the managed server, for the readers outside the
/// backend — doctor, pane bandwidth, the pixel probe, uninstall.
///
/// Use this rather than `CommandSpec::new("tmux")`: a bare argv reaches the
/// user's default server, where RimZ owns nothing. `cargo xtask invariants`
/// enforces this.
pub fn managed_cmd() -> CommandSpec {
    tmux_cmd(&managed_server_socket_path())
}

/// Extract the server socket from tmux's `socket,pid,index` environment value.
pub(crate) fn socket_path_from_tmux_var(value: &str) -> Option<PathBuf> {
    let socket = value.split(',').next()?.trim();
    (!socket.is_empty()).then(|| PathBuf::from(socket))
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

/// Working directory for every managed tmux client.
///
/// A tmux server inherits its cwd from the client that births it, and
/// `spawn.c` honours a pane's `-c` only when `getcwd()` on the server
/// succeeds. A server born in a directory that is later deleted therefore
/// strands every later pane in that deleted directory even when RimZ passes an
/// absolute `-c`. `/` cannot be deleted or unmounted, so birth and rebirth
/// always start from a readable cwd. The Zellij plugin host forks from `/` for
/// the same reason.
const MANAGED_SERVER_CWD: &str = "/";

#[derive(Debug)]
pub struct TmuxBackend {
    /// The tmux server socket every command addresses. Always set: the managed
    /// endpoint by default, a private path under test.
    socket: PathBuf,
    /// Memoized `tmux -V` stdout ([`MuxBackend::version`]).
    version: std::sync::OnceLock<String>,
    /// Guards the one-per-process socket-directory creation in [`Self::cmd`].
    socket_dir: std::sync::OnceLock<()>,
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::with_socket(managed_server_socket_path())
    }
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            version: std::sync::OnceLock::new(),
            socket_dir: std::sync::OnceLock::new(),
        }
    }

    /// Base `CommandSpec`: `tmux -S <socket>`, run from a cwd that cannot
    /// vanish and without an inherited `$TMUX`.
    ///
    /// Clearing `$TMUX` keeps an ambient session from capturing a managed
    /// command — [`CommandSpec::env`] adds to the inherited environment, so a
    /// `rimz` invoked from inside some other tmux would otherwise leak that
    /// endpoint into commands meant for the managed one.
    pub(super) fn cmd(&self) -> CommandSpec {
        self.socket_dir.get_or_init(|| {
            if let Some(parent) = self.socket.parent() {
                // Best-effort: tmux creates the socket but not its directory.
                // A genuine failure surfaces with its fix in `ensure_session`,
                // which owns the precondition.
                let _ = crate::store::paths::ensure_private_runtime_dir(parent);
            }
        });
        tmux_cmd(&self.socket)
    }

    /// Fail fast when the managed endpoint cannot be addressed.
    ///
    /// `cmd` creates the socket directory best-effort so read paths degrade
    /// quietly; birth is the entry point that owns the precondition, so it
    /// reports the real reason instead of letting tmux fail obscurely later.
    pub(super) fn ensure_endpoint_ready(&self) -> Result<()> {
        let Some(parent) = self.socket.parent() else {
            return Ok(());
        };
        crate::store::paths::ensure_private_runtime_dir(parent).map_err(|err| MuxErr::Output {
            program: "tmux".to_owned(),
            reason: format!(
                "cannot prepare the RimZ tmux socket directory {}: {err}",
                parent.display()
            ),
        })?;
        // tmux previously opted out of an AF_UNIX budget check because its own
        // socket directory is short and RimZ did not choose the path. RimZ owns
        // this path now, so a long runtime root can overflow the limit.
        crate::sock::validate_socket_path(&self.socket).map_err(|err| MuxErr::Output {
            program: "tmux".to_owned(),
            reason: err.to_string(),
        })?;
        Ok(())
    }

    /// Prove the server honoured `-c` for a session this call just created.
    ///
    /// tmux only performs the pane's `chdir` while `getcwd()` on the server
    /// succeeds, so a server whose own working directory was deleted silently
    /// strands every later pane there. Reading the birth pane back tests that
    /// property directly, and portably — the alternative, inspecting the
    /// daemon's live cwd, is unavailable on some hosts and only a proxy for
    /// what actually matters.
    pub(super) fn verify_birth_cwd(&self, session: &str, requested: &Path) -> Result<()> {
        let output = self
            .cmd()
            .args([
                "display-message",
                "-p",
                "-t",
                session,
                "-F",
                "#{pane_current_path}",
            ])
            .output_raw_with_timeout(super::command::LIST_SESSIONS_TIMEOUT)?;
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // An unreadable answer is unknown, not unsafe: birth already succeeded,
        // and refusing on a probe that could not speak would strand hosts whose
        // tmux reports nothing here.
        if actual.is_empty() {
            return Ok(());
        }
        if Path::new(&actual) == requested {
            return Ok(());
        }
        Err(MuxErr::ServerCwdUnusable {
            session: session.to_owned(),
            requested: requested.to_path_buf(),
            actual: PathBuf::from(actual),
            socket: self.socket.clone(),
        })
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

    /// Apply RimZ's tmux room options.
    pub(super) fn apply_room_options(&self, session: &str, config: &TmuxConfig) -> Result<()> {
        let terminal_features = self
            .cmd()
            .args(["show-options", "-s", "terminal-features"])
            .run()?;
        let mut commands: Vec<Vec<String>> = Vec::new();
        for (key, value) in tmux_server_options(config) {
            commands.push(vec![
                "set-option".to_owned(),
                "-s".to_owned(),
                key.to_owned(),
                value,
            ]);
        }
        // tmux appends duplicate array entries, so fixed indices make these
        // writes idempotent while the repair commands purge leaked strays.
        commands.extend(tmux_terminal_features_commands(
            config,
            &parse_terminal_features(&terminal_features.stdout),
        ));
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
        commands.extend(tmux_extended_key_bindings(config));
        self.batch(&commands)
    }
}

#[cfg(test)]
mod tests;
