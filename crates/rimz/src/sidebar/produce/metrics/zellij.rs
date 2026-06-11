use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::ProcessState;
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

#[cfg(test)]
pub(super) fn process_state_from_stat(
    current: Option<char>,
    prior: Option<char>,
) -> Option<ProcessState> {
    match current {
        Some('Z') => Some(ProcessState::Stuck),
        Some('D') if prior == Some('D') => Some(ProcessState::Stuck),
        Some(_) => None,
        None => None,
    }
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
    let Some(server_pid) = zellij_server_pid(procs, session_name, own_uid) else {
        return;
    };
    let forest = descendants(children, server_pid);
    let parent_of: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut claimed_roots: HashSet<u32> = frame
        .pane_states()
        .filter_map(|pane| pane.current.pid)
        .collect();
    for pane in frame.pane_states_mut() {
        if pane.current.pid.is_some() {
            continue;
        }
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if command == crate::mux::zellij::SIDEBAR_PANE_NAME {
            continue;
        }
        let candidates: Vec<(u32, u32)> = procs
            .iter()
            .filter(|p| forest.contains(&p.pid) && p.cmdline == command)
            .filter_map(|p| {
                walk_to_server_child(&parent_of, server_pid, p.pid)
                    .filter(|root| !claimed_roots.contains(root))
                    .map(|root| (p.pid, root))
            })
            .collect();
        let matched = resolve_candidate_root(&candidates, pane.current.cwd.as_deref(), proc_cwd);
        if let Some(root) = matched {
            pane.current.pid = Some(root);
            claimed_roots.insert(root);
        }
    }
}

pub(super) fn resolve_candidate_root(
    candidates: &[(u32, u32)],
    cwd: Option<&str>,
    proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
) -> Option<u32> {
    let roots = unique_candidate_roots(candidates);
    match roots.as_slice() {
        [root] => Some(*root),
        [] => None,
        _ => {
            let cwd = cwd?;
            let narrowed: Vec<(u32, u32)> = candidates
                .iter()
                .copied()
                .filter(|&(pid, _)| proc_cwd(pid).as_deref() == Some(Path::new(cwd)))
                .collect();
            let narrowed_roots = unique_candidate_roots(&narrowed);
            match narrowed_roots.as_slice() {
                [root] => Some(*root),
                _ => None,
            }
        }
    }
}

fn unique_candidate_roots(candidates: &[(u32, u32)]) -> Vec<u32> {
    let mut roots = Vec::new();
    for (_, root) in candidates {
        if !roots.iter().any(|known| known == root) {
            roots.push(*root);
        }
    }
    roots
}

/// The pid of the session's Zellij server: the same-uid process whose cmdline
/// is `zellij --server <socket>` with the socket's file name equal to the
/// session name (Zellij names the server socket after the session). The uid
/// gate keeps a same-named session of another user from being walked.
fn zellij_server_pid(
    procs: &[crate::proc::ProcInfo],
    session_name: &str,
    own_uid: Option<u32>,
) -> Option<u32> {
    let own_uid = own_uid?;
    procs
        .iter()
        .find(|p| p.real_uid == own_uid && cmdline_is_session_server(&p.cmdline, session_name))
        .map(|p| p.pid)
}

/// Whether a cmdline runs the Zellij server for `session_name` — exactly
/// `<path>/zellij --server <socket>` with `basename(socket) == session_name`.
fn cmdline_is_session_server(cmdline: &str, session_name: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    let file_name = |token: Option<&str>, name: &str| {
        token
            .map(Path::new)
            .and_then(Path::file_name)
            .is_some_and(|file| file == name)
    };
    file_name(tokens.next(), "zellij")
        && tokens.next() == Some("--server")
        && file_name(tokens.next(), session_name)
}

/// Every descendant of `root` in the ppid→children map — the session server's
/// process forest, one tree per pane.
fn descendants(children: &HashMap<u32, Vec<u32>>, root: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            if out.insert(child) {
                stack.push(child);
            }
        }
    }
    out
}

/// Walk `pid` up its parent chain to the direct child of `server_pid` — the
/// pane root. Terminates by construction for a forest member (its membership
/// proves a parent chain to the server); the `None` arm covers a chain that
/// leaves the table mid-walk, e.g. a process that exited between reads.
fn walk_to_server_child(
    parent_of: &HashMap<u32, u32>,
    server_pid: u32,
    mut pid: u32,
) -> Option<u32> {
    loop {
        match parent_of.get(&pid) {
            Some(&ppid) if ppid == server_pid => return Some(pid),
            Some(&ppid) => pid = ppid,
            None => return None,
        }
    }
}
