//! Remote listener discovery and live ControlMaster forward state.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use crate::mux::CommandSpec;

use super::RemoteTarget;

const MAX_CANDIDATE_PORTS: usize = 32;
const MAX_ACTIVE_FORWARDS: usize = 16;
const CLOSE_MISSING_REPORTS: u8 = 3;
const LOCAL_PORT_SCAN: u16 = 16;

/// Parse the room user's loopback and wildcard listeners from Linux procfs.
pub fn candidate_ports(tcp: &str, tcp6: &str, own_uid: u32) -> Vec<u16> {
    tcp.lines()
        .chain(tcp6.lines())
        .filter_map(|line| candidate_port(line, own_uid))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_CANDIDATE_PORTS)
        .collect()
}

fn candidate_port(line: &str, own_uid: u32) -> Option<u16> {
    let mut columns = line.split_whitespace();
    columns.next()?;
    let local = columns.next()?;
    columns.next()?;
    if columns.next()? != "0A" || columns.nth(3)?.parse::<u32>().ok()? != own_uid {
        return None;
    }
    let (address, port) = local.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    (port >= 1024 && loopback_or_wildcard(address)).then_some(port)
}

fn loopback_or_wildcard(address: &str) -> bool {
    let Some(mut bytes) = decode_hex(address) else {
        return false;
    };
    for word in bytes.chunks_exact_mut(4) {
        word.reverse();
    }
    match bytes.len() {
        4 => {
            let address = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
            address.is_loopback() || address.is_unspecified()
        }
        16 => {
            let Ok(octets) = <[u8; 16]>::try_from(bytes) else {
                return false;
            };
            let address = Ipv6Addr::from(octets);
            address.is_loopback() || address.is_unspecified()
        }
        _ => false,
    }
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !matches!(value.len(), 8 | 32) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortAction {
    Open(u16),
    Reopen(u16),
    Close { remote: u16, local: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ForwardState {
    Forwarded,
    Reopening,
    Limit,
    Refused { error: String },
    NoLocalPort,
}

impl ForwardState {
    fn occupies_slot(&self) -> bool {
        matches!(self, Self::Forwarded | Self::Reopening)
    }
}

#[derive(Clone, Copy, Debug)]
enum OpenOrigin {
    Report,
    MasterReplacement,
}

impl OpenOrigin {
    fn action(self, remote: u16) -> PortAction {
        match self {
            Self::Report => PortAction::Open(remote),
            Self::MasterReplacement => PortAction::Reopen(remote),
        }
    }
}

#[derive(Debug)]
struct ListenerForward {
    missing: u8,
    state: ForwardState,
    origin: OpenOrigin,
}

/// Local diff state for listener reports across probe streams and reconnects.
#[derive(Debug, Default)]
pub struct PortSync {
    baseline: Option<BTreeSet<u16>>,
    // Bounded by the current report plus active listeners awaiting their third miss.
    forwards: BTreeMap<u16, ListenerForward>,
    // Pins survive listener removal, bounded by distinct remote ports seen this connect.
    pins: BTreeMap<u16, u16>,
}

impl PortSync {
    pub fn observe(&mut self, report: &[u16]) -> Vec<PortAction> {
        let report = report.iter().copied().collect::<BTreeSet<_>>();
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(report);
            return Vec::new();
        };

        let mut actions = Vec::new();
        self.forwards.retain(|remote, forward| {
            if report.contains(remote) {
                forward.missing = 0;
                return true;
            }
            if !forward.state.occupies_slot() {
                return false;
            }
            forward.missing = forward.missing.saturating_add(1);
            if forward.missing < CLOSE_MISSING_REPORTS {
                return true;
            }
            if forward.state == ForwardState::Forwarded
                && let Some(local) = self.pins.get(remote)
            {
                actions.push(PortAction::Close {
                    remote: *remote,
                    local: *local,
                });
            }
            false
        });

        let mut active = self
            .forwards
            .values()
            .filter(|forward| forward.state.occupies_slot())
            .count();
        for remote in report.difference(baseline) {
            let forward = self.forwards.entry(*remote).or_insert(ListenerForward {
                missing: 0,
                state: ForwardState::Limit,
                origin: OpenOrigin::Report,
            });
            if forward.state.occupies_slot()
                || matches!(forward.state, ForwardState::Refused { .. })
            {
                continue;
            }
            if active >= MAX_ACTIVE_FORWARDS {
                forward.state = ForwardState::Limit;
                continue;
            }
            forward.state = ForwardState::Reopening;
            active += 1;
            actions.push(forward.origin.action(*remote));
        }
        actions
    }

    /// Park a refused open until the listener disappears and later returns.
    pub fn mark_open_failed(&mut self, remote: u16, error: String) {
        if let Some(forward) = self.forwards.get_mut(&remote) {
            forward.state = ForwardState::Refused { error };
        }
    }

    pub fn local_port(&self, remote: u16) -> Option<u16> {
        self.pins.get(&remote).copied()
    }

    pub fn state(&self, remote: u16) -> Option<&ForwardState> {
        self.forwards.get(&remote).map(|forward| &forward.state)
    }

    pub fn choose_local_port(
        &mut self,
        remote: u16,
        mut available: impl FnMut(u16) -> bool,
        mut fallback: impl FnMut() -> Option<u16>,
    ) -> Option<u16> {
        let first = self.local_port(remote).unwrap_or(remote);
        let reserved = |candidate| {
            self.pins
                .iter()
                .any(|(owner, local)| *owner != remote && *local == candidate)
        };
        let local = (first..=first.saturating_add(LOCAL_PORT_SCAN))
            .find(|candidate| !reserved(*candidate) && available(*candidate))
            .or_else(|| {
                for _ in 0..=self.pins.len() {
                    let candidate = fallback()?;
                    if !reserved(candidate) && available(candidate) {
                        return Some(candidate);
                    }
                }
                None
            });
        if let Some(local) = local {
            self.pins.insert(remote, local);
        } else if let Some(forward) = self.forwards.get_mut(&remote) {
            forward.state = ForwardState::NoLocalPort;
        }
        local
    }

    pub fn mark_opened(&mut self, remote: u16) {
        if let Some(forward) = self.forwards.get_mut(&remote) {
            forward.state = ForwardState::Forwarded;
        }
    }

    pub fn mark_master_lost(&mut self) {
        for forward in self.forwards.values_mut() {
            if forward.state == ForwardState::Forwarded {
                forward.state = ForwardState::Reopening;
                forward.origin = OpenOrigin::MasterReplacement;
            }
        }
    }

    /// Reopen active ports after the supervisor replaces the SSH master.
    pub fn reopen_active(&self) -> Vec<PortAction> {
        self.forwards
            .iter()
            .filter_map(|(remote, forward)| {
                (forward.state == ForwardState::Reopening).then_some(forward.origin.action(*remote))
            })
            .collect()
    }
}

pub fn open_spec(
    target: &RemoteTarget,
    control_path: &Path,
    local: u16,
    remote: u16,
) -> CommandSpec {
    control_spec(target, control_path, local, remote, "forward")
}

pub fn cancel_spec(
    target: &RemoteTarget,
    control_path: &Path,
    local: u16,
    remote: u16,
) -> CommandSpec {
    control_spec(target, control_path, local, remote, "cancel")
}

fn control_spec(
    target: &RemoteTarget,
    control_path: &Path,
    local: u16,
    remote: u16,
    operation: &str,
) -> CommandSpec {
    CommandSpec::new(super::ssh_program()).args([
        "-S".to_owned(),
        control_path.display().to_string(),
        "-O".to_owned(),
        operation.to_owned(),
        "-L".to_owned(),
        format!("127.0.0.1:{local}:localhost:{remote}"),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "--".to_owned(),
        target.ssh_destination().as_str().to_owned(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    fn row(local: &str, state: &str, uid: u32) -> String {
        format!(
            "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  {uid} 0 1"
        )
    }

    #[test]
    fn proc_parser_keeps_only_qualifying_listeners() {
        let tcp = [
            HEADER.to_owned(),
            row("0100007F:0BB8", "0A", 1000),
            row("0200007F:0BB9", "0A", 1000),
            row("00000000:1F90", "0A", 1000),
            row("0100007F:0050", "0A", 1000),
            row("0100007F:0BBA", "01", 1000),
            row("0100007F:0BBB", "0A", 1001),
            row("0200000A:0BBC", "0A", 1000),
            row("000000000:0BBD", "0A", 1000),
        ]
        .join("\n");
        let tcp6 = [
            HEADER.to_owned(),
            row("00000000000000000000000000000000:0BB8", "0A", 1000),
            row("00000000000000000000000001000000:2328", "0A", 1000),
            row("00000000000000000000000002000000:2329", "0A", 1000),
        ]
        .join("\n");

        assert_eq!(candidate_ports(&tcp, &tcp6, 1000), [3000, 3001, 8080, 9000]);
    }

    #[test]
    fn proc_parser_deduplicates_sorts_and_caps_reports() {
        let tcp = (1024..1064)
            .rev()
            .map(|port| row(&format!("0100007F:{port:04X}"), "0A", 1000))
            .chain(std::iter::once(row("00000000:0400", "0A", 1000)))
            .collect::<Vec<_>>()
            .join("\n");
        let ports = candidate_ports(&tcp, "", 1000);

        assert_eq!(ports.len(), MAX_CANDIDATE_PORTS);
        assert_eq!(ports[0], 1024);
        assert_eq!(ports[31], 1055);
    }

    #[test]
    fn port_sync_baselines_debounces_parks_and_reoffers() {
        let mut sync = PortSync::default();
        assert!(sync.observe(&[1500]).is_empty());
        assert_eq!(sync.observe(&[1500, 3000]), [PortAction::Open(3000)]);
        assert_eq!(sync.choose_local_port(3000, |_| true, || None), Some(3000));
        sync.mark_opened(3000);
        assert!(
            sync.reopen_active().is_empty(),
            "held forwards stay untouched"
        );
        assert!(sync.observe(&[1500]).is_empty());
        assert!(sync.observe(&[1500]).is_empty());
        assert_eq!(
            sync.observe(&[1500]),
            [PortAction::Close {
                remote: 3000,
                local: 3000
            }]
        );

        assert_eq!(sync.observe(&[1500, 4000]), [PortAction::Open(4000)]);
        sync.mark_open_failed(4000, "administratively prohibited".to_owned());
        assert_eq!(
            sync.state(4000),
            Some(&ForwardState::Refused {
                error: "administratively prohibited".to_owned()
            })
        );
        assert!(sync.observe(&[1500, 4000]).is_empty());
        assert!(sync.observe(&[1500]).is_empty());
        assert_eq!(sync.observe(&[1500, 4000]), [PortAction::Open(4000)]);
    }

    #[test]
    fn port_sync_caps_active_forwards() {
        let mut sync = PortSync::default();
        sync.observe(&[]);
        let report = (2000..2020).collect::<Vec<_>>();
        let actions = sync.observe(&report);

        assert_eq!(actions.len(), MAX_ACTIVE_FORWARDS);
        assert_eq!(actions.first(), Some(&PortAction::Open(2000)));
        assert_eq!(actions.last(), Some(&PortAction::Open(2015)));
        assert_eq!(sync.state(2019), Some(&ForwardState::Limit));
        sync.choose_local_port(2000, |_| true, || None);
        sync.mark_opened(2000);
        let report = (2001..2020).collect::<Vec<_>>();
        assert!(sync.observe(&report).is_empty());
        assert!(sync.observe(&report).is_empty());
        assert_eq!(
            sync.observe(&report),
            [
                PortAction::Close {
                    remote: 2000,
                    local: 2000
                },
                PortAction::Open(2016)
            ]
        );
    }

    #[test]
    fn port_sync_reopens_only_after_the_master_is_lost() {
        let mut sync = PortSync::default();
        sync.observe(&[]);
        assert_eq!(
            sync.observe(&[3000, 4000]),
            [PortAction::Open(3000), PortAction::Open(4000)]
        );
        sync.choose_local_port(3000, |port| port != 3000, || None);
        sync.choose_local_port(4000, |_| true, || None);
        sync.mark_opened(3000);
        sync.mark_opened(4000);
        sync.mark_master_lost();
        assert_eq!(sync.state(3000), Some(&ForwardState::Reopening));
        assert_eq!(
            sync.reopen_active(),
            [PortAction::Reopen(3000), PortAction::Reopen(4000)]
        );
        assert_eq!(sync.choose_local_port(3000, |_| true, || None), Some(3001));
        sync.mark_opened(3000);
        sync.mark_opened(4000);
        assert!(
            sync.reopen_active().is_empty(),
            "a replacement opens each forward once"
        );
        assert!(sync.observe(&[]).is_empty());
        assert!(sync.observe(&[]).is_empty());
        assert_eq!(
            sync.observe(&[]),
            [
                PortAction::Close {
                    remote: 3000,
                    local: 3001
                },
                PortAction::Close {
                    remote: 4000,
                    local: 4000
                }
            ]
        );
    }

    #[test]
    fn local_port_selection_scans_above_busy_ports() {
        let mut sync = PortSync::default();
        let mut tried = Vec::new();
        let local = sync.choose_local_port(
            3000,
            |port| {
                tried.push(port);
                port == 3002
            },
            || None,
        );
        assert_eq!(
            local,
            Some(3002),
            "busy candidates are remapped to the next free port"
        );
        assert_eq!(tried, [3000, 3001, 3002]);
    }

    #[test]
    fn local_port_pins_survive_close_and_reserve_the_port_for_the_listener() {
        let mut sync = PortSync::default();
        sync.observe(&[]);
        sync.observe(&[3000]);
        assert_eq!(
            sync.choose_local_port(3000, |port| port != 3000, || None),
            Some(3001)
        );
        sync.mark_opened(3000);
        sync.observe(&[]);
        sync.observe(&[]);
        assert_eq!(
            sync.observe(&[]),
            [PortAction::Close {
                remote: 3000,
                local: 3001
            }]
        );
        assert_eq!(
            sync.local_port(3000),
            Some(3001),
            "a closed listener keeps its pin"
        );
        sync.observe(&[3001]);
        assert_eq!(
            sync.choose_local_port(3001, |_| true, || None),
            Some(3002),
            "another remote never takes a dormant pin"
        );
        sync.mark_opened(3001);
        assert_eq!(sync.observe(&[3000, 3001]), [PortAction::Open(3000)]);
        assert_eq!(
            sync.choose_local_port(3000, |_| true, || None),
            Some(3001),
            "a returning listener keeps its URL even if its original port is free"
        );
    }

    #[test]
    fn local_port_selection_moves_a_busy_pin_upwards() {
        let mut sync = PortSync::default();
        assert_eq!(
            sync.choose_local_port(3000, |port| port != 3000, || None),
            Some(3001)
        );
        assert_eq!(
            sync.choose_local_port(3000, |port| port != 3001, || None),
            Some(3002),
            "reopen scans above the pin, not from the now-free remote port"
        );
        assert_eq!(sync.local_port(3000), Some(3002));
    }

    #[test]
    fn local_port_selection_uses_an_os_pick_after_the_short_scan() {
        let mut sync = PortSync::default();
        let mut tried = Vec::new();
        let local = sync.choose_local_port(
            3000,
            |port| {
                tried.push(port);
                port == 45000
            },
            || Some(45000),
        );
        assert_eq!(
            local,
            Some(45000),
            "the OS pick follows an exhausted nearby scan"
        );
        assert_eq!(tried.first(), Some(&3000));
        assert_eq!(tried.last(), Some(&45000));
        assert!(tried.len() > 2);
        assert_eq!(
            sync.choose_local_port(u16::MAX, |port| port == 45001, || Some(45001)),
            Some(45001),
            "the nearby scan does not wrap at the last port"
        );
    }

    #[test]
    fn local_port_selection_rejects_an_os_pick_reserved_by_another_remote() {
        let mut sync = PortSync::default();
        assert_eq!(
            sync.choose_local_port(4000, |port| port == 45000, || Some(45000)),
            Some(45000)
        );
        let mut picks = [45000, 45001].into_iter();
        assert_eq!(
            sync.choose_local_port(3000, |port| port >= 45000, || picks.next()),
            Some(45001)
        );
    }

    #[test]
    fn local_port_exhaustion_is_visible_and_retried_on_the_next_report() {
        let mut sync = PortSync::default();
        sync.observe(&[]);
        sync.observe(&[3000]);
        assert_eq!(sync.choose_local_port(3000, |_| false, || None), None);
        assert_eq!(sync.state(3000), Some(&ForwardState::NoLocalPort));
        assert_eq!(sync.observe(&[3000]), [PortAction::Open(3000)]);
    }

    #[test]
    fn control_specs_open_and_cancel_the_same_loopback_forward() {
        let target = RemoteTarget::parse("dev-box:query-engine").unwrap();
        let control = PathBuf::from("/tmp/rimz.sock");
        assert_eq!(
            open_spec(&target, &control, 3001, 3000).args,
            [
                "-S",
                "/tmp/rimz.sock",
                "-O",
                "forward",
                "-L",
                "127.0.0.1:3001:localhost:3000",
                "-o",
                "BatchMode=yes",
                "--",
                "dev-box",
            ]
        );
        assert_eq!(
            cancel_spec(&target, &control, 3001, 3000).args[2..6],
            ["-O", "cancel", "-L", "127.0.0.1:3001:localhost:3000"]
        );
    }

    #[test]
    fn master_reopen_keeps_its_origin_when_local_port_allocation_is_retried() {
        let mut sync = PortSync::default();
        sync.observe(&[]);
        sync.observe(&[3000]);
        sync.choose_local_port(3000, |_| true, || None);
        sync.mark_opened(3000);
        sync.mark_master_lost();
        assert_eq!(sync.reopen_active(), [PortAction::Reopen(3000)]);
        for _ in 0..2 {
            assert_eq!(sync.choose_local_port(3000, |_| false, || None), None);
            assert_eq!(
                sync.observe(&[3000]),
                [PortAction::Reopen(3000)],
                "an allocation retry must remain master recovery, not a new report open"
            );
        }
        assert_eq!(sync.choose_local_port(3000, |_| true, || None), Some(3000));
        sync.mark_opened(3000);
        assert!(sync.reopen_active().is_empty());
    }
}
