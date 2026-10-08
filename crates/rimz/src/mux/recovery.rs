//! The dangerous step is the process sweep: it signals processes by heuristic, so
//! it is scoped four ways — real uid, the recorded session name or a hook's
//! inherited workspace pin, an explicit exclusion of this process and its ancestors, and the
//! inherited environment domain — and it runs where the process backend can
//! enumerate the current user's process table.
//!
//! A hard reset also ends the processes still inside the room's sandbox views
//! (`temp_unit_holders`). That match is exact, the temp unit directory's own
//! identity at the process's `/tmp`, so it carries the uid and ancestor scopes
//! and no session-name or environment-domain guard.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::ids::{MuxName, PaneId};
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

#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequiredDomainCheck {
    World,
    Mux(MuxName),
}

fn names_session(cmdline: &str, session_name: &str) -> bool {
    cmdline
        .split(|c: char| c.is_whitespace() || c == '/')
        .any(|token| token == session_name)
}

/// What a process sweep may kill beyond the room's respawnable sidebar and
/// app-server daemons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SweepScope {
    /// The session is already killed: also sweep its pure mux server and reap
    /// the `rimz hooks feed` processes pinned to the workspace.
    Teardown,
    /// The session only reads as gone and may be live: kill nothing more.
    LivenessProbe,
}

/// Classify an orphaned room process and carry the environment-domain guard
/// required before signalling it. The exact session scopes both server and
/// daemon matches; [`SweepScope::Teardown`] admits pure server matches.
#[cfg(any(unix, test))]
fn classify_sweep_target(
    cmdline: &str,
    session_name: &str,
    workspace_id: &str,
    scope: SweepScope,
) -> Option<RequiredDomainCheck> {
    if !names_session(cmdline, session_name) {
        return None;
    }
    let mux_server = cmdline.contains("--server");
    let workspace_daemon = cmdline.contains(workspace_id)
        && (cmdline.contains("sidebar") || cmdline.contains("app-server"));
    if !(scope == SweepScope::Teardown && mux_server || workspace_daemon) {
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
    scope: SweepScope,
    hook_identity: impl Fn(u32) -> Option<(Vec<std::ffi::OsString>, String)>,
) -> Vec<(u32, RequiredDomainCheck)> {
    procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter_map(|proc| {
            classify_sweep_target(&proc.cmdline, session_name, workspace_id, scope)
                .or_else(|| {
                    // Hooks can outlive their pane and publish caches. Only an
                    // explicit teardown may reap them, not reload's liveness probe.
                    if scope != SweepScope::Teardown
                        || !["hooks feed", "hooks drain", "hooks apply"]
                            .iter()
                            .any(|command| proc.cmdline.contains(command))
                    {
                        return None;
                    }
                    let (argv, pin) = hook_identity(proc.pid)?;
                    let [program, command, subcommand, ..] = argv.as_slice() else {
                        return None;
                    };
                    (std::path::Path::new(program)
                        .file_name()
                        .is_some_and(|name| name == "rimz")
                        && command == "hooks"
                        && ["feed", "drain", "apply"]
                            .iter()
                            .any(|name| subcommand == name)
                        && pin == workspace_id)
                        .then_some(RequiredDomainCheck::World)
                })
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
/// runs it after killing the session ([`SweepScope::Teardown`]); `rimz reload`
/// runs it for a workspace whose session a probe read as gone, reaping only
/// respawnable sidebar/app-server leftovers ([`SweepScope::LivenessProbe`]) so a
/// misread live session is never destroyed.
/// Teardown also reaps hook feeds carrying this workspace's inherited pin.
#[cfg(unix)]
pub(crate) fn sweep_orphan_processes(
    workspace_id: &str,
    session_name: &str,
    scope: SweepScope,
) -> KillOutcome {
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, std::process::id());
    let own_domain = ProcessDomain::current();
    let targets = select_sweep_targets(
        &procs,
        current_uid(),
        session_name,
        workspace_id,
        &protected,
        scope,
        |pid| {
            Some((
                crate::proc::argv(pid)?,
                crate::proc::env_var(pid, crate::workspace::ENV_WORKSPACE_ID)?,
            ))
        },
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
    kill_pids(&targets, SWEEP_GRACE)
}

#[cfg(not(unix))]
pub(crate) fn sweep_orphan_processes(
    _workspace_id: &str,
    _session_name: &str,
    _scope: SweepScope,
) -> KillOutcome {
    KillOutcome::default()
}

/// Pick this user's processes whose `/tmp` is one of `units` by `(dev, ino)`,
/// minus this process and its ancestors. A process whose root cannot be read
/// has no `/tmp` identity and is spared.
fn select_temp_unit_holders(
    procs: Vec<ProcInfo>,
    my_uid: u32,
    protected: &HashSet<u32>,
    units: &HashSet<(u64, u64)>,
    tmp_identity: impl Fn(u32) -> Option<(u64, u64)>,
) -> Vec<ProcInfo> {
    procs
        .into_iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| tmp_identity(proc.pid).is_some_and(|tmp| units.contains(&tmp)))
        .collect()
}

/// This user's processes still inside one of the room's sandbox views: those
/// whose `/tmp` is one of the temp unit directories `units`, excluding the
/// caller and its ancestors. The match is the unit directory's own identity,
/// which a bind mount keeps across the reset's rename, so it needs no
/// environment-domain guard. A process whose root cannot be read is logged and
/// spared. Empty off Linux, where no sandbox view exists.
pub(crate) fn temp_unit_holders(units: &[std::path::PathBuf]) -> Vec<ProcInfo> {
    let me = std::process::id();
    let tmp = std::path::Path::new("/tmp");
    // A host path read through the caller's own root is the directory itself.
    let units: HashSet<(u64, u64)> = units
        .iter()
        .filter_map(|unit| crate::proc::root_path_identity(me, unit))
        .collect();
    if units.is_empty() {
        return Vec::new();
    }
    let procs = crate::proc::list_processes();
    let protected = protected_pids(&procs, me);
    select_temp_unit_holders(procs, current_uid(), &protected, &units, |pid| {
        let identity = crate::proc::root_path_identity(pid, tmp);
        if identity.is_none() {
            tracing::debug!(
                pid,
                "cannot read the process root; not a sandbox view holder"
            );
        }
        identity
    })
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
/// — `rimz sidebar serve` — and not the mux server, the agent app-server, or the
/// session's `rimz sidebar host`. The exact recorded session name plus the
/// workspace id scope it; the adjacent `sidebar serve` argv words select the
/// supervisor and worker pair, so a session or path that merely contains
/// either word selects nothing.
pub(crate) fn is_sidebar_serve(cmdline: &str, workspace_id: &str, session_name: &str) -> bool {
    let mut words = cmdline.split_whitespace().peekable();
    let mut serve_command = false;
    while let Some(word) = words.next() {
        serve_command |= word == "sidebar" && words.peek() == Some(&"serve");
    }
    serve_command && names_session(cmdline, session_name) && cmdline.contains(workspace_id)
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
pub(super) fn kill_sidebar_serve_for_pane(
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

/// SIGTERM each pid, allow `grace` for exit, then SIGKILL any still alive and
/// confirm exit within two seconds, warning about survivors. Reports signalled
/// and escalated pids. Identity tokens prevent waiting on or escalating a reused pid. The shared
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
    let mut live = targets
        .iter()
        .map(|&pid| (pid, crate::proc::process_start_token(pid)))
        .collect::<Vec<_>>();
    for &pid in targets {
        signal(pid, Signal::SIGTERM);
    }
    wait_for_exit(&mut live, grace);
    let sigkilled = live.iter().map(|(pid, _)| *pid).collect::<Vec<_>>();
    for &pid in &sigkilled {
        signal(pid, Signal::SIGKILL);
    }
    wait_for_exit(&mut live, Duration::from_secs(2));
    if !live.is_empty() {
        let survived = live.iter().map(|(pid, _)| *pid).collect::<Vec<_>>();
        tracing::warn!(pids = ?survived, "processes survived the room process sweep");
    }
    KillOutcome {
        signalled: targets.to_vec(),
        sigkilled,
    }
}

#[cfg(unix)]
fn wait_for_exit(targets: &mut Vec<(u32, Option<String>)>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        targets.retain(|(pid, start)| crate::proc::process_is_live(*pid, start.as_deref()));
        if targets.is_empty() || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(not(unix))]
pub(crate) fn kill_pids(_targets: &[u32], _grace: Duration) -> KillOutcome {
    KillOutcome::default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ids::WorkspaceId;

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
            select_sweep_targets(
                &procs,
                me,
                SESSION,
                WS,
                &protected,
                SweepScope::Teardown,
                |_| None
            ),
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
            select_sweep_targets(
                &procs,
                me,
                SESSION,
                WS,
                &protected,
                SweepScope::LivenessProbe,
                |_| None
            ),
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
        let got = select_sweep_targets(
            &procs,
            me,
            SESSION,
            WS,
            &protected,
            SweepScope::Teardown,
            |_| None,
        );
        assert_eq!(got, vec![(10, RequiredDomainCheck::Mux(MuxName::Zellij))]);
    }

    #[test]
    fn temp_unit_holders_match_only_by_unit_identity() {
        let me = 1000;
        let canonical_unit = (64, 7);
        // A unit an earlier failed reset left under the detached `tmp/` sibling.
        let detached_unit = (64, 8);
        let units = HashSet::from([canonical_unit, detached_unit]);
        let procs = vec![
            proc(1, 0, me, "init"),
            proc(100, 1, me, "zsh"),
            proc(101, 100, me, "rimz reset --hard"),
            proc(10, 1, me, "sh -c writer"),
            proc(11, 1, me, "node dev-server"),
            // Another user's process in the same view.
            proc(20, 1, me + 1, "sh -c writer"),
            // A host process: its `/tmp` is not a unit.
            proc(21, 1, me, "cargo build"),
            // A process whose root cannot be read.
            proc(22, 1, me, "sh -c hidden"),
        ];
        // The caller runs inside a view itself, so it and its ancestor shell
        // hold a unit too.
        let tmp_identity = |pid| match pid {
            10 | 20 | 100 | 101 => Some(canonical_unit),
            11 => Some(detached_unit),
            21 => Some((64, 2)),
            _ => None,
        };
        let holders = select_temp_unit_holders(
            procs.clone(),
            me,
            &protected_pids(&procs, 101),
            &units,
            tmp_identity,
        );
        assert_eq!(
            holders.iter().map(|proc| proc.pid).collect::<Vec<_>>(),
            [10, 11]
        );
    }

    #[test]
    fn teardown_sweeps_pinned_hook_writers_but_reload_spares_them() {
        let procs = vec![
            proc(10, 1, 1000, "/build/rimz hooks feed --source claude"),
            proc(11, 1, 1000, "rimz hooks feed --source claude"),
            proc(12, 1, 1000, "rimz hooks feed --source claude"),
            proc(13, 1, 1000, "echo rimz hooks feed --source claude"),
            proc(14, 1, 2000, "rimz hooks feed --source claude"),
            proc(15, 1, 1000, "rimz hooks feed --source claude"),
            proc(16, 1, 1000, "claude --worktree main"),
        ];
        let hook_identity = |pid| {
            let pin = match pid {
                11 => "ws-other",
                12 => return None,
                _ => WS,
            };
            let process = procs.iter().find(|process| process.pid == pid).unwrap();
            Some((
                process.cmdline.split_whitespace().map(Into::into).collect(),
                pin.to_owned(),
            ))
        };
        let protected = HashSet::from([15]);
        assert_eq!(
            select_sweep_targets(
                &procs,
                1000,
                SESSION,
                WS,
                &protected,
                SweepScope::Teardown,
                hook_identity
            ),
            vec![(10, RequiredDomainCheck::World)],
            "a room's hook writer can outlive its mux without naming the session in argv",
        );
        assert!(
            select_sweep_targets(
                &procs,
                1000,
                SESSION,
                WS,
                &protected,
                SweepScope::LivenessProbe,
                hook_identity
            )
            .is_empty(),
            "reload must not kill hooks on a possibly mistaken dead-session probe",
        );
    }

    #[cfg(unix)]
    #[test]
    fn teardown_sweeps_pinned_hook_drainers_and_apply_children() {
        let procs = vec![
            proc(10, 1, 1000, "/build/rimz hooks drain --project-root /work"),
            proc(11, 1, 1000, "/build/rimz hooks apply"),
        ];
        let identity = |pid| {
            let process = procs.iter().find(|process| process.pid == pid).unwrap();
            Some((
                process.cmdline.split_whitespace().map(Into::into).collect(),
                WS.into(),
            ))
        };
        assert_eq!(
            select_sweep_targets(
                &procs,
                1000,
                SESSION,
                WS,
                &HashSet::new(),
                SweepScope::Teardown,
                identity
            ),
            vec![
                (10, RequiredDomainCheck::World),
                (11, RequiredDomainCheck::World)
            ]
        );
        assert!(
            select_sweep_targets(
                &procs,
                1000,
                SESSION,
                WS,
                &HashSet::new(),
                SweepScope::LivenessProbe,
                identity
            )
            .is_empty()
        );
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
    fn sweep_and_sidebar_matching_do_not_cross_session_prefixes() {
        for name in ["repo-abcdef", "other-repo-abcd", "repo-abcd-suffix"] {
            let server = format!("zellij --server /run/zellij/contract_version_1/{name}");
            let sidebar = format!("rimz sidebar serve --workspace-id {WS} --session-name {name}");
            assert_eq!(
                classify_sweep_target(&server, "repo-abcd", WS, SweepScope::Teardown),
                None
            );
            assert_eq!(
                classify_sweep_target(&sidebar, "repo-abcd", WS, SweepScope::Teardown),
                None
            );
            assert!(!is_sidebar_serve(&sidebar, WS, "repo-abcd"));
        }
        for suffix in ["", " ", "/", "\t"] {
            let server = format!("zellij --server /run/zellij/repo-abcd{suffix}");
            assert_eq!(
                classify_sweep_target(&server, "repo-abcd", WS, SweepScope::Teardown),
                Some(RequiredDomainCheck::Mux(MuxName::Zellij))
            );
        }
    }

    #[test]
    fn the_room_host_is_swept_at_teardown_and_is_never_a_serve_process() {
        // A session whose name carries both words the serve matcher reads.
        let session = "sidebar-observer-abcd";
        let host = format!(
            "/usr/bin/rimz sidebar host --mux zellij --workspace-id {WS} --session-name {session}"
        );

        assert_eq!(
            classify_sweep_target(&host, session, WS, SweepScope::Teardown),
            Some(RequiredDomainCheck::World),
            "teardown kills the host with the room's other daemons"
        );
        assert!(
            !is_sidebar_serve(&host, WS, session),
            "the host is no pane's serve process: nothing attributes it to a pane or counts it as one"
        );
        let serve = format!(
            "/usr/bin/rimz sidebar serve --mux zellij --workspace-id {WS} --session-name {session}"
        );
        assert!(is_sidebar_serve(&serve, WS, session));
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
            runtime: crate::disk::paths::RuntimePaths::under(
                WorkspaceId::from_project_root(&root),
                tempfile::tempdir().expect("runtime root").path(),
            )
            .expect("runtime paths"),
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
