//! Convergence supervisor for the pane-resident sidebar renderer.
//!
//! The host alone paints. The supervisor owns the pane tty, polls durable build
//! intent, proves the painting build stable before preflight and self-exec, and
//! preserves the sidebar instance across failures and reloads. It confirms
//! self-close against authoritative mux truth, watches pane liveness, reaps
//! stray children, and shows a notice while an unavailable host is retried.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(feature = "testkit")]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::diag::record::{DiagEvent, SidebarHostUnavailableCause};
use crate::ids::SidebarInstanceId;
use crate::sidebar_pane::app::{EventForwarder, ServeConfig};
use crate::sidebar_pane::attach::Control;
use crate::tui::{MouseCapture, Screen, TerminalModeGuard};
use tracing::debug;

mod host_link;

use self::host_link::{HostEvent, HostLink};

const INSTANCE_ENV: &str = "RIMZ_SIDEBAR_INSTANCE_ID";
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
const TEST_SELF_CLOSE_PROBE_ENV: &str = "RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE";
const REAP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PANE_PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PANE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_PROBE_WAIT_STEP: Duration = Duration::from_millis(25);
const PANE_PROBE_WAIT_STEPS: u32 = 20;
const PANE_GONE_STRIKES: u8 = 3;
const SELF_CLOSE_RECONFIRM_DELAY: Duration = Duration::from_millis(500);
const RESPAWN_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(60);
const RESPAWN_STABLE_RUN: Duration = Duration::from_secs(60);
const RECORD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SUPERVISOR_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Debug, thiserror::Error)]
pub enum SidebarSuperviseErr {
    #[error(
        "rimz sidebar serve: stdout is not a terminal; the sidebar pane command must run in a pane"
    )]
    NotTerminal,
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("re-executing sidebar supervisor `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, SidebarSuperviseErr>;

pub fn instance_id() -> SidebarInstanceId {
    env::var(INSTANCE_ENV)
        .ok()
        .and_then(|raw| SidebarInstanceId::parse(&raw).ok())
        .unwrap_or_default()
}

struct StopSignal(signal_hook::SigId);

impl Drop for StopSignal {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

pub fn run(config: ServeConfig) -> Result<()> {
    use std::io::IsTerminal;

    let mut record_watch = RecordWatch::new(&config.workspace_id);
    if crate::workspace::record::read(&record_watch.record_path).is_err() {
        let no_room = crate::workspace::WorkspaceErr::NoRoom {
            location: format!("for workspace {}", config.workspace_id),
        };
        let _ = writeln!(io::stderr(), "rimz sidebar serve: {no_room}; stopping");
        return Ok(());
    }
    if !io::stdout().is_terminal() {
        return Err(SidebarSuperviseErr::NotTerminal);
    }
    let runtime = crate::RuntimePaths::for_workspace(config.workspace_id.clone())?;
    let state = crate::StatePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let _room = crate::disk::lock::RoomLock::hold(&runtime.room_lock())?;
    let stopped = Arc::new(AtomicBool::new(false));
    let _signal = StopSignal(signal_hook::flag::register(
        signal_hook::consts::SIGTERM,
        stopped.clone(),
    )?);
    // Hold the tty even when no host starts: retry notices use raw-mode lines,
    // and no mode sequence may land inside an attached host's frame.
    let modes = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main)?;
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut backoff = RESPAWN_BACKOFF_INITIAL;
    let mut pane_watchdog = PaneWatchdog::from_config(&config);
    let supervisor_build = crate::build_id::current();
    let mut exec_state = PendingExec::default();
    let diag = diag_sink(&config);
    loop {
        if stopped.load(Ordering::SeqCst) {
            modes.preserve_for_handoff();
            remove_orphan_runtime_files(&config);
            return Ok(());
        }
        if pane_watchdog
            .as_mut()
            .is_some_and(|watchdog| watchdog.probe_if_due(Instant::now()))
        {
            record_pane_gone(&config, &diag);
            modes.preserve_for_handoff();
            remove_orphan_runtime_files(&config);
            return Ok(());
        }
        let target = crate::reload::recorded_reexec_target(&config.workspace_id);
        exec_state.observe(&target, supervisor_build);
        // Atomic installs can leave this image unlinked. Start the host from
        // the durable room target, even when its bytes match this supervisor.
        let current = crate::reload::current_reexec_target().unwrap_or_else(crate::proc::rimz_exe);
        let exe = host_executable(target, current);
        let (exit, painting_build, attached_duration) =
            match attach_to_host(&config, &runtime, &state, &exe, supervisor_build) {
                Err(failure) => (Err(failure), None, None),
                Ok(attached) => {
                    spawn_test_stray_if_requested();
                    let started = Instant::now();
                    let build = attached.link.build.clone();
                    let (exit, duration) = attached.watch(HostMonitor {
                        record_watch: &mut record_watch,
                        exec_state: &mut exec_state,
                        supervisor_build,
                        started,
                        watchdog: &mut pane_watchdog,
                        stopped: &stopped,
                    });
                    (Ok(exit), build, Some(duration))
                }
            };
        let run_duration = attached_duration.unwrap_or_default();
        let failure = match exit {
            Err(failure) => failure,
            Ok(RoundExit::Stopped) => continue,
            Ok(RoundExit::HostLost) => AttachFailure::Lost,
            Ok(RoundExit::OrphanReaped) => {
                record_pane_gone(&config, &diag);
                modes.preserve_for_handoff();
                remove_orphan_runtime_files(&config);
                return Ok(());
            }
            Ok(RoundExit::Reload) => {
                if let Some(target) = exec_state.promotable(
                    painting_build.as_deref(),
                    run_duration,
                    respawn_stable_run(),
                ) {
                    diag.emit(DiagEvent::SupervisorConvergence {
                        target_build: target.build.clone(),
                    });
                    match preflight_supervisor(&target.path) {
                        Ok(()) => {
                            modes.preserve_for_handoff();
                            return exec_supervisor(&target.path, &args, &config);
                        }
                        Err(reason) => {
                            record_preflight_rejected(&diag, &target.build, &reason);
                            exec_state.reject(&target.build);
                        }
                    }
                }
                continue;
            }
            Ok(RoundExit::ConfirmSelfClose) => {
                match confirm_self_close(&config, &pane_watchdog) {
                    SelfCloseConfirmation::Close | SelfCloseConfirmation::PaneGone => {
                        drop(modes);
                        remove_orphan_runtime_files(&config);
                        record_confirmed_self_close(&diag);
                        return Ok(());
                    }
                    SelfCloseConfirmation::Keep { siblings, reason } => {
                        record_self_close_rejected(&diag, siblings, &reason);
                        sleep_respawn_backoff(
                            respawn_delay(RESPAWN_BACKOFF_INITIAL),
                            &mut record_watch,
                            &mut exec_state,
                            supervisor_build,
                            &stopped,
                        );
                    }
                }
                continue;
            }
        };
        if stopped.load(Ordering::SeqCst) {
            continue;
        }
        let (delay, next) = respawn_backoff(backoff, run_duration);
        let delay = respawn_delay(delay);
        paint_unavailable_notice(&failure, delay);
        let (cause, reason) = failure.diagnostic();
        diag.emit(DiagEvent::SidebarHostUnavailable {
            cause,
            reason,
            attached_ms: attached_duration.map(|duration| duration.as_millis() as u64),
            retry_ms: delay.as_millis() as u64,
        });
        sleep_respawn_backoff(
            delay,
            &mut record_watch,
            &mut exec_state,
            supervisor_build,
            &stopped,
        );
        backoff = next;
    }
}

#[derive(Debug, thiserror::Error)]
enum AttachFailure {
    #[error("host did not start ({error}); see {log}")]
    StartFailed { error: io::Error, log: PathBuf },
    #[error("no host answered")]
    NoAnswer,
    #[error("host rejected this pane: {0}")]
    Rejected(String),
    #[error("host reply unreadable")]
    ReplyUnreadable,
    #[error("host went away")]
    Lost,
}

impl AttachFailure {
    fn diagnostic(&self) -> (SidebarHostUnavailableCause, String) {
        match self {
            Self::StartFailed { error, log } => (
                SidebarHostUnavailableCause::StartFailed,
                format!("({error}); see {}", log.display()),
            ),
            Self::NoAnswer => (SidebarHostUnavailableCause::NoAnswer, String::new()),
            Self::Rejected(reason) => (SidebarHostUnavailableCause::Rejected, reason.clone()),
            Self::ReplyUnreadable => (SidebarHostUnavailableCause::ReplyUnreadable, String::new()),
            Self::Lost => (SidebarHostUnavailableCause::Lost, String::new()),
        }
    }
}

fn paint_unavailable_notice(failure: &AttachFailure, delay: Duration) {
    if let Err(err) = write!(
        io::stdout(),
        "\x1b[2J\x1b[1;1Hsidebar: {failure}\r\nretrying in {}s\r\n",
        delay.as_secs().max(1)
    )
    .and_then(|()| io::stdout().flush())
    {
        debug!(error = %err, "sidebar host-unavailable notice could not be written");
    }
}

/// A pane the room host is painting: its link and input forwarder.
struct Attached {
    link: HostLink,
    forwarder: EventForwarder,
}

/// Hand this pane to the session's host, or explain why it cannot paint yet.
fn attach_to_host(
    config: &ServeConfig,
    runtime: &crate::RuntimePaths,
    state: &crate::StatePaths,
    exe: &Path,
    supervisor_build: Option<&str>,
) -> std::result::Result<Attached, AttachFailure> {
    use std::os::fd::AsFd;

    let stdout = io::stdout();
    let log = state.sidebar_host_log(config.mux, &config.session_name);
    let stream = host_link::connect(config, runtime, host_link::HOST_WAIT, || {
        host_link::spawn_host(exe, config, runtime, state, &log)
    })
    .map_err(|error| AttachFailure::StartFailed { error, log })?
    .ok_or(AttachFailure::NoAnswer)?;
    let hello = host_link::hello_for(config, supervisor_build);
    let link = HostLink::open(stream, &hello, stdout.as_fd(), host_link::REPLY_WAIT)?;
    let wake_path = runtime.sidebar_socket_path(&config.instance_id);
    // The host sized the pane from its fd at the hello; a resize that raced
    // the handover is settled by one more look.
    if let Ok(waker) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = waker.send_to(b"resize", &wake_path);
    }
    let forwarder = EventForwarder::start(wake_path);
    Ok(Attached { link, forwarder })
}

struct HostMonitor<'a> {
    record_watch: &'a mut RecordWatch,
    exec_state: &'a mut PendingExec,
    supervisor_build: Option<&'a str>,
    started: Instant,
    watchdog: &'a mut Option<PaneWatchdog>,
    stopped: &'a AtomicBool,
}

impl Attached {
    /// Stay attached until the host or this pane ends it, then give the tty
    /// back to whichever process paints next.
    fn watch(mut self, monitor: HostMonitor<'_>) -> (RoundExit, Duration) {
        let started = monitor.started;
        let exit = watch_host(&mut self.link, monitor);
        let duration = started.elapsed();
        drop(self.link);
        self.forwarder.stop();
        (exit, duration)
    }
}

fn watch_host(link: &mut HostLink, monitor: HostMonitor<'_>) -> RoundExit {
    loop {
        if monitor.stopped.load(Ordering::SeqCst) {
            return RoundExit::Stopped;
        }
        match link.poll(reap_poll_interval()) {
            HostEvent::Control(Control::SelfClose) => return RoundExit::ConfirmSelfClose,
            HostEvent::Control(Control::Reload) => return RoundExit::Reload,
            HostEvent::Lost => return RoundExit::HostLost,
            HostEvent::Quiet => {}
        }
        reap_exited_children();
        let now = Instant::now();
        if let Some(change) = monitor.record_watch.poll_if_due(now) {
            // A host on a superseded build leaves by itself and says
            // `reload`; the record only tells this supervisor whether it
            // has a build of its own to move to.
            apply_record_change(monitor.exec_state, &change, monitor.supervisor_build);
        }
        if monitor
            .exec_state
            .promotable(
                link.build.as_deref(),
                now.saturating_duration_since(monitor.started),
                respawn_stable_run(),
            )
            .is_some()
        {
            return RoundExit::Reload;
        }
        if monitor
            .watchdog
            .as_mut()
            .is_some_and(|watchdog| watchdog.probe_if_due(now))
        {
            return RoundExit::OrphanReaped;
        }
    }
}

/// A host this supervisor started stays its child. One that exits after this
/// supervisor re-exec'd has no reaper thread left to collect it, so the
/// attached loop reaps exited children on every poll.
#[cfg(all(unix, not(test)))]
fn reap_exited_children() {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    while matches!(
        waitpid(nix::unistd::Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)),
        Ok(status) if status != WaitStatus::StillAlive
    ) {}
}

#[cfg(any(not(unix), test))]
fn reap_exited_children() {}

fn host_executable(
    target: crate::reload::WorkspaceReexecTarget,
    current: std::path::PathBuf,
) -> std::path::PathBuf {
    match target {
        crate::reload::WorkspaceReexecTarget::Verified(target) => target.path,
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => current,
    }
}

fn exec_supervisor(exe: &Path, args: &[OsString], config: &ServeConfig) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let source = Command::new(exe)
        .args(args)
        .env(INSTANCE_ENV, config.instance_id.as_str())
        .exec();
    Err(SidebarSuperviseErr::Spawn {
        program: render_program(exe),
        source,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundExit {
    Reload,
    ConfirmSelfClose,
    Stopped,
    OrphanReaped,
    /// The room host painting this pane went away without a word.
    HostLost,
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
        painting_build: Option<&str>,
        run_duration: Duration,
        stable_run: Duration,
    ) -> Option<crate::reload::StagedBuild> {
        self.target
            .as_ref()
            .filter(|target| painting_build == Some(target.build.as_str()))
            .filter(|_| run_duration >= stable_run)
            .cloned()
    }

    fn reject(&mut self, build: &str) {
        self.target = None;
        self.rejected_build = Some(build.to_owned());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RecordChange {
    Verified(crate::reload::StagedBuild),
    Unavailable,
}

#[derive(Debug)]
pub(super) struct RecordWatch {
    workspace_id: crate::ids::WorkspaceId,
    record_path: PathBuf,
    last_seen_mtime: Option<SystemTime>,
    next_poll: Instant,
}

impl RecordWatch {
    pub(super) fn new(workspace_id: &crate::ids::WorkspaceId) -> Self {
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

    pub(super) fn poll_if_due(&mut self, now: Instant) -> Option<RecordChange> {
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
                observed_at_ms: crate::utils::time::unix_now_ms(),
                pane_ids,
            });
        }

        match crate::mux::backend_for(self.mux).list_panes(self.probe_options()) {
            Ok(listing) => Some(AuthoritativePaneProbe {
                mux: self.mux,
                session_name: self.session_name.clone(),
                observed_at_ms: listing
                    .observed_at_ms
                    .max(crate::utils::time::unix_now_ms()),
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
    let now_ms = crate::utils::time::unix_now_ms();
    let read_fresh = || read_authoritative_pane_probe(runtime, watchdog, now_ms);
    if let Some(probe) = read_fresh() {
        return pane_probe_for(&probe, &watchdog.pane);
    }

    match crate::disk::single_flight::coordinate(
        &runtime.authoritative_pane_probe_lock(),
        PANE_PROBE_WAIT_STEP,
        PANE_PROBE_WAIT_STEPS,
        read_fresh,
    ) {
        crate::disk::single_flight::Coordination::Shared(probe) => {
            pane_probe_for(&probe, &watchdog.pane)
        }
        crate::disk::single_flight::Coordination::Produce(_guard) => {
            let Some(probe) = produce() else {
                return PaneProbe::Unknown;
            };
            if crate::sidebar::cache::write_authoritative_pane_probe(runtime, &probe).is_err() {
                return PaneProbe::Unknown;
            }
            pane_probe_for(&probe, &watchdog.pane)
        }
        crate::disk::single_flight::Coordination::Unavailable
        | crate::disk::single_flight::Coordination::ContentionTimeout => PaneProbe::Unknown,
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

fn apply_record_change(
    exec_state: &mut PendingExec,
    change: &RecordChange,
    supervisor_build: Option<&str>,
) {
    let target = match change {
        RecordChange::Verified(target) => {
            crate::reload::WorkspaceReexecTarget::Verified(target.clone())
        }
        RecordChange::Unavailable => crate::reload::WorkspaceReexecTarget::Invalid,
    };
    exec_state.observe(&target, supervisor_build);
}

fn sleep_respawn_backoff(
    delay: Duration,
    record_watch: &mut RecordWatch,
    exec_state: &mut PendingExec,
    supervisor_build: Option<&str>,
    stopped: &AtomicBool,
) {
    let deadline = Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stopped.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(remaining.min(record_poll_interval()));
        reap_exited_children();
        if let Some(change) = record_watch.poll_now() {
            apply_record_change(exec_state, &change, supervisor_build);
            return;
        }
    }
}

fn record_pane_gone(config: &ServeConfig, diag: &crate::diag::DiagSink) {
    diag.emit(DiagEvent::SupervisorPaneGone {
        pane_id: config
            .own_pane
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
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

fn record_preflight_rejected(diag: &crate::diag::DiagSink, target_build: &str, reason: &str) {
    diag.emit(DiagEvent::SupervisorPreflightRejected {
        target_build: target_build.to_owned(),
        reason: reason.to_owned(),
    });
}

fn record_self_close_rejected(diag: &crate::diag::DiagSink, siblings: usize, reason: &str) {
    diag.emit(DiagEvent::SelfCloseRejected {
        siblings,
        reason: reason.to_owned(),
    });
}

fn record_confirmed_self_close(diag: &crate::diag::DiagSink) {
    diag.emit_unlimited(DiagEvent::RendererExit {
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
        runtime.sidebar_socket_path(&config.instance_id),
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
mod tests;
