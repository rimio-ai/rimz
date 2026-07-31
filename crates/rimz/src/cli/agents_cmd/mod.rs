//! `rimz agents` — command parsing and agent-operation boundaries.

mod attribution;
mod auto_continue;
mod auto_redeem;
mod budget;
mod budget_park;
mod check;
mod exec;
mod fork;
mod history;
mod idle_compact;
pub(in crate::cli) mod launch;
mod list;
mod logs;
mod placement;
mod reconcile;
mod refresh;
mod refresh_context;
mod refresh_usage;
mod register;
mod report;
mod restart;
mod resume;
mod run_timeout;
mod runs_lookup;
mod show;
mod stop;
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
pub(super) use crate::cli::Ctx;
use crate::cli::supervised;
use rimz::agents::{AgentState, LifecycleRefreshRequest};
use rimz::harness::AutoContinueRequest;
use rimz::harness::auto_redeem::AutoRedeemRequest;
use rimz::harness::budget::BudgetParkRequest;
use rimz::harness::idle_compact::IdleCompactRequest;
use rimz::harness::plan::{
    LaunchFinalizeOptions, LayoutPaneParams, Placement, ResolvedLaunch, apply_in_place_downgrade,
    cohort_cells, compile_layout_panes, launch_identity_requests, mint_launch_id,
    resolve_fork_placement, resolve_placement, validate_agent_name,
};
use rimz::harness::resume::{PostureDegrade, ResumePosture};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus, SupervisedRunOutcome};
use rimz::harness::run_timeout::RunTimeoutRequest;
use rimz::harness::spec::{AgentCell, Cell, LayoutSpec};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::{DeliveryGate, gate_open};
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd, SplitPaneOptions, own_pane_id};
use rimz::room::{RoomContext, RoomSizing};
use rimz::sidebar::refresh::usage::AccountUsageRefreshRequest;
use rimz::store::{
    AgentLaunchBatch, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest, AgentLaunchScope,
};
use rimz::workspace::WorkspaceResolver;

use attribution::attribution;
use auto_continue::run_auto_continue;
use auto_redeem::run_auto_redeem;
use budget::{BudgetArgs, run_budget};
use budget_park::run_budget_park;
use check::{CheckArgs, run_check};
use exec::run_exec;
use fork::{ForkArgs, run_fork};
use history::history_agent;
use idle_compact::run_idle_compact;
use launch::*;
use list::list_agents;
pub(crate) use list::render_agents_table;
use logs::logs_agent;
use refresh::{RefreshArgs, run_refresh};
use refresh_usage::run_refresh_usage;
use register::{RegisterArgs, run_register};
use restart::restart_agent;
pub(in crate::cli) use restart::restart_resolved;
use resume::resume_lane;
use run_timeout::run_timeout;
pub(in crate::cli) use show::focus_resolved;
use show::{focus_agent, show_agent};
pub(in crate::cli) use stop::StopTracker;
use stop::stop_agent;
pub(in crate::cli) use stop::stop_resolved;
use supervised::OutputFormat;
use supervised::run::{run_print, run_supervised};
use top::{TopArgs, run_top};
pub(in crate::cli) use wait::wait_agent;

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const RUN_MONITOR_POLL: Duration = Duration::from_millis(250);
const RUN_EXIT_TERMINAL_GRACE: Duration = Duration::from_millis(500);
static CLEANUP_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static INTERRUPT_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

type LaunchIdentity = AgentLaunchIdentity;

#[derive(Debug, Default, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    #[command(flatten)]
    pub(crate) launch: AgentLaunchArgs,
    /// Print JSON for `list` and bare `agents` card output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Default, PartialEq, Args)]
pub(crate) struct CohortLaunchArgs {
    /// Seed launched cards' descriptions until agents name their own sessions.
    #[arg(long, value_name = "TEXT")]
    pub(crate) description: Option<String>,
    /// Use a RimZ-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "channel",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    pub(crate) worktree: Option<String>,
    /// Launch into a durable named channel.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "worktree",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
    )]
    pub(crate) channel: Option<String>,
    /// Create or reuse a RimZ-owned worktree from a pull request number or URL.
    #[arg(long = "from-pr", value_name = "PR", value_parser = parse_pr, conflicts_with = "channel")]
    pub(crate) from_pr: Option<rimz::forge::PrTarget>,
    /// Resume (alias --continue) a matching prior cohort, optionally scoped by -w or cwd.
    #[arg(
        long,
        visible_alias = "continue",
        conflicts_with_all = ["prompt", "channel", "from_pr", "description", "budget"]
    )]
    pub(crate) resume: bool,
    /// Cap each member's spend for the session (`5`) or local day (`20/day`).
    #[arg(long, value_name = "AMOUNT[/day]")]
    pub(crate) budget: Option<rimz::harness::budget::BudgetSpec>,
    /// Launch without taking focus; supervised runs print the agent name and return immediately.
    #[arg(long)]
    pub(crate) bg: bool,
    /// Open the launch in a new tab/window instead of the current view.
    #[arg(long)]
    pub(crate) new_tab: bool,
}

#[derive(Debug, Default, PartialEq, Args)]
pub(crate) struct AgentLaunchArgs {
    /// Inline spec, named team, or team role (`claude,codex+term`, `forge.planner`).
    #[arg(
        value_name = "SPEC",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    pub(crate) spec: Option<String>,
    /// Prompt delivered to the layout's leader agent (a team's `leader` role,
    /// defaulting to its first role; otherwise the first agent cell).
    #[arg(value_name = "PROMPT")]
    pub(crate) prompt: Option<String>,
    #[command(flatten)]
    pub(crate) cohort: CohortLaunchArgs,
    /// Durable name for a single launched agent.
    #[arg(long, short = 'n', conflicts_with = "resume")]
    pub(crate) name: Option<String>,
    /// Split the agent into a new pane in the current tab instead of taking
    /// over the current pane. Single agent cell only.
    #[arg(long, conflicts_with = "new_tab")]
    pub(crate) new_pane: bool,
    /// Let the agent ask before tool use where supported.
    #[arg(long, conflicts_with_all = ["yolo", "resume"])]
    pub(crate) ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long, conflicts_with = "resume")]
    pub(crate) yolo: bool,
    /// Model for the launched agents.
    #[arg(long, value_name = "MODEL", conflicts_with = "resume")]
    pub(crate) model: Option<String>,
    /// Re-base the spec's agent cells onto this profile or provider kind.
    #[arg(long, value_name = "PROFILE|KIND", conflicts_with = "resume")]
    pub(crate) agent: Option<String>,
    /// Replace each agent's base system prompt with a file's contents.
    #[arg(long, value_name = "PATH", conflicts_with = "resume")]
    pub(crate) system_prompt_file: Option<PathBuf>,
    /// Append these files in order after the replacement system prompt.
    #[arg(
        long = "append-system-prompt-file",
        value_name = "PATH",
        action = clap::ArgAction::Append,
        conflicts_with = "resume"
    )]
    pub(crate) append_system_prompt_files: Vec<PathBuf>,
    /// Reasoning effort for the launched agents (provider-specific levels).
    #[arg(long, value_name = "LEVEL", conflicts_with = "resume")]
    pub(crate) effort: Option<String>,
    /// Run one supervised agent prompt and print its final answer.
    #[arg(short = 'p', long = "print", conflicts_with = "resume")]
    pub(crate) print: bool,
    /// Internal lifecycle policy for callers whose child must remain
    /// self-cleaning even when the supervised run blocks.
    #[arg(skip)]
    pub(crate) self_cleanup_on_completion: bool,
    /// Internal launch posture for the `rimz subagents` doorway.
    #[arg(skip)]
    pub(crate) subagent: bool,
    /// Read stdin to EOF as prompt content. With a positional prompt, the
    /// instruction goes first and stdin follows inside `<stdin>` tags.
    #[arg(long, requires = "print")]
    pub(crate) stdin: bool,
    /// Wait cap for `--print` or `wait`.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout, requires = "print")]
    pub(crate) timeout: Option<Duration>,
    /// Leave the supervised agent pane open after completion.
    #[arg(long, requires = "print")]
    pub(crate) keep: bool,
    /// How `--print` renders the supervised run (text, json, stream-json).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    pub(crate) output_format: Option<OutputFormat>,
    #[arg(skip)]
    pub(crate) stream_text: bool,
    /// How `--print` reads the prompt (text positional plus explicit stdin, or
    /// stream-json on stdin).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    pub(crate) input_format: Option<InputFormat>,
    /// Maximum agentic turns for one supervised print-mode prompt.
    #[arg(long, value_name = "N", requires = "print")]
    pub(crate) max_turns: Option<u32>,
    /// Retry a failed (exit 1) supervised run up to N more times, feeding the previous failure tail back into the prompt.
    #[arg(long, value_name = "N", requires = "print", conflicts_with = "bg")]
    pub(crate) retries: Option<u32>,
    /// Verify a completed supervised run with a shell command and re-prompt the same session on failure.
    #[arg(long, value_name = "CMD", requires = "print", conflicts_with = "bg")]
    pub(crate) verify: Option<String>,
    /// Total agent turns allowed while making --verify pass.
    #[arg(long, value_name = "N", requires = "verify")]
    pub(crate) max_attempts: Option<u32>,
    /// Internal loop-scheduler placement: target the rimzd loop zone.
    #[arg(skip)]
    pub(crate) loop_zone: bool,
    /// Internal loop-task provenance for supervised run records.
    #[arg(skip)]
    pub(crate) loop_task: Option<String>,
    /// Extra argv appended to every launched agent cell.
    #[arg(last = true, conflicts_with = "resume")]
    pub(crate) passthrough: Vec<String>,
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

impl AgentsArgs {
    pub(crate) fn from_launch(launch: AgentLaunchArgs) -> Self {
        Self {
            launch,
            ..Default::default()
        }
    }

    /// The low-cardinality command label and, for a session-scoped helper, its
    /// session id and agent kind — for the Sentry command scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>, Option<&str>) {
        match &self.command {
            Some(AgentsSubcmd::RefreshContext(args)) => (
                "agents refresh-context",
                Some(args.request.session_id.as_str()),
                Some(args.request.kind.as_str()),
            ),
            _ => ("agents", None, None),
        }
    }
}

#[derive(Debug, Subcommand)]
enum AgentsSubcmd {
    /// Launch an agent, inline layout, configured profile, or team.
    #[command(
        group = clap::ArgGroup::new("launch-spec")
            .required(true)
            .args(["spec"])
    )]
    Launch(Box<AgentLaunchArgs>),
    /// Validate one third-party plugin's manifest, probes, and envelopes.
    Check(CheckArgs),
    /// Scaffold or validate a machine-tier third-party agent plugin.
    Register(RegisterArgs),
    /// List agent profiles available to launch.
    Profiles {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Include each profile's defining file path.
        #[arg(long)]
        path: bool,
    },
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
            short = 'w',
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
    /// Credit the agents and models that worked a lane.
    Attribution {
        /// Scope to one lane: `#channel`, worktree, branch, or directory name.
        #[arg(
            value_name = "SCOPE",
            conflicts_with = "all",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
        )]
        scope: Option<String>,
        /// Include every lane, not just the current channel.
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long, conflicts_with = "md")]
        json: bool,
        /// Emit a markdown block for a pull-request body.
        #[arg(long)]
        md: bool,
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
        #[arg(long, value_parser = crate::cli::supervised::parse_timeout)]
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
            conflicts_with_all = ["from_pr", "worktree"],
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
        )]
        scope: Option<String>,
        /// Filter to one lane by worktree name or path (flag spelling of SCOPE).
        #[arg(
            long,
            short = 'w',
            value_name = "NAME",
            conflicts_with = "from_pr",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
        )]
        worktree: Option<String>,
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
    AutoContinue(HelperRequestArgs<AutoContinueRequest>),
    /// Hidden helper the producer spawns to compact an eligible idle agent
    /// before its provider prompt cache expires.
    #[command(hide = true)]
    IdleCompact(HelperRequestArgs<IdleCompactRequest>),
    /// Hidden helper the producer spawns to redeem an account-wide Codex reset
    /// credit after rechecking current provider state.
    #[command(hide = true)]
    AutoRedeem(HelperRequestArgs<AutoRedeemRequest>),
    /// Hidden helper that interrupts an agent after its dollar cap is crossed.
    #[command(hide = true)]
    BudgetPark(HelperRequestArgs<BudgetParkRequest>),
    /// Hidden helper that settles a supervised run after its durable deadline.
    #[command(hide = true)]
    RunTimeout(HelperRequestArgs<RunTimeoutRequest>),
    /// Hidden helper the producer spawns to refresh one provider's account usage
    /// (rate-limit windows + paid credits) into the shared cache.
    #[command(hide = true)]
    RefreshUsage(HelperRequestArgs<AccountUsageRefreshRequest>),
    /// Hidden helper an installed hook spawns to refresh one session's context
    /// sidecar from its provider's out-of-band source.
    #[command(hide = true)]
    RefreshContext(HelperRequestArgs<LifecycleRefreshRequest>),
}

fn parse_helper_request<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|err| err.to_string())
}

#[derive(Debug, Args)]
struct HelperRequestArgs<T>
where
    T: Clone + Send + Sync + serde::de::DeserializeOwned + 'static,
{
    #[arg(
        long,
        hide = true,
        value_name = "JSON",
        value_parser = parse_helper_request::<T>
    )]
    request: T,
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    #[arg(long)]
    worktree_path: Option<PathBuf>,
    #[arg(long, hide = true, value_name = "JSON")]
    request: String,
}

pub fn run(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    let AgentsArgs {
        command,
        launch,
        json,
    } = args;
    match command {
        Some(AgentsSubcmd::Launch(launch)) => {
            // The required `launch-spec` Clap group guarantees an explicit launch spec.
            let spec = launch
                .spec
                .as_deref()
                .expect("required launch-spec group guarantees a spec");
            match top_level_spec_route(spec) {
                TopLevelSpecRoute::ScopedList => bail!(
                    "`rimz agents launch` requires a launch spec, not scope `{spec}`; use `rimz agents list {spec}`"
                ),
                TopLevelSpecRoute::Address => bail!(
                    "`{spec}` is an agent address, not a launch spec; try `rimz agents show {spec}` or `rimz message {spec} \"…\"`"
                ),
                TopLevelSpecRoute::Launch => {}
            }
            return dispatch_launch(*launch, false, globals);
        }
        Some(AgentsSubcmd::Check(args)) => return run_check(args),
        Some(AgentsSubcmd::Register(args)) => return run_register(args),
        Some(AgentsSubcmd::Profiles { json, path }) => {
            let (config, sources) = rimz::config::MachineConfig::load_with_agent_spec_sources()
                .context("loading machine config")?;
            return crate::cli::profile_report::list_profiles(
                &config.agents.profiles,
                &config.agents.commands,
                &sources,
                rimz::config::effective::ProfileScope::Agents,
                json,
                path,
            );
        }
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(*exec, globals),
        Some(AgentsSubcmd::AutoContinue(args)) => return run_auto_continue(args.request),
        Some(AgentsSubcmd::IdleCompact(args)) => return run_idle_compact(args.request),
        Some(AgentsSubcmd::AutoRedeem(args)) => return run_auto_redeem(args.request),
        Some(AgentsSubcmd::BudgetPark(args)) => return run_budget_park(args.request),
        Some(AgentsSubcmd::RunTimeout(args)) => return run_timeout(args.request, globals),
        Some(AgentsSubcmd::RefreshUsage(args)) => return run_refresh_usage(args.request),
        Some(AgentsSubcmd::RefreshContext(args)) => return refresh_context::run(args.request),
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
        Some(AgentsSubcmd::Attribution {
            scope,
            all,
            json,
            md,
        }) => return attribution(scope, all, json, md, globals),
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
        Some(AgentsSubcmd::Resume {
            scope,
            worktree,
            from_pr,
            bg,
        }) => {
            return resume_lane(scope.or(worktree), from_pr, bg, globals);
        }
        Some(AgentsSubcmd::Refresh(args)) => return run_refresh(args, globals),
        None => {}
    }
    let args = AgentsArgs {
        command: None,
        launch,
        json,
    };
    if args.launch.spec.is_none() {
        reject_launch_flags_without_spec(&args)?;
        return list_agents(
            args.json,
            false,
            args.launch.cohort.worktree.clone(),
            globals,
        );
    }
    if let Some(spec) = args.launch.spec.as_deref() {
        match top_level_spec_route(spec) {
            TopLevelSpecRoute::ScopedList => {
                reject_launch_flags_without_spec(&args)?;
                if args.launch.prompt.is_some() {
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
    dispatch_launch(args.launch, args.json, globals)
}

fn dispatch_launch(launch: AgentLaunchArgs, json: bool, globals: &GlobalFlags) -> Result<()> {
    let args = AgentsArgs {
        command: None,
        launch,
        json,
    };
    if args.launch.print {
        let (request, presentation) = into_supervised_request(args)?;
        return match run_print(request, presentation, globals) {
            Ok(Some(record)) => std::process::exit(record.status.exit_code()),
            Ok(None) => Ok(()),
            Err(err) => exit_print_usage_error(err),
        };
    }
    if json {
        bail!(
            "--json is only supported with `rimz agents` and `rimz agents list`; on `-p`, choose output with `--output-format json`"
        );
    }
    launch_layout(args, globals, true)
}

pub(in crate::cli) struct BackgroundLaunch {
    pub name: String,
    pub run_id: rimz::RunId,
}

pub(in crate::cli) enum BackgroundLaunchOutcome {
    Launched(BackgroundLaunch),
    BudgetExceeded { reason: String },
}

pub(in crate::cli) fn launch_supervised_background(
    launch: AgentLaunchArgs,
    globals: &GlobalFlags,
) -> Result<BackgroundLaunchOutcome> {
    let args = AgentsArgs::from_launch(launch);
    let (request, presentation) = into_supervised_request(args)?;
    match run_supervised(request, presentation, globals)? {
        SupervisedRunOutcome::Background { agent_name, run_id } => {
            Ok(BackgroundLaunchOutcome::Launched(BackgroundLaunch {
                name: agent_name,
                run_id,
            }))
        }
        SupervisedRunOutcome::BudgetExceeded { reason } => {
            Ok(BackgroundLaunchOutcome::BudgetExceeded { reason })
        }
        SupervisedRunOutcome::Record(_) => {
            bail!("background supervised launch returned a blocking run record")
        }
    }
}

fn into_supervised_request(
    args: AgentsArgs,
) -> Result<(
    rimz::harness::run::SupervisedRunRequest,
    supervised::SupervisedPresentation,
)> {
    if args.json {
        bail!("on `-p`, choose output with `--output-format json` (`--json` is for `list`)");
    }
    let output_format = args.launch.output_format.unwrap_or_default();
    validate_supervised_output(&args, output_format)?;
    let prompt = resolve_print_prompt(&args, args.launch.input_format.unwrap_or_default())?;
    let permission_mode =
        interactive_permission_mode_from_flags(args.launch.ask, args.launch.yolo)?
            .unwrap_or(PermissionMode::Auto);
    let system_prompt_file = resolve_launch_prompt_file(
        args.launch.system_prompt_file.as_deref(),
        "--system-prompt-file",
    )?;
    let append_system_prompt_files =
        resolve_launch_prompt_files(&args.launch.append_system_prompt_files)?;
    let request = rimz::harness::run::SupervisedRunRequest {
        spec: args
            .launch
            .spec
            .context("supervised run requires an agent spec")?,
        prompt,
        description: args.launch.cohort.description,
        worktree: args.launch.cohort.worktree,
        from_pr: args.launch.cohort.from_pr,
        channel: args.launch.cohort.channel,
        name: args.launch.name,
        background: args.launch.cohort.bg,
        self_cleanup_on_completion: args.launch.cohort.bg || args.launch.self_cleanup_on_completion,
        subagent: args.launch.subagent,
        force_new_tab: args.launch.cohort.new_tab,
        permission_mode,
        agent: rimz::harness::plan::normalized_preset_value(args.launch.agent.as_deref()),
        model: args.launch.model,
        system_prompt_file,
        append_system_prompt_files,
        effort: args.launch.effort,
        budget: args.launch.cohort.budget,
        max_turns: args.launch.max_turns,
        timeout: args.launch.timeout,
        keep: args.launch.keep,
        retries: args.launch.retries.unwrap_or(0),
        verify: args.launch.verify,
        max_attempts: args.launch.max_attempts,
        loop_zone: args.launch.loop_zone,
        loop_task: args.launch.loop_task,
        passthrough: args.launch.passthrough,
        managed_launch: rimz::agents::ManagedLaunchState::PendingResolution,
    };
    Ok((
        request,
        supervised::SupervisedPresentation {
            output_format,
            stream_text: args.launch.stream_text,
        },
    ))
}

fn validate_supervised_output(args: &AgentsArgs, output_format: OutputFormat) -> Result<()> {
    if args.launch.cohort.bg && output_format == OutputFormat::StreamJson {
        bail!("--output-format stream-json cannot be combined with --bg");
    }
    if args.launch.retries.unwrap_or(0) > 0 && output_format == OutputFormat::StreamJson {
        bail!("--retries cannot be combined with --output-format stream-json; choose text or json");
    }
    if args.launch.verify.is_some() && output_format == OutputFormat::StreamJson {
        bail!("--verify cannot be combined with --output-format stream-json; choose text or json");
    }
    if args.launch.max_attempts == Some(0) {
        bail!("--max-attempts must be at least 1");
    }
    if args.launch.max_attempts.is_some() && args.launch.verify.is_none() {
        bail!("--max-attempts requires --verify");
    }
    Ok(())
}

fn resolve_print_prompt(args: &AgentsArgs, input_format: InputFormat) -> Result<String> {
    match input_format {
        InputFormat::Text => {
            let piped = if args.launch.stdin {
                crate::cli::send::read_stdin_prompt()?
            } else {
                crate::cli::send::warn_ignored_stdin();
                None
            };
            crate::cli::send::combine_text_prompt(
                args.launch.prompt.as_deref(),
                piped.as_deref(),
            )
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "expected a prompt for `rimz agents <spec> -p` (positional PROMPT or `--stdin`)"
                    )
                })
        }
        InputFormat::StreamJson => {
            if args.launch.stdin {
                bail!("--input-format stream-json already reads stdin; drop --stdin");
            }
            if args
                .launch
                .prompt
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty())
            {
                bail!(
                    "--input-format stream-json reads the prompt from stdin; drop the positional PROMPT"
                );
            }
            let prompt = supervised::read_stream_json_prompt(std::io::stdin().lock())
                .context("reading stream-json prompt from stdin")?;
            if prompt.trim().is_empty() {
                bail!("--input-format stream-json received no user message text on stdin");
            }
            Ok(prompt)
        }
    }
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
            launch: AgentLaunchArgs {
                spec: Some(spec),
                prompt,
                cohort: CohortLaunchArgs {
                    worktree,
                    channel,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

fn parse_pr(raw: &str) -> std::result::Result<rimz::forge::PrTarget, String> {
    rimz::forge::parse(raw)
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
    let launch = rimz::config::effective::load(
        &machine_config.agents,
        &machine_config.subagents.profiles,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    if !is_launchable_type(&create.selector, &launch.profiles) {
        launch.block_untrusted_reference(
            rimz::config::effective::ProfileScope::Agents,
            Some(&create.selector),
            &machine_config.agents.commands,
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
    rimz::agents::find_definition(selector).is_some() || profiles.0.contains_key(selector)
}

#[cfg(test)]
mod tests;
