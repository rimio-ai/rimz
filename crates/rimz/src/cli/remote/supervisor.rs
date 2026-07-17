use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use rimz::remote::link::{
    LinkAck, LinkEvent, LinkMonitor, LinkProbe, SessionLinkAction, SessionLinkState,
    blackout_after_from_env, control_check_spec, probe_interval_from_env, probe_stream_spec,
    probe_timeout_from_env,
};
use rimz::remote::reachability::{
    AttemptPacer, DIAL_TIMEOUT, DialPlan, dial_interval_from_env, parse_dial_plan,
    ssh_config_query_spec,
};
use rimz::remote::recovery::{
    INTERNET_PROBE_TIMEOUT, InternetProbe, RecoveryPanel, internet_probe_from_env,
};
use rimz::remote::{RemoteTarget, SshAttachAttempt, SshAttachPlan};

use super::outage_ui::{OutageUi, PANEL_TICK, UiEvent};

const CONTROL_MASTER_CHECK_INTERVAL: Duration = Duration::from_millis(200);
const CONTROL_MASTER_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_STREAM_BLACKOUT_FAILURES: u32 = 3;
const PROBE_RESPAWN_BACKOFF_MIN: Duration = Duration::from_secs(1);
const PROBE_RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const SSH_CONFIG_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

struct OutageState {
    started: Instant,
    internet_probe: Option<InternetProbe>,
    panel: RecoveryPanel,
    attempts: u32,
}

impl OutageState {
    fn new(host: &str, internet_probe: Option<InternetProbe>, server: Option<&DialPlan>) -> Self {
        let mut panel = RecoveryPanel::new(host, internet_probe.as_ref(), server);
        panel.note_attempt(1);
        Self {
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

pub(super) fn supervise_remote(
    plan: &SshAttachPlan,
    control_path: &Path,
    setup_hint: &str,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, ReconnectState, Verdict};

    let policy = ReconnectPolicy::from_env();
    let mut reconnect = ReconnectState::new();
    let target = plan.target();
    let host = target.host_display();
    let dial_plan = resolve_dial_plan(target.ssh_destination().as_str());
    let zombie_interval = dial_plan.as_ref().and_then(|_| dial_interval_from_env());
    let mut session_link = SessionLinkState::new(policy.gatetime, zombie_interval);
    let stop = AtomicBool::new(false);
    let mut first_attempt = true;
    let mut outage = None;
    let mut ready_master = None;
    report_remote_connect(host, true);
    let guard = super::tty::TtyGuard::acquire();
    loop {
        let (events_tx, events_rx) = mpsc::channel();
        let confirmed_master = ready_master.is_some();
        let probe = if confirmed_master {
            ProbeHandle::start_preestablished(target.clone(), control_path.to_path_buf(), events_tx)
        } else {
            ProbeHandle::start(target.clone(), control_path.to_path_buf(), events_tx)
        };
        let attempt = if first_attempt {
            plan.initial()
        } else {
            plan.retry()
        };
        first_attempt = false;
        let spec = probe.attach_spec(&attempt);
        let mut outcome = run_ssh_session(
            &spec,
            host,
            &events_rx,
            &mut session_link,
            dial_plan.as_ref(),
        )?;
        outcome.established |= confirmed_master;
        guard.restore();
        drop(probe);
        drop(ready_master.take());
        if outcome.established {
            outage = None;
        }
        if outcome.killed_zombie {
            guard.reset_emulator();
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: link to {host} confirmed dead — host reachable, session silent; reconnecting now",
            );
            reconnect.settle_zombie_kill();
            let mut ui = OutageUi::auto(host);
            let outage = outage.get_or_insert_with(|| {
                OutageState::new(host, internet_probe_for_wait(&ui), dial_plan.as_ref())
            });
            match reconnect_in_background(
                plan,
                control_path,
                dial_plan.as_ref(),
                &policy,
                outage,
                &mut ui,
                Some(&stop),
            )? {
                WaitOutcome::Connected(master) => ready_master = Some(master),
                WaitOutcome::Interrupted => {
                    let _ = writeln!(std::io::stderr().lock(), "rimz: reconnect stopped");
                    return Ok(());
                }
            }
            continue;
        }
        match reconnect.settle(outcome.status.code(), outcome.established) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => {
                guard.reset_emulator();
                bail!("{}", fatal_session_message(code, host, setup_hint))
            }
            Verdict::Retry => {
                guard.reset_emulator();
                let mut ui = OutageUi::auto(host);
                let outage = outage.get_or_insert_with(|| {
                    OutageState::new(host, internet_probe_for_wait(&ui), dial_plan.as_ref())
                });
                if ui.is_plain() {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "rimz: link to {host} lost — reconnecting in the background; Ctrl-C stops",
                    );
                }
                if let Some(action) = session_link.transport_lost() {
                    render_session_link_action(host, action);
                }
                match reconnect_in_background(
                    plan,
                    control_path,
                    dial_plan.as_ref(),
                    &policy,
                    outage,
                    &mut ui,
                    Some(&stop),
                )? {
                    WaitOutcome::Connected(master) => ready_master = Some(master),
                    WaitOutcome::Interrupted => {
                        let _ = writeln!(std::io::stderr().lock(), "rimz: reconnect stopped",);
                        return Ok(());
                    }
                }
            }
        }
    }
}

pub(super) fn fatal_session_message(code: i32, host: &str, setup_hint: &str) -> String {
    if code == rimz::remote::REMOTE_RIMZ_MISSING_EXIT {
        format!(
            "rimz is not installed on {host}; install it over SSH with:\n    \
             rimz remote setup {setup_hint}"
        )
    } else {
        format!(
            "ssh to {host} exited with status {code}; not reconnecting \
             (only a dropped link on an established session is retried)"
        )
    }
}

fn run_ssh_session(
    spec: &rimz::mux::CommandSpec,
    host: &str,
    events: &mpsc::Receiver<LinkEvent>,
    link: &mut SessionLinkState,
    dial_plan: Option<&DialPlan>,
) -> Result<SessionOutcome> {
    let mut child = spec
        .to_command()
        .spawn()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
    let started = Instant::now();
    let mut update = link.begin_session();
    loop {
        if let Some(status) = child.try_wait().context("polling ssh session")? {
            let established = link.finish(started.elapsed(), events.try_iter());
            return Ok(SessionOutcome {
                status,
                established,
                killed_zombie: false,
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
                    return Ok(outcome);
                }
            } else {
                render_session_link_action(host, *action);
            }
        }
    }
}

fn verify_zombie(
    child: &mut Child,
    dial_plan: Option<&DialPlan>,
) -> Result<Option<SessionOutcome>> {
    let Some(plan) = dial_plan else {
        return Ok(None);
    };
    if !dial(plan) {
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
                killed_zombie: true,
            }))
        }
        Err(kill_err) => match child.try_wait().context("polling raced ssh session")? {
            Some(status) => Ok(Some(SessionOutcome {
                status,
                established: true,
                killed_zombie: false,
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
    killed_zombie: bool,
}

pub(super) fn resolve_dial_plan(destination: &str) -> Option<DialPlan> {
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

pub(super) enum WaitOutcome {
    Connected(MasterGuard),
    Interrupted,
}

fn internet_probe_for_wait(ui: &OutageUi) -> Option<InternetProbe> {
    if ui.is_plain() {
        None
    } else {
        internet_probe_from_env()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlainWaitOutcome {
    AttemptNow,
    Interrupted,
}

pub(super) fn wait_for_plain_attempt(
    dial_plan: Option<&DialPlan>,
    policy: &rimz::remote::ReconnectPolicy,
    host: &str,
    stop: Option<&AtomicBool>,
) -> PlainWaitOutcome {
    let started = Instant::now();
    let interval = dial_interval_from_env();
    let mut pacer = AttemptPacer::new(
        *policy,
        started,
        false,
        dial_plan.is_some() && interval.is_some(),
    );
    pacer.note_attempt_failed(started);
    let mut next_dial = started;
    let (dial_tx, dial_rx) = mpsc::channel::<DialResult>();
    let mut server_pending = false;
    let mut last_network_up = pacer.network_up();
    let ui = OutageUi::plain_lines(host);
    loop {
        let now = Instant::now();
        for result in dial_rx.try_iter() {
            server_pending = false;
            pacer.note_server(result.reachable, now);
        }
        let network_up = pacer.network_up();
        if network_up != last_network_up {
            if network_up {
                ui.report_network_restored();
            } else {
                ui.report_unreachable();
            }
            last_network_up = network_up;
        }
        if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
            return PlainWaitOutcome::Interrupted;
        }
        if interval.is_some_and(|_| now >= next_dial) {
            if let Some(plan) = dial_plan
                && !server_pending
            {
                spawn_dial(DialStage::Server, plan, DIAL_TIMEOUT, dial_tx.clone());
                server_pending = true;
            }
            next_dial = now + interval.unwrap_or_default();
        }
        if pacer.may_attempt(now) {
            return PlainWaitOutcome::AttemptNow;
        }
        sleep_retry_wait(PANEL_TICK, stop);
    }
}

fn reconnect_in_background(
    plan: &SshAttachPlan,
    control_path: &Path,
    dial_plan: Option<&DialPlan>,
    policy: &rimz::remote::ReconnectPolicy,
    outage: &mut OutageState,
    ui: &mut OutageUi,
    stop: Option<&AtomicBool>,
) -> Result<WaitOutcome> {
    let started = Instant::now();
    outage.panel.begin_wait();
    let interval = dial_interval_from_env();
    let mut pacer = AttemptPacer::new(
        *policy,
        started,
        outage.internet_probe.is_some() && interval.is_some(),
        dial_plan.is_some() && interval.is_some(),
    );
    pacer.note_attempt_failed(started);
    let mut next_dial = started;
    let (dial_tx, dial_rx) = mpsc::channel::<DialResult>();
    let mut internet_pending = false;
    let mut server_pending = false;
    let mut master: Option<MasterAttempt> = None;
    let mut master_ready_release = None;
    let mut next_control_check = started;
    let mut last_network_up = pacer.network_up();
    let mut last_reported_error = None;
    loop {
        let now = Instant::now();
        for result in dial_rx.try_iter() {
            match result.stage {
                DialStage::Internet => {
                    internet_pending = false;
                    outage.panel.note_internet(result.reachable);
                    pacer.note_internet(result.reachable, now);
                }
                DialStage::Server => {
                    server_pending = false;
                    outage.panel.note_server(result.reachable);
                    pacer.note_server(result.reachable, now);
                }
            }
        }
        let network_up = pacer.network_up();
        if network_up != last_network_up {
            if network_up {
                ui.report_network_restored();
            } else {
                ui.report_unreachable();
            }
            last_network_up = network_up;
        }
        if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
            drop(master.take());
            ui.release()?;
            return Ok(WaitOutcome::Interrupted);
        }

        if interval.is_some_and(|_| now >= next_dial) {
            pacer.note_fingerprint(network_fingerprint(), now);
            if let Some(probe) = &outage.internet_probe
                && !internet_pending
            {
                spawn_internet_probe(probe, dial_tx.clone());
                internet_pending = true;
            }
            if let Some(plan) = dial_plan
                && !server_pending
            {
                spawn_dial(DialStage::Server, plan, DIAL_TIMEOUT, dial_tx.clone());
                server_pending = true;
            }
            next_dial = now + interval.unwrap_or_default();
        }

        if let Some(attempt) = &mut master {
            if let Some(status) = attempt.try_wait().context("polling SSH ControlMaster")? {
                let stderr = master
                    .take()
                    .expect("master attempt exists")
                    .finish_failed();
                let summary =
                    rimz::remote::ssh_error_summary(&stderr).or_else(|| Some(status.to_string()));
                outage.panel.note_ssh_error(summary.clone());
                outage.note_next_attempt();
                master_ready_release = None;
                if summary != last_reported_error {
                    ui.report_attempt_failed(summary.as_deref());
                    last_reported_error = summary;
                }
                pacer.note_attempt_failed(now);
            } else if master_ready_release.is_none() && now >= next_control_check {
                if control_check_spec(plan.target(), control_path)
                    .run_with_timeout(CONTROL_MASTER_CHECK_TIMEOUT)
                    .is_ok()
                {
                    master_ready_release = Some(outage.panel.release_at(started.elapsed()));
                }
                next_control_check = now + CONTROL_MASTER_CHECK_INTERVAL;
            }
        }

        if master_ready_release.is_some_and(|release_at| started.elapsed() >= release_at) {
            let master = master.take().expect("ready master exists").into_guard();
            ui.release()?;
            ui.report_reattached();
            return Ok(WaitOutcome::Connected(master));
        }

        if master.is_none() && pacer.may_attempt(now) {
            if let Err(err) = prepare_control_path(control_path) {
                let summary = Some(format!("preparing SSH control socket: {err}"));
                outage.panel.note_ssh_error(summary.clone());
                outage.note_next_attempt();
                if summary != last_reported_error {
                    ui.report_attempt_failed(summary.as_deref());
                    last_reported_error = summary;
                }
                pacer.note_attempt_failed(now);
            } else {
                outage.begin_attempt();
                outage.panel.session_starting();
                pacer.begin_attempt(now);
                match MasterAttempt::spawn(plan.master(control_path), control_path.to_path_buf()) {
                    Ok(attempt) => {
                        master = Some(attempt);
                        next_control_check = now;
                    }
                    Err(err) => {
                        let summary = Some(format!("starting SSH: {err}"));
                        outage.panel.note_ssh_error(summary.clone());
                        outage.note_next_attempt();
                        if summary != last_reported_error {
                            ui.report_attempt_failed(summary.as_deref());
                            last_reported_error = summary;
                        }
                        pacer.note_attempt_failed(now);
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
            pacer.footer(now, master.is_some()),
        )? == UiEvent::Interrupted
        {
            drop(master.take());
            ui.release()?;
            return Ok(WaitOutcome::Interrupted);
        }
        sleep_retry_wait(PANEL_TICK, stop);
    }
}

fn network_fingerprint() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:443").ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

#[derive(Clone, Copy)]
enum DialStage {
    Internet,
    Server,
}

struct DialResult {
    stage: DialStage,
    reachable: bool,
}

fn spawn_dial(
    stage: DialStage,
    plan: &DialPlan,
    timeout: Duration,
    results: mpsc::Sender<DialResult>,
) {
    let plan = plan.clone();
    std::thread::spawn(move || {
        let _ = results.send(DialResult {
            stage,
            reachable: dial_with_timeout(&plan, timeout),
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
        });
    });
}

struct MasterAttempt {
    child: Option<Child>,
    stderr: Option<std::thread::JoinHandle<String>>,
    control_path: PathBuf,
    remove_control_path_on_drop: bool,
}

impl MasterAttempt {
    fn spawn(spec: rimz::mux::CommandSpec, control_path: PathBuf) -> std::io::Result<Self> {
        let mut child = spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut output = String::new();
                let _ = stderr.read_to_string(&mut output);
                output
            })
        });
        Ok(Self {
            child: Some(child),
            stderr,
            control_path,
            remove_control_path_on_drop: true,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.as_mut().expect("master child exists").try_wait()
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

impl Drop for MasterAttempt {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) struct MasterGuard {
    child: Option<Child>,
    stderr: Option<std::thread::JoinHandle<String>>,
    control_path: PathBuf,
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
    fn start(target: RemoteTarget, control_path: PathBuf, events: mpsc::Sender<LinkEvent>) -> Self {
        if let Err(err) = prepare_control_path(&control_path) {
            tracing::debug!(
                path = %control_path.display(),
                error = %err,
                "ControlMaster unavailable; continuing without link probes"
            );
            return Self::disabled();
        }
        spawn_probe_loop(target, control_path, events, false, true)
    }

    fn start_preestablished(
        target: RemoteTarget,
        control_path: PathBuf,
        events: mpsc::Sender<LinkEvent>,
    ) -> Self {
        spawn_probe_loop(target, control_path, events, true, false)
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
    mut control_confirmed: bool,
) {
    let mut monitor =
        LinkMonitor::with_timeout(probe_timeout_from_env(), blackout_after_from_env());
    let mut failures = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if !control_confirmed && !wait_for_control_master(&target, &control_path, &stop) {
            return;
        }
        control_confirmed = false;
        match run_probe_stream(
            &target,
            &control_path,
            interval,
            &events,
            &stop,
            &mut monitor,
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
    acknowledgements: mpsc::Receiver<u64>,
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
                    let _ = ack_tx.send(ack.seq);
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
    ) -> ProbeAckDrain {
        let mut drain = ProbeAckDrain::default();
        for seq in self.acknowledgements.try_iter() {
            let outcome = monitor.record_ack(seq, rimz::sidebar::timing::unix_now_ms());
            drain.acked |= outcome.accepted;
            drain.reported_rtt_changed |= outcome.reported_rtt_changed;
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

        let drain = child.drain_acknowledgements(monitor, events);
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
            acked |= child.drain_acknowledgements(monitor, events).acked;
            ProbeStreamExit::Ended { acked }
        }
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

/// One stderr line before the terminal belongs to ssh, so the user knows the
/// room they are about to see is remote.
pub(super) fn report_remote_connect(host: &str, reconnect: bool) {
    let mut stderr = std::io::stderr().lock();
    let tail = if reconnect {
        " (auto-reconnect on; Ctrl-C stops)"
    } else {
        ""
    };
    let _ = writeln!(stderr, "rimz: attaching to {host} over ssh…{tail}");
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
