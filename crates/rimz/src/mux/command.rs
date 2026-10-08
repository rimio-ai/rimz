//! The bounded subprocess engine every mux control command runs through.
//!
//! [`CommandSpec`] builds a `zellij`/`tmux` invocation that either runs to
//! completion under a deadline ([`CommandSpec::run`]) or hands itself back to
//! the caller as a [`Command`] for an interactive attach. Pure
//! process/I/O/timeout machinery — no panes, no sessions, no backends.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::{MuxErr, Result};
use crate::child_process::user_temp_env;
use crate::proc::{KillScope, pump_child};

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
    refusal_retry: Option<Box<RefusalRetry>>,
    error_redaction: Option<(usize, &'static str)>,
}

/// A backend's rule for rerunning a command its far side refused before
/// acting on it. The rule belongs to the backend that knows the refusal; the
/// bounded run applies it inside the command's own deadline.
#[derive(Clone, Copy)]
pub(in crate::mux) struct RefusalRetry {
    /// Whether this exit left nothing done and can clear on its own.
    pub(in crate::mux) is_refusal: fn(&CommandSpec, &Output) -> bool,
    pub(in crate::mux) reruns: u32,
    pub(in crate::mux) delay: Duration,
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
    pub(super) fn redact_arg_in_errors(mut self, index: usize, replacement: &'static str) -> Self {
        self.error_redaction = Some((index, replacement));
        self
    }

    fn error_text(&self, text: String) -> String {
        match self.error_redaction {
            Some((index, replacement)) => text.replace(&self.args[index], replacement),
            None => text,
        }
    }

    pub(super) fn command_error(&self, stderr: String) -> MuxErr {
        MuxErr::Command {
            program: self.program.clone(),
            args: self.error_text(self.args.join(" ")),
            stderr: self.error_text(stderr),
        }
    }

    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            env_remove: BTreeSet::new(),
            cwd: None,
            stdin: None,
            refusal_retry: None,
            error_redaction: None,
        }
    }

    pub(in crate::mux) fn retry_refusal(mut self, retry: RefusalRetry) -> Self {
        self.refusal_retry = Some(Box::new(retry));
        self
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

    /// Remove ambient pane/session identity, leaving endpoint configuration
    /// inherited. Explicit values supplied by [`Self::env`] still win.
    pub(crate) fn without_mux_context(self) -> Self {
        super::AMBIENT_MUX_ENV
            .into_iter()
            .fold(self, |spec, key| spec.env_remove(key))
    }

    /// Give the child the `TMPDIR` a launch saved in `saved` (the caller's
    /// [`crate::child_process::USER_TMPDIR_ENV`]), so a mux server started
    /// from an agent's tree never hands panes the agent's temp unit. `None`
    /// leaves the inherited value alone; an empty save removes `TMPDIR`. With
    /// a save, the provider temp-root keys listed in `temp_root_keys` (the
    /// caller's [`crate::child_process::TEMP_ROOT_KEYS_ENV`]) are dropped too,
    /// since they name the same unit. Neither save reaches the child, so a
    /// shell in the room starts its own agents fresh.
    pub(crate) fn restore_user_tmpdir(
        self,
        saved: Option<&str>,
        temp_root_keys: Option<&str>,
    ) -> Self {
        let Some(restore) = user_temp_env(saved, temp_root_keys) else {
            return self;
        };
        let mut spec = self;
        for key in restore.removed {
            spec = spec.env_remove(key);
        }
        match restore.tmpdir {
            Some(tmpdir) => spec.env("TMPDIR", tmpdir),
            None => spec.env_remove("TMPDIR"),
        }
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
    /// stdout/stderr and optional stdin are polled on the calling thread;
    /// their I/O and child exit share the same deadline. On the deadline the
    /// child is SIGKILLed by pid and reaped, and a [`MuxErr::Timeout`] is
    /// returned. A [`RefusalRetry`] rule reruns a refused command inside the
    /// same `timeout`.
    pub(crate) fn output_raw_with_timeout(&self, timeout: Duration) -> Result<Output> {
        let started = Instant::now();
        let mut reruns = 0;
        let result = loop {
            let result = self.run_bounded_inner(timeout, started);
            let Some(retry) = self.refusal_retry.as_deref() else {
                break result;
            };
            let refused = matches!(&result, Ok(output) if (retry.is_refusal)(self, output));
            if !refused || reruns == retry.reruns || started.elapsed() + retry.delay >= timeout {
                break result;
            }
            reruns += 1;
            std::thread::sleep(retry.delay);
        };
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
            return Err(self.command_error(stderr));
        }
        Ok(output)
    }

    /// One spawn under the caller's `timeout`, counted from `started` so a
    /// rerun waits only what is left while the error still names the bound.
    fn run_bounded_inner(&self, timeout: Duration, started: Instant) -> Result<Output> {
        crate::proc::testkit::count_spawn();
        let child = self
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
        let output = pump_child(
            child,
            self.stdin.as_deref(),
            started + timeout,
            KillScope::Process,
        )?;
        if output.timed_out {
            return Err(MuxErr::Timeout {
                program: self.program.clone(),
                args: self.error_text(self.args.join(" ")),
                seconds: timeout.as_secs(),
            });
        }
        // Preserve the command's stderr on failure; successful commands
        // must not silently accept an incomplete input payload.
        if output.status.success()
            && let Some(Err(err)) = output.stdin
        {
            return Err(MuxErr::Io(err));
        }
        Ok(Output {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_process::{TEMP_ROOT_KEYS_ENV, USER_TMPDIR_ENV};

    #[test]
    fn mux_context_removal_preserves_explicit_env_and_endpoints() {
        let spec = CommandSpec::new("zellij")
            .env_remove("ALREADY_REMOVED")
            .env("ZELLIJ_PANE_ID", "terminal_7")
            .without_mux_context()
            .env("TMUX_PANE", "%8");
        let names = [
            "TMUX",
            "TMUX_PANE",
            "ZELLIJ",
            "ZELLIJ_PANE_ID",
            "ZELLIJ_SESSION_NAME",
        ];
        assert_eq!(crate::mux::AMBIENT_MUX_ENV, names);
        for key in names {
            assert!(spec.env_remove.contains(key), "{key} remains inherited");
        }
        assert_eq!(spec.env_remove.len(), names.len() + 1);
        assert!(spec.env_remove.contains("ALREADY_REMOVED"));
        for key in ["TMUX_TMPDIR", "ZELLIJ_SOCKET_DIR"] {
            assert!(!spec.env_remove.contains(key));
        }
        for (key, value) in [("ZELLIJ_PANE_ID", "terminal_7"), ("TMUX_PANE", "%8")] {
            assert_eq!(spec.env.get(key).map(String::as_str), Some(value));
            assert_eq!(
                spec.to_command()
                    .get_envs()
                    .find(|(name, _)| *name == key)
                    .and_then(|(_, value)| value),
                Some(std::ffi::OsStr::new(value)),
            );
        }
    }

    #[test]
    fn a_saved_user_tmpdir_replaces_the_inherited_one() {
        let outside =
            CommandSpec::new("zellij").restore_user_tmpdir(None, Some("CLAUDE_CODE_TMPDIR"));
        assert!(outside.env.is_empty() && outside.env_remove.is_empty());

        let saved = CommandSpec::new("zellij").restore_user_tmpdir(
            Some("/var/folders/t"),
            Some("CLAUDE_CODE_TMPDIR OTHER_TMPDIR"),
        );
        assert_eq!(
            saved.env.get("TMPDIR").map(String::as_str),
            Some("/var/folders/t")
        );
        assert!(saved.env_remove.contains(USER_TMPDIR_ENV));
        assert!(!saved.env.contains_key(USER_TMPDIR_ENV));
        for key in ["CLAUDE_CODE_TMPDIR", "OTHER_TMPDIR", TEMP_ROOT_KEYS_ENV] {
            assert!(saved.env_remove.contains(key), "{key}");
        }

        let none = CommandSpec::new("zellij").restore_user_tmpdir(Some(""), None);
        assert!(none.env.is_empty());
        assert!(none.env_remove.contains("TMPDIR") && none.env_remove.contains(USER_TMPDIR_ENV));

        let pinned = CommandSpec::new("zellij")
            .restore_user_tmpdir(Some(""), None)
            .env("TMPDIR", "/run/test");
        let command = pinned.to_command();
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "TMPDIR")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("/run/test")),
            "an explicit spec TMPDIR still wins"
        );
    }
}
