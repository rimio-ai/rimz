//! `rimz loop` — trigger wake-ups and command checks from schedules or signals.
//!
//! The elected sidebar elder keeps time while a room for the task's project is
//! open. An opt-in OS timer invokes the same scheduler for roots without one.
//! Both fire `rimz loop run <name>`, which runs an optional shell check and then
//! drives one configured prompt through either the supervised `agents -p` seam
//! or the message path to a pinned live session.
//!
//! This handler parses commands, lists room-open and next-fire state, inspects
//! run history, executes prepared supervised-run or message effects, and owns
//! terminal presentation. [`rimz::harness::schedule::runner::TaskFire`] owns the
//! hidden runner policy and its exactly-one history transition. Pure trigger
//! parsing and due evaluation live in [`rimz::harness::schedule`]; delivery mode
//! reuses the shared message seam, and ephemeral self-waits live in
//! [`rimz::harness::schedule::instances`].

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::config::{AgentCheck, CheckOn, MachineConfig, TaskCheck, TaskEntry, TaskTarget};
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::plan::{ResolvedSingleAgentLaunch, resolve_single_agent_launch};
use rimz::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunTransition,
};
use rimz::harness::schedule::runner::{
    AfterReset, CheckEcho, InFlightRun, RunLockInfo, RunLockState, RunLocks,
    SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL, after_reset as resolve_after_reset, in_flight_run,
    parse_mode, parse_task_timeout, preflight_entry, task_login, window_condition_provider,
};
use rimz::harness::schedule::throttle::Held;
use rimz::harness::schedule::{
    self, TaskAction, TaskActionKind,
    arming::{self, ArmState, Arming, DisabledReason},
    catalog::{LoadedTask, TaskCatalog, TaskSource},
    strikes,
};
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::message::DeliveryGate;
use rimz::trust::{self, TrustState};
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

mod add;
mod condition;
mod list;
mod render;
mod run_report;
#[path = "run.rs"]
mod run_tasks;
mod stop;
mod timer;
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
    /// Remove tasks from their stores.
    Remove(NamesArgs),
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
    List(ListArgs),
    /// Hold a live loop dashboard open and repaint countdowns.
    Watch(WatchArgs),
    /// Show one task's trigger, next fire, and recent run forensics.
    Show(ShowArgs),
    /// Print full forensics for a task's recent runs.
    Logs(LogsArgs),
    /// Fire one task now in the foreground for testing; one-shots and subscriptions stay put.
    Fire(FireArgs),
    /// Run one task now. The sidebar elder calls this; humans rarely do.
    #[command(hide = true)]
    Run(RunArgs),
    /// Execute a check without inheriting its caller's controlling terminal.
    #[command(hide = true)]
    CheckExec {
        #[arg(long, allow_hyphen_values = true)]
        command: String,
    },
    /// Run one scheduler pass for roots without an open room.
    #[command(hide = true)]
    Tick,
    /// Manage the machine-wide loop timer.
    Timer(TimerArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Show tasks in every known room.
    #[arg(long)]
    all: bool,
    /// Print structured, uncollapsed task rows.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TimerArgs {
    #[command(subcommand)]
    command: TimerSubcmd,
}

#[derive(Debug, Subcommand)]
enum TimerSubcmd {
    /// Install and start the one-minute user timer.
    Install,
    /// Show whether the timer is installed and active.
    Status,
    /// Stop and remove the user timer.
    Remove,
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Task name (letters, digits, `-`, `_`).
    name: String,
    /// Short description, one line of 1–60 characters after trimming.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true, value_parser = super::parse_task_label)]
    label: Option<String>,
    /// Kind, profile, or virtual cell; with --stay, an ordinary agent layout.
    #[arg(
        long,
        conflicts_with = "wait",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    agent: Option<String>,
    /// Leave the agent layout running as ordinary room agents.
    #[arg(long)]
    stay: bool,
    /// Evaluate a condition in each RimZ-owned worktree.
    #[arg(long)]
    each_worktree: bool,
    /// Arm a standing signal subscription for the layout's prompt leader.
    #[arg(long, value_name = "SIGNAL")]
    subscribe: Vec<String>,
    /// Take over the checkout: stop its idle agents first, wait while any is busy.
    #[arg(long)]
    takeover: bool,
    /// Live agent to wake through the message path; resolved and pinned now.
    #[arg(
        long,
        value_name = "ADDRESS",
        num_args = 0..=1,
        default_missing_value = "@me",
        conflicts_with = "agent",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::handles)
    )]
    wait: Option<String>,
    /// Inline prompt for the triggered turn.
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,
    /// File whose contents are used as the triggered prompt.
    #[arg(long = "prompt-file", value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Shell command to run before any agent action.
    #[arg(long, value_name = "CMD")]
    check: Option<String>,
    /// Profile to run as a headless guard before the task's action.
    #[arg(long, value_name = "PROFILE")]
    check_agent: Option<String>,
    /// Inline question for the headless guard.
    #[arg(long, value_name = "TEXT")]
    check_prompt: Option<String>,
    /// File containing the headless guard's question.
    #[arg(long, value_name = "PATH")]
    check_prompt_file: Option<PathBuf>,
    /// Ask again this long after a resident guard declines.
    #[arg(long, value_name = "DUR")]
    check_recheck: Option<String>,
    /// Headless guard's timeout; default 5m, independent of --timeout.
    #[arg(long, value_name = "DUR")]
    check_timeout: Option<String>,
    /// Shell command that must pass before a spawned agent task is complete.
    #[arg(long, value_name = "CMD", requires = "agent")]
    verify: Option<String>,
    /// Total agent turns allowed while making --verify pass.
    #[arg(long, value_name = "N", requires = "verify")]
    max_attempts: Option<u32>,
    /// Auto-disable after N consecutive failed fires; default 3, 0 disables.
    #[arg(long, value_name = "N")]
    max_strikes: Option<u32>,
    /// Guard polarity for either check: fail, success, or any outcome.
    #[arg(long, value_name = "fail|success|any")]
    on: Option<String>,
    /// Poll-until deadline as a duration such as `30m`; resolves at add time.
    #[arg(long, value_name = "DUR")]
    until: Option<String>,
    /// One-shot firing time, or calendar time paired with --every day masks.
    #[arg(long, conflicts_with_all = ["cron", "in_after", "signal"])]
    at: Option<String>,
    /// Repeat cadence: `15m`, `day`, `weekday`, or `mon,wed,fri`.
    #[arg(long, conflicts_with_all = ["cron", "in_after", "signal"])]
    every: Option<String>,
    /// Raw 5-field cron expression.
    #[arg(long, conflicts_with_all = ["at", "every", "in_after", "signal"])]
    cron: Option<String>,
    /// Fire once after a duration such as `30m`; resolves in the configured timezone.
    #[arg(
        long = "in",
        value_name = "DUR",
        conflicts_with_all = ["at", "every", "cron", "signal"]
    )]
    in_after: Option<String>,
    /// Fire once after the next reset of the task provider's 5h or 7d window.
    #[arg(
        long = "after-reset",
        value_name = "5h|7d",
        conflicts_with_all = ["at", "every", "cron", "in_after", "signal", "when", "until"]
    )]
    after_reset: Option<rimz::agents::WindowSpan>,
    /// Fire when a signal with this name is emitted.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["at", "every", "cron", "in_after"]
    )]
    signal: Option<String>,
    /// Fire while a state expression holds; repeated clauses are ANDed.
    #[arg(long, value_name = "EXPR", conflicts_with_all = ["at", "every", "cron", "in_after", "signal", "until"])]
    when: Vec<String>,
    /// Require the whole condition to hold continuously for this duration.
    #[arg(long = "for", value_name = "DUR", requires = "when")]
    hold: Option<String>,
    /// Require a top-level signal payload field to equal this value.
    #[arg(long = "match", value_name = "KEY=VALUE", requires = "signal")]
    matches: Vec<String>,
    /// Remove a signal or condition task after its first fire.
    #[arg(long)]
    once: bool,
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
    /// Named account of the agent's provider that every fire runs on; unset follows the room.
    #[arg(long, value_name = "NAME")]
    account: Option<rimz::ids::LoginName>,
    /// Whether the task's agent starts take their turn in the loop throttle; unset is on.
    #[arg(long, value_name = "on|off", value_parser = ["on", "off"], requires = "agent")]
    throttle: Option<String>,
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
struct NamesArgs {
    #[arg(required = true, num_args = 1.., value_name = "NAME", add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    names: Vec<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    name: String,
    #[arg(long, hide = true)]
    cwd: Option<PathBuf>,
    #[arg(long, hide = true)]
    signal_json: Option<String>,
    #[arg(long, hide = true, conflicts_with = "signal_json")]
    condition_json: Option<String>,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    #[arg(
        required_unless_present = "all",
        num_args = 1..,
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::loop_tasks)
    )]
    names: Vec<String>,
    /// Apply to all machine tasks and this project's state and project tasks.
    #[arg(long, conflicts_with = "names")]
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
    /// Print the task, worktree conditions, launch ledger and run history as JSON.
    #[arg(long)]
    json: bool,
    /// Show every owned worktree, including waiting checkouts and ended leaders.
    #[arg(long)]
    all: bool,
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Number of recent run rows to show, newest first; consecutive identical runs collapse into one row and an overlapped fire folds into the run that follows it.
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
        LoopSubcmd::CheckExec { command } => {
            use std::os::unix::process::CommandExt;

            nix::unistd::setsid().context("detaching loop check from the caller's terminal")?;
            Err(std::process::Command::new("sh")
                .args(["-c", &command])
                .exec()
                .into())
        }
        LoopSubcmd::Add(args) => add::add(*args, globals),
        LoopSubcmd::Remove(args) => args
            .names
            .iter()
            .try_for_each(|name| add::remove(name, globals)),
        LoopSubcmd::Rename(args) => add::rename(&args.name, &args.new_name, globals),
        LoopSubcmd::Pause(args) => add::pause(args, globals),
        LoopSubcmd::Enable(args) => add::enable(args, globals),
        LoopSubcmd::Disable(args) => add::disable(args, globals),
        LoopSubcmd::Stop(args) => stop::stop(&args.name, globals),
        LoopSubcmd::List(args) => list::run(args, globals),
        LoopSubcmd::Watch(args) => watch::watch(args, globals),
        LoopSubcmd::Show(args) => render::show(args, globals),
        LoopSubcmd::Logs(args) => render::logs(args, globals),
        LoopSubcmd::Fire(args) => run_tasks::run_one(
            &args.name,
            LoopRunMode::Manual,
            args.keep,
            None,
            None,
            None,
            globals,
        ),
        LoopSubcmd::Run(args) => {
            let signal = args
                .signal_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("decoding loop trigger signal")?;
            let condition = args
                .condition_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("decoding loop condition evidence")?;
            run_tasks::run_one(
                &args.name,
                LoopRunMode::Scheduled,
                false,
                signal,
                condition,
                args.cwd,
                globals,
            )
        }
        LoopSubcmd::Tick => timer::tick(),
        LoopSubcmd::Timer(args) => timer::run(args.command),
    }
}

// ---- add / remove -----------------------------------------------------------

fn project_config_path(project_root: &Path) -> PathBuf {
    schedule::catalog::project_config_path(project_root)
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

/// The caller's project root and the run in flight there for `name`, a task with
/// no row: a fired one-shot whose row its run consumed.
fn in_flight_without_row(
    name: &str,
    globals: &GlobalFlags,
) -> Result<Option<(PathBuf, InFlightRun)>> {
    let Some(root) = project_root_for_globals(globals) else {
        return Ok(None);
    };
    Ok(in_flight_run(name, &root)?.map(|in_flight| (root, in_flight)))
}

fn task_catalog(globals: &GlobalFlags) -> Result<TaskCatalog> {
    let project_root = project_root_for_globals(globals);
    TaskCatalog::load(project_root.as_deref())
}

fn load_task(name: &str, globals: &GlobalFlags) -> Result<Option<LoadedTask>> {
    Ok(task_catalog(globals)?.visible().get(name).cloned())
}

fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    let state = StatePaths::for_project_root(root).ok()?;
    RuntimePaths::for_state(&state).ok()
}

fn observe_task_timing(
    name: &str,
    task: &LoadedTask,
    stamps: &BTreeMap<String, Timestamp>,
    arming: Option<&Arming>,
    now_zoned: &jiff::Zoned,
) -> schedule::TaskTiming {
    let last_fire = stamps.get(name).copied();
    let timing =
        schedule::TaskTiming::evaluate(task.trigger(), task.source(), last_fire, arming, now_zoned);
    condition::observe(name, task.entry(), timing, now_zoned.timestamp())
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
            writeln!(out, "trust: granted — task enabled and ready to fire")?;
        } else {
            writeln!(out, "trust: kept")?;
        }
        return Ok(());
    }
    if std::io::stdin().is_terminal()
        && crate::cli::trust::offer_inline_grant(project_root, "grant trust now?")?
    {
        if task_added {
            writeln!(out, "trust: granted — task enabled and ready to fire")?;
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
        "any" => Ok(CheckOn::Any),
        other => bail!("unknown loop check polarity `{other}`; use fail, success, or any"),
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
