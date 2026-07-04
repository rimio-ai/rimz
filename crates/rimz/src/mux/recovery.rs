//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by `rimz reset` and attached `rimz start` auto-reset, so teardown
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
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::MuxBackend;
use crate::proc::ProcInfo;

/// Grace between SIGTERM and SIGKILL in the process sweep — long enough for a
/// well-behaved process to exit on its own, short enough not to stall `reset`.
pub(crate) const SWEEP_GRACE: Duration = Duration::from_millis(300);

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
    // The session is already a corpse (killed above), so sweeping its lingering
    // mux server is cleanup, not destruction.
    let processes_swept = sweep_orphan_processes(workspace_id.as_str(), session_name, true);
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
    for entry in zellij_session_cache_paths_in(cache_root, name) {
        if remove_path(&entry) {
            removed.push(entry);
        }
    }
    removed
}

/// Zellij's serialized-session cache paths for `name`, across every
/// `contract_version_*` child.
pub fn zellij_session_cache_paths_in(cache_root: &Path, name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(versions) = fs::read_dir(cache_root.join("zellij")) else {
        return paths;
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
        if entry.exists() {
            paths.push(entry);
        }
    }
    paths
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
/// Within that scope we take a leaked rimz sidebar / agent app-server daemon for
/// this workspace, and — only when `include_mux_server` — an orphaned multiplexer
/// server. `rimz reset` kills the session first, so its lingering server is a
/// corpse and sweeping it is safe; `rimz reload` infers "dead" from a best-effort
/// probe, so it never sweeps a server (a probe that wrongly read a live session
/// as dead would otherwise destroy it) and reaps only respawnable daemons.
#[cfg(any(target_os = "linux", test))]
fn is_sweep_target(
    cmdline: &str,
    session_name: &str,
    workspace_id: &str,
    include_mux_server: bool,
) -> bool {
    if !cmdline.contains(session_name) {
        return false;
    }
    let mux_server = include_mux_server && cmdline.contains("--server");
    let workspace_daemon = cmdline.contains(workspace_id)
        && (cmdline.contains("sidebar") || cmdline.contains("app-server"));
    mux_server || workspace_daemon
}

/// Pick the pids to sweep: this user's processes only, matching
/// [`is_sweep_target`], minus the `protected` set (this process and its
/// ancestors). `include_mux_server` flows through to [`is_sweep_target`]. Pure
/// over its inputs so the scoping rules are unit-tested without touching real
/// processes.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn select_sweep_targets(
    procs: &[ProcInfo],
    my_uid: u32,
    session_name: &str,
    workspace_id: &str,
    protected: &HashSet<u32>,
    include_mux_server: bool,
) -> Vec<u32> {
    procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| {
            is_sweep_target(
                &proc.cmdline,
                session_name,
                workspace_id,
                include_mux_server,
            )
        })
        .map(|proc| proc.pid)
        .collect()
}

/// This process plus its ancestor chain — the pids the sweep must never signal,
/// so `rimz reset`/`rimz reload` cannot kill the shell or attach that launched it.
pub(crate) fn protected_pids(procs: &[ProcInfo], self_pid: u32) -> HashSet<u32> {
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

/// Sweep this user's orphaned server / leaked daemons for `(workspace, session)`
/// (SIGTERM→grace→SIGKILL), excluding the caller and its ancestors. `rimz reset`
/// runs it after killing the session (`include_mux_server: true`); `rimz reload`
/// runs it for a workspace whose session a probe read as gone, reaping only
/// respawnable sidebar/app-server leftovers (`include_mux_server: false`) so a
/// misread live session is never destroyed. Linux-only.
#[cfg(target_os = "linux")]
pub(crate) fn sweep_orphan_processes(
    workspace_id: &str,
    session_name: &str,
    include_mux_server: bool,
) -> Vec<u32> {
    use nix::unistd::Uid;

    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let targets = select_sweep_targets(
        &procs,
        Uid::current().as_raw(),
        session_name,
        workspace_id,
        &protected,
        include_mux_server,
    );
    kill_pids(&targets, SWEEP_GRACE)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn sweep_orphan_processes(
    _workspace_id: &str,
    _session_name: &str,
    _include_mux_server: bool,
) -> Vec<u32> {
    Vec::new()
}

/// SIGUSR1 every `rimz stats --refresh` dashboard this user owns so each
/// re-execs in place onto the freshly-installed binary. User-wide and unscoped:
/// stats are account-global, so a reload refreshes the daemon-view pane and any
/// standalone dashboard alike. Returns the pids signalled; empty off-Linux,
/// where `/proc` is unavailable and the dashboard reloads via its own `r` key.
#[cfg(target_os = "linux")]
pub(crate) fn reload_stats_dashboards() -> Vec<u32> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let my_uid = current_uid();
    let targets: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_stats_refresh(&proc.cmdline))
        .map(|proc| proc.pid)
        .collect();
    for &pid in &targets {
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGUSR1);
    }
    targets
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn reload_stats_dashboards() -> Vec<u32> {
    Vec::new()
}

/// Whether `cmdline` is a held or standalone `rimz stats --refresh` dashboard.
/// Token matching keeps the user-wide signal pass scoped to the Rimz stats
/// subcommand and excludes one-shot reports and unrelated commands mentioning
/// those words.
#[cfg(target_os = "linux")]
pub(crate) fn is_stats_refresh(cmdline: &str) -> bool {
    let Some(args) = cmdline
        .strip_prefix("rimz ")
        .or_else(|| cmdline.rsplit_once("/rimz ").map(|(_, args)| args))
    else {
        return false;
    };
    let mut saw_stats = false;
    for arg in args.split_whitespace() {
        if arg == "stats" {
            saw_stats = true;
        } else if saw_stats && arg == "--refresh" {
            return true;
        }
    }
    false
}

/// Whether `cmdline` is one of `(workspace, session)`'s sidebar *serve* processes
/// — `rimz sidebar serve` — and not the mux server or the agent app-server. The
/// exact, path-derived session name plus the workspace id scope it; `sidebar` + `serve` selects the renderer
/// pair and excludes `rimz codex app-server serve`.
pub(crate) fn is_sidebar_serve(cmdline: &str, workspace_id: &str, session_name: &str) -> bool {
    cmdline.contains(session_name)
        && cmdline.contains(workspace_id)
        && cmdline.contains("sidebar")
        && cmdline.contains("serve")
}

/// The normalized pane a sidebar process paints, from its inherited mux env var
/// — through [`super::pane_from_env_value`], the same mapping the renderer
/// applies to its own pane ([`super::own_pane_id`]). `None` when the var is
/// absent, so a caller never reaps a process it cannot place.
pub(crate) fn attributed_pane(pid: u32, mux: MuxName) -> Option<PaneId> {
    let key = super::pane_env_key(mux);
    Some(super::pane_from_env_value(
        mux,
        &crate::proc::env_var(pid, key)?,
    ))
}

/// SIGTERM→SIGKILL the sidebar serve pair attributed (by its inherited mux pane
/// env) to exactly `pane` — the cleanup for an in-place add whose pane never
/// mounted or could not be docked, so a failed add never leaks a paneless
/// renderer. Same uid/ancestor scoping as the orphan sweep. Returns the number
/// of processes signalled; empty off-Linux, where `list_processes` is empty.
pub(crate) fn kill_sidebar_serve_for_pane(
    workspace_id: &str,
    session_name: &str,
    pane: &PaneId,
    mux: MuxName,
) -> usize {
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let my_uid = current_uid();
    let targets: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_sidebar_serve(&proc.cmdline, workspace_id, session_name))
        .filter(|proc| attributed_pane(proc.pid, mux).as_ref() == Some(pane))
        .map(|proc| proc.pid)
        .collect();
    kill_pids(&targets, SWEEP_GRACE).len()
}

#[cfg(target_os = "linux")]
pub(crate) fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn current_uid() -> u32 {
    // No `/proc`, so `list_processes` is empty and this is never compared.
    u32::MAX
}

/// SIGTERM each pid, wait `grace`, then SIGKILL any still alive; returns the pids
/// signalled. The shared graceful-then-forceful kill path for the `rimz reset`
/// orphan sweep and `rimz reload`'s zombie-sidebar reaping. Linux-only — callers
/// on other platforms get an empty result and fall back to mux-level pane close.
#[cfg(target_os = "linux")]
pub(crate) fn kill_pids(targets: &[u32], grace: Duration) -> Vec<u32> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    if targets.is_empty() {
        return Vec::new();
    }
    let signal = |pid: u32, sig: Signal| {
        let _ = kill(Pid::from_raw(pid as i32), sig);
    };
    for &pid in targets {
        signal(pid, Signal::SIGTERM);
    }
    std::thread::sleep(grace);
    let still_alive: HashSet<u32> = crate::proc::list_processes()
        .iter()
        .map(|proc| proc.pid)
        .collect();
    for &pid in targets {
        if still_alive.contains(&pid) {
            signal(pid, Signal::SIGKILL);
        }
    }
    targets.to_vec()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn kill_pids(_targets: &[u32], _grace: Duration) -> Vec<u32> {
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

    const SESSION: &str = "rimz-home-user-workspace-project-rimz-rimz";
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
                    "rimz sidebar serve --workspace-id {WS} --mux zellij --session-name {SESSION}"
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
        let mut got = select_sweep_targets(&procs, me, SESSION, WS, &protected, true);
        got.sort_unstable();
        assert_eq!(got, vec![10, 11, 12]);

        // `rimz reload`'s dead-session sweep excludes the mux server (pid 10), so a
        // probe that misread a live session as gone can only reap respawnable
        // daemons, never tear the session down.
        let mut daemons_only = select_sweep_targets(&procs, me, SESSION, WS, &protected, false);
        daemons_only.sort_unstable();
        assert_eq!(daemons_only, vec![11, 12]);
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
        let got = select_sweep_targets(&procs, me, SESSION, WS, &protected, true);
        assert_eq!(got, vec![10]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn is_stats_refresh_matches_only_the_held_or_standalone_dashboard() {
        assert!(is_stats_refresh("/usr/bin/rimz stats --refresh --hold"));
        assert!(is_stats_refresh("rimz stats --refresh"));
        assert!(is_stats_refresh("/tmp/Rimz Dev/rimz stats --refresh"));
        assert!(is_stats_refresh(
            "rimz --config /tmp/config.toml stats --refresh"
        ));
        assert!(!is_stats_refresh("/usr/bin/rimz stats"));
        assert!(!is_stats_refresh("/usr/bin/rimz stats --json"));
        assert!(!is_stats_refresh("cargo test -- stats --refresh"));
        assert!(!is_stats_refresh("rimz reload"));
        assert!(!is_stats_refresh(
            "rimz daemon content --slot 0 --worktree-root /p"
        ));
        assert!(!is_stats_refresh(
            "rimz sidebar serve --workspace ws --session rimz-x"
        ));
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

    #[test]
    fn is_sidebar_serve_matches_only_the_scoped_renderer_pair() {
        let wrapper =
            format!("rimz sidebar serve --mux zellij --workspace-id {WS} --session-name {SESSION}");
        let renderer = format!(
            "rimz sidebar serve --workspace-id {WS} --mux zellij --session-name {SESSION} --tick-seconds 1"
        );
        assert!(is_sidebar_serve(&wrapper, WS, SESSION));
        assert!(is_sidebar_serve(&renderer, WS, SESSION));

        let app_server =
            format!("rimz codex app-server serve --workspace-id {WS} --session-name {SESSION}");
        let mux_server =
            format!("zellij --server /run/user/1000/zellij/contract_version_1/{SESSION}");
        assert!(
            !is_sidebar_serve(&app_server, WS, SESSION),
            "app-server is not a sidebar"
        );
        assert!(
            !is_sidebar_serve(&mux_server, WS, SESSION),
            "the mux server is never reaped"
        );

        let other_session = "rimz sidebar serve --workspace-id ws_other --session-name rimz-other";
        assert!(!is_sidebar_serve(other_session, WS, SESSION));
    }

    #[test]
    fn sidebar_serve_args_match_recovery_process_detection() {
        let root = PathBuf::from("/tmp/rimz-recovery-serve");
        let width = crate::mux::SidebarWidth::default();
        let opts = crate::mux::SidebarPaneOptions {
            session_name: SESSION.to_owned(),
            workspace_id: WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            cwd: root,
            birth_size: width.birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            pristine_birth: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: Some(75),
        };

        for mux in [MuxName::Zellij, MuxName::Tmux] {
            let mut cmdline = vec![opts.rimz_bin.to_string_lossy().into_owned()];
            cmdline.extend(crate::mux::sidebar_serve_args(mux, &opts));
            assert!(
                is_sidebar_serve(
                    &cmdline.join(" "),
                    opts.workspace_id.as_str(),
                    &opts.session_name,
                ),
                "{mux} serve argv should be detected as sidebar chrome",
            );
        }
    }
}
