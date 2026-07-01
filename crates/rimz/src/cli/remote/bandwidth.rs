use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rimz::ids::{MuxName, PaneId};
use rimz::mux::PaneListOptions;
use rimz::pane::PaneRef;
use rimz::proc::ProcInfo;
use rimz::workspace::WorkspaceResolver;

use crate::cli::GlobalFlags;
use crate::cli::render;

const DEFAULT_LABEL_WIDTH: usize = 56;
const UNAVAILABLE_NOTICE: &str =
    "rimz remote bandwidth needs /proc on the host serving the room (Linux host).";
const NO_PANE_PIDS_NOTICE: &str = "rimz remote bandwidth could not resolve any pane root process.";
const ZELLIJ_NO_PANE_PIDS_NOTICE: &str = concat!(
    "rimz remote bandwidth could not resolve any pane root process. ",
    "On Zellij, a pane resolves only while it runs a live, uniquely named foreground process; ",
    "idle look-alike shells are skipped."
);
const IO_UNREADABLE_NOTICE: &str = concat!(
    "rimz remote bandwidth resolved pane processes but could not read /proc/<pid>/io. ",
    "The room may be served by another user, or the kernel lacks CONFIG_TASK_IO_ACCOUNTING."
);
const REPORT_CAVEAT: &str = concat!(
    "per-pane rows are producer write-rate; muxes diff the focused tab and SSH compresses it, ",
    "so WIRE(ssh) is the actual TCP payload on this room's SSH socket, usually far below the ",
    "per-pane sum. WIRE is absent for local rooms."
);
const SSH_CONNECTION_ENV: &str = "SSH_CONNECTION";
const WIRE_TX_PANE: &str = "WIRE(ssh↑)";
const WIRE_RX_PANE: &str = "WIRE(ssh↓)";
const WIRE_TX_LABEL: &str = "ssh egress → client(s)";
const WIRE_RX_LABEL: &str = "ssh ingress ← client(s)";

struct PaneProfile {
    pane_id: PaneId,
    label: String,
    pids: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneSample {
    pane_id: PaneId,
    label: String,
    write_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SshConn {
    local: SocketEndpoint,
    peer: SocketEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SocketEndpoint {
    addr: String,
    port: u16,
}

impl SocketEndpoint {
    fn ss_filter_text(&self) -> String {
        if self.addr.contains(':') && !(self.addr.starts_with('[') && self.addr.ends_with(']')) {
            format!("[{}]:{}", self.addr, self.port)
        } else {
            format!("{}:{}", self.addr, self.port)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct PaneRate {
    pane_id: PaneId,
    label: String,
    #[serde(rename = "write_bps")]
    bps: u64,
}

#[derive(serde::Serialize)]
struct BandwidthJson<'a> {
    available: bool,
    sample_secs: f64,
    total_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    wire_tx_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wire_rx_bps: Option<u64>,
    panes: &'a [PaneRate],
    caveat: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

pub(super) fn run(secs: u64, json: bool, globals: &GlobalFlags) -> Result<()> {
    if secs == 0 {
        bail!("--secs must be greater than 0");
    }

    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the current room")?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let mut panes = backend
        .list_panes(PaneListOptions {
            session_name: Some(workspace.session_name.clone()),
            ..Default::default()
        })?
        .panes;
    let proc_snapshot = rimz::proc::list_processes();
    if proc_snapshot.is_empty() {
        return emit_unavailable(secs as f64, json, UNAVAILABLE_NOTICE);
    }

    let children = children_by_parent(&proc_snapshot);
    if mux == MuxName::Zellij
        && let Some(mut resolver) = rimz::mux::zellij::ZellijPaneResolver::new(
            &proc_snapshot,
            &children,
            &workspace.session_name,
            rimz::proc::own_uid(),
        )
    {
        for pane in &mut panes {
            if pane.pane_pid.is_none()
                && let Some(command) = pane.command.as_deref()
            {
                pane.pane_pid =
                    resolver.resolve(command, pane.cwd.as_deref(), &|pid| rimz::proc::cwd(pid));
            }
        }
    }

    let profiles = pane_profiles(panes, &children);
    if profiles.is_empty() {
        return emit_unavailable(secs as f64, json, no_pane_pids_notice(mux));
    }
    let conns = session_ssh_conns(mux, &workspace.session_name, &proc_snapshot);
    let (t0, t0_reads) = sample_panes(&profiles);
    let socket_t0 = socket_io(&conns);
    std::thread::sleep(Duration::from_secs(secs));
    let socket_t1 = socket_io(&conns);
    let (t1, t1_reads) = sample_panes(&profiles);
    if t0_reads == 0 && t1_reads == 0 {
        return emit_unavailable(secs as f64, json, IO_UNREADABLE_NOTICE);
    }

    let rows = sorted_rates(rates(&t0, &t1, secs as f64));
    let total = total_bps(&rows);
    let wire = wire_rates(socket_t0, socket_t1, secs as f64);
    if json {
        print_json(true, secs as f64, total, wire, &rows, None)
    } else {
        let report = format_report(&rows, total, wire, secs as f64);
        let mut out = render::out();
        out.write_all(report.as_bytes())
            .context("writing bandwidth report")
    }
}

fn pane_profiles(panes: Vec<PaneRef>, children: &HashMap<u32, Vec<u32>>) -> Vec<PaneProfile> {
    panes
        .into_iter()
        .filter_map(|pane| {
            let root = pane.pane_pid?;
            let label = pane_label(&pane);
            Some(PaneProfile {
                pane_id: pane.pane_id,
                label,
                pids: subtree_pids(children, root),
            })
        })
        .collect()
}

fn pane_label(pane: &PaneRef) -> String {
    let command = pane
        .command
        .as_deref()
        .or(pane.spawn_command.as_deref())
        .filter(|label| !label.trim().is_empty());
    let view = pane
        .view_name
        .as_deref()
        .filter(|label| !label.trim().is_empty());
    let label = match (view, command) {
        (Some(view), Some(command)) if view != command => format!("{view} - {command}"),
        (_, Some(command)) => command.to_owned(),
        (Some(view), None) => view.to_owned(),
        (None, None) => "(unknown)".to_owned(),
    };
    truncate_label(&label, DEFAULT_LABEL_WIDTH)
}

fn truncate_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    let mut truncated: String = label.chars().take(keep).collect();
    truncated.push_str("...");
    truncated
}

fn sample_panes(profiles: &[PaneProfile]) -> (Vec<PaneSample>, usize) {
    let mut readable = 0;
    let samples = profiles
        .iter()
        .map(|pane| {
            let mut write_bytes = 0_u64;
            for pid in &pane.pids {
                if let Some(bytes) = rimz::proc::write_bytes(*pid) {
                    readable += 1;
                    write_bytes = write_bytes.saturating_add(bytes);
                }
            }
            PaneSample {
                pane_id: pane.pane_id.clone(),
                label: pane.label.clone(),
                write_bytes,
            }
        })
        .collect();
    (samples, readable)
}

fn session_ssh_conns(mux: MuxName, session: &str, procs: &[ProcInfo]) -> Vec<SshConn> {
    let pids = match mux {
        MuxName::Zellij => zellij_client_pids(procs, session, rimz::proc::own_uid()),
        MuxName::Tmux => tmux_client_pids(session),
    };
    ssh_conns_for_client_pids(&pids, &|pid| rimz::proc::env_var(pid, SSH_CONNECTION_ENV))
}

fn zellij_client_pids(procs: &[ProcInfo], session: &str, own_uid: Option<u32>) -> Vec<u32> {
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

fn tmux_client_pids(session: &str) -> Vec<u32> {
    let Some(output) = rimz::mux::CommandSpec::new("tmux")
        .args(["list-clients", "-t", session, "-F", "#{client_pid}"])
        .run()
        .ok()
    else {
        return Vec::new();
    };
    parse_tmux_client_pids(&output.stdout)
}

fn parse_tmux_client_pids(raw: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

fn ssh_conns_for_client_pids(
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

fn socket_io(conns: &[SshConn]) -> Option<(u64, u64)> {
    if conns.is_empty() {
        return None;
    }

    let mut saw_counter = false;
    let mut tx = 0_u64;
    let mut rx = 0_u64;
    for conn in conns {
        let filter = format!(
            "src {} dst {}",
            conn.local.ss_filter_text(),
            conn.peer.ss_filter_text()
        );
        let output = Command::new("ss").args(["-tieHn", &filter]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        if let Some((conn_tx, conn_rx)) =
            parse_ss_counters(&String::from_utf8_lossy(&output.stdout))
        {
            saw_counter = true;
            tx = tx.saturating_add(conn_tx);
            rx = rx.saturating_add(conn_rx);
        }
    }
    saw_counter.then_some((tx, rx))
}

fn parse_ss_counters(out: &str) -> Option<(u64, u64)> {
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

fn children_by_parent(procs: &[ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::new();
    for proc in procs {
        children
            .entry(proc.ppid)
            .or_insert_with(Vec::new)
            .push(proc.pid);
    }
    children
}

fn subtree_pids(children_by_parent: &HashMap<u32, Vec<u32>>, root: u32) -> Vec<u32> {
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

fn rates(t0: &[PaneSample], t1: &[PaneSample], secs: f64) -> Vec<PaneRate> {
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

fn wire_rates(t0: Option<(u64, u64)>, t1: Option<(u64, u64)>, secs: f64) -> Option<(u64, u64)> {
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

fn sorted_rates(mut rows: Vec<PaneRate>) -> Vec<PaneRate> {
    rows.sort_by(|a, b| {
        b.bps
            .cmp(&a.bps)
            .then_with(|| a.pane_id.as_str().cmp(b.pane_id.as_str()))
    });
    rows
}

fn total_bps(rows: &[PaneRate]) -> u64 {
    rows.iter()
        .fold(0_u64, |total, row| total.saturating_add(row.bps))
}

fn fmt_bps(bps: u64) -> String {
    let kib = bps as f64 / 1_024.0;
    let mib = bps as f64 / 1_048_576.0;
    let gib = bps as f64 / 1_073_741_824.0;
    if mib >= 999.5 {
        format!("{gib:.0}G/s")
    } else if kib >= 999.5 {
        format!("{mib:.0}M/s")
    } else if bps >= 1_000 {
        format!("{kib:.0}k/s")
    } else {
        format!("{bps}B/s")
    }
}

fn format_report(rows: &[PaneRate], total: u64, wire: Option<(u64, u64)>, secs: f64) -> String {
    let rows = sorted_rates(rows.to_vec());
    let rate_text: Vec<String> = rows.iter().map(|row| fmt_bps(row.bps)).collect();
    let total_label = format!("{} sample", fmt_secs(secs));
    let mut wire_rows = Vec::new();
    if let Some((tx, rx)) = wire {
        wire_rows.push((WIRE_TX_PANE, WIRE_TX_LABEL, fmt_bps(tx)));
        wire_rows.push((WIRE_RX_PANE, WIRE_RX_LABEL, fmt_bps(rx)));
    }
    let pane_w = rows
        .iter()
        .map(|row| row.pane_id.as_str().len())
        .chain(wire_rows.iter().map(|(pane, _, _)| pane.len()))
        .max()
        .unwrap_or(0)
        .max("PANE".len())
        .max("TOTAL".len());
    let label_w = rows
        .iter()
        .map(|row| row.label.len())
        .chain(wire_rows.iter().map(|(_, label, _)| label.len()))
        .chain(std::iter::once(total_label.len()))
        .max()
        .unwrap_or(0)
        .max("LABEL".len());
    let rate_w = rate_text
        .iter()
        .map(String::len)
        .chain(wire_rows.iter().map(|(_, _, rate)| rate.len()))
        .max()
        .unwrap_or(0)
        .max("WRITE/S".len())
        .max(fmt_bps(total).len());

    let mut out = String::new();
    write_report_row(
        &mut out, "PANE", "LABEL", "WRITE/S", pane_w, label_w, rate_w,
    );
    for (row, rate) in rows.iter().zip(&rate_text) {
        write_report_row(
            &mut out,
            row.pane_id.as_str(),
            &row.label,
            rate,
            pane_w,
            label_w,
            rate_w,
        );
    }
    for (pane, label, rate) in &wire_rows {
        write_report_row(&mut out, pane, label, rate, pane_w, label_w, rate_w);
    }
    write_report_row(
        &mut out,
        "TOTAL",
        &total_label,
        &fmt_bps(total),
        pane_w,
        label_w,
        rate_w,
    );
    writeln!(&mut out).expect("writing to String cannot fail");
    writeln!(&mut out, "{REPORT_CAVEAT}").expect("writing to String cannot fail");
    out
}

fn write_report_row(
    out: &mut String,
    pane: &str,
    label: &str,
    rate: &str,
    pane_w: usize,
    label_w: usize,
    rate_w: usize,
) {
    writeln!(out, "{pane:<pane_w$}  {label:<label_w$}  {rate:>rate_w$}")
        .expect("writing to String cannot fail");
}

fn fmt_secs(secs: f64) -> String {
    let mut text = format!("{secs:.2}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{text}s")
}

fn emit_unavailable(secs: f64, json: bool, notice: &'static str) -> Result<()> {
    let rows = Vec::new();
    if json {
        return print_json(false, secs, 0, None, &rows, Some(notice));
    }
    let mut out = render::out();
    writeln!(out, "{notice}").context("writing bandwidth notice")
}

fn no_pane_pids_notice(mux: MuxName) -> &'static str {
    match mux {
        MuxName::Zellij => ZELLIJ_NO_PANE_PIDS_NOTICE,
        MuxName::Tmux => NO_PANE_PIDS_NOTICE,
    }
}

fn print_json(
    available: bool,
    sample_secs: f64,
    total_bps: u64,
    wire: Option<(u64, u64)>,
    panes: &[PaneRate],
    message: Option<&'static str>,
) -> Result<()> {
    let rendered = render_json(available, sample_secs, total_bps, wire, panes, message)?;
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

fn render_json(
    available: bool,
    sample_secs: f64,
    total_bps: u64,
    wire: Option<(u64, u64)>,
    panes: &[PaneRate],
    message: Option<&'static str>,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&BandwidthJson {
        available,
        sample_secs,
        total_bps,
        wire_tx_bps: wire.map(|(tx, _)| tx),
        wire_rx_bps: wire.map(|(_, rx)| rx),
        panes,
        caveat: REPORT_CAVEAT,
        message,
    })?)
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
    fn no_pane_pids_notice_matches_backend_resolution_path() {
        assert_eq!(
            no_pane_pids_notice(MuxName::Zellij),
            ZELLIJ_NO_PANE_PIDS_NOTICE
        );
        assert_eq!(no_pane_pids_notice(MuxName::Tmux), NO_PANE_PIDS_NOTICE);
        assert!(!no_pane_pids_notice(MuxName::Tmux).contains("Zellij"));
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

    #[test]
    fn fmt_bps_matches_sidebar_io_shape() {
        assert_eq!(fmt_bps(999), "999B/s");
        assert_eq!(fmt_bps(1_000), "1k/s");
        assert_eq!(fmt_bps(999 * 1024), "999k/s");
        assert_eq!(fmt_bps(1_024 * 1_024), "1M/s");
        assert_eq!(fmt_bps(1_024 * 1_024 * 1_024), "1G/s");
    }

    #[test]
    fn format_report_sorts_descending_and_mentions_caveat() {
        let rows = [
            pane_rate("tmux:%2", "sidebar", 12),
            pane_rate("tmux:%1", "btop", 40_000),
        ];
        let report = format_report(&rows, 40_012, None, 5.0);

        let header = report.lines().next().expect("header");
        assert!(header.contains("PANE"));
        assert!(header.contains("LABEL"));
        assert!(header.contains("WRITE/S"));
        assert!(report.find("tmux:%1  btop").unwrap() < report.find("tmux:%2  sidebar").unwrap());
        assert!(report.contains("TOTAL    5s sample"));
        assert!(report.contains(REPORT_CAVEAT));
    }

    #[test]
    fn format_report_includes_wire_rows_before_total() {
        let rows = [pane_rate("tmux:%1", "codex", 40_000)];
        let report = format_report(&rows, 40_000, Some((8_192, 1_024)), 5.0);

        assert!(report.contains(WIRE_TX_PANE));
        assert!(report.contains(WIRE_TX_LABEL));
        assert!(report.contains(WIRE_RX_PANE));
        assert!(report.contains(WIRE_RX_LABEL));
        assert!(report.find(WIRE_TX_PANE).unwrap() < report.find("TOTAL").unwrap());
    }

    #[test]
    fn render_json_carries_wire_fields_when_present() {
        let rows = [pane_rate("tmux:%1", "codex", 40_000)];
        let rendered = render_json(true, 5.0, 40_000, Some((8_192, 1_024)), &rows, None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(value["wire_tx_bps"], 8_192);
        assert_eq!(value["wire_rx_bps"], 1_024);

        let rendered = render_json(true, 5.0, 40_000, None, &rows, None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(value.get("wire_tx_bps").is_none());
        assert!(value.get("wire_rx_bps").is_none());
    }

    #[test]
    fn truncate_label_keeps_table_compact() {
        assert_eq!(truncate_label("short", 10), "short");
        assert_eq!(truncate_label("abcdefghijklmnopqrstuvwxyz", 8), "abcde...");
    }
}
