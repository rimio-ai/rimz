//! Remote-link health protocol and pure terminal-session transitions.
//!
//! The remote supervisor owns process I/O. This module only names the JSONL
//! probe/ack/file schema, classifies link health, builds SSH argv fragments, and
//! reduces transport, health, and child-exit inputs into session actions.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mux::CommandSpec;
use crate::sock;

use super::{RemoteSpec, RemoteTarget, env_ms, quote_remote_path, remote_path_prefix, sh_quote};

const LINK_SCHEMA_VERSION: &str = "rimz.link.v1";
const LINK_STATS_FILE: &str = "link-stats.json";
const LINK_PROBE_INTERVAL: Duration = Duration::from_secs(2);
const LINK_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LINK_BLACKOUT_AFTER: Duration = Duration::from_secs(8);
const LINK_WINDOW: usize = 30;

const PROBE_INTERVAL_ENV: &str = "RIMZ_REMOTE_PROBE_MS";
const PROBE_TIMEOUT_ENV: &str = "RIMZ_REMOTE_PROBE_TIMEOUT_MS";
const BLACKOUT_AFTER_ENV: &str = "RIMZ_REMOTE_BLACKOUT_MS";

/// Link-health tier for notifications, diagnostics, and CLI health output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTier {
    Good,
    Degraded,
    Bad,
}

/// Rolling link measurements published by the local probe stream and folded by
/// the remote sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    pub miss_pct: u16,
    pub window: u16,
}

/// One local-to-remote probe line. `stats` is the best view before this line's
/// ack arrives; the remote file therefore always carries the last settled
/// window, not a half-sent sample.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProbe {
    pub v: String,
    pub seq: u64,
    pub sent_at_ms: u64,
    pub stats: LinkStats,
}

impl LinkProbe {
    fn new(seq: u64, sent_at_ms: u64, stats: LinkStats) -> Self {
        Self {
            v: LINK_SCHEMA_VERSION.to_owned(),
            seq,
            sent_at_ms,
            stats,
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == LINK_SCHEMA_VERSION
    }
}

/// One remote-to-local ack line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkAck {
    pub v: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<u16>>,
}

impl LinkAck {
    pub fn new(seq: u64) -> Self {
        Self {
            v: LINK_SCHEMA_VERSION.to_owned(),
            seq,
            ports: None,
        }
    }

    pub fn with_ports(seq: u64, ports: Vec<u16>) -> Self {
        Self {
            v: LINK_SCHEMA_VERSION.to_owned(),
            seq,
            ports: Some(ports),
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == LINK_SCHEMA_VERSION
    }
}

/// The sidecar ingested on the remote host and read by sidebar enrichment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStatsFile {
    pub v: String,
    pub received_at_ms: u64,
    pub client: String,
    pub stats: LinkStats,
}

impl LinkStatsFile {
    pub fn new(received_at_ms: u64, client: String, stats: LinkStats) -> Self {
        Self {
            v: LINK_SCHEMA_VERSION.to_owned(),
            received_at_ms,
            client,
            stats,
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == LINK_SCHEMA_VERSION
    }
}

/// Link-health events emitted by local probe accounting and rendered by the
/// remote supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    FirstAck,
    Blackout(Duration),
    Recovered,
}

/// Pure decisions for one foreground SSH session and its reconnect lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionLinkAction {
    NotifyBlackout(Duration),
    NotifyTransportLoss,
    Restore,
    VerifyZombie,
}

/// Effects and next elapsed-time deadline produced by one state advance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLinkUpdate {
    pub actions: Vec<SessionLinkAction>,
    pub next_deadline: Option<Duration>,
}

/// Establishment, outage edges, and zombie eligibility for terminal SSH.
///
/// Elapsed time is supplied by the CLI so clocks, waits, process state, and
/// reachability dials stay outside this module. Outage state spans reconnects;
/// session-local state resets through [`Self::begin_session`].
#[derive(Clone, Debug)]
pub struct SessionLinkState {
    gatetime: Duration,
    zombie_interval: Option<Duration>,
    established: bool,
    transport_confirmed: bool,
    gatetime_observed: bool,
    zombie_watch: bool,
    next_zombie_verification: Duration,
    outage_active: bool,
    restore_outage_at_gatetime: bool,
}

impl SessionLinkState {
    pub fn new(gatetime: Duration, zombie_interval: Option<Duration>) -> Self {
        Self {
            gatetime,
            zombie_interval,
            established: false,
            transport_confirmed: false,
            gatetime_observed: false,
            zombie_watch: false,
            next_zombie_verification: Duration::ZERO,
            outage_active: false,
            restore_outage_at_gatetime: false,
        }
    }

    pub fn begin_session(&mut self) -> SessionLinkUpdate {
        self.established = false;
        self.transport_confirmed = false;
        self.gatetime_observed = false;
        self.zombie_watch = false;
        self.next_zombie_verification = Duration::ZERO;
        self.restore_outage_at_gatetime = self.outage_active;
        SessionLinkUpdate {
            actions: Vec::new(),
            next_deadline: Some(self.gatetime),
        }
    }

    /// Advance once after the caller drains every currently queued link event.
    pub fn advance(
        &mut self,
        elapsed: Duration,
        events: impl IntoIterator<Item = LinkEvent>,
    ) -> SessionLinkUpdate {
        let mut actions = Vec::new();
        self.observe_gatetime(elapsed, &mut actions);
        for event in events {
            self.observe_event(event, &mut actions);
        }
        self.schedule_zombie_verification(elapsed, &mut actions);
        SessionLinkUpdate {
            actions,
            next_deadline: self.next_deadline(),
        }
    }

    /// Settle an exited child without emitting queued notification edges.
    /// Child exit has priority over link presentation in the supervisor.
    pub fn finish(
        &mut self,
        elapsed: Duration,
        events: impl IntoIterator<Item = LinkEvent>,
    ) -> (bool, bool) {
        for event in events {
            if matches!(event, LinkEvent::FirstAck) {
                self.transport_confirmed = true;
                self.established = true;
            }
        }
        let lived_past_gatetime = elapsed >= self.gatetime;
        self.established |= lived_past_gatetime;
        (self.established, lived_past_gatetime)
    }

    /// Record a dropped transport, returning the notification edge once per
    /// outage even when several reconnect attempts fail.
    pub fn transport_lost(&mut self) -> Option<SessionLinkAction> {
        if self.outage_active {
            None
        } else {
            self.outage_active = true;
            Some(SessionLinkAction::NotifyTransportLoss)
        }
    }

    fn established(&self, elapsed: Duration) -> bool {
        self.established || elapsed >= self.gatetime
    }

    fn observe_gatetime(&mut self, elapsed: Duration, actions: &mut Vec<SessionLinkAction>) {
        if self.gatetime_observed || elapsed < self.gatetime {
            return;
        }
        self.gatetime_observed = true;
        self.established = true;
        if self.restore_outage_at_gatetime {
            self.restore(actions);
        }
    }

    fn observe_event(&mut self, event: LinkEvent, actions: &mut Vec<SessionLinkAction>) {
        match event {
            LinkEvent::FirstAck => {
                self.transport_confirmed = true;
                self.established = true;
                self.zombie_watch = false;
                self.restore(actions);
            }
            LinkEvent::Blackout(duration) => {
                self.zombie_watch = true;
                if !self.outage_active {
                    self.outage_active = true;
                    actions.push(SessionLinkAction::NotifyBlackout(duration));
                }
            }
            LinkEvent::Recovered => {
                self.zombie_watch = false;
                self.restore(actions);
            }
        }
    }

    fn restore(&mut self, actions: &mut Vec<SessionLinkAction>) {
        if self.outage_active {
            self.outage_active = false;
            self.restore_outage_at_gatetime = false;
            actions.push(SessionLinkAction::Restore);
        }
    }

    fn schedule_zombie_verification(
        &mut self,
        elapsed: Duration,
        actions: &mut Vec<SessionLinkAction>,
    ) {
        let Some(interval) = self.zombie_interval else {
            return;
        };
        if self.zombie_watch
            && self.established(elapsed)
            && elapsed >= self.next_zombie_verification
        {
            self.next_zombie_verification = elapsed + interval;
            actions.push(SessionLinkAction::VerifyZombie);
        }
    }

    fn next_deadline(&self) -> Option<Duration> {
        let gatetime = (!self.gatetime_observed).then_some(self.gatetime);
        let zombie = (self.zombie_watch && self.zombie_interval.is_some() && self.established)
            .then_some(self.next_zombie_verification);
        match (gatetime, zombie) {
            (Some(gatetime), Some(zombie)) => Some(gatetime.min(zombie)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        }
    }
}

/// Overall tier is the worst of RTT and probe-miss rate.
pub fn link_tier(rtt_ms: Option<u32>, miss_pct: u16) -> LinkTier {
    let rtt = match rtt_ms {
        Some(ms) if ms > 400 => LinkTier::Bad,
        Some(ms) if ms > 150 => LinkTier::Degraded,
        _ => LinkTier::Good,
    };
    let miss = if miss_pct > 10 {
        LinkTier::Bad
    } else if miss_pct >= 1 {
        LinkTier::Degraded
    } else {
        LinkTier::Good
    };
    rtt.max(miss)
}

/// Footer-badge heat, computed as the worse of the latency and probe-loss axes.
///
/// Alerting uses [`LinkTier`]; the badge is a continuous ramp the renderer turns
/// into a tone through `Theme::heat_tone` (green → gold → amber → red), keeping
/// this module theme-free. Both axes are linear — latency over `100..=400ms`,
/// loss over `0..=30%` — so a healthy fresh link reads green (`0.0`) and a bad
/// one red (`1.0`). `None` only while the RTT is still warming (no sample yet),
/// where the renderer paints its neutral resting tone.
pub fn link_badge_heat(rtt_ms: Option<u32>, miss_pct: u16) -> Option<f32> {
    let rtt = rtt_ms?;
    let latency = axis_badge_heat(rtt, 100, 400);
    let loss = axis_badge_heat(u32::from(miss_pct), 0, 30);
    Some(latency.max(loss))
}

/// One badge axis on the full ramp: `0.0` at or below `calm`, climbing linearly
/// to `1.0` at `alarm` and beyond.
fn axis_badge_heat(value: u32, calm: u32, alarm: u32) -> f32 {
    if value <= calm {
        0.0
    } else {
        ((value - calm) as f32 / (alarm - calm) as f32).min(1.0)
    }
}

impl Ord for LinkTier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        tier_rank(*self).cmp(&tier_rank(*other))
    }
}

impl PartialOrd for LinkTier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn tier_rank(tier: LinkTier) -> u8 {
    match tier {
        LinkTier::Good => 0,
        LinkTier::Degraded => 1,
        LinkTier::Bad => 2,
    }
}

/// Runtime sidecar path for the workspace's latest link stats.
pub fn stats_path(runtime: &crate::RuntimePaths) -> PathBuf {
    runtime.root.join(LINK_STATS_FILE)
}

pub fn validated_control_path() -> std::result::Result<PathBuf, sock::SocketPathTooLong> {
    validated_control_path_under(&crate::store::paths::runtime_home(), std::process::id())
}

fn validated_control_path_under(
    runtime_root: &Path,
    pid: u32,
) -> std::result::Result<PathBuf, sock::SocketPathTooLong> {
    let path = control_path_under(runtime_root, pid);
    sock::validate_socket_path(&path)?;
    Ok(path)
}

fn control_path_under(runtime_root: &Path, pid: u32) -> PathBuf {
    runtime_root
        .join("rimz")
        .join("link")
        .join(format!("link-{pid}.sock"))
}

/// Check that the interactive attach's ControlMaster socket is already live.
///
/// `ssh -S <path>` without `-O check` opportunistically falls back to a new TCP
/// connection when the socket is absent. Probe streams must run only after this
/// check succeeds, so their measurements describe the user's session link.
pub fn control_check_spec(target: &RemoteTarget, control_path: &Path) -> CommandSpec {
    CommandSpec::new(super::ssh_program()).args([
        "-S".to_owned(),
        control_path.display().to_string(),
        "-O".to_owned(),
        "check".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "--".to_owned(),
        target.ssh_destination().as_str().to_owned(),
    ])
}

/// Build the long-lived probe stream on the same SSH TCP connection as the
/// interactive attach.
pub fn probe_stream_spec(target: &RemoteTarget, control_path: &Path) -> CommandSpec {
    let mut spec = CommandSpec::new(super::ssh_program()).args([
        "-S".to_owned(),
        control_path.display().to_string(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "--".to_owned(),
    ]);
    spec = spec.arg(target.ssh_destination().as_str());
    spec.arg(link_ingest_snippet(target))
}

fn link_ingest_snippet(target: &RemoteTarget) -> String {
    let target_arg = match &target.spec {
        RemoteSpec::Session(name) => format!("--session {}", sh_quote(name)),
        RemoteSpec::Path(path) => format!("--dir {}", quote_remote_path(path)),
    };
    format!(
        "{}; exec rimz remote link-stats ingest {target_arg}",
        remote_path_prefix(),
    )
}

/// The configured probe interval. `RIMZ_REMOTE_PROBE_MS=0` disables probing.
pub fn probe_interval_from_env() -> Option<Duration> {
    match env_ms(PROBE_INTERVAL_ENV) {
        Some(ms) if ms.is_zero() => None,
        Some(ms) => Some(ms),
        None => Some(LINK_PROBE_INTERVAL),
    }
}

pub fn probe_timeout_from_env() -> Duration {
    env_ms(PROBE_TIMEOUT_ENV).unwrap_or(LINK_PROBE_TIMEOUT)
}

pub fn blackout_after_from_env() -> Duration {
    env_ms(BLACKOUT_AFTER_ENV).unwrap_or(LINK_BLACKOUT_AFTER)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeOutcome {
    Pending { seq: u64, sent_at_ms: u64 },
    Ack { sent_at_ms: u64 },
    Miss { sent_at_ms: u64 },
}

/// Rolling probe accounting: late acks are ignored, pending probes expire into
/// misses, and settled outcomes are capped to the latest [`LINK_WINDOW`].
#[derive(Clone, Debug)]
struct ProbeWindow {
    outcomes: VecDeque<ProbeOutcome>,
    ewma_ms: Option<f64>,
    reported_ms: Option<u32>,
    discard_next_ack_sample: bool,
    last_ack_at_ms: Option<u64>,
    timeout: Duration,
}

impl Default for ProbeWindow {
    fn default() -> Self {
        Self::with_timeout(LINK_PROBE_TIMEOUT)
    }
}

impl ProbeWindow {
    fn with_timeout(timeout: Duration) -> Self {
        Self {
            outcomes: VecDeque::new(),
            ewma_ms: None,
            reported_ms: None,
            discard_next_ack_sample: true,
            last_ack_at_ms: None,
            timeout,
        }
    }

    /// Start a fresh probe-stream process while preserving the settled window.
    ///
    /// Each stream execs a new remote ingest process, so its first ack can pay
    /// cold spawn cost. Re-arm the single-sample discard without clearing the
    /// EWMA or displayed RTT; a reconnect keeps showing the last steady value
    /// until real samples arrive.
    fn begin_stream(&mut self) {
        self.discard_next_ack_sample = true;
    }

    fn record_sent(&mut self, seq: u64, at_ms: u64) {
        self.outcomes.push_back(ProbeOutcome::Pending {
            seq,
            sent_at_ms: at_ms,
        });
        self.trim();
    }

    fn record_ack(&mut self, seq: u64, at_ms: u64) -> bool {
        let Some(index) = self
            .outcomes
            .iter()
            .position(|outcome| matches!(outcome, ProbeOutcome::Pending { seq: pending, .. } if *pending == seq))
        else {
            return false;
        };
        let sent_at_ms = match self.outcomes[index] {
            ProbeOutcome::Pending { sent_at_ms, .. } => sent_at_ms,
            ProbeOutcome::Ack { .. } | ProbeOutcome::Miss { .. } => return false,
        };
        let rtt = at_ms.saturating_sub(sent_at_ms);
        self.update_rtt(rtt.min(u64::from(u32::MAX)) as u32);
        self.last_ack_at_ms = Some(at_ms);
        self.outcomes[index] = ProbeOutcome::Ack { sent_at_ms };
        true
    }

    fn expire(&mut self, now_ms: u64) {
        let timeout_ms = self.timeout.as_millis() as u64;
        for outcome in &mut self.outcomes {
            if let ProbeOutcome::Pending { sent_at_ms, .. } = *outcome
                && now_ms.saturating_sub(sent_at_ms) >= timeout_ms
            {
                *outcome = ProbeOutcome::Miss { sent_at_ms };
            }
        }
    }

    fn stats(&self) -> LinkStats {
        let mut settled: u16 = 0;
        let mut misses: u16 = 0;
        for outcome in &self.outcomes {
            match outcome {
                ProbeOutcome::Ack { .. } => settled = settled.saturating_add(1),
                ProbeOutcome::Miss { .. } => {
                    settled = settled.saturating_add(1);
                    misses = misses.saturating_add(1);
                }
                ProbeOutcome::Pending { .. } => {}
            }
        }
        let miss_pct = if settled == 0 {
            0
        } else {
            ((u32::from(misses) * 100) / u32::from(settled)) as u16
        };
        LinkStats {
            rtt_ms: self.reported_ms,
            miss_pct,
            window: settled,
        }
    }

    fn reported_rtt_ms(&self) -> Option<u32> {
        self.reported_ms
    }

    fn blackout_ms(&self, now_ms: u64) -> u64 {
        if let Some(last_ack) = self.last_ack_at_ms {
            return now_ms.saturating_sub(last_ack);
        }
        self.outcomes
            .iter()
            .map(|outcome| match outcome {
                ProbeOutcome::Pending { sent_at_ms, .. }
                | ProbeOutcome::Ack { sent_at_ms }
                | ProbeOutcome::Miss { sent_at_ms } => *sent_at_ms,
            })
            .min()
            .map(|first| now_ms.saturating_sub(first))
            .unwrap_or(0)
    }

    fn update_rtt(&mut self, rtt_ms: u32) {
        if std::mem::take(&mut self.discard_next_ack_sample) {
            return;
        }

        let sample = f64::from(rtt_ms);
        let ewma = match self.ewma_ms {
            Some(prev) => {
                let alpha = ewma_alpha(prev, sample);
                prev.mul_add(1.0 - alpha, sample * alpha)
            }
            None => sample,
        };
        self.ewma_ms = Some(ewma);
        self.update_reported_ms(ewma);
    }

    fn update_reported_ms(&mut self, ewma: f64) {
        const REPORTED_MS_HYSTERESIS: u32 = 8;

        let rounded = round_ewma(ewma);
        if self
            .reported_ms
            .is_none_or(|reported| reported.abs_diff(rounded) >= REPORTED_MS_HYSTERESIS)
        {
            self.reported_ms = Some(rounded);
        }
    }

    fn trim(&mut self) {
        while self.outcomes.len() > LINK_WINDOW {
            self.outcomes.pop_front();
        }
    }
}

/// Outcome of recording one probe ack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AckOutcome {
    pub accepted: bool,
    pub reported_rtt_changed: bool,
    pub events: Vec<LinkEvent>,
}

/// Pure local link-health accounting for the probe stream.
#[derive(Clone, Debug)]
pub struct LinkMonitor {
    window: ProbeWindow,
    blackout_after: Duration,
    seen_ack: bool,
    blackout_latched: bool,
    seq: u64,
}

impl LinkMonitor {
    pub fn with_timeout(timeout: Duration, blackout_after: Duration) -> Self {
        Self {
            window: ProbeWindow::with_timeout(timeout),
            blackout_after,
            seen_ack: false,
            blackout_latched: false,
            seq: 0,
        }
    }

    pub fn begin_stream(&mut self) {
        self.window.begin_stream();
    }

    pub fn next_probe(&mut self, now_ms: u64) -> LinkProbe {
        let seq = self.next_seq();
        let probe = LinkProbe::new(seq, now_ms, self.window.stats());
        self.window.record_sent(seq, now_ms);
        probe
    }

    pub fn stats_refresh_probe(&mut self, now_ms: u64) -> LinkProbe {
        let seq = self.next_seq();
        // Stats refreshes update the remote cache immediately after a displayed
        // RTT change. They are not measurement samples, so the ack is
        // intentionally ignored by the window.
        LinkProbe::new(seq, now_ms, self.window.stats())
    }

    pub fn record_ack(&mut self, seq: u64, now_ms: u64) -> AckOutcome {
        let before_rtt = self.window.reported_rtt_ms();
        if !self.window.record_ack(seq, now_ms) {
            return AckOutcome::default();
        }

        let mut events = Vec::new();
        if !self.seen_ack {
            self.seen_ack = true;
            events.push(LinkEvent::FirstAck);
        }
        if self.blackout_latched {
            self.blackout_latched = false;
            events.push(LinkEvent::Recovered);
        }

        AckOutcome {
            accepted: true,
            reported_rtt_changed: before_rtt != self.window.reported_rtt_ms(),
            events,
        }
    }

    pub fn check_blackout(&mut self, now_ms: u64) -> Option<LinkEvent> {
        if !self.seen_ack {
            return None;
        }
        self.window.expire(now_ms);
        let blackout_ms = self.window.blackout_ms(now_ms);
        if blackout_ms >= self.blackout_after.as_millis() as u64 && !self.blackout_latched {
            self.blackout_latched = true;
            return Some(LinkEvent::Blackout(Duration::from_millis(blackout_ms)));
        }
        None
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.seq;
        self.seq = self.seq.saturating_add(1);
        seq
    }
}

fn ewma_alpha(prev: f64, sample: f64) -> f64 {
    const STABLE_ALPHA: f64 = 0.15;
    const FAST_ALPHA: f64 = 0.60;
    const STABLE_RELATIVE_DELTA: f64 = 0.08;
    const FAST_RELATIVE_DELTA: f64 = 0.50;

    let relative_delta = ((sample - prev).abs() / prev.max(1.0)).clamp(0.0, FAST_RELATIVE_DELTA);
    if relative_delta <= STABLE_RELATIVE_DELTA {
        return STABLE_ALPHA;
    }
    let ramp =
        (relative_delta - STABLE_RELATIVE_DELTA) / (FAST_RELATIVE_DELTA - STABLE_RELATIVE_DELTA);
    STABLE_ALPHA + (FAST_ALPHA - STABLE_ALPHA) * ramp
}

fn round_ewma(value: f64) -> u32 {
    value.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(input: &str) -> RemoteTarget {
        RemoteTarget::parse(input).unwrap()
    }

    #[test]
    fn session_link_acknowledgement_establishes_before_gatetime() {
        let mut state =
            SessionLinkState::new(Duration::from_secs(30), Some(Duration::from_secs(5)));
        state.begin_session();

        let update = state.advance(Duration::from_secs(5), [LinkEvent::FirstAck]);

        assert!(state.established(Duration::from_secs(5)));
        assert!(update.actions.is_empty());
        assert_eq!(update.next_deadline, Some(Duration::from_secs(30)));
    }

    #[test]
    fn session_link_gatetime_fallback_restores_an_existing_outage() {
        let mut state = SessionLinkState::new(Duration::from_secs(30), None);
        assert_eq!(
            state.transport_lost(),
            Some(SessionLinkAction::NotifyTransportLoss)
        );
        state.begin_session();

        let update = state.advance(Duration::from_secs(30), []);

        assert!(state.established(Duration::from_secs(30)));
        assert_eq!(update.actions, [SessionLinkAction::Restore]);
        assert_eq!(update.next_deadline, None);
    }

    #[test]
    fn session_link_blackout_and_recovery_emit_edges_once() {
        let mut state =
            SessionLinkState::new(Duration::from_secs(30), Some(Duration::from_secs(5)));
        state.begin_session();
        state.advance(Duration::from_secs(1), [LinkEvent::FirstAck]);

        let blackout = state.advance(
            Duration::from_secs(2),
            [LinkEvent::Blackout(Duration::from_secs(8))],
        );
        assert_eq!(
            blackout.actions,
            [
                SessionLinkAction::NotifyBlackout(Duration::from_secs(8)),
                SessionLinkAction::VerifyZombie,
            ]
        );

        let duplicate = state.advance(
            Duration::from_secs(3),
            [LinkEvent::Blackout(Duration::from_secs(9))],
        );
        assert!(duplicate.actions.is_empty());

        let recovered = state.advance(Duration::from_secs(4), [LinkEvent::Recovered]);
        assert_eq!(recovered.actions, [SessionLinkAction::Restore]);
    }

    #[test]
    fn session_link_verifies_zombies_only_after_establishment() {
        let mut state =
            SessionLinkState::new(Duration::from_secs(30), Some(Duration::from_secs(5)));
        state.begin_session();

        let blackout = state.advance(
            Duration::from_secs(2),
            [LinkEvent::Blackout(Duration::from_secs(8))],
        );
        assert_eq!(
            blackout.actions,
            [SessionLinkAction::NotifyBlackout(Duration::from_secs(8))]
        );

        let established = state.advance(Duration::from_secs(30), []);
        assert_eq!(
            established.actions,
            [SessionLinkAction::VerifyZombie],
            "gatetime makes the already-blacked-out session eligible"
        );
    }

    #[test]
    fn session_link_restores_outage_on_next_session_ack() {
        let mut state = SessionLinkState::new(Duration::from_secs(30), None);
        state.begin_session();
        state.advance(
            Duration::from_secs(2),
            [LinkEvent::Blackout(Duration::from_secs(8))],
        );

        state.begin_session();
        let update = state.advance(Duration::from_secs(1), [LinkEvent::FirstAck]);

        assert_eq!(update.actions, [SessionLinkAction::Restore]);
    }

    #[test]
    fn session_link_child_exit_wins_while_zombie_verification_is_pending() {
        let mut state =
            SessionLinkState::new(Duration::from_secs(30), Some(Duration::from_secs(5)));
        state.begin_session();
        let update = state.advance(
            Duration::from_secs(2),
            [
                LinkEvent::FirstAck,
                LinkEvent::Blackout(Duration::from_secs(8)),
            ],
        );
        assert!(update.actions.contains(&SessionLinkAction::VerifyZombie));

        assert!(
            state.finish(Duration::from_secs(2), []).0,
            "an exited acknowledged child is settled instead of zombie-killed"
        );
    }

    #[test]
    fn tier_and_badge_heat_boundaries_are_exact() {
        for (rtt, loss, expected) in [
            (Some(150), 0, LinkTier::Good),
            (Some(151), 0, LinkTier::Degraded),
            (Some(400), 0, LinkTier::Degraded),
            (Some(401), 0, LinkTier::Bad),
            (Some(10), 0, LinkTier::Good),
            (Some(10), 1, LinkTier::Degraded),
            (Some(10), 10, LinkTier::Degraded),
            (Some(10), 11, LinkTier::Bad),
        ] {
            assert_eq!(link_tier(rtt, loss), expected, "{rtt:?}/{loss}");
        }

        let approx = |actual: Option<f32>, expected: f32| {
            let got = actual.expect("heat");
            assert!(
                (got - expected).abs() < 1e-4,
                "expected {expected}, got {got}"
            );
        };
        assert_eq!(link_badge_heat(None, 0), None);
        approx(link_badge_heat(Some(100), 0), 0.0);
        approx(link_badge_heat(Some(200), 0), 1.0 / 3.0);
        approx(link_badge_heat(Some(300), 0), 2.0 / 3.0);
        approx(link_badge_heat(Some(400), 0), 1.0);
        approx(link_badge_heat(Some(401), 0), 1.0);
        approx(link_badge_heat(Some(100), 0), 0.0);
        approx(link_badge_heat(Some(100), 1), 1.0 / 30.0);
        approx(link_badge_heat(Some(100), 10), 1.0 / 3.0);
        approx(link_badge_heat(Some(100), 11), 11.0 / 30.0);
        approx(link_badge_heat(Some(100), 20), 2.0 / 3.0);
        approx(link_badge_heat(Some(100), 30), 1.0);
        approx(link_badge_heat(Some(100), 31), 1.0);
        approx(link_badge_heat(Some(120), 30), 1.0);
        approx(link_badge_heat(Some(300), 10), 2.0 / 3.0);
    }

    #[test]
    fn probe_window_accounts_ack_miss_late_ack_trim() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 1_000);
        assert!(window.record_ack(1, 1_050));
        assert_eq!(window.stats().rtt_ms, None);
        assert_eq!(window.stats().miss_pct, 0);
        assert_eq!(window.stats().window, 1);

        window.record_sent(2, 1_060);
        assert!(window.record_ack(2, 1_115));
        assert_eq!(window.stats().rtt_ms, Some(55));

        window.record_sent(3, 1_100);
        window.expire(1_201);
        assert_eq!(window.stats().miss_pct, 33);
        assert!(
            !window.record_ack(3, 1_230),
            "late ack after miss is ignored"
        );
        assert_eq!(window.blackout_ms(1_250), 135);

        for seq in 4..=(LINK_WINDOW as u64 + 5) {
            let sent_at_ms = 2_000 + seq * 10;
            window.record_sent(seq, sent_at_ms);
            assert!(window.record_ack(seq, sent_at_ms + 1));
        }
        assert_eq!(usize::from(window.stats().window), LINK_WINDOW);
    }

    #[test]
    fn probe_window_smooths_rtt_and_rearms_stream() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 0);
        assert!(window.record_ack(1, 740));
        assert_eq!(
            window.stats(),
            LinkStats {
                rtt_ms: None,
                miss_pct: 0,
                window: 1,
            }
        );

        window.record_sent(2, 1_000);
        assert!(window.record_ack(2, 1_210));
        assert_eq!(window.stats().rtt_ms, Some(210));

        window.begin_stream();
        window.record_sent(3, 2_000);
        assert!(window.record_ack(3, 2_700));
        assert_eq!(
            window.stats().rtt_ms,
            Some(210),
            "a new stream discards its cold first ack and holds the prior display"
        );

        window.record_sent(4, 3_000);
        assert!(window.record_ack(4, 3_210));
        assert_eq!(window.stats().rtt_ms, Some(210));

        window.record_sent(5, 3_400);
        assert!(window.record_ack(5, 3_615));
        assert_eq!(
            window.stats().rtt_ms,
            Some(210),
            "small jitter stays inside the display hysteresis"
        );

        window.record_sent(6, 4_000);
        assert!(window.record_ack(6, 4_600));
        let jumped = window.stats().rtt_ms.expect("large jump is reported");
        assert!(
            (400..600).contains(&jumped),
            "large deviation uses fast alpha and updates the reported value"
        );
    }

    #[test]
    fn link_monitor_emits_first_ack_publish_blackout_recovered() {
        let mut monitor =
            LinkMonitor::with_timeout(Duration::from_millis(100), LINK_BLACKOUT_AFTER);
        let blackout_after_ms = LINK_BLACKOUT_AFTER.as_millis() as u64;

        let first_probe = monitor.next_probe(1_000);
        let second_probe = monitor.next_probe(1_010);
        assert_eq!(
            monitor.check_blackout(1_000 + blackout_after_ms),
            None,
            "blackout requires at least one accepted ack"
        );

        let first = monitor.record_ack(first_probe.seq, 1_020);
        assert!(first.accepted);
        assert!(
            !first.reported_rtt_changed,
            "the first ack is accounted but keeps the badge warming"
        );
        assert_eq!(first.events, vec![LinkEvent::FirstAck]);
        assert_eq!(monitor.window.stats().rtt_ms, None);

        let second = monitor.record_ack(second_probe.seq, 1_040);
        assert!(second.accepted);
        assert!(
            second.reported_rtt_changed,
            "the second ack seeds the displayed RTT and should publish immediately"
        );
        assert!(second.events.is_empty());
        assert!(monitor.window.stats().rtt_ms.is_some());
        let stats = monitor.window.stats();
        assert_eq!(stats.window, 2);
        assert_eq!(stats.miss_pct, 0);

        assert_eq!(monitor.check_blackout(1_040 + blackout_after_ms - 1), None);
        assert_eq!(
            monitor.check_blackout(1_040 + blackout_after_ms),
            Some(LinkEvent::Blackout(LINK_BLACKOUT_AFTER))
        );
        assert_eq!(
            monitor.check_blackout(1_040 + blackout_after_ms + 1_000),
            None,
            "latched blackout events are not repeated"
        );

        let recovered_probe = monitor.next_probe(1_040 + blackout_after_ms + 1_100);
        let recovered = monitor.record_ack(recovered_probe.seq, 1_040 + blackout_after_ms + 1_120);
        assert!(recovered.accepted);
        assert_eq!(recovered.events, vec![LinkEvent::Recovered]);
    }

    #[test]
    fn link_protocol_serializes_and_builds_control_specs() {
        let probe = LinkProbe::new(
            42,
            1_000,
            LinkStats {
                rtt_ms: Some(42),
                miss_pct: 3,
                window: 12,
            },
        );
        let text = serde_json::to_string(&probe).unwrap();
        let parsed: LinkProbe = serde_json::from_str(&text).unwrap();
        assert!(parsed.version_ok());
        assert_eq!(parsed, probe);

        let ack = LinkAck::new(42);
        let ack_text = serde_json::to_string(&ack).unwrap();
        let parsed_ack = serde_json::from_str::<LinkAck>(&ack_text).unwrap();
        assert!(parsed_ack.version_ok());
        assert_eq!(parsed_ack.ports, None);
        assert_eq!(
            serde_json::from_str::<LinkAck>(r#"{"v":"rimz.link.v1","seq":42}"#)
                .unwrap()
                .ports,
            None
        );

        let ack = LinkAck::with_ports(43, vec![3000, 8080]);
        assert_eq!(
            serde_json::from_str::<LinkAck>(&serde_json::to_string(&ack).unwrap()).unwrap(),
            ack
        );
        #[derive(Deserialize)]
        struct OldLinkAck {
            v: String,
            seq: u64,
        }
        let old: OldLinkAck = serde_json::from_str(&serde_json::to_string(&ack).unwrap()).unwrap();
        assert_eq!(old.v, LINK_SCHEMA_VERSION);
        assert_eq!(old.seq, 43);

        let file = LinkStatsFile::new(2_000, "client-port server-port".to_owned(), probe.stats);
        let parsed: LinkStatsFile = serde_json::from_str(&serde_json::to_string(&file).unwrap())
            .expect("stats file round trips");
        assert!(parsed.version_ok());
        assert_eq!(parsed, file);

        let control = PathBuf::from("/tmp/rimz.sock");
        assert_eq!(
            super::super::control_options(&control),
            vec![
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/tmp/rimz.sock",
                "-o",
                "ControlPersist=no",
            ]
        );
        let path_spec = probe_stream_spec(&target("dev-box:~/code/query-engine"), &control);
        assert_eq!(path_spec.program, "ssh");
        assert_eq!(
            path_spec.args[0..5],
            ["-S", "/tmp/rimz.sock", "-o", "BatchMode=yes", "--"]
        );
        assert_eq!(path_spec.args[5], "dev-box");
        assert!(
            path_spec.args[6]
                .contains("rimz remote link-stats ingest --dir \"$HOME\"'/code/query-engine'")
        );
        let session_spec = probe_stream_spec(&target("dev-box:query-engine"), &control);
        assert!(
            session_spec.args[6].contains("rimz remote link-stats ingest --session 'query-engine'"),
            "{:?}",
            session_spec.args
        );

        let check = control_check_spec(&target("dev-box:query-engine"), &control);
        assert_eq!(check.program, "ssh");
        assert_eq!(
            check.args,
            [
                "-S",
                "/tmp/rimz.sock",
                "-O",
                "check",
                "-o",
                "BatchMode=yes",
                "--",
                "dev-box"
            ]
        );
    }

    #[test]
    fn validated_control_path_rejects_overlong_runtime_roots() {
        let runtime_root = Path::new("/tmp").join("d".repeat(sock::AF_UNIX_PATH_LIMIT));
        let control = control_path_under(&runtime_root, 12345);

        let err =
            validated_control_path_under(&runtime_root, 12345).expect_err("overlong control path");

        assert_eq!(err.path, control);
        assert!(err.used > err.limit);
        assert!(err.to_string().contains(sock::XDG_REMEDY));
    }
}
