use std::collections::HashMap;
use std::path::PathBuf;

use crate::mux::zellij::{SIDEBAR_PANE_NAME, ZellijPaneResolver};
use crate::sidebar::frame::PaneFrame;

pub(in crate::sidebar::produce) fn backfill_zellij_pane_pids_from_proc(
    frame: &mut PaneFrame,
    session_name: &str,
) -> HashMap<u32, Vec<u32>> {
    let all_procs = crate::proc::list_processes();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in &all_procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    backfill_zellij_pane_pids(
        frame,
        &all_procs,
        &children,
        session_name,
        crate::proc::own_uid(),
        &|pid| crate::proc::cwd(pid),
    );
    children
}

/// Backfill `pane_pid` for panes whose backend reported none (Zellij emits no
/// pid field; tmux fills `#{pane_pid}` natively), resolving each pane to its
/// root process — the direct child of the session's `zellij --server <socket>`
/// process — so the field carries tmux's semantics on both backends and the
/// shell→single-child descent above behaves identically.
///
/// Zellij reports a pane's *foreground* command as that process's `/proc`
/// cmdline (argv joined by spaces — the same form as
/// [`ProcInfo`](crate::proc::ProcInfo)`::cmdline`), so a pane matches the forest
/// process with that exact cmdline, then walks up to the direct server child.
/// The cwd narrow only breaks ties between same-cmdline candidates: a unique
/// match is taken as-is, since a foreground process may legitimately sit in
/// another directory than the pane reports (an agent that chdir'd into its
/// worktree). Pure over its inputs — the caller injects the process table and
/// the `/proc` cwd lookup — so the matcher unit-tests over fixtures.
///
/// Abstention is the failure mode: a pane stays pidless (no stats beats a
/// stranger's stats) when its command matches nothing or stays ambiguous after
/// the narrow — e.g. two idle `zsh` panes in one cwd. An *active* pane's
/// foreground cmdline is almost always unique, so real work still reads.
/// Sidebar chrome panes are skipped outright: every sidebar shares one
/// cmdline, and they are excluded from rows anyway.
pub(super) fn backfill_zellij_pane_pids(
    frame: &mut PaneFrame,
    procs: &[crate::proc::ProcInfo],
    children: &HashMap<u32, Vec<u32>>,
    session_name: &str,
    own_uid: Option<u32>,
    proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
) {
    // Nothing to backfill (tmux, or an empty room): skip the server scan.
    if frame.pane_states().all(|pane| pane.current.pid.is_some()) {
        return;
    }
    let Some(mut resolver) = ZellijPaneResolver::new(procs, children, session_name, own_uid) else {
        return;
    };
    for root in frame.pane_states().filter_map(|pane| pane.current.pid) {
        resolver.claim(root);
    }
    for pane in frame.pane_states_mut() {
        if pane.current.pid.is_some() {
            continue;
        }
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if command == SIDEBAR_PANE_NAME {
            continue;
        }
        if let Some(root) = resolver.resolve(command, pane.current.cwd.as_deref(), proc_cwd) {
            pane.current.pid = Some(root);
        }
    }
}
