//! Pure accounting for `rimz pane bandwidth`.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;

use crate::ids::PaneId;
use crate::proc::ProcInfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSample {
    pub pane_id: PaneId,
    pub label: String,
    pub write_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConn {
    pub local: SocketEndpoint,
    pub peer: SocketEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketEndpoint {
    pub addr: String,
    pub port: u16,
}

impl SocketEndpoint {
    pub fn ss_filter_text(&self) -> String {
        if self.addr.contains(':') && !(self.addr.starts_with('[') && self.addr.ends_with(']')) {
            format!("[{}]:{}", self.addr, self.port)
        } else {
            format!("{}:{}", self.addr, self.port)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PaneRate {
    pub pane_id: PaneId,
    pub label: String,
    #[serde(rename = "write_bps")]
    pub bps: u64,
}

pub fn zellij_client_pids(procs: &[ProcInfo], session: &str, own_uid: Option<u32>) -> Vec<u32> {
    let Some(own_uid) = own_uid else {
        return Vec::new();
    };
    procs
        .iter()
        .filter(|proc| proc.real_uid == own_uid)
        .filter(|proc| zellij_client_cmdline(&proc.cmdline, session))
        .map(|proc| proc.pid)
        .collect()
}

fn zellij_client_cmdline(cmdline: &str, session: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    let Some(program) = tokens.next() else {
        return false;
    };
    if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("zellij")
    {
        return false;
    }
    let mut has_session = false;
    for token in tokens {
        if token == "--server" {
            return false;
        }
        let file_name = Path::new(token).file_name().and_then(|name| name.to_str());
        has_session |= token == session || file_name == Some(session);
    }
    has_session
}

pub fn parse_tmux_client_pids(raw: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

pub fn ssh_conns_for_client_pids(
    pids: &[u32],
    read_env: &dyn Fn(u32) -> Option<String>,
) -> Vec<SshConn> {
    pids.iter()
        .filter_map(|&pid| read_env(pid))
        .filter_map(|raw| parse_ssh_connection(&raw))
        .collect()
}

fn parse_ssh_connection(raw: &str) -> Option<SshConn> {
    let mut fields = raw.split_whitespace();
    let client_addr = fields.next()?;
    let client_port = parse_port(fields.next()?)?;
    let server_addr = fields.next()?;
    let server_port = parse_port(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some(SshConn {
        local: SocketEndpoint {
            addr: server_addr.to_owned(),
            port: server_port,
        },
        peer: SocketEndpoint {
            addr: client_addr.to_owned(),
            port: client_port,
        },
    })
}

fn parse_port(raw: &str) -> Option<u16> {
    raw.parse().ok()
}

pub fn parse_ss_counters(out: &str) -> Option<(u64, u64)> {
    let mut saw_counter = false;
    let mut tx = 0_u64;
    let mut rx = 0_u64;
    for token in out.split_whitespace() {
        if let Some(value) = parse_ss_counter_token(token, "bytes_acked:") {
            saw_counter = true;
            tx = tx.saturating_add(value);
        } else if let Some(value) = parse_ss_counter_token(token, "bytes_received:") {
            saw_counter = true;
            rx = rx.saturating_add(value);
        }
    }
    saw_counter.then_some((tx, rx))
}

fn parse_ss_counter_token(token: &str, prefix: &str) -> Option<u64> {
    token
        .strip_prefix(prefix)?
        .trim_end_matches([',', ';'])
        .parse()
        .ok()
}

pub fn children_by_parent(procs: &[ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::new();
    for proc in procs {
        children
            .entry(proc.ppid)
            .or_insert_with(Vec::new)
            .push(proc.pid);
    }
    children
}

pub fn subtree_pids(children_by_parent: &HashMap<u32, Vec<u32>>, root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        if let Some(children) = children_by_parent.get(&pid) {
            queue.extend(children);
        }
    }
    out
}

pub fn rates(t0: &[PaneSample], t1: &[PaneSample], secs: f64) -> Vec<PaneRate> {
    if secs <= 0.0 {
        return Vec::new();
    }
    let end_by_pane: HashMap<&PaneId, &PaneSample> =
        t1.iter().map(|sample| (&sample.pane_id, sample)).collect();
    t0.iter()
        .filter_map(|start| {
            let end = end_by_pane.get(&start.pane_id)?;
            let delta = end.write_bytes.saturating_sub(start.write_bytes);
            Some(PaneRate {
                pane_id: start.pane_id.clone(),
                label: end.label.clone(),
                bps: bps(delta, secs),
            })
        })
        .collect()
}

pub fn wire_rates(t0: Option<(u64, u64)>, t1: Option<(u64, u64)>, secs: f64) -> Option<(u64, u64)> {
    if secs <= 0.0 {
        return None;
    }
    let (tx0, rx0) = t0?;
    let (tx1, rx1) = t1?;
    Some((
        bps(tx1.saturating_sub(tx0), secs),
        bps(rx1.saturating_sub(rx0), secs),
    ))
}

fn bps(delta: u64, secs: f64) -> u64 {
    let rate = (delta as f64 / secs).round();
    if rate >= u64::MAX as f64 {
        u64::MAX
    } else {
        rate as u64
    }
}

pub fn sorted_rates(mut rows: Vec<PaneRate>) -> Vec<PaneRate> {
    rows.sort_by(|a, b| {
        b.bps
            .cmp(&a.bps)
            .then_with(|| a.pane_id.as_str().cmp(b.pane_id.as_str()))
    });
    rows
}

pub fn total_bps(rows: &[PaneRate]) -> u64 {
    rows.iter()
        .fold(0_u64, |total, row| total.saturating_add(row.bps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32) -> ProcInfo {
        proc_with(pid, ppid, 1000, &format!("proc-{pid}"))
    }

    fn proc_with(pid: u32, ppid: u32, real_uid: u32, cmdline: &str) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            real_uid,
            cmdline: cmdline.to_owned(),
        }
    }

    fn pane_sample(pane: &str, label: &str, bytes: u64) -> PaneSample {
        PaneSample {
            pane_id: PaneId::parse(pane).expect("valid pane id"),
            label: label.to_owned(),
            write_bytes: bytes,
        }
    }

    fn pane_rate(pane: &str, label: &str, bps: u64) -> PaneRate {
        PaneRate {
            pane_id: PaneId::parse(pane).expect("valid pane id"),
            label: label.to_owned(),
            bps,
        }
    }

    #[test]
    fn subtree_pids_walks_root_descendants_only() {
        let procs = [
            proc(10, 1),
            proc(11, 10),
            proc(12, 11),
            proc(13, 10),
            proc(20, 1),
            proc(21, 20),
        ];

        assert_eq!(
            subtree_pids(&children_by_parent(&procs), 10),
            vec![10, 11, 13, 12]
        );
    }

    #[test]
    fn subtree_pids_includes_missing_root() {
        assert_eq!(subtree_pids(&HashMap::new(), 42), vec![42]);
    }

    #[test]
    fn parse_ssh_connection_maps_remote_socket_to_client_peer() {
        assert_eq!(
            parse_ssh_connection("203.0.113.10 54321 10.0.0.5 22"),
            Some(SshConn {
                local: SocketEndpoint {
                    addr: "10.0.0.5".to_owned(),
                    port: 22
                },
                peer: SocketEndpoint {
                    addr: "203.0.113.10".to_owned(),
                    port: 54321
                },
            })
        );
        assert_eq!(parse_ssh_connection("203.0.113.10 54321 10.0.0.5"), None);
        assert_eq!(
            parse_ssh_connection("203.0.113.10 not-a-port 10.0.0.5 22"),
            None
        );
        assert_eq!(
            parse_ssh_connection("203.0.113.10 54321 10.0.0.5 22 extra"),
            None
        );
    }

    #[test]
    fn socket_endpoint_brackets_ipv6_for_ss_filter() {
        let conn = parse_ssh_connection("2001:db8::1 54321 2001:db8::2 22").unwrap();

        assert_eq!(conn.local.ss_filter_text(), "[2001:db8::2]:22");
        assert_eq!(conn.peer.ss_filter_text(), "[2001:db8::1]:54321");
    }

    #[test]
    fn parse_ss_counters_sums_tcp_info_tokens() {
        let out = "\
ESTAB 0 0 10.0.0.5:22 203.0.113.10:54321
\t cubic wscale:7,7 rto:204 bytes_acked:4096 bytes_received:2048
ESTAB 0 0 10.0.0.5:22 203.0.113.11:54322
\t cubic wscale:7,7 rto:204 bytes_acked:6 bytes_received:2
";

        assert_eq!(parse_ss_counters(out), Some((4_102, 2_050)));
        assert_eq!(parse_ss_counters(""), None);
        assert_eq!(parse_ss_counters("ESTAB no counters here"), None);
    }

    #[test]
    fn zellij_client_pids_filter_session_clients() {
        let procs = [
            proc_with(10, 1, 1000, "/usr/bin/zellij attach --create rimz-room"),
            proc_with(11, 1, 1000, "zellij --server /tmp/rimz-room"),
            proc_with(12, 1, 1001, "zellij attach --create rimz-room"),
            proc_with(13, 1, 1000, "zellij attach --create other-room"),
            proc_with(14, 1, 1000, "bash -lc zellijish rimz-room"),
            proc_with(
                15,
                1,
                1000,
                "rimz sidebar serve --mux zellij --session-name rimz-room",
            ),
        ];

        assert_eq!(
            zellij_client_pids(&procs, "rimz-room", Some(1000)),
            vec![10]
        );
        assert!(zellij_client_pids(&procs, "rimz-room", None).is_empty());
    }

    #[test]
    fn ssh_conns_for_client_pids_reads_ssh_connection_env() {
        let read_env = |pid| match pid {
            10 => Some("203.0.113.10 54321 10.0.0.5 22".to_owned()),
            11 => Some("malformed".to_owned()),
            _ => None,
        };

        assert_eq!(
            ssh_conns_for_client_pids(&[10, 11, 12], &read_env),
            vec![SshConn {
                local: SocketEndpoint {
                    addr: "10.0.0.5".to_owned(),
                    port: 22
                },
                peer: SocketEndpoint {
                    addr: "203.0.113.10".to_owned(),
                    port: 54321
                },
            }]
        );
    }

    #[test]
    fn rates_match_existing_panes_and_saturate_delta() {
        let t0 = [
            pane_sample("tmux:%1", "btop", 1_000),
            pane_sample("tmux:%2", "codex", 5_000),
            pane_sample("tmux:%3", "gone", 10),
        ];
        let t1 = [
            pane_sample("tmux:%1", "btop", 11_000),
            pane_sample("tmux:%2", "codex", 4_000),
            pane_sample("tmux:%4", "new", 99_000),
        ];

        assert_eq!(
            rates(&t0, &t1, 5.0),
            vec![
                pane_rate("tmux:%1", "btop", 2_000),
                pane_rate("tmux:%2", "codex", 0)
            ]
        );
    }

    #[test]
    fn rates_reject_zero_window_and_handle_short_window() {
        let t0 = [pane_sample("zellij:terminal_1", "app", 100)];
        let t1 = [pane_sample("zellij:terminal_1", "app", 200)];

        assert!(rates(&t0, &t1, 0.0).is_empty());
        assert_eq!(
            rates(&t0, &t1, 0.05),
            vec![pane_rate("zellij:terminal_1", "app", 2_000)]
        );
    }

    #[test]
    fn wire_rates_require_two_socket_snapshots() {
        assert_eq!(
            wire_rates(Some((100, 200)), Some((600, 250)), 5.0),
            Some((100, 10))
        );
        assert_eq!(wire_rates(None, Some((600, 250)), 5.0), None);
        assert_eq!(
            wire_rates(Some((100, 200)), Some((50, 150)), 5.0),
            Some((0, 0))
        );
    }
}
