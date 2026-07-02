//! The bounded subprocess engine every mux control command runs through.
//!
//! [`CommandSpec`] builds a `zellij`/`tmux` invocation that either runs to
//! completion under a deadline ([`CommandSpec::run`]) or hands itself back to
//! the caller as a [`Command`] for `exec(3)` (the interactive attach). Pure
//! process/thread/timeout machinery — no panes, no sessions, no backends.

use std::collections::BTreeMap;
use std::io;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::{MuxErr, Result};

/// Upper bound on a single control-command round-trip ([`CommandSpec::run`]).
/// Generous — a real `zellij`/`tmux` control command answers in milliseconds, so
/// this only ever fires on a wedged child (a Zellij action client spinning
/// against a dead server), bounding the hang instead of letting it run forever.
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// A built-up command we can run or hand back to `exec(3)`.
#[derive(Clone, Debug, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
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

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(&self.env);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }

    /// Run the command with raw exit status and captured output. Use this for
    /// probes that need stdout even on nonzero exit (version probes, session
    /// listing, the Zellij birth client); control verbs should use the bounded
    /// runner.
    pub fn output_raw(&self) -> Result<Output> {
        self.to_command()
            .output()
            .map_err(|err| self.spawn_error(err))
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
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            tracing::debug!(
                program = %self.program,
                args = ?self.args,
                stderr = %stderr,
                "mux command exited unsuccessfully",
            );
            return Err(MuxErr::Command {
                program: self.program.clone(),
                args: self.args.join(" "),
                stderr,
            });
        }
        Ok(output)
    }

    /// Spawn the child and wait at most `timeout` for it. Its stdout/stderr are
    /// drained on threads so a full pipe never deadlocks the wait, and the wait
    /// itself is event-driven: a waiter thread blocks in `wait()` and posts the
    /// exit status over a channel, so the common (fast) path wakes the instant
    /// the child exits — no poll step, no added latency. On the deadline the
    /// child is SIGKILLed by pid, the waiter's `wait()` reaps it, and a
    /// [`MuxErr::Timeout`] is returned.
    fn run_bounded(&self, timeout: Duration) -> Result<Output> {
        let started = Instant::now();
        let result = self.run_bounded_inner(timeout);
        crate::lane::add_mux_wait_ms(duration_ms(started.elapsed()));
        result
    }

    fn run_bounded_inner(&self, timeout: Duration) -> Result<Output> {
        crate::proc::testkit::count_spawn();
        let mut child = self
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| self.spawn_error(err))?;
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
        // The waiter owns the child handle (`wait()` needs it); the pid stays
        // here for the deadline kill. The send is best-effort: a receiver that
        // already timed out is gone, and that is fine.
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        match rx.recv_timeout(timeout) {
            Ok(status) => {
                let status = status?;
                let _ = waiter.join();
                let stdout = stdout.join().unwrap_or_default();
                let stderr = stderr.join().unwrap_or_default();
                Ok(Output {
                    status,
                    stdout,
                    stderr,
                })
            }
            // Timeout — or the unreachable disconnected case (the waiter always
            // sends); both mean no exit status arrived, so kill and report.
            Err(_) => {
                kill_by_pid(pid);
                // SIGKILL is not refusable, so the joins reap the child and
                // finish the drains before the error returns. Off unix nothing
                // was killed: skip the joins — the handles detach, and the
                // waiter reaps the child whenever it eventually exits.
                #[cfg(unix)]
                {
                    let _ = waiter.join();
                    let _ = stdout.join();
                    let _ = stderr.join();
                }
                Err(MuxErr::Timeout {
                    program: self.program.clone(),
                    args: self.args.join(" "),
                    seconds: timeout.as_secs(),
                })
            }
        }
    }

    fn spawn_error(&self, err: io::Error) -> MuxErr {
        match err.kind() {
            io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: self.program.clone(),
            },
            _ => MuxErr::Io(err),
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// SIGKILL a timed-out child by pid. Safe against pid reuse: the waiter thread
/// still holds the unreaped child handle (blocked in `wait()`), so the pid
/// cannot be recycled before the signal lands.
#[cfg(unix)]
fn kill_by_pid(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

/// Off unix there is no signal to send; the timeout still returns and the
/// waiter thread reaps the child whenever it eventually exits.
#[cfg(not(unix))]
fn kill_by_pid(_pid: u32) {}
