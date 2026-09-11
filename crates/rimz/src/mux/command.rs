//! The bounded subprocess engine every mux control command runs through.
//!
//! [`CommandSpec`] builds a `zellij`/`tmux` invocation that either runs to
//! completion under a deadline ([`CommandSpec::run`]) or hands itself back to
//! the caller as a [`Command`] for an interactive attach. Pure
//! process/thread/timeout machinery — no panes, no sessions, no backends.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::{MuxErr, Result};

/// Upper bound on a single control-command round-trip ([`CommandSpec::run`]).
/// Generous — a real `zellij`/`tmux` control command answers in milliseconds, so
/// this only ever fires on a wedged child (a Zellij action client spinning
/// against a dead server), bounding the hang instead of letting it run forever.
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Tight bound for `list-sessions`: a read-only local query that runs on hot
/// paths such as room start and liveness checks.
pub(crate) const LIST_SESSIONS_TIMEOUT: Duration = Duration::from_secs(3);

/// A built-up command we can run or hand back to an interactive caller.
#[derive(Clone, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Keys cleared from the inherited environment before `env` is applied.
    pub env_remove: BTreeSet<String>,
    pub cwd: Option<PathBuf>,
    stdin: Option<Vec<u8>>,
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("env_remove", &self.env_remove)
            .field("cwd", &self.cwd)
            .field("stdin_len", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            env_remove: BTreeSet::new(),
            cwd: None,
            stdin: None,
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

    /// Clear an inherited variable for the child. [`Self::env`] adds to the
    /// parent environment rather than replacing it, so an inherited value must
    /// be dropped explicitly.
    pub(crate) fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.insert(key.into());
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Feed payload bytes to a bounded control command, then close stdin.
    pub fn stdin_bytes(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Render program and args joined with spaces for human status lines.
    /// `remote::display_ssh_command` remains the shell-safe, pasteable SSH
    /// variant because remote snippets need quoting.
    pub fn display_line(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        for key in &self.env_remove {
            command.env_remove(key);
        }
        command.envs(&self.env);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }

    /// Run the command with raw exit status and captured output. Use this only
    /// where the caller deliberately accepts an unbounded process. Anything
    /// that can wedge on a server should use `Self::output_raw_with_timeout`.
    pub fn output_raw(&self) -> Result<Output> {
        self.to_command()
            .output()
            .map_err(|err| self.spawn_error(err))
    }

    /// Run the command with raw exit status and captured output, bounded by
    /// `timeout`. Callers inspect nonzero status themselves. The child's
    /// stdout/stderr are drained on threads so a full pipe never deadlocks the
    /// wait, and the wait itself is event-driven: a waiter thread blocks in
    /// `wait()` and posts the exit status over a channel, so the common (fast)
    /// path wakes the instant the child exits — no poll step, no added latency.
    /// On the deadline the child is SIGKILLed by pid, the waiter's `wait()`
    /// reaps it, and a [`MuxErr::Timeout`] is returned.
    pub(crate) fn output_raw_with_timeout(&self, timeout: Duration) -> Result<Output> {
        let started = Instant::now();
        let result = self.run_bounded_inner(timeout);
        crate::lane::add_mux_wait_ms(duration_ms(started.elapsed()));
        result
    }

    /// Run the command to completion and capture its output, bounded by
    /// `COMMAND_TIMEOUT`. A control command (`zellij action …`, `tmux …`)
    /// finishes in milliseconds; exceeding the bound means it wedged — a Zellij
    /// action client busy-loops at 100% CPU when its session server dies, which
    /// would otherwise hang the caller (and `rimz start`) forever. On the bound
    /// the child is SIGKILLed and a [`MuxErr::Timeout`] returned, so callers — all
    /// of which treat these best-effort — degrade instead of blocking. The
    /// interactive attach never comes through here (the CLI waits on it
    /// without a deadline).
    pub fn run(&self) -> Result<Output> {
        self.run_with_timeout(COMMAND_TIMEOUT)
    }

    /// Like [`Self::run`], but with a caller-chosen bound. The health probe at
    /// `rimz start` uses a tight one so a wedged action client (spinning against
    /// a dead server) is killed in a few seconds rather than stalling the launch
    /// for the full `COMMAND_TIMEOUT`.
    pub fn run_with_timeout(&self, timeout: Duration) -> Result<Output> {
        let output = self.output_raw_with_timeout(timeout)?;
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

    fn run_bounded_inner(&self, timeout: Duration) -> Result<Output> {
        let started = Instant::now();
        crate::proc::testkit::count_spawn();
        let mut child = self
            .to_command()
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| self.spawn_error(err))?;
        let drain = |pipe: Option<Box<dyn io::Read + Send>>| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                let _ = tx.send(buf);
            });
            rx
        };
        let input = self
            .stdin
            .as_ref()
            .zip(child.stdin.take())
            .map(|(bytes, mut pipe)| {
                let bytes = bytes.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let result = pipe.write_all(&bytes);
                    drop(pipe);
                    let _ = tx.send(result);
                });
                rx
            });
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
        let remaining = || timeout.saturating_sub(started.elapsed());
        let timeout_error = || MuxErr::Timeout {
            program: self.program.clone(),
            args: self.args.join(" "),
            seconds: timeout.as_secs(),
        };
        let status = match rx.recv_timeout(remaining()) {
            Ok(status) => status?,
            Err(_) => {
                kill_by_pid(pid);
                // Reap the killed child, but do not join pipe workers: a
                // descendant may still hold their other ends open.
                #[cfg(unix)]
                let _ = waiter.join();
                return Err(timeout_error());
            }
        };
        let stdout = stdout
            .recv_timeout(remaining())
            .map_err(|_| timeout_error())?;
        let stderr = stderr
            .recv_timeout(remaining())
            .map_err(|_| timeout_error())?;
        if let Some(input) = input {
            let result = input
                .recv_timeout(remaining())
                .map_err(|_| timeout_error())?;
            // Preserve the command's stderr on failure; successful commands
            // must not silently accept an incomplete input payload.
            if status.success() {
                result?;
            }
        }
        Ok(Output {
            status,
            stdout,
            stderr,
        })
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
