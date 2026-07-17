use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::net::{TcpStream, ToSocketAddrs};
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
    DIAL_TIMEOUT, DialGate, DialPlan, WaitVerdict, dial_interval_from_env, parse_dial_plan,
    ssh_config_query_spec,
};
use rimz::remote::recovery::{INTERNET_PROBE_TIMEOUT, RecoveryPanel, internet_probe_from_env};
use rimz::remote::{RemoteTarget, SshAttachAttempt, SshAttachPlan};

use super::outage_ui::{OutageUi, PANEL_TICK, UiEvent};

const CONTROL_MASTER_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const CONTROL_MASTER_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_STREAM_BLACKOUT_FAILURES: u32 = 3;
const PROBE_RESPAWN_BACKOFF_MIN: Duration = Duration::from_secs(1);
const PROBE_RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const SSH_CONFIG_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn supervise_remote(
    plan: &SshAttachPlan,
    control_path: &Path,
    setup_hint: &str,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, ReconnectState, Verdict};

    let policy = ReconnectPolicy::from_env();
    let mut reconnect = ReconnectState::new(policy);
    let target = plan.target();
    let host = target.host_display();
    let dial_plan = resolve_dial_plan(target.ssh_destination().as_str());
    let zombie_interval = dial_plan.as_ref().and_then(|_| dial_interval_from_env());
    let mut session_link = SessionLinkState::new(policy.gatetime, zombie_interval);
    let stop = AtomicBool::new(false);
    let mut first_attempt = true;
    let mut outage_started = None;
    let mut outage_waits = 0u32;
    report_remote_connect(host, true);
    let guard = super::tty::TtyGuard::acquire();
    loop {
        let (events_tx, events_rx) = mpsc::channel();
        let probe = ProbeHandle::start(target.clone(), control_path.to_path_buf(), events_tx);
        let attempt = if first_attempt {
            plan.initial()
        } else {
            plan.retry()
        };
        first_attempt = false;
        let spec = probe.attach_spec(&attempt);
        let outcome = run_ssh_session(
            &spec,
            host,
            &events_rx,
            &mut session_link,
            dial_plan.as_ref(),
        )?;
        guard.restore();
        drop(probe);
        if outcome.established {
            outage_started = None;
            outage_waits = 0;
        }
        if outcome.killed_zombie {
            guard.reset_emulator();
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: link to {host} confirmed dead — host reachable, session silent; reconnecting now",
            );
            reconnect.settle_zombie_kill();
            outage_started = Some(Instant::now());
            outage_waits = 0;
            continue;
        }
        match reconnect.settle(outcome.status.code(), outcome.established) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => {
                guard.reset_emulator();
                bail!("{}", fatal_session_message(code, host, setup_hint))
            }
            Verdict::Retry {
                delay: ladder_delay,
            } => {
                guard.reset_emulator();
                let outage_started = *outage_started.get_or_insert_with(Instant::now);
                let outage_age = outage_started.elapsed();
                let delay = retry_delay(&policy, dial_plan.is_some(), outage_age, ladder_delay);
                let first_wait = outage_waits == 0;
                outage_waits = outage_waits.saturating_add(1);
                let consecutive_failures = reconnect.consecutive_failures();
                let mut ui = OutageUi::auto(host);
                if ui.is_plain() {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "rimz: link to {host} lost — reconnecting in {}s (attempt {consecutive_failures}); Ctrl-C stops",
                        delay.as_secs(),
                    );
                }
                if let Some(action) = session_link.transport_lost() {
                    render_session_link_action(host, action);
                }
                let internet_probe = internet_probe_for_wait(&ui);
                match wait_before_retry(
                    dial_plan.as_ref(),
                    internet_probe.as_ref(),
                    delay,
                    policy.backoff_cap,
                    first_wait,
                    &mut ui,
                    Some(&stop),
                )? {
                    WaitOutcome::AttachNow { network_restored } => {
                        if network_restored {
                            reconnect.network_restored();
                            let _ = writeln!(
                                std::io::stderr().lock(),
                                "rimz: network to {host} restored — reconnecting now",
                            );
                        }
                    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitOutcome {
    AttachNow { network_restored: bool },
    Interrupted,
}

fn internet_probe_for_wait(ui: &OutageUi) -> Option<DialPlan> {
    if ui.is_plain() {
        None
    } else {
        internet_probe_from_env()
    }
}

pub(super) fn retry_delay(
    policy: &rimz::remote::ReconnectPolicy,
    has_dial_plan: bool,
    outage_age: Duration,
    ladder_delay: Duration,
) -> Duration {
    if has_dial_plan {
        policy.reachable_delay(outage_age)
    } else {
        ladder_delay
    }
}

pub(super) fn wait_before_retry(
    dial_plan: Option<&DialPlan>,
    internet_probe: Option<&DialPlan>,
    delay: Duration,
    hold: Duration,
    first_wait: bool,
    ui: &mut OutageUi,
    stop: Option<&AtomicBool>,
) -> Result<WaitOutcome> {
    let host = ui.host().to_owned();
    let started = Instant::now();
    let interval = dial_interval_from_env();
    let mut gate = dial_plan
        .zip(interval)
        .map(|_| DialGate::new(started, delay, hold));
    let mut recovery = RecoveryPanel::new(&host, internet_probe, dial_plan, first_wait);
    let mut next_dial = started;
    let mut reported_unreachable = false;
    let (dial_tx, dial_rx) = mpsc::channel::<DialResult>();
    let mut internet_pending = false;
    let mut server_pending = false;
    loop {
        let now = Instant::now();
        for result in dial_rx.try_iter() {
            match result.stage {
                DialStage::Internet => {
                    internet_pending = false;
                    recovery.note_internet(result.reachable);
                }
                DialStage::Server => {
                    server_pending = false;
                    recovery.note_server(result.reachable);
                    if let Some(gate) = &mut gate {
                        gate.note_dial(result.reachable);
                    }
                    if !result.reachable && !reported_unreachable {
                        reported_unreachable = true;
                        ui.report_unreachable();
                    }
                }
            }
        }
        if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
            ui.release(false)?;
            return Ok(WaitOutcome::Interrupted);
        }
        if ui.tick(&mut recovery, started.elapsed())? == UiEvent::Interrupted {
            ui.release(false)?;
            return Ok(WaitOutcome::Interrupted);
        }

        let verdict = gate.as_ref().map_or_else(
            || {
                if now >= started + delay {
                    WaitVerdict::AttachNow {
                        network_restored: false,
                    }
                } else {
                    WaitVerdict::KeepWaiting
                }
            },
            |gate| gate.verdict(now),
        );
        if !matches!(verdict, WaitVerdict::KeepWaiting) && !server_pending {
            recovery.session_starting();
            let release_at = recovery.release_at(started.elapsed());
            while started.elapsed() < release_at {
                if stop.is_some_and(|stop| stop.load(Ordering::SeqCst))
                    || ui.tick(&mut recovery, started.elapsed())? == UiEvent::Interrupted
                {
                    ui.release(false)?;
                    return Ok(WaitOutcome::Interrupted);
                }
                sleep_retry_wait(
                    PANEL_TICK.min(release_at.saturating_sub(started.elapsed())),
                    stop,
                );
            }
            ui.release(true)?;
            let WaitVerdict::AttachNow { network_restored } = verdict else {
                continue;
            };
            return Ok(WaitOutcome::AttachNow { network_restored });
        }

        if interval.is_some_and(|_| now >= next_dial) {
            if let Some(plan) = internet_probe
                && !internet_pending
            {
                recovery.checking_internet();
                spawn_dial(
                    DialStage::Internet,
                    plan,
                    INTERNET_PROBE_TIMEOUT,
                    dial_tx.clone(),
                );
                internet_pending = true;
            }
            if let (Some(plan), Some(gate)) = (dial_plan, &gate)
                && !server_pending
            {
                recovery.checking_server();
                let timeout = DIAL_TIMEOUT.min(
                    gate.effective_deadline()
                        .saturating_duration_since(now)
                        .max(PANEL_TICK),
                );
                spawn_dial(DialStage::Server, plan, timeout, dial_tx.clone());
                server_pending = true;
            }
            next_dial = now + interval.unwrap_or_default();
        }
        let deadline = gate
            .as_ref()
            .map_or(started + delay, DialGate::effective_deadline);
        let until_deadline = deadline.saturating_duration_since(Instant::now());
        let wake_in = if until_deadline.is_zero() {
            PANEL_TICK
        } else {
            until_deadline.min(PANEL_TICK)
        };
        sleep_retry_wait(wake_in, stop);
    }
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
        spawn_probe_loop(target, control_path, events)
    }

    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
            control_path: None,
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
        if let Some(path) = &self.control_path {
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
) -> ProbeHandle {
    let Some(interval) = probe_interval_from_env() else {
        return ProbeHandle {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
            control_path: Some(control_path),
        };
    };
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_path = control_path.clone();
    let join = std::thread::spawn(move || {
        probe_loop(target, thread_path, interval, events, thread_stop);
    });
    ProbeHandle {
        stop,
        join: Some(join),
        control_path: Some(control_path),
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
) {
    let mut monitor =
        LinkMonitor::with_timeout(probe_timeout_from_env(), blackout_after_from_env());
    let mut failures = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if !wait_for_control_master(&target, &control_path, &stop) {
            return;
        }
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
