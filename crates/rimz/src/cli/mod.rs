//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod address;
mod agents_cmd;
mod answer;
mod asks;
mod budget;
mod channel;
mod codex;
mod complete;
mod config;
mod coverage;
mod ctx;
mod daemon;
mod doctor;
mod events;
mod first_run;
mod gc;
mod help;
mod hooks;
mod list;
mod list_pets;
mod list_themes;
mod loop_cmd;
mod message;
mod pane;
mod pricing_refresh;
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
mod supervised;
mod teams;
mod transcript;
mod trust;
mod uninstall;
mod update;
mod web;
mod workspace;
mod worktree;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use rimz::agents::AgentState;
use rimz::ids::{AskId, MuxName, WorkspaceId};
use rimz::{RuntimePaths, StatePaths, Store};

pub(crate) use ctx::Ctx;

pub(crate) fn open_browser_best_effort(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if which::which(opener).is_err() {
        return;
    }
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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
        Some(Subcmd::Stats(args)) => stats::run(args, &globals),
        Some(Subcmd::Providers(args)) => providers::run(args, &globals),
        Some(Subcmd::Budget(args)) => budget::run(args, &globals),
        Some(Subcmd::ListPets(args)) => list_pets::run(args, &globals),
        Some(Subcmd::ListThemes(args)) => list_themes::run(args, &globals),
        Some(Subcmd::Gc(args)) => gc::run(args, &globals),
        Some(Subcmd::Uninstall(args)) => uninstall::run(args, &globals),
        Some(Subcmd::Update(args)) => update::run(args, &globals),
        Some(Subcmd::Channel(args)) => channel::run(args, &globals),
        Some(Subcmd::Worktree(args)) => worktree::run(args, &globals),
        Some(Subcmd::Agents(args)) => agents_cmd::run(*args, &globals),
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
                "`rimz tab` has moved to `rimz agents <spec> [prompt]`; teams now come from `[agents.teams]`"
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
    if let Ok(channel) = std::env::var(rimz::harness::run::ENV_CHANNEL)
        && !channel.is_empty()
    {
        return Some(channel);
    }
    rimz::harness::target::resolve_room_channel(
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
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<&'a AgentState> {
    map_resolve(
        raw,
        rimz::harness::target::resolve_one(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Resolve an ask id or agent ref while preserving each caller's ask scope.
///
/// `asks show` exposes root asks only and historically folds the awaiting
/// state into ask-id lookup; `answer` may target a sub-agent and reports a
/// matched-but-stale ask as "not asking" instead.
pub(crate) fn resolve_open_ask<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    current_channel: Option<&str>,
    root_awaiting_only: bool,
) -> Result<Option<&'a AgentState>> {
    if raw.starts_with("ask_") {
        let ask_id = AskId::parse(raw)?;
        return Ok(snapshot.agents.iter().find(|agent| {
            (!root_awaiting_only || (agent.parent_agent_id.is_none() && agent.is_awaiting_input()))
                && agent.open_ask.as_ref().is_some_and(|ask| ask.id == ask_id)
        }));
    }
    resolve_agent_one(snapshot, raw, None, current_channel).map(Some)
}

/// Resolve a ref to every matching live agent pane for `message --steer` and
/// send-now messages: the producer's bound panes, so a target reaches exactly the agent
/// panes the producer saw — bound sessions (at their live pane) and lazy panes
/// with no session yet.
pub(crate) fn resolve_pane_targets<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a rimz::PaneAgent>> {
    map_resolve(
        raw,
        rimz::harness::target::resolve_targets(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Turn a clean target miss into the launch-profile/command/layout hint when
/// the ref names launch config rather than a running agent.
fn map_resolve<T>(raw: &str, result: std::result::Result<T, rimz::TargetErr>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(rimz::TargetErr::NoMatch { target, suggestion }) => {
            if let Some(hint) = launch_ref_hint(raw)? {
                anyhow::bail!("{hint}; run `rimz agents list` to see live agents");
            }
            Err(rimz::TargetErr::NoMatch { target, suggestion }.into())
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
    /// Token-activity heatmap and usage insights.
    ///
    /// Includes model and agent breakdowns.
    Stats(stats::StatsArgs),
    /// Query provider account plans, auth, limits, credits, and spend.
    Providers(providers::ProvidersArgs),
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
    /// Create, list, and remove named channels.
    Channel(channel::ChannelArgs),
    /// Create, list, and remove RimZ-owned git worktrees.
    Worktree(worktree::WorktreeArgs),
    /// Launch agent tabs, optionally in RimZ-owned worktrees.
    Agents(Box<agents_cmd::AgentsArgs>),
    /// Discover, inspect, install, launch, and resume named teams.
    Teams(Box<teams::TeamsArgs>),
    /// Inspect the blocking prompts agents currently have open.
    Asks(asks::AsksArgs),
    /// Answer one current blocking prompt in its agent pane.
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
    /// Stream durable agent lifecycle transitions.
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

pub(crate) fn machine_config() -> std::sync::Arc<rimz::config::MachineConfig> {
    rimz::config::MachineConfig::load_lenient()
}

pub(crate) fn open_store(workspace: &rimz::ResolvedWorkspace) -> Result<Store> {
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing store paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let store = Store::open(paths, runtime).context("opening store")?;
    store
        .record_workspace(workspace)
        .context("recording workspace metadata")?;
    Ok(store)
}

pub(crate) fn runtime_paths_for(workspace_id: WorkspaceId) -> Result<RuntimePaths> {
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    Ok(runtime)
}

/// The agent roster the sidebar shows: the cached rollup with the daemon-mode
/// reap applied, so paneless Codex ghosts the app-server no longer holds drop
/// exactly as `rimz agents list` and the sidebar drop them. Best-effort and
/// fail-safe — an absent daemon-reap cache keeps every session
/// (see `SidebarSnapshot::reap_runtime`).
pub(crate) fn alive_snapshot(store: &Store, session: &str) -> Result<rimz::SidebarSnapshot> {
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
mod tests {
    use super::*;

    fn parsed_scope(args: &[&str]) -> (String, String, Option<String>, Option<String>) {
        let mut matches = help::customize(<Cli as CommandFactory>::command())
            .try_get_matches_from(args)
            .unwrap();
        let canonical = matches.subcommand_name().unwrap_or("start").to_owned();
        let cli = Cli::from_arg_matches_mut(&mut matches).unwrap();
        let facts = scope_facts(&canonical, cli.subcommand.as_ref());
        let command = facts.command.to_owned();
        let session = facts.session.map(ToOwned::to_owned);
        let agent = facts.agent.map(ToOwned::to_owned);
        (canonical, command, session, agent)
    }

    fn workspace(
        project_root: &str,
        worktree_root: &str,
        worktree_branch: Option<&str>,
    ) -> rimz::ResolvedWorkspace {
        let project_root = PathBuf::from(project_root);
        rimz::ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
            project_root,
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: PathBuf::from(worktree_root),
            worktree_branch: worktree_branch.map(ToOwned::to_owned),
            session_name: "rimz-test".to_owned(),
            mux_hint: None,
        }
    }

    #[test]
    fn room_channel_stamps_in_place_team_members() {
        let workspace = workspace("/code/team-channel", "/code/team-channel", None);

        assert_eq!(
            rimz::harness::target::resolve_room_channel(
                &workspace.project_root,
                &workspace.worktree_root,
                Some("forge"),
                None,
            )
            .as_deref(),
            Some("team-channel/forge")
        );
        assert_eq!(
            rimz::harness::target::resolve_room_channel(
                &workspace.project_root,
                &workspace.worktree_root,
                None,
                None,
            ),
            None
        );
    }

    #[test]
    fn room_channel_ignores_branch_for_lane_identity() {
        let branch = workspace("/code/project", "/code/project", Some("feat/auth"));
        assert_eq!(
            rimz::harness::target::resolve_room_channel(
                &branch.project_root,
                &branch.worktree_root,
                None,
                None,
            ),
            None
        );

        let child_worktree = workspace("/code/project", "/code/project-wt/auth", None);
        assert_eq!(
            rimz::harness::target::resolve_room_channel(
                &child_worktree.project_root,
                &child_worktree.worktree_root,
                None,
                None,
            )
            .as_deref(),
            Some("auth")
        );
    }

    #[test]
    fn mux_aliases_normalize_to_mux() {
        let mut cli = Cli::try_parse_from(["rimz", "--zellij"]).unwrap();
        cli.global.normalize().unwrap();
        assert_eq!(cli.global.mux, Some(MuxName::Zellij));

        let mut cli = Cli::try_parse_from(["rimz", "--tmux"]).unwrap();
        cli.global.normalize().unwrap();
        assert_eq!(cli.global.mux, Some(MuxName::Tmux));
    }

    #[test]
    fn message_alias_parses_send_and_subcommands() {
        let cli = Cli::try_parse_from(["rimz", "msg", "@codex", "hi"]).unwrap();
        assert!(matches!(cli.subcommand, Some(Subcmd::Message(_))));

        let cli = Cli::try_parse_from(["rimz", "msg", "list"]).unwrap();
        assert!(matches!(cli.subcommand, Some(Subcmd::Message(_))));
    }

    #[test]
    fn command_scope_uses_canonical_clap_labels() {
        assert_eq!(
            parsed_scope(&["rimz"]),
            ("start".to_owned(), "start".to_owned(), None, None)
        );
        assert_eq!(
            parsed_scope(&["rimz", "list"]),
            ("list".to_owned(), "list".to_owned(), None, None)
        );
        assert_eq!(
            parsed_scope(&["rimz", "msg", "@codex", "hi"]),
            ("message".to_owned(), "message".to_owned(), None, None)
        );
    }

    #[test]
    fn command_scope_keeps_nested_labels_and_agent_identity() {
        for (args, expected) in [
            (vec!["rimz", "remote", "list"], ("remote list", None, None)),
            (
                vec!["rimz", "sidebar", "snapshot"],
                ("sidebar snapshot", None, None),
            ),
            (
                vec!["rimz", "hooks", "feed", "--source", "codex"],
                ("hooks feed", None, Some("codex")),
            ),
            (
                vec![
                    "rimz",
                    "agents",
                    "refresh-context",
                    "--kind",
                    "codex",
                    "--session-id",
                    "sess-codex",
                    "--workspace-id",
                    "ws-test",
                ],
                ("agents refresh-context", Some("sess-codex"), Some("codex")),
            ),
            (
                vec![
                    "rimz",
                    "agents",
                    "refresh-context",
                    "--kind",
                    "opencode",
                    "--session-id",
                    "sess-opencode",
                    "--workspace-id",
                    "ws-test",
                    "--server-url",
                    "http://127.0.0.1:1",
                ],
                (
                    "agents refresh-context",
                    Some("sess-opencode"),
                    Some("opencode"),
                ),
            ),
        ] {
            let (_, command, session, agent) = parsed_scope(&args);
            assert_eq!(
                (command.as_str(), session.as_deref(), agent.as_deref()),
                expected,
                "{args:?}"
            );
        }
    }

    #[test]
    fn mux_aliases_are_global_flags() {
        let mut cli = Cli::try_parse_from(["rimz", "list", "--tmux"]).unwrap();
        cli.global.normalize().unwrap();
        assert_eq!(cli.global.mux, Some(MuxName::Tmux));
    }

    #[test]
    fn mux_aliases_conflict_with_each_other_and_mux() {
        let mut cli = Cli::try_parse_from(["rimz", "--zellij", "--tmux"]).unwrap();
        let err = cli.global.normalize().unwrap_err();
        assert_eq!(err.to_string(), "choose one of --mux, --zellij, --tmux");

        let mut cli = Cli::try_parse_from(["rimz", "--mux", "zellij", "--zellij"]).unwrap();
        let err = cli.global.normalize().unwrap_err();
        assert_eq!(err.to_string(), "choose one of --mux, --zellij, --tmux");
    }

    #[test]
    fn mux_option_still_normalizes_unchanged() {
        let mut cli = Cli::try_parse_from(["rimz", "--mux", "tmux"]).unwrap();
        cli.global.normalize().unwrap();
        assert_eq!(cli.global.mux, Some(MuxName::Tmux));
    }

    #[test]
    fn loop_verify_requires_a_spawned_agent() {
        let err = Cli::try_parse_from([
            "rimz",
            "loop",
            "add",
            "check-only",
            "--check",
            "false",
            "--verify",
            "true",
            "--every",
            "1h",
        ])
        .expect_err("verify needs an agent task");

        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn current_channel_is_stable_across_branch_changes() {
        let before = workspace("/code/project", "/code/project-wt/auth", Some("feat/auth"));
        let after = workspace("/code/project", "/code/project-wt/auth", Some("scratch"));

        let before_channel = rimz::harness::target::resolve_room_channel(
            &before.project_root,
            &before.worktree_root,
            None,
            None,
        );
        let after_channel = rimz::harness::target::resolve_room_channel(
            &after.project_root,
            &after.worktree_root,
            None,
            None,
        );
        assert_eq!(before_channel, after_channel);
        assert_eq!(before_channel.as_deref(), Some("auth"));
    }

    #[test]
    fn removed_top_level_command_rejects_before_global_help() {
        assert!(
            reject_removed_top_level_tokens_from([
                OsString::from("autoping"),
                OsString::from("--help")
            ])
            .is_err()
        );
        assert!(
            reject_removed_top_level_tokens_from([
                OsString::from("--root"),
                OsString::from("."),
                OsString::from("autoping"),
                OsString::from("--help"),
            ])
            .is_err()
        );
        assert!(
            reject_removed_top_level_tokens_from([
                OsString::from("--refresh-ms"),
                OsString::from("100"),
                OsString::from("autoping"),
            ])
            .is_err()
        );
        assert!(
            reject_removed_top_level_tokens_from([
                OsString::from("--tmux"),
                OsString::from("autoping"),
            ])
            .is_err()
        );
        assert!(reject_removed_top_level_tokens_from([OsString::from("run")]).is_err());
        assert!(reject_removed_top_level_tokens_from([OsString::from("tab")]).is_err());
        assert!(reject_removed_top_level_tokens_from([OsString::from("agents")]).is_ok());
        assert!(
            reject_removed_top_level_tokens_from([
                OsString::from("docs"),
                OsString::from("autoping"),
            ])
            .is_ok()
        );
    }
}
