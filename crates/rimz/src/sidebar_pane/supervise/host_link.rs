//! The supervisor's end of a pane painted by the room host: find or start the host, hand it the pane's output, and listen for what it says.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::AttachFailure;
use crate::disk::lock::WorkspaceLock;
use crate::sidebar_pane::app::ServeConfig;
use crate::sidebar_pane::attach::{self, Control, ControlLine, Hello, PROTOCOL, Reply};
use crate::{RuntimePaths, StatePaths};

/// How long a pane waits for a host before showing a retry notice.
pub(super) const HOST_WAIT: Duration = Duration::from_secs(2);
/// How long a connected pane waits for the host's answer. It outlasts the
/// host's wait for the pane's previous attachment to close, so a pane never
/// gives up on a host that is about to accept it.
pub(super) const REPLY_WAIT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(20);

/// What a host must not inherit from the pane that happened to start it. It
/// keeps its session's environment and loses only what belongs to one pane:
/// the pane ids process attribution reads, the channel and worktree path a
/// command defaults its lane from, and the supervisor's instance. Its room pin is set from the
/// verified workspace record, never inherited. Helpers that take a room take
/// it by argv from `host_args`; the environment is the floor.
const HOST_PANE_ENV_REMOVALS: &[&str] = &[
    "TMUX_PANE",
    "ZELLIJ_PANE_ID",
    crate::workspace::ENV_CHANNEL,
    crate::workspace::ENV_WORKTREE_PATH,
    super::INSTANCE_ENV,
];

pub(super) fn host_args(config: &ServeConfig) -> Vec<OsString> {
    [
        "sidebar",
        "host",
        "--mux",
        config.mux.as_str(),
        "--workspace-id",
        config.workspace_id.as_str(),
        "--session-name",
        &config.session_name,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Start the session's host from `exe`, detached from this pane, its stderr
/// appended to `log`. A host refused for want of a verified room pin leaves
/// its reason there too.
pub(super) fn spawn_host(
    exe: &Path,
    config: &ServeConfig,
    runtime: &RuntimePaths,
    state: &StatePaths,
    log: &Path,
) -> io::Result<()> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    // A rendered pane's own log sink is off, so the host's log is where a
    // host that was never started says why. A pane waiting for a host asks again
    // on every retry, so each line says when.
    let mut command = host_command(exe, config, runtime, state).inspect_err(|err| {
        let _ = writeln!(stderr, "{} {err}", jiff::Timestamp::now());
    })?;
    command.stderr(stderr);
    crate::child_process::spawn_detached_reaped(&mut command, "sidebar-host").map(drop)
}

fn host_command(
    exe: &Path,
    config: &ServeConfig,
    runtime: &RuntimePaths,
    state: &StatePaths,
) -> io::Result<Command> {
    let record = crate::workspace::record::read(&state.workspace_record).map_err(|err| {
        io::Error::other(format!(
            "sidebar host for workspace {}: {err}",
            config.workspace_id
        ))
    })?;
    let root = crate::workspace::verify_pin(config.workspace_id.as_str(), &record.project_root)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "sidebar host for workspace {}: workspace record root {} does not verify (root missing or workspace id does not hash from it)",
                    config.workspace_id,
                    record.project_root.display(),
                ),
            )
        })?;
    let mut command = crate::child_process::detached_rimz_command(exe.to_path_buf(), runtime);
    command.args(host_args(config));
    for name in HOST_PANE_ENV_REMOVALS {
        command.env_remove(name);
    }
    command.envs(crate::workspace::pin_env(&config.workspace_id, &root));
    Ok(command)
}

/// Connect to the session's host, starting one when none answers. Every pane
/// of a new room arrives here together: the one that takes the spawn lock
/// starts the host and the rest wait for its socket. `Ok(None)` means the
/// socket did not answer within `wait`; `Err` preserves the start failure.
pub(super) fn connect(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    wait: Duration,
    start_host: impl FnOnce() -> io::Result<()>,
) -> io::Result<Option<UnixStream>> {
    let socket = runtime.sidebar_host_socket_path(config.mux, &config.session_name);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(Some(stream));
    }
    let lock = runtime.sidebar_host_spawn_lock(config.mux, &config.session_name);
    let spawning = WorkspaceLock::try_acquire(&lock).ok().flatten();
    if spawning.is_some() {
        // A host another pane started may have bound between the miss above
        // and the lock.
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Ok(Some(stream));
        }
        if let Err(err) = start_host() {
            warn!(workspace = %config.workspace_id, error = %err, "sidebar host did not start");
            return Err(err);
        }
    }
    let deadline = Instant::now() + wait;
    loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Ok(Some(stream));
        }
        if Instant::now() >= deadline {
            debug!(socket = %socket.display(), "no sidebar host answered");
            return Ok(None);
        }
        std::thread::sleep(CONNECT_RETRY);
    }
}

pub(super) fn hello_for(config: &ServeConfig, supervisor_build: Option<&str>) -> Hello {
    Hello {
        protocol: PROTOCOL.to_owned(),
        instance_id: config.instance_id.clone(),
        pane_id: config.own_pane.clone(),
        supervisor_build: supervisor_build.map(str::to_owned),
        tick_seconds: Some(config.tick_seconds),
        refresh_ms: config.refresh_ms_override,
    }
}

/// What the host said, as far as the supervisor acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostEvent {
    Control(Control),
    /// End of stream with no control: the host died, or dropped this pane.
    Lost,
    /// Nothing within the wait.
    Quiet,
}

/// An accepted attachment. Dropping it detaches the pane.
pub(super) struct HostLink {
    replies: BufReader<UnixStream>,
    /// A control line the last read left unfinished.
    partial: String,
    /// The build painting the pane.
    pub(super) build: Option<String>,
}

impl HostLink {
    /// Hand `output` to the host behind `stream`, retaining a rejected reason.
    pub(super) fn open(
        stream: UnixStream,
        hello: &Hello,
        output: BorrowedFd<'_>,
        wait: Duration,
    ) -> Result<Self, AttachFailure> {
        attach::send_hello(&stream, hello, output)
            .and_then(|()| stream.set_read_timeout(Some(wait)))
            .inspect_err(|err| debug!(error = %err, "sidebar host hello failed"))
            .map_err(|_| AttachFailure::ReplyUnreadable)?;
        let mut replies = BufReader::new(stream);
        match attach::read_line::<Reply>(&mut replies) {
            Ok(Some(Reply::Accept { build })) => Ok(Self {
                replies,
                partial: String::new(),
                build,
            }),
            Ok(Some(Reply::Reject { reason })) => {
                debug!(%reason, "sidebar host rejected this pane");
                Err(AttachFailure::Rejected(reason))
            }
            Ok(None) => Err(AttachFailure::ReplyUnreadable),
            Err(err) => {
                debug!(error = %err, "sidebar host reply unreadable");
                Err(AttachFailure::ReplyUnreadable)
            }
        }
    }

    /// Wait up to `wait` for the host's next word.
    pub(super) fn poll(&mut self, wait: Duration) -> HostEvent {
        if self.replies.get_ref().set_read_timeout(Some(wait)).is_err() {
            return HostEvent::Lost;
        }
        match self.replies.read_line(&mut self.partial) {
            Ok(_) if self.partial.ends_with('\n') => {
                let line = std::mem::take(&mut self.partial);
                match serde_json::from_str::<ControlLine>(&line) {
                    Ok(line) => HostEvent::Control(line.control),
                    // A control this build does not know: nothing to act on.
                    Err(_) => HostEvent::Quiet,
                }
            }
            Ok(_) => HostEvent::Lost,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                HostEvent::Quiet
            }
            Err(_) => HostEvent::Lost,
        }
    }
}

#[cfg(test)]
mod tests;
