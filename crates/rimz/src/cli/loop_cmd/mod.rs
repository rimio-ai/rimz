//! `rimz loop` — schedule wake-ups and command checks from the room's sidebar elder.
//!
//! The elected sidebar elder keeps time while a room for the task's project is
//! open and fires `rimz loop run <name>`, which runs an optional shell check and
//! then drives one configured prompt through either the supervised `agents -p`
//! seam or the message path to a pinned live session. A `<kind>-ping` virtual
//! cell is the window-priming special case and gets the budget-window skip
//! optimization.
//!
//! This handler parses commands, lists room-open and next-fire state, inspects
//! run history, and owns the hidden runner the elder spawns. The runner appends
//! exactly one history record after loading a task:
//! the pure executor returns an outcome, and this wrapper records success,
//! failure, or error with capped forensics.
//! Pure schedule parsing and due evaluation live in [`rimz::harness::schedule`];
//! delivery mode reuses the shared message seam, and ephemeral self-wakes live
//! in [`rimz::harness::schedule::instances`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use rimz::agents::{HookPreflightErr, TurnLifecycleNeed, find_adapter, preflight_hooks};
use rimz::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use rimz::harness::run::PermissionMode;
use rimz::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunRecord, LoopRunResult,
};
use rimz::harness::schedule::runner::{
    CHECK_DEFAULT_TIMEOUT, CheckEcho, acquire_run_lock, augment_prompt, check_only_result,
    check_record, check_timeout, deadline_expired, polarity_fires, reset_window_already_running,
    run_check, surplus_gate, tail_output, window_already_running, window_reset_at,
};
use rimz::harness::schedule::{
    self,
    instances::{self, TaskSource},
    pauses::{self, PauseEntry},
    strikes,
};
use rimz::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use rimz::ids::WorkspaceId;
use rimz::message::DeliveryGate;
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::paths::{RuntimePaths, StatePaths, config_home, state_home};
use rimz::trust::{self, TrustState};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

mod add;
mod render;
#[path = "run.rs"]
mod run_tasks;

pub(crate) use run_tasks::reap_dead_delivery_schedules;

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
    /// Pause a task until it is resumed or the optional duration elapses.
    Pause(PauseArgs),
    /// Resume a paused task without replaying missed fires.
    Resume(NameArgs),
    /// List configured tasks and whether their room is open.
    List,
    /// Hold a live loop dashboard open and repaint countdowns.
    Watch(WatchArgs),
    /// Show one task's schedule, next fire, and recent run forensics.
    Show(ShowArgs),
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
    /// Auto-pause after N consecutive failed fires; default 3, 0 disables.
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
    /// Repeat cadence: `15m`, `day`, `weekday`, `mon,wed,fri`, or `reset`.
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
struct PauseArgs {
    name: String,
    /// Resume automatically after a duration such as `2h`.
    #[arg(long = "for", value_name = "DUR")]
    pause_for: Option<String>,
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
        LoopSubcmd::Resume(args) => add::resume(&args.name, globals),
        LoopSubcmd::List => render::list(globals),
        LoopSubcmd::Watch(args) => render::watch(args, globals),
        LoopSubcmd::Show(args) => render::show(args, globals),
        LoopSubcmd::Fire(args) => {
            run_tasks::run_one(&args.name, LoopRunMode::Manual, args.keep, globals)
        }
        LoopSubcmd::Run(args) => {
            run_tasks::run_one(&args.name, LoopRunMode::Scheduled, false, globals)
        }
    }
}

// ---- add / remove -----------------------------------------------------------

struct ResolvedTaskSpec {
    kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskAction<'a> {
    Spawn(&'a str),
    Deliver(&'a TaskTarget),
    CheckOnly,
}

fn task_action<'a>(name: &str, entry: &'a TaskEntry) -> Result<TaskAction<'a>> {
    if entry.verify.is_some() && entry.agent.is_none() {
        bail!(
            "loop task `{name}` sets `verify` without `agent`; verification needs a supervised agent run"
        );
    }
    if entry.max_attempts.is_some() && entry.verify.is_none() {
        bail!("loop task `{name}` sets `max-attempts` without `verify`");
    }
    if entry.max_attempts == Some(0) {
        bail!("loop task `{name}` sets `max-attempts` to 0; use at least 1");
    }
    match (entry.agent.as_deref(), entry.wake.as_ref()) {
        (Some(agent), None) if !agent.trim().is_empty() => Ok(TaskAction::Spawn(agent)),
        (None, Some(target)) => Ok(TaskAction::Deliver(target)),
        (None, None) if entry.check.is_some() => Ok(TaskAction::CheckOnly),
        (Some(_), Some(_)) => {
            bail!("loop task `{name}` sets both `agent` and `wake`; keep exactly one")
        }
        _ => bail!("loop task `{name}` needs `agent`, `wake`, or `check`"),
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
        .agent
        .clone()
        .or_else(|| entry.wake.as_ref().map(|target| target.handle.clone()))
        .or_else(|| entry.check.as_ref().map(|_| "check".to_owned()))
        .unwrap_or_else(|| "<invalid>".to_owned())
}

fn resolve_task_spec(spec: &str, workspace: &rimz::ResolvedWorkspace) -> Result<ResolvedTaskSpec> {
    let machine_config = super::machine_config();
    let launch = rimz::config::effective::load(
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
    preflight_kind(&resolved.kind)?;
    Ok(())
}

fn preflight_kind(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    match preflight_hooks(adapter, TurnLifecycleNeed::NotUnsupported) {
        Ok(()) => Ok(()),
        Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => bail!(
            "{kind} cannot run as a scheduled turn: a verified executable turn-lifecycle signal is required; {reason}"
        ),
        Err(HookPreflightErr::HooksMissing) => bail!(
            "{kind} hooks are not installed, so a scheduled turn cannot report completion\ninstall them with `rimz hooks install {kind}`"
        ),
        Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => bail!(
            "{kind} hooks are installed but not trusted ({}), so a scheduled turn cannot report completion\n{}",
            hooks,
            fix
        ),
    }
}

fn remove_loaded_task(name: &str, entry: &TaskEntry, source: TaskSource) -> Result<bool> {
    match source {
        TaskSource::Config => {
            schedule::config_edit::remove(schedule::config_edit::TaskStore::Machine, name)
        }
        TaskSource::Instance => instances::remove(name).map_err(Into::into),
        TaskSource::Project { .. } => schedule::config_edit::remove(
            schedule::config_edit::TaskStore::Project(&entry.root),
            name,
        ),
    }
}

fn project_config_path(project_root: &Path) -> PathBuf {
    schedule::config_edit::TaskStore::Project(project_root).path()
}

fn project_tasks_for_root(
    project_root: &Path,
) -> Result<Option<rimz::config::effective::ProjectTasks>> {
    rimz::config::effective::project_tasks(project_root, &config_home()).map_err(Into::into)
}

fn project_effective_merge(
    project: &Option<rimz::config::effective::ProjectTasks>,
) -> Option<(rimz::config::Tasks, TrustState)> {
    let project = project.as_ref()?;
    (project.state == TrustState::Trusted).then(|| (project.tasks.clone(), project.state))
}

fn project_visible_merge(
    project: &Option<rimz::config::effective::ProjectTasks>,
) -> Option<(rimz::config::Tasks, TrustState)> {
    project
        .as_ref()
        .map(|project| (project.tasks.clone(), project.state))
}

fn project_tasks_for_globals(
    globals: &GlobalFlags,
) -> Result<Option<rimz::config::effective::ProjectTasks>> {
    let workspace = match WorkspaceResolver::resolve(".", globals.root.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::debug!(error = %err, "loop command using machine-only tasks");
            return Ok(None);
        }
    };
    project_tasks_for_root(&workspace.project_root)
}

pub(super) fn load_all_tasks(
    globals: &GlobalFlags,
) -> Result<BTreeMap<String, (TaskEntry, TaskSource)>> {
    validate_machine_loop_stores()?;
    let project = project_tasks_for_globals(globals)?;
    Ok(instances::load_all_visible_with_project(
        project_visible_merge(&project),
    ))
}

pub(crate) fn prune_orphan_pauses(globals: &GlobalFlags) -> Result<usize> {
    let known: BTreeSet<_> = load_all_tasks(globals)?.into_keys().collect();
    let pauses = pauses::prune_orphans(&known)?;
    let strikes = strikes::prune_orphans(&known)?;
    Ok(pauses + strikes)
}

fn load_task(name: &str, globals: &GlobalFlags) -> Result<Option<(TaskEntry, TaskSource)>> {
    validate_machine_loop_stores()?;
    let project = project_tasks_for_globals(globals)?;
    Ok(instances::load_entry_visible_with_project(
        name,
        project_visible_merge(&project),
    ))
}

fn load_runnable_task(
    name: &str,
    globals: &GlobalFlags,
) -> Result<Option<(TaskEntry, TaskSource)>> {
    validate_machine_loop_stores()?;
    let project = project_tasks_for_globals(globals)?;
    if let Some(task) = instances::load_entry_with_project(name, project_effective_merge(&project))
    {
        return Ok(Some(task));
    }
    let Some(project) = project else {
        return Ok(None);
    };
    if project.state == TrustState::Trusted {
        return Ok(None);
    }
    Ok(project.tasks.0.get(name).cloned().map(|entry| {
        (
            entry,
            TaskSource::Project {
                state: project.state,
            },
        )
    }))
}

fn validate_machine_loop_stores() -> Result<()> {
    MachineConfig::load_loop().context("reading per-machine loop.toml")?;
    let path = instances::path(&state_home());
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice::<rimz::config::Tasks>(&bytes)
                .with_context(|| format!("reading {}", path.display()))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    }
    Ok(())
}

fn finish_project_mutation(
    out: &mut impl Write,
    project_root: &Path,
    task_added: bool,
) -> Result<()> {
    if std::io::stdin().is_terminal()
        && crate::cli::trust::offer_inline_grant(project_root, "grant trust now?")?
    {
        if task_added {
            writeln!(out, "trust: granted — task fires on schedule")?;
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
    let TaskSource::Project { state } = source else {
        return Ok(());
    };
    if state == TrustState::Trusted {
        return Ok(());
    }
    bail!(
        "loop task `{name}` is blocked — project trust is {state}\nconfigured in {path}\n{fix}",
        path = project_config_path(&entry.root).display(),
        state = state.as_str(),
        fix = trust::blocked_fix(state),
    )
}

fn parse_mode(raw: &str) -> Result<String> {
    Ok(mode_name(parse_mode_value(raw)?).to_owned())
}

fn parse_check_on(raw: &str) -> Result<CheckOn> {
    match raw.trim() {
        "fail" => Ok(CheckOn::Fail),
        "success" => Ok(CheckOn::Success),
        other => bail!("unknown loop check polarity `{other}`; use fail or success"),
    }
}

fn parse_mode_value(raw: &str) -> Result<PermissionMode> {
    let trimmed = raw.trim();
    match PermissionMode::from_str(trimmed) {
        Ok(PermissionMode::Plan) | Err(_) => {
            bail!("unknown loop mode `{trimmed}`; use auto, ask, or yolo")
        }
        Ok(mode) => Ok(mode),
    }
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
        PermissionMode::Yolo => "yolo",
        PermissionMode::Plan => {
            unreachable!("loop mode parser rejects plan")
        }
    }
}

fn parse_task_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

fn pause_until_text(until: Timestamp, now: Timestamp) -> String {
    let local = until.to_zoned(MachineConfig::load_lenient().time_zone());
    format!(
        "{} ({})",
        ui::rel_until(until, now),
        local.strftime("%a %H:%M")
    )
}

fn resolve_task_prompt(name: &str, entry: &TaskEntry) -> Result<String> {
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
            name
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
