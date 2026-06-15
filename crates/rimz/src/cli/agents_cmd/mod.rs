//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

mod auto_continue;
mod commands;
mod exec;
mod launch;
mod supervised;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

use super::{GlobalFlags, RoomTarget};
use rimz::agents::AgentAdapter;
use rimz::agents_spec::{Cell, LayoutSpec};
use rimz::bridge::{self, ExpectedRunFrame, SocketGuard};
use rimz::config::TabPlacement;
use rimz::feed::AgentState;
use rimz::ids::{AgentKind, AgentSessionId, EventId};
use rimz::ledger::{AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};
use rimz::message::{DeliveryGate, gate_open};
use rimz::mux::{LayoutPanes, PaneCmd, SplitPaneOptions, TabOptions, own_pane_id};
use rimz::run::{PermissionMode, RunRecord, RunStatus};
use rimz::workspace::WorkspaceResolver;

use auto_continue::{AutoContinueArgs, run_auto_continue};
use commands::*;
use exec::run_exec;
use launch::*;

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const RUN_MONITOR_POLL: Duration = Duration::from_millis(250);
const RUN_EXIT_TERMINAL_GRACE: Duration = Duration::from_millis(500);
static CLEANUP_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

type LaunchIdentity = AgentLaunchIdentity;

struct LaunchEventParams<'a> {
    cwd: &'a Path,
    worktree_name: Option<&'a str>,
    prompt: Option<&'a str>,
    state: rimz::schema::event::AgentLaunchState,
    pane_id: Option<rimz::ids::PaneId>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    /// Layout spec or named layout (`claude,codex+term`).
    #[arg(value_name = "SPEC")]
    spec: Option<String>,
    /// Prompt broadcast to every launched agent cell.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Use a Rimz-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(long, short = 'w', value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,
    /// Durable name for a single launched agent.
    #[arg(long)]
    name: Option<String>,
    /// Launch in the background, leaving focus on the launching pane.
    #[arg(long)]
    bg: bool,
    /// Split the agent into the current view instead of a new tab. Single
    /// agent cell only; rejected for a multi-cell layout.
    #[arg(long)]
    same_tab: bool,
    /// Open the launch in a new tab/window instead of the current view.
    #[arg(long, conflicts_with = "same_tab")]
    new_tab: bool,
    /// Let the agent ask before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long)]
    yolo: bool,
    /// Replace each agent's base system prompt with a file's contents.
    #[arg(long, value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,
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
    /// How `--print` reads the prompt (text positional, or stream-json on stdin).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    input_format: Option<InputFormat>,
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
    /// The positional `PROMPT` argument.
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
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Show one agent card.
    Show {
        reference: String,
        #[arg(long)]
        json: bool,
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
    Exec(ExecArgs),
    /// Hidden helper the producer spawns to nudge a rate-limit-parked agent when
    /// its window resets (`sidebar::enrich` auto-continue).
    #[command(hide = true)]
    AutoContinue(AutoContinueArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    /// Resume a prior agent session by id instead of launching fresh — the
    /// argv resume-on-rebirth panes run ([`rimz::resume::plan_resume`]).
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "prompt")]
    resume: Option<String>,
    #[arg(long)]
    run_id: Option<rimz::RunId>,
    #[arg(long)]
    agent_name: Option<String>,
    /// The `[agents.aliases]` role this agent launched as, stamped into
    /// `RIMZ_AGENT_ALIAS` so it answers to `@<alias>`.
    #[arg(long)]
    agent_alias: Option<String>,
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

pub fn run(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(exec, globals),
        Some(AgentsSubcmd::AutoContinue(args)) => return run_auto_continue(args),
        Some(AgentsSubcmd::List {
            json,
            all,
            worktree,
        }) => return list_agents(json, all, worktree, globals),
        Some(AgentsSubcmd::Show { reference, json }) => {
            return show_agent(reference, json, globals);
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
    if args.print {
        return match run_print(args, globals) {
            Ok(()) => Ok(()),
            Err(err) => exit_print_usage_error(err),
        };
    }
    if args.json {
        bail!(
            "--json is only supported with `rimz agents` and `rimz agents list`; on `-p`, choose output with `--output-format json`"
        );
    }
    launch_layout(args, globals)
}

fn exit_print_usage_error(err: anyhow::Error) -> ! {
    let _ = writeln!(std::io::stderr().lock(), "rimz: {err:#}");
    std::process::exit(2);
}

impl AgentsArgs {
    /// A minimal single-agent launch for create-on-miss: the resolved kind or
    /// role `spec`, the message as the first `prompt`, and the channel
    /// `worktree`. Everything else defaults so the launch lands where the
    /// address pointed, under the per-machine tab policy.
    fn for_create(spec: String, prompt: Option<String>, worktree: Option<String>) -> Self {
        Self {
            command: None,
            spec: Some(spec),
            prompt,
            worktree,
            name: None,
            bg: false,
            same_tab: false,
            new_tab: false,
            ask: false,
            yolo: false,
            system_prompt_file: None,
            effort: None,
            print: false,
            timeout: None,
            keep: false,
            detach: false,
            json: false,
            output_format: None,
            input_format: None,
            passthrough: Vec::new(),
        }
    }

    /// A blocking lowest-effort `ping`→`pong` supervised run for auto-ping: the
    /// plain `kind`, `"ping"` as the prompt, and `--effort low` (mapped to each
    /// provider's own flag in [`rimz::agents::AgentAdapter::render_preset`]). This
    /// matches the `<kind>-ping` virtual cell but in supervised mode, so the
    /// transient card appears, pongs, and self-clears.
    fn for_ping(kind: &str, worktree: Option<&str>) -> Self {
        Self {
            command: None,
            spec: Some(kind.to_owned()),
            prompt: Some("ping".to_owned()),
            worktree: worktree.map(ToOwned::to_owned),
            name: None,
            bg: false,
            same_tab: false,
            new_tab: false,
            ask: false,
            yolo: false,
            system_prompt_file: None,
            effort: Some("low".to_owned()),
            print: true,
            timeout: None,
            keep: false,
            detach: false,
            json: false,
            output_format: None,
            input_format: None,
            passthrough: Vec::new(),
        }
    }
}

/// Drive one blocking window-priming ping for `rimz autoping run`. Routes through
/// the same supervised `-p` path an interactive `rimz agents <kind> -p` uses:
/// it brings the room up if it is down, spawns the transient ping pane, waits for
/// the turn, closes the pane, and exits with the run's status code. `globals`
/// carries the schedule's `--root`, so the workspace resolves with no mux pin.
pub(crate) fn run_blocking_ping(
    kind: &str,
    worktree: Option<&str>,
    globals: &GlobalFlags,
) -> Result<()> {
    run_print(AgentsArgs::for_ping(kind, worktree), globals)
}

/// Launch a missing agent for `steer`/`queue --create`. A *type* handle — a kind
/// (`@codex`) or an `[agents.aliases]` role (`@planner`) — opens a fresh agent in
/// the addressed channel with the message as its first prompt; the channel names
/// (or creates) a worktree when it differs from the current one. An instance
/// handle (pet name, ordinal, session id) or a pane/`@all` address refuses,
/// because it names something that must already exist.
pub(crate) fn create_on_miss(
    target: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
    text: &str,
    globals: &GlobalFlags,
) -> Result<()> {
    let Some(create) = rimz::target::create_mention(target, worktree_flag, current_channel)? else {
        bail!(
            "`{target}` cannot create an agent; address a kind or role like `@codex` or `@planner`"
        );
    };
    let machine_config = crate::cli::machine_config()?;
    if !is_launchable_type(&create.selector, &machine_config.agents.aliases) {
        bail!(
            "`{target}` names a specific agent that is not running; create one with `@<kind>` or a role from [agents.aliases]"
        );
    }
    // A channel other than the current one names (or creates) its worktree; the
    // current channel launches in place.
    let worktree = create
        .channel
        .filter(|channel| Some(channel.as_str()) != current_channel);
    let prompt = (!text.trim().is_empty()).then(|| text.to_owned());
    launch_layout(
        AgentsArgs::for_create(create.selector, prompt, worktree),
        globals,
    )
}

/// Whether `selector` names a launchable *type* handle: a known agent kind
/// (`@codex`) or an `[agents.aliases]` *agent* role (`@planner`). A command
/// alias names a raw pane, not an addressable agent, and carries no kind to
/// staff a channel — so `--create` refuses it, the same as a pet name or
/// ordinal that must already exist.
fn is_launchable_type(selector: &str, aliases: &rimz::config::AliasesConfig) -> bool {
    rimz::agents::find_adapter(selector).is_some()
        || matches!(
            aliases.0.get(selector),
            Some(rimz::config::Alias::Agent { .. })
        )
}

#[cfg(test)]
mod tests;
