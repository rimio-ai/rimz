//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod agents_cmd;
mod agents_launch;
mod attach_exec;
mod claude;
mod codex;
mod config;
mod daemon_view;
mod doctor;
mod event;
mod feed;
mod gc;
mod hook_consent;
mod hook_install;
mod hooks;
mod list;
mod list_themes;
mod pane;
mod parse;
mod queue;
mod reload;
mod remote;
mod render;
mod reset;
mod resolver;
mod resume;
mod room_recovery;
mod session_record;
mod setup;
mod sidebar;
mod start_notice;
mod statusline;
mod steer;
mod trust;
mod workspace;
mod worktree;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use rimz::agents::{HookInstallPreview, StatusLineChange};
use rimz::feed::AgentState;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::workspace_record;
use rimz::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, DaemonView, HostPane, MuxBackend,
    PresencePluginOptions, SessionHealth, SessionOptions, SidebarPaneOptions, SidebarWidth,
};
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths, WorkspaceRecord};

use attach_exec::{
    inside_selected_mux, report_already_inside, run_attach_action, should_report_already_inside,
};
use daemon_view::{build_daemon_view, maybe_launch_remote_control};
use hook_install::ensure_detected_agent_hooks;
use resume::{plan_room_resume, record_rebirth_boundary, report_resume, session_is_healthy_live};
use room_recovery::gate_room_before_attach;
use session_record::{pick_mux_for_session, retire_renamed_session, workspace_record_for_session};
use start_notice::report_start_notices;

pub(crate) use attach_exec::{attach_action, exec_attach_command};
pub(crate) use room_recovery::{print_reset_report, rebirth_room};
pub(crate) use start_notice::live_session_names;
/// Entry point used by `main.rs`.
pub fn dispatch() -> Result<()> {
    let cli = Cli::parse();
    let globals = cli.global;
    globals.color.write_global();
    rimz::observability::set_command_scope(scope_facts(cli.subcommand.as_ref()));
    match cli.subcommand {
        Some(Subcmd::Workspace(args)) => workspace::run(args, &globals),
        Some(Subcmd::List(args)) => list::run(args, &globals),
        Some(Subcmd::ListThemes(args)) => list_themes::run(args, &globals),
        Some(Subcmd::Event(args)) => event::run(args, &globals),
        Some(Subcmd::Feed(args)) => feed::run(args, &globals),
        Some(Subcmd::Gc(args)) => gc::run(args, &globals),
        Some(Subcmd::Worktree(args)) => worktree::run(args, &globals),
        Some(Subcmd::Agents(args)) => agents_cmd::run(*args, &globals),
        Some(Subcmd::Reload(args)) => reload::run(args, &globals),
        Some(Subcmd::Reset(args)) => reset::run(args, &globals),
        Some(Subcmd::Pane(args)) => pane::run(args, &globals),
        Some(Subcmd::Steer(args)) => steer::run(args, &globals),
        Some(Subcmd::Queue(args)) => queue::run(args, &globals),
        Some(Subcmd::Resolver(args)) => resolver::run(args, &globals),
        Some(Subcmd::Sidebar(args)) => sidebar::run(args, &globals),
        Some(Subcmd::Statusline(args)) => statusline::run(args, &globals),
        Some(Subcmd::Hooks(args)) => hooks::run(args, &globals),
        Some(Subcmd::Claude(args)) => claude::run(args, &globals),
        Some(Subcmd::Codex(args)) => codex::run(args, &globals),
        Some(Subcmd::Config(args)) => config::run(args, &globals),
        Some(Subcmd::Trust(args)) => trust::run(args, &globals),
        Some(Subcmd::Doctor(args)) => doctor::run(args, &globals),
        Some(Subcmd::Setup(args)) => setup::run(args, &globals),
        Some(Subcmd::Ping) => doctor::ping(),
        Some(Subcmd::Start(args)) => start(args, &globals),
        Some(Subcmd::Attach(args)) => attach(args, &globals),
        Some(Subcmd::Remote(args)) => remote::run(args, &globals),
        None => {
            let path = cli.path.unwrap_or_else(|| PathBuf::from("."));
            reject_removed_agent_command_path(&path)?;
            start(
                StartArgs {
                    path,
                    attach: cli.attach,
                    no_resume: cli.no_resume,
                    refresh_ms: cli.refresh_ms,
                },
                &globals,
            )
        }
    }
}

/// Low-cardinality Sentry scope facts for the resolved command: the command
/// label every event in this process inherits, plus the agent kind and session
/// when the command serves exactly one. The label is the command verb only,
/// never argument values, so it stays a stable Sentry facet.
fn scope_facts(sub: Option<&Subcmd>) -> rimz::observability::ScopeFacts<'_> {
    let (command, session, agent) = match sub {
        None | Some(Subcmd::Start(_)) => ("start", None, None),
        Some(Subcmd::Attach(_)) => ("attach", None, None),
        Some(Subcmd::Remote(_)) => ("remote", None, None),
        Some(Subcmd::Workspace(_)) => ("workspace", None, None),
        Some(Subcmd::List(_)) => ("list", None, None),
        Some(Subcmd::ListThemes(_)) => ("list-themes", None, None),
        Some(Subcmd::Event(_)) => ("event", None, None),
        Some(Subcmd::Feed(_)) => ("feed", None, None),
        Some(Subcmd::Gc(_)) => ("gc", None, None),
        Some(Subcmd::Worktree(_)) => ("worktree", None, None),
        Some(Subcmd::Agents(_)) => ("agents", None, None),
        Some(Subcmd::Reload(_)) => ("reload", None, None),
        Some(Subcmd::Reset(_)) => ("reset", None, None),
        Some(Subcmd::Pane(_)) => ("pane", None, None),
        Some(Subcmd::Steer(_)) => ("steer", None, None),
        Some(Subcmd::Queue(_)) => ("queue", None, None),
        Some(Subcmd::Resolver(_)) => ("resolver", None, None),
        Some(Subcmd::Sidebar(args)) => (args.command_label(), None, None),
        Some(Subcmd::Statusline(_)) => ("statusline", None, None),
        Some(Subcmd::Hooks(args)) => {
            let (command, agent) = args.scope();
            (command, None, agent)
        }
        Some(Subcmd::Claude(args)) => (args.command_label(), None, Some("claude")),
        Some(Subcmd::Codex(args)) => {
            let (command, session) = args.scope();
            (command, session, Some("codex"))
        }
        Some(Subcmd::Config(_)) => ("config", None, None),
        Some(Subcmd::Trust(_)) => ("trust", None, None),
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

fn reject_removed_agent_command_path(path: &Path) -> Result<()> {
    if path == Path::new("run") {
        anyhow::bail!(
            "`rimz run` has moved to `rimz agents <spec> <prompt> -p`; use `rimz agents show|wait|stop <ref>` for run records"
        );
    }
    if path == Path::new("tab") {
        anyhow::bail!(
            "`rimz tab` has moved to `rimz agents <spec> [prompt]`; layouts now come from `[agents.layouts]`"
        );
    }
    Ok(())
}

/// The current channel a command runs in: the worktree's branch, else its
/// directory basename when we are genuinely inside a separate worktree. A bare
/// directory workspace (root == worktree) yields `None`, which the resolver
/// reads as "all channels" rather than a silent narrowing.
pub(crate) fn current_channel(workspace: &rimz::ResolvedWorkspace) -> Option<String> {
    workspace.worktree_branch.clone().or_else(|| {
        (workspace.worktree_root != workspace.project_root)
            .then(|| {
                workspace
                    .worktree_root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .flatten()
    })
}

/// A human handle for an agent: its pet name, else `kind-ordinal`, else kind.
pub(crate) fn agent_label(agent: &AgentState) -> String {
    agent
        .name
        .clone()
        .unwrap_or_else(|| match agent.kind_ordinal {
            Some(ordinal) => format!("{}-{}", agent.kind, ordinal),
            None => agent.kind.to_string(),
        })
}

/// Gate a fan-out (more than one target) behind explicit confirmation. On a TTY,
/// prompt `<verb> N agents (…)? [y/N]`; off a TTY, refuse and point at `--yes`
/// so a script never broadcasts by surprise. Callers pass the per-target labels
/// so steer (panes) and queue (sessions) share one prompt.
pub(crate) fn confirm_fanout(verb: &str, target: &str, labels: &[String]) -> Result<()> {
    let list = labels.join(", ");
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "`{target}` fans out to {} agents ({list}); re-run with --yes to broadcast",
            labels.len()
        );
    }
    let mut stderr = std::io::stderr();
    write!(stderr, "{verb} {} agents ({list})? [y/N] ", labels.len())?;
    stderr.flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        anyhow::bail!("aborted");
    }
    Ok(())
}

/// Resolve a ref to exactly one agent (`show`/`focus`/`wait`/`stop`,
/// `queue clear`/`list`). `@all` or a fan-out kind is an explicit ambiguity.
pub(crate) fn resolve_agent_one<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<&'a AgentState> {
    map_resolve(
        raw,
        rimz::target::resolve_one(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Resolve a ref to every matching rollup agent for a broadcast (`queue add`).
pub(crate) fn resolve_agent_many<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a AgentState>> {
    map_resolve(
        raw,
        rimz::target::resolve_many(snapshot, raw, worktree_flag, current_channel),
    )
}

/// Resolve a ref to every matching live agent pane for `steer`: the producer's
/// bound panes, so a target reaches exactly the agent panes the producer saw —
/// bound sessions (at their live pane) and lazy panes with no session yet.
pub(crate) fn resolve_pane_targets<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a rimz::PaneAgent>> {
    map_resolve(
        raw,
        rimz::target::resolve_targets(snapshot, raw, worktree_flag, current_channel),
    )
}

/// The snapshot the talk commands (`steer`, `queue`) resolve against. Unlike the
/// rollup-only `snapshot_cached`, this folds a *fresh* live pane frame so a
/// just-started agent pane with no session yet is present and addressable —
/// `min_pane_cache_ms` floors the pull at now, bypassing the producer's pane
/// cache (up to 10s old in Zellij event mode) that would otherwise miss it. One
/// `list-panes` fork; falls back to the rollup when there is no mux to enumerate.
pub(crate) fn resolution_snapshot(
    workspace: &rimz::ResolvedWorkspace,
    ledger: &Ledger,
    globals: &GlobalFlags,
) -> Result<rimz::SidebarSnapshot> {
    use rimz::sidebar::cache::unix_now_ms;
    use rimz::sidebar::consumer::RollupCursor;
    use rimz::sidebar::produce::{ProduceOptions, pane_fixture_active, produce_snapshot};

    let mux = globals
        .mux
        .or_else(|| rimz::mux::auto_detect_backend(None).ok())
        // A deterministic pane fixture stands in for the mux in tests; produce
        // reads it without touching the real backend, so any mux value serves.
        .or_else(|| pane_fixture_active().then_some(MuxName::Zellij));
    let Some(mux) = mux else {
        return rollup_resolution_snapshot(ledger);
    };
    let state = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let opts = ProduceOptions {
        mux,
        session_name: workspace.session_name.clone(),
        exclude: None,
        min_pane_cache_ms: Some(unix_now_ms()),
        diag: None,
    };
    match produce_snapshot(&mut RollupCursor::new(), &state, &runtime, &opts) {
        Ok(snapshot) => Ok(snapshot),
        // No live session / pane discovery failed: fall back to the rollup's own
        // stamped panes so a bound agent still resolves, exactly as before.
        Err(_) => rollup_resolution_snapshot(ledger),
    }
}

/// The no-frame fallback: the rollup, with `agent_panes` synthesized from each
/// stamped session's pane. Without a live frame there is nothing to cwd-bind, so
/// only sessions that already carry a pane are reachable — the pre-fold steer
/// behaviour.
fn rollup_resolution_snapshot(ledger: &Ledger) -> Result<rimz::SidebarSnapshot> {
    let mut snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    snapshot.agent_panes = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter_map(|agent| {
            let pane = agent.pane.as_ref()?;
            Some(rimz::PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                agent_id: Some(agent.agent_id.clone()),
                pane_id: pane.pane_id.clone(),
                worktree_path: agent.worktree_path.clone(),
                worktree_branch: agent.worktree_branch.clone(),
            })
        })
        .collect();
    Ok(snapshot)
}

/// Turn a clean target miss into the launch-alias/layout hint when the ref
/// names a launch alias or layout rather than a running agent.
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
    let config = machine_config()?;
    if config.agents.aliases.0.contains_key(selector) {
        return Ok(Some(format!(
            "`{selector}` is a launch alias, not a running agent"
        )));
    }
    if selector == "peer" || config.agents.layouts.0.contains_key(selector) {
        return Ok(Some(format!(
            "`{selector}` is a launch layout, not a running agent"
        )));
    }
    Ok(None)
}

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    bin_name = "rimz",
    about = "One room per project for agents, scripts, and humans.",
    subcommand_negates_reqs = true
)]
struct Cli {
    #[clap(flatten)]
    global: GlobalFlags,

    /// Optional path; equivalent to `rimz start <path>`.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    #[clap(flatten)]
    attach: AttachFlags,

    /// Come up empty: skip re-seeding prior agents when the session is reborn.
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
    /// Workspace identity helpers.
    Workspace(workspace::WorkspaceArgs),
    /// Show known workspaces and which mux is currently running them.
    List(list::ListArgs),
    /// List the bundled sidebar theme names.
    ListThemes(list_themes::ListThemesArgs),
    /// Emit generic events into the workspace ledger.
    Event(event::EventArgs),
    /// Feed primitives: ask, push, list, show, resolve, dismiss, abstain.
    Feed(feed::FeedArgs),
    /// Remove stale runtime liveness hints.
    Gc(gc::GcArgs),
    /// Create, list, and remove Rimz-owned git worktrees.
    Worktree(worktree::WorktreeArgs),
    /// Launch agent tabs, optionally in Rimz-owned worktrees.
    Agents(Box<agents_cmd::AgentsArgs>),
    /// Reload running sidebars in place (pick up a freshly-installed build).
    Reload(reload::ReloadArgs),
    /// Force a clean rebirth of this workspace's room, destroying a stuck or
    /// resurrected Zellij session and sweeping its orphaned processes.
    Reset(reset::ResetArgs),
    /// Pane primitives backed by the selected mux backend.
    Pane(pane::PaneArgs),
    /// Send state-gated text to a live agent pane.
    Steer(steer::SteerArgs),
    /// Queue text for delivery when an agent finishes a turn.
    Queue(queue::QueueArgs),
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
    /// Claude helper API. The sidebar calls these; humans usually do not.
    #[command(hide = true)]
    Claude(claude::ClaudeArgs),
    /// Codex helper API. The Codex hook calls these; humans usually do not.
    #[command(hide = true)]
    Codex(codex::CodexArgs),
    /// Inspect and edit the per-machine config.
    Config(config::ConfigArgs),
    /// Manage the project's executable-surface trust grant.
    Trust(trust::TrustArgs),
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
    /// Come up empty: skip re-seeding prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
    /// Override the sidebar render cadence for this launch.
    #[arg(long)]
    pub refresh_ms: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_run_and_tab_tokens_do_not_fall_through_to_path_start() {
        assert!(reject_removed_agent_command_path(Path::new("run")).is_err());
        assert!(reject_removed_agent_command_path(Path::new("tab")).is_err());
        assert!(reject_removed_agent_command_path(Path::new("docs")).is_ok());
    }
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
    fn mode(&self) -> AttachMode {
        if self.attach {
            AttachMode::Attach
        } else if self.no_attach || self.print {
            AttachMode::Print
        } else {
            AttachMode::Auto
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
    /// Come up empty: skip re-seeding prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
    /// Override the sidebar render cadence for this launch.
    #[arg(long)]
    pub refresh_ms: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachMode {
    Auto,
    Attach,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachAction {
    Exec,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingSessionReport {
    Silent,
    Warn,
}

fn parse_mux(value: &str) -> std::result::Result<MuxName, String> {
    value.parse::<MuxName>().map_err(|err| err.to_string())
}

fn mux_environment_preflight(mux: MuxName, session_name: &str) -> Result<()> {
    match mux {
        MuxName::Zellij => rimz::mux::zellij::socket_preflight(session_name)?,
        // tmux sockets live under its own short per-user socket directory; the
        // Rimz session name does not participate in an AF_UNIX path budget.
        MuxName::Tmux => {}
    }
    Ok(())
}

fn rimz_socket_environment_preflight(workspace_id: &WorkspaceId) -> Result<()> {
    RuntimePaths::for_workspace(workspace_id.clone())
        .map(|_| ())
        .context("checking Rimz runtime socket budget")
}

fn start(args: StartArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&args.path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", args.path.display()))?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    // A same-mux room can't be nested: if we're already inside this backend's
    // session, report the directory's room and stop before any launch side
    // effect — hook install, session birth, sidebar, or the doomed nested
    // `attach --create`.
    if should_report_already_inside(args.attach.mode(), inside_selected_mux(mux)) {
        report_already_inside(mux, &workspace)?;
        return Ok(());
    }
    report_start_notices(&workspace)?;
    let machine_config = machine_config()?;
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    // One terminal probe per command flow: the width picks every sidebar
    // pane's birth size; the pair sizes a detached tmux birth.
    let detected_size = rimz::mux::detect_terminal_size();
    let remote_control = &machine_config.remote_control;
    // Fail-fast precondition: an enabled host that cannot start aborts the
    // launch here, with the fix, before any hook-install or session side
    // effects — never bring a workspace up around a doomed host.
    rimz::remote_control::preflight(remote_control)?;
    rimz_socket_environment_preflight(&workspace.workspace_id)?;
    mux_environment_preflight(mux, &workspace.session_name)?;
    ensure_detected_agent_hooks()?;
    let backend = rimz::mux::backend_for(mux);
    retire_renamed_session(backend.as_ref(), &workspace);
    // Capture whether this is a plain reattach *before* `ensure_session`, which on
    // tmux would create the session and erase the distinction. A healthy live room
    // re-seeds nothing; only a birth (absent or stuck) resumes prior agents.
    let was_live = session_is_healthy_live(backend.as_ref(), &workspace.session_name);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: workspace.worktree_root.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    // The daemon view (`rimzd`) is computed once, before the session is born: its
    // hosts depend on config and which agents are on PATH. When present, it leads
    // the session — on Zellij that order is fixed at birth (`open_sidebar` renders
    // the daemon tab first), since Zellij can't reorder tabs afterwards.
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &workspace.worktree_root,
        mux_config: &mux_config,
        width: sidebar_width,
        detected_size,
        refresh_ms: args.refresh_ms,
    };
    let daemon_view = build_daemon_view(remote_control, &workspace, &mux_config, &room);
    let daemon = daemon_view.as_ref().map(|view| DaemonView {
        name: view.name.clone(),
        hosts: view.hosts.clone(),
    });
    // Plan which prior agents the reborn room re-seeds, from the durable rollup.
    // Empty on a healthy reattach (the agents are still alive), when nothing is
    // recoverable, or when the user opted out — then the birth is exactly today's
    // bare working room.
    let resume_plan = if was_live {
        rimz::resume::ResumePlan::default()
    } else {
        let plan = plan_room_resume(
            &workspace.workspace_id,
            &machine_config.resume,
            args.no_resume,
        );
        record_rebirth_boundary(&workspace.workspace_id, &workspace.session_name);
        plan
    };
    launch_sidebar_for_workspace(backend.as_ref(), &room, daemon.as_ref(), &resume_plan.panes);
    maybe_launch_remote_control(
        backend.as_ref(),
        &workspace,
        remote_control,
        daemon_view.as_ref(),
    );
    // Authoritative gate before the resurrecting `attach --create`: rebirth an
    // inspected stale/serialized room, and on one that cannot self-heal or
    // cannot be inspected, offer a reset (interactive) or fail fast with the fix
    // (non-interactive). The reborn room is seeded with the resume panes.
    gate_room_before_attach(backend.as_ref(), &room, daemon.as_ref(), &resume_plan.panes)?;
    report_resume(&resume_plan);
    ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );
    let spec = backend.attach_command(&workspace.session_name, &mux_config);
    tracing::info!(
        workspace = %workspace.workspace_id,
        session = %workspace.session_name,
        mux = %mux,
        "workspace ready",
    );
    run_attach_action(&spec, args.attach.mode(), mux)
}

/// Best-effort load of the session's presence plugin — the Zellij push
/// channel that retires the producer's steady-state pane poll (tmux is a
/// no-op; its control-mode watch already pushes). Fired on every attach-shaped
/// flow: the load verb is idempotent and clientless-safe, so a room born
/// detached, a reattach, and a permission granted minutes after the first
/// prompt all converge with no machinery of their own. Failure costs latency
/// only — the producer keeps today's poll — so it never blocks an attach.
fn ensure_presence_plugin(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
) {
    let Some(wasm) = rimz::mux::zellij::presence_plugin_path() else {
        tracing::debug!(
            session = %session_name,
            "presence plugin unavailable; the producer keeps its pane poll",
        );
        return;
    };
    let opts = PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: sidebar::rimz_cli_program(),
        converge: false,
    };
    if let Err(err) = backend.ensure_presence_plugin(&opts) {
        tracing::debug!(
            session = %session_name,
            error = %err,
            "presence plugin load failed; the producer keeps its pane poll",
        );
    }
}

fn attach(args: AttachArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = args.attach.mode();
    match args.workspace {
        Some(session) => attach_named(&session, mode, args.no_resume, args.refresh_ms, globals),
        None => attach_cwd(mode, args.no_resume, args.refresh_ms, globals),
    }
}

fn attach_cwd(
    mode: AttachMode,
    no_resume: bool,
    refresh_ms: Option<u16>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let machine_config = machine_config()?;
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    rimz_socket_environment_preflight(&workspace.workspace_id)?;
    mux_environment_preflight(mux, &workspace.session_name)?;
    retire_renamed_session(backend.as_ref(), &workspace);
    let was_live = session_is_healthy_live(backend.as_ref(), &workspace.session_name);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: workspace.worktree_root.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    let resume_plan = if was_live {
        rimz::resume::ResumePlan::default()
    } else {
        let plan = plan_room_resume(&workspace.workspace_id, &machine_config.resume, no_resume);
        record_rebirth_boundary(&workspace.workspace_id, &workspace.session_name);
        plan
    };
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &workspace.worktree_root,
        mux_config: &mux_config,
        width: sidebar_width,
        detected_size,
        refresh_ms,
    };
    launch_sidebar_for_workspace(backend.as_ref(), &room, None, &resume_plan.panes);
    gate_room_before_attach(backend.as_ref(), &room, None, &resume_plan.panes)?;
    report_resume(&resume_plan);
    ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );
    let spec = backend.attach_command(&workspace.session_name, &mux_config);
    run_attach_action(&spec, mode, mux)
}

fn attach_named(
    session: &str,
    mode: AttachMode,
    no_resume: bool,
    refresh_ms: Option<u16>,
    globals: &GlobalFlags,
) -> Result<()> {
    let record = workspace_record_for_session(session);
    let missing_report = if matches!(record, Ok(Some(_))) {
        MissingSessionReport::Silent
    } else {
        MissingSessionReport::Warn
    };
    let machine_config = machine_config()?;
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let mux = pick_mux_for_session(session, globals.mux, missing_report)?;
    let backend = rimz::mux::backend_for(mux);
    mux_environment_preflight(mux, session)?;
    if let Ok(Some(record)) = &record {
        rimz_socket_environment_preflight(&record.workspace_id)?;
    }
    // Captured before `ensure_session` so a tmux create never masks a reattach.
    let was_live = session_is_healthy_live(backend.as_ref(), session);
    match record {
        Ok(Some(record)) => {
            backend.ensure_session(&SessionOptions {
                session_name: record.session_name.clone(),
                workspace_id: record.workspace_id.clone(),
                project_root: record.project_root.clone(),
                cwd: record.project_root.clone(),
                config: mux_config.clone(),
                detected_size,
            })?;
            let resume_plan = if was_live {
                rimz::resume::ResumePlan::default()
            } else {
                let plan =
                    plan_room_resume(&record.workspace_id, &machine_config.resume, no_resume);
                record_rebirth_boundary(&record.workspace_id, &record.session_name);
                plan
            };
            let room = RoomTarget {
                workspace_id: &record.workspace_id,
                project_root: &record.project_root,
                session_name: &record.session_name,
                cwd: &record.project_root,
                mux_config: &mux_config,
                width: sidebar_width,
                detected_size,
                refresh_ms,
            };
            launch_sidebar_for_workspace(backend.as_ref(), &room, None, &resume_plan.panes);
            // Only a session Rimz owns (a matching record) is force-reset; a bare
            // external session by this name is never torn down.
            gate_room_before_attach(backend.as_ref(), &room, None, &resume_plan.panes)?;
            report_resume(&resume_plan);
            ensure_presence_plugin(backend.as_ref(), &record.session_name, &record.workspace_id);
        }
        Ok(None) => {
            tracing::warn!(
                session = %session,
                "no workspace record matches session; emitting attach command only",
            );
        }
        Err(err) => {
            tracing::warn!(
                session = %session,
                error = %err,
                "workspace record lookup failed; emitting attach command only",
            );
        }
    }
    let spec = backend.attach_command(session, &mux_config);
    run_attach_action(&spec, mode, mux)
}

/// The room a sidebar launch or pre-attach gate targets: workspace identity
/// plus the per-machine knobs every [`SidebarPaneOptions`] build shares.
struct RoomTarget<'a> {
    workspace_id: &'a rimz::WorkspaceId,
    /// The workspace root behind the id — paired into the identity pin a
    /// session birth stamps into the mux environment.
    project_root: &'a Path,
    session_name: &'a str,
    cwd: &'a Path,
    mux_config: &'a rimz::config::MultiplexerConfig,
    width: SidebarWidth,
    /// The launching terminal's `(cols, rows)`, probed once per command
    /// ([`rimz::mux::detect_terminal_size`]): the width picks the sidebar's
    /// birth size, the pair sizes a detached tmux birth.
    detected_size: Option<(u16, u16)>,
    /// One-shot sidebar render-cadence override for panes born during this
    /// launch. Recovery rebuilt from workspace state falls back to config.
    refresh_ms: Option<u16>,
}

impl RoomTarget<'_> {
    /// The width verdict this command's sidebar panes are born with —
    /// `min(percent × launching terminal, max_cols)`, resolved once here and
    /// constant for the session's life.
    fn birth_size(&self) -> rimz::mux::BirthSize {
        self.width
            .birth_size(self.detected_size.map(|(cols, _)| cols))
    }
}

fn build_sidebar_opts(
    target: &RoomTarget<'_>,
    resume_panes: Vec<rimz::mux::ResumePane>,
) -> Result<SidebarPaneOptions> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    Ok(SidebarPaneOptions {
        session_name: target.session_name.to_owned(),
        workspace_id: target.workspace_id.clone(),
        project_root: target.project_root.to_path_buf(),
        cwd: target.cwd.to_path_buf(),
        width: target.width,
        birth_size: target.birth_size(),
        rimz_bin,
        replace_existing: false,
        config: target.mux_config.clone(),
        resume_panes,
        refresh_ms: target.refresh_ms,
    })
}

fn launch_sidebar_for_workspace(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> rimz::sidebar::SidebarLaunchOutcome {
    let runtime = match RuntimePaths::for_workspace(target.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(
                workspace = %target.workspace_id,
                error = %err,
                "sidebar launch skipped because runtime paths are unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    let opts = match build_sidebar_opts(target, resume_panes.to_vec()) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::warn!(
                workspace = %target.workspace_id,
                error = %err,
                "sidebar launch skipped because room options are unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    rimz::sidebar::launch_sidebar_if_needed(backend, &runtime, &opts, daemon)
}

pub(crate) fn confirm(prompt: &str) -> Result<bool> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{prompt} [y/N] ")?;
    stderr.flush()?;
    drop(stderr);
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

pub(crate) fn machine_config() -> Result<rimz::config::MachineConfig> {
    rimz::config::MachineConfig::load().context("loading per-machine config")
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
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;
    ledger
        .record_workspace(workspace)
        .context("recording workspace metadata")
}
