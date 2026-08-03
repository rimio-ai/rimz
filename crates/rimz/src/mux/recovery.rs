//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by `rimz reset` and attached `rimz start` auto-reset, so teardown
//! lives in exactly one place and is testable without a real multiplexer.
//!
//! The dangerous step is the process sweep: it signals processes by heuristic, so
//! it is scoped four ways — real uid, the exact path-derived session name in the
//! command line, an explicit exclusion of this process and its ancestors, and the
//! inherited environment domain — and it runs where the process backend can
//! enumerate the current user's process table.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::RuntimePaths;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::MuxBackend;
use crate::mux::domain::ProcessDomain;
use crate::proc::ProcInfo;

/// Grace between SIGTERM and SIGKILL in the process sweep — long enough for a
/// well-behaved process to exit on its own, short enough not to stall `reset`.
pub(crate) const SWEEP_GRACE: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KillOutcome {
    pub(crate) signalled: Vec<u32>,
    pub(crate) sigkilled: Vec<u32>,
}

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

#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequiredDomainCheck {
    World,
    Mux(MuxName),
}

/// Classify an orphaned room process and carry the environment-domain guard
/// required before signalling it. The exact session scopes both server and
/// daemon matches; `include_mux_server` controls pure server matches only.
#[cfg(any(unix, test))]
fn classify_sweep_target(
    cmdline: &str,
    session_name: &str,
    workspace_id: &str,
    include_mux_server: bool,
) -> Option<RequiredDomainCheck> {
    if !cmdline.contains(session_name) {
        return None;
    }
    let mux_server = cmdline.contains("--server");
    let workspace_daemon = cmdline.contains(workspace_id)
        && (cmdline.contains("sidebar") || cmdline.contains("app-server"));
    if !(include_mux_server && mux_server || workspace_daemon) {
        return None;
    }
    Some(if mux_server {
        RequiredDomainCheck::Mux(MuxName::Zellij)
    } else {
        RequiredDomainCheck::World
    })
}

/// Pick ordered `(pid, domain guard)` verdicts for this user's matching
/// processes, minus this process and its ancestors.
#[cfg(any(unix, test))]
fn select_sweep_targets(
    procs: &[ProcInfo],
    my_uid: u32,
    session_name: &str,
    workspace_id: &str,
    protected: &HashSet<u32>,
    include_mux_server: bool,
) -> Vec<(u32, RequiredDomainCheck)> {
    procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter_map(|proc| {
            classify_sweep_target(
                &proc.cmdline,
                session_name,
                workspace_id,
                include_mux_server,
            )
            .map(|check| (proc.pid, check))
        })
        .collect()
}

/// This process plus its ancestor chain — the pids the sweep must never signal,
/// so `rimz reset`/`rimz reload` cannot kill the shell or attach that launched it.
pub(crate) fn protected_pids(procs: &[ProcInfo], self_pid: u32) -> HashSet<u32> {
    let parents: HashMap<u32, u32> = procs.iter().map(|proc| (proc.pid, proc.ppid)).collect();
    let mut protected = HashSet::new();
    let mut current = self_pid;
    while current != 0 && protected.insert(current) {
        let Some(parent) = parents.get(&current).copied() else {
            break;
        };
        current = parent;
    }
    protected
}

/// Sweep this user's orphaned server / leaked daemons for `(workspace, session)`
/// (SIGTERM→grace→SIGKILL), excluding the caller and its ancestors. `rimz reset`
/// runs it after killing the session (`include_mux_server: true`); `rimz reload`
/// runs it for a workspace whose session a probe read as gone, reaping only
/// respawnable sidebar/app-server leftovers (`include_mux_server: false`) so a
/// misread live session is never destroyed.
#[cfg(unix)]
pub(crate) fn sweep_orphan_processes(
    workspace_id: &str,
    session_name: &str,
    include_mux_server: bool,
) -> Vec<u32> {
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let own_domain = ProcessDomain::current();
    let targets = select_sweep_targets(
        &procs,
        current_uid(),
        session_name,
        workspace_id,
        &protected,
        include_mux_server,
    )
    .into_iter()
    .filter_map(|(pid, check)| {
        let matches = match check {
            RequiredDomainCheck::World => own_domain.same_world_as_process(pid),
            RequiredDomainCheck::Mux(mux) => own_domain.same_mux_endpoint_as_process(pid, mux),
        };
        if matches { Some(pid) } else { None }
    })
    .collect::<Vec<_>>();
    kill_pids(&targets, SWEEP_GRACE).signalled
}

#[cfg(not(unix))]
pub(crate) fn sweep_orphan_processes(
    _workspace_id: &str,
    _session_name: &str,
    _include_mux_server: bool,
) -> Vec<u32> {
    Vec::new()
}

/// SIGUSR1 every `rimz stats --refresh` dashboard this user owns in this state
/// domain so each re-execs in place onto the freshly-installed binary. Stats
/// are mux-agnostic, so a reload refreshes the daemon-view pane and any
/// standalone dashboard in the same world alike. Returns the pids signalled;
/// empty where the process backend cannot enumerate processes and the dashboard
/// reloads via its own `r` key.
#[cfg(unix)]
pub(crate) fn reload_stats_dashboards() -> Vec<u32> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    #[cfg(feature = "testkit")]
    if std::env::var_os("RIMZ_TEST_SKIP_STATS_RELOAD").is_some() {
        return Vec::new();
    }
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let my_uid = current_uid();
    let own_domain = ProcessDomain::current();
    let targets: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_stats_refresh(&proc.cmdline))
        .filter(|proc| own_domain.same_world_as_process(proc.pid))
        .map(|proc| proc.pid)
        .collect();
    for &pid in &targets {
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGUSR1);
    }
    targets
}

#[cfg(not(unix))]
pub(crate) fn reload_stats_dashboards() -> Vec<u32> {
    Vec::new()
}

/// Whether `cmdline` is a held or standalone `rimz stats --refresh` dashboard.
/// Token matching keeps the user-wide signal pass scoped to the RimZ stats
/// subcommand and excludes one-shot reports and unrelated commands mentioning
/// those words.
#[cfg(any(unix, test))]
fn is_stats_refresh(cmdline: &str) -> bool {
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
/// renderer. Same uid/ancestor/environment scoping as the orphan sweep. Returns
/// the number of processes signalled; empty where `list_processes` is empty.
pub(crate) fn kill_sidebar_serve_for_pane(
    workspace_id: &str,
    session_name: &str,
    pane: &PaneId,
    mux: MuxName,
) -> usize {
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let my_uid = current_uid();
    let own_domain = ProcessDomain::current();
    let targets: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_sidebar_serve(&proc.cmdline, workspace_id, session_name))
        .filter(|proc| attributed_pane(proc.pid, mux).as_ref() == Some(pane))
        .filter(|proc| own_domain.same_mux_endpoint_as_process(proc.pid, mux))
        .map(|proc| proc.pid)
        .collect();
    kill_pids(&targets, SWEEP_GRACE).signalled.len()
}

#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

#[cfg(not(unix))]
pub(crate) fn current_uid() -> u32 {
    u32::MAX
}

/// SIGTERM each pid, wait `grace`, then SIGKILL any still alive; reports every
/// pid signalled and the subset that needed escalation. The shared
/// graceful-then-forceful kill path for the `rimz reset` orphan sweep and
/// `rimz reload`'s zombie-sidebar reaping.
#[cfg(unix)]
pub(crate) fn kill_pids(targets: &[u32], grace: Duration) -> KillOutcome {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    if targets.is_empty() {
        return KillOutcome::default();
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
    let sigkilled = targets
        .iter()
        .copied()
        .filter(|pid| still_alive.contains(pid))
        .collect::<Vec<_>>();
    for &pid in &sigkilled {
        signal(pid, Signal::SIGKILL);
    }
    KillOutcome {
        signalled: targets.to_vec(),
        sigkilled,
    }
}

#[cfg(not(unix))]
pub(crate) fn kill_pids(_targets: &[u32], _grace: Duration) -> KillOutcome {
    KillOutcome::default()
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
            // A workspace daemon containing `--server` is selected even when mux
            // server sweeping is disabled, but still requires the mux-domain guard.
            proc(
                13,
                1,
                me,
                &format!(
                    "rimz sidebar serve --server --workspace-id {WS} --session-name {SESSION}"
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
            // An unrelated tmux server is excluded by the session-name scope.
            proc(24, 1, me, "tmux -L unrelated start-server"),
        ];
        let protected = HashSet::new();
        assert_eq!(
            select_sweep_targets(&procs, me, SESSION, WS, &protected, true),
            vec![
                (10, RequiredDomainCheck::Mux(MuxName::Zellij)),
                (11, RequiredDomainCheck::World),
                (12, RequiredDomainCheck::World),
                (13, RequiredDomainCheck::Mux(MuxName::Zellij)),
            ],
        );

        // `rimz reload`'s dead-session sweep excludes the mux server (pid 10), so a
        // probe that misread a live session as gone can only reap respawnable
        // daemons, never tear the session down.
        assert_eq!(
            select_sweep_targets(&procs, me, SESSION, WS, &protected, false),
            vec![
                (11, RequiredDomainCheck::World),
                (12, RequiredDomainCheck::World),
                (13, RequiredDomainCheck::Mux(MuxName::Zellij)),
            ],
        );
    }

    #[test]
    fn sweep_excludes_self_and_ancestors() {
        let me = 1000;
        // A reset process tree: shell(100) -> rimz reset(101) -> this(102). None of
        // them carry the session name, but protect them explicitly regardless.
        let procs = vec![
            proc(1, 0, me, "init"),
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
        assert!(protected.contains(&1));
        let got = select_sweep_targets(&procs, me, SESSION, WS, &protected, true);
        assert_eq!(got, vec![(10, RequiredDomainCheck::Mux(MuxName::Zellij))]);
    }

    #[cfg(unix)]
    #[test]
    fn is_stats_refresh_matches_only_the_held_or_standalone_dashboard() {
        assert!(is_stats_refresh("/usr/bin/rimz stats --refresh --hold"));
        assert!(is_stats_refresh("rimz stats --refresh"));
        assert!(is_stats_refresh("/tmp/RimZ Dev/rimz stats --refresh"));
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
        let opts = crate::mux::SidebarPaneOptions {
            session_name: SESSION.to_owned(),
            workspace_id: WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            extra_env: Default::default(),
            cwd: root,
            target: crate::mux::SidebarTarget {
                share: crate::mux::WidthPermille::from_percent(25),
                max_cols: std::num::NonZeroU16::new(72).expect("nonzero test width"),
                pinned: false,
            },
            detected_view_size: None,
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
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
