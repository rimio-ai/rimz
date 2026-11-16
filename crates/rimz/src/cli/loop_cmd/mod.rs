//! `rimz loop` — trigger wake-ups and command checks from schedules or signals.
//!
//! The room host keeps time while a room for the task's project is
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

use rimz::config::{MachineConfig, TaskEntry, TaskTarget};
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult, ProcessExit,
    RunStage, RunTransition,
};
use rimz::harness::schedule::runner::{
    AfterReset, InFlightRun, RunLockInfo, RunLockState, RunLocks,
    SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL, after_reset as resolve_after_reset, in_flight_run,
    parse_task_timeout, window_condition_provider,
};
use rimz::harness::schedule::{
    self, TaskAction, TaskActionKind,
    arming::{self, ArmState, Arming, DisabledReason},
    catalog::{LoadedTask, TaskCatalog, TaskSource},
    strikes,
};
use rimz::harness::task_login::task_login;
use rimz::harness::throttle::Held;
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::message::DeliveryGate;
use rimz::trust::{self, TrustState};
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;
use super::wake_view::runtime_for_root;

mod add;
mod condition;
mod explain;
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
    /// Add a task, or replace the task of the same name.
    Add(Box<AddArgs>),
    /// Delete tasks by name; their run history stays.
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
    /// Evaluate condition expressions without adding a task.
    Explain(ExplainArgs),
    /// Print full forensics for a task's recent runs.
    Logs(LogsArgs),
    /// Fire one task now, outside its schedule; a one-shot or subscription stays in place.
    Fire(FireArgs),
    /// Run one task now. The sidebar host calls this; humans rarely do.
    #[command(hide = true)]
    Run(RunArgs),
    /// Execute a check without inheriting its caller's controlling terminal.
    #[command(hide = true)]
    CheckExec {
        #[arg(long, allow_hyphen_values = true, required_unless_present = "argv")]
        command: Option<String>,
        /// The program and its arguments, run without a shell.
        #[arg(last = true, conflicts_with = "command")]
        argv: Vec<std::ffi::OsString>,
    },
    /// Run one scheduler pass for roots without an open room.
    #[command(hide = true)]
    Tick,
    /// Manage the machine-wide loop timer.
    Timer(TimerArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Show every known room and unfold older one-offs and wake-ups.
    #[arg(long)]
    all: bool,
    /// Print structured, uncollapsed task rows.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ExplainArgs {
    /// Condition clauses, ANDed together.
    #[arg(required = true, num_args = 1.., value_name = "EXPR")]
    expressions: Vec<String>,
    /// Print structured readings and truth.
    #[arg(long)]
    json: bool,
    /// A path in the checkout whose board and forge readings to inspect; a linked checkout reads
    /// its own, as a wait armed there does.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Provider kind for window readings.
    #[arg(long)]
    provider: Option<String>,
    /// Provider account for window readings.
    #[arg(long)]
    account: Option<rimz::ids::LoginName>,
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
    /// Task name (letters, digits, `-`, `_`); generated as `loop-<petname>` when omitted.
    name: Option<String>,
    /// Short description: one line of 1 to 60 characters after trimming.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true, value_parser = super::parse_task_label)]
    label: Option<String>,
    /// Evaluate the trigger in each RimZ-owned worktree and fire there.
    #[arg(long)]
    each_worktree: bool,
    /// Auto-disable after N consecutive failed fires; default 3, 0 disables.
    #[arg(long, value_name = "N")]
    max_strikes: Option<u32>,
    /// Refused: fire on a condition with `--when` instead.
    #[arg(long, value_name = "DUR")]
    until: Option<String>,
    /// One-shot time (`HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`), or `HH:MM` with --every.
    #[arg(long, conflicts_with_all = ["cron", "in_after", "signal"])]
    at: Option<String>,
    /// Repeat cadence: `15m`, `day`, `weekday`, or `mon,wed,fri`.
    #[arg(long, conflicts_with_all = ["cron", "in_after", "signal"])]
    every: Option<String>,
    /// Five-field cron expression (names, steps, and `@daily`-style macros accepted).
    #[arg(long, conflicts_with_all = ["at", "every", "in_after", "signal"])]
    cron: Option<String>,
    /// Fire once after a duration such as `30m` or `3d`, rounded up to the next minute.
    #[arg(
        long = "in",
        value_name = "DUR",
        conflicts_with_all = ["at", "every", "cron", "signal"]
    )]
    in_after: Option<String>,
    /// Fire once after the next reset of the --provider's 5h or 7d window.
    #[arg(
        long = "after-reset",
        value_name = "5h|7d",
        requires = "provider",
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
    /// Fire when a state expression holds; repeated clauses are ANDed.
    #[arg(long, value_name = "EXPR", conflicts_with_all = ["at", "every", "cron", "in_after", "signal", "until"])]
    when: Vec<String>,
    /// Require the whole condition to hold continuously for this duration.
    #[arg(long = "for", value_name = "DUR", requires = "when")]
    hold: Option<String>,
    /// Require a top-level signal payload field to equal this value.
    #[arg(long = "match", value_name = "KEY=VALUE", requires = "signal")]
    matches: Vec<String>,
    /// Keep a --signal or --when task after a fire, so it fires again; without it, it fires once.
    #[arg(long)]
    repeat: bool,
    /// Project root whose room hosts the task; resolved to an absolute root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Write the task to the project's `.rimz/config.toml` instead of per-machine loop.toml.
    #[arg(long)]
    project: bool,
    /// Run each fire's command in a fresh worktree `<PREFIX>-<time>`, branched from trunk.
    #[arg(long, value_name = "PREFIX")]
    fresh_worktree: Option<String>,
    /// Provider whose usage windows a `window.*` condition or --after-reset reads.
    #[arg(long, value_name = "KIND")]
    provider: Option<String>,
    /// Named account of --provider whose windows the condition reads; unset follows the room.
    #[arg(long, value_name = "NAME")]
    account: Option<rimz::ids::LoginName>,
    /// Busy scheduled triggers: skip (default), coalesce, queue, or replace (stop for newest).
    #[arg(long, value_name = "POLICY", value_parser = ["skip", "coalesce", "queue", "replace"])]
    overlap: Option<String>,
    /// Maximum waiting triggers with --overlap queue; default 10, at least 1.
    #[arg(long, value_name = "N")]
    overlap_limit: Option<usize>,
    /// Cap for the command; a command that outlives it fails. Default `loop.default-timeout`,
    /// else 2h.
    #[arg(long, value_name = "DUR")]
    timeout: Option<String>,
    /// The command each fire runs, word for word with no shell: `-- sh -c '...'` for one.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct NameArgs {
    /// Task name, as `rimz loop list` prints it.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
}

#[derive(Debug, Args)]
struct NamesArgs {
    /// Task names, as `rimz loop list` prints them.
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
    #[arg(long, hide = true, conflicts_with_all = ["signal_json", "condition_json", "cwd"])]
    drain: bool,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    /// Task names, as `rimz loop list` prints them.
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
    /// Task name, as `rimz loop list` prints it.
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
    /// Task name, as `rimz loop list` prints it.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
}

#[derive(Debug, Args)]
struct RenameArgs {
    /// Task name, as `rimz loop list` prints it.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// The new name: letters, digits, `-`, and `_`.
    new_name: String,
}

#[derive(Debug, Args)]
struct ShowArgs {
    /// Print the task, worktree conditions, launch memory and run history as JSON.
    #[arg(long)]
    json: bool,
    /// Unfold the WORKTREES table's folded finished and gone workflow runs, one row each.
    #[arg(long)]
    all: bool,
    /// Task name, as `rimz loop list` prints it.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Number of recent run rows to show, newest first, after repeats fold into one row.
    #[arg(short = 'n', long = "runs", default_value_t = 10)]
    runs: usize,
}

#[derive(Debug, Args)]
struct LogsArgs {
    /// Task name, as `rimz loop list` prints it.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::loop_tasks
    ))]
    name: String,
    /// Number of recent blocks to print after folding and filtering.
    #[arg(short = 'n', long = "runs", default_value_t = 10)]
    runs: usize,
    /// Print only runs that failed.
    #[arg(long)]
    failed: bool,
    /// Include sibling signals heard without firing the task.
    #[arg(long)]
    heard: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    /// Lock the rimzd dashboard in place by ignoring quit keys.
    #[arg(long, hide = true)]
    hold: bool,
}

/// The start directory's workspace, or an empty-dir caller's checkout, which holds its lane.
///
/// The caller's record decides, not the start path: a symlinked RimZ home
/// resolves the empty dir to a path outside `empty_dir_root()`.
fn resolve_caller_workspace() -> Result<rimz::ResolvedWorkspace> {
    let workspace = WorkspaceResolver::resolve(".", None)?;
    let Ok(room) = WorkspaceResolver::resolve_participant(".", None) else {
        return Ok(workspace);
    };
    let Ok(Some(store)) = crate::cli::open_existing_store(&room) else {
        return Ok(workspace);
    };
    let Ok(snapshot) = store.snapshot_cached() else {
        return Ok(workspace);
    };
    let Ok(caller) = rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents) else {
        return Ok(workspace);
    };
    let Some(checkout) = caller.checkout.as_deref() else {
        return Ok(workspace);
    };
    WorkspaceResolver::resolve(checkout, None)
        .with_context(|| format!("resolving the caller's checkout `{checkout}`"))
}

/// Let a run report from any process send the strike-out notice.
pub(super) fn register_loop_notices() {
    rimz::harness::schedule::run_log::on_loop_disabled(run_tasks::notify_loop_disabled);
}

pub fn run(args: LoopArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        LoopSubcmd::CheckExec { command, argv } => {
            use std::os::unix::process::CommandExt;

            nix::unistd::setsid().context("detaching loop check from the caller's terminal")?;
            let mut child = match (&command, argv.split_first()) {
                (Some(command), _) => {
                    let mut child = std::process::Command::new("sh");
                    child.args(["-c", command]);
                    child
                }
                (None, Some((program, args))) => {
                    let mut child = std::process::Command::new(program);
                    child.args(args);
                    child
                }
                (None, None) => anyhow::bail!("check-exec needs --command or a program"),
            };
            Err(child.exec().into())
        }
        LoopSubcmd::Add(args) => add::add(*args),
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
        LoopSubcmd::Explain(args) => explain::run(args),
        LoopSubcmd::Logs(args) => render::logs(args, globals),
        LoopSubcmd::Fire(args) => run_tasks::run_one(
            RunArgs {
                name: args.name,
                cwd: None,
                signal_json: None,
                condition_json: None,
                drain: false,
            },
            LoopRunMode::Manual,
            globals,
        ),
        LoopSubcmd::Run(args) => run_tasks::run_one(args, LoopRunMode::Scheduled, globals),
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

fn stop_active_run(
    name: &str,
    root: &Path,
    expected: Option<RunLockInfo>,
    globals: &GlobalFlags,
) -> Result<(schedule::runner::StopOutcome, Option<String>)> {
    let mut pane_open = None;
    let outcome = schedule::runner::stop_task(name, root, expected, |workspace, paths, record| {
        let Some(record) = record else {
            return Ok(());
        };
        pane_open = stop_owned_run(name, workspace, paths, record, globals)?;
        Ok(())
    });
    let (outcome, transition) = match (outcome, &pane_open) {
        (Err(err), Some(open)) => return Err(err.context(open.clone())),
        (outcome, _) => outcome?,
    };
    run_tasks::handle_run_transition(name, root, transition);
    Ok((outcome, pane_open))
}

/// Stop a supervised run task `name` owns; a pane only the run's own stop can close is
/// returned for the caller to report once the rest of its stop is done.
fn stop_owned_run(
    name: &str,
    workspace: Option<&rimz::ResolvedWorkspace>,
    paths: StatePaths,
    record: &rimz::store::run::RunRecord,
    globals: &GlobalFlags,
) -> Result<Option<String>> {
    use crate::cli::supervised::StopRunErr;

    let id = paths.workspace_id.clone();
    let store = crate::cli::open_existing_store_at(paths)?
        .with_context(|| crate::cli::no_room(format_args!("for workspace {id}")))?;
    // A kept run's wrapper ends its provider on the cancel and holds the pane, as the
    // collection of the fire's ended run leaves it; only its own stop closes that pane.
    let Some(workspace) = workspace.filter(|_| !record.keep) else {
        crate::cli::supervised::cancel_supervised_run(&store, record)?;
        return Ok(None);
    };
    match crate::cli::supervised::stop_supervised_run(workspace, &store, globals, record) {
        // Cancellation succeeded; still finish the ladder before reporting
        // the pane which only the run's own stop can now close.
        Err(StopRunErr::PaneOpen(open)) => Ok(Some(format!(
            "loop `{name}`: {open}; run `rimz agents stop {}` to close it",
            record.run_id
        ))),
        Err(StopRunErr::NotCanceled(err) | StopRunErr::NotEnded(err)) => Err(err),
        Ok(()) => Ok(None),
    }
}

fn load_task(name: &str, globals: &GlobalFlags) -> Result<Option<LoadedTask>> {
    Ok(task_catalog(globals)?.visible().get(name).cloned())
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

fn pause_until_text(until: Timestamp, now: Timestamp) -> String {
    let local = until.to_zoned(MachineConfig::load_lenient().time_zone());
    format!(
        "{} ({})",
        ui::rel_until(until, now),
        local.strftime("%a %H:%M")
    )
}
