//! Convergence supervisor for the pane-resident sidebar renderer.
//!
//! The worker owns the TUI and its in-process panic diagnostics. The supervisor
//! owns the pane command PID: it relaunches or re-execs the worker onto the
//! current binary, preserves the sidebar instance identity across reloads,
//! reaps stray children, and records deaths Rust hooks cannot catch.

use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::diag::record::DiagEvent;
use crate::ids::SidebarInstanceId;
use crate::sidebar_pane::app::ServeConfig;
use crate::tui::{MouseCapture, Screen, restore_terminal};
use tracing::debug;

const WORKER_ENV: &str = "RIMZ_SIDEBAR_WORKER";
const INSTANCE_ENV: &str = "RIMZ_SIDEBAR_INSTANCE_ID";
#[cfg(feature = "testkit")]
const TEST_FAULT_ENV: &str = "RIMZ_TEST_SIDEBAR_WORKER_FAULT";
#[cfg(feature = "testkit")]
const TEST_EXIT_FILE_ENV: &str = "RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE";
#[cfg(feature = "testkit")]
const TEST_REAP_POLL_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS";
#[cfg(feature = "testkit")]
const TEST_STRAY_PID_FILE_ENV: &str = "RIMZ_TEST_SIDEBAR_SUPERVISOR_STRAY_PID_FILE";
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const REAP_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const RELOAD_EXIT_CODE: i32 = 100;
const PANIC_EXIT_CODE: i32 = 101;

#[derive(Debug, thiserror::Error)]
pub enum SidebarSuperviseErr {
    #[error("spawning sidebar render worker `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("waiting for sidebar render worker: {0}")]
    Wait(#[source] io::Error),
    #[error(
        "sidebar render worker terminated abnormally (signal {signal:?}, exit code {exit_code:?})"
    )]
    WorkerTerminated {
        signal: Option<i32>,
        exit_code: Option<i32>,
    },
}

pub type Result<T> = std::result::Result<T, SidebarSuperviseErr>;

pub fn is_worker() -> bool {
    env::var_os(WORKER_ENV).is_some()
}

pub fn instance_id() -> SidebarInstanceId {
    env::var(INSTANCE_ENV)
        .ok()
        .and_then(|raw| SidebarInstanceId::parse(&raw).ok())
        .unwrap_or_default()
}

pub fn run_worker(config: ServeConfig) -> crate::sidebar_pane::app::Result<()> {
    inject_test_fault_if_requested();
    crate::sidebar_pane::app::serve(config)
}

pub fn run(config: ServeConfig) -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    loop {
        let exe = crate::proc::rimz_exe();
        let mut child = spawn_worker(&exe, &args, &config)?;
        let worker_pid = worker_pid(&child);
        spawn_test_stray_if_requested();

        let stderr_tail = Arc::new(Mutex::new(StderrTail::new(STDERR_TAIL_BYTES)));
        let stderr_handle = child
            .stderr
            .take()
            .map(|stderr| drain_stderr(stderr, stderr_tail.clone()));
        let worker = wait_for_worker_and_reap_strays(worker_pid);
        drop(child);
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }
        let worker = worker?;

        match supervise_action(worker.exit_code, worker.signal) {
            SuperviseAction::ReloadReexec => {
                let Some(target) = crate::reload::current_reexec_target() else {
                    debug!("reload: supervisor replacement binary missing; respawning worker");
                    continue;
                };
                debug!(
                    target = %target.display(),
                    instance = %config.instance_id,
                    "reload: re-execing sidebar supervisor",
                );
                return exec_supervisor(&target, &args, &config);
            }
            SuperviseAction::Panic => std::process::exit(PANIC_EXIT_CODE),
            SuperviseAction::Done => return Ok(()),
            SuperviseAction::Death => {
                let stderr_excerpt = stderr_tail
                    .lock()
                    .map(|tail| tail.excerpt())
                    .unwrap_or_default();
                restore_terminal(MouseCapture::Stdout, Screen::Main);
                record_signal_death(&config, worker.signal, worker.exit_code, stderr_excerpt);
                return Err(SidebarSuperviseErr::WorkerTerminated {
                    signal: worker.signal,
                    exit_code: worker.exit_code,
                });
            }
        }
    }
}

fn spawn_worker(exe: &Path, args: &[OsString], config: &ServeConfig) -> Result<Child> {
    Command::new(exe)
        .args(args)
        .env(WORKER_ENV, "1")
        .env(INSTANCE_ENV, config.instance_id.as_str())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| SidebarSuperviseErr::Spawn {
            program: render_program(exe),
            source,
        })
}

fn exec_supervisor(exe: &Path, args: &[OsString], config: &ServeConfig) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let source = Command::new(exe)
        .args(args)
        .env_remove(WORKER_ENV)
        .env(INSTANCE_ENV, config.instance_id.as_str())
        .exec();
    Err(SidebarSuperviseErr::Spawn {
        program: render_program(exe),
        source,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuperviseAction {
    ReloadReexec,
    Panic,
    Done,
    Death,
}

fn supervise_action(exit_code: Option<i32>, signal: Option<i32>) -> SuperviseAction {
    match (exit_code, signal) {
        (Some(RELOAD_EXIT_CODE), None) => SuperviseAction::ReloadReexec,
        (Some(PANIC_EXIT_CODE), None) => SuperviseAction::Panic,
        (Some(0), None) => SuperviseAction::Done,
        _ => SuperviseAction::Death,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerTermination {
    signal: Option<i32>,
    exit_code: Option<i32>,
}

#[cfg(unix)]
fn worker_pid(child: &Child) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(child.id() as i32)
}

#[cfg(unix)]
fn wait_for_worker_and_reap_strays(worker_pid: nix::unistd::Pid) -> Result<WorkerTermination> {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) if pid == worker_pid => {
                return Ok(WorkerTermination {
                    exit_code: Some(status),
                    signal: None,
                });
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) if pid == worker_pid => {
                return Ok(WorkerTermination {
                    exit_code: None,
                    signal: Some(signal as i32),
                });
            }
            Ok(WaitStatus::Exited(pid, status)) => {
                debug!(
                    pid = pid.as_raw(),
                    status, "reaped stray sidebar supervisor child",
                );
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) => {
                debug!(
                    pid = pid.as_raw(),
                    signal = ?signal,
                    "reaped stray sidebar supervisor child",
                );
            }
            Ok(WaitStatus::StillAlive) => thread::sleep(reap_poll_interval()),
            Ok(status) => {
                debug!(status = ?status, "observed non-terminal child status");
            }
            Err(nix::errno::Errno::ECHILD) => return Ok(WorkerTermination::default()),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(err) => return Err(SidebarSuperviseErr::Wait(wait_error(err))),
        }
    }
}

#[cfg(unix)]
fn wait_error(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

fn record_signal_death(
    config: &ServeConfig,
    signal: Option<i32>,
    exit_code: Option<i32>,
    stderr_excerpt: String,
) {
    let diag = crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    );
    diag.emit(DiagEvent::RendererSignalDeath {
        signal,
        exit_code,
        stderr_excerpt: stderr_excerpt.clone(),
    });
    report_sentry_signal_death(signal, exit_code, &stderr_excerpt);
}

#[cfg(feature = "sentry")]
fn report_sentry_signal_death(signal: Option<i32>, exit_code: Option<i32>, stderr_excerpt: &str) {
    tracing::error!(
        target: "rimz::sidebar::crash",
        {
            tags.operation = "sidebar.render_crash",
            signal = signal.unwrap_or(0),
            exit_code = exit_code.unwrap_or(0),
            stderr = %stderr_excerpt,
        },
        "sidebar render worker terminated abnormally",
    );
}

#[cfg(not(feature = "sentry"))]
fn report_sentry_signal_death(
    _signal: Option<i32>,
    _exit_code: Option<i32>,
    _stderr_excerpt: &str,
) {
}

fn drain_stderr<R>(mut stderr: R, tail: Arc<Mutex<StderrTail>>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = [0_u8; 1024];
        loop {
            let n = match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];
            let _ = io::stderr().write_all(chunk);
            if let Ok(mut tail) = tail.lock() {
                tail.push(chunk);
            }
        }
    })
}

#[derive(Debug)]
struct StderrTail {
    bytes: Vec<u8>,
    cap: usize,
}

impl StderrTail {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.cap {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len().saturating_sub(self.cap)..]);
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.cap);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn excerpt(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

fn render_program(exe: &std::path::Path) -> String {
    exe.to_string_lossy().into_owned()
}

#[cfg(feature = "testkit")]
fn reap_poll_interval() -> Duration {
    let Some(value) = env::var_os(TEST_REAP_POLL_MS_ENV).filter(|value| !value.is_empty()) else {
        return REAP_POLL_INTERVAL;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(REAP_POLL_INTERVAL)
}

#[cfg(not(feature = "testkit"))]
fn reap_poll_interval() -> Duration {
    REAP_POLL_INTERVAL
}

#[cfg(feature = "testkit")]
fn inject_test_fault_if_requested() {
    let Some(fault) = env::var_os(TEST_FAULT_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    match fault.to_string_lossy().as_ref() {
        "abort" => {
            let _ = io::stderr().write_all(b"rimz test sidebar worker abort\n");
            std::process::abort();
        }
        "exit_on_file" => exit_when_test_file_appears(),
        _ => {}
    }
}

#[cfg(feature = "testkit")]
fn exit_when_test_file_appears() {
    let Some(path) = env::var_os(TEST_EXIT_FILE_ENV).filter(|value| !value.is_empty()) else {
        let _ = io::stderr().write_all(b"rimz test sidebar worker exit file missing\n");
        std::process::exit(2);
    };
    let path = std::path::PathBuf::from(path);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if path.exists() {
            std::process::exit(0);
        }
        if std::time::Instant::now() >= deadline {
            let _ = io::stderr().write_all(b"rimz test sidebar worker exit file timed out\n");
            std::process::exit(1);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(feature = "testkit"))]
fn inject_test_fault_if_requested() {}

#[cfg(feature = "testkit")]
fn spawn_test_stray_if_requested() {
    let Some(path) = env::var_os(TEST_STRAY_PID_FILE_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let result = Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match result {
        Ok(child) => {
            let _ = std::fs::write(path, child.id().to_string());
        }
        Err(err) => {
            let _ = std::fs::write(path, format!("spawn failed: {err}"));
        }
    }
}

#[cfg(not(feature = "testkit"))]
fn spawn_test_stray_if_requested() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_bounded_suffix() {
        let mut tail = StderrTail::new(5);

        tail.push(b"abc");
        tail.push(b"def");

        assert_eq!(tail.excerpt(), "bcdef");
    }

    #[test]
    fn stderr_tail_truncates_large_chunk_to_suffix() {
        let mut tail = StderrTail::new(4);

        tail.push(b"abcdef");

        assert_eq!(tail.excerpt(), "cdef");
    }

    #[test]
    fn supervise_action_classifies_worker_exit() {
        assert_eq!(
            supervise_action(Some(RELOAD_EXIT_CODE), None),
            SuperviseAction::ReloadReexec
        );
        assert_eq!(
            supervise_action(Some(PANIC_EXIT_CODE), None),
            SuperviseAction::Panic
        );
        assert_eq!(supervise_action(Some(0), None), SuperviseAction::Done);
        assert_eq!(supervise_action(None, Some(9)), SuperviseAction::Death);
        assert_eq!(supervise_action(Some(1), None), SuperviseAction::Death);
    }
}
