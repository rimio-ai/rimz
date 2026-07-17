//! Convergence supervisor for the pane-resident sidebar renderer.
//!
//! The worker owns the TUI and its in-process panic diagnostics. The supervisor
//! owns the pane command PID, polls durable build intent, proves a replacement
//! worker stable before preflight and self-exec, and preserves the sidebar
//! instance across failures and reloads. It also confirms worker self-close
//! requests against authoritative mux truth, reaps stray children, and records
//! deaths Rust hooks cannot catch.

use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

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
#[cfg(feature = "testkit")]
const TEST_RECORD_POLL_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_RECORD_POLL_MS";
#[cfg(feature = "testkit")]
const TEST_STABLE_RUN_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_STABLE_RUN_MS";
#[cfg(feature = "testkit")]
const TEST_HANDOFF_GRACE_MS_ENV: &str = "RIMZ_TEST_SIDEBAR_HANDOFF_GRACE_MS";
#[cfg(feature = "testkit")]
const TEST_WORKER_STARTED_FILE_ENV: &str = "RIMZ_TEST_SIDEBAR_WORKER_STARTED_FILE";
#[cfg(feature = "testkit")]
const TEST_SELF_CLOSE_PROBE_ENV: &str = "RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE";
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const REAP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PANE_PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PANE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_GONE_STRIKES: u8 = 3;
pub const RELOAD_EXIT_CODE: i32 = 100;
const PANIC_EXIT_CODE: i32 = 101;
pub const RESPAWN_EXIT_CODE: i32 = 102;
pub const SELF_CLOSE_EXIT_CODE: i32 = 103;
const RESPAWN_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(60);
const RESPAWN_STABLE_RUN: Duration = Duration::from_secs(60);
const RECORD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_HANDOFF_GRACE: Duration = Duration::from_secs(10);
const SUPERVISOR_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

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

pub fn run_worker(
    config: ServeConfig,
) -> crate::sidebar_pane::app::Result<crate::sidebar_pane::app::ServeOutcome> {
    inject_test_fault_if_requested();
    crate::sidebar_pane::app::serve(config)
}

pub fn run(config: ServeConfig) -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut backoff = RESPAWN_BACKOFF_INITIAL;
    let mut pane_watchdog = PaneWatchdog::from_config(&config);
    let mut record_watch = RecordWatch::new(&config.workspace_id);
    let supervisor_build = crate::build_id::current().map(str::to_owned);
    let runtime = crate::RuntimePaths::for_workspace(config.workspace_id.clone()).ok();
    let mut exec_state = PendingExec::default();
    loop {
        // Spawn from the durable room target even when its bytes match this
        // supervisor. The supervisor may still occupy an unlinked temp image;
        // `RIMZ_BIN` and `current_exe()` would make its next respawn fail.
        let target = crate::reload::recorded_reexec_target(&config.workspace_id);
        exec_state.observe(&target, supervisor_build.as_deref());
        // Atomic installs leave `current_exe()` spelling a deleted inode on
        // Linux. Resolve its replacement before spawning so a legacy room
        // without a verified staged target can still complete the handoff.
        let current = crate::reload::current_reexec_target().unwrap_or_else(crate::proc::rimz_exe);
        let worker_build = worker_build(&target, supervisor_build.as_deref());
        let exe = worker_executable(target, current);
        let started = Instant::now();
        let mut child = spawn_worker(&exe, &args, &config)?;
        let worker_pid = worker_pid(&child);
        record_test_worker_start(&exe, child.id());
        spawn_test_stray_if_requested();

        let stderr_tail = Arc::new(Mutex::new(StderrTail::new(STDERR_TAIL_BYTES)));
        let stderr_handle = child
            .stderr
            .take()
            .map(|stderr| drain_stderr(stderr, stderr_tail.clone()));
        let worker = wait_for_worker_and_reap_strays(
            worker_pid,
            &mut pane_watchdog,
            WorkerConvergence {
                record_watch: &mut record_watch,
                exec_state: &mut exec_state,
                worker_build: worker_build.as_deref(),
                supervisor_build: supervisor_build.as_deref(),
                started,
                runtime: runtime.as_ref(),
                config: &config,
            },
        );
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

        let action = if worker.handoff {
            SuperviseAction::ReloadReexec
        } else {
            supervise_action(worker.termination.exit_code, worker.termination.signal)
        };
        match action {
            SuperviseAction::ReloadReexec => {
                if let Some(target) = exec_state.promotable(
                    worker_build.as_deref(),
                    started.elapsed(),
                    respawn_stable_run(),
                ) {
                    match preflight_supervisor(&target.path) {
                        Ok(()) => {
                            debug!(
                                target = %target.path.display(),
                                instance = %config.instance_id,
                                "reload: re-execing proven sidebar supervisor",
                            );
                            return exec_supervisor(&target.path, &args, &config);
                        }
                        Err(reason) => {
                            record_preflight_rejected(&config, &target.build, &reason);
                            exec_state.reject(&target.build);
                        }
                    }
                }
            }
            SuperviseAction::ConfirmSelfClose => {
                match confirm_self_close(&config, &pane_watchdog) {
                    SelfCloseConfirmation::Close | SelfCloseConfirmation::PaneGone => {
                        restore_terminal(MouseCapture::Stdout, Screen::Main);
                        remove_orphan_runtime_files(&config);
                        record_confirmed_self_close(&config);
                        return Ok(());
                    }
                    SelfCloseConfirmation::Keep { siblings, reason } => {
                        record_self_close_rejected(&config, siblings, &reason);
                        sleep_respawn_backoff(
                            respawn_delay(RESPAWN_BACKOFF_INITIAL),
                            &mut record_watch,
                            &mut exec_state,
                            supervisor_build.as_deref(),
                        );
                    }
                }
            }
            SuperviseAction::Respawn => {
                let stderr_excerpt = stderr_tail
                    .lock()
                    .map(|tail| tail.excerpt())
                    .unwrap_or_default();
                restore_terminal(MouseCapture::Stdout, Screen::Main);
                record_signal_death(
                    &config,
                    worker.termination.signal,
                    worker.termination.exit_code,
                    stderr_excerpt,
                );
                let (delay, next) = respawn_backoff(backoff, started.elapsed());
                debug!(
                    delay_ms = delay.as_millis(),
                    instance = %config.instance_id,
                    "respawning sidebar worker after abnormal termination",
                );
                sleep_respawn_backoff(
                    respawn_delay(delay),
                    &mut record_watch,
                    &mut exec_state,
                    supervisor_build.as_deref(),
                );
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
    ConfirmSelfClose,
    Respawn,
}

fn supervise_action(exit_code: Option<i32>, signal: Option<i32>) -> SuperviseAction {
    match (exit_code, signal) {
        (Some(RELOAD_EXIT_CODE), None) => SuperviseAction::ReloadReexec,
        (Some(SELF_CLOSE_EXIT_CODE), None) => SuperviseAction::ConfirmSelfClose,
        (Some(PANIC_EXIT_CODE), None)
        | (Some(RESPAWN_EXIT_CODE), None)
        | (Some(0), None)
        | (None, Some(_))
        | (Some(_), None) => SuperviseAction::Respawn,
        _ => SuperviseAction::Respawn,
    }
}

fn respawn_backoff(current: Duration, run_duration: Duration) -> (Duration, Duration) {
    let delay = if run_duration >= respawn_stable_run() {
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

#[derive(Clone, Copy, Debug, Default)]
struct WorkerWait {
    termination: WorkerTermination,
    handoff: bool,
}

#[derive(Clone, Copy, Debug)]
enum WaitOutcome {
    Worker(WorkerWait),
    OrphanReaped,
}

#[derive(Clone, Debug, Default)]
struct PendingExec {
    target: Option<crate::reload::StagedBuild>,
    rejected_build: Option<String>,
}

impl PendingExec {
    fn observe(
        &mut self,
        target: &crate::reload::WorkspaceReexecTarget,
        supervisor_build: Option<&str>,
    ) {
        let crate::reload::WorkspaceReexecTarget::Verified(target) = target else {
            self.target = None;
            self.rejected_build = None;
            return;
        };
        if supervisor_build == Some(target.build.as_str()) {
            self.target = None;
            self.rejected_build = None;
        } else if self.rejected_build.as_deref() != Some(target.build.as_str()) {
            self.rejected_build = None;
            self.target = Some(target.clone());
        }
    }

    fn promotable(
        &self,
        worker_build: Option<&str>,
        run_duration: Duration,
        stable_run: Duration,
    ) -> Option<crate::reload::StagedBuild> {
        self.target
            .as_ref()
            .filter(|target| worker_build == Some(target.build.as_str()))
            .filter(|_| run_duration >= stable_run)
            .cloned()
    }

    fn reject(&mut self, build: &str) {
        self.target = None;
        self.rejected_build = Some(build.to_owned());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordChange {
    Verified(crate::reload::StagedBuild),
    Unavailable,
}

#[derive(Debug)]
struct RecordWatch {
    workspace_id: crate::ids::WorkspaceId,
    record_path: PathBuf,
    last_seen_mtime: Option<SystemTime>,
    next_poll: Instant,
}

impl RecordWatch {
    fn new(workspace_id: &crate::ids::WorkspaceId) -> Self {
        let record_path = crate::StatePaths::for_workspace(workspace_id.clone())
            .map(|paths| paths.workspace_record)
            .unwrap_or_default();
        let last_seen_mtime = record_mtime(&record_path);
        Self {
            workspace_id: workspace_id.clone(),
            record_path,
            last_seen_mtime,
            next_poll: Instant::now() + record_poll_interval(),
        }
    }

    fn poll_if_due(&mut self, now: Instant) -> Option<RecordChange> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + record_poll_interval();
        self.poll_now()
    }

    fn poll_now(&mut self) -> Option<RecordChange> {
        let mtime = record_mtime(&self.record_path);
        if mtime == self.last_seen_mtime {
            return None;
        }
        let target = crate::reload::recorded_reexec_target(&self.workspace_id);
        let change = record_change(self.last_seen_mtime, mtime, target);
        self.last_seen_mtime = mtime;
        change
    }
}

fn record_change(
    prior_mtime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    target: crate::reload::WorkspaceReexecTarget,
) -> Option<RecordChange> {
    if mtime == prior_mtime {
        return None;
    }
    Some(match target {
        crate::reload::WorkspaceReexecTarget::Verified(target) => RecordChange::Verified(target),
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => RecordChange::Unavailable,
    })
}

fn record_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn worker_build(
    target: &crate::reload::WorkspaceReexecTarget,
    supervisor_build: Option<&str>,
) -> Option<String> {
    match target {
        crate::reload::WorkspaceReexecTarget::Verified(target) => Some(target.build.clone()),
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => supervisor_build.map(str::to_owned),
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelfCloseVerdict {
    PaneGone,
    Empty { floating_siblings: usize },
    Keep { siblings: usize, reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelfCloseConfirmation {
    PaneGone,
    Close,
    Keep { siblings: usize, reason: String },
}

fn self_close_verdict(
    panes: &[crate::pane::PaneRef],
    own_pane: &crate::ids::PaneId,
) -> SelfCloseVerdict {
    let Some(own) = panes.iter().find(|pane| &pane.pane_id == own_pane) else {
        return SelfCloseVerdict::PaneGone;
    };
    let Some(view_id) = own.view_id.as_deref() else {
        return SelfCloseVerdict::Keep {
            siblings: 0,
            reason: "own view id is unavailable".to_owned(),
        };
    };
    let siblings = panes
        .iter()
        .filter(|pane| pane.pane_id != *own_pane && pane.view_id.as_deref() == Some(view_id))
        .collect::<Vec<_>>();
    let working_siblings = siblings.iter().filter(|pane| !pane.is_floating).count();
    if working_siblings > 0 {
        return SelfCloseVerdict::Keep {
            siblings: siblings.len(),
            reason: "authoritative listing still has working siblings".to_owned(),
        };
    }
    SelfCloseVerdict::Empty {
        floating_siblings: siblings.len(),
    }
}

fn confirm_self_close(
    config: &ServeConfig,
    watchdog: &Option<PaneWatchdog>,
) -> SelfCloseConfirmation {
    #[cfg(feature = "testkit")]
    if let Some(confirmation) = forced_self_close_confirmation() {
        return confirmation;
    }

    let Some(watchdog) = watchdog.as_ref() else {
        return SelfCloseConfirmation::Keep {
            siblings: 0,
            reason: "own pane is unavailable".to_owned(),
        };
    };
    let listing = match crate::mux::backend_for(watchdog.mux).list_panes(watchdog.probe_options()) {
        Ok(listing) => listing,
        Err(err) => {
            return SelfCloseConfirmation::Keep {
                siblings: 0,
                reason: format!("authoritative pane probe failed: {err}"),
            };
        }
    };
    match self_close_verdict(&listing.panes, &watchdog.pane) {
        SelfCloseVerdict::PaneGone => SelfCloseConfirmation::PaneGone,
        SelfCloseVerdict::Keep { siblings, reason } => {
            SelfCloseConfirmation::Keep { siblings, reason }
        }
        SelfCloseVerdict::Empty { floating_siblings } => {
            if floating_siblings == 0 {
                return SelfCloseConfirmation::Close;
            }
            match crate::mux::backend_for(config.mux)
                .close_view_floating_panes(&config.session_name, &watchdog.pane)
            {
                Ok(_) => SelfCloseConfirmation::Close,
                Err(err) => SelfCloseConfirmation::Keep {
                    siblings: floating_siblings,
                    reason: format!("floating-pane cleanup failed: {err}"),
                },
            }
        }
    }
}

#[cfg(feature = "testkit")]
fn forced_self_close_confirmation() -> Option<SelfCloseConfirmation> {
    match env::var(TEST_SELF_CLOSE_PROBE_ENV).ok().as_deref() {
        Some("empty") => Some(SelfCloseConfirmation::Close),
        Some("absent") => Some(SelfCloseConfirmation::PaneGone),
        Some("siblings") => Some(SelfCloseConfirmation::Keep {
            siblings: 1,
            reason: "forced siblings-present probe".to_owned(),
        }),
        Some("error") => Some(SelfCloseConfirmation::Keep {
            siblings: 0,
            reason: "forced authoritative probe failure".to_owned(),
        }),
        _ => None,
    }
}

#[cfg(unix)]
fn worker_pid(child: &Child) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(child.id() as i32)
}

#[cfg(unix)]
struct WorkerConvergence<'a> {
    record_watch: &'a mut RecordWatch,
    exec_state: &'a mut PendingExec,
    worker_build: Option<&'a str>,
    supervisor_build: Option<&'a str>,
    started: Instant,
    runtime: Option<&'a crate::RuntimePaths>,
    config: &'a ServeConfig,
}

#[cfg(unix)]
fn wait_for_worker_and_reap_strays(
    worker_pid: nix::unistd::Pid,
    watchdog: &mut Option<PaneWatchdog>,
    convergence: WorkerConvergence<'_>,
) -> Result<WaitOutcome> {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    let mut orphan_reap_pending = false;
    let mut handoff_deadline = None;
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
                    WaitOutcome::Worker(WorkerWait {
                        termination: worker,
                        handoff: handoff_deadline.is_some(),
                    })
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
                    WaitOutcome::Worker(WorkerWait {
                        termination: worker,
                        handoff: handoff_deadline.is_some(),
                    })
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
                let now = Instant::now();
                if let Some(change) = convergence.record_watch.poll_if_due(now) {
                    let worker_is_stale = apply_record_change(
                        convergence.exec_state,
                        &change,
                        convergence.supervisor_build,
                        convergence.worker_build,
                    );
                    if worker_is_stale && handoff_deadline.is_none() {
                        request_worker_handoff(
                            convergence.runtime,
                            convergence.config,
                            convergence.exec_state.target.as_ref(),
                        );
                        handoff_deadline = Some(now + worker_handoff_grace());
                    }
                }
                if handoff_deadline.is_none()
                    && convergence
                        .exec_state
                        .promotable(
                            convergence.worker_build,
                            now.saturating_duration_since(convergence.started),
                            respawn_stable_run(),
                        )
                        .is_some()
                {
                    request_worker_handoff(
                        convergence.runtime,
                        convergence.config,
                        convergence.exec_state.target.as_ref(),
                    );
                    handoff_deadline = Some(now + worker_handoff_grace());
                }
                if handoff_deadline.is_some_and(|deadline| now >= deadline) {
                    match kill(worker_pid, Signal::SIGKILL) {
                        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                        Err(err) => return Err(SidebarSuperviseErr::Wait(wait_error(err))),
                    }
                }
                if !orphan_reap_pending
                    && watchdog
                        .as_mut()
                        .is_some_and(|watchdog| watchdog.probe_if_due(now))
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
                    WaitOutcome::Worker(WorkerWait {
                        termination: WorkerTermination::default(),
                        handoff: handoff_deadline.is_some(),
                    })
                });
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(err) => return Err(SidebarSuperviseErr::Wait(wait_error(err))),
        }
    }
}

fn apply_record_change(
    exec_state: &mut PendingExec,
    change: &RecordChange,
    supervisor_build: Option<&str>,
    worker_build: Option<&str>,
) -> bool {
    let target = match change {
        RecordChange::Verified(target) => {
            crate::reload::WorkspaceReexecTarget::Verified(target.clone())
        }
        RecordChange::Unavailable => crate::reload::WorkspaceReexecTarget::Invalid,
    };
    exec_state.observe(&target, supervisor_build);
    matches!(change, RecordChange::Verified(target) if worker_build != Some(target.build.as_str()))
}

fn request_worker_handoff(
    runtime: Option<&crate::RuntimePaths>,
    config: &ServeConfig,
    target: Option<&crate::reload::StagedBuild>,
) {
    if let Some(runtime) = runtime
        && let Err(err) = crate::store::wakeup::reload_sidebar(runtime, &config.instance_id)
    {
        debug!(error = %err, "sidebar supervisor worker handoff nudge failed");
    }
    if let Some(target) = target {
        crate::diag::DiagSink::for_workspace(
            config.workspace_id.clone(),
            config.session_name.clone(),
            Some(config.instance_id.clone()),
        )
        .emit(DiagEvent::SupervisorConvergence {
            target_build: target.build.clone(),
        });
    }
}

fn sleep_respawn_backoff(
    delay: Duration,
    record_watch: &mut RecordWatch,
    exec_state: &mut PendingExec,
    supervisor_build: Option<&str>,
) {
    let deadline = Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(record_poll_interval()));
        if let Some(change) = record_watch.poll_now() {
            let _ = apply_record_change(exec_state, &change, supervisor_build, None);
            return;
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

fn preflight_supervisor(exe: &Path) -> std::result::Result<(), String> {
    let output = crate::mux::CommandSpec::new(exe.to_string_lossy())
        .arg("--version")
        .output_raw_with_timeout(SUPERVISOR_PREFLIGHT_TIMEOUT)
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("`--version` exited with {}", output.status))
    }
}

fn record_preflight_rejected(config: &ServeConfig, target_build: &str, reason: &str) {
    crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    )
    .emit(DiagEvent::SupervisorPreflightRejected {
        target_build: target_build.to_owned(),
        reason: reason.to_owned(),
    });
}

fn record_self_close_rejected(config: &ServeConfig, siblings: usize, reason: &str) {
    crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    )
    .emit(DiagEvent::SelfCloseRejected {
        siblings,
        reason: reason.to_owned(),
    });
}

fn record_confirmed_self_close(config: &ServeConfig) {
    crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    )
    .emit_unlimited(DiagEvent::RendererExit {
        cause: crate::diag::record::RendererExitCause::SelfCloseEmptyTab,
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

#[cfg(feature = "testkit")]
fn record_poll_interval() -> Duration {
    env::var(TEST_RECORD_POLL_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(RECORD_POLL_INTERVAL)
}

#[cfg(not(feature = "testkit"))]
fn record_poll_interval() -> Duration {
    RECORD_POLL_INTERVAL
}

#[cfg(feature = "testkit")]
fn respawn_stable_run() -> Duration {
    env::var(TEST_STABLE_RUN_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(RESPAWN_STABLE_RUN)
}

#[cfg(not(feature = "testkit"))]
fn respawn_stable_run() -> Duration {
    RESPAWN_STABLE_RUN
}

#[cfg(feature = "testkit")]
fn worker_handoff_grace() -> Duration {
    env::var(TEST_HANDOFF_GRACE_MS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(WORKER_HANDOFF_GRACE)
}

#[cfg(not(feature = "testkit"))]
fn worker_handoff_grace() -> Duration {
    WORKER_HANDOFF_GRACE
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
        "self_close" => std::process::exit(SELF_CLOSE_EXIT_CODE),
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

#[cfg(feature = "testkit")]
fn record_test_worker_start(exe: &Path, pid: u32) {
    use std::fs::OpenOptions;

    let Some(path) = env::var_os(TEST_WORKER_STARTED_FILE_ENV).filter(|value| !value.is_empty())
    else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{pid} {}", exe.display());
}

#[cfg(not(feature = "testkit"))]
fn record_test_worker_start(_exe: &Path, _pid: u32) {}

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
        assert_eq!(
            supervise_action(Some(SELF_CLOSE_EXIT_CODE), None),
            SuperviseAction::ConfirmSelfClose
        );
        assert_eq!(supervise_action(Some(0), None), SuperviseAction::Respawn);
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
    fn self_close_verdict_requires_authoritative_view_emptiness() {
        let own_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
        let sibling_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%2");
        let pane = |pane_id: crate::ids::PaneId, view_id: Option<&str>, is_floating: bool| {
            crate::pane::PaneRef {
                pane_id,
                session_name: "rimz-test".to_owned(),
                view_id: view_id.map(str::to_owned),
                is_floating,
                ..crate::pane::PaneRef::from_id(crate::ids::PaneId::from_parts(
                    crate::ids::MuxName::Tmux,
                    "%unused",
                ))
            }
        };

        assert_eq!(self_close_verdict(&[], &own_id), SelfCloseVerdict::PaneGone);
        assert!(matches!(
            self_close_verdict(&[pane(own_id.clone(), None, false)], &own_id),
            SelfCloseVerdict::Keep { .. }
        ));
        assert_eq!(
            self_close_verdict(&[pane(own_id.clone(), Some("@1"), false)], &own_id),
            SelfCloseVerdict::Empty {
                floating_siblings: 0
            }
        );
        assert!(matches!(
            self_close_verdict(
                &[
                    pane(own_id.clone(), Some("@1"), false),
                    pane(sibling_id.clone(), Some("@1"), false),
                ],
                &own_id,
            ),
            SelfCloseVerdict::Keep { siblings: 1, .. }
        ));
        assert_eq!(
            self_close_verdict(
                &[
                    pane(own_id.clone(), Some("@1"), false),
                    pane(sibling_id, Some("@1"), true),
                ],
                &own_id,
            ),
            SelfCloseVerdict::Empty {
                floating_siblings: 1
            }
        );
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

    #[test]
    fn record_change_requires_a_new_mtime_and_preserves_verification() {
        let prior = SystemTime::UNIX_EPOCH;
        let next = prior + Duration::from_secs(1);
        let target = crate::reload::StagedBuild {
            path: PathBuf::from("/state/builds/next/rimz"),
            build: "next".to_owned(),
        };
        assert_eq!(
            record_change(
                Some(prior),
                Some(prior),
                crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
            ),
            None,
        );
        assert_eq!(
            record_change(
                Some(prior),
                Some(next),
                crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
            ),
            Some(RecordChange::Verified(target)),
        );
        assert_eq!(
            record_change(
                Some(prior),
                Some(next),
                crate::reload::WorkspaceReexecTarget::Invalid,
            ),
            Some(RecordChange::Unavailable),
        );
    }

    #[test]
    fn pending_exec_waits_for_the_replacement_worker_stability_window() {
        let target = crate::reload::StagedBuild {
            path: PathBuf::from("/state/builds/new/rimz"),
            build: "new".to_owned(),
        };
        let mut pending = PendingExec::default();
        pending.observe(
            &crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
            Some("old"),
        );

        assert!(
            pending
                .promotable(Some("old"), RESPAWN_STABLE_RUN, RESPAWN_STABLE_RUN)
                .is_none(),
            "the old worker cannot promote the new supervisor",
        );
        assert!(
            pending
                .promotable(
                    Some("new"),
                    RESPAWN_STABLE_RUN - Duration::from_millis(1),
                    RESPAWN_STABLE_RUN,
                )
                .is_none(),
            "the new worker must first serve stably",
        );
        assert_eq!(
            pending.promotable(Some("new"), RESPAWN_STABLE_RUN, RESPAWN_STABLE_RUN),
            Some(target),
        );
    }

    #[test]
    fn pending_exec_resets_when_the_record_changes() {
        let target = |build: &str| {
            crate::reload::WorkspaceReexecTarget::Verified(crate::reload::StagedBuild {
                path: PathBuf::from(format!("/state/builds/{build}/rimz")),
                build: build.to_owned(),
            })
        };
        let mut pending = PendingExec::default();
        pending.observe(&target("first"), Some("old"));
        pending.reject("first");
        pending.observe(&target("first"), Some("old"));
        assert!(
            pending.target.is_none(),
            "a rejected build waits for a new record"
        );

        pending.observe(&target("second"), Some("old"));
        assert_eq!(
            pending.target.as_ref().map(|target| target.build.as_str()),
            Some("second")
        );
        assert_eq!(pending.rejected_build, None);
    }
}
