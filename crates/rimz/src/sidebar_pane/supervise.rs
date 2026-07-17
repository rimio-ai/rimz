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
use std::time::{Duration, Instant};

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
#[cfg(feature = "testkit")]
const TEST_RESPAWN_BACKOFF_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_SUPERVISOR_RESPAWN_BACKOFF_MS";
#[cfg(feature = "testkit")]
const TEST_PANE_PROBE_INTERVAL_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS";
#[cfg(feature = "testkit")]
const TEST_PANE_PROBE_ENV: &str = "RIMZ_TEST_SIDEBAR_PANE_PROBE";
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const REAP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PANE_PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PANE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_GONE_STRIKES: u8 = 3;
pub const RELOAD_EXIT_CODE: i32 = 100;
const PANIC_EXIT_CODE: i32 = 101;
pub const RESPAWN_EXIT_CODE: i32 = 102;
const RESPAWN_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(60);
const RESPAWN_STABLE_RUN: Duration = Duration::from_secs(60);

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
    if let crate::reload::WorkspaceReexecTarget::Verified(target) =
        crate::reload::recorded_reexec_target(&config.workspace_id)
        && crate::build_id::current() != Some(target.build.as_str())
    {
        debug!(
            target = %target.path.display(),
            instance = %config.instance_id,
            "re-execing stale sidebar supervisor onto the recorded room build",
        );
        return exec_supervisor(&target.path, &args, &config);
    }
    let mut backoff = RESPAWN_BACKOFF_INITIAL;
    let mut pane_watchdog = PaneWatchdog::from_config(&config);
    loop {
        // Spawn from the durable room target even when its bytes match this
        // supervisor. The supervisor may still occupy an unlinked temp image;
        // `RIMZ_BIN` and `current_exe()` would make its next respawn fail.
        let current = env::current_exe().unwrap_or_else(|_| crate::proc::rimz_exe());
        let exe = worker_executable(
            crate::reload::recorded_reexec_target(&config.workspace_id),
            current,
        );
        let started = Instant::now();
        let mut child = spawn_worker(&exe, &args, &config)?;
        let worker_pid = worker_pid(&child);
        spawn_test_stray_if_requested();

        let stderr_tail = Arc::new(Mutex::new(StderrTail::new(STDERR_TAIL_BYTES)));
        let stderr_handle = child
            .stderr
            .take()
            .map(|stderr| drain_stderr(stderr, stderr_tail.clone()));
        let worker = wait_for_worker_and_reap_strays(worker_pid, &mut pane_watchdog);
        drop(child);
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }
        let worker = match worker? {
            WaitOutcome::Worker(worker) => worker,
            WaitOutcome::OrphanReaped => {
                record_orphan_reap(&config, worker_pid.as_raw());
                remove_orphan_runtime_files(&config);
                return Ok(());
            }
        };

        match supervise_action(worker.exit_code, worker.signal) {
            SuperviseAction::ReloadReexec => {
                let Some(target) = crate::reload::reexec_target_for_workspace(&config.workspace_id)
                else {
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
            SuperviseAction::Done => return Ok(()),
            SuperviseAction::Respawn => {
                let stderr_excerpt = stderr_tail
                    .lock()
                    .map(|tail| tail.excerpt())
                    .unwrap_or_default();
                restore_terminal(MouseCapture::Stdout, Screen::Main);
                record_signal_death(&config, worker.signal, worker.exit_code, stderr_excerpt);
                let (delay, next) = respawn_backoff(backoff, started.elapsed());
                debug!(
                    delay_ms = delay.as_millis(),
                    instance = %config.instance_id,
                    "respawning sidebar worker after abnormal termination",
                );
                thread::sleep(respawn_delay(delay));
                backoff = next;
            }
        }
    }
}

fn worker_executable(
    target: crate::reload::WorkspaceReexecTarget,
    current: std::path::PathBuf,
) -> std::path::PathBuf {
    match target {
        crate::reload::WorkspaceReexecTarget::Verified(target) => target.path,
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => current,
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
    Done,
    Respawn,
}

fn supervise_action(exit_code: Option<i32>, signal: Option<i32>) -> SuperviseAction {
    match (exit_code, signal) {
        (Some(RELOAD_EXIT_CODE), None) => SuperviseAction::ReloadReexec,
        (Some(0), None) => SuperviseAction::Done,
        (Some(PANIC_EXIT_CODE), None)
        | (Some(RESPAWN_EXIT_CODE), None)
        | (None, Some(_))
        | (Some(_), None) => SuperviseAction::Respawn,
        _ => SuperviseAction::Respawn,
    }
}

fn respawn_backoff(current: Duration, run_duration: Duration) -> (Duration, Duration) {
    let delay = if run_duration >= RESPAWN_STABLE_RUN {
        RESPAWN_BACKOFF_INITIAL
    } else {
        current
    };
    (delay, delay.saturating_mul(2).min(RESPAWN_BACKOFF_MAX))
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerTermination {
    signal: Option<i32>,
    exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
enum WaitOutcome {
    Worker(WorkerTermination),
    OrphanReaped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneProbe {
    Present,
    Absent,
    Unknown,
}

#[derive(Debug)]
struct PaneWatchdog {
    pane: crate::ids::PaneId,
    mux: crate::ids::MuxName,
    session_name: String,
    workspace_id: crate::ids::WorkspaceId,
    next_probe: Instant,
    strikes: u8,
}

impl PaneWatchdog {
    fn from_config(config: &ServeConfig) -> Option<Self> {
        Some(Self {
            pane: config.own_pane.clone()?,
            mux: config.mux,
            session_name: config.session_name.clone(),
            workspace_id: config.workspace_id.clone(),
            next_probe: Instant::now() + pane_probe_interval(),
            strikes: 0,
        })
    }

    fn observe(&mut self, probe: PaneProbe) -> bool {
        match probe {
            PaneProbe::Present => self.strikes = 0,
            PaneProbe::Absent => self.strikes = self.strikes.saturating_add(1),
            PaneProbe::Unknown => {}
        }
        self.strikes >= PANE_GONE_STRIKES
    }

    fn probe_if_due(&mut self, now: Instant) -> bool {
        if now < self.next_probe {
            return false;
        }
        self.next_probe = now + pane_probe_interval();
        let probe = self.probe();
        self.observe(probe)
    }

    fn probe(&self) -> PaneProbe {
        #[cfg(feature = "testkit")]
        if let Some(probe) = forced_pane_probe() {
            return probe;
        }

        let options = self.probe_options();
        match crate::mux::backend_for(self.mux).list_panes(options) {
            Ok(listing) => {
                if listing.panes.iter().any(|pane| pane.pane_id == self.pane) {
                    PaneProbe::Present
                } else {
                    PaneProbe::Absent
                }
            }
            Err(err) => {
                debug!(
                    pane = %self.pane,
                    session = %self.session_name,
                    error = %err,
                    "sidebar supervisor pane-liveness probe unavailable",
                );
                PaneProbe::Unknown
            }
        }
    }

    fn probe_options(&self) -> crate::mux::PaneListOptions {
        crate::mux::PaneListOptions {
            session_name: Some(self.session_name.clone()),
            workspace_id: Some(self.workspace_id.clone()),
            // Orphan reaping kills the render worker, so require mux truth.
            // A fresh but incomplete presence cache can omit a newly-created
            // tab and must stay a latency hint rather than destructive proof.
            authoritative: true,
            require_authoritative: true,
            command_timeout: Some(PANE_PROBE_TIMEOUT),
            ..Default::default()
        }
    }
}

#[cfg(unix)]
fn worker_pid(child: &Child) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(child.id() as i32)
}

#[cfg(unix)]
fn wait_for_worker_and_reap_strays(
    worker_pid: nix::unistd::Pid,
    watchdog: &mut Option<PaneWatchdog>,
) -> Result<WaitOutcome> {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    let mut orphan_reap_pending = false;
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) if pid == worker_pid => {
                let worker = WorkerTermination {
                    exit_code: Some(status),
                    signal: None,
                };
                return Ok(if orphan_reap_pending {
                    WaitOutcome::OrphanReaped
                } else {
                    WaitOutcome::Worker(worker)
                });
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) if pid == worker_pid => {
                let worker = WorkerTermination {
                    exit_code: None,
                    signal: Some(signal as i32),
                };
                return Ok(if orphan_reap_pending {
                    WaitOutcome::OrphanReaped
                } else {
                    WaitOutcome::Worker(worker)
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
            Ok(WaitStatus::StillAlive) => {
                if !orphan_reap_pending
                    && watchdog
                        .as_mut()
                        .is_some_and(|watchdog| watchdog.probe_if_due(Instant::now()))
                {
                    match kill(worker_pid, Signal::SIGKILL) {
                        Ok(()) | Err(nix::errno::Errno::ESRCH) => orphan_reap_pending = true,
                        Err(err) => return Err(SidebarSuperviseErr::Wait(wait_error(err))),
                    }
                }
                thread::sleep(reap_poll_interval());
            }
            Ok(status) => {
                debug!(status = ?status, "observed non-terminal child status");
            }
            Err(nix::errno::Errno::ECHILD) => {
                return Ok(if orphan_reap_pending {
                    WaitOutcome::OrphanReaped
                } else {
                    WaitOutcome::Worker(WorkerTermination::default())
                });
            }
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

fn record_orphan_reap(config: &ServeConfig, worker_pid: i32) {
    crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    )
    .emit(DiagEvent::RendererOrphanReaped {
        pane_id: config
            .own_pane
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        worker_pid,
    });
}

fn remove_orphan_runtime_files(config: &ServeConfig) {
    let runtime = match crate::RuntimePaths::for_workspace(config.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            debug!(error = %err, "sidebar supervisor orphan runtime cleanup unavailable");
            return;
        }
    };
    for path in [
        runtime.sidebar_heartbeat_path(&config.instance_id),
        crate::sidebar_pane::app::socket::sidebar_socket_path(&runtime, &config.instance_id),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar supervisor orphan runtime cleanup failed");
            }
        }
    }
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

#[cfg(feature = "testkit")]
fn respawn_delay(delay: Duration) -> Duration {
    env::var(TEST_RESPAWN_BACKOFF_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(delay)
}

#[cfg(not(feature = "testkit"))]
fn respawn_delay(delay: Duration) -> Duration {
    delay
}

#[cfg(feature = "testkit")]
fn pane_probe_interval() -> Duration {
    let Some(value) =
        env::var_os(TEST_PANE_PROBE_INTERVAL_MS_ENV).filter(|value| !value.is_empty())
    else {
        return PANE_PROBE_INTERVAL;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(PANE_PROBE_INTERVAL)
}

#[cfg(not(feature = "testkit"))]
fn pane_probe_interval() -> Duration {
    PANE_PROBE_INTERVAL
}

#[cfg(feature = "testkit")]
fn forced_pane_probe() -> Option<PaneProbe> {
    match env::var(TEST_PANE_PROBE_ENV).ok().as_deref() {
        Some("present") => Some(PaneProbe::Present),
        Some("absent") => Some(PaneProbe::Absent),
        Some("unknown") => Some(PaneProbe::Unknown),
        _ => None,
    }
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
        "abort_after_delay" => {
            thread::sleep(Duration::from_millis(20));
            let _ = io::stderr().write_all(b"rimz test sidebar worker delayed abort\n");
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
            SuperviseAction::Respawn
        );
        assert_eq!(
            supervise_action(Some(RESPAWN_EXIT_CODE), None),
            SuperviseAction::Respawn
        );
        assert_eq!(supervise_action(Some(0), None), SuperviseAction::Done);
        assert_eq!(supervise_action(None, Some(9)), SuperviseAction::Respawn);
        assert_eq!(supervise_action(Some(1), None), SuperviseAction::Respawn);
    }

    #[test]
    fn respawn_backoff_doubles_caps_and_resets_after_a_stable_run() {
        assert_eq!(
            respawn_backoff(Duration::from_secs(1), Duration::from_secs(2)),
            (Duration::from_secs(1), Duration::from_secs(2))
        );
        assert_eq!(
            respawn_backoff(Duration::from_secs(60), Duration::from_secs(2)),
            (Duration::from_secs(60), Duration::from_secs(60))
        );
        assert_eq!(
            respawn_backoff(Duration::from_secs(32), RESPAWN_STABLE_RUN),
            (Duration::from_secs(1), Duration::from_secs(2))
        );
    }

    #[test]
    fn pane_watchdog_requires_three_fresh_absences() {
        let mut watchdog = PaneWatchdog {
            pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
            mux: crate::ids::MuxName::Tmux,
            session_name: "rimz-test".to_owned(),
            workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/repo")),
            next_probe: Instant::now(),
            strikes: 0,
        };

        assert!(!watchdog.observe(PaneProbe::Absent));
        assert!(!watchdog.observe(PaneProbe::Unknown));
        assert_eq!(watchdog.strikes, 1);
        assert!(!watchdog.observe(PaneProbe::Present));
        assert_eq!(watchdog.strikes, 0);
        assert!(!watchdog.observe(PaneProbe::Absent));
        assert!(!watchdog.observe(PaneProbe::Absent));
        assert!(watchdog.observe(PaneProbe::Absent));
    }

    #[test]
    fn pane_watchdog_requires_authoritative_mux_truth() {
        let watchdog = PaneWatchdog {
            pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9"),
            mux: crate::ids::MuxName::Zellij,
            session_name: "rimz-test".to_owned(),
            workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/repo")),
            next_probe: Instant::now(),
            strikes: 0,
        };

        let options = watchdog.probe_options();
        assert!(options.authoritative);
        assert!(options.require_authoritative);
        assert_eq!(options.command_timeout, Some(PANE_PROBE_TIMEOUT));
    }

    #[test]
    fn worker_spawn_prefers_the_durable_target_even_for_matching_bytes() {
        let durable = std::path::PathBuf::from("/state/rimz/builds/same/rimz");
        let ephemeral = std::path::PathBuf::from("/tmp/build/rimz (deleted)");
        assert_eq!(
            worker_executable(
                crate::reload::WorkspaceReexecTarget::Verified(crate::reload::StagedBuild {
                    path: durable.clone(),
                    build: "same".to_owned(),
                }),
                ephemeral,
            ),
            durable,
        );
    }
}
