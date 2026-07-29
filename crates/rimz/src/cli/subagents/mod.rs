//! `rimz subagents` — agent-only supervised child launch and lifecycle sugar.

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
    /// Emit the caller's child list as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum SubagentsSubcmd {
    /// Launch one supervised child agent.
    Launch(SubagentLaunchArgs),
    /// Launch children from a JSON task list.
    Fanout(FanoutArgs),
    /// List this agent's children.
    #[command(alias = "ls")]
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// List agent types available to launch.
    Types {
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
}

#[derive(Debug, Args)]
struct FanoutArgs {
    /// Tasks JSON; stdin when omitted.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
    /// Wait for every launched child and print its result.
    #[arg(long)]
    fg: bool,
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
    /// Stop the child after this duration.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Wait for the child and print its result.
    #[arg(long)]
    fg: bool,
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
        Some(SubagentsSubcmd::Fanout(fanout)) => fanout_children(fanout, globals),
        Some(SubagentsSubcmd::List { json }) => list_children(json, globals),
        Some(SubagentsSubcmd::Types { json }) => list_types(json),
        Some(SubagentsSubcmd::Wait {
            names,
            any,
            timeout,
            stream,
            json,
        }) => wait_children(names, any, timeout, stream, json, globals),
        Some(SubagentsSubcmd::Stop { names, all }) => stop_children(names, all, globals),
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
            Err(err) => {
                return Err(err).context(fanout_launch_error_context(index, &launched));
            }
        }
    }
    if !args.fg {
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
    }
    let names = launched.into_iter().map(|child| child.name).collect();
    agents_cmd::wait_agent_batch(names, args.json, globals)
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
            name: self.name,
            model: self.model,
            agent: self.agent,
            effort: self.effort,
            timeout,
            fg: false,
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
        Ok(agents_cmd::AgentLaunchArgs {
            spec: Some(spec),
            prompt: Some(prompt),
            cohort: agents_cmd::CohortLaunchArgs {
                description: self.description,
                bg: !self.fg,
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
        || args.name.is_some()
        || args.model.is_some()
        || args.agent.is_some()
        || args.effort.is_some()
        || args.timeout.is_some()
        || args.fg
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

#[derive(Debug, PartialEq, Eq, Serialize)]
struct AgentTypeReport {
    name: String,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
}

impl AgentTypeReport {
    fn detail(&self) -> String {
        let Some(agent) = &self.agent else {
            return "-".to_owned();
        };
        let posture = match (self.model.as_deref(), self.effort.as_deref()) {
            (Some(model), Some(effort)) => Some(format!("{model}@{effort}")),
            (Some(model), None) => Some(model.to_owned()),
            (None, Some(effort)) => Some(format!("@{effort}")),
            (None, None) => None,
        };
        posture.map_or_else(|| agent.clone(), |posture| format!("{agent} · {posture}"))
    }
}

fn available_types(config: &rimz::config::MachineConfig) -> Vec<AgentTypeReport> {
    let mut types = rimz::agents::known_kinds()
        .filter(|kind| !config.agents.profiles.0.contains_key(*kind))
        .map(|kind| AgentTypeReport {
            name: kind.to_owned(),
            source: "kind",
            agent: None,
            model: None,
            effort: None,
        })
        .collect::<Vec<_>>();
    types.extend(
        config
            .agents
            .profiles
            .0
            .iter()
            .map(|(name, profile)| AgentTypeReport {
                name: name.clone(),
                source: "profile",
                agent: Some(profile.agent.clone()),
                model: profile.model.clone(),
                effort: profile.effort.clone(),
            }),
    );
    types.extend(config.agents.commands.0.keys().map(|name| AgentTypeReport {
        name: name.clone(),
        source: "command",
        agent: None,
        model: None,
        effort: None,
    }));
    types
}

fn list_types(json: bool) -> Result<()> {
    let config = rimz::config::MachineConfig::load().context("loading machine config")?;
    let types = available_types(&config);
    if json {
        return render::json_pretty(&types);
    }
    let mut table = render::Table::new(["TYPE", "SOURCE", "DETAIL"]);
    for agent_type in types {
        let detail = agent_type.detail();
        table.row([
            render::cell(agent_type.name),
            render::cell(agent_type.source),
            render::cell(detail).dash(),
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
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use jiff::Timestamp;

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
        assert!(launch.self_cleanup_on_completion);
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
        let mut agents = agents;
        agents.launch.self_cleanup_on_completion = true;
        assert!(launch.subagent);
        assert_eq!(launch.prompt.as_deref(), Some("review this"));
        agents.launch.subagent = true;

        assert_eq!(launch, agents.launch);
    }

    #[test]
    fn foreground_launch_uses_the_supervised_blocking_path() {
        let args = parse(&["rimz", "claude", "review this", "--fg"]);
        let launch = args
            .launch
            .into_agent_launch(&rimz::config::SubagentsConfig::default())
            .expect("launch payload");
        let agents = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "review this",
            "-p",
            "--timeout",
            "30m",
        ])
        .expect("parse equivalent agents launch")
        .args;
        let mut agents = agents;
        agents.launch.self_cleanup_on_completion = true;
        assert!(launch.subagent);
        assert_eq!(launch.prompt.as_deref(), Some("review this"));
        agents.launch.subagent = true;

        assert_eq!(launch, agents.launch);
    }

    #[test]
    fn fanout_task_matches_the_single_launch_surface() {
        let fanout = parse(&[
            "rimz",
            "fanout",
            "tasks.json",
            "--timeout",
            "10m",
            "--keep",
            "--fg",
            "--json",
        ]);
        let Some(SubagentsSubcmd::Fanout(fanout)) = fanout.command else {
            panic!("fanout command");
        };
        assert_eq!(fanout.file, Some(PathBuf::from("tasks.json")));
        assert!(fanout.fg);
        assert!(fanout.json);
        let launches = parse_fanout_launches(
            r#"[{
                "spec": "claude",
                "prompt": "review this",
                "name": "auth-review",
                "model": "opus",
                "agent": "reviewer",
                "effort": "high",
                "timeout": "5m",
                "max_turns": 4,
                "description": "checks auth"
            }]"#,
            &fanout,
            &rimz::config::SubagentsConfig::default(),
        )
        .expect("fanout launch");
        let agents = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "review this",
            "--name",
            "auth-review",
            "--model",
            "opus",
            "--agent",
            "reviewer",
            "--effort",
            "high",
            "--timeout",
            "5m",
            "--max-turns",
            "4",
            "--description",
            "checks auth",
            "--keep",
            "-p",
            "--bg",
        ])
        .expect("parse equivalent agents launch")
        .args;
        let mut agents = agents;
        agents.launch.self_cleanup_on_completion = true;
        assert!(launches[0].subagent);
        assert_eq!(launches[0].prompt.as_deref(), Some("review this"));
        agents.launch.subagent = true;

        assert_eq!(launches, vec![agents.launch]);
    }

    #[test]
    fn fanout_timeout_precedence_is_task_then_flag_then_config() {
        let Some(SubagentsSubcmd::Fanout(flagged)) =
            parse(&["rimz", "fanout", "--timeout", "10m"]).command
        else {
            panic!("fanout command");
        };
        let defaults = rimz::config::SubagentsConfig {
            timeout: "20m".to_owned(),
        };

        let task = parse_fanout_launches(
            r#"[{"spec":"codex","prompt":"one","timeout":"5m"}]"#,
            &flagged,
            &defaults,
        )
        .expect("task timeout");
        assert_eq!(task[0].timeout, Some(Duration::from_secs(5 * 60)));

        let flag =
            parse_fanout_launches(r#"[{"spec":"codex","prompt":"one"}]"#, &flagged, &defaults)
                .expect("flag timeout");
        assert_eq!(flag[0].timeout, Some(Duration::from_secs(10 * 60)));

        let Some(SubagentsSubcmd::Fanout(unflagged)) = parse(&["rimz", "fanout"]).command else {
            panic!("fanout command");
        };
        let config = parse_fanout_launches(
            r#"[{"spec":"codex","prompt":"one"}]"#,
            &unflagged,
            &defaults,
        )
        .expect("config timeout");
        assert_eq!(config[0].timeout, Some(Duration::from_secs(20 * 60)));
    }

    #[test]
    fn fanout_validates_the_whole_task_list_before_launch() {
        let Some(SubagentsSubcmd::Fanout(fanout)) = parse(&["rimz", "fanout"]).command else {
            panic!("fanout command");
        };
        let defaults = rimz::config::SubagentsConfig::default();

        let empty = parse_fanout_launches("[]", &fanout, &defaults).expect_err("empty task list");
        assert!(empty.to_string().contains("at least one task"));

        let missing_prompt =
            parse_fanout_launches(r#"[{"spec":"codex","name":"auth"}]"#, &fanout, &defaults)
                .expect_err("missing prompt");
        assert!(format!("{missing_prompt:#}").contains("task 1 (auth)"));
        assert!(format!("{missing_prompt:#}").contains("prompt from the parent"));

        let duplicate = parse_fanout_launches(
            r#"[
                {"spec":"codex","prompt":"one","name":"auth"},
                {"spec":"claude","prompt":"two","name":"auth"}
            ]"#,
            &fanout,
            &defaults,
        )
        .expect_err("duplicate name");
        assert!(
            duplicate
                .to_string()
                .contains("task 2 repeats child name `auth`")
        );
    }

    #[test]
    fn unattended_launch_flags_are_rejected() {
        for args in [
            &["rimz", "claude", "review this", "--ask"][..],
            &["rimz", "claude", "review this", "--yolo"],
            &["rimz", "claude", "review this", "--budget", "5"],
        ] {
            assert!(
                Harness::try_parse_from(args).is_err(),
                "{args:?} must not be accepted"
            );
        }
    }

    #[test]
    fn available_types_include_kinds_profiles_and_commands_but_not_teams() {
        let mut config = rimz::config::MachineConfig::default();
        config.agents.profiles.0.insert(
            "planner".to_owned(),
            rimz::config::Profile {
                agent: "claude".to_owned(),
                mode: None,
                model: Some("fable".to_owned()),
                effort: Some("high".to_owned()),
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                args: None,
            },
        );
        config.agents.profiles.0.insert(
            "claude".to_owned(),
            rimz::config::Profile {
                agent: "claude".to_owned(),
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                args: None,
            },
        );
        config
            .agents
            .commands
            .0
            .insert("mytool".to_owned(), "mytool --chat".to_owned());
        config
            .agents
            .teams
            .0
            .insert("review".to_owned(), rimz::config::Team::default());

        let types = available_types(&config);

        assert!(types.iter().any(|entry| entry.source == "kind"));
        assert!(types.iter().any(|entry| {
            entry.name == "planner"
                && entry.source == "profile"
                && entry.detail() == "claude · fable@high"
        }));
        assert!(
            types
                .iter()
                .any(|entry| entry.name == "mytool" && entry.source == "command")
        );
        assert_eq!(
            types.iter().filter(|entry| entry.name == "claude").count(),
            1
        );
        assert!(
            types
                .iter()
                .any(|entry| entry.name == "claude" && entry.source == "profile")
        );
        assert!(!types.iter().any(|entry| entry.name == "review"));
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
    fn default_wait_keeps_finished_supervised_children() {
        let mut finished =
            rimz::agents::AgentState::stub("codex", "finished", rimz::agents::AgentStatus::Success);
        finished.name = Some("swift-otter".to_owned());
        finished.ended_at = Some(Timestamp::now());
        let mut running =
            rimz::agents::AgentState::stub("codex", "running", rimz::agents::AgentStatus::Running);
        running.name = Some("bright-owl".to_owned());
        let untracked = rimz::agents::AgentState::stub(
            "claude",
            "interactive",
            rimz::agents::AgentStatus::Idle,
        );
        let children = vec![&finished, &running, &untracked];

        let mut finished_run = rimz::harness::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-wait")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::harness::run::PermissionMode::Auto,
            "review".to_owned(),
            PathBuf::from("/tmp/subagent-wait"),
        );
        finished_run.agent_id = Some(finished.agent_id.clone());
        finished_run.agent_name = finished.name.clone();
        finished_run.status = rimz::harness::run::RunStatus::Completed;
        let mut running_run = rimz::harness::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/subagent-wait")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::harness::run::PermissionMode::Auto,
            "implement".to_owned(),
            PathBuf::from("/tmp/subagent-wait"),
        );
        running_run.agent_id = Some(running.agent_id.clone());
        running_run.agent_name = running.name.clone();
        let runs = [finished_run, running_run];

        assert_eq!(
            wait_references(&children, &runs, &[], false).expect("default join"),
            vec!["swift-otter", "bright-owl"]
        );
        assert_eq!(
            wait_references(&children, &runs, &[], true).expect("default any"),
            vec!["bright-owl"]
        );
    }

    #[test]
    fn explicit_child_resolution_uses_the_shared_address_grammar() {
        let mut child =
            rimz::agents::AgentState::stub("codex", "child", rimz::agents::AgentStatus::Running);
        child.name = Some("swift-otter".to_owned());
        child.channel = Some("review".to_owned());

        assert_eq!(
            resolve_child_names(&[&child], &["@swift-otter#review".to_owned()])
                .expect("qualified child"),
            vec![&child]
        );
        assert!(
            resolve_child_names(&[&child], &["@swift-otter#other".to_owned()]).is_err(),
            "wrong-channel child must not resolve"
        );
    }

    #[test]
    fn lifecycle_verbs_parse() {
        assert!(matches!(
            parse(&["rimz", "fanout", "tasks.json", "--fg"]).command,
            Some(SubagentsSubcmd::Fanout(FanoutArgs {
                fg: true,
                file: Some(_),
                ..
            }))
        ));
        assert!(matches!(
            parse(&["rimz", "wait", "swift-otter", "--any"]).command,
            Some(SubagentsSubcmd::Wait { any: true, .. })
        ));
        assert!(matches!(
            parse(&["rimz", "stop", "--all"]).command,
            Some(SubagentsSubcmd::Stop { all: true, .. })
        ));
    }
}
