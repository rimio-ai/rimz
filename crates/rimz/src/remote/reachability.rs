//! Pure SSH endpoint discovery and reachability-gated reconnect-wait state.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::mux::CommandSpec;

use super::{ReconnectPolicy, env_ms};

pub const DIAL_INTERVAL: Duration = Duration::from_secs(1);
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

const DIAL_INTERVAL_ENV: &str = "RIMZ_REMOTE_DIAL_MS";

/// The configured reachability-check cadence. A zero value disables endpoint
/// discovery and all TCP dials.
pub fn dial_interval_from_env() -> Option<Duration> {
    match env_ms(DIAL_INTERVAL_ENV) {
        Some(ms) if ms.is_zero() => None,
        Some(ms) => Some(ms),
        None => Some(DIAL_INTERVAL),
    }
}

/// Ask OpenSSH for its effective destination after applying user config.
pub fn ssh_config_query_spec(destination: &str) -> CommandSpec {
    CommandSpec::new(super::ssh_program()).args([
        "-G".to_owned(),
        "--".to_owned(),
        destination.to_owned(),
    ])
}

/// The direct TCP endpoint OpenSSH would connect to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialPlan {
    pub host: String,
    pub port: u16,
}

/// Parse the relevant subset of lowercase `ssh -G` output.
///
/// Proxy-based connections opt out because their reachable endpoint differs
/// from the final SSH hostname and port.
pub fn parse_dial_plan(stdout: &str) -> Option<DialPlan> {
    let mut host = None;
    let mut port = None;
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            continue;
        };
        let Some(value) = fields.next() else {
            continue;
        };
        match key {
            "hostname" => host = Some(value.to_owned()),
            "port" => port = value.parse::<u16>().ok(),
            "proxyjump" | "proxycommand" if value != "none" => return None,
            _ => {}
        }
    }

    Some(DialPlan {
        host: host.filter(|host| !host.is_empty())?,
        port: port?,
    })
}

/// What the recovery footer communicates at one supervisor tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterPhase {
    WaitingForNetwork,
    Connecting,
    NextAttemptIn(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeState {
    Unknown,
    Up,
    Down,
}

/// Pure reconnect pacing fused from the configured reachability probes.
///
/// Unknown probe results remain optimistic. The network is down only after
/// every configured probe has reported down, and any positive result keeps it
/// up. The supervisor owns the clock and supplies every observation instant.
#[derive(Clone, Copy, Debug)]
pub struct AttemptPacer {
    policy: ReconnectPolicy,
    pace_started: Instant,
    next_attempt: Option<Instant>,
    internet: Option<ProbeState>,
    server: Option<ProbeState>,
    fingerprint: Option<Option<IpAddr>>,
    failures_since_reset: u32,
}

impl AttemptPacer {
    pub fn new(
        policy: ReconnectPolicy,
        started: Instant,
        internet_configured: bool,
        server_configured: bool,
    ) -> Self {
        Self {
            policy,
            pace_started: started,
            next_attempt: None,
            internet: internet_configured.then_some(ProbeState::Unknown),
            server: server_configured.then_some(ProbeState::Unknown),
            fingerprint: None,
            failures_since_reset: 0,
        }
    }

    pub fn note_internet(&mut self, up: bool, now: Instant) {
        let before = self.network_up();
        if let Some(state) = &mut self.internet {
            *state = probe_state(up);
        }
        self.note_network_edge(before, now);
    }

    pub fn note_server(&mut self, up: bool, now: Instant) {
        let before = self.network_up();
        if let Some(state) = &mut self.server {
            *state = probe_state(up);
        }
        self.note_network_edge(before, now);
    }

    /// Record the local route address. The first sample establishes a
    /// baseline; every later edge resets pacing and schedules immediately.
    pub fn note_fingerprint(&mut self, fingerprint: Option<IpAddr>, now: Instant) {
        if self
            .fingerprint
            .is_some_and(|previous| previous != fingerprint)
        {
            self.reset(now);
        }
        self.fingerprint = Some(fingerprint);
    }

    /// Schedule the next attempt after a failed foreground or background SSH
    /// process.
    pub fn note_attempt_failed(&mut self, now: Instant) {
        let delay = if self.network_up() {
            self.policy
                .reachable_delay(now.saturating_duration_since(self.pace_started))
        } else {
            let delay = self.policy.unreachable_delay(self.failures_since_reset);
            self.failures_since_reset = self.failures_since_reset.saturating_add(1);
            delay
        };
        self.next_attempt = Some(now + delay);
    }

    pub fn begin_attempt(&mut self, _now: Instant) {
        self.next_attempt = None;
    }

    pub fn may_attempt(&self, now: Instant) -> bool {
        self.next_attempt.is_some_and(|deadline| now >= deadline)
    }

    pub fn network_up(&self) -> bool {
        [self.internet, self.server]
            .into_iter()
            .flatten()
            .any(|state| state != ProbeState::Down)
            || self.internet.is_none() && self.server.is_none()
    }

    pub fn footer(&self, now: Instant, attempt_in_flight: bool) -> FooterPhase {
        if !self.network_up() {
            return FooterPhase::WaitingForNetwork;
        }
        let remaining = self.next_attempt.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(now)
        });
        if attempt_in_flight || remaining <= Duration::from_secs(5) {
            FooterPhase::Connecting
        } else {
            FooterPhase::NextAttemptIn(remaining)
        }
    }

    fn note_network_edge(&mut self, was_up: bool, now: Instant) {
        match (was_up, self.network_up()) {
            (false, true) => self.reset(now),
            (true, false) => {
                self.failures_since_reset = 1;
                self.next_attempt = Some(now + self.policy.unreachable_delay(0));
            }
            _ => {}
        }
    }

    fn reset(&mut self, now: Instant) {
        self.pace_started = now;
        self.failures_since_reset = 0;
        self.next_attempt = Some(now);
    }
}

fn probe_state(up: bool) -> ProbeState {
    if up { ProbeState::Up } else { ProbeState::Down }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_direct_ssh_endpoints_and_port_overrides() {
        assert_eq!(
            parse_dial_plan("host dev\nhostname 192.0.2.10\nport 22\nproxyjump none\n"),
            Some(DialPlan {
                host: "192.0.2.10".to_owned(),
                port: 22,
            })
        );
        assert_eq!(
            parse_dial_plan("hostname dev.internal\nport 2202\nproxycommand none\n"),
            Some(DialPlan {
                host: "dev.internal".to_owned(),
                port: 2202,
            })
        );
    }

    #[test]
    fn proxy_and_unparseable_configs_have_no_dial_plan() {
        assert_eq!(
            parse_dial_plan("hostname dev\nport 22\nproxyjump bastion\n"),
            None
        );
        assert_eq!(
            parse_dial_plan("hostname dev\nport 22\nproxycommand ssh -W %h:%p bastion\n"),
            None
        );
        assert_eq!(parse_dial_plan("this is not ssh config\n"), None);
        assert_eq!(parse_dial_plan("hostname dev\nport nope\n"), None);
    }

    #[test]
    fn network_down_uses_the_safety_ladder() {
        let started = Instant::now();
        let policy = ReconnectPolicy::default();
        let mut pacer = AttemptPacer::new(policy, started, true, true);
        pacer.note_attempt_failed(started);
        pacer.note_internet(false, started);
        pacer.note_server(false, started);

        assert_eq!(pacer.footer(started, false), FooterPhase::WaitingForNetwork);
        let mut due = started;
        for seconds in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 30, 30] {
            due += Duration::from_secs(seconds);
            assert!(pacer.may_attempt(due), "attempt after {seconds}s");
            pacer.begin_attempt(due);
            pacer.note_attempt_failed(due);
        }
    }

    #[test]
    fn network_edges_reset_and_fire_immediately() {
        let started = Instant::now();
        let mut pacer = AttemptPacer::new(ReconnectPolicy::default(), started, false, true);
        pacer.note_attempt_failed(started);
        pacer.note_server(false, started);
        assert!(!pacer.may_attempt(started));

        let restored = started + Duration::from_millis(100);
        pacer.note_server(true, restored);
        assert!(pacer.may_attempt(restored));
        pacer.begin_attempt(restored);
        pacer.note_attempt_failed(restored);
        assert!(pacer.may_attempt(restored + Duration::from_secs(2)));

        pacer.note_fingerprint(Some("192.0.2.2".parse().unwrap()), restored);
        let changed = restored + Duration::from_millis(10);
        pacer.note_fingerprint(None, changed);
        assert!(pacer.may_attempt(changed));
    }

    #[test]
    fn footer_counts_down_only_past_five_seconds() {
        let started = Instant::now();
        let policy = ReconnectPolicy {
            flat_window: Duration::ZERO,
            ..ReconnectPolicy::default()
        };
        let mut pacer = AttemptPacer::new(policy, started, false, false);

        pacer.note_attempt_failed(started);
        assert_eq!(
            pacer.footer(started, false),
            FooterPhase::Connecting,
            "the first ramp delay is four seconds"
        );
        let later = started + Duration::from_secs(60);
        pacer.note_attempt_failed(later);
        assert_eq!(
            pacer.footer(later, false),
            FooterPhase::NextAttemptIn(Duration::from_secs(8))
        );
        assert_eq!(pacer.footer(later, true), FooterPhase::Connecting);
    }
}
