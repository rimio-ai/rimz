//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

mod auto_continue;
mod commands;
mod exec;
mod launch;
mod refresh_usage;
mod supervised;

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

use super::GlobalFlags;
use crate::cli::room::RoomTarget;
use rimz::agents::AgentAdapter;
use rimz::agents::AgentState;
use rimz::bridge::{self, ExpectedRunFrame, SocketGuard};
use rimz::config::LaunchPlacement;
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::spec::{Cell, LayoutSpec};
use rimz::ids::{AgentKind, AgentSessionId, EventId};
use rimz::ledger::{AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};
use rimz::message::{DeliveryGate, gate_open};
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd, SplitPaneOptions, TabOptions, own_pane_id};
use rimz::workspace::WorkspaceResolver;

use auto_continue::{AutoContinueArgs, run_auto_continue};
pub(crate) use commands::render_agents_table;
#[cfg(test)]
use commands::{RunPlacement, run_placement, run_stop_should_cancel};
use commands::{
    focus_agent, list_agents, run_print, run_supervised, show_agent, stop_agent, wait_agent,
};
use exec::run_exec;
use launch::*;
use refresh_usage::{RefreshUsageArgs, run_refresh_usage};

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const RUN_MONITOR_POLL: Duration = Duration::from_millis(250);
const RUN_EXIT_TERMINAL_GRACE: Duration = Duration::from_millis(500);
static CLEANUP_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static INTERRUPT_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

type LaunchIdentity = AgentLaunchIdentity;

#[derive(Clone)]
struct LaunchEventParams<'a> {
    cwd: &'a Path,
    worktree_name: Option<&'a str>,
    channel: Option<&'a str>,
    prompt: Option<&'a str>,
    state: rimz::ledger::event::AgentLaunchState,
    pane_id: Option<rimz::ids::PaneId>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    /// Inline spec, named team, or team role (`claude,codex+term`, `pcr.planner`).
    #[arg(value_name = "SPEC")]
    spec: Option<String>,
    /// Prompt broadcast to every launched agent cell.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Seed the agent card's description line until the agent names its own session.
    #[arg(long, value_name = "TEXT")]
    description: Option<String>,
    /// Use a Rimz-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "channel"
    )]
    worktree: Option<String>,
    /// Launch into a durable named channel.
    #[arg(long, value_name = "NAME", conflicts_with = "worktree")]
    channel: Option<String>,
    /// Create or reuse a Rimz-owned worktree from a pull request number or URL.
    #[arg(long = "from-pr", value_name = "PR", value_parser = parse_pr, conflicts_with = "channel")]
    from_pr: Option<rimz::forge::PrTarget>,
    /// Durable name for a single launched agent.
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// Launch in the background, leaving focus on the launching pane.
    #[arg(long)]
    bg: bool,
    /// Split the agent into a new pane in the current tab instead of taking
    /// over the current pane. Single agent cell only.
    #[arg(long)]
    new_pane: bool,
    /// Open the launch in a new tab/window instead of the current view.
    #[arg(long, conflicts_with = "new_pane")]
    new_tab: bool,
    /// Let the agent ask before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long)]
    yolo: bool,
    /// Model for the launched agents.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// Replace each agent's base system prompt with a file's contents.
    #[arg(long, value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,
    /// Append a file's contents to each agent's base system prompt where supported.
    #[arg(long, value_name = "PATH")]
    append_system_prompt_file: Option<PathBuf>,
    /// Reasoning effort for the launched agents (provider-specific levels).
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Run one supervised agent prompt and print its final answer.
    #[arg(short = 'p', long = "print")]
    print: bool,
    /// Wait cap for `--print` or `wait`.
    #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout, requires = "print")]
    timeout: Option<Duration>,
    /// Leave the supervised agent pane open after completion.
    #[arg(long, requires = "print")]
    keep: bool,
    /// Launch the supervised run and print its agent name.
    #[arg(long, requires = "print")]
    detach: bool,
    /// Print JSON for `list` and bare `agents` card output.
    #[arg(long)]
    json: bool,
    /// How `--print` renders the supervised run (text, json, stream-json).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    output_format: Option<OutputFormat>,
    /// How `--print` reads the prompt (text positional plus piped stdin, or
    /// stream-json on stdin).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    input_format: Option<InputFormat>,
    /// Maximum agentic turns for one supervised print-mode prompt.
    #[arg(long, value_name = "N", requires = "print")]
    max_turns: Option<u32>,
    /// Extra argv appended to every launched agent cell.
    #[arg(last = true)]
    passthrough: Vec<String>,
}

/// Output projection for a supervised `--print` run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(super) enum OutputFormat {
    /// The final assistant message as plain text.
    #[default]
    Text,
    /// The full run record as pretty JSON.
    Json,
    /// Newline-delimited JSON run events (NDJSON).
    StreamJson,
}

/// Prompt source for a supervised `--print` run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(super) enum InputFormat {
    /// The positional `PROMPT` argument plus piped non-TTY stdin.
    #[default]
    Text,
    /// Stream-json user messages read from stdin until EOF.
    StreamJson,
}

#[derive(Debug, Subcommand)]
enum AgentsSubcmd {
    /// List agent cards in the current room.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
        #[arg(long, conflicts_with = "all")]
        worktree: Option<String>,
    },
    /// Show one agent card.
    Show {
        reference: String,
        #[arg(long)]
        json: bool,
        /// Also capture the agent pane's visible area.
        #[arg(long)]
        capture: bool,
        /// Keep ANSI colors/attributes in the capture.
        #[arg(long, requires = "capture")]
        ansi: bool,
    },
    /// Focus an agent pane.
    Focus { reference: String },
    /// Wait for a supervised run or for an interactive agent to become idle.
    Wait {
        reference: String,
        #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout)]
        timeout: Option<Duration>,
        #[arg(long)]
        stream: bool,
        #[arg(long, requires = "stream")]
        from_start: bool,
        #[arg(long, conflicts_with = "stream")]
        json: bool,
    },
    /// Stop a supervised run or close an agent pane.
    Stop { reference: String },
    /// Hidden wrapper used inside launched agent panes.
    #[command(hide = true)]
    Exec(Box<ExecArgs>),
    /// Hidden helper the producer spawns to nudge a parked agent when its resume
    /// condition is due (`sidebar::enrich` auto-continue).
    #[command(hide = true)]
    AutoContinue(AutoContinueArgs),
    /// Hidden helper the producer spawns to refresh one provider's account usage
    /// (rate-limit windows + paid credits) into the shared cache.
    #[command(hide = true)]
    RefreshUsage(RefreshUsageArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    /// Resume a prior agent session by id instead of launching fresh — the
    /// argv resume-on-rebirth panes run ([`rimz::harness::resume::plan_resume`]).
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "prompt")]
    resume: Option<String>,
    #[arg(long)]
    run_id: Option<rimz::RunId>,
    #[arg(long)]
    agent_name: Option<String>,
    /// The `[agents.profiles]` profile this agent launched as. The launch
    /// event makes the rollup answer to `@<profile>`; `RIMZ_AGENT_PROFILE`
    /// remains the pane's sender-attribution identity.
    #[arg(long)]
    agent_profile: Option<String>,
    /// The `[agents.teams]` role this agent launched as. The launch event
    /// makes the rollup answer to `@<role>`; `RIMZ_AGENT_ROLE` remains the
    /// pane's sender-attribution identity.
    #[arg(long)]
    agent_role: Option<String>,
    /// The `[agents.teams]` team this agent launched under. The launch event
    /// makes in-place members resolve inside `<dir>/<team>`.
    #[arg(long)]
    agent_team: Option<String>,
    /// The inline multi-agent launch cohort this agent belongs to.
    #[arg(long)]
    launch_group: Option<String>,
    /// The agent's order inside its launch cohort.
    #[arg(long)]
    launch_ordinal: Option<u32>,
    /// The named channel this agent launched under.
    #[arg(long)]
    agent_channel: Option<String>,
    /// The profile/CLI-selected model to stamp into lifecycle observations.
    #[arg(long)]
    agent_model: Option<String>,
    /// The profile/CLI-selected reasoning effort to stamp into lifecycle observations.
    #[arg(long)]
    agent_effort: Option<String>,
    #[arg(long)]
    launch_id: Option<String>,
    #[arg(long, hide = true)]
    exit_on_run_completion: bool,
    #[arg(long, hide = true)]
    close_pane_on_exit: bool,
    #[arg(long)]
    worktree_path: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(last = true)]
    extra_args: Vec<String>,
}

pub fn run(mut args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(*exec, globals),
        Some(AgentsSubcmd::AutoContinue(args)) => return run_auto_continue(args),
        Some(AgentsSubcmd::RefreshUsage(args)) => return run_refresh_usage(args, globals),
        Some(AgentsSubcmd::List {
            json,
            all,
            worktree,
        }) => return list_agents(json, all, worktree, globals),
        Some(AgentsSubcmd::Show {
            reference,
            json,
            capture,
            ansi,
        }) => {
            return show_agent(reference, json, capture, ansi, globals);
        }
        Some(AgentsSubcmd::Focus { reference }) => return focus_agent(reference, globals),
        Some(AgentsSubcmd::Wait {
            reference,
            timeout,
            stream,
            from_start,
            json,
        }) => return wait_agent(reference, timeout, stream, from_start, json, globals),
        Some(AgentsSubcmd::Stop { reference }) => return stop_agent(reference, globals),
        None => {}
    }
    if args.spec.is_none() {
        reject_launch_flags_without_spec(&args)?;
        return list_agents(args.json, false, args.worktree, globals);
    }
    default_virtual_ping_prompt(&mut args);
    if args.print {
        return match run_print(args, globals) {
            Ok(Some(record)) => std::process::exit(record.status.exit_code()),
            Ok(None) => Ok(()),
            Err(err) => exit_print_usage_error(err),
        };
    }
    if args.json {
        bail!(
            "--json is only supported with `rimz agents` and `rimz agents list`; on `-p`, choose output with `--output-format json`"
        );
    }
    launch_layout(args, globals, true)
}

fn default_virtual_ping_prompt(args: &mut AgentsArgs) {
    if args.prompt.is_none()
        && args
            .spec
            .as_deref()
            .is_some_and(rimz::harness::spec::virtual_ping_shape)
        && args.input_format.unwrap_or_default() != InputFormat::StreamJson
    {
        args.prompt = Some(rimz::harness::spec::PING_PROMPT.to_owned());
    }
}

fn exit_print_usage_error(err: anyhow::Error) -> ! {
    let _ = writeln!(std::io::stderr().lock(), "rimz: {err:#}");
    std::process::exit(2);
}

impl AgentsArgs {
    /// A minimal single-agent launch for create-on-miss: the resolved kind or
    /// profile `spec`, the message as the first `prompt`, and the channel
    /// `worktree`. Everything else defaults so the launch lands where the
    /// address pointed, under the per-machine tab policy.
    fn for_create(
        spec: String,
        prompt: Option<String>,
        worktree: Option<String>,
        channel: Option<String>,
    ) -> Self {
        Self {
            command: None,
            spec: Some(spec),
            prompt,
            description: None,
            worktree,
            channel,
            from_pr: None,
            name: None,
            bg: false,
            new_pane: false,
            new_tab: false,
            ask: false,
            yolo: false,
            model: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            effort: None,
            print: false,
            timeout: None,
            keep: false,
            detach: false,
            json: false,
            output_format: None,
            input_format: None,
            max_turns: None,
            passthrough: Vec::new(),
        }
    }

    /// A blocking supervised run for a scheduled loop task. It routes through
    /// the same `-p` path as an interactive print run, while the loop handler
    /// owns task validation, prompt-file reads, ping skipping, and one-shot
    /// cleanup.
    pub(crate) fn for_task(task: TaskRunArgs) -> Self {
        Self {
            command: None,
            spec: Some(task.spec),
            prompt: task.prompt,
            description: None,
            worktree: task.worktree,
            channel: None,
            from_pr: None,
            name: None,
            bg: false,
            new_pane: false,
            new_tab: false,
            ask: matches!(task.mode, Some(PermissionMode::Ask)),
            yolo: matches!(task.mode, Some(PermissionMode::Yolo)),
            model: None,
            system_prompt_file: task.system_prompt_file,
            append_system_prompt_file: None,
            effort: task.effort,
            print: true,
            timeout: task.timeout,
            keep: task.keep,
            detach: false,
            json: false,
            output_format: None,
            input_format: None,
            max_turns: None,
            passthrough: Vec::new(),
        }
    }
}

pub(crate) struct TaskRunArgs {
    pub(crate) spec: String,
    pub(crate) prompt: Option<String>,
    pub(crate) worktree: Option<String>,
    pub(crate) mode: Option<PermissionMode>,
    pub(crate) effort: Option<String>,
    pub(crate) system_prompt_file: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) keep: bool,
}

fn parse_pr(raw: &str) -> std::result::Result<rimz::forge::PrTarget, String> {
    rimz::forge::parse(raw)
}

/// Drive one blocking scheduled loop task for `rimz loop run`, returning the
/// stored run record so loop history can link to its transcript. Routes through
/// the same supervised `-p` path an interactive `rimz agents <spec> -p` uses.
/// `globals` carries the task's `root`, so the workspace resolves with no mux
/// pin.
pub(crate) fn run_blocking_task(
    args: AgentsArgs,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    run_supervised(args, globals)
}

/// Launch a missing agent for `message --create`. A *type* handle — a kind
/// (`@codex`) or an `[agents.profiles]` profile (`@planner`) — opens a fresh agent in
/// the addressed channel with the message as its first prompt; the channel names
/// (or creates) a worktree when it differs from the current one. An instance
/// handle (pet name, ordinal, session id) or a pane/`@all` address refuses,
/// because it names something that must already exist.
pub(crate) fn create_on_miss(
    target: &str,
    worktree_flag: Option<&str>,
    channel_flag: Option<&str>,
    current_channel: Option<&str>,
    text: &str,
    globals: &GlobalFlags,
) -> Result<()> {
    let channel_filter = worktree_flag.or(channel_flag);
    let Some(create) =
        rimz::harness::target::create_mention(target, channel_filter, current_channel)?
    else {
        bail!(
            "`{target}` cannot create an agent; address a kind or profile like `@codex` or `@planner`"
        );
    };
    let machine_config = crate::cli::machine_config();
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let profiles = effective_launch_profiles(&machine_config, &workspace)?;
    let teams = effective_launch_teams(&machine_config, &workspace)?;
    if !is_launchable_type(&create.selector, &profiles) {
        rimz::config::effective::block_untrusted_profile_reference(
            Some(&create.selector),
            &profiles,
            &machine_config.agents.commands,
            &teams,
            &workspace.project_root,
            &rimz::ledger::paths::config_home(),
        )?;
        bail!(
            "`{target}` names a specific agent that is not running; create one with `@<kind>` or a profile from [agents.profiles]"
        );
    }
    // `--worktree` keeps the historical worktree create-on-miss path. `--channel`
    // and inline `#name` create durable named lanes.
    let inline_named_channel = target
        .split_once('#')
        .is_some_and(|(_, channel)| rimz::channel::valid_name(channel));
    let current_named = std::env::var(rimz::harness::run::ENV_CHANNEL).ok();
    let named = channel_flag.is_some()
        || (worktree_flag.is_none() && inline_named_channel)
        || create
            .channel
            .as_deref()
            .is_some_and(|channel| current_named.as_deref() == Some(channel));
    let (worktree, channel) = if named {
        (None, create.channel)
    } else {
        (
            create
                .channel
                .filter(|channel| Some(channel.as_str()) != current_channel),
            None,
        )
    };
    let prompt = (!text.trim().is_empty()).then(|| text.to_owned());
    launch_layout(
        AgentsArgs::for_create(create.selector, prompt, worktree, channel),
        globals,
        false,
    )
}

/// Whether `selector` names a launchable *type* handle: a known agent kind
/// (`@codex`) or an `[agents.profiles]` profile (`@planner`). A command
/// name names a raw pane, not an addressable agent, and carries no kind to
/// staff a channel — so `--create` refuses it, the same as a pet name or
/// ordinal that must already exist.
fn is_launchable_type(selector: &str, profiles: &rimz::config::ProfilesConfig) -> bool {
    rimz::agents::find_adapter(selector).is_some() || profiles.0.contains_key(selector)
}

#[cfg(test)]
mod tests;
