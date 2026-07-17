//! Codex managed-standalone remote-control daemon lifecycle and stale recovery.
//!
//! Provider commands run from durable `CODEX_HOME`. Recovery signals only the
//! updater whose PID records, process start times, ownership, executable, argv,
//! and sole zombie child prove the known upstream stale-daemon shape.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::mux::CommandSpec;

use super::codex_home;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const PID_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const RECOVERY_POLL: Duration = Duration::from_millis(25);
const START_RETRY_DELAY: Duration = Duration::from_secs(3);

/// Official managed-standalone installer surfaced by readiness guidance.
const INSTALL_COMMAND: &str = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";
/// Human-facing recycle. The leading `cd` anchors the restarted daemon in a
/// durable directory: the detached app-server inherits the spawning shell's
/// working directory, so a recycle run from a worktree that is later removed
/// leaves the daemon resolving config against a deleted path, failing every
/// `thread/start` with "failed to load configuration". RimZ's own spawn passes
/// `CODEX_HOME` as cwd for the same reason.
const RECYCLE_COMMAND: &str =
    "cd ~; codex remote-control stop; sleep 3; codex remote-control start";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Disabled,
    Ready,
    Uninstalled(Issue),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue {
    StandaloneMissing,
}

/// A live managed updater still executes a different standalone release from
/// the current managed binary. Codex's update loop can restart the shared
/// app-server while this skew persists, disconnecting every attached client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdaterSkew {
    pub updater_pid: u32,
    pub updater_exe: PathBuf,
    pub managed_exe: PathBuf,
    pub updater_exe_deleted: bool,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StandaloneMissing => write!(
                f,
                "Codex remote-control is enabled (`[remote_control] codex = true`) but the \
                 managed standalone Codex install is missing, so `rimz start` brings the \
                 room up without the Codex remote-control host.\n\
                 `codex remote-control start` boots its app-server daemon from \
                 `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME defaults to \
                 `~/.codex`); a `codex` on PATH is a different binary and does not satisfy it.\n\n\
                 Install it with:\n    {INSTALL_COMMAND}\n\n\
                 then re-run to enable the host, or set `[remote_control] codex = false` to \
                 silence this."
            ),
        }
    }
}

impl std::error::Error for Issue {}

impl std::fmt::Display for UpdaterSkew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let deleted = if self.updater_exe_deleted {
            " (deleted)"
        } else {
            ""
        };
        write!(
            f,
            "Codex remote-control updater version skew:\n\
                 updater pid: {}\n\
                 updater exe: {}{}\n\
                 managed exe: {}\n\
             The next hourly update tick can restart the shared app-server and disconnect every \
             daemon-backed Codex session.\n\n\
             Schedule one deliberate recycle while no valuable Codex turns are running:\n    \
             {RECYCLE_COMMAND}\n\
             (a start immediately after stop races the daemon teardown and fails with \
             \"connection is errored\"). This disconnects daemon-backed Codex sessions once; \
             resume them afterwards.",
            self.updater_pid,
            self.updater_exe.display(),
            deleted,
            self.managed_exe.display(),
        )
    }
}

pub fn readiness(enabled: bool) -> Readiness {
    if !enabled {
        Readiness::Disabled
    } else if standalone_bin().is_some() {
        Readiness::Ready
    } else {
        Readiness::Uninstalled(Issue::StandaloneMissing)
    }
}

/// Report a fully identified live updater whose executable differs from the
/// managed standalone target while the daemon control socket is present.
///
/// This is diagnostic only. A healthy but skewed daemon keeps serving until
/// the user schedules a provider-owned recycle; RimZ does not replace it and
/// reproduce the session loss this warning exists to explain.
pub fn updater_skew() -> Option<UpdaterSkew> {
    let home = codex_home()?;
    updater_skew_under(&home)
}

fn updater_skew_under(home: &Path) -> Option<UpdaterSkew> {
    if !home
        .join("app-server-control")
        .join("app-server-control.sock")
        .exists()
    {
        return None;
    }
    let updater = read_pid_record(
        &home
            .join("app-server-daemon")
            .join("app-server-updater.pid"),
    )?;
    if !pid_record_matches(&updater) {
        return None;
    }
    let metrics = crate::proc::stat_metrics(updater.pid)?;
    if matches!(metrics.state, 'Z' | 'X')
        || crate::proc::real_uid(updater.pid)? != crate::proc::own_uid()?
    {
        return None;
    }
    let (updater_executable, deleted) = crate::proc::exe_path(updater.pid)?;
    if !managed_executable(home, &updater_executable) {
        return None;
    }
    let argv = crate::proc::argv(updater.pid)?;
    if !updater_argv(home, &argv) {
        return None;
    }
    let managed_executable = std::fs::canonicalize(standalone_bin_under(home)?).ok()?;
    classify_updater_skew(updater.pid, updater_executable, managed_executable, deleted)
}

fn classify_updater_skew(
    updater_pid: u32,
    updater_exe: PathBuf,
    managed_exe: PathBuf,
    updater_exe_deleted: bool,
) -> Option<UpdaterSkew> {
    let resolved_updater =
        std::fs::canonicalize(&updater_exe).unwrap_or_else(|_| updater_exe.clone());
    (updater_exe_deleted || resolved_updater != managed_exe).then_some(UpdaterSkew {
        updater_pid,
        updater_exe,
        managed_exe,
        updater_exe_deleted,
    })
}

/// Ensure the enabled per-user daemon once the managed standalone resolves.
pub fn ensure(enabled: bool) {
    let home = codex_home();
    let standalone = home.as_deref().and_then(standalone_bin_under);
    if !should_ensure(enabled, standalone.is_some()) {
        return;
    }
    let (Some(home), Some(bin)) = (home, standalone) else {
        return;
    };
    if recover_stale(&home) {
        tracing::warn!(
            "recovered a stale Codex daemon updater after its app-server became a zombie"
        );
    }
    spawn(&bin, &home);
}

/// Apply one synchronous start/stop transition, retrying once after a fully
/// verified stale-updater recovery or, for start, after a provider teardown
/// settle delay.
pub fn reconcile(enabled: bool) -> Result<(), ControlError> {
    let Some(home) = codex_home() else {
        return Ok(());
    };
    let Some(bin) = standalone_bin_under(&home) else {
        return Ok(());
    };
    let argv = command(&bin, enabled);
    let Err(first_error) = run_command(&argv, enabled, &home) else {
        return Ok(());
    };
    match failed_command_retry(enabled, recover_stale(&home)) {
        FailedCommandRetry::AfterStaleRecovery => {
            tracing::warn!(
                action = action(enabled),
                "recovered a stale Codex daemon updater after its app-server became a zombie",
            );
            run_command(&argv, enabled, &home)
        }
        FailedCommandRetry::AfterStartSettle => {
            tracing::warn!(
                action = action(enabled),
                error = %first_error,
                retry_delay_secs = START_RETRY_DELAY.as_secs(),
                "Codex remote-control start failed during daemon teardown; retrying after settle delay",
            );
            std::thread::sleep(START_RETRY_DELAY);
            run_command(&argv, enabled, &home)
        }
        FailedCommandRetry::None => Err(first_error),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailedCommandRetry {
    AfterStaleRecovery,
    AfterStartSettle,
    None,
}

fn failed_command_retry(enabled: bool, recovered_stale: bool) -> FailedCommandRetry {
    if recovered_stale {
        FailedCommandRetry::AfterStaleRecovery
    } else if enabled {
        FailedCommandRetry::AfterStartSettle
    } else {
        FailedCommandRetry::None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
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

fn run_command(argv: &[String], enabled: bool, home: &Path) -> Result<(), ControlError> {
    let Some(spec) = command_spec(argv, home) else {
        return Ok(());
    };
    let output = spec
        .output_raw_with_timeout(CONTROL_TIMEOUT)
        .map_err(|source| ControlError::Command {
            action: action(enabled),
            source,
        })?;
    if !output.status.success() {
        return Err(ControlError::Exit {
            action: action(enabled),
            program: PathBuf::from(&spec.program),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn action(enabled: bool) -> &'static str {
    if enabled { "start" } else { "stop" }
}

fn command(bin: &Path, enabled: bool) -> Vec<String> {
    vec![
        bin.to_string_lossy().into_owned(),
        "remote-control".to_owned(),
        action(enabled).to_owned(),
    ]
}

fn command_spec(argv: &[String], home: &Path) -> Option<CommandSpec> {
    let (program, args) = argv.split_first()?;
    Some(
        CommandSpec::new(program)
            .args(args.iter().cloned())
            .cwd(home),
    )
}

fn spawn(bin: &Path, home: &Path) {
    let argv = command(bin, true);
    let Some(spec) = command_spec(&argv, home) else {
        return;
    };
    let mut cmd = spec.to_command();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-daemon") {
        tracing::warn!(error = %err, "failed to spawn the codex app-server daemon");
    }
}

/// Managed standalone at `$CODEX_HOME/packages/standalone/current/codex`.
fn standalone_bin() -> Option<PathBuf> {
    standalone_bin_under(&codex_home()?)
}

fn standalone_bin_under(home: &Path) -> Option<PathBuf> {
    let bin = home
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    bin.is_file().then_some(bin)
}

fn should_ensure(enabled: bool, standalone_present: bool) -> bool {
    enabled && standalone_present
}

/// PID plus `ps -o lstart` identity recorded by Codex.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PidRecord {
    pid: u32,
    process_start_time: String,
}

struct ProcessSnapshot {
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

fn recover_stale(home: &Path) -> bool {
    if home
        .join("app-server-control")
        .join("app-server-control.sock")
        .exists()
    {
        return false;
    }
    let state_dir = home.join("app-server-daemon");
    let Some(app) = read_pid_record(&state_dir.join("app-server.pid")) else {
        return false;
    };
    let Some(updater) = read_pid_record(&state_dir.join("app-server-updater.pid")) else {
        return false;
    };
    let Some(snapshot) = process_snapshot(&app, &updater) else {
        return false;
    };
    let Some(updater_pid) = stale_updater_pid(home, &app, &updater, &snapshot) else {
        return false;
    };
    if !terminate_updater(updater_pid) {
        return false;
    }

    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        if !pid_record_matches(&app) && !pid_record_matches(&updater) {
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
        std::thread::sleep(RECOVERY_POLL);
    }
}

fn read_pid_record(path: &Path) -> Option<PidRecord> {
    let bytes = std::fs::read(path).ok()?;
    let record = serde_json::from_slice::<PidRecord>(&bytes).ok()?;
    (record.pid > 0 && !record.process_start_time.trim().is_empty()).then_some(record)
}

fn process_snapshot(app: &PidRecord, updater: &PidRecord) -> Option<ProcessSnapshot> {
    let app_metrics = crate::proc::stat_metrics(app.pid)?;
    if app_metrics.state != 'Z' {
        return None;
    }
    let (_, app_parent) = crate::proc::comm_and_ppid(app.pid)?;
    let updater_metrics = crate::proc::stat_metrics(updater.pid)?;
    Some(ProcessSnapshot {
        app_state: app_metrics.state,
        app_parent,
        app_uid: crate::proc::real_uid(app.pid)?,
        app_identity_matches: pid_record_matches(app),
        updater_state: updater_metrics.state,
        updater_uid: crate::proc::real_uid(updater.pid)?,
        updater_identity_matches: pid_record_matches(updater),
        updater_exe: crate::proc::exe_path(updater.pid)?.0,
        updater_argv: crate::proc::argv(updater.pid)?,
        updater_children: crate::proc::children(updater.pid),
    })
}

fn stale_updater_pid(
    home: &Path,
    app: &PidRecord,
    updater: &PidRecord,
    snapshot: &ProcessSnapshot,
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
        && managed_executable(home, &snapshot.updater_exe)
        && updater_argv(home, &snapshot.updater_argv)
        && snapshot.updater_children.as_slice() == expected_children)
        .then_some(updater.pid)
}

fn managed_executable(home: &Path, executable: &Path) -> bool {
    executable.starts_with(home.join("packages").join("standalone"))
        && executable.file_name() == Some(OsStr::new("codex"))
}

fn updater_argv(home: &Path, argv: &[OsString]) -> bool {
    let [program, app_server, daemon, update_loop] = argv else {
        return false;
    };
    managed_executable(home, Path::new(program))
        && app_server == "app-server"
        && daemon == "daemon"
        && update_loop == "pid-update-loop"
}

fn pid_record_matches(record: &PidRecord) -> bool {
    let pid = record.pid.to_string();
    let output = CommandSpec::new("ps")
        .args(["-p", &pid, "-o", "lstart="])
        .output_raw_with_timeout(PID_PROBE_TIMEOUT);
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == record.process_start_time
    })
}

#[cfg(unix)]
fn terminate_updater(pid: u32) -> bool {
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
fn terminate_updater(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests;
