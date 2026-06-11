//! Remote-link health protocol and pure probe accounting.
//!
//! The remote supervisor owns process I/O. This module only names the JSONL
//! probe/ack/file schema, classifies link health, builds SSH argv fragments, and
//! maintains the rolling probe window.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mux::CommandSpec;

use super::{RemoteSpec, RemoteTarget, quote_remote_path, remote_path_prefix, sh_quote};

pub const LINK_SCHEMA_VERSION: &str = "rimz.link.v1";
pub const LINK_STATS_FILE: &str = "link-stats.json";
pub const LINK_PROBE_INTERVAL: Duration = Duration::from_secs(2);
pub const LINK_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub const LINK_BLACKOUT_AFTER: Duration = Duration::from_secs(8);
pub const LINK_WINDOW: usize = 30;

const PROBE_INTERVAL_ENV: &str = "RIMZ_REMOTE_PROBE_MS";
const PROBE_TIMEOUT_ENV: &str = "RIMZ_REMOTE_PROBE_TIMEOUT_MS";

/// Link-health tier for notifications, diagnostics, and CLI health output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTier {
    Good,
    Degraded,
    Bad,
}

/// Four-rung display gradient for the sidebar link badge.
///
/// Alerting uses [`LinkTier`]. The badge keeps a calmer ladder so a healthy
/// remote link recedes while latency and loss still move visibly through
/// yellow, amber, and red.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkBadgeLevel {
    Calm,
    Minor,
    Major,
    Critical,
}

/// Rolling link measurements published by the local probe stream and folded by
/// the remote sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    pub miss_pct: u16,
    pub window: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_bps: Option<u64>,
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
    pub fn new(seq: u64, sent_at_ms: u64, stats: LinkStats) -> Self {
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
}

impl LinkAck {
    pub fn new(seq: u64) -> Self {
        Self {
            v: LINK_SCHEMA_VERSION.to_owned(),
            seq,
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

/// Display level for the footer badge, computed as the worse of latency and
/// probe-loss display bands.
pub fn link_badge_level(rtt_ms: Option<u32>, miss_pct: u16) -> LinkBadgeLevel {
    let rtt = match rtt_ms {
        Some(ms) if ms > 500 => LinkBadgeLevel::Critical,
        Some(ms) if ms > 300 => LinkBadgeLevel::Major,
        Some(ms) if ms > 150 => LinkBadgeLevel::Minor,
        _ => LinkBadgeLevel::Calm,
    };
    let loss = if miss_pct > 30 {
        LinkBadgeLevel::Critical
    } else if miss_pct > 20 {
        LinkBadgeLevel::Major
    } else if miss_pct > 10 {
        LinkBadgeLevel::Minor
    } else {
        LinkBadgeLevel::Calm
    };
    rtt.max(loss)
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

/// PID-scoped SSH ControlMaster socket path. Concurrent `rimz remote connect`
/// invocations never share a master.
pub fn control_path() -> PathBuf {
    crate::ledger::paths::runtime_home()
        .join("rimz")
        .join("link")
        .join(format!("link-{}.sock", std::process::id()))
}

pub fn control_options(path: &Path) -> Vec<String> {
    vec![
        "-o".to_owned(),
        "ControlMaster=auto".to_owned(),
        "-o".to_owned(),
        format!("ControlPath={}", path.display()),
    ]
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
        target.destination.clone(),
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
    spec = spec.arg(target.destination.clone());
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

fn env_ms(key: &str) -> Option<Duration> {
    std::env::var(key)
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
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
pub struct ProbeWindow {
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
    pub fn with_timeout(timeout: Duration) -> Self {
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
    pub fn begin_stream(&mut self) {
        self.discard_next_ack_sample = true;
    }

    pub fn record_sent(&mut self, seq: u64, at_ms: u64) {
        self.outcomes.push_back(ProbeOutcome::Pending {
            seq,
            sent_at_ms: at_ms,
        });
        self.trim();
    }

    pub fn record_ack(&mut self, seq: u64, at_ms: u64) -> bool {
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

    pub fn expire(&mut self, now_ms: u64) {
        let timeout_ms = self.timeout.as_millis() as u64;
        for outcome in &mut self.outcomes {
            if let ProbeOutcome::Pending { sent_at_ms, .. } = *outcome
                && now_ms.saturating_sub(sent_at_ms) >= timeout_ms
            {
                *outcome = ProbeOutcome::Miss { sent_at_ms };
            }
        }
    }

    pub fn stats(&self) -> LinkStats {
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
            bandwidth_bps: None,
        }
    }

    pub fn reported_rtt_ms(&self) -> Option<u32> {
        self.reported_ms
    }

    pub fn blackout_ms(&self, now_ms: u64) -> u64 {
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
    fn tier_boundaries_are_exact() {
        assert_eq!(link_tier(Some(150), 0), LinkTier::Good);
        assert_eq!(link_tier(Some(151), 0), LinkTier::Degraded);
        assert_eq!(link_tier(Some(400), 0), LinkTier::Degraded);
        assert_eq!(link_tier(Some(401), 0), LinkTier::Bad);
        assert_eq!(link_tier(Some(10), 0), LinkTier::Good);
        assert_eq!(link_tier(Some(10), 1), LinkTier::Degraded);
        assert_eq!(link_tier(Some(10), 10), LinkTier::Degraded);
        assert_eq!(link_tier(Some(10), 11), LinkTier::Bad);
    }

    #[test]
    fn badge_level_boundaries_are_exact() {
        assert_eq!(link_badge_level(None, 0), LinkBadgeLevel::Calm);
        assert_eq!(link_badge_level(Some(150), 0), LinkBadgeLevel::Calm);
        assert_eq!(link_badge_level(Some(151), 0), LinkBadgeLevel::Minor);
        assert_eq!(link_badge_level(Some(300), 0), LinkBadgeLevel::Minor);
        assert_eq!(link_badge_level(Some(301), 0), LinkBadgeLevel::Major);
        assert_eq!(link_badge_level(Some(500), 0), LinkBadgeLevel::Major);
        assert_eq!(link_badge_level(Some(501), 0), LinkBadgeLevel::Critical);
        assert_eq!(link_badge_level(Some(10), 10), LinkBadgeLevel::Calm);
        assert_eq!(link_badge_level(Some(10), 11), LinkBadgeLevel::Minor);
        assert_eq!(link_badge_level(Some(10), 20), LinkBadgeLevel::Minor);
        assert_eq!(link_badge_level(Some(10), 21), LinkBadgeLevel::Major);
        assert_eq!(link_badge_level(Some(10), 30), LinkBadgeLevel::Major);
        assert_eq!(link_badge_level(Some(10), 31), LinkBadgeLevel::Critical);
        assert_eq!(link_badge_level(Some(180), 31), LinkBadgeLevel::Critical);
    }

    #[test]
    fn probe_window_tracks_ack_miss_late_ack_and_blackout() {
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
    }

    #[test]
    fn first_ack_is_accounted_but_not_reported() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 0);
        assert!(window.record_ack(1, 740));
        assert_eq!(
            window.stats(),
            LinkStats {
                rtt_ms: None,
                miss_pct: 0,
                window: 1,
                bandwidth_bps: None,
            }
        );
        window.record_sent(2, 1_000);
        assert!(window.record_ack(2, 1_210));
        assert_eq!(window.stats().rtt_ms, Some(210));
    }

    #[test]
    fn begin_stream_rearms_cold_ack_discard_without_resetting_reported_rtt() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 0);
        assert!(window.record_ack(1, 740));
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
    }

    #[test]
    fn adaptive_ewma_holds_small_wander_and_snaps_on_jump() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 0);
        assert!(window.record_ack(1, 100));
        window.record_sent(2, 200);
        assert!(window.record_ack(2, 300));
        assert_eq!(window.stats().rtt_ms, Some(100));

        window.record_sent(3, 400);
        assert!(window.record_ack(3, 505));
        assert_eq!(
            window.stats().rtt_ms,
            Some(100),
            "small jitter stays inside the display hysteresis"
        );

        window.record_sent(4, 600);
        assert!(window.record_ack(4, 1_200));
        assert_eq!(
            window.stats().rtt_ms,
            Some(400),
            "large deviation uses fast alpha and updates the reported value"
        );
    }

    #[test]
    fn ewma_window_is_capped() {
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        window.record_sent(1, 0);
        assert!(window.record_ack(1, 100));
        window.record_sent(2, 200);
        assert!(window.record_ack(2, 300));
        for seq in 3..=(LINK_WINDOW as u64 + 5) {
            window.record_sent(seq, seq * 10);
            assert!(window.record_ack(seq, seq * 10 + 1));
        }
        assert_eq!(usize::from(window.stats().window), LINK_WINDOW);
    }

    #[test]
    fn serde_round_trips_schema_shapes() {
        let probe = LinkProbe::new(
            42,
            1_000,
            LinkStats {
                rtt_ms: Some(42),
                miss_pct: 3,
                window: 12,
                bandwidth_bps: None,
            },
        );
        let text = serde_json::to_string(&probe).unwrap();
        let parsed: LinkProbe = serde_json::from_str(&text).unwrap();
        assert!(parsed.version_ok());
        assert_eq!(parsed, probe);

        let ack = LinkAck::new(42);
        assert!(
            serde_json::from_str::<LinkAck>(&serde_json::to_string(&ack).unwrap())
                .unwrap()
                .version_ok()
        );
    }

    #[test]
    fn control_options_and_probe_spec_are_stable() {
        let control = PathBuf::from("/tmp/rimz.sock");
        assert_eq!(
            control_options(&control),
            vec![
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/tmp/rimz.sock",
            ]
        );
        let spec = probe_stream_spec(&target("dev-box:~/code/query-engine"), &control);
        assert_eq!(spec.program, "ssh");
        assert_eq!(
            spec.args[0..5],
            ["-S", "/tmp/rimz.sock", "-o", "BatchMode=yes", "--"]
        );
        assert_eq!(spec.args[5], "dev-box");
        assert!(
            spec.args[6]
                .contains("rimz remote link-stats ingest --dir \"$HOME\"'/code/query-engine'")
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
}
