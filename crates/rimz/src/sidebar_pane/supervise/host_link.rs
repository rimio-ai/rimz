//! The supervisor's end of a pane painted by the room host: find or start the host, hand it the pane's output, and listen for what it says.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader};
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use tracing::debug;

use crate::RuntimePaths;
use crate::disk::lock::WorkspaceLock;
use crate::sidebar_pane::app::ServeConfig;
use crate::sidebar_pane::attach::{self, Control, ControlLine, Hello, PROTOCOL, Reply};

/// How long a pane waits for a host before its own worker paints it: long
/// enough for a host to start, short enough to read as a slow first frame.
pub(super) const HOST_WAIT: Duration = Duration::from_secs(2);
/// How long a connected pane waits for the host's answer. It outlasts the
/// host's wait for the pane's previous attachment to close, so a pane never
/// gives up on a host that is about to accept it.
pub(super) const REPLY_WAIT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(20);

/// Per-pane and per-session variables a host must not inherit from the pane that happened to start it: it belongs to no pane, and process attribution reads these from its environment. The host gets its room and mux from `host_args` and passes them to its helpers by argv.
const PANE_SCOPED_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
    crate::workspace::ENV_WORKSPACE_ID,
    crate::workspace::ENV_PROJECT_ROOT,
    crate::workspace::ENV_CHANNEL,
    crate::workspace::ENV_WORKTREE_PATH,
    super::WORKER_ENV,
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
/// appended to `log`.
pub(super) fn spawn_host(
    exe: &Path,
    config: &ServeConfig,
    runtime: &RuntimePaths,
    log: &Path,
) -> io::Result<()> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let mut command = crate::child_process::detached_rimz_command(exe.to_path_buf(), runtime);
    command.stderr(stderr).args(host_args(config));
    for name in PANE_SCOPED_ENV {
        command.env_remove(name);
    }
    crate::child_process::spawn_detached_reaped(&mut command, "sidebar-host").map(drop)
}

/// Connect to the session's host, starting one when none answers. Every pane
/// of a new room arrives here together: the one that takes the spawn lock
/// starts the host and the rest wait for its socket. `None` after `wait`
/// leaves the pane to its own worker.
pub(super) fn connect(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    wait: Duration,
    start_host: impl FnOnce() -> io::Result<()>,
) -> Option<UnixStream> {
    let socket = runtime.sidebar_host_socket_path(config.mux, &config.session_name);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Some(stream);
    }
    let lock = runtime.sidebar_host_spawn_lock(config.mux, &config.session_name);
    let spawning = WorkspaceLock::try_acquire(&lock).ok().flatten();
    if spawning.is_some() {
        // A host another pane started may have bound between the miss above
        // and the lock.
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Some(stream);
        }
        if let Err(err) = start_host() {
            debug!(error = %err, "sidebar host did not start; falling back to a worker");
            return None;
        }
    }
    let deadline = Instant::now() + wait;
    loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Some(stream);
        }
        if Instant::now() >= deadline {
            debug!(socket = %socket.display(), "no sidebar host answered; falling back to a worker");
            return None;
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
    /// Hand `output` to the host behind `stream`. `None` is a reject or a
    /// host that went away mid-hello; either leaves the pane to a worker.
    pub(super) fn open(
        stream: UnixStream,
        hello: &Hello,
        output: BorrowedFd<'_>,
        wait: Duration,
    ) -> Option<Self> {
        attach::send_hello(&stream, hello, output)
            .and_then(|()| stream.set_read_timeout(Some(wait)))
            .inspect_err(|err| debug!(error = %err, "sidebar host hello failed"))
            .ok()?;
        let mut replies = BufReader::new(stream);
        match attach::read_line::<Reply>(&mut replies) {
            Ok(Some(Reply::Accept { build })) => Some(Self {
                replies,
                partial: String::new(),
                build,
            }),
            Ok(Some(Reply::Reject { reason })) => {
                debug!(%reason, "sidebar host rejected this pane; falling back to a worker");
                None
            }
            Ok(None) => None,
            Err(err) => {
                debug!(error = %err, "sidebar host reply unreadable; falling back to a worker");
                None
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
