//! Bounded subprocess helpers for integration tests.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Ambient session prefixes a developer's live room leaks into the suite: the
/// `RIMZ_*` identity pin redirects workspace resolution, and the `ZELLIJ*` /
/// `TMUX*` variables flip backend detection onto the real session. Prefix
/// matching keeps a new pin covered without touching this list.
const AMBIENT_SESSION_PREFIXES: [&str; 3] = ["RIMZ_", "ZELLIJ", "TMUX"];

/// The ambient environment keys [`ScrubSessionEnvExt::scrub_session_env`]
/// drops from a child.
fn ambient_session_keys() -> impl Iterator<Item = String> {
    std::env::vars_os().filter_map(|(key, _)| {
        let key = key.into_string().ok()?;
        AMBIENT_SESSION_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
            .then_some(key)
    })
}

/// Drop every ambient Rimz/mux session variable from a child's environment, so
/// a suite run from inside a live Rimz room behaves like a clean shell. Apply
/// at builder construction: a test that *sets* one of these afterwards wins
/// over the removal. Every builder that runs `rimz` or creates a mux server
/// goes through this — a mux server captures the spawning environment and
/// hands it to every pane it ever creates. Under coverage, point child profile
/// output at the null device: several fixtures intentionally SIGKILL long-lived
/// child `rimz` processes, and a half-written `.profraw` poisons the merge even
/// though the test process itself exited cleanly.
pub trait ScrubSessionEnvExt {
    fn scrub_session_env(&mut self) -> &mut Self;
}

impl ScrubSessionEnvExt for Command {
    fn scrub_session_env(&mut self) -> &mut Self {
        for key in ambient_session_keys() {
            self.env_remove(key);
        }
        suppress_child_coverage(self);
        self
    }
}

impl ScrubSessionEnvExt for portable_pty::CommandBuilder {
    fn scrub_session_env(&mut self) -> &mut Self {
        for key in ambient_session_keys() {
            self.env_remove(key);
        }
        suppress_pty_child_coverage(self);
        self
    }
}

fn suppress_child_coverage(cmd: &mut Command) {
    if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
        cmd.env("LLVM_PROFILE_FILE", profile_sink());
    }
}

fn suppress_pty_child_coverage(cmd: &mut portable_pty::CommandBuilder) {
    if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
        cmd.env("LLVM_PROFILE_FILE", profile_sink());
    }
}

fn profile_sink() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// Maximum wall time for a mux control command in tests. Healthy control
/// commands answer in milliseconds; this only catches wedged children.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

pub trait CommandTimeoutExt {
    fn bounded_output(&mut self) -> io::Result<Output>;

    fn bounded_output_within(&mut self, timeout: Duration) -> io::Result<Output>;

    fn bounded_status(&mut self) -> io::Result<ExitStatus>;

    fn assert_success_within_timeout(&mut self, label: &str) -> ExitStatus;
}

impl CommandTimeoutExt for Command {
    fn bounded_output(&mut self) -> io::Result<Output> {
        self.bounded_output_within(COMMAND_TIMEOUT)
    }

    fn bounded_output_within(&mut self, timeout: Duration) -> io::Result<Output> {
        let debug = format!("{self:?}");
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().map(drain_pipe);
        let stderr = child.stderr.take().map(drain_pipe);
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Output {
                    status,
                    stdout: join_pipe(stdout),
                    stderr: join_pipe(stderr),
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                drop(stdout);
                drop(stderr);
                return Err(timeout_error(&debug, timeout));
            }
            thread::sleep(POLL_STEP);
        }
    }

    fn bounded_status(&mut self) -> io::Result<ExitStatus> {
        let debug = format!("{self:?}");
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timeout_error(&debug, COMMAND_TIMEOUT));
            }
            thread::sleep(POLL_STEP);
        }
    }

    fn assert_success_within_timeout(&mut self, label: &str) -> ExitStatus {
        let status = self
            .bounded_status()
            .unwrap_or_else(|err| panic!("{label} did not finish: {err}"));
        assert!(status.success(), "{label} exited with {status}");
        status
    }
}

fn drain_pipe<R>(mut pipe: R) -> thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn join_pipe(handle: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

fn timeout_error(command: &str, timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{command} did not finish within {timeout:?}; killed"),
    )
}
