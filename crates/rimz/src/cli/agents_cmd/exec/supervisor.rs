//! Fold-free parked provider supervision and same-PID image handoff.

use super::*;
use rimz::harness::parent_watch::WatchdogSeed;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::sync::mpsc;

const PARK_VERSION: u32 = 1;
const CARD_EVIDENCE_POLL: Duration = Duration::from_millis(250);

#[derive(Serialize, Deserialize)]
struct ParkState {
    v: u32,
    request: rimz::harness::launch::ExecRequest,
    identity: Option<LaunchIdentity>,
    provider: SavedCommand,
    provider_pid: u32,
    spawned_at: jiff::Timestamp,
    relaunch_cap: u32,
    relaunch_wait_ms: u64,
    fresh_launch: bool,
    termios: Option<SavedTermios>,
    keep: bool,
    survives_parent: bool,
    awaiting_reopen: Option<u32>,
    watchdog: Option<WatchdogSeed>,
    entered_worktree: Option<PathBuf>,
    cwd: PathBuf,
    isolation: rimz::config::Isolation,
    project_root: PathBuf,
}

impl ParkState {
    #[cfg(unix)]
    fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading park state {}", path.display()))?;
        let state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding park state {}", path.display()))?;
        if state.v != PARK_VERSION {
            bail!(
                "park state {} has version {}; expected {PARK_VERSION}",
                path.display(),
                state.v
            );
        }
        if state.relaunch_cap > u32::from(u8::MAX) {
            bail!("park state {} has an invalid relaunch cap", path.display());
        }
        Ok(state)
    }
}

#[derive(Serialize, Deserialize)]
struct SavedCommand {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    unset: Vec<OsString>,
    cwd: Option<PathBuf>,
}

impl SavedCommand {
    fn capture(command: &Command) -> Self {
        let mut env = Vec::new();
        let mut unset = Vec::new();
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => env.push((key.to_owned(), value.to_owned())),
                None => unset.push(key.to_owned()),
            }
        }
        Self {
            program: command.get_program().to_owned(),
            args: command.get_args().map(OsString::from).collect(),
            env,
            unset,
            cwd: command.get_current_dir().map(Path::to_owned),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).envs(self.env.iter().cloned());
        for key in &self.unset {
            command.env_remove(key);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }
}

#[cfg(unix)]
#[derive(Serialize, Deserialize)]
struct SavedTermios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    c_line: u8,
    c_cc: Vec<u8>,
    c_ispeed: u64,
    c_ospeed: u64,
}

#[cfg(unix)]
impl SavedTermios {
    fn capture(terminal: &ProviderTerminal) -> Option<Self> {
        let raw: nix::libc::termios = terminal.saved.clone()?.into();
        Some(Self {
            c_iflag: raw.c_iflag as u64,
            c_oflag: raw.c_oflag as u64,
            c_cflag: raw.c_cflag as u64,
            c_lflag: raw.c_lflag as u64,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            c_line: raw.c_line,
            c_cc: raw.c_cc.to_vec(),
            c_ispeed: raw.c_ispeed as u64,
            c_ospeed: raw.c_ospeed as u64,
        })
    }

    fn terminal(&self) -> Result<ProviderTerminal> {
        let c_cc = self
            .c_cc
            .as_slice()
            .try_into()
            .context("invalid saved terminal control-character count")?;
        let raw = nix::libc::termios {
            c_iflag: self
                .c_iflag
                .try_into()
                .context("invalid saved terminal c_iflag")?,
            c_oflag: self
                .c_oflag
                .try_into()
                .context("invalid saved terminal c_oflag")?,
            c_cflag: self
                .c_cflag
                .try_into()
                .context("invalid saved terminal c_cflag")?,
            c_lflag: self
                .c_lflag
                .try_into()
                .context("invalid saved terminal c_lflag")?,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            c_line: self.c_line,
            c_cc,
            c_ispeed: self
                .c_ispeed
                .try_into()
                .context("invalid saved terminal c_ispeed")?,
            c_ospeed: self
                .c_ospeed
                .try_into()
                .context("invalid saved terminal c_ospeed")?,
        };
        Ok(ProviderTerminal {
            saved: Some(raw.into()),
        })
    }
}

#[cfg(unix)]
fn reexec_command(binary: &Path, original: &[OsString], path: &Path) -> Command {
    use std::os::unix::process::CommandExt as _;
    let mut command = Command::new(binary);
    if let Some(arg0) = original.first() {
        command.arg0(arg0);
    }
    command
        .args(original.iter().skip(1))
        .arg("--supervise")
        .arg(path);
    command
}

#[cfg(not(unix))]
#[derive(Serialize, Deserialize)]
struct SavedTermios;

#[cfg(not(unix))]
impl SavedTermios {
    fn capture(_terminal: &ProviderTerminal) -> Option<Self> {
        None
    }
    fn terminal(&self) -> Result<ProviderTerminal> {
        Ok(ProviderTerminal {})
    }
}

impl ParkState {
    fn capture(startup: ParkStartup<'_>, workspace: &rimz::ResolvedWorkspace) -> Self {
        Self {
            v: PARK_VERSION,
            request: startup.request.clone(),
            identity: startup.identity.cloned(),
            provider: SavedCommand::capture(startup.command),
            provider_pid: startup.provider_pid,
            spawned_at: startup.spawned_at,
            relaunch_cap: u32::from(startup.relaunch_cap),
            relaunch_wait_ms: u64::try_from(startup.relaunch_wait.as_millis()).unwrap_or(u64::MAX),
            fresh_launch: startup.fresh_launch,
            termios: SavedTermios::capture(startup.terminal),
            keep: startup.keep,
            survives_parent: startup.survives_parent,
            awaiting_reopen: startup.awaiting_reopen,
            watchdog: startup.watchdog,
            entered_worktree: startup.entered_worktree.map(Path::to_owned),
            cwd: startup.cwd.to_owned(),
            isolation: startup.isolation,
            project_root: workspace.project_root.clone(),
        }
    }

    fn into_exit(
        self,
        outcome: ExecOutcome,
        terminal_grace: Duration,
        startup_deaths: Option<u32>,
    ) -> ParkExit {
        ParkExit {
            request: self.request,
            identity: self.identity,
            cwd: self.cwd,
            isolation: self.isolation,
            entered_worktree: self.entered_worktree,
            keep: self.keep,
            outcome,
            terminal_grace,
            startup_deaths,
        }
    }
}

pub(super) fn park(
    startup: ParkStartup<'_>,
    path: &Path,
    child: Child,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<ParkExit> {
    let state = ParkState::capture(startup, workspace);
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, raise};
        use std::os::unix::process::CommandExt as _;
        let handoff = || -> Result<()> {
            rimz::disk::atomic::write_temp_then_rename_cache(path, &state)
                .with_context(|| format!("writing park state {}", path.display()))?;
            let _signal_mask = rimz::child_process::CleanupSignalMask::block()?;
            if cleanup_signal_received() {
                raise(Signal::SIGTERM)?;
            }
            if interrupt_signal_flag().load(Ordering::SeqCst) {
                raise(Signal::SIGINT)?;
            }
            let binary = rimz::reload::current_reexec_target().unwrap_or_else(rimz::proc::rimz_exe);
            let original = std::env::args_os().collect::<Vec<_>>();
            let error = reexec_command(&binary, &original, path)
                .current_dir(&state.cwd)
                .exec();
            Err(error).with_context(|| format!("entering park image from {}", path.display()))
        };
        if let Err(error) = handoff() {
            let _ = writeln!(crate::cli::render::err(), "rimz: {error:#}");
        }
        let _ = std::fs::remove_file(path);
        let mut outcome = terminate_child(ProviderChild::Spawned(child));
        {
            use std::os::unix::process::ExitStatusExt as _;
            outcome.status = ExitStatus::from_raw(1 << 8);
        }
        let _ = workspace;
        Ok(state.into_exit(outcome, RUN_EXIT_TERMINAL_GRACE, None))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        run_loop(state, ProviderChild::Spawned(child), workspace)
    }
}

#[cfg(unix)]
pub(super) fn run(path: &Path, workspace: &rimz::ResolvedWorkspace) -> Result<ParkExit> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    let path = handoff_path(path, &paths)?;
    let exit = (|| {
        let state = ParkState::read(&path)?;
        if rimz::proc::comm_and_ppid(state.provider_pid).map(|(_, ppid)| ppid)
            != Some(std::process::id())
        {
            bail!(
                "park state {} does not name this wrapper's provider child",
                path.display()
            );
        }
        install_cleanup_signal_handlers()?;
        install_interrupt_signal_handler()?;
        let pid = state.provider_pid;
        run_loop(state, ProviderChild::Retained(pid), workspace)
    })();
    if let Err(error) = &exit {
        fail_handoff(&path, error);
    }
    let _ = std::fs::remove_file(&path);
    exit.with_context(|| format!("supervising park state {}", path.display()))
}

#[cfg(unix)]
fn handoff_path(path: &Path, paths: &rimz::StatePaths) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("reading park state {}", path.display()))?;
    let temp = paths.tmp_dir.canonicalize()?;
    let suffix = format!(".{}.json", std::process::id());
    if !path.starts_with(temp)
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("park.") && name.ends_with(&suffix))
    {
        bail!(
            "park state {} is not this wrapper's temp-unit handoff",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(unix)]
pub(super) fn fail_handoff(path: &Path, error: &anyhow::Error) {
    let fail = || -> Result<()> {
        let state = ParkState::read(path)?;
        let paths = rimz::StatePaths::for_project_root(&state.project_root)?;
        let path = handoff_path(path, &paths)?;
        fail_run(&state.request, &state.project_root, error);
        let _ = std::fs::remove_file(path);
        Ok(())
    };
    if let Err(error) = fail() {
        tracing::debug!(%error, "could not finalize failed park handoff");
    }
    terminate_retained();
}

#[cfg(unix)]
pub(super) fn fail_run(
    request: &rimz::harness::launch::ExecRequest,
    project_root: &Path,
    error: &anyhow::Error,
) {
    let Some(run_id) = request.run_id.as_ref() else {
        return;
    };
    let fail = || -> Result<()> {
        let paths = rimz::StatePaths::for_project_root(project_root)?;
        if let Some(record) = rimz::harness::run::fail_if_nonterminal(
            &paths,
            run_id,
            &crate::cli::render::error_line(error),
        )? {
            let runtime = rimz::RuntimePaths::for_state(&paths)?;
            rimz::store::run::wake_run(&runtime, &record);
        }
        Ok(())
    };
    if let Err(error) = fail() {
        tracing::debug!(%run_id, %error, "could not fail retained supervised run");
    }
}

enum ProviderChild {
    Spawned(Child),
    #[cfg(unix)]
    Retained(u32),
}

impl ProviderChild {
    fn adopt(self, wake: mpsc::Sender<()>) -> rimz::child_process::SupervisedChild {
        match self {
            Self::Spawned(child) => rimz::child_process::SupervisedChild::adopt(child, wake),
            #[cfg(unix)]
            Self::Retained(pid) => rimz::child_process::SupervisedChild::adopt_pid(pid, wake),
        }
    }
}

#[cfg(unix)]
pub(super) fn terminate_retained() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    for pid in rimz::proc::children(std::process::id()) {
        if rimz::proc::argv(pid).is_some_and(|argv| {
            argv.windows(2)
                .any(|args| args == ["agents", "supervise-duty"])
        }) {
            continue;
        }
        let _ = terminate_child(ProviderChild::Retained(pid));
    }
    ExitStatus::from_raw(1 << 8)
}

#[cfg(unix)]
fn terminate_child(child: ProviderChild) -> ExecOutcome {
    let (wake, events) = mpsc::channel();
    let mut child = child.adopt(wake);
    child.signal_term();
    let deadline = Instant::now() + CHILD_SIGNAL_GRACE;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "could not wait for provider during failed park handoff");
                use std::os::unix::process::ExitStatusExt as _;
                break ExitStatus::from_raw(1 << 8);
            }
        }
        if !killed && Instant::now() >= deadline {
            child.signal_kill();
            killed = true;
        }
        rimz::child_process::wait_wake(&events, (!killed).then_some(deadline));
    };
    ExecOutcome {
        status,
        abrupt: true,
        parent_ended: false,
        parent_watchdog: None,
    }
}

fn run_loop(
    mut state: ParkState,
    mut child: ProviderChild,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<ParkExit> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    let runtime = rimz::RuntimePaths::for_state(&paths)?;
    let diag = rimz::diag::DiagSink::under(
        paths.root.clone(),
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        None,
    );
    let run_paths = state.request.run_id.as_ref().map(|run_id| RunPaths {
        run_id: run_id.clone(),
        paths: paths.clone(),
        runtime: runtime.clone(),
    });
    let request = &state.request;
    let mut command = state.provider.command();
    let provider_terminal = state
        .termios
        .as_ref()
        .map(SavedTermios::terminal)
        .transpose()?
        .unwrap_or(ProviderTerminal {
            #[cfg(unix)]
            saved: None,
        });
    let age_ms = jiff::Timestamp::now()
        .as_millisecond()
        .saturating_sub(state.spawned_at.as_millisecond())
        .max(0);
    let mut spawned_at = Instant::now()
        .checked_sub(Duration::from_millis(age_ms as u64))
        .unwrap_or_else(Instant::now);
    let mut parent_watchdog = {
        #[cfg(unix)]
        let _worker_mask = rimz::child_process::CleanupSignalMask::block()?;
        state.watchdog.take().map(|seed| {
            let workspace_id = workspace.workspace_id.clone();
            rimz::harness::parent_watch::ParentWatchdog::from_seed(
                seed,
                paths,
                move |seed, pane_strikes| duty::confirm(&workspace_id, seed, pane_strikes),
            )
            .start()
        })
    };
    let mut relaunches: u8 = 0;
    let relaunch_cap = u8::try_from(state.relaunch_cap)?;
    let relaunch_wait = Duration::from_millis(state.relaunch_wait_ms);
    let fresh_launch = state.fresh_launch;
    let awaiting_reopen = state.awaiting_reopen;
    let launch_identity = state.identity.as_ref();
    let program = state.provider.program.to_string_lossy();
    let (outcome, terminal_grace, startup_deaths) = loop {
        let mut outcome = supervise_child(
            child,
            run_paths.as_ref(),
            &workspace.session_name,
            request.exit_on_run_completion,
            if request.subagent {
                StopPolicy::ParentReceived
            } else {
                StopPolicy::RunTerminal
            },
            awaiting_reopen,
            parent_watchdog.take(),
        )
        .context("supervising agent process")?;
        let startup = spawned_at.elapsed();
        let exit_code = outcome.status.code();
        let exit = rimz::harness::run::ProviderExit {
            fresh_launch,
            success: outcome.status.success(),
            abrupt: outcome.abrupt,
            signaled: cleanup_signal_received() || interrupt_signal_flag().load(Ordering::SeqCst),
            relaunches,
            startup,
        };
        if rimz::harness::run::provider_startup_exit(exit) {
            #[cfg(unix)]
            let signal = {
                use std::os::unix::process::ExitStatusExt;
                outcome.status.signal()
            };
            #[cfg(not(unix))]
            let signal = None;
            let action = match &request.action {
                rimz::harness::launch::ExecAction::Launch { .. } => "launch",
                rimz::harness::launch::ExecAction::Resume { .. } => "resume",
                rimz::harness::launch::ExecAction::Fork { .. } => "fork",
            };
            diag.emit(rimz::diag::record::DiagEvent::ProviderStartupExit {
                agent_kind: request.kind.to_string(),
                agent_name: request.identity.name.clone(),
                action: action.to_owned(),
                exit_code,
                signal,
                startup_ms: u64::try_from(startup.as_millis()).unwrap_or(u64::MAX),
                relaunches,
            });
        }
        let parent_ended = || {
            outcome
                .parent_watchdog
                .as_ref()
                .is_some_and(|watchdog| watchdog.parent_ended())
        };
        // Asked fresh each time: the answer is the consumer's, read at the
        // moment it would spawn.
        let mut next_card_check = Instant::now();
        let mut relaunch = || {
            if run_paths.is_none() {
                std::thread::sleep(next_card_check.saturating_duration_since(Instant::now()));
            }
            let exit = rimz::harness::run::ProviderExit {
                abrupt: outcome.abrupt || parent_ended(),
                signaled: cleanup_signal_received()
                    || interrupt_signal_flag().load(Ordering::SeqCst),
                ..exit
            };
            // A card answer costs a folding helper: ask only when it can decide.
            let card = rimz::harness::run::StartupEvidence::Card { provisional: true };
            if run_paths.is_none()
                && rimz::harness::run::startup_relaunch(exit, card, relaunch_cap)
                    == rimz::harness::run::StartupRelaunch::No
            {
                return rimz::harness::run::StartupRelaunch::No;
            }
            next_card_check = Instant::now() + CARD_EVIDENCE_POLL;
            startup_evidence(run_paths.as_ref(), workspace, launch_identity)
                .map_or(rimz::harness::run::StartupRelaunch::No, |evidence| {
                    rimz::harness::run::startup_relaunch(exit, evidence, relaunch_cap)
                })
        };
        // The grace a late first hook gets here is the one a late terminal hook gets at settle.
        let grace_ends = Instant::now() + RUN_EXIT_TERMINAL_GRACE;
        let terminal_grace = || grace_ends.saturating_duration_since(Instant::now());
        let evidence_poll = if run_paths.is_none() {
            CARD_EVIDENCE_POLL
        } else {
            CHILD_WAIT_POLL
        };
        let mut verdict = startup_relaunch_at(grace_ends, evidence_poll, &mut relaunch);
        if verdict == rimz::harness::run::StartupRelaunch::Due {
            // A provider killed mid-setup can leave the pane raw, where
            // Ctrl-C is a byte and the announced cancel never arrives.
            provider_terminal.restore();
            announce_startup_relaunch(request, exit_code, relaunch_wait, relaunches, relaunch_cap);
            let retry_at = Instant::now()
                .checked_add(relaunch_wait)
                .context("invalid park relaunch wait")?;
            verdict = startup_relaunch_at(retry_at, evidence_poll, &mut relaunch);
        }
        match verdict {
            rimz::harness::run::StartupRelaunch::Due => {}
            rimz::harness::run::StartupRelaunch::Spent => {
                break (outcome, terminal_grace(), Some(u32::from(relaunches) + 1));
            }
            rimz::harness::run::StartupRelaunch::No => {
                // Seen after the provider exited, in the grace or the wait:
                // settled as the parent end supervision would have made it.
                if parent_ended() && !outcome.parent_ended {
                    outcome.parent_ended = true;
                    outcome.abrupt = true;
                    if let Some(context) = run_paths.as_ref()
                        && let Err(err) = rimz::harness::run::cancel_and_wake_paths(
                            &context.paths,
                            &context.runtime,
                            &context.run_id,
                        )
                    {
                        tracing::debug!(
                            run_id = %context.run_id,
                            error = &err as &dyn std::error::Error,
                            "could not cancel subagent run after parent exit",
                        );
                    }
                }
                break (outcome, terminal_grace(), None);
            }
        }
        relaunches += 1;
        spawned_at = Instant::now();
        let spawned = command.spawn();
        if let Err(error) =
            rimz::harness::assist_log::try_append(&rimz::harness::assist_log::AssistRecord {
                at: jiff::Timestamp::now(),
                assist: rimz::harness::assist_log::Assist::LaunchRetry {
                    kind: request.kind.clone(),
                    label: launch_identity
                        .map(|identity| format!("@{}", identity.name))
                        .or_else(|| run_paths.as_ref().map(|context| context.run_id.to_string()))
                        .unwrap_or_default(),
                    run_id: run_paths.as_ref().map(|context| context.run_id.clone()),
                    attempt: relaunches,
                    exit_code,
                    startup_ms: u64::try_from(startup.as_millis()).unwrap_or(u64::MAX),
                    relaunched: spawned.is_ok(),
                    error: spawned
                        .as_ref()
                        .err()
                        .map(|error| format!("running {program}: {error}")),
                },
            })
        {
            let _ = writeln!(
                crate::cli::render::err(),
                "rimz: could not record launch retry: {error}"
            );
        }
        match spawned {
            Ok(next) => {
                if let Some(context) = run_paths.as_ref() {
                    record_provider_process_paths(context, next.id());
                }
                child = ProviderChild::Spawned(next);
                parent_watchdog = outcome.parent_watchdog.take();
            }
            Err(error) => {
                let _ = writeln!(
                    crate::cli::render::err(),
                    "rimz: running {program}: {error}"
                );
                break (outcome, terminal_grace(), None);
            }
        }
    };

    Ok(state.into_exit(outcome, terminal_grace, startup_deaths))
}

fn record_provider_process_paths(context: &RunPaths, pid: u32) {
    if let Err(error) = rimz::harness::run::record_provider_process(
        &context.paths,
        &context.run_id,
        pid,
        rimz::proc::process_start_token(pid),
    ) {
        tracing::debug!(run_id = %context.run_id, %error, "could not record supervised provider process");
    }
}

fn startup_evidence(
    run: Option<&RunPaths>,
    workspace: &rimz::ResolvedWorkspace,
    identity: Option<&LaunchIdentity>,
) -> Option<rimz::harness::run::StartupEvidence> {
    if let Some(context) = run {
        return rimz::harness::run::load(&context.paths, &context.run_id)
            .ok()
            .map(|record| rimz::harness::run::StartupEvidence::Run(record.status));
    }
    let identity = identity?;
    let status = duty::command(
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        Duty::CardEvidence {
            kind: identity.kind.clone(),
            launch_id: identity.agent_id.clone(),
            name: identity.name.clone(),
        },
    )
    .and_then(|mut command| command.status().context("running card evidence duty"));
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            tracing::debug!(%error, "card evidence duty failed");
            return None;
        }
    };
    match status.code() {
        Some(0) => Some(rimz::harness::run::StartupEvidence::Card { provisional: true }),
        Some(3) => Some(rimz::harness::run::StartupEvidence::Card { provisional: false }),
        _ => {
            tracing::debug!(?status, "card evidence duty failed");
            None
        }
    }
}
#[derive(Clone, Copy)]
enum StopPolicy {
    RunTerminal,
    ParentReceived,
}

struct RunPaths {
    run_id: rimz::RunId,
    paths: rimz::StatePaths,
    runtime: rimz::RuntimePaths,
}

enum DutyChild {
    Process(Child),
    #[cfg(test)]
    Fake(std::rc::Rc<RefCell<Option<i32>>>),
}

impl DutyChild {
    fn reap(self) {
        match self {
            Self::Process(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            #[cfg(test)]
            Self::Fake(_) => {}
        }
    }

    fn poll(&mut self) -> Option<i32> {
        match self {
            Self::Process(child) => match child.try_wait() {
                Ok(status) => status.map(|status| status.code().unwrap_or(1)),
                Err(error) => {
                    tracing::debug!(%error, "could not wait for supervisor duty");
                    Some(1)
                }
            },
            #[cfg(test)]
            Self::Fake(status) => *status.borrow(),
        }
    }
}

struct RunMonitor {
    self_cleanup: bool,
    stop_policy: StopPolicy,
    awaiting_reopen: Option<u32>,
    next_park_check: Instant,
    next_receipt_check: Instant,
    reported_revision: Option<jiff::Timestamp>,
    strand: Option<DutyChild>,
    receipt: Option<DutyChild>,
    strand_backoff: Duration,
    queue_stamp: Option<(std::time::SystemTime, u64)>,
    receipt_dirty: bool,
    receipt_ready: bool,
    last_receipt_check: Option<Instant>,
    waiting_for_receipt: bool,
    run_was_ready: bool,
}

impl Drop for RunMonitor {
    fn drop(&mut self) {
        for child in [self.strand.take(), self.receipt.take()]
            .into_iter()
            .flatten()
        {
            child.reap();
        }
    }
}

impl RunMonitor {
    fn new(
        self_cleanup: bool,
        stop_policy: StopPolicy,
        awaiting_reopen: Option<u32>,
        now: Instant,
    ) -> Self {
        Self {
            self_cleanup,
            stop_policy,
            awaiting_reopen,
            next_park_check: now,
            next_receipt_check: now,
            reported_revision: None,
            strand: None,
            receipt: None,
            strand_backoff: PARK_STRAND_POLL,
            queue_stamp: None,
            receipt_dirty: true,
            receipt_ready: false,
            last_receipt_check: None,
            waiting_for_receipt: false,
            run_was_ready: false,
        }
    }

    fn poll(
        &mut self,
        context: &RunPaths,
        now: Instant,
        parent_changed: bool,
        mut spawn: impl FnMut(Duty) -> Result<DutyChild>,
    ) -> bool {
        self.waiting_for_receipt = false;
        let record = match rimz::harness::run::load(&context.paths, &context.run_id) {
            Ok(record) => record,
            Err(error) => {
                tracing::debug!(run_id = %context.run_id, %error, "could not read supervised run record while monitoring pane");
                return false;
            }
        };
        let parked = record.parked_at.is_some() && !record.status.is_terminal();
        if let Some(code) = self.strand.as_mut().and_then(DutyChild::poll) {
            self.strand = None;
            self.next_park_check = now + self.strand_backoff;
            if code != 0 {
                tracing::debug!(code, "strand duty failed");
                self.strand_backoff = (self.strand_backoff * 2).min(Duration::from_secs(60));
            }
        }
        if !parked {
            self.strand_backoff = PARK_STRAND_POLL;
            self.next_park_check = now;
        } else {
            if self.strand.is_none() && now >= self.next_park_check {
                match spawn(Duty::Strand {
                    run_id: context.run_id.clone(),
                }) {
                    Ok(child) => self.strand = Some(child),
                    Err(error) => {
                        tracing::debug!(run_id = %context.run_id, %error, "could not spawn strand duty");
                        self.next_park_check = now + self.strand_backoff;
                        self.strand_backoff =
                            (self.strand_backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
        }
        if !self.self_cleanup {
            return false;
        }
        if matches!(self.stop_policy, StopPolicy::RunTerminal) {
            return record.status.is_terminal() && run_ready(context);
        }
        if let Some(ordinal) = self.awaiting_reopen {
            if record.follow_ups <= ordinal {
                return false;
            }
            self.awaiting_reopen = None;
        }
        if !record.status.is_terminal() {
            if let Some(child) = self.receipt.take() {
                child.reap();
            }
            self.receipt_ready = false;
            self.receipt_dirty = true;
            self.run_was_ready = false;
            return false;
        }
        let ready = run_ready(context);
        if record.report_to == rimz::store::run::ReportTo::Nobody {
            return ready;
        }
        self.waiting_for_receipt = true;
        self.receipt_dirty |= ready && !self.run_was_ready;
        self.run_was_ready = ready;
        let queue_stamp = std::fs::metadata(context.paths.message_queue_file())
            .ok()
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        self.receipt_dirty |= parent_changed
            || queue_stamp != self.queue_stamp
            || self.reported_revision != Some(record.updated_at);
        self.queue_stamp = queue_stamp;
        if let Some(code) = self.receipt.as_mut().and_then(DutyChild::poll) {
            self.receipt = None;
            self.receipt_ready = code == 0;
            if code != 0 && code != 3 {
                tracing::debug!(code, "receipt duty failed");
            }
        }
        let periodic = self
            .last_receipt_check
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60));
        if self.receipt.is_none()
            && now >= self.next_receipt_check
            && (self.receipt_dirty || periodic)
        {
            let report = self.reported_revision != Some(record.updated_at);
            match spawn(Duty::Receipt {
                run_id: context.run_id.clone(),
                report,
            }) {
                Ok(child) => {
                    self.receipt = Some(child);
                    self.reported_revision = Some(record.updated_at);
                    self.receipt_dirty = false;
                    self.receipt_ready = false;
                    self.last_receipt_check = Some(now);
                }
                Err(error) => {
                    tracing::debug!(run_id = %context.run_id, %error, "could not spawn receipt duty")
                }
            }
            self.next_receipt_check = now + PARENT_RECEIPT_POLL;
        }
        ready && self.receipt_ready && !self.receipt_dirty && self.receipt.is_none()
    }
}

fn run_ready(context: &RunPaths) -> bool {
    match rimz::store::run::run_waiter_is_live(&context.runtime, &context.run_id) {
        Ok(live) => !live,
        Err(error) => {
            tracing::debug!(run_id = %context.run_id, %error, "could not probe supervised run waiter");
            false
        }
    }
}

pub(super) struct ExecOutcome {
    pub(super) status: ExitStatus,
    pub(super) abrupt: bool,
    pub(super) parent_ended: bool,
    pub(super) parent_watchdog: Option<rimz::harness::parent_watch::ParentWatch>,
}

fn supervise_child(
    child: ProviderChild,
    run_monitor: Option<&RunPaths>,
    session_name: &str,
    self_cleanup: bool,
    stop_policy: StopPolicy,
    awaiting_reopen: Option<u32>,
    parent_watchdog: Option<rimz::harness::parent_watch::ParentWatch>,
) -> Result<ExecOutcome> {
    #[cfg(unix)]
    let signal_mask = rimz::child_process::CleanupSignalMask::block()?;
    let (wake_tx, wake_rx) = mpsc::channel();
    let mut child = child.adopt(wake_tx.clone());
    #[cfg(unix)]
    let cleanup_signals = {
        use signal_hook::consts::signal::{SIGHUP, SIGTERM};
        vec![SIGHUP, SIGTERM]
    };
    #[cfg(not(unix))]
    let cleanup_signals = Vec::new();
    rimz::child_process::register_signal_wake(cleanup_signals, wake_tx)
        .context("registering cleanup signal wakeups")?;
    #[cfg(unix)]
    {
        drop(signal_mask);
        rimz::child_process::CleanupSignalMask::unblock()?;
    }

    let mut signal_seen_at = cleanup_signal_received().then(Instant::now);
    let mut term_sent_at: Option<Instant> = None;
    let mut kill_sent = false;
    let mut run_completed = false;
    let mut parent_ended = false;
    let mut next_run_check = Instant::now();
    let mut monitor_state =
        RunMonitor::new(self_cleanup, stop_policy, awaiting_reopen, next_run_check);
    loop {
        let now = Instant::now();
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(watch) = &parent_watchdog {
                    watch.set_receipt_waiting(false);
                }
                return Ok(ExecOutcome {
                    status,
                    abrupt: run_completed
                        || parent_ended
                        || signal_seen_at.is_some()
                        || cleanup_signal_received(),
                    parent_ended,
                    parent_watchdog,
                });
            }
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err).context("waiting for agent process"),
        }

        if !parent_ended
            && parent_watchdog
                .as_ref()
                .is_some_and(|watchdog| watchdog.parent_ended())
        {
            parent_ended = true;
            if let Some(monitor) = run_monitor
                && let Err(err) = rimz::harness::run::cancel_and_wake_paths(
                    &monitor.paths,
                    &monitor.runtime,
                    &monitor.run_id,
                )
            {
                tracing::debug!(
                    run_id = %monitor.run_id,
                    error = &err as &dyn std::error::Error,
                    "could not cancel subagent run after parent exit",
                );
            }
            child.signal_term();
            term_sent_at = Some(now);
        }

        if !run_completed
            && let Some(monitor) = run_monitor
            && now >= next_run_check
        {
            next_run_check = now
                + if self_cleanup {
                    RUN_MONITOR_POLL
                } else {
                    PARK_STRAND_POLL
                };
            let parent_changed = parent_watchdog
                .as_ref()
                .is_some_and(|watch| watch.take_parent_changed());
            let completed = monitor_state.poll(monitor, now, parent_changed, |request| {
                duty::command(
                    monitor.paths.workspace_id.clone(),
                    session_name.to_owned(),
                    request,
                )?
                .spawn()
                .map(DutyChild::Process)
                .context("spawning supervise duty")
            });
            if let Some(watch) = &parent_watchdog {
                watch.set_receipt_waiting(monitor_state.waiting_for_receipt && !completed);
            }
            if completed {
                run_completed = true;
                child.signal_term();
                term_sent_at = Some(now);
            }
        }

        if cleanup_signal_received() {
            let first_seen = *signal_seen_at.get_or_insert(now);
            if term_sent_at.is_none() && now.duration_since(first_seen) >= CHILD_SIGNAL_GRACE {
                child.signal_term();
                term_sent_at = Some(now);
            }
        }
        if let Some(sent_at) = term_sent_at
            && !kill_sent
            && now.duration_since(sent_at) >= CHILD_SIGNAL_GRACE
        {
            child.signal_kill();
            kill_sent = true;
        }

        let deadline = [
            (!run_completed && run_monitor.is_some()).then_some(next_run_check),
            signal_seen_at
                .filter(|_| term_sent_at.is_none())
                .map(|seen_at| seen_at + CHILD_SIGNAL_GRACE),
            term_sent_at
                .filter(|_| !kill_sent)
                .map(|sent_at| sent_at + CHILD_SIGNAL_GRACE),
        ]
        .into_iter()
        .flatten()
        .min();
        rimz::child_process::wait_wake(&wake_rx, deadline);
    }
}

/// The relaunch answer as it stands at `deadline`, or `No` as soon as it is
/// `No`: a late first hook, a stop, an ended parent, or a settled run ends the
/// wait at once, and the last ask is the one the spawn follows.
fn startup_relaunch_at(
    deadline: Instant,
    poll: Duration,
    mut relaunch: impl FnMut() -> rimz::harness::run::StartupRelaunch,
) -> rimz::harness::run::StartupRelaunch {
    loop {
        let answer = relaunch();
        let left = deadline.saturating_duration_since(Instant::now());
        if answer == rimz::harness::run::StartupRelaunch::No || left.is_zero() {
            return answer;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests;
