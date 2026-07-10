//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

mod auto_continue;
mod budget;
mod budget_park;
mod check;
mod exec;
mod fork;
mod history;
mod launch;
mod list;
mod logs;
mod reconcile;
mod refresh;
mod refresh_usage;
mod register;
mod restart;
mod resume;
mod runs_lookup;
mod show;
mod stop;
mod supervised;
mod top;
mod wait;

use std::collections::BTreeMap;
use std::io::Write;
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
use rimz::harness::plan::{
    LayoutPaneParams, Placement, apply_in_place_downgrade, cohort_cells,
    fresh_resume_launch_requests, launch_identity_requests, layout_panes_with_names,
    mint_launch_id, resolve_placement, validate_agent_name,
};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::spec::{Cell, LayoutSpec};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::{DeliveryGate, gate_open};
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd, SplitPaneOptions, TabOptions, own_pane_id};
use rimz::store::{AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};
use rimz::workspace::WorkspaceResolver;

use auto_continue::{AutoContinueArgs, run_auto_continue};
use budget::{BudgetArgs, run_budget};
use budget_park::{BudgetParkArgs, run_budget_park};
use check::{CheckArgs, run_check};
use exec::run_exec;
use fork::{ForkArgs, run_fork};
use history::history_agent;
use launch::*;
use list::list_agents;
pub(crate) use list::render_agents_table;
use logs::logs_agent;
use refresh::{RefreshArgs, run_refresh};
use refresh_usage::{RefreshUsageArgs, run_refresh_usage};
use register::{RegisterArgs, run_register};
use restart::restart_agent;
use resume::resume_lane;
use show::{focus_agent, show_agent};
#[cfg(test)]
use stop::run_stop_should_cancel;
use stop::stop_agent;
#[cfg(test)]
use supervised::run::{RunPlacement, run_placement, validate_supervised_output};
use supervised::run::{run_print, run_supervised};
use top::{TopArgs, run_top};
use wait::wait_agent;

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
    state: rimz::store::event::AgentLaunchState,
    pane_id: Option<rimz::ids::PaneId>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    /// Inline spec, named team, or team role (`claude,codex+term`, `forge.planner`).
    #[arg(
        value_name = "SPEC",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    spec: Option<String>,
    /// Prompt delivered to the layout's leader agent (a team's `leader` role,
    /// defaulting to its first role; otherwise the first agent cell).
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
        conflicts_with = "channel",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
    /// Launch into a durable named channel.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "worktree",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
    )]
    channel: Option<String>,
    /// Create or reuse a Rimz-owned worktree from a pull request number or URL.
    #[arg(long = "from-pr", value_name = "PR", value_parser = parse_pr, conflicts_with = "channel")]
    from_pr: Option<rimz::forge::PrTarget>,
    /// Resume (alias --continue) a prior cohort matching SPEC, optionally scoped by -w or cwd.
    #[arg(
        long,
        visible_alias = "continue",
        conflicts_with_all = [
            "prompt",
            "channel",
            "from_pr",
            "name",
            "description",
            "model",
            "effort",
            "budget",
            "ask",
            "yolo",
            "system_prompt_file",
            "append_system_prompt_file",
            "print",
            "passthrough"
        ]
    )]
    resume: bool,
    /// Durable name for a single launched agent.
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// Launch in the background, leaving focus on the launching pane; with `-p`, print the run's agent name and return immediately.
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
    /// Cap agent spend for the session (`5`) or local day (`20/day`).
    #[arg(long, value_name = "AMOUNT[/day]")]
    budget: Option<rimz::harness::budget::BudgetSpec>,
    /// Run one supervised agent prompt and print its final answer.
    #[arg(short = 'p', long = "print")]
    print: bool,
    /// Read stdin to EOF as prompt content. With a positional prompt, the
    /// instruction goes first and stdin follows inside `<stdin>` tags.
    #[arg(long, requires = "print")]
    stdin: bool,
    /// Wait cap for `--print` or `wait`.
    #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout, requires = "print")]
    timeout: Option<Duration>,
    /// Leave the supervised agent pane open after completion.
    #[arg(long, requires = "print")]
    keep: bool,
    /// Print JSON for `list` and bare `agents` card output.
    #[arg(long)]
    json: bool,
    /// How `--print` renders the supervised run (text, json, stream-json).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    output_format: Option<OutputFormat>,
    /// How `--print` reads the prompt (text positional plus explicit stdin, or
    /// stream-json on stdin).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    input_format: Option<InputFormat>,
    /// Maximum agentic turns for one supervised print-mode prompt.
    #[arg(long, value_name = "N", requires = "print")]
    max_turns: Option<u32>,
    /// Retry a failed (exit 1) supervised run up to N more times, feeding the previous failure tail back into the prompt.
    #[arg(long, value_name = "N", requires = "print", conflicts_with = "bg")]
    retries: Option<u32>,
    /// Verify a completed supervised run with a shell command and re-prompt the same session on failure.
    #[arg(long, value_name = "CMD", requires = "print", conflicts_with = "bg")]
    verify: Option<String>,
    /// Total agent turns allowed while making --verify pass.
    #[arg(long, value_name = "N", requires = "verify")]
    max_attempts: Option<u32>,
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
    /// The positional `PROMPT` argument plus stdin when `--stdin` is passed.
    #[default]
    Text,
    /// Stream-json user messages read from stdin until EOF.
    StreamJson,
}

#[derive(Debug, Subcommand)]
enum AgentsSubcmd {
    /// Validate one third-party plugin's manifest, probes, and envelopes.
    Check(CheckArgs),
    /// Scaffold or validate a machine-tier third-party agent plugin.
    Register(RegisterArgs),
    /// List agent cards in the current room.
    #[command(aliases = ["ls", "ps"])]
    List {
        /// Scope to one lane: `#channel`, worktree, branch, or directory name.
        #[arg(
            value_name = "SCOPE",
            conflicts_with_all = ["worktree", "all"],
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
        )]
        scope: Option<String>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Include every lane, not just the current channel.
        #[arg(long)]
        all: bool,
        /// Filter to one lane by worktree name or path (flag spelling of SCOPE).
        #[arg(
            long,
            conflicts_with = "all",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
        )]
        worktree: Option<String>,
    },
    /// Show one agent card.
    #[command(alias = "inspect")]
    Show {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Also capture the agent pane's visible area.
        #[arg(long)]
        capture: bool,
        /// Keep ANSI colors/attributes in the capture.
        #[arg(long, requires = "capture")]
        ansi: bool,
    },
    /// Show one agent transcript.
    Logs {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
        /// Keep the last N chat lines.
        #[arg(short = 'n', long = "tail")]
        tail: Option<usize>,
        /// Print new lines as they land.
        #[arg(short = 'f', long, conflicts_with = "all")]
        follow: bool,
        /// Include prior-session history.
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show token usage and cost for each turn in an agent session.
    History {
        reference: String,
        /// Keep the last N turns.
        #[arg(short = 'n', long = "tail")]
        tail: Option<usize>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show live agent resource usage.
    Top(TopArgs),
    /// Focus an agent pane.
    Focus {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
    },
    /// Fork an agent with its full conversation history under a new session id.
    Fork(ForkArgs),
    /// Wait for supervised runs or interactive agents; several references join.
    Wait {
        #[arg(
            required = true,
            num_args = 1..,
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_refs)
        )]
        references: Vec<String>,
        /// Return when the first target finishes; print its name.
        #[arg(long, conflicts_with = "stream")]
        any: bool,
        /// Stop waiting after this duration.
        #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout)]
        timeout: Option<Duration>,
        /// Tail the transcript while waiting.
        #[arg(long)]
        stream: bool,
        /// Replay the transcript from the top before tailing.
        #[arg(long, requires = "stream")]
        from_start: bool,
        /// Emit a labeled result map for joins; with `--stream`, emit NDJSON run events.
        #[arg(long)]
        json: bool,
    },
    /// Stop a supervised run or close an agent pane.
    Stop {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
        /// Stop every agent the address matches.
        #[arg(long)]
        all: bool,
    },
    /// Stop an agent and relaunch it in place, resuming its session.
    Restart { reference: String },
    /// Resume a lane's closed agents where they left off.
    Resume {
        /// Lane to resume: `#channel`, worktree, branch, or directory name.
        #[arg(
            value_name = "SCOPE",
            conflicts_with = "from_pr",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
        )]
        scope: Option<String>,
        /// Resume the lane developed from this pull request (number or URL).
        #[arg(long, value_name = "PR", value_parser = parse_pr)]
        from_pr: Option<rimz::forge::PrTarget>,
        /// Open without focusing the resumed tab.
        #[arg(long)]
        bg: bool,
    },
    /// Force-refresh agent-card context from local transcripts and helpers.
    Refresh(RefreshArgs),
    /// Inspect or change one agent's dollar cap.
    Budget(BudgetArgs),
    /// Hidden wrapper used inside launched agent panes.
    #[command(hide = true)]
    Exec(Box<ExecArgs>),
    /// Hidden helper the producer spawns to nudge a parked agent when its resume
    /// condition is due (`sidebar::enrich` auto-continue).
    #[command(hide = true)]
    AutoContinue(AutoContinueArgs),
    /// Hidden helper that interrupts an agent after its dollar cap is crossed.
    #[command(hide = true)]
    BudgetPark(BudgetParkArgs),
    /// Hidden helper the producer spawns to refresh one provider's account usage
    /// (rate-limit windows + paid credits) into the shared cache.
    #[command(hide = true)]
    RefreshUsage(RefreshUsageArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    /// Fork a prior agent session by id: full history under a new
    /// provider-assigned session id.
    #[arg(
        long,
        value_name = "SESSION_ID",
        conflicts_with_all = ["resume", "prompt"]
    )]
    fork: Option<String>,
    /// Resume a prior agent session by id instead of launching fresh — the
    /// argv resume-on-rebirth panes run ([`rimz::harness::resume::plan_resume`]).
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "prompt")]
    resume: Option<String>,
    #[arg(long)]
    run_id: Option<rimz::RunId>,
    #[arg(long)]
    agent_name: Option<String>,
    #[arg(long, hide = true)]
    agent_name_explicit: bool,
    /// The `[agents.profiles]` profile this agent launched as. The launch
    /// event makes the rollup answer to `@<profile>`; `RIMZ_AGENT_PROFILE`
    /// remains the pane's sender-attribution identity.
    #[arg(long)]
    agent_profile: Option<String>,
    /// The permission posture selected for this launch.
    #[arg(long)]
    agent_mode: Option<PermissionMode>,
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
    /// The launch-selected canonical budget to stamp into lifecycle observations.
    #[arg(long)]
    agent_budget: Option<String>,
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
        Some(AgentsSubcmd::Check(args)) => return run_check(args),
        Some(AgentsSubcmd::Register(args)) => return run_register(args),
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(*exec, globals),
        Some(AgentsSubcmd::AutoContinue(args)) => return run_auto_continue(args),
        Some(AgentsSubcmd::BudgetPark(args)) => return run_budget_park(args),
        Some(AgentsSubcmd::RefreshUsage(args)) => return run_refresh_usage(args, globals),
        Some(AgentsSubcmd::Budget(args)) => return run_budget(args, globals),
        Some(AgentsSubcmd::List {
            scope,
            json,
            all,
            worktree,
        }) => return list_agents(json, all, scope.or(worktree), globals),
        Some(AgentsSubcmd::Show {
            reference,
            json,
            capture,
            ansi,
        }) => {
            return show_agent(reference, json, capture, ansi, globals);
        }
        Some(AgentsSubcmd::Logs {
            reference,
            tail,
            follow,
            all,
            json,
        }) => return logs_agent(reference, tail, follow, all, json, globals),
        Some(AgentsSubcmd::History {
            reference,
            tail,
            json,
        }) => return history_agent(reference, tail, json, globals),
        Some(AgentsSubcmd::Top(args)) => return run_top(args, globals),
        Some(AgentsSubcmd::Focus { reference }) => return focus_agent(reference, globals),
        Some(AgentsSubcmd::Fork(args)) => return run_fork(args, globals),
        Some(AgentsSubcmd::Wait {
            references,
            any,
            timeout,
            stream,
            from_start,
            json,
        }) => {
            return wait_agent(references, any, timeout, stream, from_start, json, globals);
        }
        Some(AgentsSubcmd::Stop { reference, all }) => return stop_agent(reference, all, globals),
        Some(AgentsSubcmd::Restart { reference }) => return restart_agent(reference, globals),
        Some(AgentsSubcmd::Resume { scope, from_pr, bg }) => {
            return resume_lane(scope, from_pr, bg, globals);
        }
        Some(AgentsSubcmd::Refresh(args)) => return run_refresh(args, globals),
        None => {}
    }
    if args.spec.is_none() {
        reject_launch_flags_without_spec(&args)?;
        return list_agents(args.json, false, args.worktree, globals);
    }
    if let Some(spec) = args.spec.as_deref() {
        match top_level_spec_route(spec) {
            TopLevelSpecRoute::ScopedList => {
                reject_launch_flags_without_spec(&args)?;
                if args.prompt.is_some() {
                    bail!(
                        "scope `{spec}` takes no prompt; use `rimz agents {spec}` or `rimz agents list {spec}`"
                    );
                }
                return list_agents(args.json, false, Some(spec.to_owned()), globals);
            }
            TopLevelSpecRoute::Address => bail!(
                "`{spec}` is an agent address, not a launch spec; try `rimz agents show {spec}`, `rimz message {spec} \"…\"`, or `rimz agents list`"
            ),
            TopLevelSpecRoute::Launch => {}
        }
    }
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

#[derive(Debug, PartialEq, Eq)]
enum TopLevelSpecRoute {
    ScopedList,
    Address,
    Launch,
}

fn top_level_spec_route(spec: &str) -> TopLevelSpecRoute {
    if spec.starts_with('#') {
        TopLevelSpecRoute::ScopedList
    } else if spec.starts_with('@') {
        TopLevelSpecRoute::Address
    } else {
        TopLevelSpecRoute::Launch
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
            resume: false,
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
            budget: None,
            print: false,
            stdin: false,
            timeout: None,
            keep: false,
            json: false,
            output_format: None,
            input_format: None,
            max_turns: None,
            retries: None,
            verify: None,
            max_attempts: None,
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
            resume: false,
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
            budget: task.budget,
            print: true,
            stdin: false,
            timeout: task.timeout,
            keep: task.keep,
            json: false,
            output_format: None,
            input_format: None,
            max_turns: None,
            retries: None,
            verify: task.verify,
            max_attempts: task.max_attempts,
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
    pub(crate) budget: Option<rimz::harness::budget::BudgetSpec>,
    pub(crate) system_prompt_file: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) keep: bool,
    pub(crate) verify: Option<String>,
    pub(crate) max_attempts: Option<u32>,
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
    let launch = effective_launch_agents(&machine_config, &workspace)?;
    if !is_launchable_type(&create.selector, &launch.profiles) {
        launch
            .block_untrusted_reference(Some(&create.selector), &machine_config.agents.commands)?;
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
