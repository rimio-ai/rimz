//! `rimz subagents` — agent-only supervised child launch and lifecycle sugar.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{Ctx, GlobalFlags, agents_cmd, render};
use rimz::agents::AgentState;
use rimz::harness::budget::BudgetSpec;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SubagentsArgs {
    #[command(subcommand)]
    command: Option<SubagentsSubcmd>,
    #[command(flatten)]
    launch: SubagentLaunchArgs,
    /// Emit the caller's child list as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum SubagentsSubcmd {
    /// Launch one supervised child agent.
    Launch(SubagentLaunchArgs),
    /// List this agent's children.
    #[command(alias = "ls")]
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
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
    /// Restart one live child in place, resuming its session.
    Restart {
        #[arg(value_name = "NAME")]
        name: String,
    },
}

#[derive(Debug, Default, PartialEq, Args)]
#[command(
    after_help = "Launch several children in parallel, then join them with `rimz subagents wait`. The printed petname is also an address: use `rimz message @petname \"…\"` for a follow-up."
)]
struct SubagentLaunchArgs {
    /// Agent kind or configured profile.
    #[arg(
        value_name = "SPEC",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    spec: Option<String>,
    /// Complete task prompt supplied by the parent agent.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
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
    /// Let the child ask before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long)]
    yolo: bool,
    /// Cap this child's spend.
    #[arg(long, value_name = "AMOUNT[/day]")]
    budget: Option<BudgetSpec>,
    /// Stop the child after this duration.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
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
    require_agent_caller(crate::cli::send::agent_caller())?;
    match args.command {
        Some(SubagentsSubcmd::Launch(launch)) => launch_child(launch, globals),
        Some(SubagentsSubcmd::List { json }) => list_children(json, globals),
        Some(SubagentsSubcmd::Wait {
            names,
            any,
            timeout,
            stream,
            json,
        }) => wait_children(names, any, timeout, stream, json, globals),
        Some(SubagentsSubcmd::Stop { names, all }) => stop_children(names, all, globals),
        Some(SubagentsSubcmd::Restart { name }) => restart_child(&name, globals),
        None if args.launch.spec.is_some() => {
            if args.json {
                bail!("--json is only supported with `rimz subagents` and `rimz subagents list`");
            }
            launch_child(args.launch, globals)
        }
        None => {
            reject_launch_flags_without_spec(&args.launch)?;
            list_children(args.json, globals)
        }
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

fn launch_child(args: SubagentLaunchArgs, globals: &GlobalFlags) -> Result<()> {
    let config = rimz::config::MachineConfig::load().context("loading machine config")?;
    let launch = args.into_agent_launch(&config.agents.subagents)?;
    agents_cmd::run(agents_cmd::AgentsArgs::from_launch(launch), globals)
}

impl SubagentLaunchArgs {
    fn into_agent_launch(
        self,
        defaults: &rimz::config::SubagentsConfig,
    ) -> Result<agents_cmd::AgentLaunchArgs> {
        let spec = self.spec.context("a subagent needs an agent spec")?;
        let prompt = self
            .prompt
            .filter(|prompt| !prompt.trim().is_empty())
            .context("a subagent needs its prompt from the parent")?;
        let timeout = self
            .timeout
            .map(Ok)
            .unwrap_or_else(|| crate::cli::supervised::parse_timeout(&defaults.timeout))
            .map_err(anyhow::Error::msg)
            .context("parsing agents.subagents.timeout")?;
        let budget = match self.budget {
            Some(budget) => Some(budget),
            None => defaults
                .budget
                .as_deref()
                .map(str::parse)
                .transpose()
                .context("parsing agents.subagents.budget")?,
        };
        Ok(agents_cmd::AgentLaunchArgs {
            spec: Some(spec),
            prompt: Some(prompt),
            cohort: agents_cmd::CohortLaunchArgs {
                description: self.description,
                budget,
                bg: true,
                ..Default::default()
            },
            name: self.name,
            ask: self.ask,
            yolo: self.yolo,
            model: self.model,
            agent: self.agent,
            effort: self.effort,
            print: true,
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
        || args.name.is_some()
        || args.model.is_some()
        || args.agent.is_some()
        || args.effort.is_some()
        || args.ask
        || args.yolo
        || args.budget.is_some()
        || args.timeout.is_some()
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
    let caller = rimz::harness::plan::resolve_launch_caller_from_env(agents)?;
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
    let mut table = render::Table::new(["SUBAGENT", "KIND", "STATUS", "RUN"]);
    for child in reports {
        table.row([
            render::cell(child.handle),
            render::cell(child.kind),
            render::cell(child.status),
            render::cell(child.run_status.unwrap_or_else(|| "-".to_owned())).dash(),
        ]);
    }
    table.render(&mut render::out()).map_err(Into::into)
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
    let references = if names.is_empty() {
        children
            .into_iter()
            .filter(|child| child.ended_at.is_none())
            .map(child_reference)
            .collect::<Vec<_>>()
    } else {
        resolve_child_names(&children, &names)?
            .into_iter()
            .map(child_reference)
            .collect()
    };
    if references.is_empty() {
        bail!("this agent has no live subagents to wait for");
    }
    agents_cmd::wait_agent(references, any, timeout, stream, false, json, globals)
}

fn stop_children(names: Vec<String>, all: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (_, all_children) = caller_and_children(&snapshot.agents)?;
    let children = if all {
        all_children
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
        match agents_cmd::stop_resolved(&ctx, globals, child, &mut tracker) {
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

fn restart_child(name: &str, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (_, children) = caller_and_children(&snapshot.agents)?;
    let child = resolve_child_names(&children, &[name.to_owned()])?
        .into_iter()
        .next()
        .context("restart requires one child")?;
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let message = agents_cmd::restart_resolved(&ctx, child, &peers)?;
    writeln!(render::out(), "{message}")?;
    Ok(())
}

fn resolve_child_names<'a>(
    children: &[&'a AgentState],
    names: &[String],
) -> Result<Vec<&'a AgentState>> {
    names
        .iter()
        .map(|name| {
            let reference = name
                .strip_prefix('@')
                .unwrap_or(name)
                .split('#')
                .next()
                .unwrap_or(name);
            let mut matches = children.iter().copied().filter(|child| {
                child.name.as_deref() == Some(reference)
                    || child.agent_id.as_str() == reference
                    || child.launch_id.as_deref() == Some(reference)
            });
            let child = matches
                .next()
                .with_context(|| format!("`{name}` is not one of this agent's live subagents"))?;
            if matches.next().is_some() {
                bail!("subagent name `{name}` is ambiguous");
            }
            Ok(child)
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
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(flatten)]
        args: SubagentsArgs,
    }

    #[derive(Debug, Parser)]
    struct AgentsHarness {
        #[command(flatten)]
        args: agents_cmd::AgentsArgs,
    }

    fn parse(argv: &[&str]) -> SubagentsArgs {
        Harness::try_parse_from(argv)
            .expect("parse subagents command")
            .args
    }

    #[test]
    fn launch_implies_supervised_background_defaults() {
        let args = parse(&["rimz", "claude", "review this", "--effort", "high"]);
        let launch = args
            .launch
            .into_agent_launch(&rimz::config::SubagentsConfig::default())
            .expect("launch payload");
        let agents = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "review this",
            "--effort",
            "high",
            "-p",
            "--bg",
            "--timeout",
            "30m",
        ])
        .expect("parse equivalent agents launch")
        .args;

        assert_eq!(launch, agents.launch);
    }

    #[test]
    fn launch_requires_parent_prompt() {
        let args = parse(&["rimz", "claude"]);
        let error = args
            .launch
            .into_agent_launch(&rimz::config::SubagentsConfig::default())
            .expect_err("missing prompt");
        assert!(error.to_string().contains("prompt from the parent"));
    }

    #[test]
    fn command_is_agent_only() {
        let error = require_agent_caller(false).expect_err("human caller");
        assert!(error.to_string().contains("rimz agents <spec>"));
    }

    #[test]
    fn lifecycle_verbs_parse() {
        assert!(matches!(
            parse(&["rimz", "wait", "swift-otter", "--any"]).command,
            Some(SubagentsSubcmd::Wait { any: true, .. })
        ));
        assert!(matches!(
            parse(&["rimz", "stop", "--all"]).command,
            Some(SubagentsSubcmd::Stop { all: true, .. })
        ));
        assert!(matches!(
            parse(&["rimz", "restart", "swift-otter"]).command,
            Some(SubagentsSubcmd::Restart { .. })
        ));
    }
}
