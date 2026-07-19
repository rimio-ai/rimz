//! Remote listener discovery and live ControlMaster forward state.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use crate::mux::CommandSpec;

use super::RemoteTarget;

const MAX_CANDIDATE_PORTS: usize = 32;
const MAX_ACTIVE_FORWARDS: usize = 16;
const CLOSE_MISSING_REPORTS: u8 = 3;

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
    Close(u16),
}

/// Local diff state for listener reports across probe streams and reconnects.
#[derive(Debug, Default)]
pub struct PortSync {
    baseline: Option<BTreeSet<u16>>,
    active: BTreeMap<u16, u8>,
    parked: BTreeSet<u16>,
}

impl PortSync {
    pub fn observe(&mut self, report: &[u16]) -> Vec<PortAction> {
        let report = report.iter().copied().collect::<BTreeSet<_>>();
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(report);
            return Vec::new();
        };

        self.parked.retain(|port| report.contains(port));

        let mut actions = Vec::new();
        let mut closed = Vec::new();
        for (port, missing) in &mut self.active {
            if report.contains(port) {
                *missing = 0;
            } else {
                *missing = missing.saturating_add(1);
                if *missing >= CLOSE_MISSING_REPORTS {
                    closed.push(*port);
                }
            }
        }
        for port in closed {
            self.active.remove(&port);
            actions.push(PortAction::Close(port));
        }

        for port in report {
            if self.active.len() >= MAX_ACTIVE_FORWARDS {
                break;
            }
            if !baseline.contains(&port)
                && !self.active.contains_key(&port)
                && !self.parked.contains(&port)
            {
                self.active.insert(port, 0);
                actions.push(PortAction::Open(port));
            }
        }
        actions
    }

    /// Park a refused open until the listener disappears and later returns.
    pub fn mark_open_failed(&mut self, port: u16) {
        self.active.remove(&port);
        self.parked.insert(port);
    }

    /// Reopen active ports after the supervisor replaces the SSH master.
    pub fn reopen_active(&self) -> Vec<PortAction> {
        self.active.keys().copied().map(PortAction::Open).collect()
    }
}

pub fn open_spec(target: &RemoteTarget, control_path: &Path, port: u16) -> CommandSpec {
    control_spec(target, control_path, port, "forward")
}

pub fn cancel_spec(target: &RemoteTarget, control_path: &Path, port: u16) -> CommandSpec {
    control_spec(target, control_path, port, "cancel")
}

fn control_spec(
    target: &RemoteTarget,
    control_path: &Path,
    port: u16,
    operation: &str,
) -> CommandSpec {
    CommandSpec::new(super::ssh_program()).args([
        "-S".to_owned(),
        control_path.display().to_string(),
        "-O".to_owned(),
        operation.to_owned(),
        "-L".to_owned(),
        format!("127.0.0.1:{port}:localhost:{port}"),
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
        assert_eq!(sync.reopen_active(), [PortAction::Open(3000)]);
        assert!(sync.observe(&[1500]).is_empty());
        assert!(sync.observe(&[1500]).is_empty());
        assert_eq!(sync.observe(&[1500]), [PortAction::Close(3000)]);

        assert_eq!(sync.observe(&[1500, 4000]), [PortAction::Open(4000)]);
        sync.mark_open_failed(4000);
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
    }

    #[test]
    fn control_specs_open_and_cancel_the_same_loopback_forward() {
        let target = RemoteTarget::parse("dev-box:query-engine").unwrap();
        let control = PathBuf::from("/tmp/rimz.sock");
        assert_eq!(
            open_spec(&target, &control, 3000).args,
            [
                "-S",
                "/tmp/rimz.sock",
                "-O",
                "forward",
                "-L",
                "127.0.0.1:3000:localhost:3000",
                "-o",
                "BatchMode=yes",
                "--",
                "dev-box",
            ]
        );
        assert_eq!(
            cancel_spec(&target, &control, 3000).args[2..6],
            ["-O", "cancel", "-L", "127.0.0.1:3000:localhost:3000"]
        );
    }
}
