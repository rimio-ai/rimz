//! Daemon dashboard child supervisors. Content slots resolve their command
//! from the current per-machine `[daemon]` config so edits take effect in place.
//! Content supervisors and host wrappers follow the room's recorded build.
//!
//! Internals: `docs/internals/rimzd.md`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use serde::Deserialize;

#[cfg(unix)]
use crate::child_process::CleanupSignalMask;
use crate::child_process::{SupervisedChild, register_signal_wake, wait_wake};
use crate::config::{DaemonConfig, DaemonPane, MachineConfig};
use crate::ids::WorkspaceId;
use crate::reload::{RoomBuildFollower, exec_current_args};

const STATS_TOKEN: &str = "stats";

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);
const HOST_STOP_GRACE: Duration = Duration::from_secs(10);
const CHILD_TREE_POLL: Duration = Duration::from_millis(25);

struct DaemonChild {
    process: SupervisedChild,
    pid: u32,
    start: Option<String>,
}

impl DaemonChild {
    fn adopt(child: Child, wake: mpsc::Sender<()>) -> Self {
        let pid = child.id();
        let start = crate::proc::process_start_token(pid);
        Self {
            process: SupervisedChild::adopt(child, wake),
            pid,
            start,
        }
    }
}

struct SupervisorWake {
    tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<()>,
    signals: mpsc::Receiver<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPane {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

fn stats_argv(rimz_bin: &Path) -> Vec<String> {
    vec![
        rimz_bin.to_string_lossy().into_owned(),
        STATS_TOKEN.to_owned(),
        "--refresh".to_owned(),
        "--hold".to_owned(),
    ]
}

pub(crate) fn resolve_content(
    daemon: &DaemonConfig,
    rimz_bin: &Path,
    worktree_root: &Path,
) -> Vec<ResolvedPane> {
    let resolved: Vec<ResolvedPane> = daemon
        .pane
        .iter()
        .filter_map(|pane| resolve_pane(pane, rimz_bin, worktree_root))
        .collect();
    if resolved.is_empty() {
        vec![stats_pane(rimz_bin, worktree_root)]
    } else {
        resolved
    }
}

fn resolve_slot(
    daemon: &DaemonConfig,
    slot: usize,
    rimz_bin: &Path,
    worktree_root: &Path,
) -> ResolvedPane {
    resolve_content(daemon, rimz_bin, worktree_root)
        .into_iter()
        .nth(slot)
        .unwrap_or_else(|| stats_pane(rimz_bin, worktree_root))
}

fn resolve_pane(pane: &DaemonPane, rimz_bin: &Path, worktree_root: &Path) -> Option<ResolvedPane> {
    let cwd = match &pane.cwd {
        Some(cwd) if cwd.is_absolute() => cwd.clone(),
        Some(cwd) => worktree_root.join(cwd),
        None => worktree_root.to_path_buf(),
    };
    if pane.command == STATS_TOKEN {
        return Some(ResolvedPane {
            argv: stats_argv(rimz_bin),
            cwd,
        });
    }
    match shlex::split(&pane.command) {
        Some(argv) if !argv.is_empty() => Some(ResolvedPane { argv, cwd }),
        _ => {
            tracing::warn!(
                command = %pane.command,
                "skipping daemon pane: unparseable or empty command",
            );
            None
        }
    }
}

pub fn run_supervisor(
    slot: usize,
    worktree_root: &Path,
    workspace_id: Option<&WorkspaceId>,
) -> io::Result<ExitStatus> {
    let rimz_bin = workspace_id
        .and_then(|_| {
            crate::build_id::current()
                .map(|build| crate::disk::paths::builds_dir().join(build).join("rimz"))
                .filter(|path| path.is_file())
                .or_else(crate::reload::current_reexec_target)
        })
        .unwrap_or_else(crate::proc::rimz_exe);
    let wake = signal_wake_channel()?;
    let mut follower = workspace_id.map(RoomBuildFollower::new);

    let config_path = MachineConfig::config_path();
    let daemon = load_daemon_config(&config_path).unwrap_or_default();
    let mut current = resolve_slot(&daemon, slot, &rimz_bin, worktree_root);
    let mut child = DaemonChild::adopt(spawn_child(&current)?, wake.tx.clone());
    let watch = {
        #[cfg(unix)]
        let _worker_mask = CleanupSignalMask::block()?;
        watch_config(&config_path, wake.tx.clone())
    };
    let mut reload_due_at: Option<Instant> = None;

    loop {
        let now = Instant::now();
        if terminate_signal_received() {
            return terminate_child(&mut child, &wake);
        }

        if let Some(follower) = &mut follower
            && follower.poll_if_due(now).is_some()
        {
            let status = terminate_child(&mut child, &wake)?;
            {
                #[cfg(unix)]
                let _handoff_mask = CleanupSignalMask::block()?;
                if terminate_signal_received() {
                    return Ok(status);
                }
                if let Some(target) = follower.recheck() {
                    let error = exec_current_args(&target);
                    tracing::warn!(%error, "daemon content exec failed; restarting current child");
                }
            }
            if terminate_signal_received() {
                return Ok(status);
            }
            child = DaemonChild::adopt(spawn_child(&current)?, wake.tx.clone());
        }

        let now = Instant::now();
        if drain_config_changes(watch.as_ref()) {
            reload_due_at = Some(now + CONFIG_RELOAD_DEBOUNCE);
        }
        if reload_due_at.is_some_and(|due| now >= due) {
            reload_due_at = None;
            if let Some(daemon) = load_daemon_config(&config_path) {
                let next = resolve_slot(&daemon, slot, &rimz_bin, worktree_root);
                if needs_restart(&current, &next) {
                    match reload_child(&mut child, &next, &wake)? {
                        ReloadedChild::Next(next_child) => {
                            child = next_child;
                            current = next;
                        }
                        ReloadedChild::Current => {}
                    }
                }
            }
        }

        match child.process.try_wait()? {
            Some(status) => return Ok(status),
            None => {
                let deadline = reload_due_at
                    .into_iter()
                    .chain(follower.as_ref().map(RoomBuildFollower::next_poll))
                    .min();
                wait_wake(&wake.rx, deadline);
            }
        }
    }
}

/// Hold a foreground host, restarting it only after a room-build change.
pub fn run_host(workspace_id: &WorkspaceId, argv: &[OsString]) -> io::Result<ExitStatus> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon host needs a command",
        ));
    };
    let wake = signal_wake_channel()?;
    let mut follower = RoomBuildFollower::new(workspace_id);
    let mut command = Command::new(program);
    command.args(args);
    let child = command.spawn()?;
    let mut child = DaemonChild::adopt(child, wake.tx.clone());
    loop {
        if terminate_signal_received() {
            return wait_host_exit(&mut child, &wake);
        }
        if follower.poll_if_due(Instant::now()).is_some() {
            let status = stop_child_tree(
                &mut child,
                signal_hook::consts::signal::SIGHUP,
                host_stop_grace(),
                &wake,
            )?;
            {
                #[cfg(unix)]
                let _handoff_mask = CleanupSignalMask::block()?;
                if terminate_signal_received() {
                    return Ok(status);
                }
                if let Some(target) = follower.recheck() {
                    let error = exec_current_args(&target);
                    tracing::warn!(%error, "daemon host exec failed; restarting current host");
                }
            }
            if terminate_signal_received() {
                return Ok(status);
            }
            child = DaemonChild::adopt(command.spawn()?, wake.tx.clone());
        }
        if let Some(status) = child.process.try_wait()? {
            return Ok(status);
        }
        wait_wake(&wake.rx, Some(follower.next_poll()));
    }
}

fn wait_host_exit(child: &mut DaemonChild, wake: &SupervisorWake) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.process.try_wait()? {
            return Ok(status);
        }
        for signal in wake.signals.try_iter() {
            signal_unreaped_child(child.pid, child.start.as_deref(), |pid| {
                signal_host(pid, signal);
            });
        }
        wait_wake(&wake.rx, None);
    }
}

fn signal_unreaped_child(pid: u32, start: Option<&str>, signal: impl FnOnce(u32)) {
    if start.is_none_or(|start| remembered_process_is_live(pid, start)) {
        signal(pid);
    }
}

fn stop_child_tree(
    child: &mut DaemonChild,
    signal: i32,
    grace: Duration,
    wake: &SupervisorWake,
) -> io::Result<ExitStatus> {
    let mut tree: Vec<_> = child
        .start
        .clone()
        .into_iter()
        .map(|start| (child.pid, start))
        .collect();
    if signal == signal_hook::consts::signal::SIGTERM
        && child.process.try_wait()?.is_none()
        && child
            .start
            .as_deref()
            .is_none_or(|start| remembered_process_is_live(child.pid, start))
        && let Some(scope) = crate::proc::cgroup::scope_of(child.pid)
        && let Err(error) = crate::proc::cgroup::thaw(&scope)
    {
        tracing::warn!(pid = child.pid, %error, "could not thaw daemon child before SIGTERM");
    }
    if child.start.is_none() && child.process.try_wait()?.is_none() {
        remember_children(child.pid, &mut tree);
    }
    extend_child_tree(&mut tree);
    for (pid, start) in tree.iter().rev() {
        if remembered_process_is_live(*pid, start) {
            signal_host(*pid, signal);
        }
    }
    if child.start.is_none() && child.process.try_wait()?.is_none() {
        signal_host(child.pid, signal);
    }
    let mut deadline = Instant::now().checked_add(grace);
    loop {
        let status = if signal == signal_hook::consts::signal::SIGHUP && terminate_signal_received()
        {
            Some(wait_host_exit(child, wake)?)
        } else {
            child.process.try_wait()?
        };
        tree.retain(|(pid, start)| remembered_process_is_live(*pid, start));
        if let Some(status) = status
            && tree.is_empty()
        {
            return Ok(status);
        }
        let now = Instant::now();
        if deadline.is_some_and(|deadline| now >= deadline) {
            if child.start.is_none() && status.is_none() {
                remember_children(child.pid, &mut tree);
            }
            extend_child_tree(&mut tree);
            for (pid, start) in tree.iter().rev() {
                if remembered_process_is_live(*pid, start) {
                    signal_host(*pid, signal_hook::consts::signal::SIGKILL);
                }
            }
            if child.start.is_none() && child.process.try_wait()?.is_none() {
                signal_host(child.pid, signal_hook::consts::signal::SIGKILL);
            }
            deadline = None;
        }
        // Orphaned descendants do not wake the child's waiter.
        let poll = (!tree.is_empty()).then(|| now + CHILD_TREE_POLL);
        wait_wake(&wake.rx, deadline.into_iter().chain(poll).min());
    }
}

fn extend_child_tree(tree: &mut Vec<(u32, String)>) {
    let mut index = 0;
    while index < tree.len() {
        let (pid, start) = &tree[index];
        if remembered_process_is_live(*pid, start) {
            remember_children(*pid, tree);
        }
        index += 1;
    }
}

fn remember_children(parent: u32, tree: &mut Vec<(u32, String)>) {
    for pid in crate::proc::children(parent) {
        if !tree.iter().any(|(known, _)| *known == pid)
            && let Some(start) = crate::proc::process_start_token(pid)
        {
            tree.push((pid, start));
        }
    }
}

fn remembered_process_is_live(pid: u32, start: &str) -> bool {
    // macOS's liveness fallback can succeed without checking the start token.
    crate::proc::process_start_token(pid).as_deref() == Some(start)
        && crate::proc::process_is_live(pid, Some(start))
}

fn signal_host(pid: u32, signal: i32) {
    #[cfg(unix)]
    if let Ok(signal) = nix::sys::signal::Signal::try_from(signal) {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), signal);
    }
}

fn host_stop_grace() -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(millis) = std::env::var("RIMZ_TEST_DAEMON_HOST_STOP_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(millis);
    }
    HOST_STOP_GRACE
}

fn signal_wake_channel() -> io::Result<SupervisorWake> {
    reset_signal_flags();
    install_signal_handlers()?;
    let (tx, rx) = mpsc::channel();
    let (signal_tx, signals) = mpsc::channel();
    #[cfg(unix)]
    let cleanup_signals = vec![
        signal_hook::consts::signal::SIGHUP,
        signal_hook::consts::signal::SIGTERM,
    ];
    #[cfg(not(unix))]
    let cleanup_signals = Vec::new();
    let wake = tx.clone();
    register_signal_wake(cleanup_signals, move |signal| {
        let _ = signal_tx.send(signal);
        let _ = wake.send(());
    })?;
    #[cfg(unix)]
    CleanupSignalMask::unblock()?;
    Ok(SupervisorWake { tx, rx, signals })
}

fn stats_pane(rimz_bin: &Path, worktree_root: &Path) -> ResolvedPane {
    ResolvedPane {
        argv: stats_argv(rimz_bin),
        cwd: worktree_root.to_path_buf(),
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DaemonConfigFile {
    daemon: DaemonConfig,
}

fn load_daemon_config(config_path: &Path) -> Option<DaemonConfig> {
    match std::fs::read_to_string(config_path) {
        Ok(text) => match toml::from_str::<DaemonConfigFile>(&text) {
            Ok(config) => Some(config.daemon),
            Err(err) => {
                tracing::debug!(
                    path = %config_path.display(),
                    error = %err,
                    "daemon content config reload skipped; keeping current child",
                );
                None
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some(DaemonConfig::default()),
        Err(err) => {
            tracing::debug!(
                path = %config_path.display(),
                error = %err,
                "daemon content config reload skipped; keeping current child",
            );
            None
        }
    }
}

fn needs_restart(current: &ResolvedPane, next: &ResolvedPane) -> bool {
    current.argv != next.argv || current.cwd != next.cwd
}

enum ReloadedChild {
    Next(DaemonChild),
    Current,
}

fn reload_child(
    child: &mut DaemonChild,
    next: &ResolvedPane,
    wake: &SupervisorWake,
) -> io::Result<ReloadedChild> {
    match spawn_child(next) {
        Ok(next_child) => {
            let mut next_child = DaemonChild::adopt(next_child, wake.tx.clone());
            if let Err(err) = terminate_child(child, wake) {
                let _ = terminate_child(&mut next_child, wake);
                return Err(err);
            }
            Ok(ReloadedChild::Next(next_child))
        }
        Err(err) => {
            tracing::warn!(
                argv = ?next.argv,
                cwd = %next.cwd.display(),
                error = %err,
                "daemon content reload command failed; keeping current child",
            );
            Ok(ReloadedChild::Current)
        }
    }
}

fn spawn_child(pane: &ResolvedPane) -> io::Result<Child> {
    let Some((program, rest)) = pane.argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon content pane resolved to an empty argv",
        ));
    };
    Command::new(program)
        .args(rest)
        .current_dir(&pane.cwd)
        .spawn()
}

struct ConfigWatch {
    _watcher: crate::fs_watch::RecommendedWatcher,
    rx: mpsc::Receiver<()>,
}

fn watch_config(config_path: &Path, wake: mpsc::Sender<()>) -> Option<ConfigWatch> {
    let Some(parent) = config_path.parent() else {
        tracing::debug!(
            path = %config_path.display(),
            "daemon content hot-reload disabled: config path has no parent",
        );
        return None;
    };
    let watched_parent = parent.to_path_buf();
    let watched_name = config_path.file_name().map(ToOwned::to_owned);
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher =
        match crate::fs_watch::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                )
                && event.paths.iter().any(|path| {
                    path.parent() == Some(watched_parent.as_path())
                        && path.file_name() == watched_name.as_deref()
                })
            {
                let _ = event_tx.send(());
                let _ = wake.send(());
            }
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::debug!(
                    path = %config_path.display(),
                    error = %err,
                    "daemon content hot-reload watcher could not start",
                );
                return None;
            }
        };
    if let Err(err) = watcher.watch(parent, RecursiveMode::NonRecursive) {
        tracing::debug!(
            path = %parent.display(),
            error = %err,
            "daemon content hot-reload watcher could not watch config directory",
        );
        return None;
    }
    Some(ConfigWatch {
        _watcher: watcher,
        rx: event_rx,
    })
}

fn drain_config_changes(watch: Option<&ConfigWatch>) -> bool {
    let Some(watch) = watch else {
        return false;
    };
    let mut changed = false;
    while watch.rx.try_recv().is_ok() {
        changed = true;
    }
    changed
}

fn reset_signal_flags() {
    terminate_signal_flag().store(0, Ordering::SeqCst);
    ignored_sigint_flag().store(false, Ordering::SeqCst);
}

fn terminate_signal_received() -> bool {
    terminate_signal_flag().load(Ordering::SeqCst) != 0
}

fn terminate_signal_flag() -> &'static Arc<AtomicUsize> {
    static FLAG: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicUsize::new(0)))
}

fn ignored_sigint_flag() -> &'static Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

#[cfg(unix)]
fn install_signal_handlers() -> io::Result<()> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

    signal_hook::flag::register(SIGINT, ignored_sigint_flag().clone())?;
    for signal in [SIGHUP, SIGTERM] {
        signal_hook::flag::register_usize(
            signal,
            terminate_signal_flag().clone(),
            signal as usize,
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handlers() -> io::Result<()> {
    Ok(())
}

fn terminate_child(child: &mut DaemonChild, wake: &SupervisorWake) -> io::Result<ExitStatus> {
    stop_child_tree(
        child,
        signal_hook::consts::signal::SIGTERM,
        CHILD_SIGNAL_GRACE,
        wake,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreaped_child_without_a_start_token_still_receives_its_signal() {
        let mut signalled = None;
        signal_unreaped_child(42, None, |pid| signalled = Some(pid));
        assert_eq!(signalled, Some(42));
    }

    #[test]
    fn config_watch_ignores_access_and_sibling_changes() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let sibling = dir.path().join("sibling");
        let sibling_dir = dir.path().join("ws");
        std::fs::write(&config, "").unwrap();
        std::fs::write(&sibling, "").unwrap();
        std::fs::create_dir(&sibling_dir).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        let watch = watch_config(&config, wake_tx).unwrap();

        std::fs::read(&config).unwrap();
        std::fs::read(&sibling).unwrap();
        std::fs::read_dir(&sibling_dir).unwrap().count();
        std::fs::read_dir(dir.path()).unwrap().count();
        std::fs::write(&sibling, "changed").unwrap();
        let renamed = dir.path().join("renamed");
        std::fs::rename(&sibling, &renamed).unwrap();
        std::fs::remove_file(&renamed).unwrap();
        std::fs::write(dir.path().join("new-sibling"), "").unwrap();

        assert_eq!(
            wake_rx.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert!(!drain_config_changes(Some(&watch)));
    }

    #[test]
    fn config_watch_wakes_for_config_mutations() {
        for mutation in ["create", "write", "replace", "remove"] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config.toml");
            let replacement = dir.path().join("config.tmp");
            if mutation != "create" {
                std::fs::write(&config, "").unwrap();
            }
            std::fs::write(&replacement, "[daemon]\n").unwrap();
            let (wake_tx, wake_rx) = mpsc::channel();
            let watch = watch_config(&config, wake_tx).unwrap();

            match mutation {
                "create" | "write" => std::fs::write(&config, "[daemon]\n").unwrap(),
                "replace" => std::fs::rename(&replacement, &config).unwrap(),
                "remove" => std::fs::remove_file(&config).unwrap(),
                _ => unreachable!(),
            }

            wake_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|err| panic!("no wake for {mutation}: {err}"));
            assert!(drain_config_changes(Some(&watch)), "{mutation}");
        }
    }

    fn pane(command: &str, cwd: Option<&str>) -> DaemonPane {
        DaemonPane {
            command: command.to_owned(),
            cwd: cwd.map(PathBuf::from),
        }
    }

    #[test]
    fn stats_argv_runs_refreshing_held_stats() {
        assert_eq!(
            stats_argv(Path::new("/usr/bin/rimz")),
            vec![
                "/usr/bin/rimz".to_owned(),
                "stats".to_owned(),
                "--refresh".to_owned(),
                "--hold".to_owned(),
            ]
        );
    }

    #[test]
    fn resolve_content_defaults_to_live_stats() {
        assert_eq!(
            resolve_content(
                &DaemonConfig::default(),
                Path::new("/usr/bin/rimz"),
                Path::new("/proj/wt")
            ),
            vec![ResolvedPane {
                argv: stats_argv(Path::new("/usr/bin/rimz")),
                cwd: PathBuf::from("/proj/wt"),
            }]
        );
    }

    #[test]
    fn resolve_pane_expands_stats_token_and_relative_cwd() {
        assert_eq!(
            resolve_pane(
                &pane("stats", Some("reports")),
                Path::new("/usr/bin/rimz"),
                Path::new("/proj/wt"),
            ),
            Some(ResolvedPane {
                argv: stats_argv(Path::new("/usr/bin/rimz")),
                cwd: PathBuf::from("/proj/wt/reports"),
            })
        );
    }

    #[test]
    fn resolve_content_splits_shell_commands_and_resolves_cwd() {
        let daemon = DaemonConfig {
            pane: vec![
                pane(r#"btop --config "two words""#, None),
                pane("tail -f app.log", Some("/var/log")),
            ],
        };

        assert_eq!(
            resolve_content(&daemon, Path::new("/usr/bin/rimz"), Path::new("/proj/wt")),
            vec![
                ResolvedPane {
                    argv: vec![
                        "btop".to_owned(),
                        "--config".to_owned(),
                        "two words".to_owned(),
                    ],
                    cwd: PathBuf::from("/proj/wt"),
                },
                ResolvedPane {
                    argv: vec!["tail".to_owned(), "-f".to_owned(), "app.log".to_owned()],
                    cwd: PathBuf::from("/var/log"),
                },
            ]
        );
    }

    #[test]
    fn resolve_content_skips_unparseable_commands_and_falls_back_to_stats() {
        let daemon = DaemonConfig {
            pane: vec![pane("   ", None), pane(r#""unterminated"#, None)],
        };

        assert_eq!(
            resolve_content(&daemon, Path::new("/usr/bin/rimz"), Path::new("/proj/wt")),
            vec![ResolvedPane {
                argv: stats_argv(Path::new("/usr/bin/rimz")),
                cwd: PathBuf::from("/proj/wt"),
            }]
        );
    }

    #[test]
    fn resolve_slot_picks_resolved_position_and_falls_back_out_of_range() {
        let daemon = DaemonConfig {
            pane: vec![pane("one", None), pane("two", None)],
        };

        assert_eq!(
            resolve_slot(
                &daemon,
                1,
                Path::new("/usr/bin/rimz"),
                Path::new("/proj/wt")
            ),
            ResolvedPane {
                argv: vec!["two".to_owned()],
                cwd: PathBuf::from("/proj/wt"),
            }
        );
        assert_eq!(
            resolve_slot(
                &daemon,
                5,
                Path::new("/usr/bin/rimz"),
                Path::new("/proj/wt")
            ),
            ResolvedPane {
                argv: stats_argv(Path::new("/usr/bin/rimz")),
                cwd: PathBuf::from("/proj/wt"),
            }
        );
    }

    #[test]
    fn needs_restart_compares_argv_and_cwd() {
        let current = ResolvedPane {
            argv: vec!["btop".to_owned()],
            cwd: PathBuf::from("/proj/wt"),
        };
        assert!(!needs_restart(&current, &current));
        assert!(needs_restart(
            &current,
            &ResolvedPane {
                argv: vec!["htop".to_owned()],
                cwd: current.cwd.clone(),
            }
        ));
        assert!(needs_restart(
            &current,
            &ResolvedPane {
                argv: current.argv.clone(),
                cwd: PathBuf::from("/tmp"),
            }
        ));
    }

    #[test]
    fn daemon_content_wrapper_is_not_a_managed_host_marker() {
        assert!(!crate::pane::command_is_host(
            "/usr/bin/rimz daemon content --slot 0 --worktree-root /proj/wt"
        ));
    }
}
