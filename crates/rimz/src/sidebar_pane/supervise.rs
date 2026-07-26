//! Convergence supervisor for the pane-resident sidebar renderer.
//!
//! The worker owns the TUI and its in-process panic diagnostics. The supervisor
//! owns the pane command PID, polls durable build intent, proves a replacement
//! worker stable before preflight and self-exec, and preserves the sidebar
//! instance across failures and reloads. It also confirms worker self-close
//! requests against authoritative mux truth, proves routine pane liveness from
//! cached presence before escalating to mux truth, reaps stray children, and
//! records deaths Rust hooks cannot catch.

use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

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
const TEST_PANE_PROBE_ABSENT_FILE_ENV: &str = "RIMZ_TEST_SIDEBAR_PANE_PROBE_ABSENT_FILE";
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
const PANE_PROBE_WAIT_STEP: Duration = Duration::from_millis(25);
const PANE_PROBE_WAIT_STEPS: u32 = 20;
const PANE_GONE_STRIKES: u8 = 3;
const SELF_CLOSE_RECONFIRM_DELAY: Duration = Duration::from_millis(500);
pub const RELOAD_EXIT_CODE: i32 = 100;
#[cfg(test)]
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
            WorkerMonitor {
                record_watch: &mut record_watch,
                exec_state: &mut exec_state,
                worker_build: worker_build.as_deref(),
                supervisor_build: supervisor_build.as_deref(),
                started,
                runtime: runtime.as_ref(),
                config: &config,
                watchdog: &mut pane_watchdog,
                orphan_reap_pending: false,
                handoff_deadline: None,
            },
        );
        drop(child);
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }
        let worker = worker?;
        match worker {
            WorkerExit::OrphanReaped => {
                record_orphan_reap(&config, worker_pid.as_raw());
                remove_orphan_runtime_files(&config);
                return Ok(());
            }
            WorkerExit::Reload => {
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
            WorkerExit::ConfirmSelfClose => match confirm_self_close(&config, &pane_watchdog) {
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
            },
            WorkerExit::Respawn { signal, exit_code } => {
                let stderr_excerpt = stderr_tail
                    .lock()
                    .map(|tail| tail.excerpt())
                    .unwrap_or_default();
                restore_terminal(MouseCapture::Stdout, Screen::Main);
                record_signal_death(&config, signal, exit_code, stderr_excerpt);
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
enum WorkerExit {
    Reload,
    ConfirmSelfClose,
    Respawn {
        signal: Option<i32>,
        exit_code: Option<i32>,
    },
    OrphanReaped,
}

fn classify_worker_exit(
    orphan_reap_pending: bool,
    handoff_requested: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
) -> WorkerExit {
    if orphan_reap_pending {
        return WorkerExit::OrphanReaped;
    }
    if handoff_requested || exit_code == Some(RELOAD_EXIT_CODE) && signal.is_none() {
        return WorkerExit::Reload;
    }
    if exit_code == Some(SELF_CLOSE_EXIT_CODE) && signal.is_none() {
        return WorkerExit::ConfirmSelfClose;
    }
    WorkerExit::Respawn { signal, exit_code }
}

fn respawn_backoff(current: Duration, run_duration: Duration) -> (Duration, Duration) {
    let delay = if run_duration >= respawn_stable_run() {
        RESPAWN_BACKOFF_INITIAL
    } else {
        current
    };
    (delay, delay.saturating_mul(2).min(RESPAWN_BACKOFF_MAX))
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
    Present(u64),
    Absent(u64),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AuthoritativePaneProbe {
    mux: crate::ids::MuxName,
    session_name: String,
    observed_at_ms: u64,
    pane_ids: Vec<crate::ids::PaneId>,
}

#[derive(Debug)]
struct PaneWatchdog {
    pane: crate::ids::PaneId,
    mux: crate::ids::MuxName,
    session_name: String,
    workspace_id: crate::ids::WorkspaceId,
    next_probe: Instant,
    strikes: u8,
    last_observed_at_ms: Option<u64>,
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
            last_observed_at_ms: None,
        })
    }

    fn observe(&mut self, probe: PaneProbe) -> bool {
        match probe {
            PaneProbe::Present(observed_at_ms) => {
                if self.last_observed_at_ms != Some(observed_at_ms) {
                    self.strikes = 0;
                    self.last_observed_at_ms = Some(observed_at_ms);
                }
            }
            PaneProbe::Absent(observed_at_ms) => {
                if self.last_observed_at_ms != Some(observed_at_ms) {
                    self.strikes = self.strikes.saturating_add(1);
                    self.last_observed_at_ms = Some(observed_at_ms);
                }
            }
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
        let Ok(runtime) = crate::RuntimePaths::for_workspace(self.workspace_id.clone()) else {
            return PaneProbe::Unknown;
        };
        #[cfg(feature = "testkit")]
        let roster = forced_pane_probe()
            .is_none()
            .then(|| {
                crate::mux::backend_for(self.mux)
                    .cached_pane_roster(&self.session_name, &self.workspace_id)
            })
            .flatten();
        #[cfg(not(feature = "testkit"))]
        let roster = crate::mux::backend_for(self.mux)
            .cached_pane_roster(&self.session_name, &self.workspace_id);
        ladder_probe(&self.pane, roster.as_ref(), || {
            shared_authoritative_pane_probe(self, &runtime, || self.produce_probe())
        })
    }

    fn produce_probe(&self) -> Option<AuthoritativePaneProbe> {
        #[cfg(feature = "testkit")]
        if let Some(probe) = forced_pane_probe() {
            let pane_ids = match probe {
                PaneProbe::Present(_) => vec![self.pane.clone()],
                PaneProbe::Absent(_) => Vec::new(),
                PaneProbe::Unknown => return None,
            };
            return Some(AuthoritativePaneProbe {
                mux: self.mux,
                session_name: self.session_name.clone(),
                observed_at_ms: crate::sidebar::timing::unix_now_ms(),
                pane_ids,
            });
        }

        match crate::mux::backend_for(self.mux).list_panes(self.probe_options()) {
            Ok(listing) => Some(AuthoritativePaneProbe {
                mux: self.mux,
                session_name: self.session_name.clone(),
                observed_at_ms: listing
                    .observed_at_ms
                    .max(crate::sidebar::timing::unix_now_ms()),
                pane_ids: listing.panes.into_iter().map(|pane| pane.pane_id).collect(),
            }),
            Err(err) => {
                debug!(
                    pane = %self.pane,
                    session = %self.session_name,
                    error = %err,
                    "sidebar supervisor pane-liveness probe unavailable",
                );
                None
            }
        }
    }

    fn probe_options(&self) -> crate::mux::PaneListOptions {
        crate::mux::PaneListOptions {
            session_name: Some(self.session_name.clone()),
            workspace_id: Some(self.workspace_id.clone()),
            // Presence proves routine liveness. An absent or unavailable hint
            // escalates here because orphan reaping requires mux truth.
            consistency: crate::mux::PaneReadConsistency::RequireAuthoritative,
            command_timeout: Some(PANE_PROBE_TIMEOUT),
            ..Default::default()
        }
    }
}

fn ladder_probe(
    pane: &crate::ids::PaneId,
    roster: Option<&crate::mux::CachedPaneRoster>,
    escalate: impl FnOnce() -> PaneProbe,
) -> PaneProbe {
    match roster {
        Some(roster) if roster.pane_ids.contains(pane) => PaneProbe::Present(roster.observed_at_ms),
        _ => escalate(),
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
    let backend = crate::mux::backend_for(watchdog.mux);
    let listing = match backend.list_panes(watchdog.probe_options()) {
        Ok(listing) => listing,
        Err(err) => {
            return SelfCloseConfirmation::Keep {
                siblings: 0,
                reason: format!("authoritative pane probe failed: {err}"),
            };
        }
    };
    match self_close_verdict(&listing.panes, &watchdog.pane) {
        SelfCloseVerdict::PaneGone => reconfirm_pane_gone(
            || {
                backend
                    .list_panes(watchdog.probe_options())
                    .map(|listing| self_close_verdict(&listing.panes, &watchdog.pane))
                    .map_err(|err| err.to_string())
            },
            || thread::sleep(SELF_CLOSE_RECONFIRM_DELAY),
        ),
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

fn reconfirm_pane_gone(
    reprobe: impl FnOnce() -> std::result::Result<SelfCloseVerdict, String>,
    pause: impl FnOnce(),
) -> SelfCloseConfirmation {
    pause();
    match reprobe() {
        Ok(SelfCloseVerdict::PaneGone) => SelfCloseConfirmation::PaneGone,
        Err(err) => SelfCloseConfirmation::Keep {
            siblings: 0,
            reason: format!("pane-gone reconfirmation probe failed: {err}"),
        },
        Ok(SelfCloseVerdict::Keep { siblings, .. }) => SelfCloseConfirmation::Keep {
            siblings,
            reason: "authoritative absence not reproduced".to_owned(),
        },
        Ok(SelfCloseVerdict::Empty { floating_siblings }) => SelfCloseConfirmation::Keep {
            siblings: floating_siblings,
            reason: "authoritative absence not reproduced".to_owned(),
        },
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

fn shared_authoritative_pane_probe(
    watchdog: &PaneWatchdog,
    runtime: &crate::RuntimePaths,
    produce: impl FnOnce() -> Option<AuthoritativePaneProbe>,
) -> PaneProbe {
    let now_ms = crate::sidebar::timing::unix_now_ms();
    let read_fresh = || read_authoritative_pane_probe(runtime, watchdog, now_ms);
    if let Some(probe) = read_fresh() {
        return pane_probe_for(&probe, &watchdog.pane);
    }

    match crate::store::single_flight::coordinate(
        &runtime.authoritative_pane_probe_lock(),
        PANE_PROBE_WAIT_STEP,
        PANE_PROBE_WAIT_STEPS,
        read_fresh,
    ) {
        crate::store::single_flight::Coordination::Shared(probe) => {
            pane_probe_for(&probe, &watchdog.pane)
        }
        crate::store::single_flight::Coordination::Produce(_guard) => {
            let Some(probe) = produce() else {
                return PaneProbe::Unknown;
            };
            if crate::sidebar::cache::write_authoritative_pane_probe(runtime, &probe).is_err() {
                return PaneProbe::Unknown;
            }
            pane_probe_for(&probe, &watchdog.pane)
        }
        crate::store::single_flight::Coordination::Unavailable
        | crate::store::single_flight::Coordination::ContentionTimeout => PaneProbe::Unknown,
    }
}

fn read_authoritative_pane_probe(
    runtime: &crate::RuntimePaths,
    watchdog: &PaneWatchdog,
    now_ms: u64,
) -> Option<AuthoritativePaneProbe> {
    let bytes = std::fs::read(runtime.authoritative_pane_probe_path()).ok()?;
    let probe = serde_json::from_slice::<AuthoritativePaneProbe>(&bytes).ok()?;
    (probe.mux == watchdog.mux
        && probe.session_name == watchdog.session_name
        && now_ms.saturating_sub(probe.observed_at_ms) < pane_probe_interval().as_millis() as u64)
        .then_some(probe)
}

fn pane_probe_for(probe: &AuthoritativePaneProbe, pane: &crate::ids::PaneId) -> PaneProbe {
    if probe.pane_ids.contains(pane) {
        PaneProbe::Present(probe.observed_at_ms)
    } else {
        PaneProbe::Absent(probe.observed_at_ms)
    }
}

#[cfg(unix)]
fn worker_pid(child: &Child) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(child.id() as i32)
}

#[cfg(unix)]
struct WorkerMonitor<'a> {
    record_watch: &'a mut RecordWatch,
    exec_state: &'a mut PendingExec,
    worker_build: Option<&'a str>,
    supervisor_build: Option<&'a str>,
    started: Instant,
    runtime: Option<&'a crate::RuntimePaths>,
    config: &'a ServeConfig,
    watchdog: &'a mut Option<PaneWatchdog>,
    orphan_reap_pending: bool,
    handoff_deadline: Option<Instant>,
}

#[cfg(unix)]
impl WorkerMonitor<'_> {
    fn terminal(&self, exit_code: Option<i32>, signal: Option<i32>) -> WorkerExit {
        classify_worker_exit(
            self.orphan_reap_pending,
            self.handoff_deadline.is_some(),
            exit_code,
            signal,
        )
    }

    fn poll(&mut self, worker_pid: nix::unistd::Pid, now: Instant) -> Result<()> {
        use nix::sys::signal::Signal;

        if let Some(change) = self.record_watch.poll_if_due(now) {
            let worker_is_stale = apply_record_change(
                self.exec_state,
                &change,
                self.supervisor_build,
                self.worker_build,
            );
            if worker_is_stale && self.handoff_deadline.is_none() {
                self.request_handoff(now);
            }
        }
        if self.handoff_deadline.is_none()
            && self
                .exec_state
                .promotable(
                    self.worker_build,
                    now.saturating_duration_since(self.started),
                    respawn_stable_run(),
                )
                .is_some()
        {
            self.request_handoff(now);
        }
        if self
            .handoff_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            kill_worker(worker_pid, Signal::SIGKILL)?;
        }
        if !self.orphan_reap_pending
            && self
                .watchdog
                .as_mut()
                .is_some_and(|watchdog| watchdog.probe_if_due(now))
        {
            kill_worker(worker_pid, Signal::SIGKILL)?;
            self.orphan_reap_pending = true;
        }
        Ok(())
    }

    fn request_handoff(&mut self, now: Instant) {
        request_worker_handoff(self.runtime, self.config, self.exec_state.target.as_ref());
        self.handoff_deadline = Some(now + worker_handoff_grace());
    }
}

#[cfg(unix)]
fn kill_worker(worker_pid: nix::unistd::Pid, signal: nix::sys::signal::Signal) -> Result<()> {
    match nix::sys::signal::kill(worker_pid, signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(err) => Err(SidebarSuperviseErr::Wait(wait_error(err))),
    }
}

#[cfg(unix)]
fn wait_for_worker_and_reap_strays(
    worker_pid: nix::unistd::Pid,
    mut monitor: WorkerMonitor<'_>,
) -> Result<WorkerExit> {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) if pid == worker_pid => {
                return Ok(monitor.terminal(Some(status), None));
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) if pid == worker_pid => {
                return Ok(monitor.terminal(None, Some(signal as i32)));
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
                monitor.poll(worker_pid, Instant::now())?;
                thread::sleep(reap_poll_interval());
            }
            Ok(status) => {
                debug!(status = ?status, "observed non-terminal child status");
            }
            Err(nix::errno::Errno::ECHILD) => {
                return Ok(monitor.terminal(None, None));
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
        diag_sink(config).emit(DiagEvent::SupervisorConvergence {
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
    diag_sink(config).emit(DiagEvent::RendererSignalDeath {
        signal,
        exit_code,
        stderr_excerpt: stderr_excerpt.clone(),
    });
    report_sentry_signal_death(signal, exit_code, &stderr_excerpt);
}

fn record_orphan_reap(config: &ServeConfig, worker_pid: i32) {
    diag_sink(config).emit(DiagEvent::RendererOrphanReaped {
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
    diag_sink(config).emit(DiagEvent::SupervisorPreflightRejected {
        target_build: target_build.to_owned(),
        reason: reason.to_owned(),
    });
}

fn record_self_close_rejected(config: &ServeConfig, siblings: usize, reason: &str) {
    diag_sink(config).emit(DiagEvent::SelfCloseRejected {
        siblings,
        reason: reason.to_owned(),
    });
}

fn record_confirmed_self_close(config: &ServeConfig) {
    diag_sink(config).emit_unlimited(DiagEvent::RendererExit {
        cause: crate::diag::record::RendererExitCause::SelfCloseEmptyTab,
    });
}

fn diag_sink(config: &ServeConfig) -> crate::diag::DiagSink {
    crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    )
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
    duration_override(TEST_REAP_POLL_MS_ENV, REAP_POLL_INTERVAL)
}

#[cfg(feature = "testkit")]
fn respawn_delay(delay: Duration) -> Duration {
    duration_override(TEST_RESPAWN_BACKOFF_MS_ENV, delay)
}

#[cfg(feature = "testkit")]
fn record_poll_interval() -> Duration {
    duration_override(TEST_RECORD_POLL_MS_ENV, RECORD_POLL_INTERVAL)
}

#[cfg(not(feature = "testkit"))]
fn record_poll_interval() -> Duration {
    RECORD_POLL_INTERVAL
}

#[cfg(feature = "testkit")]
fn respawn_stable_run() -> Duration {
    duration_override(TEST_STABLE_RUN_MS_ENV, RESPAWN_STABLE_RUN)
}

#[cfg(not(feature = "testkit"))]
fn respawn_stable_run() -> Duration {
    RESPAWN_STABLE_RUN
}

#[cfg(feature = "testkit")]
fn worker_handoff_grace() -> Duration {
    duration_override(TEST_HANDOFF_GRACE_MS_ENV, WORKER_HANDOFF_GRACE)
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
    duration_override(TEST_PANE_PROBE_INTERVAL_MS_ENV, PANE_PROBE_INTERVAL)
}

#[cfg(feature = "testkit")]
fn duration_override(name: &str, default: Duration) -> Duration {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

#[cfg(not(feature = "testkit"))]
fn pane_probe_interval() -> Duration {
    PANE_PROBE_INTERVAL
}

#[cfg(feature = "testkit")]
fn forced_pane_probe() -> Option<PaneProbe> {
    if let Some(path) = env::var_os(TEST_PANE_PROBE_ABSENT_FILE_ENV).filter(|path| !path.is_empty())
    {
        return Some(if Path::new(&path).exists() {
            PaneProbe::Absent(0)
        } else {
            PaneProbe::Present(0)
        });
    }
    match env::var(TEST_PANE_PROBE_ENV).ok().as_deref() {
        Some("present") => Some(PaneProbe::Present(0)),
        Some("absent") => Some(PaneProbe::Absent(0)),
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
    fn worker_exit_classifier_honours_orphan_then_handoff_then_status() {
        assert_eq!(
            classify_worker_exit(false, false, Some(RELOAD_EXIT_CODE), None),
            WorkerExit::Reload
        );
        assert_eq!(
            classify_worker_exit(false, false, Some(PANIC_EXIT_CODE), None),
            WorkerExit::Respawn {
                signal: None,
                exit_code: Some(PANIC_EXIT_CODE)
            }
        );
        assert_eq!(
            classify_worker_exit(false, false, Some(SELF_CLOSE_EXIT_CODE), None),
            WorkerExit::ConfirmSelfClose
        );
        assert_eq!(
            classify_worker_exit(false, true, Some(SELF_CLOSE_EXIT_CODE), None),
            WorkerExit::Reload,
            "requested handoff outranks an exit code"
        );
        assert_eq!(
            classify_worker_exit(true, true, Some(RELOAD_EXIT_CODE), None),
            WorkerExit::OrphanReaped,
            "orphan reap outranks handoff and exit code"
        );
        assert_eq!(
            classify_worker_exit(false, false, None, None),
            WorkerExit::Respawn {
                signal: None,
                exit_code: None
            },
            "ECHILD's unknown status is abnormal termination"
        );
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
            last_observed_at_ms: None,
        };

        assert!(!watchdog.observe(PaneProbe::Absent(1)));
        assert!(!watchdog.observe(PaneProbe::Absent(1)));
        assert_eq!(watchdog.strikes, 1, "a cached absence counts once");
        assert!(!watchdog.observe(PaneProbe::Unknown));
        assert_eq!(watchdog.strikes, 1);
        assert!(!watchdog.observe(PaneProbe::Present(2)));
        assert_eq!(watchdog.strikes, 0);
        assert!(!watchdog.observe(PaneProbe::Absent(3)));
        assert!(!watchdog.observe(PaneProbe::Absent(4)));
        assert!(watchdog.observe(PaneProbe::Absent(5)));
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
            last_observed_at_ms: None,
        };

        let options = watchdog.probe_options();
        assert_eq!(
            options.consistency,
            crate::mux::PaneReadConsistency::RequireAuthoritative
        );
        assert_eq!(options.command_timeout, Some(PANE_PROBE_TIMEOUT));
    }

    #[test]
    fn pane_watchdog_presence_ladder_escalates_only_on_suspicion() {
        let pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9");
        let roster = crate::mux::CachedPaneRoster {
            pane_ids: vec![pane.clone()],
            observed_at_ms: 42,
        };
        let escalations = std::cell::Cell::new(0);
        let escalate = || {
            escalations.set(escalations.get() + 1);
            PaneProbe::Absent(43)
        };

        assert_eq!(
            ladder_probe(&pane, Some(&roster), escalate),
            PaneProbe::Present(42),
        );
        assert_eq!(escalations.get(), 0);

        let missing = crate::mux::CachedPaneRoster {
            pane_ids: Vec::new(),
            observed_at_ms: 44,
        };
        assert_eq!(
            ladder_probe(&pane, Some(&missing), escalate),
            PaneProbe::Absent(43),
        );
        assert_eq!(ladder_probe(&pane, None, escalate), PaneProbe::Absent(43));
        assert_eq!(escalations.get(), 2);
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
    fn self_close_accepts_only_reproduced_pane_absence() {
        assert_eq!(
            reconfirm_pane_gone(|| Ok(SelfCloseVerdict::PaneGone), || {}),
            SelfCloseConfirmation::PaneGone
        );

        assert_eq!(
            reconfirm_pane_gone(
                || {
                    Ok(SelfCloseVerdict::Empty {
                        floating_siblings: 0,
                    })
                },
                || {},
            ),
            SelfCloseConfirmation::Keep {
                siblings: 0,
                reason: "authoritative absence not reproduced".to_owned(),
            }
        );

        assert_eq!(
            reconfirm_pane_gone(|| Err("mux timed out".to_owned()), || {}),
            SelfCloseConfirmation::Keep {
                siblings: 0,
                reason: "pane-gone reconfirmation probe failed: mux timed out".to_owned(),
            }
        );
    }

    #[test]
    fn authoritative_probe_is_shared_across_consumers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
        let watchdog = PaneWatchdog {
            pane: pane.clone(),
            mux: crate::ids::MuxName::Tmux,
            session_name: "rimz-test".to_owned(),
            workspace_id,
            next_probe: Instant::now(),
            strikes: 0,
            last_observed_at_ms: None,
        };
        let calls = std::cell::Cell::new(0);
        let observed_at_ms = crate::sidebar::timing::unix_now_ms();
        let first = shared_authoritative_pane_probe(&watchdog, &runtime, || {
            calls.set(calls.get() + 1);
            Some(AuthoritativePaneProbe {
                mux: crate::ids::MuxName::Tmux,
                session_name: "rimz-test".to_owned(),
                observed_at_ms,
                pane_ids: vec![pane],
            })
        });
        let second = shared_authoritative_pane_probe(&watchdog, &runtime, || {
            calls.set(calls.get() + 1);
            None
        });

        assert_eq!(first, PaneProbe::Present(observed_at_ms));
        assert_eq!(second, first);
        assert_eq!(calls.get(), 1, "one producer feeds every consumer");
    }

    #[test]
    fn authoritative_probe_rejects_malformed_and_mismatched_cache() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let watchdog = PaneWatchdog {
            pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1"),
            mux: crate::ids::MuxName::Zellij,
            session_name: "rimz-test".to_owned(),
            workspace_id,
            next_probe: Instant::now(),
            strikes: 0,
            last_observed_at_ms: None,
        };
        std::fs::write(runtime.authoritative_pane_probe_path(), b"not json").unwrap();
        assert!(
            read_authoritative_pane_probe(
                &runtime,
                &watchdog,
                crate::sidebar::timing::unix_now_ms()
            )
            .is_none()
        );

        crate::sidebar::cache::write_authoritative_pane_probe(
            &runtime,
            &AuthoritativePaneProbe {
                mux: crate::ids::MuxName::Tmux,
                session_name: "other".to_owned(),
                observed_at_ms: crate::sidebar::timing::unix_now_ms(),
                pane_ids: Vec::new(),
            },
        )
        .unwrap();
        assert!(
            read_authoritative_pane_probe(
                &runtime,
                &watchdog,
                crate::sidebar::timing::unix_now_ms()
            )
            .is_none()
        );

        crate::sidebar::cache::write_authoritative_pane_probe(
            &runtime,
            &AuthoritativePaneProbe {
                mux: watchdog.mux,
                session_name: watchdog.session_name.clone(),
                observed_at_ms: 1,
                pane_ids: Vec::new(),
            },
        )
        .unwrap();
        assert!(read_authoritative_pane_probe(&runtime, &watchdog, 60_001).is_none());
    }

    #[test]
    fn authoritative_probe_cache_accepts_both_mux_identities() {
        for (mux, pane_raw) in [
            (crate::ids::MuxName::Zellij, "terminal_1"),
            (crate::ids::MuxName::Tmux, "%1"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
            let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
            runtime.ensure_dirs().unwrap();
            let pane = crate::ids::PaneId::from_parts(mux, pane_raw);
            let watchdog = PaneWatchdog {
                pane: pane.clone(),
                mux,
                session_name: "rimz-test".to_owned(),
                workspace_id,
                next_probe: Instant::now(),
                strikes: 0,
                last_observed_at_ms: None,
            };
            crate::sidebar::cache::write_authoritative_pane_probe(
                &runtime,
                &AuthoritativePaneProbe {
                    mux,
                    session_name: "rimz-test".to_owned(),
                    observed_at_ms: 10,
                    pane_ids: vec![pane],
                },
            )
            .unwrap();

            let probe = read_authoritative_pane_probe(&runtime, &watchdog, 11).unwrap();
            assert_eq!(
                pane_probe_for(&probe, &watchdog.pane),
                PaneProbe::Present(10)
            );
        }
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
