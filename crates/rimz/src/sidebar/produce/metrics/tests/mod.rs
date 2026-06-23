use super::*;
use crate::sidebar::produce::test_support::pane;
use std::path::PathBuf;

mod binding_cache;
mod cadence;
mod pid_backfill;
mod tree;

/// A process-table entry for the pid-backfill matcher fixtures; everything
/// runs as one uid (1000) unless a test says otherwise.
fn proc_info(pid: u32, ppid: u32, cmdline: &str) -> crate::proc::ProcInfo {
    crate::proc::ProcInfo {
        pid,
        ppid,
        real_uid: 1000,
        cmdline: cmdline.to_owned(),
    }
}

/// The ppid→children map `enrich_pane_metrics` builds, over a fixture table.
fn children_of(procs: &[crate::proc::ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    children
}

/// The session's Zellij server process, socket named after the session.
fn server(pid: u32, session: &str) -> crate::proc::ProcInfo {
    proc_info(
        pid,
        1,
        &format!("/usr/bin/zellij --server /run/user/1000/zellij/contract_version_1/{session}"),
    )
}

const SESSION: &str = "rimz-query-engine";

fn frame_from_panes(panes: Vec<crate::pane::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, SESSION)
}

fn pane_id(raw: &str) -> crate::ids::PaneId {
    crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, raw)
}

fn state<'a>(
    frame: &'a crate::sidebar::frame::PaneFrame,
    raw: &str,
) -> &'a crate::sidebar::frame::PaneState {
    let pane_id = pane_id(raw);
    frame
        .pane_states()
        .find(|pane| pane.pane_id == pane_id)
        .expect("pane state exists")
}

fn sync_panes_from_frame(
    panes: &mut [crate::pane::PaneRef],
    frame: crate::sidebar::frame::PaneFrame,
) {
    let mut projected: HashMap<crate::ids::PaneId, crate::pane::PaneRef> = frame
        .to_pane_refs()
        .into_iter()
        .map(|pane| (pane.pane_id.clone(), pane))
        .collect();
    for slot in panes {
        if let Some(next) = projected.remove(&slot.pane_id) {
            *slot = next;
        }
    }
}

fn backfill(
    panes: &mut [crate::pane::PaneRef],
    procs: &[crate::proc::ProcInfo],
    cwds: &[(u32, &str)],
) {
    let cwds: HashMap<u32, PathBuf> = cwds
        .iter()
        .map(|(pid, cwd)| (*pid, PathBuf::from(cwd)))
        .collect();
    let mut frame = frame_from_panes(panes.to_vec());
    backfill_zellij_pane_pids(
        &mut frame,
        procs,
        &children_of(procs),
        SESSION,
        Some(1000),
        &|pid| cwds.get(&pid).cloned(),
    );
    sync_panes_from_frame(panes, frame);
}

/// A cache entry binding `pane_pid` with `start_ticks`, as the prior tick
/// records it. `command` is the sample-time foreground the carry guard keys on.
fn binding_entry(pane_pid: u32, start_ticks: u64, command: &str) -> MetricsSampleEntry {
    MetricsSampleEntry {
        sample_version: METRICS_SAMPLE_VERSION,
        stats_pid: pane_pid,
        cpu_ticks: 0,
        io_bytes: 0,
        io_bytes_valid: true,
        sampled_at_ms: 0,
        pane_pid: Some(pane_pid),
        root_start_ticks: Some(start_ticks),
        command: Some(command.to_owned()),
        cpu_pct: None,
        io_bps: None,
        rss_kb: None,
        state_samples: Vec::new(),
        process_state: None,
    }
}

fn fresh_entry(
    pane_pid: u32,
    start_ticks: u64,
    command: &str,
    sampled_at_ms: u64,
) -> MetricsSampleEntry {
    MetricsSampleEntry {
        sampled_at_ms,
        ..binding_entry(pane_pid, start_ticks, command)
    }
}
