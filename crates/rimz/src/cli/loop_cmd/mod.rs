//! `rimz loop` — schedule wake-ups and command checks from the room's sidebar elder.
//!
//! The elected sidebar elder keeps time while a room for the task's project is
//! open and fires `rimz loop run <name>`, which runs an optional shell check and
//! then drives one configured prompt through either the supervised `agents -p`
//! seam or the message path to a pinned live session. A `<kind>-ping` virtual
//! cell is the window-priming special case and gets the budget-window skip
//! optimization.
//!
//! This handler parses and edits the per-machine config, lists room-open and
//! next-fire state, inspects run history, and owns the hidden runner the elder
//! spawns. The runner appends exactly one history record after loading a task:
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
use toml_edit::{DocumentMut, Item, Table, value};

use rimz::agents::{find_adapter, hook_trust_fix};
use rimz::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use rimz::harness::run::PermissionMode;
use rimz::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunRecord, LoopRunResult,
};
use rimz::harness::schedule::runner::{
    CHECK_DEFAULT_TIMEOUT, CheckEcho, acquire_run_lock, augment_prompt, check_only_result,
    check_record, check_timeout, deadline_expired, polarity_fires, reset_window_already_running,
    run_check, tail_output, window_already_running, window_reset_at,
};
use rimz::harness::schedule::{
    self,
    instances::{self, TaskSource},
    pauses::{self, PauseEntry},
};
use rimz::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use rimz::ids::WorkspaceId;
use rimz::message::DeliveryGate;
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::atomic::write_bytes_atomically;
use rimz::store::paths::{RuntimePaths, StatePaths, agents_home, config_home, state_home};
use rimz::trust::{self, TrustState};
use rimz::workspace::WorkspaceResolver;

use super::GlobalFlags;
use super::render as ui;

mod add;
mod render;
#[path = "run.rs"]
mod run_tasks;

pub(crate) use run_tasks::reap_dead_delivery_schedules;

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

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

pub fn run(args: LoopArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        LoopSubcmd::Add(args) => add::add(*args, globals),
        LoopSubcmd::Remove(args) => add::remove(&args.name, globals),
        LoopSubcmd::Rename(args) => add::rename(&args.name, &args.new_name, globals),
        LoopSubcmd::Pause(args) => add::pause(args, globals),
        LoopSubcmd::Resume(args) => add::resume(&args.name, globals),
        LoopSubcmd::List => render::list(globals),
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
    if !adapter.hooks_installed() {
        bail!(
            "{kind} hooks are not installed, so a scheduled turn cannot report completion\ninstall them with `rimz hooks install {kind}`"
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{kind} hooks are installed but not trusted ({}), so a scheduled turn cannot report completion\n{}",
            untrusted.join(", "),
            hook_trust_fix(kind)
        );
    }
    Ok(())
}

fn remove_task(name: &str, source: TaskSource) -> Result<bool> {
    match source {
        TaskSource::Config => config_remove(name),
        TaskSource::Instance => instances::remove(name).map_err(Into::into),
        TaskSource::Project { .. } => {
            bail!("internal error: project task `{name}` removal needs its project root")
        }
    }
}

fn remove_loaded_task(name: &str, entry: &TaskEntry, source: TaskSource) -> Result<bool> {
    match source {
        TaskSource::Config => config_remove(name),
        TaskSource::Instance => instances::remove(name).map_err(Into::into),
        TaskSource::Project { .. } => project_config_remove(&entry.root, name),
    }
}

fn project_config_path(project_root: &Path) -> PathBuf {
    project_root.join(PROJECT_CONFIG_REL)
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
    pauses::prune_orphans(&known).map_err(Into::into)
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

    root_tasks_table(&mut doc)?.insert(name, Item::Table(task_entry_table(entry, true)));

    let rendered = doc.to_string();
    MachineConfig::parse_text(&path, &rendered, &agents_home())
        .with_context(|| format!("validating `loop.tasks.{name}`"))?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn project_config_set_entry(project_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    let path = project_config_path(project_root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    root_tasks_table(&mut doc)?.insert(name, Item::Table(task_entry_table(entry, false)));

    let rendered = doc.to_string();
    validate_project_config_tasks(project_root, &path, &rendered, name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn task_entry_table(entry: &TaskEntry, include_root: bool) -> Table {
    let mut table = Table::new();
    if let Some(agent) = &entry.agent {
        table["agent"] = value(agent);
    }
    if let Some(target) = &entry.wake {
        table["wake"] = Item::Table(task_target_table(target));
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
    if let Some(verify) = &entry.verify {
        table["verify"] = value(verify);
    }
    if let Some(max_attempts) = entry.max_attempts {
        table["max-attempts"] = value(i64::from(max_attempts));
    }
    if let Some(on) = entry.on {
        table["on"] = value(match on {
            CheckOn::Fail => "fail",
            CheckOn::Success => "success",
        });
    }
    if include_root {
        table["root"] = value(entry.root.to_string_lossy().into_owned());
    }
    if let Some(worktree) = &entry.worktree {
        table["worktree"] = value(worktree);
    }
    if let Some(mode) = &entry.mode {
        table["mode"] = value(mode);
    }
    if let Some(effort) = &entry.effort {
        table["effort"] = value(effort);
    }
    if let Some(budget) = &entry.budget {
        table["budget"] = value(budget);
    }
    if let Some(budget) = &entry.budget_per_day {
        table["budget-per-day"] = value(budget);
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
    if let Some(every) = &entry.every {
        table["every"] = value(every);
    }
    if let Some(cron) = &entry.cron {
        table["cron"] = value(cron);
    }
    if let Some(deadline) = entry.deadline {
        table["deadline"] = value(deadline.to_string());
    }
    table
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

fn project_config_remove(project_root: &Path, name: &str) -> Result<bool> {
    let path = project_config_path(project_root);
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
        validate_project_config_tasks(project_root, &path, &rendered, name)?;
        write_bytes_atomically(&path, rendered.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

fn config_rename(name: &str, new_name: &str) -> Result<bool> {
    let path = MachineConfig::loop_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let Some(tasks) = doc.get_mut("tasks").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    if tasks.contains_key(new_name) {
        bail!("loop task `{new_name}` already exists");
    }
    let Some(entry) = tasks.remove(name) else {
        return Ok(false);
    };
    tasks.insert(new_name, entry);

    let rendered = doc.to_string();
    MachineConfig::parse_text(&path, &rendered, &agents_home())
        .with_context(|| format!("validating `loop.tasks.{new_name}`"))?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn project_config_rename(project_root: &Path, name: &str, new_name: &str) -> Result<bool> {
    let path = project_config_path(project_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let Some(tasks) = doc.get_mut("tasks").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    if tasks.contains_key(new_name) {
        bail!("loop task `{new_name}` already exists");
    }
    let Some(entry) = tasks.remove(name) else {
        return Ok(false);
    };
    tasks.insert(new_name, entry);

    let rendered = doc.to_string();
    validate_project_config_tasks(project_root, &path, &rendered, new_name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn validate_project_config_tasks(
    project_root: &Path,
    path: &Path,
    rendered: &str,
    name: &str,
) -> Result<()> {
    let value = toml::from_str::<toml::Value>(rendered)
        .with_context(|| format!("parsing {}", path.display()))?;
    rimz::config::effective::project_tasks_from_value(
        project_root,
        path,
        TrustState::Untrusted,
        &value,
    )
    .with_context(|| format!("validating project `tasks.{name}`"))?;
    Ok(())
}
