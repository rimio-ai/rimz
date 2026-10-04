//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod accounts;
mod address;
mod agents_cmd;
mod answer;
mod asks;
mod budget;
mod codex;
mod complete;
mod config;
mod coverage;
mod ctx;
mod daemon;
mod doctor;
mod events;
mod first_run;
mod folder_trust;
mod gc;
mod help;
mod hooks;
mod list;
mod list_pets;
mod list_themes;
mod loop_cmd;
mod loop_timer;
mod lsp;
mod lsp_admission;
mod message;
mod pane;
mod paths;
mod pricing_refresh;
mod profile_report;
mod providers;
mod reload;
mod remote;
mod render;
mod reset;
mod room;
mod send;
mod sessions;
mod setup;
mod sidebar;
mod spinner;
mod stats;
mod statusline;
mod subagents;
mod supervised;
mod teams;
mod transcript;
mod trust;
mod uninstall;
mod update;
mod usage;
mod wait;
mod web;
mod workspace;
mod worktree;
mod worktree_protection;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use rimz::agents::AgentState;
use rimz::ids::{AskId, MuxName, WorkspaceId};
use rimz::store::snapshot::{PaneAgent, SidebarSnapshot};
use rimz::{RuntimePaths, StatePaths, Store};

pub(crate) use ctx::Ctx;
pub use usage::UsageError;

pub(crate) fn open_browser_best_effort(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if which::which(opener).is_err() {
        return;
    }
    let mut command = std::process::Command::new(opener);
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    rimz::child_process::restore_user_temp_env(&mut command);
    let _ = command.spawn();
}

/// Render a command failure at the binary boundary.
pub fn report(error: &anyhow::Error) {
    render::report(error);
}

/// Serve environment-activated completion before any normal startup work can
/// write stdout or install process-wide reporting.
pub fn complete_env() {
    clap_complete::CompleteEnv::with_factory(
        || help::customize(<Cli as CommandFactory>::command()),
    )
    .complete();
}

/// Entry point used by `main.rs`.
pub fn dispatch() -> Result<()> {
    reject_removed_top_level_tokens()?;
    let cmd = help::customize(<Cli as CommandFactory>::command());
    let mut matches = cmd.get_matches();
    let canonical_command = matches.subcommand_name().unwrap_or("start").to_owned();
    let cli = Cli::from_arg_matches_mut(&mut matches).unwrap_or_else(|err| err.exit());
    let mut globals = cli.global;
    globals.normalize()?;
    globals.color.write_global();
    rimz::observability::set_command_scope(scope_facts(
        &canonical_command,
        cli.subcommand.as_ref(),
    ));
    match cli.subcommand {
        Some(Subcmd::Workspace(args)) => workspace::run(args, &globals),
        Some(Subcmd::List(args)) => list::run(args, &globals),
        Some(Subcmd::Lsp(args)) => lsp::run(args, &globals),
        Some(Subcmd::Paths(args)) => paths::run(args, &globals),
        Some(Subcmd::Stats(args)) => stats::run(args, &globals),
        Some(Subcmd::Providers(args)) => providers::run(args, &globals),
        Some(Subcmd::Logins(args)) => accounts::run(args, &globals),
        Some(Subcmd::Budget(args)) => budget::run(args, &globals),
        Some(Subcmd::ListPets(args)) => list_pets::run(args, &globals),
        Some(Subcmd::ListThemes(args)) => list_themes::run(args, &globals),
        Some(Subcmd::Gc(args)) => gc::run(args, &globals),
        Some(Subcmd::Uninstall(args)) => uninstall::run(args, &globals),
        Some(Subcmd::Update(args)) => update::run(args, &globals),
        Some(Subcmd::Worktree(args)) => worktree::run(args, &globals),
        Some(Subcmd::Agents(args)) => agents_cmd::run(*args, &globals),
        Some(Subcmd::Subagents(args)) => subagents::run(*args, &globals),
        Some(Subcmd::Wait(args)) => wait::run(*args, &globals),
        Some(Subcmd::Teams(args)) => teams::run(*args, &globals),
        Some(Subcmd::Asks(args)) => asks::run(args, &globals),
        Some(Subcmd::Answer(args)) => answer::run(args, &globals),
        Some(Subcmd::Loop(args)) => loop_cmd::run(args, &globals),
        Some(Subcmd::Reload(args)) => reload::run(args, &globals),
        Some(Subcmd::Reset(args)) => reset::run(args, &globals),
        Some(Subcmd::Pane(args)) => pane::run(args, &globals),
        Some(Subcmd::PricingRefresh(args)) => pricing_refresh::run(args),
        Some(Subcmd::Message(args)) => message::run(*args, &globals),
        Some(Subcmd::Sidebar(args)) => sidebar::run(args, &globals),
        Some(Subcmd::Statusline(args)) => statusline::run(args, &globals),
        Some(Subcmd::Hooks(args)) => hooks::run(args, &globals),
        Some(Subcmd::Codex(args)) => codex::run(args, &globals),
        Some(Subcmd::Daemon(args)) => daemon::run(args, &globals),
        Some(Subcmd::Config(args)) => config::run(args, &globals),
        Some(Subcmd::Coverage(args)) => coverage::run(args, &globals),
        Some(Subcmd::Trust(args)) => trust::run(args, &globals),
        Some(Subcmd::Transcript(args)) => transcript::run(args, &globals),
        Some(Subcmd::Doctor(args)) => doctor::run(args, &globals),
        Some(Subcmd::Events(args)) => events::run(args, &globals),
        Some(Subcmd::Setup(args)) => setup::run(args, &globals),
        Some(Subcmd::Ping) => doctor::ping(),
        Some(Subcmd::Start(args)) => room::start(args, &globals),
        Some(Subcmd::RecoverParked(args)) => room::recover_deferred(args),
        Some(Subcmd::Attach(args)) => room::attach(args, &globals),
        Some(Subcmd::Sessions(args)) => sessions::run(args, &globals),
        Some(Subcmd::Remote(args)) => remote::run(args, &globals),
        Some(Subcmd::Web(args)) => web::run(args, &globals),
        None => room::start(
            StartArgs {
                path: PathBuf::from("."),
                attach: cli.attach,
                no_resume: cli.no_resume,
                refresh_ms: cli.refresh_ms,
                account: Vec::new(),
            },
            &globals,
        ),
    }
}

fn reject_removed_top_level_tokens() -> Result<()> {
    reject_removed_top_level_tokens_from(std::env::args_os().skip(1))
}

fn reject_removed_top_level_tokens_from<I>(args: I) -> Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if matches!(
            arg.to_str(),
            Some("--mux" | "--root" | "--color" | "--refresh-ms")
        ) {
            let _ = args.next();
            continue;
        }
        if arg.to_str().is_some_and(|arg| {
            arg.starts_with("--mux=")
                || arg.starts_with("--root=")
                || arg.starts_with("--color=")
                || arg.starts_with("--refresh-ms=")
        }) {
            continue;
        }
        match arg.to_str() {
            Some("autoping") => {
                anyhow::bail!("`rimz autoping` has moved to `rimz loop`; use `rimz loop --help`")
            }
            Some("run") => anyhow::bail!(
                "`rimz run` has moved to `rimz agents <spec> <prompt> -p`; use `rimz agents show|wait|stop <ref>` for run records"
            ),
            Some("event") => anyhow::bail!(
                "`rimz event` has been removed; use `rimz message`, `rimz agents -p`, or pane primitives for automation"
            ),
            Some("feed") => anyhow::bail!(
                "`rimz feed` has been removed; blocking agent prompts now surface as Waiting state in the agent pane"
            ),
            Some("tab") => anyhow::bail!(
                "`rimz tab` has moved to `rimz agents <spec> [prompt]`; teams now come from `<agents_home>/teams/<name>.md`"
            ),
            Some("channel") => anyhow::bail!(
                "`rimz channel` has been removed; channels are implicit: launch into one with `rimz agents <spec> --channel <name>`, and see lanes with `rimz agents list --all` and `rimz worktree list`"
            ),
            _ => {}
        }
        if !arg.to_string_lossy().starts_with('-') {
            return Ok(());
        }
    }
    Ok(())
}

/// Low-cardinality Sentry scope facts for the resolved command: the command
/// label every event in this process inherits, plus the agent kind and session
/// when the command serves exactly one. The label is the command verb only,
/// never argument values, so it stays a stable Sentry facet.
fn scope_facts<'a>(
    canonical_command: &'a str,
    sub: Option<&'a Subcmd>,
) -> rimz::observability::ScopeFacts<'a> {
    let (command, session, agent) = match sub {
        Some(Subcmd::Remote(args)) => (args.command_label(), None, None),
        Some(Subcmd::Sidebar(args)) => (args.command_label(), None, None),
        Some(Subcmd::Hooks(args)) => {
            let (command, agent) = args.scope();
            (command, None, agent)
        }
        Some(Subcmd::Codex(args)) => {
            let (command, session) = args.scope();
            (command, session, Some("codex"))
        }
        Some(Subcmd::Agents(args)) => args.scope(),
        _ => (canonical_command, None, None),
    };
    rimz::observability::ScopeFacts {
        command,
        session,
        agent,
    }
}

/// The current channel a command runs in: an explicit named lane from
/// `RIMZ_CHANNEL`, else the worktree directory basename when we are genuinely
/// inside a separate worktree. A bare directory workspace (root == worktree)
/// yields `None` for humans; RimZ-launched team members carry `RIMZ_CHANNEL`,
/// so their calls scope to the stamped team lane.
pub(crate) fn current_channel(workspace: &rimz::ResolvedWorkspace) -> Option<String> {
    if let Ok(channel) = std::env::var(rimz::workspace::ENV_CHANNEL)
        && !channel.is_empty()
    {
        return Some(channel);
    }
    rimz::harness::spec::resolve_room_channel(
        &workspace.project_root,
        &workspace.worktree_root,
        None,
        None,
    )
}

/// Refuse a plain selector that matched several agents. A bare `@<kind>`/`@<profile>`
/// names a profile, not "everyone", so several matches is a "pick one" error: it
/// lists the disambiguating handles to retype and names `--all` as the opt-in to
/// reach every match. The explicit broadcast `@all` and `--all` fan out directly;
/// each delivery carries the addressed handle as a prefix.
pub(crate) fn ambiguous_fanout(verb: &str, target: &str, labels: &[String]) -> anyhow::Error {
    let list = labels
        .iter()
        .map(|label| {
            if label.starts_with('@') {
                label.clone()
            } else {
                format!("@{label}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::anyhow!(
        "`{target}` matches {} agents ({list}); name one above, or pass --all to {verb} them all",
        labels.len()
    )
}

/// Resolve a ref to exactly one agent (`show`/`focus`/`wait`/`stop`,
/// `message clear`/`list`). `@all` or a fan-out kind is an explicit ambiguity.
pub(crate) fn resolve_agent_one<'a>(
    store: &rimz::Store,
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<&'a AgentState> {
    if raw == "@me" {
        let unidentified = || {
            anyhow::anyhow!(
                "@me requires an agent RimZ can identify; run this command from an agent pane"
            )
        };
        let caller = send::resolve_caller(store)?.ok_or_else(unidentified)?;
        let agent = rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, &caller)
            .map_err(|_| unidentified())?;
        if agent.ended_at.is_some() {
            anyhow::bail!("@me requires a live agent; the calling agent has ended");
        }
        if agent.agent_id.is_provisional() {
            anyhow::bail!("@me requires a registered session; the calling agent is still starting");
        }
        return Ok(agent);
    }
    map_resolve(
        raw,
        rimz::address::resolve_one(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Resolve an ask id or agent ref while preserving each caller's ask scope.
///
/// `asks show` folds the awaiting state into ask-id lookup; `answer` may target
/// a stale agent and reports a matched-but-stale ask as "not asking" instead.
pub(crate) fn resolve_open_ask<'a>(
    store: &rimz::Store,
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    current_channel: Option<&str>,
) -> Result<Option<&'a AgentState>> {
    if raw.starts_with("ask_") {
        let ask_id = AskId::parse(raw)?;
        return Ok(find_open_ask(snapshot, &ask_id));
    }
    resolve_agent_one(store, snapshot, raw, None, current_channel).map(Some)
}

fn find_open_ask<'a>(snapshot: &'a SidebarSnapshot, ask_id: &AskId) -> Option<&'a AgentState> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.actionable_asks().any(|(ask, _)| &ask.id == ask_id))
}

/// Resolve a ref to every matching live agent pane for `message --steer` and
/// send-now messages: the producer's bound panes, so a target reaches exactly the agent
/// panes the producer saw — bound sessions (at their live pane) and lazy panes
/// with no session yet.
pub(crate) fn resolve_pane_targets<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a PaneAgent>> {
    map_resolve(
        raw,
        rimz::address::resolve_targets(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Turn a clean target miss into the launch-profile/command/layout hint when
/// the ref names launch config rather than a running agent.
fn map_resolve<T>(
    raw: &str,
    result: std::result::Result<T, rimz::address::TargetErr>,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(rimz::address::TargetErr::NoMatch { target, suggestion }) => {
            if let Some(hint) = launch_ref_hint(raw)? {
                anyhow::bail!("{hint}; run `rimz agents list` to see live agents");
            }
            Err(rimz::address::TargetErr::NoMatch { target, suggestion }.into())
        }
        Err(err) => Err(err.into()),
    }
}

fn launch_ref_hint(raw: &str) -> Result<Option<String>> {
    if raw.contains(':') {
        return Ok(None);
    }
    let without_channel = raw.split('#').next().unwrap_or(raw);
    let selector = without_channel.strip_prefix('@').unwrap_or(without_channel);
    let config = machine_config();
    if config.agents.profiles.0.contains_key(selector) {
        return Ok(Some(format!(
            "`{selector}` is a launch profile, not a running agent"
        )));
    }
    if config.agents.commands.0.contains_key(selector) {
        return Ok(Some(format!(
            "`{selector}` is a launch command, not a running agent"
        )));
    }
    if selector == "peer" || config.agents.teams.0.contains_key(selector) {
        return Ok(Some(format!(
            "`{selector}` is a launch team, not a running agent"
        )));
    }
    Ok(None)
}

#[derive(Debug, Parser)]
#[command(
    author,
    version = rimz::build_id::VERSION,
    bin_name = "rimz",
    about = "One room per project for agents, scripts, and humans."
)]
struct Cli {
    #[command(flatten)]
    global: GlobalFlags,

    #[command(next_help_heading = "Launch options (bare `rimz` / start / attach)")]
    #[command(flatten)]
    attach: AttachFlags,

    /// Come up empty: skip recovering prior agents when the session is reborn.
    #[arg(long)]
    no_resume: bool,
    /// Override the sidebar render cadence for this launch.
    #[arg(long)]
    refresh_ms: Option<u16>,

    #[command(subcommand)]
    subcommand: Option<Subcmd>,
}

/// Flags accepted at every level. Shared so per-command code stays terse.
#[derive(Debug, Args, Clone)]
pub struct GlobalFlags {
    /// Override multiplexer backend selection.
    #[arg(
        long,
        value_parser = parse_mux,
        global = true,
        add = clap_complete::ArgValueCandidates::new(complete::mux_names)
    )]
    pub mux: Option<MuxName>,
    /// Select the Zellij backend (shorthand for `--mux zellij`).
    #[arg(long, global = true)]
    pub zellij: bool,
    /// Select the tmux backend (shorthand for `--mux tmux`).
    #[arg(long, global = true)]
    pub tmux: bool,
    /// Override project-root resolution (monorepo escape hatch).
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    /// When to colorize human output: `auto` (default), `always`, or `never`.
    /// `auto` follows the terminal and the `NO_COLOR`/`CLICOLOR` environment.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, global = true)]
    pub color: ColorWhen,
}

impl GlobalFlags {
    /// Fold the `--zellij`/`--tmux` shorthands into `mux`. They are aliases for
    /// `--mux zellij`/`--mux tmux`; giving more than one backend selector fails
    /// fast at the CLI boundary.
    fn normalize(&mut self) -> Result<()> {
        let selectors = self.mux.is_some() as u8 + self.zellij as u8 + self.tmux as u8;
        if selectors > 1 {
            anyhow::bail!("choose one of --mux, --zellij, --tmux");
        }
        if self.zellij {
            self.mux = Some(MuxName::Zellij);
        } else if self.tmux {
            self.mux = Some(MuxName::Tmux);
        }
        Ok(())
    }
}

/// `--color` choice, mapped onto the global `colorchoice` that `render::out`
/// consults when it auto-detects whether to emit ANSI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    fn write_global(self) {
        let choice = match self {
            ColorWhen::Auto => colorchoice::ColorChoice::Auto,
            ColorWhen::Always => colorchoice::ColorChoice::Always,
            ColorWhen::Never => colorchoice::ColorChoice::Never,
        };
        choice.write_global();
    }
}

#[derive(Debug, Subcommand)]
enum Subcmd {
    /// Navigate code through shared language servers.
    Lsp(lsp::LspArgs),
    /// Open or attach the room for a path (default action).
    Start(StartArgs),
    /// Attach to a room by session name.
    ///
    /// Omit the name to use the cwd's workspace.
    Attach(AttachArgs),
    /// Pick and open live RimZ rooms in a session manager.
    Sessions(sessions::SessionsArgs),
    /// Manage and connect to SSH remote rooms.
    Remote(remote::RemoteArgs),
    /// Open a Zellij room in the browser.
    Web(web::WebArgs),
    /// Workspace identity helpers.
    Workspace(workspace::WorkspaceArgs),
    /// Show known workspaces and which mux is running them.
    List(list::ListArgs),
    /// Show where RimZ keeps this project's files.
    Paths(paths::PathsArgs),
    /// Token-activity heatmap and usage insights.
    ///
    /// Includes model and agent breakdowns.
    Stats(stats::StatsArgs),
    /// Query provider account plans, auth, limits, credits, and spend.
    Providers(providers::ProvidersArgs),
    /// Declare, list, and remove named provider accounts.
    ///
    /// A named account is a separate provider home a room launches into.
    #[command(name = "accounts")]
    Logins(accounts::AccountsArgs),
    /// Inspect or change room and provider-account daily dollar caps.
    Budget(budget::BudgetArgs),
    /// Preview the bundled provider-dashboard pets.
    ///
    /// Renders the pets as pane-local cell art.
    ListPets(list_pets::ListPetsArgs),
    /// List the bundled sidebar theme names.
    ListThemes(list_themes::ListThemesArgs),
    /// Remove stale runtime liveness hints.
    Gc(gc::GcArgs),
    /// Remove RimZ from this machine.
    ///
    /// Removes hooks, rooms, and runtime footprint. Use --state, --config, or
    /// --all to purge durable state and config.
    Uninstall(uninstall::UninstallArgs),
    /// Update RimZ to the latest release.
    Update(update::UpdateArgs),
    /// Create, enter, merge, sweep, and remove RimZ-owned git worktrees.
    Worktree(worktree::WorktreeArgs),
    /// Launch agent tabs, optionally in RimZ-owned worktrees.
    Agents(Box<agents_cmd::AgentsArgs>),
    /// Launch and drive supervised child agents.
    Subagents(Box<subagents::SubagentsArgs>),
    /// Wait for a timer, a process, or a command to finish.
    Wait(Box<wait::WaitCommand>),
    /// Discover, inspect, install, launch, and resume named teams.
    Teams(Box<teams::TeamsArgs>),
    /// Inspect the prompts agents currently have open.
    Asks(asks::AsksArgs),
    /// Answer one current prompt in its agent pane.
    Answer(answer::AnswerArgs),
    /// Schedule supervised agent turns.
    ///
    /// Runs from the room's sidebar elder.
    #[command(name = "loop")]
    Loop(loop_cmd::LoopArgs),
    /// Reload running sidebars in place.
    ///
    /// Picks up a freshly-installed build.
    Reload(reload::ReloadArgs),
    /// Force a clean rebirth of this workspace's room.
    ///
    /// Destroys a stuck or resurrected Zellij session and sweeps its orphaned
    /// processes.
    Reset(reset::ResetArgs),
    /// Pane primitives backed by the selected mux backend.
    Pane(pane::PaneArgs),
    /// Pricing snapshot projection helper. Contributor automation calls this.
    #[command(hide = true)]
    PricingRefresh(pricing_refresh::PricingRefreshArgs),
    /// Complete an attended recovery after a client attaches.
    #[command(hide = true)]
    RecoverParked(room::DeferredRecoveryArgs),
    /// Message agents; list, edit, steer, requeue, cancel.
    ///
    /// Bare send routes now with `--steer`, or at the next safe turn boundary.
    #[command(visible_alias = "msg")]
    Message(Box<message::MessageArgs>),
    /// Sidebar helper API. The sidebar calls these; humans usually do not.
    #[command(hide = true)]
    Sidebar(sidebar::SidebarArgs),
    /// Statusline datasource. The installed `statusLine` command calls this;
    /// humans do not.
    #[command(hide = true)]
    Statusline(statusline::StatuslineArgs),
    /// Install or uninstall agent hooks.
    ///
    /// Internal hook entrypoints live here too.
    Hooks(hooks::HooksArgs),
    /// Codex helper API. The Codex hook calls these; humans usually do not.
    #[command(hide = true)]
    Codex(codex::CodexArgs),
    /// Daemon dashboard helper API. The rimzd content panes call this; humans do not.
    #[command(hide = true)]
    Daemon(daemon::DaemonArgs),
    /// Inspect and edit the per-machine config.
    Config(config::ConfigArgs),
    /// Adapter integration and lifecycle-hook coverage matrices.
    Coverage(coverage::CoverageArgs),
    /// Manage the project's executable-surface trust grant.
    Trust(trust::TrustArgs),
    /// Inspect agent or channel conversation transcripts.
    Transcript(transcript::TranscriptArgs),
    /// Follow durable events or emit a signal.
    Events(events::EventsArgs),
    /// Environment and backend report.
    Doctor(doctor::DoctorArgs),
    /// First-run setup report and default config bootstrap.
    Setup(setup::SetupArgs),
    /// Machine-readable liveness check (prints `ok`).
    Ping,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[command(flatten)]
    attach: AttachFlags,
    /// Path to use as the workspace cwd.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Come up empty: skip recovering prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
    /// Override the sidebar render cadence for this launch.
    #[arg(long)]
    pub refresh_ms: Option<u16>,
    /// Launch this provider's agents under a named account (repeatable).
    /// For a running room, use `rimz accounts use` instead.
    #[arg(long, value_name = "KIND=NAME", value_parser = accounts::parse_account_flag)]
    pub account: Vec<accounts::AccountFlag>,
}

#[derive(Debug, Args, Default)]
#[group(required = false, multiple = false)]
pub struct AttachFlags {
    /// Attach to the mux session instead of printing the attach command.
    #[arg(long)]
    attach: bool,
    /// Print the attach command instead of entering the mux session.
    #[arg(long)]
    no_attach: bool,
    /// Alias for `--no-attach`.
    #[arg(long)]
    print: bool,
}

impl AttachFlags {
    pub(crate) fn mode(&self) -> room::AttachMode {
        if self.attach {
            room::AttachMode::Attach
        } else if self.no_attach || self.print {
            room::AttachMode::Print
        } else {
            room::AttachMode::Auto
        }
    }
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    #[command(flatten)]
    attach: AttachFlags,
    /// Workspace session name (omit to use the cwd's workspace).
    #[arg(
        value_name = "SESSION",
        add = clap_complete::ArgValueCandidates::new(complete::sessions)
    )]
    workspace: Option<String>,
    /// Come up empty: skip recovering prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
    /// Override the sidebar render cadence for this launch.
    #[arg(long)]
    pub refresh_ms: Option<u16>,
}

fn parse_mux(value: &str) -> std::result::Result<MuxName, String> {
    value.parse::<MuxName>().map_err(|err| err.to_string())
}

pub(crate) fn confirm(prompt: &str) -> Result<bool> {
    confirm_with_default(prompt, false)
}

pub(crate) fn confirm_with_default(prompt: &str, default_yes: bool) -> Result<bool> {
    let mut stderr = std::io::stderr().lock();
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    write!(stderr, "{prompt} {suffix} ")?;
    stderr.flush()?;
    drop(stderr);
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(default_yes);
    }
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

fn parse_choice(answer: &str, choices: &[&str], default: usize) -> Option<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Some(default);
    }
    if let Some(index) = choices
        .iter()
        .position(|choice| choice.eq_ignore_ascii_case(answer))
    {
        return Some(index);
    }
    let answer = answer.to_ascii_lowercase();
    let mut matches = choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| choice.to_ascii_lowercase().starts_with(&answer));
    let (index, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(index)
}

fn choose(prompt: &str, choices: &[&str], default: usize) -> Result<Option<usize>> {
    let mut stderr = std::io::stderr().lock();
    write!(
        stderr,
        "{prompt} ({}) [{}]: ",
        choices.join("/"),
        choices[default]
    )?;
    stderr.flush()?;
    drop(stderr);
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Ok(None);
    }
    Ok(parse_choice(&answer, choices, default))
}

pub(crate) fn resolve_pr_branch_choice(
    facts: &rimz::worktree::WorktreeErr,
) -> Result<Option<rimz::worktree::PrBranchChoice>> {
    resolve_pr_branch_choice_with(facts, std::io::stdin().is_terminal(), choose, |line| {
        writeln!(render::err(), "{line}")
    })
}

fn resolve_pr_branch_choice_with(
    facts: &rimz::worktree::WorktreeErr,
    is_terminal: bool,
    choose: impl FnOnce(&str, &[&str], usize) -> Result<Option<usize>>,
    mut write_stderr: impl FnMut(&str) -> std::io::Result<()>,
) -> Result<Option<rimz::worktree::PrBranchChoice>> {
    use rimz::worktree::{PrBranchChoice, PrBranchDivergence, WorktreeErr};
    let WorktreeErr::PrBranchDiverged {
        branch,
        holder,
        divergence,
    } = facts
    else {
        anyhow::bail!("{facts}");
    };
    write_stderr(&format!(
        "rimz: {} branch `{branch}`: {divergence}",
        render::paint(render::palette::warn().bold(), "warning:")
    ))?;
    if let Some(holder) = holder {
        write_stderr(&format!("  worktree: {}", holder.display()))?;
    }
    write_stderr("  the previous tip stays in the reflog")?;
    if !is_terminal {
        if matches!(divergence, PrBranchDivergence::Behind { .. }) {
            write_stderr(&format!("  fast-forwarding `{branch}` to the PR head"))?;
            return Ok(Some(PrBranchChoice::Remote));
        }
        anyhow::bail!("{facts}");
    }
    let default = usize::from(matches!(divergence, PrBranchDivergence::Diverged { .. }));
    Ok(choose(
        "Continue from the PR head (remote) or the local tip (local)?",
        &["remote", "local"],
        default,
    )?
    .map(|choice| {
        if choice == 0 {
            PrBranchChoice::Remote
        } else {
            PrBranchChoice::Local
        }
    }))
}

fn resolve_launch_checkout(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::MachineConfig,
    worktree: Option<&str>,
    from_pr: Option<&rimz::forge::PrTarget>,
    cwd: Option<&std::path::Path>,
) -> Result<Option<rimz::worktree::LaunchCheckout>> {
    if worktree.is_some() || from_pr.is_some() {
        require_worktree_config(config)?;
    }
    let config = &config.agents.worktree;
    let checkout =
        rimz::worktree::resolve_launch_checkout(workspace, config, worktree, from_pr, None, cwd);
    settle_launch_checkout(workspace, config, worktree, from_pr, cwd, checkout)
}

fn settle_launch_checkout(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    worktree: Option<&str>,
    from_pr: Option<&rimz::forge::PrTarget>,
    cwd: Option<&std::path::Path>,
    checkout: rimz::worktree::Result<rimz::worktree::LaunchCheckout>,
) -> Result<Option<rimz::worktree::LaunchCheckout>> {
    let launch = match checkout {
        Ok(launch) => launch,
        Err(err @ rimz::worktree::WorktreeErr::PrBranchDiverged { .. }) => {
            let Some(choice) = resolve_pr_branch_choice(&err)? else {
                writeln!(render::err(), "Launch aborted; nothing changed.")?;
                return Ok(None);
            };
            rimz::worktree::resolve_launch_checkout(
                workspace,
                config,
                worktree,
                from_pr,
                Some(choice),
                cwd,
            )?
        }
        Err(rimz::worktree::WorktreeErr::Unmarked { name, .. }) if from_pr.is_none() => {
            let launch =
                rimz::worktree::resolve_unmanaged_launch_checkout(workspace, config, &name)?;
            if !std::io::stdin().is_terminal() {
                anyhow::bail!(
                    "worktree `{name}` is not RimZ-managed; rerun in a terminal to confirm entering it"
                );
            }
            if !confirm(&format!(
                "Worktree `{name}` at {} is not RimZ-managed. Enter it without adopting it or enabling automatic cleanup?",
                launch.cwd.display(),
            ))? {
                writeln!(render::err(), "Launch aborted; nothing changed.")?;
                return Ok(None);
            }
            launch
        }
        Err(err) => return Err(err.into()),
    };
    if let Some(pr) = from_pr.filter(|_| launch.reused) {
        writeln!(
            render::err(),
            "reusing worktree `{}` at {} (PR #{} head branch `{}`)",
            launch.worktree_name.as_deref().unwrap_or_default(),
            launch.cwd.display(),
            pr.number,
            launch.branch.as_deref().unwrap_or_default()
        )?;
    }
    if let Some(marker) = &launch.created {
        match open_store(workspace) {
            Ok(store) => emit_worktree_created(&store, workspace, marker),
            Err(error) => tracing::debug!(%error, "skipping worktree signal: store unavailable"),
        }
    }
    if let Some(reason) = launch.review_only_reason.as_deref() {
        writeln!(
            std::io::stderr(),
            "review-only checkout ({reason}); pushes are not configured — install gh/tea for a pushable checkout"
        )?;
    }
    if let Some(stale) = &launch.stale_base {
        writeln!(
            render::err(),
            "warning: {stale}; fast-forward {} first, or set agents.worktree.base to fresh",
            stale.branch
        )?;
    }
    Ok(Some(launch))
}

fn emit_worktree_created(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    marker: &rimz::worktree::WorktreeMarker,
) {
    let mut payload = serde_json::Map::from_iter([
        ("name".into(), serde_json::json!(marker.name)),
        ("branch".into(), serde_json::json!(marker.branch)),
        (
            "path".into(),
            serde_json::json!(marker.worktree_path.to_string_lossy()),
        ),
        (
            "repo".into(),
            serde_json::json!(marker.repo_root.to_string_lossy()),
        ),
        (
            "base".into(),
            serde_json::json!(marker.base_branch.as_deref().unwrap_or(&marker.base_ref)),
        ),
    ]);
    if let Some(number) = marker.from_pr {
        payload.insert("from_pr".into(), serde_json::json!(number));
    }
    let signal = rimz::harness::schedule::signal::Signal {
        name: "worktree.created"
            .parse()
            .expect("built-in signal name is valid"),
        payload,
        source: rimz::store::event::SignalSource::Worktree,
        watch: None,
    };
    rimz::harness::schedule::signal::emit_in_process(
        store,
        &workspace.session_name,
        &workspace.project_root,
        &signal,
    );
}

/// The project root of the room this process runs inside, from its verified
/// pane pin; `None` outside any room.
pub(crate) fn pinned_room_root() -> Option<PathBuf> {
    std::env::var(rimz::workspace::ENV_WORKSPACE_ID)
        .ok()
        .zip(std::env::var_os(rimz::workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| rimz::workspace::verify_pin(&id, &PathBuf::from(root)))
}

/// The standing at the caller's position, and whether the caller runs inside
/// the room there, where `rimz accounts use` can move it.
pub(crate) fn position_standing(
    globals: &GlobalFlags,
    config: &rimz::config::MachineConfig,
) -> Result<(rimz::room::AccountStanding, bool)> {
    let position = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let standing = rimz::room::AccountStanding::at(&position.project_root, config)?;
    let in_room = pinned_room_root()
        .is_some_and(|pin| pin.canonicalize().ok() == position.project_root.canonicalize().ok());
    Ok((standing, in_room))
}

/// The `rimz accounts use` line that moves `kind`'s marker to `name` from the
/// layer deciding it; `None` when project config decides it or nothing does,
/// since no form moves the marker then.
pub(crate) fn accounts_use_command(
    deciding: Option<rimz::room::Deciding>,
    kind: &str,
    name: &rimz::ids::LoginName,
) -> Option<String> {
    let flag = match deciding? {
        rimz::room::Deciding::Room => "",
        rimz::room::Deciding::Machine => "--global ",
        rimz::room::Deciding::Project => return None,
    };
    Some(format!("rimz accounts use {flag}{kind} {name}"))
}

fn check_launch_room(globals: &GlobalFlags) -> Result<()> {
    let Some(root) = globals.root.as_ref() else {
        return Ok(());
    };
    let Some(caller) = rimz::harness::ancestry::CallerIdentity::from_env() else {
        return Ok(());
    };
    let pinned = std::env::var(rimz::workspace::ENV_WORKSPACE_ID)
        .ok()
        .zip(std::env::var_os(rimz::workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| rimz::workspace::verify_pin(&id, std::path::Path::new(&root)));
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", Some(root.clone()))?;
    let pinned =
        rimz::StatePaths::for_workspace(rimz::ids::WorkspaceId::from_project_root(&pinned))?;
    let target = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    rimz::harness::ancestry::check_launch_room(
        Some(&caller),
        Some((&pinned.workspace_id, &pinned.dir_name)),
        (&target.workspace_id, &target.dir_name),
        root,
    )?;
    Ok(())
}

fn resolve_launch_cwd(
    path: Option<&std::path::Path>,
    paths: &rimz::StatePaths,
) -> Result<Option<std::path::PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let absolute = std::env::current_dir()
        .context("reading the caller cwd")?
        .join(path);
    let host = caller_host_path(&absolute, Some(paths))?;
    if !host.is_dir() {
        anyhow::bail!(
            "--cwd `{}` is not a directory; create it first or pass an existing one",
            path.display()
        );
    }
    host.canonicalize()
        .map(Some)
        .with_context(|| format!("resolving --cwd `{}`", path.display()))
}

/// The host path behind an absolute path a calling agent names. A sandboxed
/// caller names its temp unit as `/tmp`, while the launch reads the path
/// outside that view; `paths` defaults to the room the caller is pinned to.
pub(crate) fn caller_host_path(
    absolute: &std::path::Path,
    paths: Option<&rimz::StatePaths>,
) -> Result<std::path::PathBuf> {
    use rimz::config::Isolation;
    if Isolation::ambient(&rimz::agents::ambient_env()) != Some(Isolation::Sandbox) {
        return Ok(absolute.to_path_buf());
    }
    let pinned;
    let paths = match paths {
        Some(paths) => paths,
        None => {
            let Some(root) = std::env::var(rimz::workspace::ENV_WORKSPACE_ID)
                .ok()
                .zip(std::env::var_os(rimz::workspace::ENV_PROJECT_ROOT))
                .and_then(|(id, root)| {
                    rimz::workspace::verify_pin(&id, std::path::Path::new(&root))
                })
            else {
                return Ok(absolute.to_path_buf());
            };
            pinned =
                rimz::StatePaths::for_workspace(rimz::ids::WorkspaceId::from_project_root(&root))?;
            &pinned
        }
    };
    let caller = rimz::harness::ancestry::CallerIdentity::from_env();
    let view = rimz::sandbox::TmpView::current(
        Isolation::Sandbox,
        caller.as_ref().and_then(|caller| caller.name.as_deref()),
        paths,
    );
    Ok(view.host_path(absolute))
}

pub(crate) fn confirm_cross_repo_worktree(workspace: &rimz::ResolvedWorkspace) -> Result<bool> {
    confirm_cross_repo_worktree_with(
        workspace,
        std::io::stdin().is_terminal(),
        confirm,
        |message| writeln!(render::err(), "{message}"),
    )
}

fn confirm_cross_repo_worktree_with(
    workspace: &rimz::ResolvedWorkspace,
    is_terminal: bool,
    confirm: impl FnOnce(&str) -> Result<bool>,
    mut write_stderr: impl FnMut(&str) -> std::io::Result<()>,
) -> Result<bool> {
    let Some(cwd_root) = workspace.cwd_project_root.as_deref() else {
        return Ok(true);
    };
    if cwd_root == workspace.project_root {
        return Ok(true);
    }

    write_stderr(&format!(
        "rimz: {} the room root and current Git root differ",
        render::paint(render::palette::warn().bold(), "warning:")
    ))?;
    write_stderr(&format!(
        "  room root: {}\n  current git root: {}\n  \
         the worktree will be created under the current Git root's configured worktree directory\n  \
         this room will not list, remove, or garbage-collect that cross-repo worktree",
        workspace.project_root.display(),
        cwd_root.display(),
    ))?;

    if !is_terminal {
        anyhow::bail!(
            "room root {} differs from current Git root {}; \
             pass --root {} (or run from the room's checkout) for a non-interactive launch",
            workspace.project_root.display(),
            cwd_root.display(),
            cwd_root.display(),
        );
    }
    if !confirm("Create the worktree at the current Git root and manage its cleanup from there?")? {
        write_stderr("Launch aborted; nothing changed.")?;
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn machine_config() -> std::sync::Arc<rimz::config::MachineConfig> {
    rimz::config::MachineConfig::load_lenient()
}

fn require_worktree_config(config: &rimz::config::MachineConfig) -> Result<()> {
    config.require_readable_core("create a worktree")?;
    Ok(())
}

pub(crate) fn report_unknown_config_keys(config: &rimz::config::MachineConfig) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    for (path, error) in &config.notices.unreadable_files {
        writeln!(
            stderr,
            "rimz: warning: {error}; using built-in defaults for this file; correct {}; `rimz config get` shows the strict error",
            path.display(),
        )?;
    }
    for notice in &config.notices.unknown_keys {
        writeln!(
            stderr,
            "rimz: unknown config key `{}` in {} — ignored; run `rimz setup` to remove it",
            notice.key,
            notice.path.display(),
        )?;
    }
    for warning in &config.notices.host_isolation_fallback {
        writeln!(stderr, "rimz: {warning}")?;
    }
    Ok(())
}

pub(crate) fn report_definition_errors(config: &rimz::config::MachineConfig) -> Result<()> {
    let mut err = render::err();
    for error in &config.notices.definition_errors {
        writeln!(err, "rimz: {}: {}", error.path.display(), error.message)?;
    }
    Ok(())
}

pub(crate) fn open_store(workspace: &rimz::ResolvedWorkspace) -> Result<Store> {
    let paths =
        StatePaths::for_project_root(&workspace.project_root).context("preparing store paths")?;
    let runtime = RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
    let store = Store::open(paths, runtime).context("opening store")?;
    store
        .record_workspace(workspace)
        .context("recording workspace metadata")?;
    Ok(store)
}

pub(crate) fn open_existing_store(workspace: &rimz::ResolvedWorkspace) -> Result<Option<Store>> {
    let paths =
        StatePaths::for_project_root(&workspace.project_root).context("preparing store paths")?;
    if !paths.root.is_dir() {
        return Ok(None);
    }
    let runtime = RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
    Ok(Store::open_existing(paths, runtime))
}

pub(crate) fn runtime_paths_for(workspace_id: WorkspaceId) -> Result<RuntimePaths> {
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime
        .ensure_runtime_dirs()
        .context("preparing runtime dirs")?;
    Ok(runtime)
}

/// The agent roster the sidebar shows: the cached rollup with the daemon-mode
/// reap applied, so paneless Codex ghosts the app-server no longer holds drop
/// exactly as `rimz agents list` and the sidebar drop them. Best-effort and
/// fail-safe — an absent daemon-reap cache keeps every session
/// (see `SidebarSnapshot::reap_runtime`).
pub(crate) fn alive_snapshot(store: &Store, session: &str) -> Result<SidebarSnapshot> {
    let base = store.snapshot_cached().context("reading agent snapshot")?;
    Ok(rimz::sidebar::consumer::cached_alive_snapshot(
        base,
        store.runtime_paths(),
        session,
    ))
}

// The shipped flag surface, pinned as a snapshot. `help.rs` already guards the
// visible subcommand list; this covers the flags under each one.
#[cfg(test)]
#[path = "surface_tests.rs"]
mod surface_tests;

#[cfg(test)]
mod tests;
