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

use std::collections::BTreeMap;
use std::io::Write;
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
};
use rimz::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use rimz::ids::WorkspaceId;
use rimz::message::DeliveryGate;
use rimz::sidebar::fresh_sidebar_present;
use rimz::store::atomic::write_bytes_atomically;
use rimz::store::paths::{RuntimePaths, StatePaths, agents_home, config_home, state_home};
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
    /// Fire the ping 1 minute after the provider's longest budget window resets.
    #[arg(long = "at-reset", conflicts_with_all = ["at", "days", "every", "cron", "in_after"])]
    at_reset: bool,
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

#[derive(Debug, Args)]
struct FireArgs {
    name: String,
    /// Leave the transient run pane open for inspection.
    #[arg(long)]
    keep: bool,
}

#[derive(Debug, Args)]
struct RenameArgs {
    name: String,
    new_name: String,
}

#[derive(Debug, Args)]
struct ShowArgs {
    name: String,
    /// Number of recent run rows to show; consecutive identical runs collapse into one.
    #[arg(short = 'n', long = "runs", default_value_t = 10)]
    runs: usize,
}

pub fn run(args: LoopArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        LoopSubcmd::Add(args) => add::add(*args),
        LoopSubcmd::Remove(args) => add::remove(&args.name),
        LoopSubcmd::Rename(args) => add::rename(&args.name, &args.new_name),
        LoopSubcmd::List => render::list(),
        LoopSubcmd::Show(args) => render::show(args),
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

fn remove_task(name: &str, source: TaskSource) -> Result<bool> {
    match source {
        TaskSource::Config => config_remove(name),
        TaskSource::Instance => instances::remove(name).map_err(Into::into),
    }
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
    if entry.at_reset {
        table["at-reset"] = value(true);
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
