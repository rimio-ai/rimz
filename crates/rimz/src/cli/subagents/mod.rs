//! `rimz subagents` — supervised child launch and lifecycle sugar.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use super::usage::UsageError;
use super::{Ctx, GlobalFlags, agents_cmd, render};
use agents_cmd::newest_run_for_child;
use rimz::address::selector::{self, AgentRow, Field, Query};
use rimz::agents::AgentState;
use rimz::store::snapshot::SidebarSnapshot;

mod resume;
pub(in crate::cli) use resume::{ResumeChildOutcome, resume_child};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SubagentsArgs {
    #[command(subcommand)]
    command: Option<SubagentsSubcmd>,
    #[command(flatten)]
    launch: SubagentLaunchArgs,
    /// Emit the waited launch result as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum SubagentsSubcmd {
    /// Launch one supervised child agent.
    Launch(Box<LaunchArgs>),
    /// Launch children from a JSON task list.
    Fanout(FanoutArgs),
    /// List supervised child agents.
    #[command(alias = "ls")]
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Keep children matching `field=value` terms; repeatable, `--filter help` lists fields.
        #[arg(
            long = "filter",
            value_name = "QUERY",
            add = clap_complete::ArgValueCompleter::new(crate::cli::complete::filter_terms)
        )]
        filters: Vec<String>,
        /// Print one address per listed child and nothing else.
        #[arg(short = 'q', long, conflicts_with = "json")]
        quiet: bool,
    },
    /// List agent profiles available to launch.
    Profiles {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Include each profile's defining file path.
        #[arg(long)]
        path: bool,
    },
    /// Wait for this agent's supervised children.
    Wait {
        /// Child names; at least one.
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Return when the first named child finishes; print its result block, or a one-entry map with `--json`.
        #[arg(long, conflicts_with = "stream")]
        any: bool,
        /// Stop waiting after this duration.
        #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
        timeout: Option<Duration>,
        /// Tail one child's transcript while waiting.
        #[arg(long)]
        stream: bool,
        /// Emit a labeled result map for joins; with `--stream`, emit NDJSON events.
        #[arg(long)]
        json: bool,
    },
    /// Stop named children, or every live child with `--all`.
    Stop {
        #[arg(value_name = "NAME", required_unless_present = "all")]
        names: Vec<String>,
        /// Stop every live child.
        #[arg(long, conflicts_with = "names")]
        all: bool,
    },
}

#[derive(Debug, Args)]
struct LaunchArgs {
    #[command(flatten)]
    launch: SubagentLaunchArgs,
    /// Emit the waited result as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct FanoutArgs {
    /// Tasks JSON; stdin when omitted.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
    /// Wait for every launched child and print its result; use `--wait=5m` for a deadline.
    #[arg(
        long,
        value_name = "DURATION",
        num_args = 0..=1,
        require_equals = true,
        value_parser = crate::cli::supervised::parse_timeout
    )]
    wait: Option<Option<Duration>>,
    /// Tell each child to stop after this duration, then allow its reporting grace.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Warning offsets before the deadline, comma-separated; `none` disables warnings.
    #[arg(long, value_name = "OFFSETS", value_parser = parse_warn)]
    warn: Option<WarnOffsets>,
    /// Time to report after the deadline; `0s` kills at the deadline.
    #[arg(long, value_name = "DURATION", value_parser = crate::cli::supervised::parse_timeout)]
    grace: Option<Duration>,
    /// Disable automatic completion cleanup; hold each child's pane after exit until its parent ends or Ctrl-C is pressed in the pane (run timeouts still apply).
    #[arg(long)]
    keep: bool,
    /// Retired; independent runs use `rimz agents -p --bg --detach`.
    #[arg(long, hide = true)]
    detach: bool,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FanoutTask {
    cwd: Option<PathBuf>,
    #[serde(default)]
    add_dirs: Vec<PathBuf>,
    #[serde(default)]
    empty_dir: bool,
    profile: Option<String>,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    model: Option<String>,
    tier: Option<rimz::config::tiers::ModelTier>,
    agent: Option<String>,
    effort: Option<String>,
    /// `true` requests the provider's fast mode; `false` requests none, as `--no-fast` does.
    fast: Option<bool>,
    timeout: Option<String>,
    warn: Option<Vec<String>>,
    grace: Option<String>,
    max_turns: Option<u32>,
    description: Option<String>,
}

#[derive(Debug, Default, PartialEq, Args)]
#[command(
    after_help = "Launch several children in parallel. The printed petname is also an address: use `rimz message @petname \"…\"` for a follow-up."
)]
struct SubagentLaunchArgs {
    /// Working directory for the launched agent; the room stays the caller's.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Additional working directory with file access, without loading its configuration.
    #[arg(long = "add-dir", value_name = "DIR")]
    add_dirs: Vec<PathBuf>,
    /// Require a private empty working directory, also beside --cwd.
    #[arg(long)]
    empty_dir: bool,
    /// Configured subagent profile, agent kind, or shared command.
    #[arg(
        value_name = "PROFILE",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::subagent_profiles)
    )]
    profile: Option<String>,
    /// Complete task prompt supplied by the parent agent.
    #[arg(value_name = "PROMPT", allow_hyphen_values = true)]
    prompt: Option<String>,
    /// File whose contents become the child's prompt.
    #[arg(long = "prompt-file", value_name = "PATH", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    /// Model for the child.
    #[arg(long, value_name = "MODEL", conflicts_with = "tier")]
    model: Option<String>,
    /// Capability tier for the child.
    #[arg(long, value_name = "TIER")]
    tier: Option<rimz::config::tiers::ModelTier>,
    /// Prefer this provider kind, or re-base onto a replacement profile.
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
    /// Reasoning effort for the child.
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Run the child in the provider's fast mode (Claude, Codex, Amp, Pi on OpenAI).
    #[arg(long, conflicts_with = "no_fast")]
    fast: bool,
    /// Request no fast mode for the child, even where its profile sets `fast`.
    #[arg(long)]
    no_fast: bool,
    /// Run the child under this isolation instead of inheriting the parent's.
    #[arg(long, value_name = "host|scoped|sandbox")]
    isolation: Option<rimz::config::Isolation>,
    /// Run the child in yolo mode, up to the parent's own mode.
    #[arg(long)]
    yolo: bool,
    /// Tell the child to stop after this duration, then allow its reporting grace.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Warning offsets before the deadline, comma-separated; `none` disables warnings.
    #[arg(long, value_name = "OFFSETS", value_parser = parse_warn)]
    warn: Option<WarnOffsets>,
    /// Time to report after the deadline; `0s` kills at the deadline.
    #[arg(long, value_name = "DURATION", value_parser = crate::cli::supervised::parse_timeout)]
    grace: Option<Duration>,
    /// Wait for the child and print its result; use `--wait=5m` for a deadline.
    #[arg(
        long,
        value_name = "DURATION",
        num_args = 0..=1,
        require_equals = true,
        value_parser = crate::cli::supervised::parse_timeout
    )]
    wait: Option<Option<Duration>>,
    /// Disable automatic completion cleanup; hold the child's pane after exit until its parent ends or Ctrl-C is pressed in the pane (run timeouts still apply).
    #[arg(long)]
    keep: bool,
    /// Retired; independent runs use `rimz agents -p --bg --detach`.
    #[arg(long, hide = true)]
    detach: bool,
    /// Seed the child card's description.
    #[arg(long, value_name = "TEXT")]
    description: Option<String>,
    /// Maximum agentic turns for the prompt.
    #[arg(long, value_name = "N")]
    max_turns: Option<u32>,
    /// Extra argv appended to the launched child.
    #[arg(last = true)]
    passthrough: Vec<String>,
}

pub fn run(args: SubagentsArgs, globals: &GlobalFlags) -> Result<()> {
    reject_detach(&args)?;
    if command_is_agent_only(&args) {
        require_agent_caller(resolve_agent_caller(globals)?)?;
    }
    match args.command {
        Some(SubagentsSubcmd::Launch(args)) => launch_child(args.launch, args.json, globals),
        Some(SubagentsSubcmd::Fanout(fanout)) => fanout_children(fanout, globals),
        Some(SubagentsSubcmd::List {
            json,
            filters,
            quiet,
        }) => list_children(json, quiet, &filters, globals),
        Some(SubagentsSubcmd::Profiles { json, path }) => list_profiles(json, path, globals),
        Some(SubagentsSubcmd::Wait {
            names,
            any,
            timeout,
            stream,
            json,
        }) => wait_children(names, any, timeout, stream, json, globals),
        Some(SubagentsSubcmd::Stop { names, all }) => stop_children(names, all, globals),
        None if names_profile(&args) => launch_child(args.launch, args.json, globals),
        None => {
            let flags: &[&str] = if args.json { &["--json"] } else { &[] };
            Err(UsageError::missing_verb("subagents", "a profile", flags).into())
        }
    }
}

/// A blank PROFILE, as an empty shell variable leaves it, names nothing.
fn names_profile(args: &SubagentsArgs) -> bool {
    args.launch
        .profile
        .as_deref()
        .is_some_and(|profile| !profile.trim().is_empty())
}

fn command_is_agent_only(args: &SubagentsArgs) -> bool {
    match args.command.as_ref() {
        Some(SubagentsSubcmd::Profiles { .. } | SubagentsSubcmd::List { .. }) => false,
        Some(
            SubagentsSubcmd::Launch(_)
            | SubagentsSubcmd::Fanout(_)
            | SubagentsSubcmd::Wait { .. }
            | SubagentsSubcmd::Stop { .. },
        ) => true,
        None => names_profile(args),
    }
}

fn require_agent_caller(agent_caller: bool) -> Result<()> {
    if agent_caller {
        return Ok(());
    }
    bail!(
        "launching, joining, and stopping subagents is only available to an agent RimZ can identify; from a user shell, `rimz subagents list` inspects the current channel's subagents, and `rimz agents <spec>`, `rimz agents list/wait/stop`, or `rimz teams` drive agents directly"
    )
}

fn resolve_agent_caller(globals: &GlobalFlags) -> Result<bool> {
    if rimz::harness::ancestry::CallerIdentity::from_env().is_some() {
        return Ok(true);
    }
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let Some(store) = crate::cli::open_existing_store(&workspace)? else {
        return Ok(false);
    };
    Ok(crate::cli::send::resolve_caller(&store)?.is_some())
}

fn reject_detach(args: &SubagentsArgs) -> Result<()> {
    let detach = match args.command.as_ref() {
        Some(SubagentsSubcmd::Launch(args)) => args.launch.detach,
        Some(SubagentsSubcmd::Fanout(args)) => args.detach,
        None => args.launch.detach,
        _ => false,
    };
    if detach {
        return Err(UsageError::new(
            "--detach was removed from `rimz subagents`: a subagent ends with its parent. For a run that outlives you, launch a peer: `rimz agents <spec> \"<prompt>\" -p --bg --detach`".into(),
        )
        .into());
    }
    Ok(())
}

fn launch_child(args: SubagentLaunchArgs, json: bool, globals: &GlobalFlags) -> Result<()> {
    let wait = args.wait;
    if json && wait.is_none() {
        bail!("--json on a single launch requires --wait");
    }
    let launch = args.into_agent_launch()?;
    let child = match agents_cmd::launch_supervised_background(launch, globals)? {
        agents_cmd::BackgroundLaunchOutcome::Launched(child) => child,
        agents_cmd::BackgroundLaunchOutcome::BudgetExceeded { reason } => {
            let err = anyhow::Error::msg(reason).context("launching subagent");
            render::report(&err);
            std::process::exit(rimz::store::run::RunStatus::BudgetExceeded.exit_code());
        }
        agents_cmd::BackgroundLaunchOutcome::Aborted => return Ok(()),
    };
    if !json {
        writeln!(render::out(), "{}", child.name)?;
    }
    let Some(timeout) = wait else {
        crate::cli::supervised::output::write_background_receipt(
            &mut render::err(),
            &[child.name.as_str()],
            child.response_path.as_deref(),
            true,
            rimz::store::run::ReportTo::Launcher,
        )?;
        return Ok(());
    };
    agents_cmd::wait_agent(
        vec![(child.name.clone(), child.name)],
        false,
        timeout,
        false,
        false,
        json,
        globals,
    )
}

fn fanout_children(args: FanoutArgs, globals: &GlobalFlags) -> Result<()> {
    let (raw, source) = match args.file.as_deref() {
        Some(path) => (
            fs::read_to_string(path)
                .with_context(|| format!("reading fanout tasks from `{}`", path.display()))?,
            format!("`{}`", path.display()),
        ),
        None => {
            let mut raw = String::new();
            std::io::stdin()
                .read_to_string(&mut raw)
                .context("reading fanout tasks from stdin")?;
            (raw, "stdin".to_owned())
        }
    };
    let launches = parse_fanout_launches(&raw, &args)
        .with_context(|| format!("validating fanout tasks from {source}"))?;
    let ctx = Ctx::open(globals)?;
    for launch in &launches {
        rimz::launcher::checkout::resolve_launch_cwd(launch.cwd.as_deref(), ctx.store.paths())?;
        for dir in &launch.add_dirs {
            rimz::launcher::checkout::resolve_launch_cwd(Some(dir), ctx.store.paths())
                .context("resolving --add-dir")?;
        }
    }
    let mut launched = Vec::with_capacity(launches.len());
    for (index, launch) in launches.into_iter().enumerate() {
        match agents_cmd::launch_supervised_background(launch, globals) {
            Ok(agents_cmd::BackgroundLaunchOutcome::Launched(child)) => {
                if !args.json {
                    writeln!(render::out(), "{}", child.name)?;
                }
                launched.push(child);
            }
            Ok(agents_cmd::BackgroundLaunchOutcome::BudgetExceeded { reason }) => {
                let err = anyhow::Error::msg(reason)
                    .context(fanout_launch_error_context(index, &launched));
                render::report(&err);
                std::process::exit(rimz::store::run::RunStatus::BudgetExceeded.exit_code());
            }
            Ok(agents_cmd::BackgroundLaunchOutcome::Aborted) => return Ok(()),
            Err(err) => {
                return Err(err).context(fanout_launch_error_context(index, &launched));
            }
        }
    }
    let Some(wait_timeout) = args.wait else {
        crate::cli::supervised::output::write_background_receipt(
            &mut render::err(),
            &launched
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            launched
                .first()
                .and_then(|child| child.response_path.as_deref()),
            true,
            rimz::store::run::ReportTo::Launcher,
        )?;
        if args.json {
            #[derive(Serialize)]
            struct BackgroundReport<'a> {
                run_id: &'a rimz::RunId,
            }

            let report = launched
                .iter()
                .map(|child| {
                    (
                        child.name.as_str(),
                        BackgroundReport {
                            run_id: &child.run_id,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            render::json(&report)?;
        }
        return Ok(());
    };
    let names = launched
        .into_iter()
        .map(|child| (child.name.clone(), child.name))
        .collect();
    agents_cmd::wait_agent(names, false, wait_timeout, false, false, args.json, globals)
}

fn fanout_launch_error_context(index: usize, launched: &[agents_cmd::BackgroundLaunch]) -> String {
    if launched.is_empty() {
        return format!("launching fanout task {}", index + 1);
    }
    let names = launched
        .iter()
        .map(|child| child.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "launching fanout task {} after starting {names}; the launched children keep running, so join or stop them with `rimz subagents wait <name>…` or `rimz subagents stop --all`",
        index + 1
    )
}

fn parse_fanout_launches(raw: &str, args: &FanoutArgs) -> Result<Vec<agents_cmd::AgentLaunchArgs>> {
    let tasks: Vec<FanoutTask> =
        serde_json::from_str(raw).context("fanout input must be a JSON task array")?;
    if tasks.is_empty() {
        bail!("fanout needs at least one task");
    }
    tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| {
            task.into_agent_launch(args)
                .with_context(|| format!("task {}", index + 1))
        })
        .collect()
}

impl FanoutTask {
    fn into_agent_launch(self, fanout: &FanoutArgs) -> Result<agents_cmd::AgentLaunchArgs> {
        let timeout = self
            .timeout
            .as_deref()
            .map(crate::cli::supervised::parse_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)
            .context("parsing timeout")?
            .or(fanout.timeout);
        let warn = self
            .warn
            .map(|offsets| {
                offsets
                    .iter()
                    .map(|offset| crate::cli::supervised::parse_timeout(offset))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map(WarnOffsets)
            })
            .transpose()
            .map_err(anyhow::Error::msg)
            .context("parsing warn")?
            .or_else(|| fanout.warn.clone());
        let grace = self
            .grace
            .as_deref()
            .map(crate::cli::supervised::parse_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)
            .context("parsing grace")?
            .or(fanout.grace);
        let launch = SubagentLaunchArgs {
            cwd: self.cwd,
            add_dirs: self.add_dirs,
            empty_dir: self.empty_dir,
            profile: self.profile,
            prompt: self.prompt,
            prompt_file: self.prompt_file,
            model: self.model,
            tier: self.tier,
            agent: self.agent,
            effort: self.effort,
            fast: self.fast == Some(true),
            no_fast: self.fast == Some(false),
            isolation: None,
            yolo: false,
            timeout,
            warn,
            grace,
            wait: None,
            keep: fanout.keep,
            detach: false,
            description: self.description,
            max_turns: self.max_turns,
            passthrough: Vec::new(),
        }
        .into_agent_launch()?;
        Ok(launch)
    }
}

impl SubagentLaunchArgs {
    fn into_agent_launch(self) -> Result<agents_cmd::AgentLaunchArgs> {
        if self.model.is_some() && self.tier.is_some() {
            bail!("choose one of model and tier");
        }
        let profile = self.profile.context("a subagent needs a profile")?;
        if self.wait == Some(None)
            && let Some(prompt) = self.prompt.as_deref()
        {
            let duration = prompt.trim();
            if crate::cli::supervised::parse_timeout(duration).is_ok() {
                bail!(
                    "prompt `{prompt}` looks like a wait duration; did you mean `--wait={duration}`?"
                );
            }
        }
        let prompt = match (self.prompt, self.prompt_file) {
            (Some(prompt), None) if !prompt.trim().is_empty() => prompt,
            (None, Some(path)) => crate::cli::send::read_prompt_file(&path)?,
            (Some(_), Some(_)) => {
                bail!("a subagent task cannot set both `prompt` and `prompt_file`")
            }
            _ => bail!(
                "a subagent needs its prompt from the parent via `PROMPT`, `--prompt-file`, or `prompt_file`"
            ),
        };
        Ok(agents_cmd::AgentLaunchArgs {
            cwd: self.cwd,
            add_dirs: self.add_dirs,
            empty_dir: self.empty_dir,
            spec: Some(profile),
            prompt: Some(prompt),
            cohort: agents_cmd::CohortLaunchArgs {
                description: self.description,
                bg: true,
                ..Default::default()
            },
            overrides: agents_cmd::LaunchOverrideArgs {
                model: self.model,
                tier: self.tier,
                agent: self.agent,
                effort: self.effort,
                fast: self.fast,
                no_fast: self.no_fast,
                isolation: self.isolation,
                yolo: self.yolo,
                passthrough: self.passthrough,
                ..Default::default()
            },
            print: true,
            self_cleanup_on_completion: true,
            subagent: true,
            timeout: self.timeout,
            warn: self.warn.map(|offsets| offsets.0),
            grace: self.grace,
            keep: self.keep,
            max_turns: self.max_turns,
            ..Default::default()
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct WarnOffsets(Vec<Duration>);

fn parse_warn(raw: &str) -> std::result::Result<WarnOffsets, String> {
    if raw == "none" {
        return Ok(WarnOffsets(Vec::new()));
    }
    raw.split(',')
        .map(crate::cli::supervised::parse_timeout)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(WarnOffsets)
}

fn caller_and_children(agents: &[AgentState]) -> Result<(&AgentState, Vec<&AgentState>)> {
    let caller = rimz::harness::ancestry::resolve_calling_agent(agents)?;
    let children = rimz::address::launched_children(agents, caller);
    Ok((caller, children))
}

#[derive(Serialize)]
struct ChildReport {
    name: String,
    handle: String,
    parent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    kind: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_error: Option<ChildTurnError>,
    #[serde(skip)]
    turn_error_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<jiff::Timestamp>,
    #[serde(skip)]
    elapsed_secs: Option<u64>,
    /// The selector row's flat keys, which win where a key is shared.
    #[serde(skip)]
    row: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
struct ChildTurnError {
    class: rimz::agents::TurnErrorClass,
    label: Option<String>,
}

impl ChildReport {
    /// The muted line under a row: why the child stopped, else what it is doing.
    fn detail(&self) -> Option<String> {
        self.turn_error_label
            .clone()
            .or_else(|| self.description.clone())
    }

    /// The `--json` entry: the row's flat keys, then this report's keys beside them.
    fn json(&self) -> serde_json::Result<serde_json::Value> {
        let mut fields = self.row.clone();
        if let serde_json::Value::Object(report) = serde_json::to_value(self)? {
            for (key, value) in report {
                fields.entry(key).or_insert(value);
            }
        }
        Ok(serde_json::Value::Object(fields))
    }
}

/// Where the preset's children come from before the filter's terms apply.
#[derive(Clone, Copy)]
enum ListScope<'a> {
    /// The caller's own launched children, finished ones included.
    Caller(&'a AgentState),
    /// Every launched child in a lane, or in the room without one.
    Channel,
}

/// A resolved agent caller sees its own children unless a term names `parent`.
fn list_scope<'a>(caller: Option<&'a AgentState>, query: &Query) -> ListScope<'a> {
    match caller {
        Some(caller) if !query.names(Field::Parent) => ListScope::Caller(caller),
        _ => ListScope::Channel,
    }
}

/// The preset over the selector's rows: launched children only, the scope's
/// children, the current `lane` unless a term names `lane`, then every term.
fn preset_rows<'a>(
    rows: Vec<AgentRow<'a>>,
    agents: &[AgentState],
    scope: ListScope<'_>,
    lane: Option<&rimz::address::LaneSelector>,
    query: &Query,
    reader: &rimz::address::AddressContext,
) -> Vec<AgentRow<'a>> {
    let own = match scope {
        ListScope::Caller(caller) => Some(rimz::address::launched_children(agents, caller)),
        ListScope::Channel => None,
    };
    let lane = lane.filter(|_| own.is_none() && !query.names(Field::Lane));
    rows.into_iter()
        .filter(|row| {
            let agent = row.agent();
            agent.is_launched_child()
                && own.as_ref().is_none_or(|own| {
                    own.iter()
                        .any(|child| child.kind == agent.kind && child.agent_id == agent.agent_id)
                })
                && lane.is_none_or(|lane| rimz::address::agent_in_lane(agent, lane))
                && query.matches(row, reader)
        })
        .collect()
}

/// What `subagents list` lists until a term names the field.
const FILTER_DEFAULTS: &str = "naming parent or lane replaces its default: an agent's own \
     children (else the current lane's), the current lane; live has no default";

fn list_children(json: bool, quiet: bool, filters: &[String], globals: &GlobalFlags) -> Result<()> {
    if filters.iter().any(|filter| filter.trim() == "help") {
        return Ok(render::filter_help(&mut render::out(), FILTER_DEFAULTS)?);
    }
    let query = Query::parse(filters).map_err(|err| UsageError::new(err.to_string()))?;
    let ctx = Ctx::open(globals)?;
    let _mux = rimz::room::require_live_mux(globals.mux, &ctx.workspace)?;
    let snapshot = ctx.published_snapshot()?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading agent history")?
        .agents;
    let now = jiff::Timestamp::now();
    let demotion = rimz::harness::park_demotion(
        Some(&ctx.store),
        ctx.store.paths(),
        ctx.runtime(),
        &crate::cli::machine_config(),
        &snapshot.agents,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
        now,
    );
    let audit_snapshot = ctx.fold_agent_context(SidebarSnapshot::build_with_agents(
        ctx.workspace.workspace_id.clone(),
        audit.clone(),
        now,
    ));
    let reader = ctx.address_context();
    let mut rows = selector::live_rows(&snapshot, &demotion, &reader, now);
    let closed = selector::closed_rows(&audit_snapshot, &snapshot, &rows, &demotion, &reader, now);
    rows.extend(closed);
    let caller = rimz::harness::ancestry::resolve_caller(&audit).and_then(|identity| {
        rimz::harness::ancestry::resolve_launch_caller(&audit, &identity).ok()
    });
    let scope = list_scope(caller, &query);
    let rows = preset_rows(rows, &audit, scope, reader.lane().as_ref(), &query, &reader);
    let runs = rimz::harness::run::list(ctx.store.paths())?;
    let reports = child_reports(&rows, &runs, now)?;
    if json {
        let entries = reports
            .iter()
            .map(ChildReport::json)
            .collect::<serde_json::Result<Vec<_>>>()?;
        return render::json_pretty(&entries);
    }
    if quiet {
        let mut out = render::out();
        for child in &reports {
            writeln!(out, "{}", child.handle)?;
        }
        return Ok(());
    }
    if reports.is_empty() {
        let channel = reader
            .channel
            .as_ref()
            .filter(|_| !query.names(Field::Lane))
            .map(|channel| format!("#{channel}"));
        let line = match (scope, channel, filters.is_empty()) {
            (ListScope::Caller(caller), _, true) => format!(
                "{} has launched no subagents. Launch one: rimz subagents launch <PROFILE> \"<PROMPT>\" (profiles: rimz subagents profiles)",
                peer_handle(&audit, caller)
            ),
            (ListScope::Caller(caller), _, false) => format!(
                "No subagent of {} matches the filter",
                peer_handle(&audit, caller)
            ),
            (ListScope::Channel, Some(channel), true) => format!("No subagents in {channel}"),
            (ListScope::Channel, Some(channel), false) => {
                format!("No subagent in {channel} matches the filter")
            }
            (ListScope::Channel, None, true) => "No subagents in this room".to_owned(),
            (ListScope::Channel, None, false) => {
                "No subagent in this room matches the filter".to_owned()
            }
        };
        return render::finish(writeln!(render::err(), "{line}"));
    }
    let headers = match scope {
        ListScope::Caller(_) => vec!["SUBAGENT", "KIND", "AGENT", "RUN", "ELAPSED"],
        ListScope::Channel => vec![
            "SUBAGENT", "PARENT", "CHANNEL", "KIND", "AGENT", "RUN", "ELAPSED",
        ],
    };
    let mut table = render::Table::new(headers).max_width(render::terminal_columns(120));
    for child in reports {
        let detail = child
            .detail()
            .map(|line| render::cell(line).fg(render::palette::muted()));
        let elapsed = render::cell(
            child
                .elapsed_secs
                .map_or_else(|| "-".to_owned(), render::format_compact_duration),
        )
        .dash();
        let row = match scope {
            ListScope::Caller(_) => vec![
                render::cell(child.handle),
                render::cell(child.kind),
                render::cell(child.status),
                render::cell(child.run_status.unwrap_or_else(|| "-".to_owned())).dash(),
                elapsed,
            ],
            ListScope::Channel => vec![
                render::cell(child.handle),
                render::cell(child.parent),
                render::cell(child.channel.unwrap_or_else(|| "-".to_owned())).dash(),
                render::cell(child.kind),
                render::cell(child.status),
                render::cell(child.run_status.unwrap_or_else(|| "-".to_owned())).dash(),
                elapsed,
            ],
        };
        table.card(row, detail);
    }
    table.render(&mut render::out()).map_err(Into::into)
}

/// One report per row, open runs first, then settled ones, newest answer first,
/// then runless children, ties in registration order. Status, handle, and
/// parent are the row's.
fn child_reports(
    rows: &[AgentRow<'_>],
    runs: &[rimz::store::run::RunRecord],
    now: jiff::Timestamp,
) -> serde_json::Result<Vec<ChildReport>> {
    let mut rows = rows
        .iter()
        .map(|row| (row, newest_run_for_child(runs, row.agent())))
        .collect::<Vec<_>>();
    rows.sort_by_key(|(row, run)| {
        let registered = (row.agent().registered_at, row.agent().agent_id.clone());
        match run {
            Some(run) => (
                u8::from(run.status.is_terminal()),
                Some(Reverse(run.answer_started_at())),
                registered,
            ),
            None => (2, None, registered),
        }
    });
    rows.into_iter()
        .map(|(row, run)| {
            let child = row.agent();
            Ok(ChildReport {
                name: child
                    .name
                    .clone()
                    .unwrap_or_else(|| child.agent_id.to_string()),
                handle: row.handle().to_owned(),
                parent: row.parent().map(ToOwned::to_owned).unwrap_or_else(|| {
                    format!(
                        "@{}",
                        child
                            .parent_agent_id
                            .as_ref()
                            // `preset_rows` keeps launched children only, and a
                            // launched child carries a parent id.
                            .expect("launched children have a parent")
                    )
                }),
                channel: child.lane_name().map(String::from),
                kind: child.kind.to_string(),
                status: row.card().0.as_str().to_owned(),
                description: child.activity_line(),
                turn_error_label: selector::child_card(row.source(), child)
                    .and_then(|card| card.turn_error_label.clone())
                    .or_else(|| child.displayed_turn_error_label(now)),
                turn_error: child
                    .displayed_turn_error(now)
                    .map(|(class, error)| ChildTurnError {
                        class,
                        label: error.label.clone(),
                    }),
                run_id: run.map(|run| run.run_id.to_string()),
                run_status: run.map(|run| run.status.as_str().to_owned()),
                started_at: run.map(rimz::store::run::RunRecord::answer_started_at),
                elapsed_secs: run.map(|run| {
                    let end = if run.status.is_terminal() {
                        run.completed_at.unwrap_or(run.updated_at)
                    } else {
                        now
                    };
                    end.duration_since(run.answer_started_at()).as_secs().max(0) as u64
                }),
                row: serde_json::from_value(serde_json::to_value(row)?)?,
            })
        })
        .collect()
}

fn list_profiles(json: bool, path: bool, globals: &GlobalFlags) -> Result<()> {
    let (config, mut sources) = rimz::config::MachineConfig::load_with_agent_spec_sources()
        .context("loading machine config")?;
    crate::cli::report_definition_errors(&config)?;
    let participant = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let caller = match crate::cli::open_existing_store(&participant)? {
        Some(store) => {
            let projection = store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .context("reading agent history")?;
            rimz::harness::ancestry::resolve_caller(&projection.agents)
                .map(|identity| {
                    rimz::harness::ancestry::resolve_launch_caller(&projection.agents, &identity)
                        .cloned()
                })
                .transpose()?
        }
        None => None,
    };
    let project_root = if caller.is_some() {
        participant.project_root
    } else {
        rimz::WorkspaceResolver::resolve(".", globals.root.clone())
            .context("resolving current project")?
            .project_root
    };
    let effective = rimz::config::effective::load(&config, &project_root)?;
    effective.overlay_profile_sources(&mut sources);
    let catalog = rimz::harness::subagent_policy::catalog(
        caller.as_ref().and_then(|caller| caller.profile.as_deref()),
        &effective.profiles,
        &effective.subagent_profiles,
        &config.agents.commands,
    );
    let reports = crate::cli::profile_report::subagent_reports(
        catalog,
        &effective.subagent_profiles,
        &sources,
    );
    crate::cli::profile_report::list_profiles(
        reports,
        crate::cli::profile_report::ProfileListing::Subagents,
        json,
        path,
    )
}

/// How the `PARENT` column renders `agent` among the room's peers.
fn peer_handle(agents: &[AgentState], agent: &AgentState) -> String {
    let peers = agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .collect::<Vec<_>>();
    rimz::address::agent_handle(agent, &peers, false)
}

fn wait_children(
    names: Vec<String>,
    any: bool,
    timeout: Option<Duration>,
    stream: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading agent history")?;
    let (_, children) = caller_and_children(&audit.agents)?;
    let references = wait_references(&children, &names, &ctx.address_context())?;
    agents_cmd::wait_agent(references, any, timeout, stream, false, json, globals)
}

fn wait_references(
    children: &[&AgentState],
    names: &[String],
    context: &rimz::address::AddressContext,
) -> Result<Vec<(String, String)>> {
    if names.is_empty() {
        if children.is_empty() {
            bail!("this agent has no supervised subagents to wait for");
        }
        let names = children
            .iter()
            .map(|child| child_reference(child))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "`rimz subagents wait` needs at least one child name; this agent's subagents: {names} (see `rimz subagents list`)"
        );
    }
    let mut seen = std::collections::HashSet::new();
    let names: Vec<_> = names
        .iter()
        .filter(|name| seen.insert(*name))
        .cloned()
        .collect();
    Ok(resolve_child_names(children, &names, context)?
        .into_iter()
        .zip(names)
        .map(|(child, name)| (name, child_reference(child)))
        .collect())
}

fn stop_children(names: Vec<String>, all: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (_, all_children) = caller_and_children(&snapshot.agents)?;
    let children = if all {
        all_children
            .into_iter()
            .filter(|child| child.ended_at.is_none())
            .collect()
    } else {
        resolve_child_names(&all_children, &names, &ctx.address_context())?
    };
    if children.is_empty() {
        bail!("this agent has no live subagents to stop");
    }
    let runs = rimz::harness::run::list(ctx.store.paths())?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let mut out = render::out();
    // Prepare the whole fleet before any cancellation wakes a child reporter.
    let (children, errors) =
        prepare_children_for_stop(&ctx.store, &ctx.workspace.session_name, &runs, children);
    let mut failed = !errors.is_empty();
    for (child, err) in errors {
        let label = rimz::address::agent_handle(child, &peers, true);
        writeln!(out, "error {label}: {err:#}")?;
    }
    let stops = agents_cmd::stop_resolved(&ctx, globals, &snapshot, &children)?;
    for (child, stop) in children.into_iter().zip(stops) {
        let label = rimz::address::agent_handle(child, &peers, true);
        failed |= stop.report(&mut out, &label)?;
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn prepare_children_for_stop<'a>(
    store: &rimz::Store,
    session_name: &str,
    runs: &[rimz::store::run::RunRecord],
    children: Vec<&'a AgentState>,
) -> (
    Vec<&'a AgentState>,
    Vec<(&'a AgentState, rimz::store::StoreErr)>,
) {
    let mut prepared = Vec::with_capacity(children.len());
    let mut errors = Vec::new();
    for child in children {
        let Some(run) = newest_run_for_child(runs, child) else {
            prepared.push(child);
            continue;
        };
        match rimz::harness::run::report::join_and_settle_digest(
            store,
            session_name,
            &run.run_id,
            None,
            "stopped by parent",
        ) {
            Ok(()) => prepared.push(child),
            Err(err) => errors.push((child, err)),
        }
    }
    (prepared, errors)
}

fn resolve_child_names<'a>(
    children: &[&'a AgentState],
    names: &[String],
    context: &rimz::address::AddressContext,
) -> Result<Vec<&'a AgentState>> {
    let mut context = context.clone();
    context.channel = None;
    names
        .iter()
        .map(|name| {
            rimz::address::resolve_agent(name, None, &context, children)
                .with_context(|| format!("`{name}` is not one of this agent's subagents"))
        })
        .collect()
}

fn child_reference(child: &AgentState) -> String {
    child
        .name
        .clone()
        .unwrap_or_else(|| child.agent_id.to_string())
}

#[cfg(test)]
mod tests;
