//! Calling-thread I/O and deadlines for bounded subprocesses.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

/// How the deadline kill reaches the child.
pub(crate) enum KillScope {
    Process,
    Group,
}

/// Reads per stream after the deadline kill, a default 64 KiB pipe's worth: a
/// writer the kill did not reach cannot hold the return past the bound.
const DRAIN_READS: usize = 8;

/// The reap wait after both outputs close starts here and doubles to
/// [`REAP_STEP_MAX`]. An exiting child closes its pipes just before it becomes
/// reapable, so the first `try_wait` usually misses and a fixed 1 ms step
/// would land on most successful runs.
const REAP_STEP_MIN: Duration = Duration::from_micros(40);
const REAP_STEP_MAX: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub(crate) struct BoundedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    /// `None` without input; `Some(Ok(()))` when every byte reached the child.
    /// `Some(Err(_))` if stdin closed or the run ended with bytes unwritten.
    pub(crate) stdin: Option<io::Result<()>>,
    pub(crate) timed_out: bool,
}

/// Drive an already-spawned child to completion on the calling thread.
///
/// The child stays unreaped until both output pipes close or the deadline
/// fires, so its pid cannot be recycled before the deadline signal lands.
/// On timeout, only bytes available without waiting for pipe EOF are drained.
pub(crate) fn pump_child(
    mut child: Child,
    input: Option<&[u8]>,
    deadline: Instant,
    kill_scope: KillScope,
) -> io::Result<BoundedOutput> {
    let mut stdout = nonblocking(child.stdout.take()).unwrap_or_default();
    let mut stderr = nonblocking(child.stderr.take()).unwrap_or_default();
    let mut stdin = input.map(|_| {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "child stdin unavailable or payload incomplete",
        ))
    });
    let mut input_pipe = match nonblocking(input.and_then(|_| child.stdin.take())) {
        Ok(pipe) => pipe,
        Err(err) => {
            stdin = Some(Err(err));
            None
        }
    };
    let bytes = input.unwrap_or_default();
    let mut written = 0;
    if input_pipe.is_some() && bytes.is_empty() {
        input_pipe.take();
        stdin = Some(Ok(()));
    }
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut reap_step = REAP_STEP_MIN;
    let (status, timed_out) = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let pid = Pid::from_raw(child.id() as i32);
            let _ = match kill_scope {
                KillScope::Process => kill(pid, Signal::SIGKILL),
                KillScope::Group => killpg(pid, Signal::SIGKILL),
            };
            let status = child.wait()?;
            for _ in 0..DRAIN_READS {
                let more_stdout = read_pipe(&mut stdout, &mut stdout_bytes);
                let more_stderr = read_pipe(&mut stderr, &mut stderr_bytes);
                if !more_stdout && !more_stderr {
                    break;
                }
            }
            break (status, true);
        }
        if stdout.is_none() && stderr.is_none() {
            input_pipe.take();
            if let Some(status) = child.try_wait()? {
                break (status, false);
            }
            std::thread::sleep(remaining.min(reap_step));
            reap_step = (reap_step * 2).min(REAP_STEP_MAX);
            continue;
        }
        let ready = {
            let mut fds: Vec<_> = [
                stdout
                    .as_ref()
                    .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLIN)),
                stderr
                    .as_ref()
                    .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLIN)),
                input_pipe
                    .as_ref()
                    .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLOUT)),
            ]
            .into_iter()
            .flatten()
            .collect();
            match poll(
                &mut fds,
                PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX),
            ) {
                Ok(_) => {
                    let mut events = fds.iter();
                    [stdout.is_some(), stderr.is_some(), input_pipe.is_some()].map(|open| {
                        open && events
                            .next()
                            .is_some_and(|fd| fd.revents().is_some_and(|events| !events.is_empty()))
                    })
                }
                Err(Errno::EINTR) => continue,
                // Pipe failures are best-effort capture, just like read errors.
                Err(_) => [true; 3],
            }
        };
        if ready[0] {
            read_pipe(&mut stdout, &mut stdout_bytes);
        }
        if ready[1] {
            read_pipe(&mut stderr, &mut stderr_bytes);
        }
        if ready[2]
            && let Some(pipe) = input_pipe.as_mut()
        {
            match pipe.write(&bytes[written..]) {
                Ok(0) => {
                    input_pipe.take();
                    stdin = Some(Err(io::ErrorKind::WriteZero.into()));
                }
                Ok(count) => {
                    written += count;
                    if written == bytes.len() {
                        input_pipe.take();
                        stdin = Some(Ok(()));
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => {
                    input_pipe.take();
                    stdin = Some(Err(err));
                }
            }
        }
    };
    Ok(BoundedOutput {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        stdin,
        timed_out,
    })
}

fn nonblocking<T: AsFd>(pipe: Option<T>) -> io::Result<Option<T>> {
    if let Some(pipe) = pipe.as_ref() {
        fcntl(pipe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    }
    Ok(pipe)
}

fn read_pipe(pipe: &mut Option<impl Read>, bytes: &mut Vec<u8>) -> bool {
    let Some(reader) = pipe.as_mut() else {
        return false;
    };
    let mut buffer = [0; 8192];
    match reader.read(&mut buffer) {
        Ok(0) => {
            pipe.take();
            false
        }
        Ok(count) => {
            bytes.extend_from_slice(&buffer[..count]);
            true
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => true,
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => false,
        Err(_) => {
            pipe.take();
            false
        }
    }
}

/// Capture both output streams on the calling thread within a wall-clock bound.
///
/// Timeout kills the process group, reaps the child, and drains only bytes
/// already available, even if a descendant outside the group holds a pipe.
pub(crate) fn run_bounded_output(
    command: &mut Command,
    timeout: Duration,
) -> io::Result<BoundedOutput> {
    super::testkit::count_spawn();
    command.process_group(0);
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = pump_child(child, None, Instant::now() + timeout, KillScope::Group)?;
    if output.timed_out {
        tracing::debug!(
            program = %command.get_program().to_string_lossy(),
            timeout_ms = timeout.as_millis(),
            "bounded subprocess timed out",
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::thread::{self, spawn};
    use std::time::{Duration, Instant};

    use super::run_bounded_output;

    fn probe_thread_id() -> u64 {
        let id = spawn(|| thread::current().id())
            .join()
            .expect("probe thread");
        format!("{id:?}")
            .strip_prefix("ThreadId(")
            .and_then(|id| id.strip_suffix(')'))
            .expect("ThreadId debug format")
            .parse()
            .expect("numeric ThreadId")
    }

    #[cfg(target_os = "linux")]
    fn thread_count() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .expect("process status")
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .expect("thread count")
            .trim()
            .parse()
            .expect("numeric thread count")
    }

    #[test]
    fn bounded_output_creates_no_threads() {
        #[cfg(target_os = "linux")]
        let threads = thread_count();
        // Nextest runs each test in its own process, so only our calls can
        // advance std's thread-id counter between the two probe threads.
        let before = probe_thread_id();
        for _ in 0..4 {
            let output = run_bounded_output(
                Command::new("sh").args(["-c", "printf out; printf err >&2"]),
                Duration::from_secs(2),
            )
            .expect("bounded output");
            assert!(!output.timed_out);
            assert!(output.status.success());
            assert_eq!(output.stdout, b"out");
            assert_eq!(output.stderr, b"err");
        }
        for script in ["sleep 30 & exec sleep 30", "sleep 1 & exit 0"] {
            let output = run_bounded_output(
                Command::new("sh").args(["-c", script]),
                Duration::from_millis(100),
            )
            .expect("bounded output");
            assert!(output.timed_out, "{script}");
        }
        #[cfg(target_os = "linux")]
        assert_eq!(thread_count(), threads, "bounded runs leave no threads");
        assert_eq!(
            probe_thread_id(),
            before + 1,
            "bounded runs spawn no threads"
        );
    }

    #[test]
    fn bounded_output_captures_large_concurrent_streams() {
        let output = run_bounded_output(
            Command::new("sh").args([
                "-c",
                "head -c 300000 /dev/zero >&2 & head -c 300000 /dev/zero; wait",
            ]),
            Duration::from_secs(5),
        )
        .expect("bounded output");
        assert!(output.status.success());
        assert!(!output.timed_out);
        assert_eq!(output.stdout, vec![0; 300000]);
        assert_eq!(output.stderr, vec![0; 300000]);
    }

    #[test]
    fn bounded_output_captures_both_streams() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf rimz; printf trace >&2"]);

        let output = run_bounded_output(&mut cmd, Duration::from_secs(1)).expect("bounded output");

        assert!(output.status.success());
        assert!(!output.timed_out);
        assert_eq!(output.stdout, b"rimz");
        assert_eq!(output.stderr, b"trace");
    }

    #[test]
    fn bounded_output_kills_and_reaps_on_timeout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);

        let output =
            run_bounded_output(&mut cmd, Duration::from_millis(20)).expect("bounded output");

        assert!(output.timed_out);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_output_kills_pipe_holding_grandchildren_on_timeout() {
        for script in ["sleep 30 & exec sleep 30", "sleep 1 & exit 0"] {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            let started = Instant::now();

            let output =
                run_bounded_output(&mut cmd, Duration::from_millis(100)).expect("bounded output");

            assert!(output.timed_out, "{script}");
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}
