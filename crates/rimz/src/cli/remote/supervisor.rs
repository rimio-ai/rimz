use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use rimz::remote::forward::{PortAction, PortSync, cancel_spec, open_spec};
use rimz::remote::link::{
    LinkAck, LinkEvent, LinkMonitor, LinkProbe, SessionLinkAction, SessionLinkState,
    blackout_after_from_env, control_check_spec, probe_interval_from_env, probe_stream_spec,
    probe_timeout_from_env,
};
use rimz::remote::reachability::{
    AttemptPacer, DIAL_TIMEOUT, DialPlan, FooterPhase, dial_interval_from_env, is_tun_interface,
    parse_dial_plan, ssh_config_query_spec,
};
use rimz::remote::recovery::{
    ConnectStage, HandoffStage, INTERNET_PROBE_TIMEOUT, InternetProbe, RecoveryPanel,
    internet_probe_from_env,
};
use rimz::remote::{RemoteTarget, SshAttachAttempt, SshAttachPlan};

use super::outage_ui::{HandoffScreen, OutageUi, PANEL_TICK, UiEvent};

const CONTROL_MASTER_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const CONTROL_MASTER_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_STREAM_BLACKOUT_FAILURES: u32 = 3;
const PROBE_RESPAWN_BACKOFF_MIN: Duration = Duration::from_secs(1);
const PROBE_RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const SSH_CONFIG_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const AUTO_FORWARD_CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

struct OutageState {
    connect_stage: ConnectStage,
    started: Instant,
    internet_probe: Option<InternetProbe>,
    panel: RecoveryPanel,
    attempts: u32,
}

impl OutageState {
    fn new(
        connect_stage: ConnectStage,
        handoff_stage: HandoffStage,
        host: &str,
        internet_probe: Option<InternetProbe>,
        server: Option<&DialPlan>,
    ) -> Self {
        let mut panel = RecoveryPanel::new(
            connect_stage,
            handoff_stage,
            host,
            internet_probe.as_ref(),
            server,
        );
        panel.note_attempt(1);
        Self {
            connect_stage,
            started: Instant::now(),
            panel,
            internet_probe,
            attempts: 0,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn begin_attempt(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        self.panel.note_attempt(self.attempts);
    }

    fn note_next_attempt(&mut self) {
        self.panel.note_attempt(self.attempts.saturating_add(1));
    }
}

pub(super) struct LinkSupervisor {
    plan: SshAttachPlan,
    control_path: PathBuf,
    policy: rimz::remote::ReconnectPolicy,
    dial_plan: Option<DialPlan>,
    stop: AtomicBool,
    outage: Option<OutageState>,
    master: Option<MasterGuard>,
    held_screen: Option<rimz::tui::TerminalModeGuard>,
    host: String,
}

impl LinkSupervisor {
    /// Establish the initial batch-mode master. `None` means the user
    /// interrupted the recovery panel; a supervisor without a control path
    /// requests the initial interactive fallback.
    pub(super) fn connect(plan: SshAttachPlan, control_path: PathBuf) -> Result<Option<Self>> {
        let dial_plan = resolve_dial_plan(plan.target().ssh_destination().as_str());
        let host = plan.target().host_display().to_owned();
        let mut supervisor = Self {
            plan,
            control_path,
            policy: rimz::remote::ReconnectPolicy::from_env(),
            dial_plan,
            stop: AtomicBool::new(false),
            outage: None,
            master: None,
            held_screen: None,
            host,
        };
        let mut ui = OutageUi::auto(ConnectStage::Initial, &supervisor.host);
        ui.report_connecting();
        let mut outage = OutageState::new(
            ConnectStage::Initial,
            HandoffStage::WebTunnel,
            &supervisor.host,
            internet_probe_for_wait(&ui),
            supervisor.dial_plan.as_ref(),
        );
        match wait_for_master(
            &supervisor.plan,
            &supervisor.control_path,
            supervisor.dial_plan.as_ref(),
            &supervisor.policy,
            &mut outage,
            &mut ui,
            Some(&supervisor.stop),
        )? {
            WaitOutcome::Connected {
                master,
                held_screen,
            } => {
                supervisor.master = Some(master);
                supervisor.held_screen = held_screen;
                Ok(Some(supervisor))
            }
            WaitOutcome::NeedsInteractive => Ok(Some(supervisor)),
            WaitOutcome::Interrupted => Ok(None),
        }
    }

    pub(super) fn recover(&mut self) -> Result<bool> {
        self.release_screen();
        drop(self.master.take());
        let mut ui = OutageUi::auto(ConnectStage::Recovery, &self.host);
        let outage = self.outage.get_or_insert_with(|| {
            if ui.is_plain() {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: web tunnel to {} lost — reconnecting in the background; Ctrl-C stops",
                    self.host,
                );
            }
            OutageState::new(
                ConnectStage::Recovery,
                HandoffStage::WebTunnel,
                &self.host,
                internet_probe_for_wait(&ui),
                self.dial_plan.as_ref(),
            )
        });
        match wait_for_master(
            &self.plan,
            &self.control_path,
            self.dial_plan.as_ref(),
            &self.policy,
            outage,
            &mut ui,
            Some(&self.stop),
        )? {
            WaitOutcome::Connected {
                master,
                held_screen,
            } => {
                self.master = Some(master);
                self.held_screen = held_screen;
                Ok(true)
            }
            WaitOutcome::Interrupted => Ok(false),
            WaitOutcome::NeedsInteractive => unreachable!("web recovery stays in batch mode"),
        }
    }

    pub(super) fn control(&self) -> Option<&Path> {
        self.master.as_ref().map(|_| self.control_path.as_path())
    }

    pub(super) fn round_ready(&mut self) {
        self.release_screen();
        self.outage = None;
    }

    pub(super) fn wait_for_exit(&mut self) -> Result<Option<i32>> {
        self.master
            .as_mut()
            .context("remote web ControlMaster is not running")?
            .wait_for_exit()
    }

    fn release_screen(&mut self) {
        drop(self.held_screen.take());
    }
}

impl Drop for LinkSupervisor {
    fn drop(&mut self) {
        self.release_screen();
    }
}

pub(super) fn supervise_remote(
    plan: &SshAttachPlan,
    control_path: &Path,
    setup_hint: &str,
    auto_forward: bool,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, ReconnectState, Verdict};

    let policy = ReconnectPolicy::from_env();
    let mut reconnect = ReconnectState::default();
    let target = plan.target();
    let host = target.host_display();
    let dial_plan = resolve_dial_plan(target.ssh_destination().as_str());
    let zombie_interval = dial_plan.as_ref().and_then(|_| dial_interval_from_env());
    let mut session_link = SessionLinkState::new(policy.gatetime, zombie_interval);
    let stop = AtomicBool::new(false);
    let mut first_attempt = true;
    let guard = super::tty::TtyGuard::acquire();
    let mut initial_ui = OutageUi::auto(ConnectStage::Initial, host);
    let port_sync = auto_forward.then(|| Arc::new(Mutex::new(PortSync::default())));
    initial_ui.report_connecting();
    let mut initial_outage = OutageState::new(
        ConnectStage::Initial,
        HandoffStage::Multiplexer,
        host,
        internet_probe_for_wait(&initial_ui),
        dial_plan.as_ref(),
    );
    let (mut ready_master, mut held_screen) = match wait_for_master(
        plan,
        control_path,
        dial_plan.as_ref(),
        &policy,
        &mut initial_outage,
        &mut initial_ui,
        Some(&stop),
    )? {
        WaitOutcome::Connected {
            master,
            held_screen,
        } => (Some(master), held_screen),
        WaitOutcome::NeedsInteractive => (None, None),
        WaitOutcome::Interrupted => return Ok(()),
    };
    let mut outage = None;
    loop {
        let (events_tx, events_rx) = mpsc::channel();
        let confirmed_master = ready_master.is_some();
        let probe = if confirmed_master {
            ProbeHandle::start_preestablished(
                target.clone(),
                control_path.to_path_buf(),
                events_tx,
                port_sync.clone(),
            )
        } else {
            ProbeHandle::start(
                target.clone(),
                control_path.to_path_buf(),
                events_tx,
                port_sync.clone(),
            )
        };
        let replacement = !first_attempt;
        let attempt = if first_attempt {
            plan.initial()
        } else {
            plan.retry()
        }
        .with_mark(held_screen.is_some() && !rimz::tui::no_color());
        first_attempt = false;
        let spec = probe.attach_spec(&attempt);
        if replacement {
            guard.settle_terminal_replies();
        }
        let outcome = run_ssh_session(
            &spec,
            host,
            &events_rx,
            &mut session_link,
            dial_plan.as_ref(),
            confirmed_master,
        );
        guard.restore();
        let mut handoff = HandoffScreen::take(&mut held_screen);
        let mut outcome = outcome?;
        outcome.established |= confirmed_master;
        if outcome
            .stderr
            .as_deref()
            .is_some_and(|stderr| rimz::remote::attach_interrupted(outcome.status.code(), stderr))
        {
            handoff.release();
            guard.reset_emulator();
            return Ok(());
        }
        let summary = session_error_summary(
            outcome.stderr.as_deref(),
            outcome.status.code(),
            confirmed_master,
        );
        drop(probe);
        let attach_evidence = ready_master
            .as_mut()
            .map(MasterGuard::is_running)
            .transpose()?
            .map(|control_alive| (control_alive, outcome.lived_past_gatetime));
        if outcome.established {
            outage = None;
        }
        let retry_cause = if outcome.killed_zombie {
            drop(ready_master.take());
            handoff.release();
            guard.reset_emulator();
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: link to {host} confirmed dead — host reachable, session silent; reconnecting now",
            );
            reconnect.settle_zombie_kill();
            RetryCause::Zombie
        } else {
            match reconnect.settle(outcome.status.code(), outcome.established, attach_evidence) {
                Verdict::CleanExit => {
                    handoff.release();
                    if outcome.stderr.is_some() {
                        let _ = writeln!(std::io::stderr().lock(), "rimz: detached from {host}");
                    }
                    return Ok(());
                }
                Verdict::Fatal { code } => {
                    let message = fatal_session_message(
                        code,
                        host,
                        target.remote_path(),
                        setup_hint,
                        summary.as_deref(),
                    );
                    handoff.hold_failure(&message);
                    guard.reset_emulator();
                    bail!("{message}")
                }
                Verdict::Retry => {
                    drop(ready_master.take());
                    handoff.release();
                    guard.reset_emulator();
                    RetryCause::Dropped
                }
                Verdict::Reattach => {
                    handoff.release();
                    guard.reset_emulator();
                    let detail = summary
                        .as_deref()
                        .map(|summary| format!(" — {summary}"))
                        .unwrap_or_default();
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "rimz: remote session on {host} disconnected{detail} — reattaching",
                    );
                    continue;
                }
            }
        };
        let mut ui = OutageUi::auto(ConnectStage::Recovery, host);
        let outage = outage.get_or_insert_with(|| {
            OutageState::new(
                ConnectStage::Recovery,
                HandoffStage::Multiplexer,
                host,
                internet_probe_for_wait(&ui),
                dial_plan.as_ref(),
            )
        });
        if matches!(retry_cause, RetryCause::Dropped) {
            outage.panel.note_ssh_error(summary.clone());
            if ui.is_plain() {
                let detail = summary
                    .as_deref()
                    .map(|summary| format!(" — {summary}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: link to {host} lost{detail} — reconnecting in the background; Ctrl-C stops",
                );
            }
            if let Some(action) = session_link.transport_lost() {
                render_session_link_action(host, action);
            }
        }
        match wait_for_master(
            plan,
            control_path,
            dial_plan.as_ref(),
            &policy,
            outage,
            &mut ui,
            Some(&stop),
        )? {
            WaitOutcome::Connected {
                master,
                held_screen: next_held_screen,
            } => {
                ready_master = Some(master);
                held_screen = next_held_screen;
            }
            WaitOutcome::Interrupted => {
                return Ok(());
            }
            // `wait_for_master` only requests an interactive fallback for an
            // initial connection.
            WaitOutcome::NeedsInteractive => unreachable!("recovery stays in batch mode"),
        }
    }
}

enum RetryCause {
    Zombie,
    Dropped,
}

pub(super) fn fatal_session_message(
    code: i32,
    host: &str,
    remote_path: Option<&str>,
    setup_hint: &str,
    summary: Option<&str>,
) -> String {
    match code {
        rimz::remote::REMOTE_PATH_MISSING_EXIT => {
            remote_path_missing_message(host, remote_path, None)
        }
        rimz::remote::REMOTE_RIMZ_MISSING_EXIT => format!(
            "rimz is not installed on {host}; install it over SSH with:\n    \
             rimz remote setup {setup_hint}"
        ),
        rimz::remote::REMOTE_VERSION_SKEW_EXIT => format!(
            "your rimz and {host}'s rimz differ by a minor version; upgrade the older side \
             (`rimz remote setup {setup_hint}` upgrades the remote), or retry with \
             --force-version to attach anyway"
        ),
        rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT => format!(
            "your rimz and {host}'s rimz differ by a major version; upgrade required — \
             `rimz remote setup {setup_hint}` upgrades the remote"
        ),
        rimz::remote::REMOTE_SESSION_LOST_EXIT => {
            format!("remote session on {host} ended before it could settle; not reconnecting")
        }
        _ => {
            let message = format!("ssh to {host} exited with status {code}; not reconnecting");
            match summary {
                Some(summary) => format!("{message} — {summary}"),
                None => message,
            }
        }
    }
}

fn remote_path_missing_message(host: &str, path: Option<&str>, summary: Option<&str>) -> String {
    let error = summary.map_or_else(
        || match path {
            Some(path) => format!("remote path does not exist on {host}: {path}"),
            None => format!("remote path does not exist on {host}"),
        },
        str::to_owned,
    );
    format!(
        "{error}\ncheck the target with `rimz remote list`, then correct the alias or remote path"
    )
}

fn interrupted_message(
    connect_stage: ConnectStage,
    host: &str,
    last_error: Option<&str>,
) -> String {
    let action = match connect_stage {
        ConnectStage::Initial => "connect",
        ConnectStage::Recovery => "reconnect",
    };
    let detail = last_error
        .map(|error| format!(" — last error: {error}"))
        .unwrap_or_default();
    format!("rimz: {action} to {host} stopped{detail}")
}

fn run_ssh_session(
    spec: &rimz::mux::CommandSpec,
    host: &str,
    events: &mpsc::Receiver<LinkEvent>,
    link: &mut SessionLinkState,
    dial_plan: Option<&DialPlan>,
    capture_stderr: bool,
) -> Result<SessionOutcome> {
    let mut command = spec.to_command();
    if capture_stderr {
        command.stderr(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
    let stderr = drain_stderr(&mut child);
    let started = Instant::now();
    let mut update = link.begin_session();
    loop {
        if let Some(status) = child.try_wait().context("polling ssh session")? {
            let (established, lived_past_gatetime) =
                link.finish(started.elapsed(), events.try_iter());
            return Ok(SessionOutcome {
                status,
                established,
                lived_past_gatetime,
                killed_zombie: false,
                stderr: finish_session_stderr(stderr),
            });
        }
        let elapsed = started.elapsed();
        let poll = session_poll_interval(elapsed, update.next_deadline);
        let event = recv_link_event(events, poll);
        let elapsed = started.elapsed();
        update = link.advance(elapsed, event.into_iter().chain(events.try_iter()));
        for action in &update.actions {
            if matches!(action, SessionLinkAction::VerifyZombie) {
                if let Some(outcome) = verify_zombie(&mut child, dial_plan)? {
                    return Ok(SessionOutcome {
                        stderr: finish_session_stderr(stderr),
                        ..outcome
                    });
                }
            } else {
                render_session_link_action(host, *action);
            }
        }
    }
}

fn finish_session_stderr(stderr: Option<std::thread::JoinHandle<String>>) -> Option<String> {
    stderr.map(|reader| reader.join().unwrap_or_default())
}

fn session_error_summary(
    stderr: Option<&str>,
    exit_code: Option<i32>,
    confirmed_master: bool,
) -> Option<String> {
    stderr
        .and_then(rimz::remote::attach_error_summary)
        .or_else(|| {
            (confirmed_master && exit_code == Some(rimz::remote::SSH_TRANSPORT_EXIT))
                .then(|| "SSH control connection dropped".to_owned())
        })
}

fn verify_zombie(
    child: &mut Child,
    dial_plan: Option<&DialPlan>,
) -> Result<Option<SessionOutcome>> {
    let Some(plan) = dial_plan else {
        return Ok(None);
    };
    if route_tun(plan).is_none() && !dial(plan) {
        return Ok(None);
    }
    // A reachable host plus an established, blacked-out probe proves this
    // transport is a zombie. SIGKILL releases its tty before replacement.
    match child.kill() {
        Ok(()) => {
            let status = child.wait().context("waiting for killed ssh session")?;
            Ok(Some(SessionOutcome {
                status,
                established: true,
                lived_past_gatetime: true,
                killed_zombie: true,
                stderr: None,
            }))
        }
        Err(kill_err) => match child.try_wait().context("polling raced ssh session")? {
            Some(status) => Ok(Some(SessionOutcome {
                status,
                established: true,
                lived_past_gatetime: true,
                killed_zombie: false,
                stderr: None,
            })),
            None => Err(kill_err).context("killing zombie ssh session"),
        },
    }
}

fn session_poll_interval(elapsed: Duration, next_deadline: Option<Duration>) -> Duration {
    let poll = Duration::from_millis(200);
    next_deadline.map_or(poll, |deadline| poll.min(deadline.saturating_sub(elapsed)))
}

#[derive(Debug)]
struct SessionOutcome {
    status: std::process::ExitStatus,
    established: bool,
    lived_past_gatetime: bool,
    killed_zombie: bool,
    stderr: Option<String>,
}

fn resolve_dial_plan(destination: &str) -> Option<DialPlan> {
    dial_interval_from_env()?;
    let output = match ssh_config_query_spec(destination).run_with_timeout(SSH_CONFIG_QUERY_TIMEOUT)
    {
        Ok(output) => output,
        Err(err) => {
            tracing::debug!(destination, error = %err, "SSH endpoint discovery failed");
            return None;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let plan = parse_dial_plan(&stdout);
    if plan.is_none() {
        tracing::debug!(destination, "SSH endpoint is not directly dialable");
    }
    plan
}

fn dial(plan: &DialPlan) -> bool {
    dial_with_timeout(plan, DIAL_TIMEOUT)
}

fn dial_with_timeout(plan: &DialPlan, timeout: Duration) -> bool {
    if timeout.is_zero() {
        return false;
    }
    let Ok(addrs) = (plan.host.as_str(), plan.port).to_socket_addrs() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    for addr in addrs {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if TcpStream::connect_timeout(&addr, remaining).is_ok() {
            return true;
        }
    }
    false
}

enum WaitOutcome {
    Connected {
        master: MasterGuard,
        held_screen: Option<rimz::tui::TerminalModeGuard>,
    },
    NeedsInteractive,
    Interrupted,
}

fn internet_probe_for_wait(ui: &OutageUi) -> Option<InternetProbe> {
    if ui.is_plain() {
        None
    } else {
        internet_probe_from_env()
    }
}

fn wait_for_master(
    plan: &SshAttachPlan,
    control_path: &Path,
    dial_plan: Option<&DialPlan>,
    policy: &rimz::remote::ReconnectPolicy,
    outage: &mut OutageState,
    ui: &mut OutageUi,
    stop: Option<&AtomicBool>,
) -> Result<WaitOutcome> {
    let connect_stage = outage.connect_stage;
    let started = Instant::now();
    outage.panel.begin_wait();
    let mut reachability =
        ReachabilityDriver::new(*policy, started, outage.internet_probe.as_ref(), dial_plan);
    reachability.note_attempt_failed(started);
    let mut master = MasterState::Idle;
    let mut last_reported_error = None;
    let mut initial_attempt_due = connect_stage == ConnectStage::Initial;
    loop {
        let now = Instant::now();
        reachability.poll(now).present(Some(&mut outage.panel), ui);
        if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
            drop(std::mem::take(&mut master));
            let last_error = outage.panel.last_error().map(str::to_owned);
            ui.release()?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                interrupted_message(
                    connect_stage,
                    plan.target().host_display(),
                    last_error.as_deref()
                )
            );
            return Ok(WaitOutcome::Interrupted);
        }
        reachability.schedule_probes(now);

        match master.advance(
            now,
            started.elapsed(),
            plan,
            control_path,
            policy.master_deadline,
            &mut outage.panel,
        )? {
            MasterTick::Pending(state) => master = state,
            MasterTick::Failed(summary, may_need_interactive) => {
                master = MasterState::Idle;
                let needs_interactive = may_need_interactive
                    && connect_stage == ConnectStage::Initial
                    && !summary
                        .as_deref()
                        .is_some_and(rimz::remote::transport_failure);
                note_master_failure(
                    outage,
                    ui,
                    &mut reachability,
                    now,
                    summary,
                    &mut last_reported_error,
                );
                if needs_interactive {
                    ui.release()?;
                    return Ok(WaitOutcome::NeedsInteractive);
                }
            }
            MasterTick::Connected(guard) => {
                if connect_stage == ConnectStage::Initial
                    && let Some((spec, path)) = plan.path_preflight(control_path)
                    && run_path_preflight(&spec)
                {
                    let fix = "check the target with `rimz remote list`, then correct the alias or remote path".to_owned();
                    let details = [format!("{}: {path}", plan.target().host_display()), fix];
                    let message =
                        remote_path_missing_message(plan.target().host_display(), Some(path), None);
                    outage.panel.note_ssh_error(Some(message.clone()));
                    ui.fail_hold("Remote path does not exist", &details)?;
                    drop(guard);
                    return Err(anyhow::anyhow!(message));
                }
                let frame = outage
                    .panel
                    .frame(outage.elapsed(), FooterPhase::Connecting);
                let held_screen = ui.handoff(&frame)?;
                ui.report_reattached();
                return Ok(WaitOutcome::Connected {
                    master: guard,
                    held_screen,
                });
            }
        }

        if master.is_idle() && (initial_attempt_due || reachability.attempt_due(now)) {
            initial_attempt_due = false;
            if let Err(err) = prepare_control_path(control_path) {
                note_master_failure(
                    outage,
                    ui,
                    &mut reachability,
                    now,
                    Some(format!("preparing SSH control socket: {err}")),
                    &mut last_reported_error,
                );
            } else {
                outage.begin_attempt();
                outage.panel.session_starting();
                reachability.begin_attempt(now);
                match MasterAttempt::spawn(
                    plan.master(control_path, policy.master_connect_timeout),
                    control_path.to_path_buf(),
                ) {
                    Ok(attempt) => master = MasterState::connecting(attempt, now),
                    Err(err) => {
                        note_master_failure(
                            outage,
                            ui,
                            &mut reachability,
                            now,
                            Some(format!("starting SSH: {err}")),
                            &mut last_reported_error,
                        );
                    }
                }
            }
        }

        let wait_elapsed = started.elapsed();
        let outage_elapsed = outage.elapsed();
        if ui.tick(
            &mut outage.panel,
            wait_elapsed,
            outage_elapsed,
            reachability.footer(now, master.in_flight()),
        )? == UiEvent::Interrupted
        {
            drop(std::mem::take(&mut master));
            let last_error = outage.panel.last_error().map(str::to_owned);
            ui.release()?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                interrupted_message(
                    connect_stage,
                    plan.target().host_display(),
                    last_error.as_deref()
                )
            );
            return Ok(WaitOutcome::Interrupted);
        }
        sleep_retry_wait(PANEL_TICK, stop);
    }
}

fn run_path_preflight(spec: &rimz::mux::CommandSpec) -> bool {
    match spec
        .to_command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => path_preflight_missing(status.code()),
        Err(err) => {
            tracing::debug!(
                error = %err,
                command = %rimz::remote::display_ssh_command(spec),
                "remote path preflight unavailable"
            );
            false
        }
    }
}

fn path_preflight_missing(exit_code: Option<i32>) -> bool {
    exit_code == Some(1)
}

fn note_master_failure(
    outage: &mut OutageState,
    ui: &OutageUi,
    reachability: &mut ReachabilityDriver,
    now: Instant,
    summary: Option<String>,
    last_reported_error: &mut Option<String>,
) {
    outage.panel.note_ssh_error(summary.clone());
    outage.note_next_attempt();
    if summary.as_ref() != last_reported_error.as_ref() {
        ui.report_attempt_failed(summary.as_deref());
        *last_reported_error = summary;
    }
    reachability.note_attempt_failed(now);
}

fn network_fingerprint() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:443").ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

fn route_tun(plan: &DialPlan) -> Option<String> {
    if let Ok(ifname) = std::env::var("RIMZ_REMOTE_TUN")
        && !ifname.is_empty()
    {
        return Some(ifname);
    }

    let addrs = (plan.host.as_str(), plan.port).to_socket_addrs().ok()?;
    for endpoint in addrs {
        let bind_address = if endpoint.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let Ok(socket) = UdpSocket::bind(bind_address) else {
            continue;
        };
        if socket.connect(endpoint).is_err() {
            continue;
        }
        let Ok(local) = socket.local_addr() else {
            continue;
        };
        let Ok(interfaces) = nix::ifaddrs::getifaddrs() else {
            continue;
        };
        for interface in interfaces {
            let Some(address) = interface.address else {
                continue;
            };
            let interface_ip = address
                .as_sockaddr_in()
                .map(|address| IpAddr::V4(address.ip()))
                .or_else(|| {
                    address
                        .as_sockaddr_in6()
                        .map(|address| IpAddr::V6(address.ip()))
                });
            if interface_ip == Some(local.ip())
                && is_tun_interface(
                    &interface.interface_name,
                    interface
                        .flags
                        .contains(nix::net::if_::InterfaceFlags::IFF_POINTOPOINT),
                )
            {
                return Some(interface.interface_name);
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
enum DialStage {
    Internet,
    Server,
}

struct DialResult {
    stage: DialStage,
    reachable: bool,
    tun: Option<String>,
}

struct ReachabilityDriver {
    pacer: AttemptPacer,
    interval: Option<Duration>,
    next_dial: Instant,
    result_tx: mpsc::Sender<DialResult>,
    result_rx: mpsc::Receiver<DialResult>,
    internet_pending: bool,
    server_pending: bool,
    internet_probe: Option<InternetProbe>,
    server_plan: Option<DialPlan>,
    last_network_up: bool,
    last_tun: Option<String>,
}

impl ReachabilityDriver {
    fn new(
        policy: rimz::remote::ReconnectPolicy,
        started: Instant,
        internet_probe: Option<&InternetProbe>,
        server_plan: Option<&DialPlan>,
    ) -> Self {
        let interval = dial_interval_from_env();
        let pacer = AttemptPacer::new(
            policy,
            started,
            internet_probe.is_some() && interval.is_some(),
            server_plan.is_some() && interval.is_some(),
        );
        let last_network_up = pacer.network_up();
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            pacer,
            interval,
            next_dial: started,
            result_tx,
            result_rx,
            internet_pending: false,
            server_pending: false,
            internet_probe: internet_probe.cloned(),
            server_plan: server_plan.cloned(),
            last_network_up,
            last_tun: None,
        }
    }

    fn poll(&mut self, now: Instant) -> ReachabilityTick {
        let mut tick = ReachabilityTick::default();
        for result in self.result_rx.try_iter() {
            match result.stage {
                DialStage::Internet => {
                    self.internet_pending = false;
                    self.pacer.note_internet(result.reachable, now);
                    tick.internet = Some(result.reachable);
                }
                DialStage::Server => {
                    self.server_pending = false;
                    if result.tun.as_ref() != self.last_tun.as_ref() {
                        tick.tun_edge = result.tun.clone();
                    }
                    self.last_tun = result.tun.clone();
                    self.pacer.note_server(result.reachable, now);
                    tick.server = Some((result.reachable, result.tun));
                }
            }
        }
        let network_up = self.pacer.network_up();
        if network_up != self.last_network_up {
            tick.network_edge = Some(network_up);
            self.last_network_up = network_up;
        }
        tick
    }

    fn schedule_probes(&mut self, now: Instant) {
        let Some(interval) = self.interval.filter(|_| now >= self.next_dial) else {
            return;
        };
        self.pacer.note_fingerprint(network_fingerprint(), now);
        if let Some(probe) = &self.internet_probe
            && !self.internet_pending
        {
            spawn_internet_probe(probe, self.result_tx.clone());
            self.internet_pending = true;
        }
        if let Some(plan) = &self.server_plan
            && !self.server_pending
        {
            spawn_dial(
                DialStage::Server,
                plan,
                DIAL_TIMEOUT,
                self.result_tx.clone(),
            );
            self.server_pending = true;
        }
        self.next_dial = now + interval;
    }

    fn begin_attempt(&mut self, now: Instant) {
        self.pacer.begin_attempt(now);
    }

    fn note_attempt_failed(&mut self, now: Instant) {
        self.pacer.note_attempt_failed(now);
    }

    fn attempt_due(&self, now: Instant) -> bool {
        self.pacer.may_attempt(now)
    }

    fn footer(
        &self,
        now: Instant,
        attempt_in_flight: bool,
    ) -> rimz::remote::reachability::FooterPhase {
        self.pacer.footer(now, attempt_in_flight)
    }
}

#[derive(Default)]
struct ReachabilityTick {
    internet: Option<bool>,
    server: Option<(bool, Option<String>)>,
    network_edge: Option<bool>,
    tun_edge: Option<String>,
}

impl ReachabilityTick {
    fn present(self, panel: Option<&mut RecoveryPanel>, ui: &OutageUi) {
        if let Some(panel) = panel {
            if let Some(reachable) = self.internet {
                panel.note_internet(reachable);
            }
            if let Some((reachable, tun)) = &self.server {
                if let Some(tun) = tun {
                    panel.note_server_tun(tun);
                } else {
                    panel.note_server(*reachable);
                }
            }
        }
        if let Some(tun) = &self.tun_edge {
            ui.report_server_tun(tun);
        }
        if let Some(network_up) = self.network_edge {
            if network_up {
                ui.report_network_restored();
            } else {
                ui.report_unreachable();
            }
        }
    }
}

fn spawn_dial(
    stage: DialStage,
    plan: &DialPlan,
    timeout: Duration,
    results: mpsc::Sender<DialResult>,
) {
    let plan = plan.clone();
    std::thread::spawn(move || {
        let tun = route_tun(&plan);
        let _ = results.send(DialResult {
            stage,
            reachable: tun.is_some() || dial_with_timeout(&plan, timeout),
            tun,
        });
    });
}

fn spawn_internet_probe(probe: &InternetProbe, results: mpsc::Sender<DialResult>) {
    let url = probe.url().to_owned();
    std::thread::spawn(move || {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(INTERNET_PROBE_TIMEOUT))
            .build()
            .new_agent();
        let reachable = agent
            .get(&url)
            .call()
            .is_ok_and(|response| response.status().as_u16() == 204);
        let _ = results.send(DialResult {
            stage: DialStage::Internet,
            reachable,
            tun: None,
        });
    });
}

struct MasterAttempt {
    child: Option<Child>,
    stderr: Option<std::thread::JoinHandle<String>>,
    control_path: PathBuf,
    remove_control_path_on_drop: bool,
}

#[derive(Default)]
enum MasterState {
    #[default]
    Idle,
    Connecting {
        attempt: MasterAttempt,
        started: Instant,
        next_control_check: Instant,
    },
    Ready {
        attempt: MasterAttempt,
        release_at: Duration,
    },
}

impl MasterState {
    fn connecting(attempt: MasterAttempt, now: Instant) -> Self {
        Self::Connecting {
            attempt,
            started: now,
            next_control_check: now,
        }
    }

    fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    fn in_flight(&self) -> bool {
        !self.is_idle()
    }

    fn advance(
        self,
        now: Instant,
        wait_elapsed: Duration,
        plan: &SshAttachPlan,
        control_path: &Path,
        deadline: Duration,
        panel: &mut RecoveryPanel,
    ) -> Result<MasterTick> {
        match self {
            Self::Idle => Ok(MasterTick::Pending(Self::Idle)),
            Self::Connecting {
                mut attempt,
                started,
                mut next_control_check,
            } => {
                if let Some(status) = attempt.try_wait().context("polling SSH ControlMaster")? {
                    return Ok(MasterTick::Failed(
                        failed_master_summary(attempt, status),
                        true,
                    ));
                }
                if now >= next_control_check {
                    if control_check_spec(plan.target(), control_path)
                        .run_with_timeout(CONTROL_MASTER_CHECK_TIMEOUT)
                        .is_ok()
                    {
                        panel.note_master_ready();
                        let release_at = panel.release_at(wait_elapsed);
                        if wait_elapsed >= release_at {
                            return Ok(MasterTick::Connected(attempt.into_guard()));
                        }
                        return Ok(MasterTick::Pending(Self::Ready {
                            attempt,
                            release_at,
                        }));
                    }
                    next_control_check = now + CONTROL_MASTER_CHECK_INTERVAL;
                }
                if now.saturating_duration_since(started) >= deadline {
                    drop(attempt);
                    return Ok(MasterTick::Failed(
                        Some(format!(
                            "SSH connect timed out after {}s",
                            deadline.as_secs()
                        )),
                        false,
                    ));
                }
                Ok(MasterTick::Pending(Self::Connecting {
                    attempt,
                    started,
                    next_control_check,
                }))
            }
            Self::Ready {
                mut attempt,
                release_at,
            } => {
                if let Some(status) = attempt.try_wait().context("polling SSH ControlMaster")? {
                    return Ok(MasterTick::Failed(
                        failed_master_summary(attempt, status),
                        true,
                    ));
                }
                if wait_elapsed >= release_at {
                    return Ok(MasterTick::Connected(attempt.into_guard()));
                }
                Ok(MasterTick::Pending(Self::Ready {
                    attempt,
                    release_at,
                }))
            }
        }
    }
}

enum MasterTick {
    Pending(MasterState),
    Failed(Option<String>, bool),
    Connected(MasterGuard),
}

fn failed_master_summary(
    attempt: MasterAttempt,
    status: std::process::ExitStatus,
) -> Option<String> {
    let stderr = attempt.finish_failed();
    rimz::remote::ssh_error_summary(&stderr).or_else(|| Some(status.to_string()))
}

impl MasterAttempt {
    fn spawn(spec: rimz::mux::CommandSpec, control_path: PathBuf) -> std::io::Result<Self> {
        let mut child = spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = drain_stderr(&mut child);
        Ok(Self {
            child: Some(child),
            stderr,
            control_path,
            remove_control_path_on_drop: true,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.as_mut().map_or(Ok(None), Child::try_wait)
    }

    fn finish_failed(mut self) -> String {
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        remove_control_path(&self.control_path);
        self.remove_control_path_on_drop = false;
        self.stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    }

    fn into_guard(mut self) -> MasterGuard {
        let guard = MasterGuard {
            child: self.child.take(),
            stderr: self.stderr.take(),
            control_path: self.control_path.clone(),
        };
        self.remove_control_path_on_drop = false;
        guard
    }

    fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
        if self.remove_control_path_on_drop {
            remove_control_path(&self.control_path);
        }
    }
}

fn drain_stderr(child: &mut Child) -> Option<std::thread::JoinHandle<String>> {
    child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut output = String::new();
            let _ = stderr.read_to_string(&mut output);
            output
        })
    })
}

impl Drop for MasterAttempt {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MasterGuard {
    child: Option<Child>,
    stderr: Option<std::thread::JoinHandle<String>>,
    control_path: PathBuf,
}

impl MasterGuard {
    fn is_running(&mut self) -> Result<bool> {
        self.child
            .as_mut()
            .context("SSH ControlMaster is not running")?
            .try_wait()
            .context("polling SSH ControlMaster")
            .map(|status| status.is_none())
    }

    /// Wait for the owning ControlMaster. Master-owned forwards remain live
    /// until this child exits, so remote web uses this as its foreground wait.
    fn wait_for_exit(&mut self) -> Result<Option<i32>> {
        let mut child = self
            .child
            .take()
            .context("SSH ControlMaster is not running")?;
        let status = match child.wait() {
            Ok(status) => status,
            Err(err) => {
                self.child = Some(child);
                return Err(err).context("waiting for SSH ControlMaster");
            }
        };
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
        remove_control_path(&self.control_path);
        Ok(status.code())
    }
}

impl Drop for MasterGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
        remove_control_path(&self.control_path);
    }
}

fn sleep_retry_wait(duration: Duration, stop: Option<&AtomicBool>) {
    if let Some(stop) = stop {
        super::sleep_interruptibly(duration, stop);
    } else {
        std::thread::sleep(duration);
    }
}

fn recv_link_event(events: &mpsc::Receiver<LinkEvent>, poll: Duration) -> Option<LinkEvent> {
    match events.recv_timeout(poll) {
        Ok(event) => Some(event),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::thread::sleep(poll);
            None
        }
    }
}

fn render_session_link_action(host: &str, action: SessionLinkAction) {
    match action {
        SessionLinkAction::NotifyBlackout(duration) => {
            emit_local_link_notification(
                rimz::sidebar::notify::NotificationKind::LinkLost,
                "RimZ: remote link stalled",
                &format!(
                    "No probe ack from {host} for {}s.",
                    duration.as_secs().max(1)
                ),
                LocalLinkNotificationDelivery::TerminalOnly,
            );
        }
        SessionLinkAction::NotifyTransportLoss => emit_local_link_notification(
            rimz::sidebar::notify::NotificationKind::LinkLost,
            "RimZ: remote link lost",
            &format!("SSH to {host} dropped; reconnecting."),
            LocalLinkNotificationDelivery::TerminalAndCommand,
        ),
        SessionLinkAction::Restore => emit_local_link_notification(
            rimz::sidebar::notify::NotificationKind::LinkRestored,
            "RimZ: remote link restored",
            &format!("SSH to {host} is responsive again."),
            LocalLinkNotificationDelivery::TerminalAndCommand,
        ),
        SessionLinkAction::VerifyZombie => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalLinkNotificationDelivery {
    TerminalOnly,
    TerminalAndCommand,
}

impl LocalLinkNotificationDelivery {
    fn allows_command(self) -> bool {
        matches!(self, Self::TerminalAndCommand)
    }
}

fn emit_local_link_notification(
    kind: rimz::sidebar::notify::NotificationKind,
    title: &str,
    body: &str,
    delivery: LocalLinkNotificationDelivery,
) {
    let prefs = rimz::config::MachineConfig::load_lenient()
        .notifications
        .clone();
    let bytes = local_link_terminal_notification_bytes(
        title,
        body,
        &prefs,
        std::io::stderr().is_terminal(),
    );
    if !bytes.is_empty() {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(&bytes);
        let _ = stderr.flush();
    }
    let Some(notification) = local_link_command_notification(kind, title, body, delivery, &prefs)
    else {
        return;
    };
    rimz::sidebar::notify::spawn_notify_handlers(&prefs, &notification);
}

fn local_link_terminal_notification_bytes(
    title: &str,
    body: &str,
    prefs: &rimz::config::NotificationsPrefs,
    stderr_is_terminal: bool,
) -> Vec<u8> {
    if stderr_is_terminal {
        rimz::osc::local_terminal_notification_bytes(prefs, title, body)
    } else {
        Vec::new()
    }
}

fn local_link_command_notification(
    kind: rimz::sidebar::notify::NotificationKind,
    title: &str,
    body: &str,
    delivery: LocalLinkNotificationDelivery,
    prefs: &rimz::config::NotificationsPrefs,
) -> Option<rimz::sidebar::notify::Notification> {
    if !prefs.enabled || !delivery.allows_command() || !prefs.has_handlers() {
        return None;
    }
    Some(rimz::sidebar::notify::Notification {
        agents: Vec::new(),
        notification_kind: kind,
        title: title.to_owned(),
        body: body.to_owned(),
        unread_count: None,
    })
}

fn prepare_control_path(path: &Path) -> Result<()> {
    let link_dir = path
        .parent()
        .with_context(|| format!("SSH control path {} has no parent", path.display()))?;
    let rimz_dir = link_dir
        .parent()
        .with_context(|| format!("SSH control directory {} has no parent", link_dir.display()))?;
    let runtime_dir = rimz_dir
        .parent()
        .with_context(|| format!("SSH control directory {} has no parent", rimz_dir.display()))?;
    rimz::store::paths::ensure_private_runtime_dir(runtime_dir)?;
    rimz::store::paths::ensure_private_runtime_dir(rimz_dir)?;
    rimz::store::paths::ensure_private_runtime_dir(link_dir)?;
    remove_control_path(path);
    Ok(())
}

fn remove_control_path(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "remove SSH control socket failed")
        }
    }
}

struct ProbeHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    control_path: Option<PathBuf>,
    remove_control_path: bool,
}

impl ProbeHandle {
    fn start(
        target: RemoteTarget,
        control_path: PathBuf,
        events: mpsc::Sender<LinkEvent>,
        port_sync: Option<Arc<Mutex<PortSync>>>,
    ) -> Self {
        if let Err(err) = prepare_control_path(&control_path) {
            tracing::debug!(
                path = %control_path.display(),
                error = %err,
                "ControlMaster unavailable; continuing without link probes"
            );
            return Self::disabled();
        }
        spawn_probe_loop(target, control_path, events, port_sync, false, true)
    }

    fn start_preestablished(
        target: RemoteTarget,
        control_path: PathBuf,
        events: mpsc::Sender<LinkEvent>,
        port_sync: Option<Arc<Mutex<PortSync>>>,
    ) -> Self {
        spawn_probe_loop(target, control_path, events, port_sync, true, false)
    }

    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
            control_path: None,
            remove_control_path: false,
        }
    }

    fn attach_spec(&self, attempt: &SshAttachAttempt<'_>) -> rimz::mux::CommandSpec {
        match &self.control_path {
            Some(path) => attempt.control(path),
            None => attempt.plain(),
        }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if self.remove_control_path
            && let Some(path) = &self.control_path
        {
            remove_control_path(path);
        }
    }
}

impl Drop for ProbeHandle {
    fn drop(&mut self) {
        self.finish();
    }
}

fn spawn_probe_loop(
    target: RemoteTarget,
    control_path: PathBuf,
    events: mpsc::Sender<LinkEvent>,
    port_sync: Option<Arc<Mutex<PortSync>>>,
    control_confirmed: bool,
    remove_control_path: bool,
) -> ProbeHandle {
    let Some(interval) = probe_interval_from_env() else {
        return ProbeHandle {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
            control_path: Some(control_path),
            remove_control_path,
        };
    };
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_path = control_path.clone();
    let join = std::thread::spawn(move || {
        probe_loop(
            target,
            thread_path,
            interval,
            events,
            thread_stop,
            port_sync,
            control_confirmed,
        );
    });
    ProbeHandle {
        stop,
        join: Some(join),
        control_path: Some(control_path),
        remove_control_path,
    }
}

fn wait_for_control_master(target: &RemoteTarget, control_path: &Path, stop: &AtomicBool) -> bool {
    while !stop.load(Ordering::Relaxed) {
        if control_check_spec(target, control_path)
            .run_with_timeout(CONTROL_MASTER_CHECK_TIMEOUT)
            .is_ok()
        {
            return true;
        }
        super::sleep_interruptibly(CONTROL_MASTER_CHECK_INTERVAL, stop);
    }
    false
}

fn probe_loop(
    target: RemoteTarget,
    control_path: PathBuf,
    interval: Duration,
    events: mpsc::Sender<LinkEvent>,
    stop: Arc<AtomicBool>,
    port_sync: Option<Arc<Mutex<PortSync>>>,
    mut control_confirmed: bool,
) {
    let mut monitor =
        LinkMonitor::with_timeout(probe_timeout_from_env(), blackout_after_from_env());
    let mut failures = 0u32;
    let mut reopened = false;
    while !stop.load(Ordering::Relaxed) {
        if !control_confirmed && !wait_for_control_master(&target, &control_path, &stop) {
            return;
        }
        control_confirmed = false;
        if !reopened {
            if let Some(sync) = port_sync.as_ref() {
                reopen_port_forwards(sync, &target, &control_path);
            }
            reopened = true;
        }
        match run_probe_stream(
            &target,
            &control_path,
            interval,
            &events,
            &stop,
            &mut monitor,
            port_sync.as_ref(),
        ) {
            ProbeStreamExit::Stopped | ProbeStreamExit::VersionSkew => return,
            ProbeStreamExit::Ended { acked } => {
                let respawn_delay = if acked {
                    failures = 0;
                    PROBE_RESPAWN_BACKOFF_MIN
                } else {
                    let delay = rimz::remote::backoff(
                        failures,
                        PROBE_RESPAWN_BACKOFF_MIN,
                        PROBE_RESPAWN_BACKOFF_MAX,
                    );
                    failures = failures.saturating_add(1);
                    delay
                };
                if failures >= PROBE_STREAM_BLACKOUT_FAILURES
                    && let Some(event) =
                        monitor.check_blackout(rimz::sidebar::timing::unix_now_ms())
                {
                    let _ = events.send(event);
                }
                super::sleep_interruptibly(respawn_delay, &stop);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeStreamExit {
    Ended { acked: bool },
    Stopped,
    VersionSkew,
}

struct ProbeChild {
    child: Child,
    stdin: ChildStdin,
    acknowledgements: mpsc::Receiver<LinkAck>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl ProbeChild {
    fn spawn(target: &RemoteTarget, control_path: &Path) -> std::io::Result<Self> {
        let mut child = probe_stream_spec(target, control_path)
            .to_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("probe stream missing stdout"));
        };
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("probe stream missing stdin"));
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                let Ok(ack) = serde_json::from_str::<LinkAck>(&line) else {
                    continue;
                };
                if ack.version_ok() {
                    let _ = ack_tx.send(ack);
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            acknowledgements: ack_rx,
            reader: Some(reader),
        })
    }

    fn drain_acknowledgements(
        &self,
        monitor: &mut LinkMonitor,
        events: &mpsc::Sender<LinkEvent>,
        target: &RemoteTarget,
        control_path: &Path,
        port_sync: Option<&Arc<Mutex<PortSync>>>,
    ) -> ProbeAckDrain {
        let mut drain = ProbeAckDrain::default();
        for ack in self.acknowledgements.try_iter() {
            let outcome = monitor.record_ack(ack.seq, rimz::sidebar::timing::unix_now_ms());
            drain.acked |= outcome.accepted;
            drain.reported_rtt_changed |= outcome.reported_rtt_changed;
            if let (Some(sync), Some(ports)) = (port_sync, ack.ports) {
                apply_port_report(sync, &ports, target, control_path);
            }
            for event in outcome.events {
                let _ = events.send(event);
            }
        }
        drain
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[derive(Default)]
struct ProbeAckDrain {
    acked: bool,
    reported_rtt_changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeStreamStop {
    Ended,
    Stopped,
    VersionSkew,
}

fn run_probe_stream(
    target: &RemoteTarget,
    control_path: &Path,
    interval: Duration,
    events: &mpsc::Sender<LinkEvent>,
    stop: &AtomicBool,
    monitor: &mut LinkMonitor,
    port_sync: Option<&Arc<Mutex<PortSync>>>,
) -> ProbeStreamExit {
    let mut child = match ProbeChild::spawn(target, control_path) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::debug!(error = %err, "remote link probe stream spawn failed");
            return ProbeStreamExit::Ended { acked: false };
        }
    };
    monitor.begin_stream();
    let mut next_tick = Instant::now();
    let mut acked = false;
    let reason = loop {
        if stop.load(Ordering::Relaxed) {
            break ProbeStreamStop::Stopped;
        }
        match child.child.try_wait() {
            Ok(Some(status)) => {
                if matches!(
                    status.code(),
                    Some(rimz::remote::REMOTE_RIMZ_MISSING_EXIT | 2)
                ) {
                    break ProbeStreamStop::VersionSkew;
                }
                break ProbeStreamStop::Ended;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(error = %err, "remote link probe stream poll failed");
                break ProbeStreamStop::Ended;
            }
        }

        let drain = child.drain_acknowledgements(monitor, events, target, control_path, port_sync);
        acked |= drain.acked;
        if drain.reported_rtt_changed {
            let probe = monitor.stats_refresh_probe(rimz::sidebar::timing::unix_now_ms());
            if write_link_probe(&mut child.stdin, &probe).is_err() {
                break ProbeStreamStop::Ended;
            }
        }
        if let Some(event) = monitor.check_blackout(rimz::sidebar::timing::unix_now_ms()) {
            let _ = events.send(event);
        }

        if Instant::now() >= next_tick {
            let probe = monitor.next_probe(rimz::sidebar::timing::unix_now_ms());
            if write_link_probe(&mut child.stdin, &probe).is_err() {
                break ProbeStreamStop::Ended;
            }
            next_tick = Instant::now() + interval;
        }
        sleep_until_next_tick(next_tick, stop);
    };
    child.shutdown();

    match reason {
        ProbeStreamStop::Stopped => ProbeStreamExit::Stopped,
        ProbeStreamStop::VersionSkew => ProbeStreamExit::VersionSkew,
        ProbeStreamStop::Ended => {
            acked |= child
                .drain_acknowledgements(monitor, events, target, control_path, port_sync)
                .acked;
            ProbeStreamExit::Ended { acked }
        }
    }
}

fn apply_port_report(
    sync: &Mutex<PortSync>,
    ports: &[u16],
    target: &RemoteTarget,
    control_path: &Path,
) {
    let actions = match sync.lock() {
        Ok(mut sync) => sync.observe(ports),
        Err(poisoned) => poisoned.into_inner().observe(ports),
    };
    apply_port_actions(sync, actions, target, control_path);
}

fn reopen_port_forwards(sync: &Mutex<PortSync>, target: &RemoteTarget, control_path: &Path) {
    let actions = match sync.lock() {
        Ok(sync) => sync.reopen_active(),
        Err(poisoned) => poisoned.into_inner().reopen_active(),
    };
    apply_port_actions(sync, actions, target, control_path);
}

fn apply_port_actions(
    sync: &Mutex<PortSync>,
    actions: Vec<PortAction>,
    target: &RemoteTarget,
    control_path: &Path,
) {
    for action in actions {
        match action {
            PortAction::Open(port) => {
                let listener = match TcpListener::bind(("127.0.0.1", port)) {
                    Ok(listener) => listener,
                    Err(error) => {
                        mark_port_open_failed(sync, port);
                        tracing::info!(port, %error, "remote port auto-forward parked; local port unavailable");
                        continue;
                    }
                };
                drop(listener);
                match open_spec(target, control_path, port)
                    .run_with_timeout(AUTO_FORWARD_CONTROL_TIMEOUT)
                {
                    Ok(_) => tracing::info!(port, "remote port auto-forward opened"),
                    Err(error) => {
                        mark_port_open_failed(sync, port);
                        tracing::warn!(port, %error, "remote port auto-forward open refused");
                    }
                }
            }
            PortAction::Close(port) => {
                match cancel_spec(target, control_path, port)
                    .run_with_timeout(AUTO_FORWARD_CONTROL_TIMEOUT)
                {
                    Ok(_) => tracing::info!(port, "remote port auto-forward closed"),
                    Err(error) => {
                        tracing::warn!(port, %error, "remote port auto-forward close refused")
                    }
                }
            }
        }
    }
}

fn mark_port_open_failed(sync: &Mutex<PortSync>, port: u16) {
    match sync.lock() {
        Ok(mut sync) => sync.mark_open_failed(port),
        Err(poisoned) => poisoned.into_inner().mark_open_failed(port),
    }
}

fn write_link_probe(stdin: &mut impl Write, probe: &LinkProbe) -> std::io::Result<()> {
    serde_json::to_writer(&mut *stdin, probe).map_err(std::io::Error::other)?;
    writeln!(stdin)?;
    stdin.flush()
}

fn sleep_until_next_tick(next_tick: Instant, stop: &AtomicBool) {
    let now = Instant::now();
    let until_tick = next_tick.saturating_duration_since(now);
    super::sleep_interruptibly(until_tick.min(Duration::from_millis(50)), stop);
}

pub(super) fn print_remote_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", rimz::remote::display_ssh_command(spec));
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
