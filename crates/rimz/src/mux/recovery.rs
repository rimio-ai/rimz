//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by `rimz reset` and the auto-offer in `rimz start`, so the teardown
//! lives in exactly one place and is testable without a real multiplexer.
//!
//! The dangerous step is the process sweep: it signals processes by heuristic, so
//! it is scoped three ways — real uid, the exact (path-derived, globally unique)
//! session name in the command line, and an explicit exclusion of this process
//! and its ancestors — and it is Linux-gated (it needs `/proc`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::mux::MuxBackend;
use crate::proc::ProcInfo;

/// Grace between SIGTERM and SIGKILL in the process sweep — long enough for a
/// well-behaved process to exit on its own, short enough not to stall `reset`.
const SWEEP_GRACE: Duration = Duration::from_millis(300);

/// What [`teardown_room`] removed, for the user-facing `rimz reset` report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeardownReport {
    /// The session was deleted (or was already gone).
    pub session_killed: bool,
    /// Resurrection-cache paths removed.
    pub cache_removed: Vec<PathBuf>,
    /// Orphaned server / leaked daemon pids signalled.
    pub processes_swept: Vec<u32>,
}

/// Tear the room down to a clean slate: delete the session, purge the backend's
/// resurrection cache, reap stale sidebar runtime files, and sweep orphaned
/// servers / leaked daemons scoped to this workspace. Every step is best-effort
/// and independent — a failure in one never blocks the others — so a later
/// rebirth always starts from the cleanest state reachable.
pub fn teardown_room(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    runtime: &RuntimePaths,
) -> TeardownReport {
    // Delete the session first, so the only server matching this exact name in
    // the sweep below is the corpse — never a freshly-born replacement.
    let session_killed = backend.kill_session(session_name).is_ok();
    let cache_removed = backend.purge_resurrection_cache(session_name);
    crate::sidebar::sweep_orphan_runtime(runtime);
    let processes_swept = sweep_orphan_processes(workspace_id.as_str(), session_name);
    TeardownReport {
        session_killed,
        cache_removed,
        processes_swept,
    }
}

/// Remove Zellij's serialized-session cache for `name` under `cache_root`, across
/// every `contract_version_*` child so it survives a Zellij contract bump.
/// Returns the paths removed; a missing cache removes nothing. Pure over its
/// `cache_root` argument so it is testable against a tempdir.
pub fn purge_zellij_session_cache_in(cache_root: &Path, name: &str) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(versions) = fs::read_dir(cache_root.join("zellij")) else {
        return removed;
    };
    for version in versions.flatten() {
        if !version
            .file_name()
            .to_string_lossy()
            .starts_with("contract_version")
        {
            continue;
        }
        let entry = version.path().join("session_info").join(name);
        if remove_path(&entry) {
            removed.push(entry);
        }
    }
    removed
}

/// Remove a file or directory, returning whether anything was removed. A path
/// that does not exist is not an error (the goal state is "gone").
fn remove_path(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let result = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.is_ok()
}

/// Whether `cmdline` belongs to an orphaned room process worth sweeping. The
/// exact, path-derived `session_name` is globally unique, so requiring it in the
/// command line is the load-bearing scope: nothing from another room can match.
/// Within that scope we take either an orphaned multiplexer server or a leaked
/// rimz sidebar / agent app-server daemon for this workspace.
fn is_sweep_target(cmdline: &str, session_name: &str, workspace_id: &str) -> bool {
    if !cmdline.contains(session_name) {
        return false;
    }
    let mux_server = cmdline.contains("--server");
    let workspace_daemon = cmdline.contains(workspace_id)
        && (cmdline.contains("rimz-sidebar")
            || cmdline.contains("sidebar")
            || cmdline.contains("app-server"));
    mux_server || workspace_daemon
}

/// Pick the pids to sweep: this user's processes only, matching
/// [`is_sweep_target`], minus the `protected` set (this process and its
/// ancestors). Pure over its inputs so the scoping rules are unit-tested without
/// touching real processes.
pub(crate) fn select_sweep_targets(
    procs: &[ProcInfo],
    my_uid: u32,
    session_name: &str,
    workspace_id: &str,
    protected: &HashSet<u32>,
) -> Vec<u32> {
    procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_sweep_target(&proc.cmdline, session_name, workspace_id))
        .map(|proc| proc.pid)
        .collect()
}

/// This process plus its ancestor chain — the pids the sweep must never signal,
/// so `rimz reset` cannot kill the shell or attach that launched it.
fn protected_pids(procs: &[ProcInfo], self_pid: u32) -> HashSet<u32> {
    let parents: HashMap<u32, u32> = procs.iter().map(|proc| (proc.pid, proc.ppid)).collect();
    let mut protected = HashSet::new();
    let mut current = self_pid;
    // Bounded so a `/proc` glitch (a cycle) cannot loop forever.
    for _ in 0..64 {
        if !protected.insert(current) {
            break;
        }
        match parents.get(&current) {
            Some(&parent) if parent > 1 => current = parent,
            _ => break,
        }
    }
    protected
}

#[cfg(target_os = "linux")]
fn sweep_orphan_processes(workspace_id: &str, session_name: &str) -> Vec<u32> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::{Pid, Uid};

    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let targets = select_sweep_targets(
        &procs,
        Uid::current().as_raw(),
        session_name,
        workspace_id,
        &protected,
    );
    if targets.is_empty() {
        return targets;
    }
    let signal = |pid: u32, sig: Signal| {
        let _ = kill(Pid::from_raw(pid as i32), sig);
    };
    for &pid in &targets {
        signal(pid, Signal::SIGTERM);
    }
    std::thread::sleep(SWEEP_GRACE);
    let still_alive: HashSet<u32> = crate::proc::list_processes()
        .iter()
        .map(|proc| proc.pid)
        .collect();
    for &pid in &targets {
        if still_alive.contains(&pid) {
            signal(pid, Signal::SIGKILL);
        }
    }
    targets
}

#[cfg(not(target_os = "linux"))]
fn sweep_orphan_processes(_workspace_id: &str, _session_name: &str) -> Vec<u32> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, uid: u32, cmdline: &str) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            real_uid: uid,
            cmdline: cmdline.to_owned(),
        }
    }

    const SESSION: &str = "rimz-home-marvin-workspace-project-rimz-rimz";
    const WS: &str = "ws_f89e49906df0621ad2765112";

    #[test]
    fn sweep_selects_only_scoped_orphans() {
        let me = 1000;
        let procs = vec![
            // Orphaned Zellij server for this session — swept.
            proc(
                10,
                1,
                me,
                &format!("zellij --server /run/user/1000/zellij/contract_version_1/{SESSION}"),
            ),
            // Leaked sidebar daemon for this workspace+session — swept.
            proc(
                11,
                1,
                me,
                &format!(
                    "rimz-sidebar serve --workspace-id {WS} --mux zellij --session-name {SESSION}"
                ),
            ),
            // Leaked codex app-server for this workspace+session — swept.
            proc(
                12,
                1,
                me,
                &format!(
                    "rimz codex app-server serve --workspace-id {WS} --session-name {SESSION}"
                ),
            ),
            // A different user's identical server — excluded by uid.
            proc(
                20,
                1,
                0,
                &format!("zellij --server /run/user/0/zellij/contract_version_1/{SESSION}"),
            ),
            // A server for a DIFFERENT session — excluded by the session-name scope.
            proc(
                21,
                1,
                me,
                "zellij --server /run/user/1000/zellij/contract_version_1/rimz-other-room",
            ),
            // The user's interactive shell — no session name, never swept.
            proc(22, 1, me, "zsh"),
            // A claude agent pane — no session name in argv, never swept.
            proc(23, 1, me, "claude --worktree main"),
        ];
        let protected = HashSet::new();
        let mut got = select_sweep_targets(&procs, me, SESSION, WS, &protected);
        got.sort_unstable();
        assert_eq!(got, vec![10, 11, 12]);
    }

    #[test]
    fn sweep_excludes_self_and_ancestors() {
        let me = 1000;
        // A reset process tree: shell(100) -> rimz reset(101) -> this(102). None of
        // them carry the session name, but protect them explicitly regardless.
        let procs = vec![
            proc(100, 1, me, "zsh"),
            proc(101, 100, me, "rimz reset"),
            proc(102, 101, me, "rimz reset"),
            // An orphan that WOULD match, to prove protection is the only exclusion.
            proc(
                10,
                1,
                me,
                &format!("zellij --server /run/user/1000/zellij/contract_version_1/{SESSION}"),
            ),
        ];
        let protected = protected_pids(&procs, 102);
        assert!(protected.contains(&102));
        assert!(protected.contains(&101));
        assert!(protected.contains(&100));
        let got = select_sweep_targets(&procs, me, SESSION, WS, &protected);
        assert_eq!(got, vec![10]);
    }

    #[test]
    fn cache_purge_removes_every_contract_version_and_ignores_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Two contract versions hold this session; a third dir is unrelated.
        for version in ["contract_version_1", "contract_version_2"] {
            let entry = root.join("zellij").join(version).join("session_info");
            fs::create_dir_all(&entry).expect("mkdir");
            fs::write(entry.join(SESSION), b"serialized").expect("write");
        }
        fs::create_dir_all(root.join("zellij").join("permissions")).expect("mkdir");

        let mut removed = purge_zellij_session_cache_in(root, SESSION);
        removed.sort();
        assert_eq!(
            removed.len(),
            2,
            "both contract versions purged: {removed:?}"
        );
        assert!(
            !root
                .join("zellij/contract_version_1/session_info")
                .join(SESSION)
                .exists()
        );

        // A second run finds nothing to remove and does not error.
        assert!(purge_zellij_session_cache_in(root, SESSION).is_empty());
        // An absent cache root is a no-op.
        assert!(purge_zellij_session_cache_in(&root.join("missing"), SESSION).is_empty());
    }
}
