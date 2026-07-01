use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write as _;
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
const NO_PANE_PIDS_NOTICE: &str = concat!(
    "rimz remote bandwidth could not resolve any pane root process. ",
    "On Zellij, a pane resolves only while it runs a live, uniquely named foreground process; ",
    "idle look-alike shells are skipped."
);
const IO_UNREADABLE_NOTICE: &str = concat!(
    "rimz remote bandwidth resolved pane processes but could not read /proc/<pid>/io. ",
    "The room may be served by another user, or the kernel lacks CONFIG_TASK_IO_ACCOUNTING."
);
const REPORT_CAVEAT: &str =
    "process write-rate includes non-pty writes; muxes diff/throttle, so wire bytes <= this.";

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
        return emit_unavailable(secs as f64, json, NO_PANE_PIDS_NOTICE);
    }
    let (t0, t0_reads) = sample_panes(&profiles);
    std::thread::sleep(Duration::from_secs(secs));
    let (t1, t1_reads) = sample_panes(&profiles);
    if t0_reads == 0 && t1_reads == 0 {
        return emit_unavailable(secs as f64, json, IO_UNREADABLE_NOTICE);
    }

    let rows = sorted_rates(rates(&t0, &t1, secs as f64));
    let total = total_bps(&rows);
    if json {
        print_json(true, secs as f64, total, &rows, None)
    } else {
        let report = format_report(&rows, total, secs as f64);
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

fn format_report(rows: &[PaneRate], total: u64, secs: f64) -> String {
    let rows = sorted_rates(rows.to_vec());
    let rate_text: Vec<String> = rows.iter().map(|row| fmt_bps(row.bps)).collect();
    let pane_w = rows
        .iter()
        .map(|row| row.pane_id.as_str().len())
        .max()
        .unwrap_or(0)
        .max("PANE".len())
        .max("TOTAL".len());
    let label_w = rows
        .iter()
        .map(|row| row.label.len())
        .max()
        .unwrap_or(0)
        .max("LABEL".len());
    let rate_w = rate_text
        .iter()
        .map(String::len)
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
    write_report_row(
        &mut out,
        "TOTAL",
        &format!("{} sample", fmt_secs(secs)),
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
        return print_json(false, secs, 0, &rows, Some(notice));
    }
    let mut out = render::out();
    writeln!(out, "{notice}").context("writing bandwidth notice")
}

fn print_json(
    available: bool,
    sample_secs: f64,
    total_bps: u64,
    panes: &[PaneRate],
    message: Option<&'static str>,
) -> Result<()> {
    let rendered = serde_json::to_string_pretty(&BandwidthJson {
        available,
        sample_secs,
        total_bps,
        panes,
        caveat: REPORT_CAVEAT,
        message,
    })?;
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            real_uid: 1000,
            cmdline: format!("proc-{pid}"),
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
        let report = format_report(&rows, 40_012, 5.0);

        assert!(report.contains("PANE     LABEL    WRITE/S\n"));
        assert!(report.find("tmux:%1  btop").unwrap() < report.find("tmux:%2  sidebar").unwrap());
        assert!(report.contains("TOTAL    5s sample"));
        assert!(report.contains(REPORT_CAVEAT));
    }

    #[test]
    fn truncate_label_keeps_table_compact() {
        assert_eq!(truncate_label("short", 10), "short");
        assert_eq!(truncate_label("abcdefghijklmnopqrstuvwxyz", 8), "abcde...");
    }
}
