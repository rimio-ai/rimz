//! Loop runner domain: shell checks, run locks, and budget-window gates.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use crate::agents::{
    HookPreflightErr, ProviderCapacity, TurnLifecycleNeed, WindowSurplus, find_adapter,
    preflight_hooks,
};
use crate::config::{CheckOn, MachineConfig, TaskEntry};
use crate::harness::run::PermissionMode;
use crate::harness::schedule::TaskAction;
use crate::harness::schedule::run_log::{CheckRecord, LoopRunResult};
use crate::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use crate::ids::WorkspaceId;
use crate::store::paths::{RuntimePaths, config_home, runtime_home};
use crate::workspace::WorkspaceResolver;

pub const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL: &str = "2h";
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const RUN_LOCK_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;
const TASK_TIMEOUT_UNITS: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTaskSpec {
    kind: String,
}

impl ResolvedTaskSpec {
    pub fn kind(&self) -> &str {
        &self.kind
    }
}

pub fn resolve_task_spec(
    spec: &str,
    workspace: &crate::workspace::ResolvedWorkspace,
) -> Result<ResolvedTaskSpec> {
    let machine_config = MachineConfig::load_lenient();
    let launch = crate::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &config_home(),
    )?;
    let layout = match agents_spec::resolve_spec(
        Some(spec),
        &launch.profiles,
        &machine_config.agents.commands,
        &launch.teams,
    ) {
        Ok(layout) => layout,
        Err(err @ agents_spec::LayoutErr::UnknownTeam { .. })
        | Err(err @ agents_spec::LayoutErr::UnknownCell { .. }) => {
            launch.block_untrusted_reference(Some(spec), &machine_config.agents.commands)?;
            return Err(err.into());
        }
        Err(err) => return Err(err.into()),
    };
    single_agent_cell(spec, &layout)
}

fn single_agent_cell(spec: &str, layout: &LayoutSpec) -> Result<ResolvedTaskSpec> {
    let cell_count: usize = layout.columns.iter().map(|column| column.rows.len()).sum();
    if cell_count != 1 {
        anyhow::bail!(
            "loop task `{spec}` must resolve to one agent; use a kind, profile, or virtual cell"
        );
    }
    let cell = &layout.columns[0].rows[0];
    let Cell::Agent { kind, .. } = cell else {
        anyhow::bail!(
            "loop task `{spec}` must resolve to one agent; command cells are not supported"
        );
    };
    Ok(ResolvedTaskSpec {
        kind: kind.as_str().to_owned(),
    })
}

pub fn ping_kind_supported(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if adapter.ping_args().is_none() {
        anyhow::bail!("agent kind `{kind}` does not support a ping turn; use `claude` or `codex`");
    }
    Ok(())
}

pub fn preflight_entry(
    name: &str,
    entry: &TaskEntry,
    resolved: Option<&ResolvedTaskSpec>,
) -> Result<()> {
    match TaskAction::from_entry(name, entry)? {
        TaskAction::Spawn(spec) => {
            let resolved = resolved
                .with_context(|| format!("missing resolved loop task spec for `{spec}`"))?;
            preflight_resolved_task(spec, resolved)?;
        }
        TaskAction::Deliver(target) => preflight_kind(&target.kind)?,
        TaskAction::CheckOnly => {}
    }
    Ok(())
}

pub fn preflight_task(entry: &TaskEntry) -> Result<ResolvedTaskSpec> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let spec = entry
        .agent
        .as_deref()
        .context("loop task is missing `agent`")?;
    let resolved = resolve_task_spec(spec, &workspace)?;
    preflight_resolved_task(spec, &resolved)?;
    Ok(resolved)
}

fn preflight_resolved_task(spec: &str, resolved: &ResolvedTaskSpec) -> Result<()> {
    if agents_spec::virtual_ping_shape(spec) {
        ping_kind_supported(&resolved.kind)?;
    }
    preflight_kind(&resolved.kind)
}

fn preflight_kind(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    match preflight_hooks(adapter, TurnLifecycleNeed::NotUnsupported) {
        Ok(()) => Ok(()),
        Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => anyhow::bail!(
            "{kind} cannot run as a scheduled turn: a verified executable turn-lifecycle signal is required; {reason}"
        ),
        Err(HookPreflightErr::HooksMissing) => anyhow::bail!(
            "{kind} hooks are not installed, so a scheduled turn cannot report completion\ninstall them with `rimz hooks install {kind}`"
        ),
        Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => anyhow::bail!(
            "{kind} hooks are installed but not trusted ({}), so a scheduled turn cannot report completion\n{}",
            hooks,
            fix
        ),
    }
}

pub fn parse_mode(raw: &str) -> Result<String> {
    Ok(mode_name(parse_mode_value(raw)?).to_owned())
}

pub fn parse_mode_value(raw: &str) -> Result<PermissionMode> {
    let trimmed = raw.trim();
    match PermissionMode::from_str(trimmed) {
        Ok(PermissionMode::Plan) | Err(_) => {
            anyhow::bail!("unknown loop mode `{trimmed}`; use auto, ask, or yolo")
        }
        Ok(mode) => Ok(mode),
    }
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
        PermissionMode::Yolo => "yolo",
        PermissionMode::Plan => unreachable!("loop mode parser rejects plan"),
    }
}

pub fn parse_task_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse_duration_units(raw, TASK_TIMEOUT_UNITS)
}

pub fn resolve_task_prompt(name: &str, entry: &TaskEntry) -> Result<String> {
    if let Some(prompt) = entry
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        return Ok(prompt.to_owned());
    }
    let Some(path) = entry.prompt_file.as_deref() else {
        anyhow::bail!("loop task `{name}` has no prompt; set `prompt` or `prompt-file`");
    };
    let path = resolve_config_path(path)?;
    let prompt = std::fs::read_to_string(&path)
        .with_context(|| format!("reading prompt-file `{}`", path.display()))?;
    if prompt.trim().is_empty() {
        anyhow::bail!("prompt-file `{}` is empty", path.display());
    }
    Ok(prompt)
}

pub fn resolve_config_path(path: &Path) -> Result<PathBuf> {
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let loop_path = MachineConfig::loop_path();
    let config_dir = loop_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(config_dir.join(expanded))
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn task_scope_target(
    name: &str,
    entry: &TaskEntry,
) -> Result<Option<(crate::ids::AgentKind, WorkspaceId)>> {
    let workspace = WorkspaceResolver::resolve(entry.resolved_root(), None)?;
    match TaskAction::from_entry(name, entry)? {
        TaskAction::Spawn(spec) => Ok(Some((
            crate::ids::AgentKind::new_unchecked(resolve_task_spec(spec, &workspace)?.kind),
            workspace.workspace_id,
        ))),
        TaskAction::Deliver(target) => Ok(Some((
            crate::ids::AgentKind::new_unchecked(target.kind.clone()),
            workspace.workspace_id,
        ))),
        TaskAction::CheckOnly => Ok(None),
    }
}

pub fn effective_spawn_timeout(
    mode: crate::harness::schedule::run_log::LoopRunMode,
    task_timeout: Option<Duration>,
    configured_timeout: Option<Duration>,
) -> Option<Duration> {
    task_timeout.or_else(|| {
        (mode == crate::harness::schedule::run_log::LoopRunMode::Scheduled)
            .then_some(configured_timeout.unwrap_or(SCHEDULED_RUN_DEFAULT_TIMEOUT))
    })
}

pub struct RunLockGuard {
    file: File,
}

impl Drop for RunLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLockInfo {
    pub pid: u32,
    pub started_at: Timestamp,
}

pub enum RunLockAttempt {
    Acquired(RunLockGuard),
    Held(Option<RunLockInfo>),
}

pub enum RunLockState {
    Available,
    Held(Option<RunLockInfo>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAction {
    Done,
    CancelRun,
    Signal(RunLockInfo),
    Manual,
}

pub fn acquire_run_lock(name: &str, entry: &TaskEntry) -> Result<RunLockAttempt> {
    let path = run_lock_path(name, entry)?;
    let parent = path
        .parent()
        .context("loop run lock path has no runtime parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating loop task runtime for `{}`", path.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening loop run lock `{}`", path.display()))?;
    acquire_run_lock_file(file, &path)
}

pub fn probe_run_lock(name: &str, entry: &TaskEntry) -> Result<RunLockState> {
    let path = run_lock_path(name, entry)?;
    probe_run_lock_path(&path)
}

fn probe_run_lock_path(path: &Path) -> Result<RunLockState> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RunLockState::Available);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("opening loop run lock `{}`", path.display()));
        }
    };
    probe_run_lock_file(file, path)
}

pub fn run_lock_path(name: &str, entry: &TaskEntry) -> Result<PathBuf> {
    let runtime =
        RuntimePaths::for_workspace(WorkspaceId::from_project_root(&entry.resolved_root()))
            .context("locating loop task runtime")?;
    Ok(runtime.root.join(format!("loop-run-{name}.lock")))
}

pub fn next_stop_action(
    state: &RunLockState,
    run_found: bool,
    cancel_attempted: bool,
    signal_attempted: bool,
) -> StopAction {
    match state {
        RunLockState::Available => StopAction::Done,
        RunLockState::Held(_) if run_found && !cancel_attempted => StopAction::CancelRun,
        RunLockState::Held(Some(info)) if !signal_attempted => StopAction::Signal(*info),
        RunLockState::Held(_) => StopAction::Manual,
    }
}

pub fn signal_run_lock_holder(info: &RunLockInfo) -> Result<()> {
    let pid = i32::try_from(info.pid).context("loop run lock holder pid is out of range")?;
    if pid == 0 {
        anyhow::bail!("loop run lock holder pid must be positive");
    }
    if info.pid == std::process::id() {
        anyhow::bail!("refusing to signal the current process as a loop run lock holder");
    }
    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("signaling loop run lock holder pid {pid}")),
    }
}

pub fn wait_for_run_lock_release(name: &str, entry: &TaskEntry, grace: Duration) -> Result<bool> {
    wait_for_run_lock_release_path(&run_lock_path(name, entry)?, grace)
}

fn wait_for_run_lock_release_path(path: &Path, grace: Duration) -> Result<bool> {
    let deadline = Instant::now() + grace;
    loop {
        if matches!(probe_run_lock_path(path)?, RunLockState::Available) {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(RUN_LOCK_RELEASE_POLL_INTERVAL.min(remaining));
    }
}

fn acquire_run_lock_file(mut file: File, path: &Path) -> Result<RunLockAttempt> {
    match file.try_lock() {
        Ok(()) => {
            let info = RunLockInfo {
                pid: std::process::id(),
                started_at: Timestamp::now(),
            };
            // Runtime scratch needs no fsync and stays on the locked fd:
            // renaming an atomic replacement would detach the advisory lock.
            file.set_len(0)
                .with_context(|| format!("truncating loop run lock `{}`", path.display()))?;
            file.rewind()
                .with_context(|| format!("rewinding loop run lock `{}`", path.display()))?;
            serde_json::to_writer(&mut file, &info)
                .with_context(|| format!("writing loop run lock `{}`", path.display()))?;
            file.flush()
                .with_context(|| format!("flushing loop run lock `{}`", path.display()))?;
            Ok(RunLockAttempt::Acquired(RunLockGuard { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            Ok(RunLockAttempt::Held(read_run_lock_info(&mut file)))
        }
        Err(err) => Err(std::io::Error::from(err))
            .with_context(|| format!("locking loop run lock `{}`", path.display())),
    }
}

fn probe_run_lock_file(mut file: File, path: &Path) -> Result<RunLockState> {
    match file.try_lock() {
        Ok(()) => Ok(RunLockState::Available),
        Err(std::fs::TryLockError::WouldBlock) => {
            Ok(RunLockState::Held(read_run_lock_info(&mut file)))
        }
        Err(err) => Err(std::io::Error::from(err))
            .with_context(|| format!("probing loop run lock `{}`", path.display())),
    }
}

fn read_run_lock_info(file: &mut File) -> Option<RunLockInfo> {
    let mut payload = Vec::new();
    file.read_to_end(&mut payload)
        .ok()
        .and_then(|_| serde_json::from_slice(&payload).ok())
}

pub struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    output: String,
    code: Option<i32>,
}

impl CheckOutcome {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

pub enum CheckEcho {
    Capture,
    Stream { prefix: String },
}

pub fn check_record(outcome: &CheckOutcome) -> CheckRecord {
    CheckRecord {
        code: outcome.code,
        timed_out: outcome.timed_out,
        output: outcome.output.clone(),
    }
}

pub fn deadline_expired(entry: &TaskEntry) -> bool {
    entry
        .deadline
        .is_some_and(|deadline| Timestamp::now() >= deadline)
}

pub fn check_timeout(entry: &TaskEntry) -> Result<Option<Duration>> {
    entry
        .timeout
        .as_deref()
        .map(|raw| super::parse_duration_units(raw, TASK_TIMEOUT_UNITS))
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

pub fn check_only_result(outcome: &CheckOutcome) -> LoopRunResult {
    if outcome.timed_out {
        LoopRunResult::TimedOut
    } else if outcome.passed {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    }
}

pub fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed,
        CheckOn::Success => outcome.passed,
    }
}

pub fn augment_prompt(base: String, cmd: &str, outcome: &CheckOutcome) -> String {
    let status = if outcome.timed_out {
        "timeout".to_owned()
    } else {
        outcome
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned())
    };
    format!(
        "{base}\n\n--- check `{cmd}` exited {status} ---\n{}",
        outcome.output
    )
}

pub fn run_check(
    dir: &Path,
    cmd: &str,
    timeout: Duration,
    echo: CheckEcho,
) -> Result<CheckOutcome> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running loop check `{cmd}` in {}", dir.display()))?;
    let prefix = match echo {
        CheckEcho::Capture => None,
        CheckEcho::Stream { prefix } => Some(prefix),
    };
    let stdout = drain_pipe(
        child.stdout.take(),
        prefix
            .clone()
            .map(|prefix| PipeForward::new(PipeDestination::Stdout, prefix)),
    );
    let stderr = drain_pipe(
        child.stderr.take(),
        prefix.map(|prefix| PipeForward::new(PipeDestination::Stderr, prefix)),
    );
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping timed-out loop check `{cmd}`"))?;
            break (status, true);
        }
        std::thread::sleep(CHECK_POLL_INTERVAL);
    };
    let mut output = stdout.join().unwrap_or_default();
    output.extend(stderr.join().unwrap_or_default());
    let output = tail_output(&output, CHECK_OUTPUT_CAP);
    Ok(CheckOutcome {
        passed: status.success() && !timed_out,
        timed_out,
        output,
        code: status.code(),
    })
}

#[derive(Clone, Copy)]
enum PipeDestination {
    Stdout,
    Stderr,
}

struct PipeForward {
    destination: PipeDestination,
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

impl PipeForward {
    fn new(destination: PipeDestination, prefix: String) -> Self {
        Self {
            destination,
            prefix: prefix.into_bytes(),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(line) = take_complete_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn finish(mut self) {
        if let Some(line) = take_trailing_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        let mut painted = Vec::with_capacity(self.prefix.len() + line.len());
        painted.extend_from_slice(&self.prefix);
        painted.extend_from_slice(line);
        match self.destination {
            PipeDestination::Stdout => {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(&painted)?;
                out.flush()
            }
            PipeDestination::Stderr => {
                let mut err = anstream::AutoStream::auto(std::io::stderr().lock());
                err.write_all(&painted)?;
                err.flush()
            }
        }
    }
}

fn take_complete_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = pending.iter().position(|byte| *byte == b'\n')?;
    Some(pending.drain(..=end).collect())
}

fn take_trailing_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    if pending.is_empty() {
        return None;
    }
    let mut line = std::mem::take(pending);
    line.push(b'\n');
    Some(line)
}

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    mut forward: Option<PipeForward>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8 * 1024];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                buf.extend_from_slice(bytes);
                if let Some(forward) = &mut forward {
                    forward.push(bytes);
                }
            }
        }
        if let Some(forward) = forward {
            forward.finish();
        }
        buf
    })
}

pub fn tail_output(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Whether `entry`'s provider already has a budget window counting down, read
/// from the shared account-scoped cache. The window state is account-scoped, so
/// the entry's workspace is resolved only to reach this user's runtime root.
pub fn window_already_running(entry: &TaskEntry, kind: &str) -> Result<bool> {
    let runtime = entry_runtime(entry)?;
    Ok(ProviderCapacity::read(&runtime, kind)
        .and_then(|capacity| capacity.shortest_window_running(Timestamp::now()))
        == Some(true))
}

/// Whether `entry`'s provider already has its longest budget window counting
/// down, read from the shared account-scoped cache.
pub fn reset_window_already_running(entry: &TaskEntry, kind: &str) -> Result<bool> {
    let runtime = entry_runtime(entry)?;
    Ok(ProviderCapacity::read(&runtime, kind)
        .and_then(|capacity| capacity.longest_window_running(Timestamp::now()))
        == Some(true))
}

/// Decide whether a task's provider-window surplus gate keeps this fire closed.
pub fn surplus_gate(entry: &TaskEntry, kind: &str, now: Timestamp) -> Result<Option<String>> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return Ok(None);
    }
    let runtime = entry_runtime(entry)?;
    Ok(surplus_gate_in(
        entry,
        kind,
        ProviderCapacity::read(&runtime, kind)
            .and_then(|capacity| capacity.longest_window_surplus(now)),
    ))
}

fn surplus_gate_in(
    entry: &TaskEntry,
    kind: &str,
    reading: Option<WindowSurplus>,
) -> Option<String> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return None;
    }
    let Some(reading) = reading else {
        return Some(format!(
            "no {kind} budget-window reading; surplus gate stays closed"
        ));
    };
    let after = match entry
        .surplus_after
        .as_deref()
        .map(super::parse_surplus_after)
    {
        Some(Ok(after)) => Some(after),
        Some(Err(_)) => {
            return Some("invalid surplus-after gate; surplus gate stays closed".to_owned());
        }
        None => None,
    };
    if let Some(after) = after
        && (reading.elapsed.as_secs().max(0) as u64) < after.as_secs()
    {
        return Some(format!(
            "{kind} {} window {} elapsed; fires after {}",
            window_label(reading.duration_mins),
            elapsed_label(reading.elapsed),
            entry.surplus_after.as_deref().unwrap_or_default().trim(),
        ));
    }
    let threshold = match entry.surplus.as_deref().map(super::parse_surplus) {
        Some(Ok(threshold)) => threshold,
        Some(Err(_)) => return Some("invalid surplus gate; surplus gate stays closed".to_owned()),
        None => 1.0,
    };
    (reading.headroom < threshold).then(|| {
        format!(
            "{kind} {} window surplus {:.1}x below {threshold:.1}x",
            window_label(reading.duration_mins),
            reading.headroom,
        )
    })
}

fn window_label(duration_mins: u32) -> String {
    if duration_mins.is_multiple_of(24 * 60) {
        format!("{}d", duration_mins / (24 * 60))
    } else if duration_mins.is_multiple_of(60) {
        format!("{}h", duration_mins / 60)
    } else {
        format!("{duration_mins}m")
    }
}

fn elapsed_label(elapsed: jiff::SignedDuration) -> String {
    let total_mins = elapsed.as_secs().max(0) / 60;
    let days = total_mins / (24 * 60);
    let hours = total_mins % (24 * 60) / 60;
    let mins = total_mins % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h{mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{mins}m")
    }
}

/// Raw reset stamp for `entry`'s provider longest budget window.
pub fn window_reset_at(entry: &TaskEntry, kind: &str) -> Result<Option<Timestamp>> {
    let runtime = entry_runtime(entry)?;
    Ok(ProviderCapacity::read(&runtime, kind)
        .and_then(|capacity| capacity.longest_window_reset_at()))
}

fn entry_runtime(entry: &TaskEntry) -> Result<RuntimePaths> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    RuntimePaths::under(workspace.workspace_id, &runtime_home()).context("locating runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_path = dir.path().join("missing.lock");
        assert!(matches!(
            probe_run_lock_path(&missing_path).expect("probe missing lock"),
            RunLockState::Available
        ));
        assert!(
            !missing_path.exists(),
            "probing should not create a lock file"
        );
        let path = dir.path().join("task.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open lock");

        let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
            RunLockAttempt::Acquired(guard) => guard,
            RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
        };
        let written: RunLockInfo =
            serde_json::from_slice(&std::fs::read(&path).expect("read lock"))
                .expect("parse lock info");
        assert_eq!(written.pid, std::process::id());

        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open contender");
        match acquire_run_lock_file(contender, &path).expect("contend for lock") {
            RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
            RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
            RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
        }
        drop(guard);
        let before_probe = std::fs::read(&path).expect("read lock before probe");
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open probe");
        assert!(matches!(
            probe_run_lock_file(probe, &path).expect("probe available lock"),
            RunLockState::Available
        ));
        assert_eq!(
            std::fs::read(&path).expect("read lock after probe"),
            before_probe,
            "probing an available lock should not rewrite its metadata"
        );

        let empty_path = dir.path().join("legacy.lock");
        let empty = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&empty_path)
            .expect("open empty lock");
        empty.try_lock().expect("hold empty lock");
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&empty_path)
            .expect("open empty contender");
        assert!(matches!(
            probe_run_lock_file(contender, &empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ));
    }

    #[test]
    fn stop_ladder_cancels_then_signals_then_reports_manual_recovery() {
        let info = RunLockInfo {
            pid: 42,
            started_at: Timestamp::from_second(1).expect("timestamp"),
        };
        assert_eq!(
            next_stop_action(&RunLockState::Available, true, false, false),
            StopAction::Done
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, false, false),
            StopAction::CancelRun
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, true, false),
            StopAction::Signal(info)
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, true, true),
            StopAction::Manual
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(None), false, false, false),
            StopAction::Manual
        );
    }

    #[test]
    fn wait_for_run_lock_release_observes_guard_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open lock");
        let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
            RunLockAttempt::Acquired(guard) => guard,
            RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
        };
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(guard);
        });

        assert!(
            wait_for_run_lock_release_path(&path, Duration::from_secs(1))
                .expect("wait for release")
        );
        releaser.join().expect("release thread");
    }

    fn surplus_entry(surplus: Option<&str>, surplus_after: Option<&str>) -> TaskEntry {
        TaskEntry {
            surplus: surplus.map(ToOwned::to_owned),
            surplus_after: surplus_after.map(ToOwned::to_owned),
            ..TaskEntry::default()
        }
    }

    fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
        WindowSurplus {
            duration_mins: 7 * 24 * 60,
            elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
            headroom,
        }
    }

    #[test]
    fn surplus_gate_covers_closed_elapsed_headroom_and_open_branches() {
        assert_eq!(surplus_gate_in(&TaskEntry::default(), "claude", None), None);
        assert_eq!(
            surplus_gate_in(&surplus_entry(Some("1.5x"), None), "claude", None).as_deref(),
            Some("no claude budget-window reading; surplus gate stays closed")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(2, 2.0)),
            )
            .as_deref(),
            Some("claude 7d window 2d elapsed; fires after 3d")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.4)),
            )
            .as_deref(),
            Some("claude 7d window surplus 1.4x below 1.5x")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.5)),
            ),
            None
        );
    }

    #[test]
    fn surplus_after_alone_implies_sustainable_headroom() {
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(None, Some("3d")),
                "codex",
                Some(reading(4, 0.9)),
            )
            .as_deref(),
            Some("codex 7d window surplus 0.9x below 1.0x")
        );
    }

    #[test]
    fn check_polarity_truth_table() {
        let passed = CheckOutcome {
            passed: true,
            timed_out: false,
            output: String::new(),
            code: Some(0),
        };
        let failed = CheckOutcome {
            passed: false,
            timed_out: false,
            output: String::new(),
            code: Some(1),
        };
        let timed_out = CheckOutcome {
            passed: false,
            timed_out: true,
            output: String::new(),
            code: None,
        };

        assert!(!polarity_fires(Some(CheckOn::Fail), &passed));
        assert!(polarity_fires(Some(CheckOn::Fail), &failed));
        assert!(polarity_fires(Some(CheckOn::Fail), &timed_out));
        assert!(polarity_fires(Some(CheckOn::Success), &passed));
        assert!(!polarity_fires(Some(CheckOn::Success), &failed));
        assert!(!polarity_fires(Some(CheckOn::Success), &timed_out));
    }

    #[test]
    fn run_check_captures_output_and_status() {
        let dir = tempfile::tempdir().expect("tempdir");

        let passed = run_check(
            dir.path(),
            "printf out; printf err >&2",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("passed check");
        assert!(passed.passed);
        assert_eq!(passed.code, Some(0));
        assert!(passed.output.contains("out"));
        assert!(passed.output.contains("err"));

        let failed = run_check(
            dir.path(),
            "printf nope; exit 1",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("failed check");
        assert!(!failed.passed);
        assert!(!failed.timed_out);
        assert_eq!(failed.code, Some(1));
        assert!(failed.output.contains("nope"));
    }

    #[test]
    fn run_check_honours_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome = run_check(
            dir.path(),
            "sleep 1",
            Duration::from_millis(50),
            CheckEcho::Capture,
        )
        .expect("timed-out check");

        assert!(!outcome.passed);
        assert!(outcome.timed_out);
    }

    #[test]
    fn pipe_forward_buffers_partial_lines_and_terminates_the_tail() {
        let mut pending = b"first".to_vec();
        assert_eq!(take_complete_line(&mut pending), None);

        pending.extend_from_slice(b" line\nsecond");
        assert_eq!(
            take_complete_line(&mut pending),
            Some(b"first line\n".to_vec())
        );
        assert_eq!(pending, b"second");
        assert_eq!(take_trailing_line(&mut pending), Some(b"second\n".to_vec()));
        assert!(pending.is_empty());
    }
}
