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
    LinkAck, LinkEvent, LinkMonitor, LinkProbe, blackout_after_from_env, control_check_spec,
    probe_interval_from_env, probe_stream_spec, probe_timeout_from_env,
};
use rimz::remote::reachability::{
    DIAL_TIMEOUT, DialGate, DialPlan, WaitVerdict, dial_interval_from_env, parse_dial_plan,
    ssh_config_query_spec,
};
use rimz::remote::{RemoteTarget, SshAttachPlan};

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
    let stop = AtomicBool::new(false);
    let mut outage_active = false;
    let mut first_attempt = true;
    report_remote_connect(host, true);
    loop {
        let control_ready = match prepare_control_path(control_path) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(
                    path = %control_path.display(),
                    error = %err,
                    "ControlMaster unavailable; continuing without link probes"
                );
                false
            }
        };
        let (events_tx, events_rx) = mpsc::channel();
        let probe = if control_ready {
            spawn_probe_loop(target.clone(), control_path.to_path_buf(), events_tx)
        } else {
            drop(events_tx);
            ProbeHandle::disabled()
        };
        let attempt = if first_attempt {
            plan.initial()
        } else {
            plan.retry()
        };
        first_attempt = false;
        let spec = if control_ready {
            attempt.control(control_path)
        } else {
            attempt.plain()
        };
        let restore_existing_outage_after_gatetime = outage_active;
        let outcome = run_ssh_session(
            &spec,
            host,
            &events_rx,
            &mut outage_active,
            policy.gatetime,
            restore_existing_outage_after_gatetime,
            dial_plan.as_ref(),
        )?;
        probe.stop();
        if control_ready {
            remove_control_path(control_path);
        }
        if outcome.killed_zombie {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: link to {host} confirmed dead — host reachable, session silent; reconnecting now",
            );
            reconnect.settle_zombie_kill();
            continue;
        }
        match reconnect.settle(outcome.status.code(), outcome.established) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => bail!("{}", fatal_session_message(code, host, setup_hint)),
            Verdict::Retry { delay } => {
                let consecutive_failures = reconnect.consecutive_failures();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "rimz: link to {host} lost — reconnecting in {}s (attempt {consecutive_failures}); Ctrl-C stops",
                    delay.as_secs(),
                );
                drop(stderr);
                if !outage_active {
                    outage_active = true;
                    emit_local_link_notification(
                        rimz::sidebar::notify::NotificationKind::LinkLost,
                        "Rimz: remote link lost",
                        &format!("SSH to {host} dropped; reconnecting."),
                        LocalLinkNotificationDelivery::TerminalAndCommand,
                    );
                }
                if matches!(
                    wait_before_retry(dial_plan.as_ref(), delay, policy.backoff_cap, host, &stop,),
                    WaitVerdict::AttachNow {
                        network_restored: true
                    }
                ) {
                    reconnect.network_restored();
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "rimz: network to {host} restored — reconnecting now",
                    );
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
    outage_active: &mut bool,
    gatetime: Duration,
    restore_existing_outage_after_gatetime: bool,
    dial_plan: Option<&DialPlan>,
) -> Result<SessionOutcome> {
    let mut child = spec
        .to_command()
        .spawn()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
    let started = Instant::now();
    let mut reported_established = false;
    let mut transport_confirmed = false;
    let mut zombie_watch = false;
    let mut next_zombie_dial = Instant::now();
    let dial_interval = dial_interval_from_env();
    loop {
        if let Some(status) = child.try_wait().context("polling ssh session")? {
            while let Ok(event) = events.try_recv() {
                transport_confirmed |= matches!(event, LinkEvent::FirstAck);
            }
            return Ok(SessionOutcome {
                status,
                established: session_established(transport_confirmed, started.elapsed(), gatetime),
                killed_zombie: false,
            });
        }
        if !reported_established && started.elapsed() >= gatetime {
            reported_established = true;
            if restore_existing_outage_after_gatetime && *outage_active {
                report_link_restored(host, outage_active);
            }
        }
        let mut poll = ssh_session_poll_interval(started, gatetime, reported_established);
        if zombie_watch && dial_plan.is_some() {
            poll = poll.min(next_zombie_dial.saturating_duration_since(Instant::now()));
        }
        if let Some(event) = recv_link_event(events, poll) {
            observe_session_link_event(
                host,
                event,
                outage_active,
                &mut transport_confirmed,
                &mut zombie_watch,
            );
            while let Ok(event) = events.try_recv() {
                observe_session_link_event(
                    host,
                    event,
                    outage_active,
                    &mut transport_confirmed,
                    &mut zombie_watch,
                );
            }
        }
        let established = session_established(transport_confirmed, started.elapsed(), gatetime);
        if zombie_watch
            && established
            && let (Some(plan), Some(interval)) = (dial_plan, dial_interval)
            && Instant::now() >= next_zombie_dial
        {
            next_zombie_dial = Instant::now() + interval;
            if dial(plan) {
                // A reachable host plus an established, blacked-out probe
                // proves this transport is a zombie. SIGKILL releases its tty
                // immediately so the replacement ssh can own it.
                match child.kill() {
                    Ok(()) => {
                        let status = child.wait().context("waiting for killed ssh session")?;
                        return Ok(SessionOutcome {
                            status,
                            established: true,
                            killed_zombie: true,
                        });
                    }
                    Err(kill_err) => {
                        if let Some(status) =
                            child.try_wait().context("polling raced ssh session")?
                        {
                            return Ok(SessionOutcome {
                                status,
                                established,
                                killed_zombie: false,
                            });
                        }
                        return Err(kill_err).context("killing zombie ssh session");
                    }
                }
            }
        }
    }
}

fn observe_session_link_event(
    host: &str,
    event: LinkEvent,
    outage_active: &mut bool,
    transport_confirmed: &mut bool,
    zombie_watch: &mut bool,
) {
    match event {
        LinkEvent::FirstAck => {
            *transport_confirmed = true;
            *zombie_watch = false;
        }
        LinkEvent::Blackout(_) => *zombie_watch = true,
        LinkEvent::Recovered => *zombie_watch = false,
    }
    handle_link_event(host, event, outage_active);
}

fn ssh_session_poll_interval(
    started: Instant,
    gatetime: Duration,
    reported_established: bool,
) -> Duration {
    let poll = Duration::from_millis(200);
    if reported_established {
        return poll;
    }
    let elapsed = started.elapsed();
    if elapsed >= gatetime {
        Duration::ZERO
    } else {
        poll.min(gatetime - elapsed)
    }
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

pub(super) fn wait_before_retry(
    dial_plan: Option<&DialPlan>,
    delay: Duration,
    hold: Duration,
    host: &str,
    stop: &AtomicBool,
) -> WaitVerdict {
    let Some(plan) = dial_plan else {
        super::sleep_interruptibly(delay, stop);
        return WaitVerdict::AttachNow {
            network_restored: false,
        };
    };
    let Some(interval) = dial_interval_from_env() else {
        super::sleep_interruptibly(delay, stop);
        return WaitVerdict::AttachNow {
            network_restored: false,
        };
    };

    let started = Instant::now();
    let mut gate = DialGate::new(started, delay, hold);
    let mut next_dial = started;
    let mut reported_unreachable = false;
    loop {
        let now = Instant::now();
        let verdict = gate.verdict(now);
        if !matches!(verdict, WaitVerdict::KeepWaiting) {
            return verdict;
        }
        if stop.load(Ordering::SeqCst) {
            return WaitVerdict::AttachNow {
                network_restored: false,
            };
        }
        if now >= next_dial {
            // Unknown reachability keeps the backoff deadline so the first dial cannot delay
            // an otherwise-ready retry; an unreachable result extends later dials to the hold.
            let timeout =
                DIAL_TIMEOUT.min(gate.effective_deadline().saturating_duration_since(now));
            let reachable = dial_with_timeout(plan, timeout);
            gate.note_dial(reachable);
            if !reachable && !reported_unreachable {
                reported_unreachable = true;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: {host} unreachable — holding reconnect until the network returns; Ctrl-C stops",
                );
            }
            let verdict = gate.verdict(Instant::now());
            if !matches!(verdict, WaitVerdict::KeepWaiting) {
                return verdict;
            }
            next_dial = Instant::now() + interval;
        }
        let wake_at = next_dial.min(gate.effective_deadline());
        super::sleep_interruptibly(wake_at.saturating_duration_since(Instant::now()), stop);
    }
}

/// A finished ssh session counts as established once its SSH transport is
/// confirmed up by the link probe, or it outlived the gatetime fallback.
fn session_established(transport_confirmed: bool, lifetime: Duration, gatetime: Duration) -> bool {
    transport_confirmed || lifetime >= gatetime
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

fn handle_link_event(host: &str, event: LinkEvent, outage_active: &mut bool) {
    match event {
        LinkEvent::FirstAck if *outage_active => report_link_restored(host, outage_active),
        LinkEvent::FirstAck => {}
        LinkEvent::Blackout(duration) => {
            if *outage_active {
                return;
            }
            *outage_active = true;
            emit_local_link_notification(
                rimz::sidebar::notify::NotificationKind::LinkLost,
                "Rimz: remote link stalled",
                &format!(
                    "No probe ack from {host} for {}s.",
                    duration.as_secs().max(1)
                ),
                LocalLinkNotificationDelivery::TerminalOnly,
            );
        }
        LinkEvent::Recovered => report_link_restored(host, outage_active),
    }
}

fn report_link_restored(host: &str, outage_active: &mut bool) {
    if !*outage_active {
        return;
    }
    *outage_active = false;
    emit_local_link_notification(
        rimz::sidebar::notify::NotificationKind::LinkRestored,
        "Rimz: remote link restored",
        &format!("SSH to {host} is responsive again."),
        LocalLinkNotificationDelivery::TerminalAndCommand,
    );
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
}

impl ProbeHandle {
    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn spawn_probe_loop(
    target: RemoteTarget,
    control_path: PathBuf,
    events: mpsc::Sender<LinkEvent>,
) -> ProbeHandle {
    let Some(interval) = probe_interval_from_env() else {
        return ProbeHandle::disabled();
    };
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        probe_loop(target, control_path, interval, events, thread_stop);
    });
    ProbeHandle {
        stop,
        join: Some(join),
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
    reader: std::thread::JoinHandle<()>,
}

impl ProbeChild {
    fn spawn(
        target: &RemoteTarget,
        control_path: &Path,
    ) -> std::io::Result<(Self, mpsc::Receiver<u64>)> {
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
        Ok((
            Self {
                child,
                stdin,
                reader,
            },
            ack_rx,
        ))
    }

    fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader.join();
    }
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
    let (mut child, ack_rx) = match ProbeChild::spawn(target, control_path) {
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

        let mut reported_rtt_changed = false;
        while let Ok(seq) = ack_rx.try_recv() {
            let outcome = monitor.record_ack(seq, rimz::sidebar::timing::unix_now_ms());
            acked |= outcome.accepted;
            reported_rtt_changed |= outcome.reported_rtt_changed;
            for event in outcome.events {
                let _ = events.send(event);
            }
        }
        if reported_rtt_changed {
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
            while let Ok(seq) = ack_rx.try_recv() {
                let outcome = monitor.record_ack(seq, rimz::sidebar::timing::unix_now_ms());
                acked |= outcome.accepted;
                for event in outcome.events {
                    let _ = events.send(event);
                }
            }
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
