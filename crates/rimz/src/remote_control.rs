//! Remote-control hosts, daemon-view assembly and repair, and dashboard-pane
//! classification.
//!
//! When a [`crate::config::RemoteControlConfig`] toggle is set and that agent
//! can start, `rimz start` brings its host up — but the two have different
//! lifecycles, so they launch differently:
//!
//! - **Claude** runs `claude remote-control --spawn worktree`, a long-lived
//!   foreground host, in the workspace session's one named [`VIEW_NAME`]
//!   background view (a tmux window / Zellij tab). It runs from the project root
//!   so `--spawn=worktree` carves new on-demand sessions off the canonical repo,
//!   not the current worktree. It is a pane but not a coding agent — no Rimz
//!   hooks, never stamps a pane — so the sidebar must not render it as an idle
//!   agent: [`pane_is_host`] identifies the host pane and the snapshot reducer
//!   filters it out, surfacing remote control as a health-colored `⇅ rc` flag
//!   on the Claude provider dashboard block instead.
//! - **Codex** runs `remote-control start` from the *managed standalone install*
//!   ([`codex_standalone_bin`]), which brings up the Codex app-server daemon
//!   with remote control enabled and returns. That daemon is a **per-user
//!   singleton** (one control socket), so it is *not* a per-workspace pane:
//!   [`ensure_codex_daemon`] spawns the (idempotent) start command detached with
//!   null stdio, and Codex enrichment reaches the daemon over the control socket
//!   (see [`crate::agents::codex::app_server`]). A missing control socket plus
//!   Codex PID records that prove the app-server is a zombie child of its
//!   managed updater triggers one bounded updater recycle before startup.
//! - **Loops** run an always-present `rimz loop watch` panel in the runtime
//!   column. Scheduled loop runs split against that pane so transient agents
//!   stay in the daemon view's loop zone.
//!
//! `remote-control start` boots and updates its daemon from the standalone's
//! fixed path, so a `codex` merely on PATH (a different binary) is not enough.
//! When the `codex` toggle is on but that install is absent, [`preflight`]
//! skips that inert host so the room still starts, and `rimz doctor` surfaces
//! the install fix. Claude has version- and settings-gated preconditions: old
//! binaries lack remote control, `disableRemoteControl` blocks the surface,
//! and incompatible authentication or API endpoints disable remote control on
//! affected releases. Those installed-but-blocked cases stay fail-fast at
//! `rimz start`.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::agents::claude::remote_control as claude_rc;
use crate::agents::codex::app_server::codex_home;
use crate::agents::version::CliVersion;
use crate::config::{DaemonConfig, RemoteControlConfig};
use crate::ids::WorkspaceId;
use crate::mux::{
    CommandSpec, DaemonView, HostPane, MuxBackend, PaneListOptions, SplitDirection,
    SplitPaneOptions,
};
use crate::pane::PaneRef;
use crate::store::{paths::StatePaths, workspace_record};

/// View name for the managed daemon tab. Shared by the launcher (the idempotency
/// key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. The tab hosts configurable
/// content in the middle (live stats by default) and stacks the per-session
/// Codex app-server broker, the Claude remote-control host, and the loop panel
/// on the right when they apply.
pub const VIEW_NAME: &str = "rimzd";

const CODEX_DAEMON_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_DAEMON_PID_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_DAEMON_RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_DAEMON_RECOVERY_POLL: Duration = Duration::from_millis(25);

/// Inputs that determine the managed panes in one workspace's daemon view.
pub struct DaemonViewSpecParams<'a> {
    pub remote_control: &'a RemoteControlConfig,
    pub daemon: &'a DaemonConfig,
    pub rimz_bin: &'a Path,
    pub workspace_id: &'a WorkspaceId,
    pub session_name: &'a str,
    pub project_root: &'a Path,
    pub worktree_root: &'a Path,
    pub claude_present: bool,
    pub codex_present: bool,
}

/// Build the authoritative managed-pane specification for the `rimzd` view.
pub fn daemon_view_spec(params: DaemonViewSpecParams<'_>) -> DaemonView {
    DaemonView {
        name: VIEW_NAME.to_owned(),
        content: content_panes(params.daemon, params.rimz_bin, params.worktree_root),
        hosts: daemon_hosts(&params),
        loop_panel: loop_panel(params.rimz_bin, params.worktree_root),
    }
}

/// Recreate every missing managed pane while any pane in `rimzd` survives.
/// Closing the whole view leaves no anchor and is treated as deliberate.
pub fn repair_daemon_view(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    view: &DaemonView,
) {
    let listing = match backend.list_panes(PaneListOptions {
        session_name: Some(session_name.to_owned()),
        workspace_id: Some(workspace_id.clone()),
        command_timeout: Some(std::time::Duration::from_millis(500)),
        authoritative: true,
        ..Default::default()
    }) {
        Ok(listing) => listing,
        Err(err) => {
            tracing::debug!(
                session = %session_name,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; pane listing failed",
            );
            return;
        }
    };
    let Some(anchor) = find_daemon_view_anchor(&listing.panes) else {
        tracing::debug!(
            session = %session_name,
            "daemon view repair skipped; no surviving daemon pane found",
        );
        return;
    };
    let missing = missing_managed_panes(view, &listing.panes);
    for pane in missing {
        if let Err(err) = backend.split_pane(SplitPaneOptions {
            session_name: Some(session_name.to_owned()),
            target_view_id: anchor.view_id.clone(),
            target_pane_id: Some(anchor.pane_id.clone()),
            cwd: Some(pane.cwd.to_string_lossy().into_owned()),
            command: Some(pane.argv.clone()),
            env: Default::default(),
            stacked: false,
            direction: SplitDirection::Down,
            focus: false,
        }) {
            tracing::warn!(
                session = %session_name,
                view = VIEW_NAME,
                argv = ?pane.argv,
                error = &err as &dyn std::error::Error,
                "daemon view repair could not recreate managed pane",
            );
        }
    }
    for pane_id in disabled_claude_host_panes(view, &listing.panes) {
        if let Err(err) = backend.close_pane(session_name, &pane_id) {
            tracing::warn!(
                session = %session_name,
                view = VIEW_NAME,
                pane = %pane_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair could not stop disabled Claude remote control",
            );
        }
    }
}

/// Best-effort elder duty that reconstructs the daemon-view spec from durable
/// workspace metadata and current machine configuration, then repairs it.
pub fn ensure_daemon_view(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
) {
    let paths = match StatePaths::for_workspace(workspace_id.clone()) {
        Ok(paths) => paths,
        Err(err) => {
            tracing::debug!(
                workspace = %workspace_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; state paths unavailable",
            );
            return;
        }
    };
    let record = match workspace_record::read(&paths.workspace_record) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                workspace = %workspace_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; workspace record unavailable",
            );
            return;
        }
    };
    let machine = crate::config::MachineConfig::load_lenient();
    ensure_daemon_view_with_config(
        backend,
        workspace_id,
        session_name,
        &record,
        machine.as_ref(),
    );
}

fn ensure_daemon_view_with_config(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &workspace_record::WorkspaceRecord,
    machine: &crate::config::MachineConfig,
) {
    let rimz_bin = crate::proc::rimz_exe();
    let worktree_root = record
        .worktree_root
        .as_deref()
        .unwrap_or(&record.project_root);
    let mut remote_control = machine.remote_control.clone();
    if let Err(err) = preflight_claude(&remote_control) {
        tracing::debug!(
            workspace = %workspace_id,
            error = &err as &dyn std::error::Error,
            "Claude remote-control runtime toggle refused",
        );
        remote_control.claude = false;
    }
    let view = daemon_view_spec(DaemonViewSpecParams {
        remote_control: &remote_control,
        daemon: &machine.daemon,
        rimz_bin: &rimz_bin,
        workspace_id,
        session_name,
        project_root: &record.project_root,
        worktree_root,
        claude_present: which::which("claude").is_ok(),
        codex_present: which::which("codex").is_ok(),
    });
    repair_daemon_view(backend, session_name, workspace_id, &view);
}

/// A provider whose per-machine remote-control toggle changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteControlHost {
    Claude,
    Codex,
}

/// Apply one `rimz config set remote_control.<provider> …` change to the live
/// machine. Claude panes converge in every running room that retains its daemon
/// view, Codex starts or stops its per-user daemon, and every known workspace
/// receives a sidebar wakeup so the provider-dashboard flag follows the
/// persisted config without a restart.
pub fn apply_runtime_toggle(
    host: RemoteControlHost,
    machine: &crate::config::MachineConfig,
) -> Result<(), CodexDaemonControlError> {
    if host == RemoteControlHost::Codex {
        reconcile_codex_daemon(machine.remote_control.codex)?;
    }

    let workspaces = match crate::workspace::known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(error = %err, "remote-control toggle could not enumerate workspaces");
            return Ok(());
        }
    };

    if host == RemoteControlHost::Claude {
        let live_zellij = live_sessions(crate::ids::MuxName::Zellij);
        let live_tmux = live_sessions(crate::ids::MuxName::Tmux);
        for workspace in &workspaces {
            let mux = if live_zellij.contains(&workspace.session_name) {
                Some(crate::ids::MuxName::Zellij)
            } else if live_tmux.contains(&workspace.session_name) {
                Some(crate::ids::MuxName::Tmux)
            } else {
                None
            };
            let Some(mux) = mux else {
                continue;
            };
            let paths = match StatePaths::for_workspace(workspace.workspace_id.clone()) {
                Ok(paths) => paths,
                Err(err) => {
                    tracing::debug!(
                        workspace = %workspace.workspace_id,
                        error = &err as &dyn std::error::Error,
                        "remote-control toggle skipped a workspace with unavailable state paths",
                    );
                    continue;
                }
            };
            let record = match workspace_record::read(&paths.workspace_record) {
                Ok(record) => record,
                Err(err) => {
                    tracing::debug!(
                        workspace = %workspace.workspace_id,
                        error = &err as &dyn std::error::Error,
                        "remote-control toggle skipped a workspace with unavailable metadata",
                    );
                    continue;
                }
            };
            let backend = crate::mux::backend_for(mux);
            ensure_daemon_view_with_config(
                backend.as_ref(),
                &workspace.workspace_id,
                &workspace.session_name,
                &record,
                machine,
            );
        }
    }

    for workspace in workspaces {
        let Ok(runtime) = crate::store::RuntimePaths::for_workspace(workspace.workspace_id) else {
            continue;
        };
        if let Err(err) = crate::store::wakeup::wake_sidebars(&runtime) {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "remote-control toggle could not wake sidebars",
            );
        }
    }
    Ok(())
}

fn live_sessions(mux: crate::ids::MuxName) -> HashSet<String> {
    crate::mux::backend_for(mux)
        .list_sessions()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn daemon_hosts(params: &DaemonViewSpecParams<'_>) -> Vec<HostPane> {
    let mut hosts = Vec::new();
    if params.codex_present {
        hosts.push(HostPane {
            argv: vec![
                params.rimz_bin.to_string_lossy().into_owned(),
                "codex".to_owned(),
                "app-server".to_owned(),
                "serve".to_owned(),
                "--workspace-id".to_owned(),
                params.workspace_id.as_str().to_owned(),
                "--session-name".to_owned(),
                params.session_name.to_owned(),
            ],
            cwd: params.worktree_root.to_path_buf(),
        });
    }
    if params.remote_control.claude && params.claude_present {
        hosts.push(HostPane {
            argv: claude_host_argv(),
            cwd: params.project_root.to_path_buf(),
        });
    }
    hosts
}

fn loop_panel(rimz_bin: &Path, worktree_root: &Path) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "loop".to_owned(),
            "watch".to_owned(),
            "--hold".to_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
}

fn content_panes(daemon: &DaemonConfig, rimz_bin: &Path, worktree_root: &Path) -> Vec<HostPane> {
    (0..crate::daemon_content::resolve_content(daemon, rimz_bin, worktree_root).len())
        .map(|slot| content_supervisor_pane(slot, rimz_bin, worktree_root))
        .collect()
}

fn content_supervisor_pane(slot: usize, rimz_bin: &Path, worktree_root: &Path) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "daemon".to_owned(),
            "content".to_owned(),
            "--slot".to_owned(),
            slot.to_string(),
            "--worktree-root".to_owned(),
            worktree_root.to_string_lossy().into_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
}

/// Substring marking the Claude remote-control host in a pane's command line —
/// the subcommand it spells (`claude remote-control …`).
pub(crate) const COMMAND_MARKER: &str = "remote-control";

/// Substring marking the Codex app-server broker in a pane's command line
/// (`rimz codex app-server serve …`). The broker is a per-session host pane in
/// the same view, distinct from the per-user daemon [`ensure_codex_daemon`] runs.
pub(crate) const APP_SERVER_MARKER: &str = "app-server";

/// Substring marking the always-present loop panel command
/// (`rimz loop watch --hold`).
pub(crate) const LOOP_PANEL_MARKER: &str = "loop watch";

/// The Claude Remote Control argv (program first). `--spawn worktree` isolates
/// each on-demand remote session in its own git worktree — the worktree mode.
pub fn claude_command() -> Vec<String> {
    vec![
        "claude".to_owned(),
        "remote-control".to_owned(),
        "--spawn".to_owned(),
        "worktree".to_owned(),
    ]
}

/// The daemon-host Claude argv. Server mode is independent of agent view, so
/// it uses the same direct command documented by Claude Code.
pub fn claude_host_argv() -> Vec<String> {
    claude_command()
}

/// The Codex remote-control argv (program first), invoked through `bin` — the
/// managed standalone install from [`codex_standalone_bin`]. `start` brings up
/// the app-server daemon with remote control enabled, then returns. Invoking the
/// standalone path directly means the launch never depends on a `codex` being on
/// PATH, and runs exactly the binary the daemon updates from.
pub fn codex_command(bin: &Path) -> Vec<String> {
    vec![
        bin.to_string_lossy().into_owned(),
        "remote-control".to_owned(),
        "start".to_owned(),
    ]
}

/// The Codex daemon shutdown argv (program first). An explicit
/// `remote_control.codex = false` transition uses the same managed standalone
/// install as startup so the per-user remote bridge turns off immediately.
pub fn codex_stop_command(bin: &Path) -> Vec<String> {
    vec![
        bin.to_string_lossy().into_owned(),
        "remote-control".to_owned(),
        "stop".to_owned(),
    ]
}

/// Ensure the per-user Codex app-server daemon is running when `[remote_control]
/// codex` is on and the managed standalone install resolves. The daemon is a
/// per-user singleton (one control socket), so it is ensured once here rather
/// than parked in a per-workspace pane; enrichment reaches it over the socket.
/// Best-effort, gated by [`should_ensure_codex_daemon`].
pub fn ensure_codex_daemon(config: &RemoteControlConfig) {
    let home = codex_home();
    let standalone = home.as_deref().and_then(standalone_bin_under);
    if !should_ensure_codex_daemon(config.codex, standalone.is_some()) {
        return;
    }
    let (Some(home), Some(bin)) = (home, standalone) else {
        return;
    };
    if recover_stale_codex_daemon(&home) {
        tracing::warn!(
            "recovered a stale Codex daemon updater after its app-server became a zombie",
        );
    }
    spawn_codex_daemon(&bin);
}

/// A synchronous Codex remote-control transition requested by `rimz config
/// set`. The standalone CLI's `start` and `stop` commands return after the
/// per-user daemon reaches the requested state, which keeps consecutive on/off
/// toggles ordered. An absent managed standalone install preserves the room
/// start contract: the enabled host is skipped and `rimz doctor` carries the
/// install fix.
pub fn reconcile_codex_daemon(enabled: bool) -> Result<(), CodexDaemonControlError> {
    let Some(home) = codex_home() else {
        return Ok(());
    };
    let Some(bin) = standalone_bin_under(&home) else {
        return Ok(());
    };
    let argv = if enabled {
        codex_command(&bin)
    } else {
        codex_stop_command(&bin)
    };
    let first = run_codex_daemon_command(&argv, enabled);
    if first.is_err() && recover_stale_codex_daemon(&home) {
        tracing::warn!(
            action = codex_daemon_action(enabled),
            "recovered a stale Codex daemon updater after its app-server became a zombie",
        );
        return run_codex_daemon_command(&argv, enabled);
    }
    first
}

fn run_codex_daemon_command(argv: &[String], enabled: bool) -> Result<(), CodexDaemonControlError> {
    let Some((program, args)) = argv.split_first() else {
        return Ok(());
    };
    let output = CommandSpec::new(program)
        .args(args.iter().cloned())
        .output_raw_with_timeout(CODEX_DAEMON_CONTROL_TIMEOUT)
        .map_err(|source| CodexDaemonControlError::Command {
            action: codex_daemon_action(enabled),
            source,
        })?;
    if !output.status.success() {
        return Err(CodexDaemonControlError::Exit {
            action: codex_daemon_action(enabled),
            program: PathBuf::from(program),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn codex_daemon_action(enabled: bool) -> &'static str {
    if enabled { "start" } else { "stop" }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexDaemonControlError {
    #[error("could not {action} Codex remote control: {source}")]
    Command {
        action: &'static str,
        #[source]
        source: crate::mux::MuxErr,
    },
    #[error(
        "Codex remote-control {action} failed with {status} using {}: {stderr}",
        program.display()
    )]
    Exit {
        action: &'static str,
        program: PathBuf,
        status: std::process::ExitStatus,
        stderr: String,
    },
}

/// Codex's daemon records process identity as a PID plus `ps -o lstart`. Keep
/// the upstream shape typed: a PID alone is never authority to signal a
/// process because the kernel may already have reused it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexDaemonPidRecord {
    pid: u32,
    process_start_time: String,
}

/// The exact process evidence required to repair Codex's stale-daemon state.
/// A current upstream bug treats a zombie app-server as live because
/// `kill(pid, 0)` succeeds, so `remote-control start` waits for a socket the
/// dead child cannot create and `stop` waits for signals a zombie cannot
/// receive. Terminating the verified updater parent lets init reap that child;
/// the next provider command then discards both stale PID records itself.
struct CodexDaemonProcessSnapshot {
    app_state: char,
    app_parent: u32,
    app_uid: u32,
    app_identity_matches: bool,
    updater_state: char,
    updater_uid: u32,
    updater_identity_matches: bool,
    updater_exe: PathBuf,
    updater_argv: Vec<OsString>,
    updater_children: Vec<u32>,
}

/// Recover only the provider state whose full process tree proves the known
/// zombie failure. Every unreadable or drifting input abstains, preserving the
/// original Codex error. A successful signal is followed by bounded identity
/// polling before the caller retries the provider command once.
fn recover_stale_codex_daemon(codex_home: &Path) -> bool {
    if codex_home
        .join("app-server-control")
        .join("app-server-control.sock")
        .exists()
    {
        return false;
    }
    let state_dir = codex_home.join("app-server-daemon");
    let Some(app) = read_codex_daemon_pid_record(&state_dir.join("app-server.pid")) else {
        return false;
    };
    let Some(updater) = read_codex_daemon_pid_record(&state_dir.join("app-server-updater.pid"))
    else {
        return false;
    };
    let Some(snapshot) = codex_daemon_process_snapshot(&app, &updater) else {
        return false;
    };
    let Some(updater_pid) = stale_codex_updater_pid(codex_home, &app, &updater, &snapshot) else {
        return false;
    };
    if !terminate_codex_updater(updater_pid) {
        return false;
    }

    let deadline = Instant::now() + CODEX_DAEMON_RECOVERY_TIMEOUT;
    loop {
        if !codex_pid_record_matches(&app) && !codex_pid_record_matches(&updater) {
            return true;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                app_pid = app.pid,
                updater_pid,
                "Codex stale-daemon recovery timed out waiting for provider processes to exit",
            );
            return false;
        }
        std::thread::sleep(CODEX_DAEMON_RECOVERY_POLL);
    }
}

fn read_codex_daemon_pid_record(path: &Path) -> Option<CodexDaemonPidRecord> {
    let bytes = std::fs::read(path).ok()?;
    let record = serde_json::from_slice::<CodexDaemonPidRecord>(&bytes).ok()?;
    (record.pid > 0 && !record.process_start_time.trim().is_empty()).then_some(record)
}

fn codex_daemon_process_snapshot(
    app: &CodexDaemonPidRecord,
    updater: &CodexDaemonPidRecord,
) -> Option<CodexDaemonProcessSnapshot> {
    let app_metrics = crate::proc::stat_metrics(app.pid)?;
    if app_metrics.state != 'Z' {
        return None;
    }
    let (_, app_parent) = crate::proc::comm_and_ppid(app.pid)?;
    let updater_metrics = crate::proc::stat_metrics(updater.pid)?;
    Some(CodexDaemonProcessSnapshot {
        app_state: app_metrics.state,
        app_parent,
        app_uid: crate::proc::real_uid(app.pid)?,
        app_identity_matches: codex_pid_record_matches(app),
        updater_state: updater_metrics.state,
        updater_uid: crate::proc::real_uid(updater.pid)?,
        updater_identity_matches: codex_pid_record_matches(updater),
        updater_exe: crate::proc::exe_path(updater.pid)?.0,
        updater_argv: crate::proc::argv(updater.pid)?,
        updater_children: crate::proc::children(updater.pid),
    })
}

fn stale_codex_updater_pid(
    codex_home: &Path,
    app: &CodexDaemonPidRecord,
    updater: &CodexDaemonPidRecord,
    snapshot: &CodexDaemonProcessSnapshot,
) -> Option<u32> {
    let own_uid = crate::proc::own_uid()?;
    let expected_children = [app.pid];
    (app.pid != updater.pid
        && snapshot.app_state == 'Z'
        && snapshot.app_parent == updater.pid
        && snapshot.app_uid == own_uid
        && snapshot.app_identity_matches
        && !matches!(snapshot.updater_state, 'Z' | 'X')
        && snapshot.updater_uid == own_uid
        && snapshot.updater_identity_matches
        && managed_codex_executable(codex_home, &snapshot.updater_exe)
        && codex_updater_argv(codex_home, &snapshot.updater_argv)
        && snapshot.updater_children.as_slice() == expected_children)
        .then_some(updater.pid)
}

fn managed_codex_executable(codex_home: &Path, executable: &Path) -> bool {
    executable.starts_with(codex_home.join("packages").join("standalone"))
        && executable.file_name() == Some(OsStr::new("codex"))
}

fn codex_updater_argv(codex_home: &Path, argv: &[OsString]) -> bool {
    let [program, app_server, daemon, update_loop] = argv else {
        return false;
    };
    managed_codex_executable(codex_home, Path::new(program))
        && app_server == "app-server"
        && daemon == "daemon"
        && update_loop == "pid-update-loop"
}

fn codex_pid_record_matches(record: &CodexDaemonPidRecord) -> bool {
    let pid = record.pid.to_string();
    let output = CommandSpec::new("ps")
        .args(["-p", &pid, "-o", "lstart="])
        .output_raw_with_timeout(CODEX_DAEMON_PID_PROBE_TIMEOUT);
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == record.process_start_time
    })
}

#[cfg(unix)]
fn terminate_codex_updater(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => true,
        Err(err) => {
            tracing::warn!(pid, error = %err, "failed to terminate stale Codex daemon updater");
            false
        }
    }
}

#[cfg(not(unix))]
fn terminate_codex_updater(_pid: u32) -> bool {
    false
}

/// The pure ensure-daemon decision, split from [`ensure_codex_daemon`] so the
/// matrix is unit-testable without touching the filesystem: ensure iff the
/// toggle is on *and* the managed standalone install is present (a `codex` on
/// PATH does not satisfy `remote-control start` — see [`codex_standalone_bin`]).
fn should_ensure_codex_daemon(codex_enabled: bool, standalone_present: bool) -> bool {
    codex_enabled && standalone_present
}

/// Spawn `codex remote-control start` from the managed standalone `bin` detached,
/// with all stdio nulled, and hand it to the shared reaper. The command is
/// idempotent — it no-ops once the per-user daemon is up — and returns as soon
/// as the daemon is running, so this adds no latency and prints nothing to the
/// terminal. Best-effort: a spawn failure is logged and ignored, because the
/// app-server is enrichment, not correctness — the enrichment client cold-spawns
/// a server when the daemon is absent.
fn spawn_codex_daemon(bin: &Path) {
    let argv = codex_command(bin);
    let mut parts = argv.iter();
    let Some(program) = parts.next() else {
        return;
    };
    let mut cmd = Command::new(program);
    cmd.args(parts)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-daemon") {
        tracing::warn!(error = %err, "failed to spawn the codex app-server daemon");
    }
}

/// The managed standalone Codex install `codex remote-control start` boots its
/// daemon from: `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME
/// defaults to `~/.codex`). Returns the path only when it exists, so callers can
/// gate on a host that can actually start. A `codex` on PATH is a different
/// binary and does not satisfy this — see [`preflight`].
pub fn codex_standalone_bin() -> Option<PathBuf> {
    standalone_bin_under(&codex_home()?)
}

/// [`codex_standalone_bin`] rooted at an explicit Codex home — split out pure so
/// tests can point at a tempdir without touching `CODEX_HOME` or `HOME`.
fn standalone_bin_under(codex_home: &Path) -> Option<PathBuf> {
    let bin = codex_home
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    bin.is_file().then_some(bin)
}

/// The official one-liner that installs the managed standalone Codex. Surfaced
/// verbatim by [`PreflightError`] and `rimz doctor`, so the guidance never
/// drifts from one place to the other.
pub const CODEX_INSTALL_COMMAND: &str = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";

/// A configured remote-control host cannot start. [`preflight`] skips
/// uninstalled hosts so the room still launches, while installed agents with
/// fixable misconfigurations make `rimz start` refuse up front with the fix.
/// `rimz doctor` surfaces both categories.
#[derive(Debug, PartialEq, Eq)]
pub enum PreflightError {
    /// `[remote_control] codex = true` but the managed standalone install is
    /// absent. `rimz start` skips this host; the `Display` carries the
    /// user-facing install fix for `rimz doctor`.
    CodexStandaloneMissing,
    /// `[remote_control] claude = true` but the installed Claude Code version is
    /// older than remote-control support.
    ClaudeTooOld { found: CliVersion },
    /// Claude's own settings explicitly disable remote control.
    ClaudeRemoteControlDisabled { settings_path: PathBuf },
    /// Claude Code disables remote control when API-key auth is active on
    /// affected versions.
    ClaudeAuthConflict {
        sources: Vec<ClaudeAuthConflictSource>,
    },
}

impl PreflightError {
    /// Whether this refusal is an enabled host whose agent is not installed.
    /// `rimz start` skips these so the room still launches; `rimz doctor`
    /// reports them as advisories with the install fix.
    pub fn is_uninstalled_host(&self) -> bool {
        matches!(self, Self::CodexStandaloneMissing)
    }
}

/// A configured auth source that disables Claude remote control on affected
/// Claude Code versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeAuthConflictSource {
    ApiKeyEnv,
    AuthTokenEnv,
    ApiKeyHelperSetting,
    SettingsEnv,
    EndpointEnv,
    SettingsEndpoint,
}

impl std::fmt::Display for ClaudeAuthConflictSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKeyEnv => write!(f, "ANTHROPIC_API_KEY in the launch environment"),
            Self::AuthTokenEnv => write!(f, "ANTHROPIC_AUTH_TOKEN in the launch environment"),
            Self::ApiKeyHelperSetting => write!(f, "apiKeyHelper in Claude settings"),
            Self::SettingsEnv => write!(
                f,
                "ANTHROPIC_API_KEY/ANTHROPIC_AUTH_TOKEN in Claude settings env"
            ),
            Self::EndpointEnv => write!(
                f,
                "a custom Anthropic endpoint or third-party provider in the launch environment"
            ),
            Self::SettingsEndpoint => write!(
                f,
                "a custom Anthropic endpoint or third-party provider in Claude settings env"
            ),
        }
    }
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodexStandaloneMissing => write!(
                f,
                "Codex remote-control is enabled (`[remote_control] codex = true`) but the \
                 managed standalone Codex install is missing, so `rimz start` brings the \
                 room up without the Codex remote-control host.\n\
                 `codex remote-control start` boots its app-server daemon from \
                 `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME defaults to \
                 `~/.codex`); a `codex` on PATH is a different binary and does not satisfy it.\n\n\
                 Install it with:\n    {CODEX_INSTALL_COMMAND}\n\n\
                 then re-run to enable the host, or set `[remote_control] codex = false` to \
                 silence this."
            ),
            Self::ClaudeTooOld { found } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `claude --version` reports {found}; remote control requires Claude Code \
                 >= {}.\n\n\
                 Upgrade Claude Code, then re-run, or set `[remote_control] claude = false` \
                 to disable the Claude host.",
                claude_rc::MIN_REMOTE_CONTROL,
            ),
            Self::ClaudeRemoteControlDisabled { settings_path } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `disableRemoteControl: true` in {} blocks it.\n\n\
                 Remove that setting or set it to false, then re-run, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                settings_path.display(),
            ),
            Self::ClaudeAuthConflict { sources } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 Claude Code disables remote control with the configured authentication \
                 or API endpoint on this version. Conflicting source(s): {}.\n\n\
                 Remove those auth sources and use a claude.ai login for remote control, \
                 then re-run, or set `[remote_control] claude = false` to disable the \
                 Claude host.",
                sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }
}

impl std::error::Error for PreflightError {}

/// Gate `rimz start` for configured remote-control hosts. An enabled host whose
/// agent is not installed is skipped so the room still launches; an installed
/// agent with a fixable misconfiguration refuses the start with the fix. Codex's
/// `remote-control start` requires the managed standalone install
/// ([`codex_standalone_bin`]); Claude's host is version- and settings-gated
/// when the `claude` binary is present. `rimz doctor` reports both hard
/// refusals and skipped hosts.
pub fn preflight(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    start_decision(preflight_codex(config), preflight_claude(config))
}

/// The pure start-gate decision over the two host preflights: abort on the first
/// fixable misconfiguration of an installed agent, and skip an enabled host
/// whose agent is not installed ([`PreflightError::is_uninstalled_host`]) so the
/// room still starts.
fn start_decision(
    codex: Result<(), PreflightError>,
    claude: Result<(), PreflightError>,
) -> Result<(), PreflightError> {
    for refusal in [codex, claude] {
        if let Err(err) = refusal
            && !err.is_uninstalled_host()
        {
            return Err(err);
        }
    }
    Ok(())
}

/// Check only the configured Codex remote-control daemon precondition.
/// `rimz doctor` uses this beside [`preflight_claude`] so it can report every
/// configured host failure instead of only the first one.
pub fn preflight_codex(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    preflight_decision(config.codex, codex_standalone_bin().is_some())
}

/// Check only the configured Claude remote-control host preconditions. `rimz
/// doctor` uses this to report Claude readiness beside Codex readiness while
/// `preflight` keeps the single fail-fast entry point for `rimz start`.
pub fn preflight_claude(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    if !config.claude {
        return Ok(());
    }
    if which::which("claude").is_err() {
        return Ok(());
    }
    let (settings_path, settings) = claude_rc::read_rc_settings();
    let version = (!settings.disable_remote_control)
        .then(claude_rc::probed_version)
        .flatten();
    claude_preflight_decision(
        config.claude,
        true,
        version,
        settings_path,
        settings,
        env_var_present("ANTHROPIC_API_KEY"),
        env_var_present("ANTHROPIC_AUTH_TOKEN"),
        claude_rc::launch_endpoint_conflict(),
    )
}

/// The pure preflight decision, split from [`preflight`] so the full matrix is
/// unit-testable without touching the filesystem.
fn preflight_decision(
    codex_enabled: bool,
    codex_standalone_present: bool,
) -> Result<(), PreflightError> {
    if codex_enabled && !codex_standalone_present {
        return Err(PreflightError::CodexStandaloneMissing);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn claude_preflight_decision(
    claude_enabled: bool,
    claude_present: bool,
    version: Option<CliVersion>,
    settings_path: PathBuf,
    settings: claude_rc::ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
    env_endpoint_conflict: bool,
) -> Result<(), PreflightError> {
    if !claude_enabled || !claude_present {
        return Ok(());
    }
    if settings.disable_remote_control {
        return Err(PreflightError::ClaudeRemoteControlDisabled { settings_path });
    }

    let Some(found) = version else {
        tracing::warn!(
            "Claude remote-control preflight could not determine `claude --version`; applying version-independent gates only"
        );
        return Ok(());
    };
    if found < claude_rc::MIN_REMOTE_CONTROL {
        return Err(PreflightError::ClaudeTooOld { found });
    }
    let mut sources = Vec::new();
    if env_api_key {
        sources.push(ClaudeAuthConflictSource::ApiKeyEnv);
    }
    if env_auth_token {
        sources.push(ClaudeAuthConflictSource::AuthTokenEnv);
    }
    if settings.api_key_helper {
        sources.push(ClaudeAuthConflictSource::ApiKeyHelperSetting);
    }
    if settings.env_auth_conflict {
        sources.push(ClaudeAuthConflictSource::SettingsEnv);
    }
    if found < claude_rc::AUTH_ENV_BLOCKS_RC_SINCE {
        sources.clear();
    }
    if found >= claude_rc::CUSTOM_ENDPOINT_BLOCKS_RC_SINCE {
        if env_endpoint_conflict {
            sources.push(ClaudeAuthConflictSource::EndpointEnv);
        }
        if settings.env_endpoint_conflict {
            sources.push(ClaudeAuthConflictSource::SettingsEndpoint);
        }
    }
    if !sources.is_empty() {
        return Err(PreflightError::ClaudeAuthConflict { sources });
    }

    Ok(())
}

fn env_var_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty())
}

/// Whether a command line is one of Rimz's managed daemon hosts.
pub fn command_is_host(command: &str) -> bool {
    command.contains(COMMAND_MARKER) || command.contains(APP_SERVER_MARKER)
}

pub fn command_is_loop_panel(command: &str) -> bool {
    command.contains(LOOP_PANEL_MARKER)
}

pub fn find_loop_panel(panes: &[PaneRef]) -> Option<&PaneRef> {
    panes.iter().find(|pane| {
        pane.view_name.as_deref() == Some(VIEW_NAME)
            && (pane
                .spawn_command
                .as_deref()
                .is_some_and(command_is_loop_panel)
                || pane.command.as_deref().is_some_and(command_is_loop_panel))
    })
}

pub fn find_daemon_view_anchor(panes: &[PaneRef]) -> Option<&PaneRef> {
    panes
        .iter()
        .find(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ManagedPaneMarker {
    ContentSlot(usize),
    CodexAppServer,
    ClaudeRemoteControl,
    LoopPanel,
}

/// Return managed daemon-view panes absent from the live pane listing.
///
/// The repair pass adds missing RimZ-owned panes. Extra user panes inside
/// `rimzd` are left alone, and geometry is repaired at the next full view birth.
pub fn missing_managed_panes(view: &DaemonView, panes: &[PaneRef]) -> Vec<HostPane> {
    view.content
        .iter()
        .chain(view.hosts.iter())
        .chain(std::iter::once(&view.loop_panel))
        .filter(|host| {
            host_marker(host)
                .as_ref()
                .is_some_and(|marker| !pane_listing_contains_marker(panes, marker))
        })
        .cloned()
        .collect()
}

/// Select Claude host panes that remain after the machine toggle turns off.
/// Only the named daemon view and the managed command marker qualify; a user's
/// Claude command in a working view stays outside this reconciliation.
fn disabled_claude_host_panes(view: &DaemonView, panes: &[PaneRef]) -> Vec<crate::ids::PaneId> {
    let claude_enabled = view
        .hosts
        .iter()
        .filter_map(host_marker)
        .any(|marker| marker == ManagedPaneMarker::ClaudeRemoteControl);
    if claude_enabled {
        return Vec::new();
    }
    panes
        .iter()
        .filter(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
        .filter(|pane| {
            [pane.spawn_command.as_deref(), pane.command.as_deref()]
                .into_iter()
                .flatten()
                .any(|command| {
                    command_matches_marker(command, &ManagedPaneMarker::ClaudeRemoteControl)
                })
        })
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn pane_listing_contains_marker(panes: &[PaneRef], marker: &ManagedPaneMarker) -> bool {
    panes
        .iter()
        .filter(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
        .flat_map(|pane| [pane.spawn_command.as_deref(), pane.command.as_deref()])
        .flatten()
        .any(|command| command_matches_marker(command, marker))
}

fn host_marker(host: &HostPane) -> Option<ManagedPaneMarker> {
    content_slot_from_args(&host.argv)
        .map(ManagedPaneMarker::ContentSlot)
        .or_else(|| {
            let command = host.argv.join(" ");
            if command.contains(APP_SERVER_MARKER) {
                Some(ManagedPaneMarker::CodexAppServer)
            } else if command_is_claude_host(&command) {
                Some(ManagedPaneMarker::ClaudeRemoteControl)
            } else if command.contains(LOOP_PANEL_MARKER) {
                Some(ManagedPaneMarker::LoopPanel)
            } else {
                None
            }
        })
}

fn command_matches_marker(command: &str, marker: &ManagedPaneMarker) -> bool {
    match marker {
        ManagedPaneMarker::ContentSlot(slot) => content_slot_from_command(command) == Some(*slot),
        ManagedPaneMarker::CodexAppServer => command.contains(APP_SERVER_MARKER),
        ManagedPaneMarker::ClaudeRemoteControl => command_is_claude_host(command),
        ManagedPaneMarker::LoopPanel => command.contains(LOOP_PANEL_MARKER),
    }
}

fn command_is_claude_host(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    while let Some(token) = tokens.next() {
        let is_claude = Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "claude");
        if is_claude {
            return tokens.next() == Some(COMMAND_MARKER);
        }
    }
    false
}

fn content_slot_from_args(args: &[String]) -> Option<usize> {
    if !args
        .windows(2)
        .any(|pair| pair[0] == "daemon" && pair[1] == "content")
    {
        return None;
    }
    args.windows(2).find_map(|pair| {
        (pair[0] == "--slot")
            .then(|| pair[1].parse().ok())
            .flatten()
    })
}

fn content_slot_from_command(command: &str) -> Option<usize> {
    let args = command
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    content_slot_from_args(&args)
}

/// Whether `pane` belongs to the daemon dashboard. Command markers catch daemon
/// hosts wherever they are reported; the `rimzd` view name catches the full
/// dashboard, including content panes on backends that report only a foreground
/// binary basename.
pub fn pane_is_host(pane: &PaneRef) -> bool {
    pane.spawn_command.as_deref().is_some_and(command_is_host)
        || pane.command.as_deref().is_some_and(command_is_host)
        || pane.view_name.as_deref() == Some(VIEW_NAME)
}

/// Whether the managed Claude remote-control host pane is present in `panes`.
pub fn claude_host_present(panes: &[PaneRef]) -> bool {
    pane_listing_contains_marker(panes, &ManagedPaneMarker::ClaudeRemoteControl)
}

#[cfg(test)]
mod tests;
