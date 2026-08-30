//! `rimz subagents` — supervised child launch and lifecycle sugar.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use super::{Ctx, GlobalFlags, agents_cmd, render};
use rimz::agents::AgentState;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SubagentsArgs {
    #[command(subcommand)]
    command: Option<SubagentsSubcmd>,
    #[command(flatten)]
    launch: SubagentLaunchArgs,
    /// Emit the child list or waited launch result as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum SubagentsSubcmd {
    /// Launch one supervised child agent.
    Launch(Box<LaunchArgs>),
    /// Launch children from a JSON task list.
    Fanout(FanoutArgs),
    /// List this agent's children.
    #[command(alias = "ls")]
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
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
        /// Child names. With none, wait for every live child.
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Return when the first child finishes; print its name.
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
    /// Stop each child after this duration.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Leave child panes open after completion.
    #[arg(long)]
    keep: bool,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Deserialize)]
struct FanoutTask {
    spec: Option<String>,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    name: Option<String>,
    model: Option<String>,
    agent: Option<String>,
    effort: Option<String>,
    timeout: Option<String>,
    max_turns: Option<u32>,
    description: Option<String>,
}

#[derive(Debug, Default, PartialEq, Args)]
#[command(
    after_help = "Launch several children in parallel, then join them with `rimz subagents wait`. The printed petname is also an address: use `rimz message @petname \"…\"` for a follow-up."
)]
struct SubagentLaunchArgs {
    /// Agent kind or configured profile.
    #[arg(
        value_name = "SPEC",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::subagent_specs)
    )]
    spec: Option<String>,
    /// Complete task prompt supplied by the parent agent.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// File whose contents become the child's prompt.
    #[arg(long = "prompt-file", value_name = "PATH", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    /// Durable child petname.
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// Model for the child.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// Re-base the spec onto this profile or provider kind.
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
    /// Reasoning effort for the child.
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Stop the child after this duration.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Wait for the child and print its result; use `--wait=5m` for a deadline.
    #[arg(
        long,
        value_name = "DURATION",
        num_args = 0..=1,
        require_equals = true,
        value_parser = crate::cli::supervised::parse_timeout
    )]
    wait: Option<Option<Duration>>,
    /// Leave the child pane open after completion.
    #[arg(long)]
    keep: bool,
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
    if command_is_agent_only(args.command.as_ref()) {
        require_agent_caller(crate::cli::send::agent_caller())?;
    }
    match args.command {
        Some(SubagentsSubcmd::Launch(args)) => launch_child(args.launch, args.json, globals),
        Some(SubagentsSubcmd::Fanout(fanout)) => fanout_children(fanout, globals),
        Some(SubagentsSubcmd::List { json }) => list_children(json, globals),
        Some(SubagentsSubcmd::Profiles { json, path }) => list_profiles(json, path, globals),
        Some(SubagentsSubcmd::Wait {
            names,
            any,
            timeout,
            stream,
            json,
        }) => wait_children(names, any, timeout, stream, json, globals),
        Some(SubagentsSubcmd::Stop { names, all }) => stop_children(names, all, globals),
        None if args.launch.spec.is_some() => launch_child(args.launch, args.json, globals),
        None => {
            reject_launch_flags_without_spec(&args.launch)?;
            list_children(args.json, globals)
        }
    }
}

fn command_is_agent_only(command: Option<&SubagentsSubcmd>) -> bool {
    match command {
        Some(SubagentsSubcmd::Profiles { .. }) => false,
        Some(
            SubagentsSubcmd::Launch(_)
            | SubagentsSubcmd::Fanout(_)
            | SubagentsSubcmd::List { .. }
            | SubagentsSubcmd::Wait { .. }
            | SubagentsSubcmd::Stop { .. },
        )
        | None => true,
    }
}

fn require_agent_caller(agent_caller: bool) -> Result<()> {
    if agent_caller {
        return Ok(());
    }
    bail!(
        "`rimz subagents` is only available inside a RimZ-launched agent; from a user shell use `rimz agents <spec>`, `rimz agents list/wait/stop`, or `rimz teams`"
    )
}

fn launch_child(args: SubagentLaunchArgs, json: bool, globals: &GlobalFlags) -> Result<()> {
    let wait = args.wait;
    if json && wait.is_none() {
        bail!("--json on a single launch requires --wait");
    }
    let config = rimz::config::MachineConfig::load().context("loading machine config")?;
    let launch = args.into_agent_launch(&config.agents.subagents)?;
    let child = match agents_cmd::launch_supervised_background(launch, globals)? {
        agents_cmd::BackgroundLaunchOutcome::Launched(child) => child,
        agents_cmd::BackgroundLaunchOutcome::BudgetExceeded { reason } => {
            let err = anyhow::Error::msg(reason).context("launching subagent");
            render::report(&err);
            std::process::exit(rimz::harness::run::RunStatus::BudgetExceeded.exit_code());
        }
        agents_cmd::BackgroundLaunchOutcome::Aborted => return Ok(()),
    };
    if !json {
        writeln!(render::out(), "{}", child.name)?;
    }
    let Some(timeout) = wait else {
        return Ok(());
    };
    agents_cmd::wait_agent(
        vec![child.name],
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
    let config = rimz::config::MachineConfig::load().context("loading machine config")?;
    let launches = parse_fanout_launches(&raw, &args, &config.agents.subagents)
        .with_context(|| format!("validating fanout tasks from {source}"))?;
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
                std::process::exit(rimz::harness::run::RunStatus::BudgetExceeded.exit_code());
            }
            Ok(agents_cmd::BackgroundLaunchOutcome::Aborted) => return Ok(()),
            Err(err) => {
                return Err(err).context(fanout_launch_error_context(index, &launched));
            }
        }
    }
    let Some(wait_timeout) = args.wait else {
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
    let names = launched.into_iter().map(|child| child.name).collect();
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
        "launching fanout task {} after starting {names}; the launched children keep running, so join or stop them with `rimz subagents wait` or `rimz subagents stop --all`",
        index + 1
    )
}

fn parse_fanout_launches(
    raw: &str,
    args: &FanoutArgs,
    defaults: &rimz::config::SubagentsConfig,
) -> Result<Vec<agents_cmd::AgentLaunchArgs>> {
    let tasks: Vec<FanoutTask> =
        serde_json::from_str(raw).context("fanout input must be a JSON task array")?;
    if tasks.is_empty() {
        bail!("fanout needs at least one task");
    }
    let mut names = HashSet::new();
    for (index, task) in tasks.iter().enumerate() {
        if let Some(name) = task.name.as_deref() {
            rimz::harness::plan::validate_agent_name(name)
                .with_context(|| format!("task {} ({name})", index + 1))?;
            if !names.insert(name) {
                bail!("task {} repeats child name `{name}`", index + 1);
            }
        }
    }
    tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| {
            let label = task
                .name
                .as_deref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default();
            task.into_agent_launch(args, defaults)
                .with_context(|| format!("task {}{label}", index + 1))
        })
        .collect()
}

impl FanoutTask {
    fn into_agent_launch(
        self,
        fanout: &FanoutArgs,
        defaults: &rimz::config::SubagentsConfig,
    ) -> Result<agents_cmd::AgentLaunchArgs> {
        let timeout = self
            .timeout
            .as_deref()
            .map(crate::cli::supervised::parse_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)
            .context("parsing timeout")?
            .or(fanout.timeout);
        SubagentLaunchArgs {
            spec: self.spec,
            prompt: self.prompt,
            prompt_file: self.prompt_file,
            name: self.name,
            model: self.model,
            agent: self.agent,
            effort: self.effort,
            timeout,
            wait: None,
            keep: fanout.keep,
            description: self.description,
            max_turns: self.max_turns,
            passthrough: Vec::new(),
        }
        .into_agent_launch(defaults)
    }
}

impl SubagentLaunchArgs {
    fn into_agent_launch(
        self,
        defaults: &rimz::config::SubagentsConfig,
    ) -> Result<agents_cmd::AgentLaunchArgs> {
        let spec = self.spec.context("a subagent needs an agent spec")?;
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
        let timeout = self
            .timeout
            .map(Ok)
            .unwrap_or_else(|| crate::cli::supervised::parse_timeout(&defaults.timeout))
            .map_err(anyhow::Error::msg)
            .context("parsing agents.subagents.timeout")?;
        Ok(agents_cmd::AgentLaunchArgs {
            spec: Some(spec),
            prompt: Some(prompt),
            cohort: agents_cmd::CohortLaunchArgs {
                description: self.description,
                bg: true,
                ..Default::default()
            },
            name: self.name,
            model: self.model,
            agent: self.agent,
            effort: self.effort,
            print: true,
            self_cleanup_on_completion: true,
            subagent: true,
            timeout: Some(timeout),
            keep: self.keep,
            max_turns: self.max_turns,
            passthrough: self.passthrough,
            ..Default::default()
        })
    }
}

fn reject_launch_flags_without_spec(args: &SubagentLaunchArgs) -> Result<()> {
    if args.prompt.is_some()
        || args.prompt_file.is_some()
        || args.name.is_some()
        || args.model.is_some()
        || args.agent.is_some()
        || args.effort.is_some()
        || args.timeout.is_some()
        || args.wait.is_some()
        || args.keep
        || args.description.is_some()
        || args.max_turns.is_some()
        || !args.passthrough.is_empty()
    {
        bail!("subagent launch options require an agent spec");
    }
    Ok(())
}

fn caller_and_children(agents: &[AgentState]) -> Result<(&AgentState, Vec<&AgentState>)> {
    let caller = rimz::harness::ancestry::resolve_launch_caller_from_env(agents)?;
    let children = rimz::harness::target::launched_children(agents, caller);
    Ok((caller, children))
}

#[derive(Serialize)]
struct ChildReport {
    name: String,
    handle: String,
    kind: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_status: Option<String>,
}

fn list_children(json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading agent history")?;
    let (_, children) = caller_and_children(&audit.agents)?;
    let runs = rimz::harness::run::list(ctx.store.paths())?;
    let reports = children
        .into_iter()
        .map(|child| {
            let run = newest_run_for_child(&runs, child);
            let name = child
                .name
                .clone()
                .unwrap_or_else(|| child.agent_id.to_string());
            ChildReport {
                handle: format!("@{name}"),
                name,
                kind: child.kind.to_string(),
                status: child.status.as_str().to_owned(),
                description: child.activity_line(),
                run_id: run.map(|run| run.run_id.to_string()),
                run_status: run.map(|run| run.status.as_str().to_owned()),
            }
        })
        .collect::<Vec<_>>();
    if json {
        return render::json_pretty(&reports);
    }
    if reports.is_empty() {
        return Ok(());
    }
    let mut table = render::Table::new(["SUBAGENT", "KIND", "STATUS", "RUN"])
        .max_width(render::terminal_columns(120));
    for child in reports {
        let detail = child
            .description
            .map(|line| render::cell(line).fg(render::palette::muted()));
        table.card(
            [
                render::cell(child.handle),
                render::cell(child.kind),
                render::cell(child.status),
                render::cell(child.run_status.unwrap_or_else(|| "-".to_owned())).dash(),
            ],
            detail,
        );
    }
    table.render(&mut render::out()).map_err(Into::into)
}

fn list_profiles(json: bool, path: bool, globals: &GlobalFlags) -> Result<()> {
    let (config, sources) = rimz::config::MachineConfig::load_with_agent_spec_sources()
        .context("loading machine config")?;
    if !crate::cli::send::agent_caller() {
        return crate::cli::profile_report::list_profiles(
            &config.subagents.profiles,
            &config.agents.commands,
            &sources,
            rimz::config::effective::ProfileScope::Subagents,
            vec![crate::cli::profile_report::general_report(None)],
            None,
            json,
            path,
        );
    }
    let ctx = Ctx::open(globals)?;
    let projection = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading agent history")?;
    let caller = rimz::harness::ancestry::resolve_launch_caller_from_env(&projection.agents)?;
    let effective = rimz::config::effective::load(
        &config.agents,
        &config.subagents.profiles,
        &ctx.workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let allowed = rimz::harness::subagent_policy::allowed_specs(caller, &effective.profiles);
    crate::cli::profile_report::list_profiles(
        &effective.subagent_profiles,
        &config.agents.commands,
        &sources,
        rimz::config::effective::ProfileScope::Subagents,
        vec![crate::cli::profile_report::general_report(Some(caller))],
        allowed,
        json,
        path,
    )
}

fn newest_run_for_child<'a>(
    runs: &'a [rimz::harness::run::RunRecord],
    child: &AgentState,
) -> Option<&'a rimz::harness::run::RunRecord> {
    runs.iter()
        .filter(|run| {
            run.agent_id.as_ref() == Some(&child.agent_id)
                || run.agent_name.as_deref() == child.name.as_deref()
        })
        .max_by_key(|run| run.started_at)
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
    let runs = rimz::harness::run::list(ctx.store.paths())?;
    let references = wait_references(&children, &runs, &names, any)?;
    if references.is_empty() {
        bail!("this agent has no supervised subagents to wait for");
    }
    agents_cmd::wait_agent(references, any, timeout, stream, false, json, globals)
}

fn wait_references(
    children: &[&AgentState],
    runs: &[rimz::harness::run::RunRecord],
    names: &[String],
    any: bool,
) -> Result<Vec<String>> {
    if names.is_empty() {
        return Ok(children
            .iter()
            .copied()
            .filter_map(|child| {
                let run = newest_run_for_child(runs, child)?;
                (!any || !run.status.is_terminal()).then(|| child_reference(child))
            })
            .collect());
    }
    Ok(resolve_child_names(children, names)?
        .into_iter()
        .map(child_reference)
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
        resolve_child_names(&all_children, &names)?
    };
    if children.is_empty() {
        bail!("this agent has no live subagents to stop");
    }
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let mut tracker = agents_cmd::StopTracker::default();
    let mut failed = false;
    let mut out = render::out();
    for child in children {
        let label = rimz::harness::target::agent_handle(child, &peers, true);
        match agents_cmd::stop_resolved(&ctx, globals, &snapshot, child, &mut tracker) {
            Ok(true) => writeln!(out, "stopped {label}")?,
            Ok(false) => {}
            Err(err) => {
                failed = true;
                writeln!(out, "error {label}: {err:#}")?;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn resolve_child_names<'a>(
    children: &[&'a AgentState],
    names: &[String],
) -> Result<Vec<&'a AgentState>> {
    names
        .iter()
        .map(|name| {
            rimz::harness::target::resolve_agent(name, None, None, children)
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
