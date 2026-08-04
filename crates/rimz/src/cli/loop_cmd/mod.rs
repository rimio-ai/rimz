//! `rimz loop` — schedule wake-ups and command checks from the room's sidebar elder.
//!
//! The elected sidebar elder keeps time while a room for the task's project is
//! open and fires `rimz loop run <name>`, which runs an optional shell check and
//! then drives one configured prompt through either the supervised `agents -p`
//! seam or the message path to a pinned live session.
//!
//! This handler parses commands, lists room-open and next-fire state, inspects
//! run history, executes prepared supervised-run or message effects, and owns
//! terminal presentation. [`rimz::harness::schedule::runner::TaskFire`] owns the
//! hidden runner policy and its exactly-one history transition. Pure schedule
//! parsing and due evaluation live in [`rimz::harness::schedule`]; delivery mode
//! reuses the shared message seam, and ephemeral self-wakes live in
//! [`rimz::harness::schedule::instances`].

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use rimz::harness::plan::{ResolvedSingleAgentLaunch, resolve_single_agent_launch};
use rimz::harness::run::RunRecord;
use rimz::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunTransition,
};
use rimz::harness::schedule::runner::{
    CheckEcho, RunLockInfo, RunLockState, SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL, StopAction,
    newest_active_run, newest_active_run_for_entry, next_stop_action, parse_mode,
    parse_task_timeout, preflight_entry, probe_run_lock, run_lock_path, signal_run_lock_holder,
    wait_for_run_lock_release,
};
use rimz::harness::schedule::{
    self, TaskAction, TaskActionKind,
    arming::{self, ArmState, Arming, DisabledReason, TaskKey},
    catalog::{LoadedTask, TaskCatalog, TaskSource},
    strikes,
};
use rimz::ids::WorkspaceId;
use rimz::message::DeliveryGate;
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::paths::{RuntimePaths, StatePaths, state_home};
use rimz::trust::{self, TrustState};
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

mod add;
mod render;
mod run_report;
#[path = "run.rs"]
mod run_tasks;
mod stop;
mod watch;

#[derive(Debug, Args)]
pub struct LoopArgs {
    #[command(subcommand)]
    command: LoopSubcmd,
}

#[derive(Debug, Subcommand)]
enum LoopSubcmd {
    /// Add or replace a task in the per-machine config.
    Add(Box<AddArgs>),
    /// Remove a task from its store.
    Remove(NameArgs),
    /// Rename a task in the store that owns it.
    Rename(RenameArgs),
    /// Pause a task for a bounded duration.
    Pause(PauseArgs),
    /// Enable tasks here without replaying missed fires.
    Enable(ScopeArgs),
    /// Disable tasks here until explicitly enabled.
    Disable(ScopeArgs),
    /// Stop a task's active run, releasing its overlap lock.
    Stop(NameArgs),
    /// List configured tasks and whether their room is open.
    List,
    /// Hold a live loop dashboard open and repaint countdowns.
    Watch(WatchArgs),
    /// Show one task's schedule, next fire, and recent run forensics.
    Show(ShowArgs),
    /// Print full forensics for a task's recent runs.
    Logs(LogsArgs),
    /// Fire one task now in the foreground for testing; one-shots and schedules stay put.
    Fire(FireArgs),
    /// Run one task now. The sidebar elder calls this; humans rarely do.
    #[command(hide = true)]
    Run(NameArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Schedule name (letters, digits, `-`, `_`).
    name: String,
    /// Kind, profile, or virtual cell; launches a fresh supervised pane.
    #[arg(
        long,
        conflicts_with = "wake",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    agent: Option<String>,
    /// Live agent to wake through the message path; resolved and pinned now.
    #[arg(
        long,
        value_name = "ADDRESS",
        conflicts_with = "agent",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::handles)
    )]
    wake: Option<String>,
    /// Inline prompt for the scheduled turn.
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,
    /// File whose contents are used as the scheduled prompt.
    #[arg(long = "prompt-file", value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Shell command to run before any agent action.
    #[arg(long, value_name = "CMD")]
    check: Option<String>,
    /// Shell command that must pass before a spawned agent task is complete.
    #[arg(long, value_name = "CMD", requires = "agent")]
    verify: Option<String>,
    /// Total agent turns allowed while making --verify pass.
    #[arg(long, value_name = "N", requires = "verify")]
    max_attempts: Option<u32>,
    /// Auto-disable after N consecutive failed fires; default 3, 0 disables.
    #[arg(long, value_name = "N")]
    max_strikes: Option<u32>,
    /// Guard polarity for --check: fail wakes on non-zero exit, success wakes on zero exit.
    #[arg(long, value_name = "fail|success")]
    on: Option<String>,
    /// Poll-until deadline as a duration such as `30m`; resolves at add time.
    #[arg(long, value_name = "DUR")]
    until: Option<String>,
    /// One-shot firing time, or calendar time paired with --every day masks.
    #[arg(long, conflicts_with_all = ["cron", "in_after"])]
    at: Option<String>,
    /// Repeat cadence: `15m`, `day`, `weekday`, or `mon,wed,fri`.
    #[arg(long, conflicts_with_all = ["cron", "in_after"])]
    every: Option<String>,
    /// Raw 5-field cron expression.
    #[arg(long, conflicts_with_all = ["at", "every", "in_after"])]
    cron: Option<String>,
    /// Fire once after a duration such as `30m`; resolves in the configured timezone.
    #[arg(long = "in", value_name = "DUR", conflicts_with_all = ["at", "every", "cron"])]
    in_after: Option<String>,
    /// Project root whose room hosts the task; resolved to an absolute root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Write the task to the project's `.rimz/config.toml` instead of per-machine loop.toml.
    #[arg(long)]
    project: bool,
    /// Optional channel/worktree to host the transient task pane.
    #[arg(long)]
    worktree: Option<String>,
    /// Permission posture for the supervised turn: auto, ask, or yolo.
    #[arg(long)]
    mode: Option<String>,
    /// Reasoning effort for the launched agent.
    #[arg(long)]
    effort: Option<String>,
    /// Dollar cap for each spawned agent run.
    #[arg(long, value_name = "AMOUNT[/day]")]
    budget: Option<String>,
    /// Skip a fire once this task's local-day run spend reaches the amount.
    #[arg(long = "budget-per-day", value_name = "AMOUNT", requires = "budget")]
    budget_per_day: Option<String>,
    /// Fire only while the provider's longest budget window holds this forward headroom, e.g. 1.5x.
    #[arg(long, value_name = "RATIO")]
    surplus: Option<String>,
    /// Fire only once this much of the provider's longest budget window has elapsed, e.g. 3d.
    #[arg(long = "surplus-after", value_name = "DUR")]
    surplus_after: Option<String>,
    /// Replace the agent's base system prompt with a file's contents.
    #[arg(long = "system-prompt-file", value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,
    /// Wait cap for the supervised turn.
    #[arg(long)]
    timeout: Option<String>,
}

#[derive(Debug, Args)]
struct NameArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    #[arg(
        required_unless_present = "all",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::loop_tasks)
    )]
    name: Option<String>,
    /// Apply to every task listed here: machine, state, and this project.
    #[arg(long, conflicts_with = "name")]
    all: bool,
}

#[derive(Debug, Args)]
struct PauseArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Lift the pause automatically after a duration such as `2h`; use disable for indefinite holds.
    #[arg(long = "for", value_name = "DUR", required = true)]
    pause_for: String,
}

#[derive(Debug, Args)]
struct FireArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Leave the transient run pane open for inspection.
    #[arg(long)]
    keep: bool,
}

#[derive(Debug, Args)]
struct RenameArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    new_name: String,
}

#[derive(Debug, Args)]
struct ShowArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Number of recent run rows to show; consecutive identical runs collapse into one.
    #[arg(short = 'n', long = "runs", default_value_t = 10)]
    runs: usize,
}

#[derive(Debug, Args)]
struct LogsArgs {
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Number of recent runs to print.
    #[arg(short = 'n', long = "runs", default_value_t = 10)]
    runs: usize,
    /// Print only runs that failed.
    #[arg(long)]
    failed: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    /// Lock the rimzd dashboard in place by ignoring quit keys.
    #[arg(long, hide = true)]
    hold: bool,
}

pub fn run(args: LoopArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        LoopSubcmd::Add(args) => add::add(*args, globals),
        LoopSubcmd::Remove(args) => add::remove(&args.name, globals),
        LoopSubcmd::Rename(args) => add::rename(&args.name, &args.new_name, globals),
        LoopSubcmd::Pause(args) => add::pause(args, globals),
        LoopSubcmd::Enable(args) => add::enable(args, globals),
        LoopSubcmd::Disable(args) => add::disable(args, globals),
        LoopSubcmd::Stop(args) => stop::stop(&args.name, globals),
        LoopSubcmd::List => render::list(globals),
        LoopSubcmd::Watch(args) => watch::watch(args, globals),
        LoopSubcmd::Show(args) => render::show(args, globals),
        LoopSubcmd::Logs(args) => render::logs(args, globals),
        LoopSubcmd::Fire(args) => {
            run_tasks::run_one(&args.name, LoopRunMode::Manual, args.keep, globals)
        }
        LoopSubcmd::Run(args) => {
            run_tasks::run_one(&args.name, LoopRunMode::Scheduled, false, globals)
        }
    }
}

// ---- add / remove -----------------------------------------------------------

fn task_subject(task: &LoadedTask) -> String {
    task.action()
        .map(|action| action.subject().to_owned())
        .unwrap_or_else(|_| "<invalid>".to_owned())
}

fn project_config_path(project_root: &Path) -> PathBuf {
    schedule::config_edit::TaskStore::Project(project_root).path()
}

fn project_root_for_globals(globals: &GlobalFlags) -> Option<PathBuf> {
    let workspace = match WorkspaceResolver::resolve(".", globals.root.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::debug!(error = %err, "loop command using machine-only tasks");
            return None;
        }
    };
    Some(workspace.project_root)
}

fn task_catalog(globals: &GlobalFlags) -> Result<TaskCatalog> {
    let project_root = project_root_for_globals(globals);
    TaskCatalog::load(project_root.as_deref())
}

fn load_task(name: &str, globals: &GlobalFlags) -> Result<Option<LoadedTask>> {
    Ok(task_catalog(globals)?.visible().get(name).cloned())
}

fn task_key(name: &str, task: &LoadedTask) -> String {
    TaskKey::for_task(name, task.source(), &task.entry().resolved_root())
}

fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    RuntimePaths::for_workspace(WorkspaceId::from_project_root(root)).ok()
}

fn observe_task_timing(
    name: &str,
    task: &LoadedTask,
    stamps: &BTreeMap<String, Timestamp>,
    arming: Option<&Arming>,
    now_zoned: &jiff::Zoned,
) -> schedule::TaskTiming {
    let last_fire = stamps.get(name).copied();
    schedule::TaskTiming::evaluate(task.schedule(), task.source(), last_fire, arming, now_zoned)
}

fn task_next_fire_text(
    name: &str,
    task: &LoadedTask,
    arming: Option<&Arming>,
    now: &jiff::Zoned,
) -> Option<String> {
    let runtime = runtime_for_root(&task.entry().resolved_root())?;
    let stamps = schedule::last_stamps(&runtime);
    observe_task_timing(name, task, &stamps, arming, now)
        .next_timestamp()
        .map(|next| ui::rel_until(next, now.timestamp()))
}

fn finish_project_mutation(
    out: &mut impl Write,
    project_root: &Path,
    task_added: bool,
    pre_state: TrustState,
) -> Result<()> {
    if crate::cli::trust::regrant_own_mutation(project_root, pre_state)? {
        if task_added {
            writeln!(out, "trust: granted — task enabled and fires on schedule")?;
        } else {
            writeln!(out, "trust: kept")?;
        }
        return Ok(());
    }
    if std::io::stdin().is_terminal()
        && crate::cli::trust::offer_inline_grant(project_root, "grant trust now?")?
    {
        if task_added {
            writeln!(out, "trust: granted — task enabled and fires on schedule")?;
        }
        return Ok(());
    }
    let state = trust::status(project_root)
        .context("reading trust state after project task change")?
        .state;
    writeln!(
        out,
        "trust: {} — project tasks stay inert until you run `rimz trust grant` (review with `rimz trust`)",
        state.as_str()
    )?;
    Ok(())
}

fn block_untrusted_project_task(name: &str, entry: &TaskEntry, source: TaskSource) -> Result<()> {
    let Some(state) = source.blocked_state() else {
        return Ok(());
    };
    bail!(
        "loop task `{name}` is blocked — project trust is {state}\nconfigured in {path}\n{fix}",
        path = project_config_path(&entry.root).display(),
        state = state.as_str(),
        fix = trust::blocked_fix(state),
    )
}

fn parse_check_on(raw: &str) -> Result<CheckOn> {
    match raw.trim() {
        "fail" => Ok(CheckOn::Fail),
        "success" => Ok(CheckOn::Success),
        other => bail!("unknown loop check polarity `{other}`; use fail or success"),
    }
}

fn pause_until_text(until: Timestamp, now: Timestamp) -> String {
    let local = until.to_zoned(MachineConfig::load_lenient().time_zone());
    format!(
        "{} ({})",
        ui::rel_until(until, now),
        local.strftime("%a %H:%M")
    )
}
