//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod agents_cmd;
mod agents_launch;
mod channel;
mod codex;
mod config;
mod coverage;
mod daemon;
mod doctor;
mod event;
mod feed;
mod gc;
mod hooks;
mod list;
mod list_pets;
mod list_themes;
mod loop_cmd;
mod message;
mod opencode;
mod pane;
mod parse;
mod reload;
mod remote;
mod render;
mod reset;
mod resolver;
pub(crate) mod room;
mod send;
mod setup;
mod sidebar;
mod spinner;
mod stats;
mod statusline;
mod transcript;
mod trust;
mod version;
mod web;
mod workspace;
mod worktree;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use rimz::agents::AgentState;
use rimz::ids::MuxName;
use rimz::{Ledger, RuntimePaths, StatePaths};
/// Entry point used by `main.rs`.
pub fn dispatch() -> Result<()> {
    reject_removed_top_level_tokens()?;
    let cli = Cli::parse();
    let globals = cli.global;
    globals.color.write_global();
    rimz::observability::set_command_scope(scope_facts(cli.subcommand.as_ref()));
    match cli.subcommand {
        Some(Subcmd::Workspace(args)) => workspace::run(args, &globals),
        Some(Subcmd::List(args)) => list::run(args, &globals),
        Some(Subcmd::Stats(args)) => stats::run(args, &globals),
        Some(Subcmd::ListPets(args)) => list_pets::run(args, &globals),
        Some(Subcmd::ListThemes(args)) => list_themes::run(args, &globals),
        Some(Subcmd::Event(args)) => event::run(args, &globals),
        Some(Subcmd::Feed(args)) => feed::run(args, &globals),
        Some(Subcmd::Gc(args)) => gc::run(args, &globals),
        Some(Subcmd::Channel(args)) => channel::run(args, &globals),
        Some(Subcmd::Worktree(args)) => worktree::run(args, &globals),
        Some(Subcmd::Agents(args)) => agents_cmd::run(*args, &globals),
        Some(Subcmd::Loop(args)) => loop_cmd::run(args, &globals),
        Some(Subcmd::Reload(args)) => reload::run(args, &globals),
        Some(Subcmd::Reset(args)) => reset::run(args, &globals),
        Some(Subcmd::Pane(args)) => pane::run(args, &globals),
        Some(Subcmd::Message(args)) => message::run(args, &globals),
        Some(Subcmd::Resolver(args)) => resolver::run(args, &globals),
        Some(Subcmd::Sidebar(args)) => sidebar::run(args, &globals),
        Some(Subcmd::Statusline(args)) => statusline::run(args, &globals),
        Some(Subcmd::Hooks(args)) => hooks::run(args, &globals),
        Some(Subcmd::Codex(args)) => codex::run(args, &globals),
        Some(Subcmd::Opencode(args)) => opencode::run(args, &globals),
        Some(Subcmd::Daemon(args)) => daemon::run(args, &globals),
        Some(Subcmd::Config(args)) => config::run(args, &globals),
        Some(Subcmd::Coverage(args)) => coverage::run(args, &globals),
        Some(Subcmd::Trust(args)) => trust::run(args, &globals),
        Some(Subcmd::Transcript(args)) => transcript::run(args, &globals),
        Some(Subcmd::Doctor(args)) => doctor::run(args, &globals),
        Some(Subcmd::Setup(args)) => setup::run(args, &globals),
        Some(Subcmd::Ping) => doctor::ping(),
        Some(Subcmd::Start(args)) => room::start(args, &globals),
        Some(Subcmd::Attach(args)) => room::attach(args, &globals),
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
fn scope_facts(sub: Option<&Subcmd>) -> rimz::observability::ScopeFacts<'_> {
    let (command, session, agent) = match sub {
        None | Some(Subcmd::Start(_)) => ("start", None, None),
        Some(Subcmd::Attach(_)) => ("attach", None, None),
        Some(Subcmd::Remote(args)) => (args.command_label(), None, None),
        Some(Subcmd::Web(_)) => ("web", None, None),
        Some(Subcmd::Workspace(_)) => ("workspace", None, None),
        Some(Subcmd::List(_)) => ("list", None, None),
        Some(Subcmd::Stats(_)) => ("stats", None, None),
        Some(Subcmd::ListPets(_)) => ("list-pets", None, None),
        Some(Subcmd::ListThemes(_)) => ("list-themes", None, None),
        Some(Subcmd::Event(_)) => ("event", None, None),
        Some(Subcmd::Feed(_)) => ("feed", None, None),
        Some(Subcmd::Gc(_)) => ("gc", None, None),
        Some(Subcmd::Channel(_)) => ("channel", None, None),
        Some(Subcmd::Worktree(_)) => ("worktree", None, None),
        Some(Subcmd::Agents(_)) => ("agents", None, None),
        Some(Subcmd::Loop(_)) => ("loop", None, None),
        Some(Subcmd::Reload(_)) => ("reload", None, None),
        Some(Subcmd::Reset(_)) => ("reset", None, None),
        Some(Subcmd::Pane(_)) => ("pane", None, None),
        Some(Subcmd::Message(_)) => ("message", None, None),
        Some(Subcmd::Resolver(_)) => ("resolver", None, None),
        Some(Subcmd::Sidebar(args)) => (args.command_label(), None, None),
        Some(Subcmd::Statusline(_)) => ("statusline", None, None),
        Some(Subcmd::Hooks(args)) => {
            let (command, agent) = args.scope();
            (command, None, agent)
        }
        Some(Subcmd::Codex(args)) => {
            let (command, session) = args.scope();
            (command, session, Some("codex"))
        }
        Some(Subcmd::Opencode(args)) => {
            let (command, session) = args.scope();
            (command, session, Some("opencode"))
        }
        Some(Subcmd::Daemon(_)) => ("daemon", None, None),
        Some(Subcmd::Config(_)) => ("config", None, None),
        Some(Subcmd::Coverage(_)) => ("coverage", None, None),
        Some(Subcmd::Trust(_)) => ("trust", None, None),
        Some(Subcmd::Transcript(_)) => ("transcript", None, None),
        Some(Subcmd::Doctor(_)) => ("doctor", None, None),
        Some(Subcmd::Setup(_)) => ("setup", None, None),
        Some(Subcmd::Ping) => ("ping", None, None),
    };
    rimz::observability::ScopeFacts {
        command,
        session,
        agent,
    }
}

/// The current channel a command runs in: an explicit named lane from
/// `RIMZ_CHANNEL`, else the worktree's branch, else its directory basename when
/// we are genuinely inside a separate worktree. A bare directory workspace
/// (root == worktree) yields `None` for humans, but Rimz-launched members carry
/// `RIMZ_CHANNEL` or `RIMZ_TEAM`, so their calls scope to that lane.
pub(crate) fn current_channel(workspace: &rimz::ResolvedWorkspace) -> Option<String> {
    if let Ok(channel) = std::env::var(rimz::harness::run::ENV_CHANNEL)
        && !channel.is_empty()
    {
        return Some(channel);
    }
    let team = std::env::var(rimz::harness::run::ENV_TEAM).ok();
    current_channel_for_team(workspace, team.as_deref())
}

fn current_channel_for_team(
    workspace: &rimz::ResolvedWorkspace,
    team: Option<&str>,
) -> Option<String> {
    if let Some(branch) = workspace.worktree_branch.as_deref() {
        return Some(branch.to_owned());
    }
    if workspace.worktree_root != workspace.project_root {
        return workspace
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    match (
        workspace
            .project_root
            .file_name()
            .map(|name| name.to_string_lossy()),
        team.filter(|value| !value.is_empty()),
    ) {
        (Some(dir), Some(team)) => Some(format!("{dir}/{team}")),
        (None, Some(team)) => Some(team.to_owned()),
        _ => None,
    }
}

/// Refuse a plain selector that matched several agents. A bare `@<kind>`/`@<profile>`
/// names a profile, not "everyone", so several matches is a "pick one" error: it
/// lists the disambiguating handles to retype and names `--all` as the opt-in to
/// reach every match. The explicit broadcast `@all` and `--all` fan out directly;
/// each delivery carries the addressed handle as a prefix.
pub(crate) fn ambiguous_fanout(verb: &str, target: &str, labels: &[String]) -> anyhow::Error {
    let list = labels
        .iter()
        .map(|label| format!("@{label}"))
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

/// The snapshot the `message` command resolves against. Unlike the
/// rollup-only `snapshot_cached`, this folds a *fresh* live pane frame onto the
/// rollup without the render spine, so a just-started sessionless pane is
/// addressable without paying group-root, spending, account, dashboard, or git
/// enrichment. `min_pane_cache_ms` floors the pane pull at now, bypassing the
/// producer's pane cache (up to 10s old in Zellij event mode). One `list-panes`
/// fork; falls back to the rollup when there is no mux to enumerate.
pub(crate) fn resolution_snapshot(
    workspace: &rimz::ResolvedWorkspace,
    ledger: &Ledger,
    globals: &GlobalFlags,
) -> Result<rimz::SidebarSnapshot> {
    Ok(rimz::sidebar::produce::resolution_snapshot(
        workspace,
        ledger,
        globals.mux,
    )?)
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
    version = version::VERSION,
    bin_name = "rimz",
    about = "One room per project for agents, scripts, and humans."
)]
struct Cli {
    #[clap(flatten)]
    global: GlobalFlags,

    #[clap(flatten)]
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
    #[arg(long, value_parser = parse_mux, global = true)]
    pub mux: Option<MuxName>,
    /// Override project-root resolution (monorepo escape hatch).
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    /// When to colorize human output: `auto` (default), `always`, or `never`.
    /// `auto` follows the terminal and the `NO_COLOR`/`CLICOLOR` environment.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, global = true)]
    pub color: ColorWhen,
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
    /// Start or attach to a workspace session (default action).
    Start(StartArgs),
    /// Attach to a workspace session by name.
    Attach(AttachArgs),
    /// Manage and connect to SSH remote rooms.
    Remote(remote::RemoteArgs),
    /// Open a Zellij room in the browser.
    Web(web::WebArgs),
    /// Workspace identity helpers.
    Workspace(workspace::WorkspaceArgs),
    /// Show known workspaces and which mux is currently running them.
    List(list::ListArgs),
    /// Token-activity heatmap, model/agent breakdowns, and usage insights.
    Stats(stats::StatsArgs),
    /// Preview the bundled provider-dashboard pets as cell-art.
    ListPets(list_pets::ListPetsArgs),
    /// List the bundled sidebar theme names.
    ListThemes(list_themes::ListThemesArgs),
    /// Emit generic events into the workspace ledger.
    Event(event::EventArgs),
    /// Feed primitives: ask, push, list, show, resolve, dismiss, abstain.
    Feed(feed::FeedArgs),
    /// Remove stale runtime liveness hints.
    Gc(gc::GcArgs),
    /// Create, list, and remove named channels.
    Channel(channel::ChannelArgs),
    /// Create, list, and remove Rimz-owned git worktrees.
    Worktree(worktree::WorktreeArgs),
    /// Launch agent tabs, optionally in Rimz-owned worktrees.
    Agents(Box<agents_cmd::AgentsArgs>),
    /// Schedule supervised agent turns from the room's sidebar elder.
    #[command(name = "loop")]
    Loop(loop_cmd::LoopArgs),
    /// Reload running sidebars in place (pick up a freshly-installed build).
    Reload(reload::ReloadArgs),
    /// Force a clean rebirth of this workspace's room, destroying a stuck or
    /// resurrected Zellij session and sweeping its orphaned processes.
    Reset(reset::ResetArgs),
    /// Pane primitives backed by the selected mux backend.
    Pane(pane::PaneArgs),
    /// Message an agent: `--steer` interrupts now; the default parks for the
    /// next safe turn boundary, optionally after `--schedule`.
    Message(message::MessageArgs),
    /// Manage the per-machine resolver allowlist.
    Resolver(resolver::ResolverArgs),
    /// Sidebar helper API. The sidebar calls these; humans usually do not.
    #[command(hide = true)]
    Sidebar(sidebar::SidebarArgs),
    /// Statusline datasource. The installed `statusLine` command calls this;
    /// humans do not.
    #[command(hide = true)]
    Statusline(statusline::StatuslineArgs),
    /// Install/uninstall agent hooks. Internal hook entrypoints live here too.
    Hooks(hooks::HooksArgs),
    /// Codex helper API. The Codex hook calls these; humans usually do not.
    #[command(hide = true)]
    Codex(codex::CodexArgs),
    /// OpenCode helper API. The OpenCode hook calls these; humans usually do not.
    #[command(hide = true)]
    Opencode(opencode::OpencodeArgs),
    /// Daemon dashboard helper API. The rimzd content panes call this; humans do not.
    #[command(hide = true)]
    Daemon(daemon::DaemonArgs),
    /// Inspect and edit the per-machine config.
    Config(config::ConfigArgs),
    /// Adapter integration-concern and lifecycle-hook coverage matrices.
    Coverage(coverage::CoverageArgs),
    /// Manage the project's executable-surface trust grant.
    Trust(trust::TrustArgs),
    /// Inspect agent or channel conversation transcripts.
    Transcript(transcript::TranscriptArgs),
    /// Environment + backend report.
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
    #[arg(value_name = "SESSION")]
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

pub(crate) fn machine_config() -> rimz::config::MachineConfig {
    rimz::config::MachineConfig::load_lenient()
}

pub(crate) fn open_ledger(workspace: &rimz::ResolvedWorkspace) -> Result<Ledger> {
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;
    ledger
        .record_workspace(workspace)
        .context("recording workspace metadata")?;
    Ok(ledger)
}

pub(crate) fn record_workspace(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    open_ledger(workspace).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn current_channel_scopes_in_place_team_members() {
        let workspace = workspace("/code/team-channel", "/code/team-channel", None);

        assert_eq!(
            current_channel_for_team(&workspace, Some("pcr")).as_deref(),
            Some("team-channel/pcr")
        );
        assert_eq!(current_channel_for_team(&workspace, None), None);
    }

    #[test]
    fn current_channel_keeps_worktree_precedence() {
        let branch = workspace("/code/project", "/code/project", Some("feat/auth"));
        assert_eq!(
            current_channel_for_team(&branch, Some("pcr")).as_deref(),
            Some("feat/auth")
        );

        let child_worktree = workspace("/code/project", "/code/project-wt/auth", None);
        assert_eq!(
            current_channel_for_team(&child_worktree, Some("pcr")).as_deref(),
            Some("auth")
        );
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
