//! `rimz loop` — schedule wake-ups and command checks from the room's sidebar elder.
//!
//! The elected sidebar elder keeps time while a room for the task's project is
//! open and fires `rimz loop run <name>`, which runs an optional shell check and
//! then drives one configured prompt through either the supervised `agents -p`
//! seam or the message path to a pinned live session. A `<kind>-ping` virtual
//! cell is the window-priming special case and gets the budget-window skip
//! optimization.
//!
//! This handler parses and edits the per-machine config, lists room-open state,
//! and owns the hidden runner the elder spawns. The pure schedule parsing and
//! due evaluation live in [`rimz::schedule`]; delivery mode reuses the shared
//! message seam, and ephemeral self-wakes live in [`rimz::loop_instances`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use toml_edit::{DocumentMut, Item, Table, value};

use rimz::agents::{find_adapter, hook_trust_fix};
use rimz::agents_spec::{self, Cell, LayoutSpec};
use rimz::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use rimz::ids::WorkspaceId;
use rimz::ledger::atomic::write_bytes_atomically;
use rimz::ledger::paths::{RuntimePaths, agents_home, config_home, runtime_home, state_home};
use rimz::loop_instances;
use rimz::loop_run_log::{self, LoopRunRecord, LoopRunResult};
use rimz::message::DeliveryGate;
use rimz::schedule;
use rimz::sidebar::enrich::shortest_window_running;
use rimz::sidebar::fresh_sidebar_present;
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;

#[derive(Debug, Args)]
pub struct LoopArgs {
    #[command(subcommand)]
    command: LoopSubcmd,
}

#[derive(Debug, Subcommand)]
enum LoopSubcmd {
    /// Add or replace a task in the per-machine config.
    Add(Box<AddArgs>),
    /// Remove a task from the config.
    Remove(NameArgs),
    /// List configured tasks and whether their room is open.
    List,
    /// Run one task now. The sidebar elder calls this; humans rarely do.
    #[command(hide = true)]
    Run(NameArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Schedule name (letters, digits, `-`, `_`).
    name: String,
    /// Single agent cell to drive: a kind, profile, or virtual cell.
    #[arg(long, conflicts_with = "bind")]
    spec: Option<String>,
    /// Live agent instance to wake through the message path.
    #[arg(long, value_name = "ADDRESS", conflicts_with = "spec")]
    bind: Option<String>,
    /// Inline prompt for the scheduled turn.
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,
    /// File whose contents are used as the scheduled prompt.
    #[arg(long = "prompt-file", value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Shell command to run before any agent action.
    #[arg(long, value_name = "CMD")]
    check: Option<String>,
    /// Guard polarity for --check: fail wakes on non-zero exit, success wakes on zero exit.
    #[arg(long, value_name = "fail|success")]
    on: Option<String>,
    /// Poll-until deadline as a duration such as `30m`; resolves at add time.
    #[arg(long, value_name = "DUR")]
    until: Option<String>,
    /// Daily firing time, 24-hour `HH:MM` in the configured timezone.
    #[arg(long, conflicts_with_all = ["every", "cron", "in_after"])]
    at: Option<String>,
    /// Day mask: `daily`, `weekdays`, `weekends`, a range `mon-fri`, or a list `mon,wed,fri`.
    #[arg(long, conflicts_with_all = ["every", "cron", "in_after"])]
    days: Option<String>,
    /// Interval such as `15m`, `2h`, or `1d`.
    #[arg(long, conflicts_with_all = ["at", "days", "cron"])]
    every: Option<String>,
    /// Raw 5-field cron expression (replaces `--at`/`--days`).
    #[arg(long, conflicts_with_all = ["at", "days", "every", "in_after"])]
    cron: Option<String>,
    /// Remove the task after a successful fire.
    #[arg(long)]
    once: bool,
    /// Fire once after a duration such as `30m`; resolves in the configured timezone.
    #[arg(long = "in", value_name = "DUR", conflicts_with_all = ["at", "days", "every", "cron"])]
    in_after: Option<String>,
    /// Project root whose room hosts the task; resolved to an absolute root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Optional channel/worktree to host the transient task pane.
    #[arg(long)]
    worktree: Option<String>,
    /// Permission posture for the supervised turn: auto, ask, or yolo.
    #[arg(long)]
    mode: Option<String>,
    /// Reasoning effort for the launched agent.
    #[arg(long)]
    effort: Option<String>,
    /// Replace the agent's base system prompt with a file's contents.
    #[arg(long = "system-prompt-file", value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,
    /// Wait cap for the supervised turn.
    #[arg(long)]
    timeout: Option<String>,
}

#[derive(Debug, Args)]
struct NameArgs {
    name: String,
}

struct AddTiming {
    at: Option<String>,
    days: Option<String>,
    once: bool,
    deadline: Option<Timestamp>,
}

pub fn run(args: LoopArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        LoopSubcmd::Add(args) => add(*args),
        LoopSubcmd::Remove(args) => remove(&args.name),
        LoopSubcmd::List => list(),
        LoopSubcmd::Run(args) => run_one(&args.name, globals),
    }
}

// ---- add / remove -----------------------------------------------------------

fn add(args: AddArgs) -> Result<()> {
    schedule::validate_name(&args.name)?;
    let has_agent_action = args.spec.is_some() || args.bind.is_some();
    if !has_agent_action && args.check.is_none() {
        bail!("loop task `{}` needs --spec, --bind, or --check", args.name);
    }
    if args.on.is_some() && args.check.is_none() {
        bail!("--on requires --check");
    }
    if args.until.is_some() {
        if args.check.is_none() {
            bail!("--until requires --check");
        }
        if args.every.is_none() {
            bail!("--until requires --every");
        }
        if !has_agent_action {
            bail!("--until requires --spec or --bind");
        }
        if args.once {
            bail!("--until conflicts with --once");
        }
        if args.in_after.is_some() {
            bail!("--until conflicts with --in");
        }
    }
    let workspace = WorkspaceResolver::resolve(&args.root, None)
        .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    let target = match args.bind.as_deref() {
        Some(address) => Some(resolve_delivery_target(&workspace, &args, address)?),
        None => None,
    };
    let resolved = match args.spec.as_deref() {
        Some(spec) => Some(resolve_task_spec(spec, &workspace)?),
        None => None,
    };
    let is_ping = args
        .spec
        .as_deref()
        .is_some_and(agents_spec::virtual_ping_shape);
    if is_ping {
        ping_kind_supported(&resolved.as_ref().expect("ping has spec").kind)?;
    }
    let mode = if target.is_some() {
        reject_delivery_spawn_flags(&args)?;
        None
    } else if resolved.is_none() {
        reject_check_only_agent_flags(&args)?;
        None
    } else {
        args.mode.as_deref().map(parse_mode).transpose()?
    };
    if let Some(timeout) = args.timeout.as_deref() {
        parse_task_timeout(timeout).map_err(|err| anyhow::anyhow!("{err}"))?;
    }
    let on = args.on.as_deref().map(parse_check_on).transpose()?;
    let timing = resolve_add_timing(&args)?;
    let prompt = if is_ping && args.prompt.is_none() && args.prompt_file.is_none() {
        Some("ping".to_owned())
    } else {
        args.prompt
    };
    if has_agent_action && prompt.is_none() && args.prompt_file.is_none() {
        bail!(
            "loop task `{}` needs a prompt; pass --prompt or --prompt-file",
            args.name
        );
    }
    let check = args.check;
    let uses_check_timeout = check.is_some();
    let entry = TaskEntry {
        spec: args.spec,
        bind: target,
        prompt,
        prompt_file: args.prompt_file,
        check,
        on,
        root: workspace.project_root,
        worktree: if resolved.is_some() {
            args.worktree
        } else {
            None
        },
        mode,
        effort: if resolved.is_some() {
            args.effort
        } else {
            None
        },
        system_prompt_file: if resolved.is_some() {
            args.system_prompt_file
        } else {
            None
        },
        timeout: if resolved.is_some() || uses_check_timeout {
            args.timeout
        } else {
            None
        },
        at: timing.at,
        days: timing.days,
        every: args.every,
        cron: args.cron,
        deadline: timing.deadline,
        once: timing.once,
    };
    // Validate the firing time before writing, so a bad `--at`/`--days` fails here.
    let parsed = schedule::parse_schedule(&args.name, &entry)?;
    if has_agent_action {
        preflight_entry(&args.name, &entry, resolved.as_ref())?;
    }
    if is_ephemeral(&entry) {
        config_remove(&args.name)?;
        loop_instances::insert(&args.name, &entry)?;
    } else {
        loop_instances::remove(&args.name)?;
        config_set_entry(&args.name, &entry)?;
    }

    let mut out = ui::out();
    writeln!(
        out,
        "added loop task `{}`: {} {} in {}",
        args.name,
        task_subject(&entry),
        parsed.describe(),
        entry.root.display()
    )?;
    writeln!(
        out,
        "live while a room for {} is open",
        entry.root.display()
    )?;
    if !room_open(&entry.root) {
        writeln!(out, "no room is open there; start one with `rimz start`")?;
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let removed = loop_instances::remove(name)? | config_remove(name)?;
    let mut out = ui::out();
    if removed {
        writeln!(out, "removed loop task `{name}`")?;
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

// ---- list -------------------------------------------------------------------

fn list() -> Result<()> {
    let tasks = load_all();
    let mut out = ui::out();
    if tasks.is_empty() {
        writeln!(out, "no loop tasks; add one with `rimz loop add`")?;
        return Ok(());
    }
    let stats = loop_run_log::stats(&state_home());
    let now = Timestamp::now();
    let mut table = ui::Table::new([
        "NAME", "SOURCE", "SPEC", "SCHEDULE", "ROOM", "RUNS", "LAST RUN", "RESULT", "ROOT",
    ])
    .right(&[5]);
    for (name, (entry, source)) in &tasks {
        let when = match schedule::parse_schedule(name, entry) {
            Ok(schedule) => schedule.describe(),
            Err(err) => format!("invalid: {err}"),
        };
        let root = entry.resolved_root();
        let state = if room_open(&root) { "open" } else { "no room" };
        let task_stats = stats.get(name);
        let runs = task_stats.map_or(0, |stats| stats.runs);
        let last_run = if let Some(stats) = task_stats {
            ui::cell(ui::rel_age(stats.last.at, now))
        } else {
            ui::cell("-").dash()
        };
        let result = task_stats
            .map(|stats| loop_result_cell(stats.last.result))
            .unwrap_or_else(|| ui::cell("-").dash());
        let root_text = root.to_string_lossy();
        table.row([
            ui::cell(name.as_str()).fg(ui::palette::ACCENT),
            ui::cell(source.label()),
            ui::cell(task_subject(entry)),
            ui::cell(when),
            ui::cell(state),
            ui::cell(runs.to_string()),
            last_run,
            result,
            ui::cell(ui::home_relative(root_text.as_ref())),
        ]);
    }
    table.render(&mut out)?;
    Ok(())
}

fn room_open(root: &Path) -> bool {
    RuntimePaths::for_workspace(WorkspaceId::from_project_root(root))
        .is_ok_and(|runtime| fresh_sidebar_present(&runtime))
}

fn loop_result_cell(result: LoopRunResult) -> ui::Cell {
    let style = match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => ui::palette::GOOD,
        LoopRunResult::Failed | LoopRunResult::TimedOut | LoopRunResult::Errored => {
            ui::palette::ALARM
        }
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::SkippedWindow => ui::palette::WARN,
        LoopRunResult::CheckSkipped => ui::palette::MUTED,
    };
    ui::cell(result.label()).fg(style)
}

fn loop_record(task: &str, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        task: task.to_owned(),
        at: Timestamp::now(),
        result,
    }
}

// ---- run --------------------------------------------------------------------

fn run_one(name: &str, globals: &GlobalFlags) -> Result<()> {
    let (entry, source) = load_entry(name)?;
    let action = task_action(name, &entry)?;
    if deadline_expired(&entry) {
        let _ = remove_task(name, source)?;
        loop_run_log::append(&loop_record(name, LoopRunResult::Expired));
        return Ok(());
    }
    let prompt_override = match entry.check.as_deref() {
        Some(cmd) => {
            let outcome = run_check(
                &entry.resolved_root(),
                cmd,
                check_timeout(&entry)?.unwrap_or(CHECK_DEFAULT_TIMEOUT),
            )?;
            match action {
                TaskAction::CheckOnly => {
                    if is_ephemeral(&entry) {
                        let _ = remove_task(name, source)?;
                    }
                    loop_run_log::append(&loop_record(name, check_only_result(&outcome)));
                    return Ok(());
                }
                TaskAction::Spawn(_) | TaskAction::Deliver(_) => {
                    if !polarity_fires(entry.on, &outcome) {
                        loop_run_log::append(&loop_record(name, LoopRunResult::CheckSkipped));
                        return Ok(());
                    }
                    Some(augment_prompt(resolve_task_prompt(&entry)?, cmd, &outcome))
                }
            }
        }
        None => None,
    };
    let TaskAction::Spawn(spec) = action else {
        if let TaskAction::Deliver(target) = action {
            return run_delivery_task(name, &entry, source, target, prompt_override, globals);
        }
        unreachable!("check-only task without check is rejected by task_action");
    };
    let resolved = preflight_task(&entry)?;
    let is_ping = agents_spec::virtual_ping_shape(spec);
    // The ping exists only to *start* a sliding budget window, so a token spent on
    // one already counting down buys nothing — skip it. Best-effort: an unknown or
    // cold reading falls through to the ping.
    if is_ping && window_already_running(&entry, &resolved.kind)? {
        writeln!(
            ui::out(),
            "loop `{name}`: {} budget window already active; skipping ping",
            resolved.kind
        )?;
        loop_run_log::append(&loop_record(name, LoopRunResult::SkippedWindow));
        return Ok(());
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(&entry)?,
    };
    let system_prompt_file = entry
        .system_prompt_file
        .as_deref()
        .map(resolve_config_path)
        .transpose()?;
    let (ask, yolo) = mode_flags(entry.mode.as_deref())?;
    let timeout = entry
        .timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut run_globals = globals.clone();
    run_globals.root = Some(entry.resolved_root());
    if is_ephemeral(&entry) {
        // One-shot cleanup happens before the terminal run. A one-shot removed
        // pre-fire that then fails to launch is not retried.
        let _ = remove_task(name, source)?;
    }
    let effort = entry
        .effort
        .clone()
        .or_else(|| is_ping.then(|| "low".to_owned()));
    let args = super::agents_cmd::AgentsArgs::for_task(super::agents_cmd::TaskRunArgs {
        spec: spec.to_owned(),
        prompt: Some(prompt),
        worktree: entry.worktree,
        ask,
        yolo,
        effort,
        system_prompt_file,
        timeout,
    });
    match super::agents_cmd::run_blocking_task(args, &run_globals) {
        Ok(Some(status)) => {
            loop_run_log::append(&loop_record(name, status.into()));
            std::process::exit(status.exit_code());
        }
        Ok(None) => Ok(()),
        Err(err) => {
            loop_run_log::append(&loop_record(name, LoopRunResult::Errored));
            Err(err)
        }
    }
}

fn run_delivery_task(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    target: &TaskTarget,
    prompt_override: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    if !delivery_target_alive(entry, target)? {
        writeln!(
            ui::out(),
            "loop `{name}`: target {} not alive; removing schedule",
            target.handle
        )?;
        let _ = remove_task(name, source)?;
        loop_run_log::append(&loop_record(name, LoopRunResult::TargetGone));
        return Ok(());
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(entry)?,
    };
    if is_ephemeral(entry) {
        let _ = remove_task(name, source)?;
    }
    let root = entry.resolved_root();
    match super::message::to_session(
        &root,
        &target.kind,
        &target.session,
        prompt,
        DeliveryGate::Done,
        globals,
    ) {
        Ok(()) => {
            loop_run_log::append(&loop_record(name, LoopRunResult::Delivered));
            Ok(())
        }
        Err(err) if queue_resolution_miss(&err) => {
            writeln!(
                ui::out(),
                "loop `{name}`: target {} not alive; removing schedule",
                target.handle
            )?;
            let _ = remove_task(name, source)?;
            loop_run_log::append(&loop_record(name, LoopRunResult::TargetGone));
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn delivery_target_alive(entry: &TaskEntry, target: &TaskTarget) -> Result<bool> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let ledger = super::open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    Ok(snapshot.agents.iter().any(|agent| {
        agent.parent_agent_id.is_none()
            && agent.kind.as_str() == target.kind.as_str()
            && agent.agent_id.as_str() == target.session
    }))
}

fn queue_resolution_miss(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<rimz::TargetErr>(),
        Some(rimz::TargetErr::NoMatch { .. } | rimz::TargetErr::NoMatchInChannel { .. })
    )
}

struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    output: String,
    code: Option<i32>,
}

fn deadline_expired(entry: &TaskEntry) -> bool {
    entry
        .deadline
        .is_some_and(|deadline| Timestamp::now() >= deadline)
}

fn check_timeout(entry: &TaskEntry) -> Result<Option<Duration>> {
    entry
        .timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn check_only_result(outcome: &CheckOutcome) -> LoopRunResult {
    if outcome.timed_out {
        LoopRunResult::TimedOut
    } else if outcome.passed {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    }
}

fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed,
        CheckOn::Success => outcome.passed,
    }
}

fn augment_prompt(base: String, cmd: &str, outcome: &CheckOutcome) -> String {
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

fn run_check(dir: &Path, cmd: &str, timeout: Duration) -> Result<CheckOutcome> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running loop check `{cmd}` in {}", dir.display()))?;
    let stdout = drain_pipe(child.stdout.take());
    let stderr = drain_pipe(child.stderr.take());
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

fn drain_pipe(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

fn tail_output(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

pub(crate) fn reap_dead_delivery_schedules() -> Result<usize> {
    let mut reaped = 0;
    for (name, (entry, source)) in load_all() {
        let target = match task_action(&name, &entry) {
            Ok(TaskAction::Deliver(target)) => target,
            Ok(TaskAction::Spawn(_) | TaskAction::CheckOnly) => continue,
            Err(err) => {
                tracing::debug!(task = %name, error = %err, "invalid loop task skipped by schedule gc");
                continue;
            }
        };
        match delivery_target_alive(&entry, target) {
            Ok(true) => {}
            Ok(false) => {
                let _ = remove_task(&name, source)?;
                reaped += 1;
            }
            Err(err) => {
                tracing::debug!(task = %name, error = %err, "loop schedule gc skipped task");
            }
        }
    }
    Ok(reaped)
}

/// Whether `entry`'s provider already has a budget window counting down, read
/// from the shared account-scoped cache. The window state is account-scoped, so
/// the entry's workspace is resolved only to reach this user's runtime root.
fn window_already_running(entry: &TaskEntry, kind: &str) -> Result<bool> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let runtime = RuntimePaths::under(workspace.workspace_id, &runtime_home())
        .context("locating the runtime root")?;
    Ok(shortest_window_running(&runtime, kind, Timestamp::now()) == Some(true))
}

// ---- shared helpers ---------------------------------------------------------

struct ResolvedTaskSpec {
    kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskAction<'a> {
    Spawn(&'a str),
    Deliver(&'a TaskTarget),
    CheckOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskSource {
    Config,
    Instance,
}

impl TaskSource {
    fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Instance => "state",
        }
    }
}

fn task_action<'a>(name: &str, entry: &'a TaskEntry) -> Result<TaskAction<'a>> {
    match (entry.spec.as_deref(), entry.bind.as_ref()) {
        (Some(spec), None) if !spec.trim().is_empty() => Ok(TaskAction::Spawn(spec)),
        (None, Some(target)) => Ok(TaskAction::Deliver(target)),
        (None, None) if entry.check.is_some() => Ok(TaskAction::CheckOnly),
        (Some(_), Some(_)) => {
            bail!("loop task `{name}` sets both `spec` and `bind`; keep exactly one")
        }
        _ => bail!("loop task `{name}` needs `spec`, `bind`, or `check`"),
    }
}

fn preflight_entry(
    name: &str,
    entry: &TaskEntry,
    resolved: Option<&ResolvedTaskSpec>,
) -> Result<()> {
    match task_action(name, entry)? {
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

fn task_subject(entry: &TaskEntry) -> String {
    entry
        .spec
        .clone()
        .or_else(|| entry.bind.as_ref().map(|target| target.handle.clone()))
        .or_else(|| entry.check.as_ref().map(|_| "check".to_owned()))
        .unwrap_or_else(|| "<invalid>".to_owned())
}

fn resolve_delivery_target(
    workspace: &rimz::ResolvedWorkspace,
    args: &AddArgs,
    address: &str,
) -> Result<TaskTarget> {
    let ledger = super::open_ledger(workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let channel = super::current_channel(workspace);
    let agent = match super::resolve_agent_one(
        &snapshot,
        address,
        args.worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(agent) => agent,
        Err(_) => {
            bail!("no live agent matches `{address}`; run /schedule from inside the agent pane")
        }
    };
    if agent.agent_id.is_provisional() {
        bail!(
            "`{address}` has not registered a real session yet; run /schedule from inside the agent pane"
        );
    }
    let peers: Vec<_> = snapshot
        .agents
        .iter()
        .filter(|peer| peer.parent_agent_id.is_none())
        .collect();
    Ok(TaskTarget {
        kind: agent.kind.as_str().to_owned(),
        session: agent.agent_id.as_str().to_owned(),
        handle: rimz::target::agent_handle(agent, &peers, true),
    })
}

fn reject_delivery_spawn_flags(args: &AddArgs) -> Result<()> {
    let mut flags = Vec::new();
    if args.mode.is_some() {
        flags.push("--mode");
    }
    if args.effort.is_some() {
        flags.push("--effort");
    }
    if args.system_prompt_file.is_some() {
        flags.push("--system-prompt-file");
    }
    if args.timeout.is_some() && args.check.is_none() {
        flags.push("--timeout");
    }
    if flags.is_empty() {
        return Ok(());
    }
    bail!(
        "`{}` uses --bind, so {} only apply to --spec tasks",
        args.name,
        flags.join(", ")
    )
}

fn reject_check_only_agent_flags(args: &AddArgs) -> Result<()> {
    let mut flags = Vec::new();
    if args.worktree.is_some() {
        flags.push("--worktree");
    }
    if args.mode.is_some() {
        flags.push("--mode");
    }
    if args.effort.is_some() {
        flags.push("--effort");
    }
    if args.system_prompt_file.is_some() {
        flags.push("--system-prompt-file");
    }
    if flags.is_empty() {
        return Ok(());
    }
    bail!(
        "`{}` uses --check without an agent action, so {} only apply to --spec tasks",
        args.name,
        flags.join(", ")
    )
}

fn resolve_task_spec(spec: &str, workspace: &rimz::ResolvedWorkspace) -> Result<ResolvedTaskSpec> {
    let machine_config = super::machine_config();
    let profiles = rimz::config::effective::effective_profiles(
        &machine_config.agents.profiles,
        &workspace.project_root,
        &config_home(),
    )?;
    let teams = rimz::config::effective::effective_teams(
        &machine_config.agents.teams,
        &workspace.project_root,
        &config_home(),
    )?;
    let layout = match agents_spec::resolve_spec(
        Some(spec),
        &profiles,
        &machine_config.agents.commands,
        &teams,
    ) {
        Ok(layout) => layout,
        Err(err @ agents_spec::LayoutErr::UnknownTeam { .. })
        | Err(err @ agents_spec::LayoutErr::UnknownCell { .. }) => {
            rimz::config::effective::block_untrusted_profile_reference(
                Some(spec),
                &profiles,
                &machine_config.agents.commands,
                &teams,
                &workspace.project_root,
                &config_home(),
            )?;
            return Err(err.into());
        }
        Err(err) => return Err(err.into()),
    };
    single_agent_cell(spec, &layout)
}

fn single_agent_cell(spec: &str, layout: &LayoutSpec) -> Result<ResolvedTaskSpec> {
    let cell_count: usize = layout.columns.iter().map(|column| column.rows.len()).sum();
    if cell_count != 1 {
        bail!("loop task `{spec}` must resolve to one agent; use a kind, profile, or virtual cell");
    }
    let cell = &layout.columns[0].rows[0];
    let Cell::Agent { kind, .. } = cell else {
        bail!("loop task `{spec}` must resolve to one agent; command cells are not supported");
    };
    Ok(ResolvedTaskSpec {
        kind: kind.as_str().to_owned(),
    })
}

/// Validate that a kind can be pinged at all — enforced at add time, before the
/// hooks/trust preconditions a fired ping needs.
fn ping_kind_supported(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if adapter.ping_args().is_none() {
        bail!("agent kind `{kind}` does not support a ping turn; use `claude` or `codex`");
    }
    Ok(())
}

/// The full precondition a fired task needs: installed and trusted hooks so the
/// supervised turn can report completion. Ping tasks also require ping support.
fn preflight_task(entry: &TaskEntry) -> Result<ResolvedTaskSpec> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let spec = entry
        .spec
        .as_deref()
        .context("loop task is missing `spec`")?;
    let resolved = resolve_task_spec(spec, &workspace)?;
    preflight_resolved_task(spec, &resolved)?;
    Ok(resolved)
}

fn preflight_resolved_task(spec: &str, resolved: &ResolvedTaskSpec) -> Result<()> {
    if agents_spec::virtual_ping_shape(spec) {
        ping_kind_supported(&resolved.kind)?;
    }
    preflight_kind(&resolved.kind)?;
    Ok(())
}

fn preflight_kind(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if !adapter.hooks_installed() {
        bail!(
            "{kind} hooks are not installed, so the task cannot report completion; run `rimz hooks install {kind}`"
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            untrusted.join(", "),
            hook_trust_fix(kind)
        );
    }
    Ok(())
}

fn load_tasks() -> std::collections::BTreeMap<String, TaskEntry> {
    MachineConfig::load_lenient().r#loop.tasks.0
}

fn load_all() -> std::collections::BTreeMap<String, (TaskEntry, TaskSource)> {
    let mut tasks: std::collections::BTreeMap<_, _> = loop_instances::load()
        .0
        .into_iter()
        .map(|(name, entry)| (name, (entry, TaskSource::Instance)))
        .collect();
    tasks.extend(
        load_tasks()
            .into_iter()
            .map(|(name, entry)| (name, (entry, TaskSource::Config))),
    );
    tasks
}

fn load_entry(name: &str) -> Result<(TaskEntry, TaskSource)> {
    if let Some(entry) = loop_instances::load().0.remove(name) {
        return Ok((entry, TaskSource::Instance));
    }
    load_tasks()
        .remove(name)
        .map(|entry| (entry, TaskSource::Config))
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))
}

fn remove_task(name: &str, source: TaskSource) -> Result<bool> {
    match source {
        TaskSource::Config => config_remove(name),
        TaskSource::Instance => loop_instances::remove(name).map_err(Into::into),
    }
}

fn is_ephemeral(entry: &TaskEntry) -> bool {
    entry.once || entry.deadline.is_some()
}

fn resolve_add_timing(args: &AddArgs) -> Result<AddTiming> {
    let deadline = args.until.as_deref().map(resolve_deadline).transpose()?;
    let Some(raw) = args.in_after.as_deref() else {
        return Ok(AddTiming {
            at: args.at.clone(),
            days: args.days.clone(),
            once: args.once,
            deadline,
        });
    };
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--in must be greater than zero");
    }
    let target = Timestamp::now()
        .to_zoned(MachineConfig::load_lenient().time_zone())
        .checked_add(duration)
        .context("resolving --in against the configured clock")?;
    Ok(AddTiming {
        at: Some(format!("{:02}:{:02}", target.hour(), target.minute())),
        days: Some(weekday_name(target.weekday()).to_owned()),
        once: true,
        deadline,
    })
}

fn resolve_deadline(raw: &str) -> Result<Timestamp> {
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--until must be greater than zero");
    }
    Ok(Timestamp::now()
        .to_zoned(MachineConfig::load_lenient().time_zone())
        .checked_add(duration)
        .context("resolving --until against the configured clock")?
        .timestamp())
}

fn weekday_name(day: jiff::civil::Weekday) -> &'static str {
    match day {
        jiff::civil::Weekday::Monday => "mon",
        jiff::civil::Weekday::Tuesday => "tue",
        jiff::civil::Weekday::Wednesday => "wed",
        jiff::civil::Weekday::Thursday => "thu",
        jiff::civil::Weekday::Friday => "fri",
        jiff::civil::Weekday::Saturday => "sat",
        jiff::civil::Weekday::Sunday => "sun",
    }
}

fn parse_mode(raw: &str) -> Result<String> {
    match raw.trim() {
        "auto" => Ok("auto".to_owned()),
        "ask" => Ok("ask".to_owned()),
        "yolo" => Ok("yolo".to_owned()),
        other => bail!("unknown loop mode `{other}`; use auto, ask, or yolo"),
    }
}

fn parse_check_on(raw: &str) -> Result<CheckOn> {
    match raw.trim() {
        "fail" => Ok(CheckOn::Fail),
        "success" => Ok(CheckOn::Success),
        other => bail!("unknown loop check polarity `{other}`; use fail or success"),
    }
}

fn mode_flags(raw: Option<&str>) -> Result<(bool, bool)> {
    match raw.map(str::trim).filter(|mode| !mode.is_empty()) {
        None | Some("auto") => Ok((false, false)),
        Some("ask") => Ok((true, false)),
        Some("yolo") => Ok((false, true)),
        Some(other) => bail!("unknown loop mode `{other}`; use auto, ask, or yolo"),
    }
}

fn parse_task_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

fn resolve_task_prompt(entry: &TaskEntry) -> Result<String> {
    if let Some(prompt) = entry
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        return Ok(prompt.to_owned());
    }
    let Some(path) = entry.prompt_file.as_deref() else {
        bail!(
            "loop task `{}` has no prompt; set `prompt` or `prompt-file`",
            task_subject(entry)
        );
    };
    let path = resolve_config_path(path)?;
    let prompt = std::fs::read_to_string(&path)
        .with_context(|| format!("reading prompt-file `{}`", path.display()))?;
    if prompt.trim().is_empty() {
        bail!("prompt-file `{}` is empty", path.display());
    }
    Ok(prompt)
}

fn resolve_config_path(path: &Path) -> Result<PathBuf> {
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

// ---- config editing (toml_edit, comment-preserving) -------------------------

fn config_set_entry(name: &str, entry: &TaskEntry) -> Result<()> {
    let path = MachineConfig::loop_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            MachineConfig::template_loop().to_owned()
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    let mut table = Table::new();
    if let Some(spec) = &entry.spec {
        table["spec"] = value(spec);
    }
    if let Some(target) = &entry.bind {
        table["bind"] = Item::Table(task_target_table(target));
    }
    if let Some(prompt) = &entry.prompt {
        table["prompt"] = value(prompt);
    }
    if let Some(prompt_file) = &entry.prompt_file {
        table["prompt-file"] = value(prompt_file.to_string_lossy().into_owned());
    }
    if let Some(check) = &entry.check {
        table["check"] = value(check);
    }
    if let Some(on) = entry.on {
        table["on"] = value(match on {
            CheckOn::Fail => "fail",
            CheckOn::Success => "success",
        });
    }
    table["root"] = value(entry.root.to_string_lossy().into_owned());
    if let Some(worktree) = &entry.worktree {
        table["worktree"] = value(worktree);
    }
    if let Some(mode) = &entry.mode {
        table["mode"] = value(mode);
    }
    if let Some(effort) = &entry.effort {
        table["effort"] = value(effort);
    }
    if let Some(path) = &entry.system_prompt_file {
        table["system-prompt-file"] = value(path.to_string_lossy().into_owned());
    }
    if let Some(timeout) = &entry.timeout {
        table["timeout"] = value(timeout);
    }
    if let Some(at) = &entry.at {
        table["at"] = value(at);
    }
    if let Some(days) = &entry.days {
        table["days"] = value(days);
    }
    if let Some(every) = &entry.every {
        table["every"] = value(every);
    }
    if let Some(cron) = &entry.cron {
        table["cron"] = value(cron);
    }
    if let Some(deadline) = entry.deadline {
        table["deadline"] = value(deadline.to_string());
    }
    if entry.once {
        table["once"] = value(true);
    }
    root_tasks_table(&mut doc)?.insert(name, Item::Table(table));

    let rendered = doc.to_string();
    MachineConfig::parse_text(&path, &rendered, &agents_home())
        .with_context(|| format!("validating `loop.tasks.{name}`"))?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn task_target_table(target: &TaskTarget) -> Table {
    let mut table = Table::new();
    table["kind"] = value(target.kind.as_str());
    table["session"] = value(target.session.as_str());
    table["handle"] = value(target.handle.as_str());
    table
}

fn root_tasks_table(doc: &mut DocumentMut) -> Result<&mut Table> {
    let tasks = doc
        .as_table_mut()
        .entry("tasks")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`tasks` is not a table")?;
    tasks.set_implicit(true);
    Ok(tasks)
}

fn config_remove(name: &str) -> Result<bool> {
    let path = MachineConfig::loop_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let removed = doc
        .get_mut("tasks")
        .and_then(Item::as_table_mut)
        .map(|tasks| tasks.remove(name).is_some())
        .unwrap_or(false);
    if removed {
        let rendered = doc.to_string();
        write_bytes_atomically(&path, rendered.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .expect("passed check");
        assert!(passed.passed);
        assert_eq!(passed.code, Some(0));
        assert!(passed.output.contains("out"));
        assert!(passed.output.contains("err"));

        let failed = run_check(dir.path(), "printf nope; exit 1", Duration::from_secs(1))
            .expect("failed check");
        assert!(!failed.passed);
        assert!(!failed.timed_out);
        assert_eq!(failed.code, Some(1));
        assert!(failed.output.contains("nope"));
    }

    #[test]
    fn run_check_honours_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome =
            run_check(dir.path(), "sleep 1", Duration::from_millis(50)).expect("timed-out check");

        assert!(!outcome.passed);
        assert!(outcome.timed_out);
    }
}
