//! The hosted subprocess runner shared by loop checks and watches, capacity
//! checks, supervised verify, and workflow command steps and shell checks:
//! one child per call, its output captured to a bounded tail, its deadline,
//! interrupts and SIGTERM handling applied the same way for every caller.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::harness::loop_task::LOOP_TASK_ENV;
use crate::harness::run::RunCancellation;

const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHECK_DRAIN_GRACE: Duration = Duration::from_millis(200);
const CHECK_INTERRUPT_GRACE: Duration = Duration::from_secs(1);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;
pub(in crate::harness) const WAIT_TAIL_CAP: usize = 4 * 1024;

pub struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    interrupted: bool,
    terminated: bool,
    output: String,
    code: Option<i32>,
    stderr_line: String,
}

impl CheckOutcome {
    pub(in crate::harness) fn stderr_line(&self) -> &str {
        &self.stderr_line
    }

    pub(in crate::harness) fn timed_out(&self) -> bool {
        self.timed_out
    }

    pub(in crate::harness) fn new(
        passed: bool,
        timed_out: bool,
        output: String,
        code: Option<i32>,
    ) -> Self {
        Self {
            passed,
            timed_out,
            interrupted: false,
            terminated: false,
            output,
            code,
            stderr_line: String::new(),
        }
    }

    pub(in crate::harness) fn output(&self) -> &str {
        &self.output
    }

    pub fn passed(&self) -> bool {
        self.passed
    }

    pub fn terminated(&self) -> bool {
        self.terminated
    }

    pub(crate) fn code(&self) -> Option<i32> {
        self.code
    }

    pub(crate) fn interrupted(&self) -> bool {
        self.interrupted
    }
}

pub enum CheckEcho {
    Capture,
    Tee {
        file: File,
    },
    Stream {
        announcement: Option<String>,
        prefix: String,
    },
}

pub fn run_check(
    dir: &Path,
    cmd: &str,
    timeout: Duration,
    echo: CheckEcho,
    env: &BTreeMap<String, String>,
) -> Result<CheckOutcome> {
    run_command(
        dir,
        cmd,
        WatchDeadline::KillAfter(timeout),
        echo,
        env,
        |_, _| {},
        &mut None,
    )
}

pub(crate) enum WatchDeadline<'a> {
    KillAfter(Duration),
    CapacityCheck(Duration),
    /// Run to exit, with an optional one-time check-in.
    Watch(Option<Duration>),
    Hosted {
        argv: Option<&'a [std::ffi::OsString]>,
        interrupted: &'a dyn Fn() -> bool,
        spawned: &'a mut dyn FnMut(u32) -> std::io::Result<()>,
    },
}

#[cfg(not(test))]
fn check_command(command: &str) -> Command {
    let mut child = Command::new(crate::proc::rimz_exe());
    child.args(["loop", "check-exec", "--command", command]);
    child
}

/// A workflow's shell check or command step, through the same session trampoline as a loop
/// check: a command that opens `/dev/tty` must not stop on the caller's terminal job control.
/// Hosted argv comes from the validated workflow resolver.
#[cfg(not(test))]
fn hosted_command(
    dir: &Path,
    env: &BTreeMap<String, String>,
    cmd: &str,
    argv: Option<&[std::ffi::OsString]>,
) -> std::io::Result<Command> {
    // The trampoline's failed exec would read as the command's exit, so a missing program
    // fails here as a direct spawn would, against the child's PATH.
    let path = env
        .get("PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    let program = argv.map_or("sh".as_ref(), |argv| argv[0].as_os_str());
    which::which_in(program, path, dir)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let Some(argv) = argv else {
        return Ok(check_command(cmd));
    };
    let mut child = Command::new(crate::proc::rimz_exe());
    child.args(["loop", "check-exec", "--"]).args(argv);
    Ok(child)
}

#[cfg(test)]
fn hosted_command(
    _dir: &Path,
    _env: &BTreeMap<String, String>,
    cmd: &str,
    argv: Option<&[std::ffi::OsString]>,
) -> std::io::Result<Command> {
    use std::os::unix::process::CommandExt;

    let Some((program, args)) = argv.and_then(<[std::ffi::OsString]>::split_first) else {
        return Ok(check_command(cmd));
    };
    let mut child = Command::new(program);
    child.args(args).process_group(0);
    Ok(child)
}

#[cfg(test)]
fn check_command(command: &str) -> Command {
    use std::os::unix::process::CommandExt;

    // Unit tests cover the driver; CLI integration covers the session trampoline.
    let mut child = Command::new("sh");
    child.args(["-c", command]).process_group(0);
    child
}

struct CheckSignalState {
    unhandled: Option<Arc<AtomicBool>>,
    active: usize,
}

static CHECK_SIGNALS: Mutex<CheckSignalState> = Mutex::new(CheckSignalState {
    unhandled: None,
    active: 0,
});

/// Keep SIGTERM handled until an action's terminal record releases its run lock.
pub struct CheckSignalGuard {
    cancellation: Option<signal_hook::SigId>,
}

impl CheckSignalGuard {
    pub(in crate::harness) fn register<T>(
        register: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<(T, Self)> {
        let mut state = CHECK_SIGNALS
            .lock()
            .map_err(|_| std::io::Error::other("loop check signal lock poisoned"))?;
        let unhandled = match &state.unhandled {
            Some(flag) => Arc::clone(flag),
            None => {
                let flag = Arc::new(AtomicBool::new(true));
                // Unregistering signal-hook's iterator leaves SIGTERM ignored, so retain its
                // default action whenever no action or terminal record owns a guard.
                signal_hook::flag::register_conditional_default(
                    signal_hook::consts::SIGTERM,
                    Arc::clone(&flag),
                )?;
                state.unhandled = Some(Arc::clone(&flag));
                flag
            }
        };
        let handler = register()?;
        state.active += 1;
        unhandled.store(false, Ordering::SeqCst);
        Ok((handler, Self { cancellation: None }))
    }
}

impl Drop for CheckSignalGuard {
    fn drop(&mut self) {
        if let Some(handler) = self.cancellation.take() {
            signal_hook::low_level::unregister(handler);
        }
        let Ok(mut state) = CHECK_SIGNALS.lock() else {
            return;
        };
        state.active -= 1;
        if state.active == 0
            && let Some(flag) = &state.unhandled
        {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

struct CheckSignals {
    signals: signal_hook::iterator::Signals,
}

impl CheckSignals {
    fn new(signals: &[i32], record_hold: &mut Option<CheckSignalGuard>) -> std::io::Result<Self> {
        let (signals, guard) =
            CheckSignalGuard::register(|| signal_hook::iterator::Signals::new(signals))?;
        *record_hold = Some(guard);
        Ok(Self { signals })
    }

    fn pending(&mut self) -> impl Iterator<Item = i32> + '_ {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_CHECK_BEFORE_SIGNAL_READ");
        self.signals.pending()
    }
}

/// Cancel a loop-owned run on SIGTERM until the returned guard is dropped.
pub fn cancel_run_on_sigterm(cancellation: RunCancellation) -> std::io::Result<CheckSignalGuard> {
    let (handler, mut guard) = CheckSignalGuard::register(|| {
        signal_hook::flag::register(signal_hook::consts::SIGTERM, cancellation.signal_flag())
    })?;
    guard.cancellation = Some(handler);
    Ok(guard)
}

pub(crate) fn run_command(
    dir: &Path,
    cmd: &str,
    mut deadline: WatchDeadline<'_>,
    echo: CheckEcho,
    env: &BTreeMap<String, String>,
    mut check_in: impl FnMut(u64, String),
    record_hold: &mut Option<CheckSignalGuard>,
) -> Result<CheckOutcome> {
    let (prefix, file, cap) = match echo {
        CheckEcho::Capture => (None, None, CHECK_OUTPUT_CAP),
        CheckEcho::Tee { file } => (None, Some(file), WAIT_TAIL_CAP),
        CheckEcho::Stream {
            announcement,
            prefix,
        } => {
            if let Some(announcement) = announcement {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(announcement.as_bytes())?;
                out.flush()?;
            }
            (Some(prefix), None, CHECK_OUTPUT_CAP)
        }
    };
    let capture = Arc::new(Mutex::new(CheckCapture {
        file,
        tail: Vec::with_capacity(cap),
        cap,
    }));
    let captured_output = || {
        capture
            .lock()
            .map(|capture| capture.output())
            .map_err(|_| anyhow::anyhow!("loop check output lock poisoned"))
    };
    let spawn_context = || format!("running loop check `{cmd}` in {}", dir.display());
    let (mut command, mut interrupts, kill_after, check_in_after) = match &deadline {
        WatchDeadline::KillAfter(timeout) | WatchDeadline::CapacityCheck(timeout) => (
            check_command(cmd),
            Some(CheckSignals::new(
                &[signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM],
                record_hold,
            )?),
            Some(*timeout),
            None,
        ),
        WatchDeadline::Watch(check_in) => {
            let mut command = Command::new("sh");
            command.args(["-c", cmd]);
            (command, None, None, *check_in)
        }
        WatchDeadline::Hosted { argv, .. } => (
            hosted_command(dir, env, cmd, *argv).with_context(spawn_context)?,
            None,
            None,
            None,
        ),
    };
    if env.contains_key(LOOP_TASK_ENV) {
        command.env_remove(crate::workspace::ENV_CHANNEL);
        for (key, _) in std::env::vars_os()
            .filter(|(key, _)| key.as_encoded_bytes().starts_with(b"RIMZ_AGENT_"))
        {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(spawn_context)?;
    let stdout = drain_pipe(
        child.stdout.take(),
        Arc::clone(&capture),
        prefix
            .clone()
            .map(|prefix| PipeForward::new(PipeDestination::Stdout, prefix)),
    );
    let stderr = drain_pipe(
        child.stderr.take(),
        Arc::clone(&capture),
        prefix.map(|prefix| PipeForward::new(PipeDestination::Stderr, prefix)),
    );
    let started = Instant::now();
    let mut kill_at = kill_after.map(|timeout| started + timeout);
    let mut check_in_at = check_in_after.map(|timeout| started + timeout);
    let publication_error = match &mut deadline {
        WatchDeadline::Hosted { spawned, .. } => spawned(child.id()).err(),
        _ => None,
    };
    let host_interrupted =
        || matches!(&deadline, WatchDeadline::Hosted { interrupted, .. } if interrupted());
    let hosted = matches!(&deadline, WatchDeadline::Hosted { .. });
    let mut interrupted = false;
    let mut terminated = false;
    let mut forwarded = false;
    let mut pending_signals = Vec::new();
    let (status, timed_out) = loop {
        if publication_error.is_some() || host_interrupted() {
            if publication_error.is_none()
                && stdout.is_finished()
                && stderr.is_finished()
                && let Some(status) = child
                    .try_wait()
                    .with_context(|| format!("waiting for hosted command `{cmd}`"))?
            {
                break (status, false);
            }
            interrupted = true;
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping interrupted hosted check `{cmd}`"))?;
            break (status, false);
        }
        // Read stops before exit so a completed check is not judged against
        // liveness sampled before the stop read.
        if let Some(interrupts) = &mut interrupts {
            for signal in interrupts.pending() {
                interrupted = true;
                let signal = if signal == signal_hook::consts::SIGTERM {
                    terminated = true;
                    Signal::SIGTERM
                } else {
                    let interrupt_deadline = Instant::now() + CHECK_INTERRUPT_GRACE;
                    kill_at = kill_at.map(|at| at.min(interrupt_deadline));
                    Signal::SIGINT
                };
                if !pending_signals.contains(&signal) {
                    pending_signals.push(signal);
                }
            }
        }
        if !forwarded
            && (!hosted || (stdout.is_finished() && stderr.is_finished()))
            && let Some(status) = child
                .try_wait()
                .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            interrupted = false;
            terminated = false;
            break (status, false);
        }
        pending_signals.retain(
            |signal| match killpg(Pid::from_raw(child.id() as i32), *signal) {
                Ok(()) => {
                    forwarded = true;
                    false
                }
                Err(Errno::ESRCH) => true,
                Err(_) => false,
            },
        );
        // A shell can exit before its group's TERM handlers, and a hosted leader's start
        // token and PGID are its kill handle. Keep the lock, the original timeout, and the
        // leader unreaped until those members close their output pipes.
        if (!(terminated || hosted) || (stdout.is_finished() && stderr.is_finished()))
            && let Some(status) = child
                .try_wait()
                .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            break (status, false);
        }
        if kill_at.is_some_and(|at| Instant::now() >= at) {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping timed-out loop check `{cmd}`"))?;
            break (status, !interrupted);
        }
        if check_in_at.take_if(|at| Instant::now() >= *at).is_some() {
            let output = captured_output()?;
            check_in(elapsed_millis(started), output);
        }
        std::thread::sleep(CHECK_POLL_INTERVAL);
    };
    let capacity_drain = matches!(deadline, WatchDeadline::CapacityCheck(_));
    let drain_deadline = if capacity_drain && !(timed_out || interrupted) {
        kill_at
    } else {
        (timed_out || interrupted).then(|| Instant::now() + CHECK_DRAIN_GRACE)
    };
    let mut stderr_line = String::new();
    for (is_stderr, drain) in [(false, stdout), (true, stderr)] {
        while !drain.is_finished() && drain_deadline.is_some_and(|at| Instant::now() < at) {
            std::thread::sleep(CHECK_POLL_INTERVAL);
        }
        if drain_deadline.is_some() && !drain.is_finished() {
            if capacity_drain {
                let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            }
            tracing::debug!("stopped check output truncated while a survivor holds its pipe");
            continue;
        }
        let first_line = drain
            .join()
            .map_err(|_| anyhow::anyhow!("loop check output reader panicked"))??;
        if is_stderr {
            stderr_line = first_line;
        }
    }
    if let Some(error) = publication_error {
        return Err(error).context("publishing hosted check child");
    }
    Ok(CheckOutcome {
        passed: status.success() && !timed_out && !interrupted,
        timed_out,
        interrupted,
        terminated,
        output: captured_output()?,
        code: status.code(),
        stderr_line,
    })
}

#[derive(Clone, Copy)]
enum PipeDestination {
    Stdout,
    Stderr,
}

struct PipeForward {
    destination: PipeDestination,
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

impl PipeForward {
    fn new(destination: PipeDestination, prefix: String) -> Self {
        Self {
            destination,
            prefix: prefix.into_bytes(),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(line) = take_complete_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn finish(mut self) {
        if let Some(line) = take_trailing_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        let mut painted = Vec::with_capacity(self.prefix.len() + line.len());
        painted.extend_from_slice(&self.prefix);
        painted.extend_from_slice(line);
        match self.destination {
            PipeDestination::Stdout => {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(&painted)?;
                out.flush()
            }
            PipeDestination::Stderr => {
                let mut err = anstream::AutoStream::auto(std::io::stderr().lock());
                err.write_all(&painted)?;
                err.flush()
            }
        }
    }
}

fn take_complete_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = pending.iter().position(|byte| *byte == b'\n')?;
    Some(pending.drain(..=end).collect())
}

fn take_trailing_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    if pending.is_empty() {
        return None;
    }
    let mut line = std::mem::take(pending);
    line.push(b'\n');
    Some(line)
}

struct CheckCapture {
    file: Option<File>,
    tail: Vec<u8>,
    cap: usize,
}

impl CheckCapture {
    fn output(&self) -> String {
        let output = String::from_utf8_lossy(&self.tail);
        let mut start = output.len().saturating_sub(self.cap);
        while !output.is_char_boundary(start) {
            start += 1;
        }
        output[start..].to_owned()
    }

    fn push(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            file.write_all(bytes)?;
        }
        if bytes.len() >= self.cap {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - self.cap..]);
            return Ok(());
        }
        let discard = (self.tail.len() + bytes.len()).saturating_sub(self.cap);
        self.tail.drain(..discard);
        self.tail.extend_from_slice(bytes);
        Ok(())
    }
}

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    capture: Arc<Mutex<CheckCapture>>,
    mut forward: Option<PipeForward>,
) -> std::thread::JoinHandle<std::io::Result<String>> {
    std::thread::spawn(move || {
        let mut first_line = Vec::new();
        let mut complete = false;
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8 * 1024];
            loop {
                let read = pipe.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                if !complete {
                    let end = bytes.iter().position(|byte| *byte == b'\n');
                    let line = &bytes[..end.unwrap_or(bytes.len())];
                    let remaining = 1024_usize.saturating_sub(first_line.len());
                    first_line.extend_from_slice(&line[..line.len().min(remaining)]);
                    complete = end.is_some() || first_line.len() == 1024;
                }
                capture
                    .lock()
                    .map_err(|_| std::io::Error::other("loop check output lock poisoned"))?
                    .push(bytes)?;
                if let Some(forward) = &mut forward {
                    forward.push(bytes);
                }
            }
        }
        if let Some(forward) = forward {
            forward.finish();
        }
        if first_line.len() == 1024
            && let Err(error) = std::str::from_utf8(&first_line)
            && error.error_len().is_none()
        {
            first_line.truncate(error.valid_up_to());
        }
        Ok(String::from_utf8_lossy(&first_line).trim_end().to_owned())
    })
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
