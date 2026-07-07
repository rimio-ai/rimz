use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use rimz::remote::RemoteTarget;
use rimz::remote::link::{
    LinkAck, LinkEvent, LinkMonitor, LinkProbe, control_check_spec, probe_interval_from_env,
    probe_stream_spec, probe_timeout_from_env,
};

const CONTROL_MASTER_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const CONTROL_MASTER_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_STREAM_BLACKOUT_FAILURES: u32 = 3;
const PROBE_RESPAWN_BACKOFF_MIN: Duration = Duration::from_secs(1);
const PROBE_RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);

pub(super) fn supervise_remote(
    control_spec: &rimz::mux::CommandSpec,
    plain_spec: &rimz::mux::CommandSpec,
    target: &RemoteTarget,
    control_path: &Path,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, ReconnectState, Verdict};

    let policy = ReconnectPolicy::from_env();
    let mut reconnect = ReconnectState::new(policy);
    let host = target.host_display();
    let mut outage_active = false;
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
        let spec = if control_ready {
            control_spec
        } else {
            plain_spec
        };
        let restore_existing_outage_after_gatetime = outage_active;
        let outcome = run_ssh_session(
            spec,
            host,
            &events_rx,
            &mut outage_active,
            policy.gatetime,
            restore_existing_outage_after_gatetime,
        )?;
        probe.stop();
        if control_ready {
            remove_control_path(control_path);
        }
        match reconnect.settle(outcome.status.code(), outcome.established) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => bail!(
                "ssh to {host} exited with status {code}; not reconnecting \
                 (only a dropped link on an established session is retried)"
            ),
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
                std::thread::sleep(delay);
            }
        }
    }
}

fn run_ssh_session(
    spec: &rimz::mux::CommandSpec,
    host: &str,
    events: &mpsc::Receiver<LinkEvent>,
    outage_active: &mut bool,
    gatetime: Duration,
    restore_existing_outage_after_gatetime: bool,
) -> Result<SessionOutcome> {
    let mut child = spec
        .to_command()
        .spawn()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
    let started = Instant::now();
    let mut reported_established = false;
    loop {
        if let Some(status) = child.try_wait().context("polling ssh session")? {
            return Ok(SessionOutcome {
                status,
                established: started.elapsed() >= gatetime,
            });
        }
        if !reported_established && started.elapsed() >= gatetime {
            reported_established = true;
            if restore_existing_outage_after_gatetime && *outage_active {
                report_link_restored(host, outage_active);
            }
        }
        if let Some(event) = recv_link_event(
            events,
            ssh_session_poll_interval(started, gatetime, reported_established),
        ) {
            handle_link_event(host, event, outage_active);
            while let Ok(event) = events.try_recv() {
                handle_link_event(host, event, outage_active);
            }
        }
    }
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
    let mut monitor = LinkMonitor::with_timeout(probe_timeout_from_env());
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
