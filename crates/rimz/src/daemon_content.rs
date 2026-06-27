//! Daemon dashboard middle-column content. The mux births each content pane as
//! a small Rimz-owned supervisor, and the supervisor resolves the child command
//! from the current per-machine `[daemon]` config so edits take effect in place.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use serde::Deserialize;

use crate::config::{DaemonConfig, DaemonPane, MachineConfig};

pub const STATS_TOKEN: &str = "stats";

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPane {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

pub fn stats_argv(rimz_bin: &Path) -> Vec<String> {
    vec![
        rimz_bin.to_string_lossy().into_owned(),
        "stats".to_owned(),
        "--refresh".to_owned(),
        "--hold".to_owned(),
    ]
}

pub fn resolve_content(
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

pub fn resolve_slot(
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

pub fn resolve_pane(
    pane: &DaemonPane,
    rimz_bin: &Path,
    worktree_root: &Path,
) -> Option<ResolvedPane> {
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

pub fn run_supervisor(slot: usize, worktree_root: &Path) -> io::Result<ExitStatus> {
    let rimz_bin = std::env::current_exe()?;
    reset_signal_flags();
    install_signal_handlers()?;

    let config_path = MachineConfig::config_path();
    let daemon = load_daemon_config(&config_path).unwrap_or_default();
    let mut current = resolve_slot(&daemon, slot, &rimz_bin, worktree_root);
    let mut child = spawn_child(&current)?;
    let watch = watch_config(&config_path);
    let mut reload_due_at: Option<Instant> = None;

    loop {
        let now = Instant::now();
        if terminate_signal_received() {
            return terminate_child(&mut child);
        }

        if drain_config_changes(watch.as_ref()) {
            reload_due_at = Some(now + CONFIG_RELOAD_DEBOUNCE);
        }
        if reload_due_at.is_some_and(|due| now >= due) {
            reload_due_at = None;
            if let Some(daemon) = load_daemon_config(&config_path) {
                let next = resolve_slot(&daemon, slot, &rimz_bin, worktree_root);
                if needs_restart(&current, &next) {
                    match reload_child(&mut child, &next)? {
                        ReloadedChild::Next(next_child) => {
                            child = next_child;
                            current = next;
                        }
                        ReloadedChild::Current => {}
                    }
                }
            }
        }

        match try_wait_child(&mut child)? {
            Some(status) => return Ok(status),
            None => std::thread::sleep(CHILD_WAIT_POLL),
        }
    }
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
    Next(Child),
    Current,
}

fn reload_child(child: &mut Child, next: &ResolvedPane) -> io::Result<ReloadedChild> {
    match spawn_child(next) {
        Ok(mut next_child) => {
            if let Err(err) = terminate_child(child) {
                let _ = terminate_child(&mut next_child);
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
    _watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<()>,
}

fn watch_config(config_path: &Path) -> Option<ConfigWatch> {
    let Some(parent) = config_path.parent() else {
        tracing::debug!(
            path = %config_path.display(),
            "daemon content hot-reload disabled: config path has no parent",
        );
        return None;
    };
    let target = config_path.to_path_buf();
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && event.paths.iter().any(|path| path == &target)
            {
                let _ = event_tx.send(());
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
    terminate_signal_flag().store(false, Ordering::SeqCst);
    ignored_sigint_flag().store(false, Ordering::SeqCst);
}

fn terminate_signal_received() -> bool {
    terminate_signal_flag().load(Ordering::SeqCst)
}

fn terminate_signal_flag() -> &'static Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
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
        signal_hook::flag::register(signal, terminate_signal_flag().clone())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handlers() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) -> io::Result<ExitStatus> {
    if let Some(status) = try_wait_child(child)? {
        return Ok(status);
    }
    signal_child(child.id(), ChildSignal::Term);
    let deadline = Instant::now() + CHILD_SIGNAL_GRACE;
    loop {
        if let Some(status) = try_wait_child(child)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(CHILD_WAIT_POLL);
    }
    signal_child(child.id(), ChildSignal::Kill);
    wait_child(child)
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) -> io::Result<ExitStatus> {
    if let Some(status) = try_wait_child(child)? {
        return Ok(status);
    }
    child.kill()?;
    wait_child(child)
}

fn try_wait_child(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    loop {
        match child.try_wait() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn wait_child(child: &mut Child) -> io::Result<ExitStatus> {
    loop {
        match child.wait() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ChildSignal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_child(pid: u32, signal: ChildSignal) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let signal = match signal {
        ChildSignal::Term => Signal::SIGTERM,
        ChildSignal::Kill => Signal::SIGKILL,
    };
    let _ = kill(Pid::from_raw(pid as i32), signal);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!crate::remote_control::command_is_host(
            "/usr/bin/rimz daemon content --slot 0 --worktree-root /proj/wt"
        ));
    }
}
