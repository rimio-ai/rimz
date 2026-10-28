//! `rimz agents` — command parsing and agent-operation boundaries.

mod attribution;
mod auto_continue;
mod auto_redeem;
mod budget;
mod budget_park;
mod cache_keepalive;
mod check;
mod compact;
mod exec;
mod explain;
mod fork;
mod hibernate;
mod history;
mod idle_compact;
mod idle_stop;
mod launch;
mod launch_resolve;
mod list;
mod logs;
mod molt;
mod orphan_subagent;
mod park_notice;
use rimz::launcher::placement;
mod refresh;
mod refresh_context;
mod refresh_usage;
mod register;
mod report;
mod restart;
mod resume;
mod run_command;
mod run_timeout;
mod runs_lookup;
mod session_step;
mod show;
mod stall_notice;
mod stop;
mod subagent_report;
pub(in crate::cli) mod team_report;
mod top;
mod validate;
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
use super::usage::{UsageError, shell_command};
pub(super) use crate::cli::Ctx;
use crate::cli::supervised;
use placement::{Placement, resolve_fork_placement};
use rimz::agents::PermissionMode;
use rimz::agents::{AgentState, LifecycleRefreshRequest};
use rimz::harness::AutoContinueRequest;
use rimz::harness::auto_redeem::AutoRedeemRequest;
use rimz::harness::budget::BudgetParkRequest;
use rimz::harness::idle_compact::IdleCompactRequest;
use rimz::harness::orphan_sweep::OrphanSubagentRequest;
#[cfg(test)]
use rimz::harness::plan::ResolvedLaunch;
use rimz::harness::plan::{mint_launch_id, validate_agent_name};
use rimz::harness::resume::{PostureDegrade, ResumePosture};
use rimz::harness::run::SupervisedRunOutcome;
use rimz::harness::run_timeout::RunTimeoutRequest;
use rimz::harness::spec::{AgentCell, Cell, LayoutSpec};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::mux::{LayoutColumn, LayoutPanes, PaneCmd, SplitPaneOptions, own_pane_id};
use rimz::room::{RoomContext, RoomSizing};
use rimz::sidebar::refresh::usage::AccountUsageRefreshRequest;
use rimz::store::run::{RunRecord, RunStatus};
use rimz::store::{
    writer::AgentLaunchBatch, writer::AgentLaunchIdentity, writer::AgentLaunchName,
    writer::AgentLaunchRequest, writer::AgentLaunchScope,
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
use idle_stop::run_idle_stop;
use launch::*;
use launch_resolve::*;
use list::list_agents;
pub(crate) use list::render_agents_table;
use logs::logs_agent;
use molt::{MoltArgs, run_molt};
use orphan_subagent::repair_orphan;
use refresh::{RefreshArgs, run_refresh};
use refresh_usage::run_refresh_usage;
use register::{RegisterArgs, run_register};
use restart::restart_agent;
pub(in crate::cli) use restart::{relaunch_action, relaunch_request, restart_resolved};
use resume::resume_lane;
use run_timeout::run_timeout;
pub(in crate::cli) use show::focus_resolved;
use show::{focus_agent, show_agent};
pub(in crate::cli) use stop::stop_resolved;
use stop::{WhenIdle, parse_when_idle, stop_agent};
use supervised::OutputFormat;
use supervised::run::{run_print, run_supervised};
use top::{TopArgs, run_top};
pub(in crate::cli) use wait::wait_agent;

fn agent_session_resumable(kind: &AgentKind, agent_id: &AgentSessionId, cwd: &Path) -> bool {
    !agent_id.is_provisional()
        && rimz::agents::find_definition(kind.as_str())
            .is_some_and(|adapter| adapter.resume_command(agent_id, cwd).is_some())
}

fn resolve_live_or_audit(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    reference: &str,
    channel: &rimz::address::AddressContext,
) -> Result<(AgentState, bool)> {
    let live_error =
        match super::resolve_agent_one_unhinted(Some(store), snapshot, reference, None, channel) {
            Ok(agent) => {
                return Ok((agent.clone(), true));
            }
            Err(error) if super::resolution_missed(&error) => error,
            Err(error) => return Err(error),
        };
    match show::resolve_audit_agent(store, workspace, runtime, reference, channel)? {
        Some(agent) => Ok((agent, false)),
        None => super::resolve_hint(reference, Err(live_error)),
    }
}

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
    /// Retired listing flag, kept so the no-verb refusal can echo it into `list`.
    #[arg(long, hide = true, conflicts_with_all = ["spec", "worktree"])]
    all: bool,
    /// Retired listing flag, kept so the no-verb refusal can echo it into `list`.
    #[arg(long, hide = true)]
    json: bool,
}

const COHORT_PROMPT_SHORTS: &[char] = &['w'];
const AGENT_PROMPT_SHORTS: &[char] = &['w', 'n'];

pub(super) fn parse_cohort_prompt(value: &str) -> Result<String, String> {
    parse_launch_prompt(value, COHORT_PROMPT_SHORTS)
}

fn parse_agent_prompt(value: &str) -> Result<String, String> {
    parse_launch_prompt(value, AGENT_PROMPT_SHORTS)
}

fn parse_launch_prompt(value: &str, shorts: &[char]) -> Result<String, String> {
    if value == "--resume" || value.starts_with("--resume=") {
        return Err("use --continue to reopen a cohort".to_owned());
    }
    if let Some(short) = value.strip_prefix('-').and_then(|rest| rest.chars().next())
        && shorts.contains(&short)
    {
        return Err(format!(
            "prompt starts with -{short}; write -{short} NAME with a space"
        ));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Default, PartialEq, Args)]
pub(crate) struct CohortLaunchArgs {
    /// Seed launched cards' descriptions until agents name their own sessions.
    #[arg(long, value_name = "TEXT")]
    pub(crate) description: Option<String>,
    /// Use a RimZ-owned worktree. NAME creates or checks an existing tree; bare -w generates a name.
    /// Give NAME without a leading #.
    ///
    /// For channel #feat-a, pass -w feat-a. With shell comments enabled, -w #feat-a becomes
    /// bare -w and generates a name; quoting #feat-a still passes an invalid worktree name.
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
    /// Launch into the named channel.
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
    /// Continue a matching prior cohort, optionally scoped by -w or cwd.
    #[arg(
        long = "continue",
        conflicts_with_all = ["channel", "from_pr", "description", "budget"]
    )]
    pub(crate) continue_cohort: bool,
    /// Launch new sessions in a named worktree without asking to enter it or resume a closed cohort.
    #[arg(long, conflicts_with_all = ["continue_cohort", "from_pr"])]
    pub(crate) fresh: bool,
    /// Cap each member's spend for the session (`5`) or local day (`20/day`).
    #[arg(long, value_name = "AMOUNT[/day]")]
    pub(crate) budget: Option<rimz::harness::budget::BudgetSpec>,
    /// Launch without taking focus; supervised runs print the agent name and return immediately.
    #[arg(long)]
    pub(crate) bg: bool,
    /// Open the launch in a new tab/window instead of the current view.
    #[arg(long)]
    pub(crate) new_tab: bool,
    /// Launch without a report and without holding the launching agent; the result stays readable with `wait`.
    #[arg(long)]
    pub(crate) detach: bool,
}

impl CohortLaunchArgs {
    fn validate_fresh_worktree(&self) -> Result<()> {
        if self.fresh
            && self
                .worktree
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
        {
            bail!("--fresh needs a named worktree (-w NAME)");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Args)]
pub(crate) struct LaunchOverrideArgs {
    /// Let the agent ask before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    pub(crate) ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long)]
    pub(crate) yolo: bool,
    /// Model for the agents; on resume, becomes the saved launch record.
    #[arg(long, value_name = "MODEL", conflicts_with = "tier")]
    pub(crate) model: Option<String>,
    /// Capability tier; on resume, picks a same-provider entry and saves the launch record.
    #[arg(long, value_name = "TIER")]
    pub(crate) tier: Option<rimz::config::tiers::ModelTier>,
    /// Prefer this provider kind, or re-base onto a replacement profile. On resume, must match the provider and becomes the saved launch record.
    #[arg(long, value_name = "PROFILE|KIND")]
    pub(crate) agent: Option<String>,
    /// Replace each agent's base system prompt with a file's contents.
    #[arg(long, value_name = "PATH")]
    pub(crate) system_prompt_file: Option<PathBuf>,
    /// Append these files in order after the replacement system prompt.
    #[arg(
        long = "append-system-prompt-file",
        value_name = "PATH",
        action = clap::ArgAction::Append
    )]
    pub(crate) append_system_prompt_files: Vec<PathBuf>,
    /// Reasoning effort for the launched agents (provider-specific levels).
    #[arg(long, value_name = "LEVEL")]
    pub(crate) effort: Option<String>,
    /// Launch in the provider's fast mode (Claude, Codex, Amp, Pi on OpenAI).
    /// Resumes replay it.
    #[arg(long, conflicts_with = "no_fast")]
    pub(crate) fast: bool,
    /// Request no fast mode, even where the profile sets `fast`; resumes replay it.
    #[arg(long)]
    pub(crate) no_fast: bool,
    /// Run the launched agents under this isolation instead of machine `agents.isolation`.
    #[arg(long, value_name = "host|scoped|sandbox")]
    pub(crate) isolation: Option<rimz::config::Isolation>,
    /// Extra argv appended to every launched agent cell.
    #[arg(last = true)]
    pub(crate) passthrough: Vec<String>,
}

impl LaunchOverrideArgs {
    /// The fast-mode override: `Some(true)` for `--fast`, `Some(false)` for
    /// `--no-fast`, `None` to keep the profile's setting.
    fn fast_override(&self) -> Option<bool> {
        match (self.fast, self.no_fast) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Default, PartialEq, Args)]
struct SessionResumeOverrides {
    /// Ask before tool use where supported (@handle only).
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Skip provider permission prompts where supported (@handle only).
    #[arg(long)]
    yolo: bool,
    /// Save this model for later resumes (@handle only).
    #[arg(long, value_name = "MODEL", conflicts_with = "tier")]
    model: Option<String>,
    /// Select a same-provider tier and save it (@handle only).
    #[arg(long, value_name = "TIER")]
    tier: Option<rimz::config::tiers::ModelTier>,
    /// Rebase onto a same-provider profile or kind (@handle only).
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
    /// Save this reasoning effort for later resumes (@handle only).
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Enable provider fast mode and save it (@handle only).
    #[arg(long, conflicts_with = "no_fast")]
    fast: bool,
    /// Disable provider fast mode and save it (@handle only).
    #[arg(long)]
    no_fast: bool,
    /// Replace the session's saved isolation (@handle only).
    #[arg(long, value_name = "host|scoped|sandbox")]
    isolation: Option<rimz::config::Isolation>,
}

impl SessionResumeOverrides {
    fn into_launch_overrides(self) -> LaunchOverrideArgs {
        LaunchOverrideArgs {
            ask: self.ask,
            yolo: self.yolo,
            model: self.model,
            tier: self.tier,
            agent: self.agent,
            effort: self.effort,
            fast: self.fast,
            no_fast: self.no_fast,
            isolation: self.isolation,
            ..Default::default()
        }
    }
}

#[derive(Debug, Default, PartialEq, Args)]
pub(crate) struct AgentLaunchArgs {
    /// Resume an exact provider session; omit the id to request its session picker.
    #[arg(
        long,
        value_name = "ID",
        num_args = 0..=1,
        value_parser = clap::builder::NonEmptyStringValueParser::new(),
        conflicts_with_all = [
            "continue_cohort", "prompt", "channel", "from_pr", "description", "budget",
            "worktree", "fresh", "detach", "cwd", "name", "print",
            "system_prompt_file", "append_system_prompt_files", "passthrough"
        ]
    )]
    pub(crate) resume: Option<Option<String>>,
    /// Working directory for the launched agent; the room stays the caller's.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["worktree", "from_pr", "continue_cohort", "fresh"])]
    pub(crate) cwd: Option<PathBuf>,
    /// Inline spec, named team, or team role (`claude,codex+term`, `forge.planner`).
    #[arg(
        value_name = "SPEC",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_specs)
    )]
    pub(crate) spec: Option<String>,
    /// Prompt delivered to the layout's leader agent (a team's `leader` role,
    /// defaulting to its first role; otherwise the first agent cell).
    /// On resume, queued as a user message after reopening.
    #[arg(value_name = "PROMPT", allow_hyphen_values = true, value_parser = parse_agent_prompt)]
    pub(crate) prompt: Option<String>,
    #[command(flatten)]
    pub(crate) cohort: CohortLaunchArgs,
    /// Durable name for a single launched agent.
    #[arg(long, short = 'n', conflicts_with = "continue_cohort")]
    pub(crate) name: Option<String>,
    /// Split the agent into a new pane in the current tab instead of taking
    /// over the current pane. Single agent cell only.
    #[arg(long, conflicts_with = "new_tab")]
    pub(crate) new_pane: bool,
    #[command(flatten)]
    pub(crate) overrides: LaunchOverrideArgs,
    /// Run one supervised agent prompt and print its final answer.
    #[arg(short = 'p', long = "print", conflicts_with = "continue_cohort")]
    pub(crate) print: bool,
    /// Internal lifecycle policy for callers whose child must remain
    /// self-cleaning even when the supervised run blocks.
    #[arg(skip)]
    pub(crate) self_cleanup_on_completion: bool,
    /// Internal launch posture for the `rimz subagents` doorway.
    #[arg(skip)]
    pub(crate) subagent: bool,
    /// Internal resume posture for the `rimz teams` doorways: a named lane
    /// matches by recorded lane, and refusals name teams-scene commands.
    #[arg(skip)]
    pub(crate) teams_scene: bool,
    /// Read stdin to EOF as prompt content. With a positional prompt, the
    /// instruction goes first and stdin follows inside `<stdin>` tags.
    #[arg(long, requires = "print")]
    pub(crate) stdin: bool,
    /// Wait cap for `--print` or `wait`.
    #[arg(long, value_parser = crate::cli::supervised::parse_timeout, requires = "print")]
    pub(crate) timeout: Option<Duration>,
    #[arg(skip)]
    pub(crate) warn: Vec<Duration>,
    #[arg(skip)]
    pub(crate) grace: Option<Duration>,
    /// Leave the supervised agent pane open after completion.
    #[arg(long, requires = "print")]
    pub(crate) keep: bool,
    /// How `--print` renders the supervised run (text, json, stream-json).
    #[arg(long, value_name = "FORMAT", requires = "print")]
    pub(crate) output_format: Option<OutputFormat>,
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

    /// `rimz agents launch <SPEC>`: the explicit verb, which a bare word's
    /// live-agent refusal never stops.
    pub(crate) fn launch_verb(launch: AgentLaunchArgs) -> Self {
        Self {
            command: Some(AgentsSubcmd::Launch(Box::new(launch))),
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
    /// Validate the machine's Markdown agent, subagent, and team definitions.
    Validate {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Launch an agent, inline layout, configured profile, or team.
    #[command(
        group = clap::ArgGroup::new("launch-spec")
            .required(true)
            .args(["spec"])
    )]
    Launch(Box<AgentLaunchArgs>),
    /// Print the launch plan for a profile, team role, or agent seat.
    Explain(explain::ExplainArgs),
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
        /// Include team role profiles (`<team>.<role>`), listed alone by `rimz teams profiles`.
        #[arg(long)]
        teams: bool,
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
        /// Agent to show: @handle, @handle#lane, pet name, or pane id (tmux:%12).
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
    /// Show the messages to and from one agent (its live screen: rimz agents show --capture)
    Logs {
        /// Whose conversation: @handle[#channel], a session id, or run_<id>. A bare name is read as @name.
        #[arg(value_name = "TARGET", add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
        /// Keep the last N entries (a thread keeps its opening message).
        #[arg(short = 'n', long = "tail", visible_alias = "last", value_name = "N")]
        tail: Option<usize>,
        /// Print new entries as they land.
        #[arg(short = 'f', long, conflicts_with = "all")]
        follow: bool,
        /// Include all retained history, including prior sessions.
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show token usage and cost for each turn in an agent session.
    History {
        /// Agent whose turns to show: @handle, @handle#lane, pet name, or pane id (tmux:%12).
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
        /// Scope to one lane: `#channel`, worktree, path, or directory name.
        #[arg(
            value_name = "SCOPE",
            conflicts_with = "all",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::scope_names)
        )]
        scope: Option<String>,
        /// Scope to one branch; defaults to the selected checkout's HEAD, none with `--all`.
        #[arg(long, value_name = "BRANCH")]
        branch: Option<String>,
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
        /// Agent to focus: @handle, @handle#lane, pet name, or pane id (tmux:%12).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: String,
    },
    /// Fork an agent with its full conversation history under a new session id.
    Fork(ForkArgs),
    /// Move yourself into a fresh session in a new tab (agent-only).
    #[command(
        long_about = "Run only inside an agent. Move into a fresh session in a new, unfocused tab with NOTE as its first prompt. Molting does not count toward max-chain-length. The old session stops once idle for 5m with nothing owed; typing into it keeps it."
    )]
    Molt(MoltArgs),
    /// Wait for supervised runs or interactive agents; several references join.
    Wait {
        /// Agents or runs to wait for: @handle, @handle#lane, pet name, pane id (tmux:%12), or run id.
        #[arg(
            required = true,
            num_args = 1..,
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_refs)
        )]
        references: Vec<String>,
        /// Return when the first target finishes; print its result block, or a one-entry map with `--json`.
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
        /// Agent or run to stop: @handle, @handle#lane, pet name, pane id (tmux:%12), or run id.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::agent_refs
        ))]
        reference: Option<String>,
        /// Stop every agent the address matches.
        #[arg(long)]
        all: bool,
        /// Stop the agent once it has rested this long with nothing owed
        /// (`3m` when bare); a prompt from the user withdraws the request, as does `off`.
        #[arg(
            long,
            value_name = "DURATION|off",
            num_args = 0..=1,
            default_missing_value = stop::WHEN_IDLE_DEFAULT,
            value_parser = parse_when_idle,
            conflicts_with = "all"
        )]
        when_idle: Option<WhenIdle>,
    },
    /// Stop an agent and relaunch it in place, resuming its session.
    Restart {
        /// Agent to restart: @handle, @handle#lane, pet name, or pane id (tmux:%12).
        reference: String,
    },
    /// Compact an agent's context at its next turn boundary.
    Compact {
        /// Agent to compact: @handle, @handle#lane, pet name, or pane id (tmux:%12).
        #[arg(add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_refs))]
        reference: String,
        /// Replace the configured compaction brief; an empty string sends the bare command.
        #[arg(allow_hyphen_values = true)]
        instruction: Option<String>,
    },
    /// Resume a closed agent or a lane's closed agents where they left off.
    Resume {
        /// Lane: `#channel`, worktree, branch, or directory; `@handle` for one closed session.
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
        /// Start new sessions in the lane's saved shape.
        #[arg(long)]
        fresh: bool,
        /// Open without focusing the resumed tab.
        #[arg(long)]
        bg: bool,
        #[command(flatten)]
        overrides: SessionResumeOverrides,
    },
    /// Force-refresh agent-card context from local transcripts and helpers.
    Refresh(RefreshArgs),
    /// Inspect or change one agent's dollar cap.
    Budget(BudgetArgs),
    /// Hidden wrapper used inside launched agent panes.
    #[command(hide = true)]
    Exec(Box<ExecArgs>),
    #[command(hide = true)]
    SessionStep(session_step::Args),
    #[command(hide = true)]
    RunCommand(run_command::Args),
    /// Hidden helper the producer spawns to nudge a parked agent when its resume
    /// condition is due (`sidebar::enrich` auto-continue).
    #[command(hide = true)]
    AutoContinue(HelperRequestArgs<AutoContinueRequest>),
    /// Hidden helper the producer spawns to compact an eligible idle agent
    /// before its provider prompt cache expires.
    #[command(hide = true)]
    IdleCompact(HelperRequestArgs<IdleCompactRequest>),
    /// Hidden helper the producer spawns to stop an agent whose
    /// `stop --when-idle` request is due.
    #[command(hide = true)]
    IdleStop(HelperRequestArgs<rimz::harness::idle_stop::IdleStopHelperRequest>),
    /// Hidden helper the producer spawns to freeze an eligible idle agent.
    #[command(hide = true)]
    Hibernate(HelperRequestArgs<rimz::harness::hibernate::HibernateHelperRequest>),
    /// Hidden helper that thaws a hibernating pane.
    #[command(hide = true)]
    Wake(HelperRequestArgs<rimz::harness::hibernate::WakeHelperRequest>),
    /// Hidden helper that samples refault counters after a thaw.
    #[command(hide = true)]
    WakeRefault(HelperRequestArgs<rimz::harness::hibernate::RefaultHelperRequest>),
    /// Hidden helper that refreshes a sleeping agent's prompt cache.
    #[command(hide = true)]
    CacheKeepalive(HelperRequestArgs<rimz::harness::cache_keepalive::CacheKeepaliveRequest>),
    /// Hidden helper the producer spawns to redeem an account-wide Codex reset
    /// credit after rechecking current provider state.
    #[command(hide = true)]
    AutoRedeem(HelperRequestArgs<AutoRedeemRequest>),
    /// Hidden helper that interrupts an agent after its dollar cap is crossed.
    #[command(hide = true)]
    BudgetPark(HelperRequestArgs<BudgetParkRequest>),
    /// Hidden helper that tells a parent its child stopped on a provider limit.
    #[command(hide = true)]
    ParkNotice(HelperRequestArgs<rimz::harness::park_notice::ParkNoticeRequest>),
    /// Hidden helper that tells a parent its child went silent past the stall window.
    #[command(hide = true)]
    StallNotice(HelperRequestArgs<rimz::harness::stall_notice::StallNoticeRequest>),
    /// Hidden helper that settles a supervised run after its durable deadline.
    #[command(hide = true)]
    RunTimeout(HelperRequestArgs<RunTimeoutRequest>),
    /// Hidden helper that records and repairs a subagent whose parent ended.
    #[command(hide = true)]
    OrphanSubagent(HelperRequestArgs<OrphanSubagentRequest>),
    /// Hidden helper that reconstructs a missed subagent fleet digest.
    #[command(hide = true)]
    SubagentDigest(HelperRequestArgs<SubagentDigestRequest>),
    /// Hidden helper for the parked wrapper's store-owning duties.
    #[command(hide = true)]
    SuperviseDuty(HelperRequestArgs<exec::SuperviseDutyRequest>),
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

#[derive(Clone, Debug, serde::Deserialize)]
struct SubagentDigestRequest {
    workspace_id: rimz::WorkspaceId,
    parent_agent_id: AgentSessionId,
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    #[arg(long)]
    worktree_path: Option<PathBuf>,
    #[arg(
        long,
        hide = true,
        value_name = "PATH",
        required_unless_present = "request",
        conflicts_with = "request"
    )]
    request_file: Option<PathBuf>,
    // A wrapper started by a pre-request-file binary must still park under the new one.
    // Remove this legacy flag in the following release.
    #[arg(
        long,
        hide = true,
        value_name = "JSON",
        required_unless_present = "request_file"
    )]
    request: Option<String>,
    #[arg(long, hide = true)]
    supervise: Option<PathBuf>,
}

pub fn run(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    let AgentsArgs {
        command,
        launch,
        all,
        json,
    } = args;
    match command {
        Some(AgentsSubcmd::Validate { json }) => return validate::run(json),
        Some(AgentsSubcmd::Launch(launch)) => {
            // The required `launch-spec` Clap group guarantees an explicit launch spec.
            let spec = launch
                .spec
                .as_deref()
                .expect("required launch-spec group guarantees a spec");
            match top_level_spec_route(spec) {
                TopLevelSpecRoute::Scope => return Err(scope_refusal(spec, false).into()),
                TopLevelSpecRoute::Address => {
                    return Err(UsageError::new(format!(
                        "`{spec}` is an agent address, not a launch spec; try `rimz agents show {spec}` or `rimz message {spec} \"…\"`"
                    ))
                    .into());
                }
                TopLevelSpecRoute::Launch => {}
            }
            return dispatch_launch(*launch, false, globals, false);
        }
        Some(AgentsSubcmd::Explain(args)) => return explain::run(args, globals),
        Some(AgentsSubcmd::Check(args)) => return run_check(args),
        Some(AgentsSubcmd::Register(args)) => return run_register(args),
        Some(AgentsSubcmd::Profiles { json, path, teams }) => {
            let (config, sources) = rimz::config::MachineConfig::load_with_agent_spec_sources()
                .context("loading machine config")?;
            crate::cli::report_definition_errors(&config)?;
            let mut reports = crate::cli::profile_report::available_profiles(
                &config.agents.profiles,
                &config.agents.commands,
                &sources,
                rimz::config::effective::ProfileScope::Agents,
            );
            let mut team_profiles_hidden = false;
            if !teams {
                let (team, standalone) = crate::cli::profile_report::partition_team_profiles(
                    reports,
                    &config.agents.teams,
                );
                team_profiles_hidden = !team.is_empty();
                reports = standalone;
            }
            return crate::cli::profile_report::list_profiles(
                reports,
                crate::cli::profile_report::ProfileListing::Agents {
                    team_profiles_hidden,
                },
                json,
                path,
            );
        }
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(*exec, globals),
        Some(AgentsSubcmd::SessionStep(args)) => return session_step::run(args),
        Some(AgentsSubcmd::RunCommand(args)) => return run_command::run(args, globals),
        Some(AgentsSubcmd::AutoContinue(args)) => return run_auto_continue(args.request),
        Some(AgentsSubcmd::IdleCompact(args)) => return run_idle_compact(args.request),
        Some(AgentsSubcmd::IdleStop(args)) => return run_idle_stop(args.request, globals),
        Some(AgentsSubcmd::Hibernate(args)) => return hibernate::run(args.request),
        Some(AgentsSubcmd::Wake(args)) => return hibernate::wake(args.request),
        Some(AgentsSubcmd::WakeRefault(args)) => {
            rimz::harness::hibernate::sample_wake_refault(args.request);
            return Ok(());
        }
        Some(AgentsSubcmd::CacheKeepalive(args)) => return cache_keepalive::run(args.request),
        Some(AgentsSubcmd::AutoRedeem(args)) => return run_auto_redeem(args.request),
        Some(AgentsSubcmd::BudgetPark(args)) => return run_budget_park(args.request),
        Some(AgentsSubcmd::ParkNotice(args)) => return park_notice::run(args.request),
        Some(AgentsSubcmd::StallNotice(args)) => return stall_notice::run(args.request),
        Some(AgentsSubcmd::RunTimeout(args)) => return run_timeout(args.request, globals),
        Some(AgentsSubcmd::OrphanSubagent(args)) => {
            return repair_orphan(args.request, globals);
        }
        Some(AgentsSubcmd::SubagentDigest(args)) => {
            return subagent_report::backstop_digest(args.request);
        }
        Some(AgentsSubcmd::SuperviseDuty(args)) => return exec::run_supervise_duty(args.request),
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
            branch,
            all,
            json,
            md,
        }) => return attribution(scope, branch, all, json, md, globals),
        Some(AgentsSubcmd::Top(args)) => return run_top(args, globals),
        Some(AgentsSubcmd::Focus { reference }) => return focus_agent(reference, globals),
        Some(AgentsSubcmd::Fork(args)) => return run_fork(args, globals),
        Some(AgentsSubcmd::Molt(args)) => return run_molt(args, globals),
        Some(AgentsSubcmd::Wait {
            references,
            any,
            timeout,
            stream,
            from_start,
            json,
        }) => {
            let references = references
                .into_iter()
                .map(|reference| (reference.clone(), reference))
                .collect();
            return wait_agent(references, any, timeout, stream, from_start, json, globals);
        }
        Some(AgentsSubcmd::Stop {
            reference,
            all,
            when_idle,
        }) => return stop_agent(reference, all, when_idle, globals),
        Some(AgentsSubcmd::Restart { reference }) => return restart_agent(reference, globals),
        Some(AgentsSubcmd::Compact {
            reference,
            instruction,
        }) => {
            return compact::compact_agent(reference, instruction, globals);
        }
        Some(AgentsSubcmd::Resume {
            scope,
            worktree,
            from_pr,
            bg,
            fresh,
            overrides,
        }) => {
            if let Some(reference) = scope.as_deref().filter(|scope| scope.starts_with('@')) {
                if fresh {
                    return Err(UsageError::new(
                        "--fresh takes a lane scope, not an agent reference; drop --fresh to resume this session".to_owned(),
                    ).into());
                }
                return resume::resume_agent(reference, bg, overrides, globals);
            }
            if overrides != SessionResumeOverrides::default() {
                return Err(UsageError::new(
                    "launch overrides apply to one closed session (@handle); to change a lane's settings use rimz agents <spec> --continue or rimz teams resume".to_owned(),
                ).into());
            }
            return resume_lane(scope.or(worktree), from_pr, bg, fresh, globals);
        }
        Some(AgentsSubcmd::Refresh(args)) => return run_refresh(args, globals),
        None => {}
    }
    if let Some(refusal) = launch_refusal(&launch, all, json) {
        return Err(refusal.into());
    }
    if let Some(spec) = launch.spec.as_deref()
        && top_level_spec_route(spec) == TopLevelSpecRoute::Address
    {
        return Err(UsageError::new(format!(
            "`{spec}` is an agent address, not a launch spec; try `rimz agents show {spec}`, `rimz message {spec} \"…\"`, or `rimz agents list`"
        ))
        .into());
    }
    if let Some(word) = lone_launch_word(&launch)
        && let Some((snapshot, current_channel)) = live_room_snapshot(globals)
        && let Some(refusal) = collision_refusal(
            word,
            &snapshot.pane_bound_roots().collect::<Vec<_>>(),
            &snapshot,
            &current_channel,
        )
    {
        return Err(refusal.into());
    }
    dispatch_launch(launch, json, globals, true)
}

fn dispatch_launch(
    launch: AgentLaunchArgs,
    json: bool,
    globals: &GlobalFlags,
    bare: bool,
) -> Result<()> {
    let args = AgentsArgs {
        command: None,
        launch,
        all: false,
        json,
    };
    let loop_task = rimz::harness::schedule::runner::loop_check_task();
    validate_detach(&args.launch, args.launch.print || loop_task.is_some())?;
    if args.launch.print || loop_task.is_some() {
        let (request, presentation) = if let Some(task) = loop_task {
            into_loop_check_request(args, &task, &rimz::config::MachineConfig::load_lenient())?
        } else {
            into_supervised_request(args)?
        };
        return match run_print(request, presentation, globals) {
            Ok(Some(record)) => std::process::exit(record.status.exit_code()),
            Ok(None) => Ok(()),
            Err(err) => exit_print_usage_error(err),
        };
    }
    if json {
        bail!(
            "--json is only supported with `rimz agents list`; on `-p`, choose output with `--output-format json`"
        );
    }
    launch_layout(args, globals, true)
        .map_err(|error| crate::cli::render::launch_suggestions::agents_error(error, bare))
}

/// `--detach` drops the report a launch task owes; refuse it where no such report exists.
fn validate_detach(launch: &AgentLaunchArgs, print: bool) -> Result<()> {
    if !launch.cohort.detach {
        return Ok(());
    }
    if launch.cohort.continue_cohort {
        bail!(
            "--detach cannot be combined with --continue: a resumed cohort has no launch task to detach; drop --detach"
        );
    }
    if print {
        if !launch.cohort.bg {
            bail!(
                "--detach on `-p` requires --bg: a foreground run prints its answer inline; add --bg, or drop --detach"
            );
        }
        return Ok(());
    }
    if launch
        .prompt
        .as_deref()
        .is_none_or(|prompt| prompt.trim().is_empty())
    {
        bail!(
            "--detach requires a prompt: without a launch task no report is owed; add a prompt, or drop --detach"
        );
    }
    Ok(())
}

fn into_loop_check_request(
    mut args: AgentsArgs,
    task: &str,
    config: &rimz::config::MachineConfig,
) -> Result<(
    rimz::harness::run::SupervisedRunRequest,
    supervised::SupervisedPresentation,
)> {
    if args.launch.prompt.is_none() {
        bail!("a loop check launches an agent with a prompt: `rimz agents <cell> \"<prompt>\"`");
    }
    if args.launch.cohort.bg {
        bail!("a loop check waits for its agent; remove `--bg` to capture the completed run");
    }
    args.launch.print = true;
    let (mut request, presentation) = into_supervised_request(args)?;
    rimz::harness::schedule::runner::shape_loop_owned(
        &mut request,
        task,
        config,
        rimz::harness::schedule::run_log::LoopRunMode::Scheduled,
    )?;
    Ok((request, presentation))
}

pub(in crate::cli) struct BackgroundLaunch {
    pub name: String,
    pub run_id: rimz::RunId,
    pub response_path: Option<PathBuf>,
}

pub(in crate::cli) enum BackgroundLaunchOutcome {
    Launched(BackgroundLaunch),
    BudgetExceeded { reason: String },
    Aborted,
}

pub(in crate::cli) fn launch_supervised_background(
    launch: AgentLaunchArgs,
    globals: &GlobalFlags,
) -> Result<BackgroundLaunchOutcome> {
    let args = AgentsArgs::from_launch(launch);
    let (request, presentation) = into_supervised_request(args)?;
    let Some(outcome) = run_supervised(request, presentation, globals, &mut None)? else {
        return Ok(BackgroundLaunchOutcome::Aborted);
    };
    match outcome {
        SupervisedRunOutcome::Background {
            agent_name,
            run_id,
            response_path,
        } => Ok(BackgroundLaunchOutcome::Launched(BackgroundLaunch {
            name: agent_name,
            run_id,
            response_path,
        })),
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
    args.launch.cohort.validate_fresh_worktree()?;
    if args.json {
        bail!("on `-p`, choose output with `--output-format json` (`--json` is for `list`)");
    }
    let output_format = args.launch.output_format.unwrap_or_default();
    validate_supervised_output(&args, output_format)?;
    let prompt = resolve_print_prompt(&args, args.launch.input_format.unwrap_or_default())?;
    let worktree_fresh_command = args
        .launch
        .cohort
        .worktree
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| {
            use crate::cli::worktree_entry::{PrintInput, relaunch_commands};
            let input = match args.launch.input_format.unwrap_or_default() {
                InputFormat::StreamJson => PrintInput::StreamJson,
                InputFormat::Text if args.launch.stdin => PrintInput::Stdin,
                InputFormat::Text => PrintInput::Text,
            };
            relaunch_commands(
                name,
                args.launch.spec.as_deref().unwrap_or("<spec>"),
                None,
                None,
                args.launch.prompt.as_deref(),
                Some(input),
            )
            .1
        });
    let permission_mode = interactive_permission_mode_from_flags(
        args.launch.overrides.ask,
        args.launch.overrides.yolo,
    )?;
    let system_prompt_file = resolve_launch_prompt_file(
        args.launch.overrides.system_prompt_file.as_deref(),
        "--system-prompt-file",
    )?;
    let append_system_prompt_files =
        resolve_launch_prompt_files(&args.launch.overrides.append_system_prompt_files)?;
    let spec = args
        .launch
        .spec
        .context("supervised run requires an agent spec")?;
    let mut request = rimz::harness::run::SupervisedRunRequest::new(
        spec,
        prompt,
        permission_mode,
        rimz::agents::ManagedLaunchState::PendingResolution,
    );
    request.description = args.launch.cohort.description;
    request.worktree = args.launch.cohort.worktree;
    request.fresh = args.launch.cohort.fresh;
    request.cwd = args.launch.cwd;
    request.from_pr = args.launch.cohort.from_pr;
    request.channel = args.launch.cohort.channel;
    request.name = args.launch.name;
    request.background = args.launch.cohort.bg;
    request.self_cleanup_on_completion =
        args.launch.cohort.bg || args.launch.self_cleanup_on_completion;
    request.subagent = args.launch.subagent;
    request.force_new_tab = args.launch.cohort.new_tab;
    request.agent =
        rimz::harness::plan::normalized_preset_value(args.launch.overrides.agent.as_deref());
    request.fast = args.launch.overrides.fast_override();
    request.model = args.launch.overrides.model;
    request.tier = args.launch.overrides.tier;
    request.isolation = args.launch.overrides.isolation;
    request.system_prompt_file = system_prompt_file;
    request.append_system_prompt_files = append_system_prompt_files;
    request.effort = args.launch.overrides.effort;
    request.budget = args.launch.cohort.budget;
    request.max_turns = args.launch.max_turns;
    request.timeout = args.launch.timeout;
    request.warn = args.launch.warn;
    request.grace = args.launch.grace;
    request.keep = args.launch.keep;
    request.report_to = report_to(args.launch.cohort.detach);
    request.retries = args.launch.retries.unwrap_or(0);
    request.verify = args.launch.verify;
    request.max_attempts = args.launch.max_attempts;
    request.passthrough = args.launch.overrides.passthrough;
    Ok((
        request,
        supervised::SupervisedPresentation {
            output_format,
            stream_text: false,
            worktree_fresh_command,
        },
    ))
}

fn report_to(detach: bool) -> rimz::store::run::ReportTo {
    if detach {
        rimz::store::run::ReportTo::Nobody
    } else {
        rimz::store::run::ReportTo::Launcher
    }
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
    Scope,
    Address,
    Launch,
}

fn top_level_spec_route(spec: &str) -> TopLevelSpecRoute {
    if spec.starts_with('#') {
        TopLevelSpecRoute::Scope
    } else if spec.starts_with('@') {
        TopLevelSpecRoute::Address
    } else {
        TopLevelSpecRoute::Launch
    }
}

/// The refusal for a no-verb `rimz agents` that names no launch spec: a
/// missing or blank SPEC, or a `#scope` in its place, whatever else was passed.
fn launch_refusal(launch: &AgentLaunchArgs, all: bool, json: bool) -> Option<UsageError> {
    match launch.spec.as_deref().map(str::trim) {
        Some(spec) if top_level_spec_route(spec) == TopLevelSpecRoute::Scope => {
            return Some(scope_refusal(spec, json));
        }
        Some(spec) if !spec.is_empty() => return None,
        _ => {}
    }
    let worktree = launch
        .cohort
        .worktree
        .as_deref()
        .filter(|name| !name.is_empty());
    let mut flags = Vec::new();
    if all {
        flags.push("--all");
    }
    if let Some(name) = worktree {
        flags.extend(["--worktree", name]);
    }
    if json {
        flags.push("--json");
    }
    Some(UsageError::missing_scene_verb(
        crate::cli::help::Scene::Agents,
        "a launch spec",
        &flags,
    ))
}

/// The word of a bare `rimz agents <word>` with no prompt and no launch flag,
/// unless it is a kind: the only launch the live-agent refusal can stop.
fn lone_launch_word(launch: &AgentLaunchArgs) -> Option<&str> {
    let word = launch.spec.as_deref()?;
    let lone = AgentLaunchArgs {
        spec: launch.spec.clone(),
        ..Default::default()
    };
    (*launch == lone
        && top_level_spec_route(word) == TopLevelSpecRoute::Launch
        && !rimz::agents::known_kinds().any(|kind| kind == word))
    .then_some(word)
}

/// The refusal for a lone launch `word` that also names one of the `live`
/// agents: launching it silently would start a second agent the user likely
/// meant to inspect.
fn collision_refusal(
    word: &str,
    live: &[&AgentState],
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    current_channel: &rimz::address::AddressContext,
) -> Option<UsageError> {
    let matches = rimz::address::launch_word_matches(word, live, current_channel);
    let newest = matches.iter().max_by_key(|agent| agent.last_activity)?;
    let address = rimz::address::paste_address(newest, snapshot, current_channel);
    let named = match matches.len() {
        1 => format!("a live agent ({address})"),
        count => format!("{count} live agents"),
    };
    let mut message = format!(
        "`{word}` names {named} as well as a launch spec\n  inspect: {}",
        shell_command(["rimz", "agents", "show", &address])
    );
    if matches.len() > 1 {
        message.push_str("\n  all:     rimz agents list --all");
    }
    message.push_str(&format!(
        "\n  launch:  {}",
        shell_command(["rimz", "agents", "launch", word])
    ));
    Some(UsageError::new(message))
}

/// The room's live agents, as `rimz agents list --all` sees them, beside the
/// caller's address context; none when no room is live, so the launch meets
/// its own room check as before.
fn live_room_snapshot(
    globals: &GlobalFlags,
) -> Option<(
    rimz::store::snapshot::SidebarSnapshot,
    rimz::address::AddressContext,
)> {
    let ctx = crate::cli::Ctx::open(globals).ok()?;
    rimz::room::require_live_mux(globals.mux, &ctx.workspace).ok()?;
    Some((ctx.published_snapshot().ok()?, ctx.address_context()))
}

fn scope_refusal(scope: &str, json: bool) -> UsageError {
    let json = json.then_some("--json");
    let list = shell_command(["rimz", "agents", "list", scope].into_iter().chain(json));
    UsageError::new(format!(
        "`{scope}` is a scope, not a launch spec; to list that lane, run `{list}`"
    ))
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
pub(super) fn create_on_miss(
    target: &str,
    worktree_flag: Option<&str>,
    channel_flag: Option<&str>,
    current_channel: &super::CurrentChannel,
    text: &str,
    globals: &GlobalFlags,
) -> Result<()> {
    let channel_filter = worktree_flag.or(channel_flag);
    let Some(create) =
        rimz::address::create_mention(target, channel_filter, current_channel.as_deref())?
    else {
        bail!(
            "`{target}` cannot create an agent; address a kind or profile like `@codex` or `@planner`"
        );
    };
    let machine_config = crate::cli::machine_config();
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let launch = rimz::config::effective::load(&machine_config, &workspace.project_root)?;
    if !is_launchable_type(&create.selector, &launch.profiles) {
        launch.block_untrusted_reference(
            rimz::config::effective::ProfileScope::Agents,
            Some(&create.selector),
            &machine_config.agents.commands,
        )?;
        bail!(
            "`{target}` names a specific agent that is not running; create one with `@<kind>` or a configured profile (`rimz agents profiles`)"
        );
    }
    launch_layout(
        create_launch_args(
            create,
            target,
            worktree_flag,
            channel_flag,
            current_channel,
            text,
        ),
        globals,
        false,
    )
}

fn create_launch_args(
    create: rimz::address::CreateMention,
    target: &str,
    worktree_flag: Option<&str>,
    channel_flag: Option<&str>,
    current_channel: &super::CurrentChannel,
    text: &str,
) -> AgentsArgs {
    // `--worktree` keeps the historical worktree create-on-miss path. `--channel`
    // and inline `#name` launch into a named lane.
    let inline_named_channel = target
        .split_once('#')
        .is_some_and(|(_, channel)| rimz::channel::valid_name(channel));
    let named = channel_flag.is_some()
        || (worktree_flag.is_none() && inline_named_channel)
        || create
            .channel
            .as_deref()
            .is_some_and(|channel| matches!(current_channel, super::CurrentChannel::Named(current) if current == channel));
    let (worktree, channel) = if named {
        (None, create.channel)
    } else {
        (
            create
                .channel
                .filter(|channel| Some(channel.as_str()) != current_channel.as_deref()),
            None,
        )
    };
    let prompt = (!text.trim().is_empty()).then(|| text.to_owned());
    AgentsArgs::for_create(create.selector, prompt, worktree, channel)
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
