//! Pure SSH endpoint discovery and reconnect-wait acceleration state.

use std::time::{Duration, Instant};

use crate::mux::CommandSpec;

use super::env_ms;

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

/// Whether the reconnect wait remains active or may attach now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitVerdict {
    KeepWaiting,
    AttachNow { network_restored: bool },
}

/// A reconnect wait that accelerates only after an observed network transition.
#[derive(Clone, Copy, Debug)]
pub struct DialGate {
    deadline: Instant,
    first_dial_reachable: Option<bool>,
    network_restored: bool,
}

impl DialGate {
    pub fn new(started: Instant, delay: Duration) -> Self {
        Self {
            deadline: started + delay,
            first_dial_reachable: None,
            network_restored: false,
        }
    }

    pub fn note_dial(&mut self, reachable: bool) {
        match self.first_dial_reachable {
            None => self.first_dial_reachable = Some(reachable),
            Some(false) if reachable => self.network_restored = true,
            Some(_) => {}
        }
    }

    pub fn verdict(&self, now: Instant) -> WaitVerdict {
        if now >= self.deadline {
            return WaitVerdict::AttachNow {
                network_restored: false,
            };
        }
        if self.network_restored {
            return WaitVerdict::AttachNow {
                network_restored: true,
            };
        }
        WaitVerdict::KeepWaiting
    }
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
    fn reachable_from_start_honors_backoff() {
        let started = Instant::now();
        let mut gate = DialGate::new(started, Duration::from_secs(10));
        gate.note_dial(true);
        gate.note_dial(false);
        gate.note_dial(true);

        assert_eq!(
            gate.verdict(started + Duration::from_secs(9)),
            WaitVerdict::KeepWaiting
        );
        assert_eq!(
            gate.verdict(started + Duration::from_secs(10)),
            WaitVerdict::AttachNow {
                network_restored: false
            }
        );
    }

    #[test]
    fn unreachable_to_reachable_transition_accelerates() {
        let started = Instant::now();
        let mut gate = DialGate::new(started, Duration::from_secs(10));
        gate.note_dial(false);
        assert_eq!(
            gate.verdict(started + Duration::from_secs(1)),
            WaitVerdict::KeepWaiting
        );

        gate.note_dial(true);
        assert_eq!(
            gate.verdict(started + Duration::from_secs(2)),
            WaitVerdict::AttachNow {
                network_restored: true
            }
        );
    }

    #[test]
    fn backoff_expiry_wins_without_a_transition() {
        let started = Instant::now();
        let mut gate = DialGate::new(started, Duration::from_secs(10));
        gate.note_dial(false);

        assert_eq!(
            gate.verdict(started + Duration::from_secs(10)),
            WaitVerdict::AttachNow {
                network_restored: false
            }
        );
    }
}
