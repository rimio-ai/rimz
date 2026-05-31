//! Bounded subprocess helpers for integration tests.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum wall time for a mux control command in tests. Healthy control
/// commands answer in milliseconds; this only catches wedged children.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

pub trait CommandTimeoutExt {
    fn bounded_output(&mut self) -> io::Result<Output>;

    fn bounded_status(&mut self) -> io::Result<ExitStatus>;
}

impl CommandTimeoutExt for Command {
    fn bounded_output(&mut self) -> io::Result<Output> {
        let debug = format!("{self:?}");
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().map(drain_pipe);
        let stderr = child.stderr.take().map(drain_pipe);
        let deadline = Instant::now() + COMMAND_TIMEOUT;
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
                return Err(timeout_error(&debug));
            }
            thread::sleep(POLL_STEP);
        }
    }

    fn bounded_status(&mut self) -> io::Result<ExitStatus> {
        let debug = format!("{self:?}");
        let mut child = self.spawn()?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timeout_error(&debug));
            }
            thread::sleep(POLL_STEP);
        }
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

fn timeout_error(command: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{command} did not finish within {COMMAND_TIMEOUT:?}; killed"),
    )
}
