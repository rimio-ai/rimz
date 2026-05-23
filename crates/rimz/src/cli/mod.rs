//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod doctor;
mod event;
mod feed;
mod hooks;
mod pane;
mod resolver;
mod sidebar;
mod workspace;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use rimz::ids::MuxName;
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths};

/// Entry point used by `main.rs`.
pub fn dispatch() -> Result<()> {
    let cli = Cli::parse();
    let globals = cli.global;
    match cli.subcommand {
        Some(Subcmd::Workspace(args)) => workspace::run(args, &globals),
        Some(Subcmd::Event(args)) => event::run(args, &globals),
        Some(Subcmd::Feed(args)) => feed::run(args, &globals),
        Some(Subcmd::Pane(args)) => pane::run(args, &globals),
        Some(Subcmd::Resolver(args)) => resolver::run(args, &globals),
        Some(Subcmd::Sidebar(args)) => sidebar::run(args, &globals),
        Some(Subcmd::Hooks(args)) => hooks::run(args, &globals),
        Some(Subcmd::Doctor) => doctor::run(&globals),
        Some(Subcmd::Ping) => doctor::ping(),
        Some(Subcmd::Start(args)) => start(args, &globals),
        Some(Subcmd::Attach { workspace }) => attach(workspace, &globals),
        None => start(
            StartArgs {
                path: cli.path.unwrap_or_else(|| PathBuf::from(".")),
            },
            &globals,
        ),
    }
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
}

#[derive(Debug, Subcommand)]
enum Subcmd {
    /// Start or attach to a workspace session (default action).
    Start(StartArgs),
    /// Attach to a workspace session by name.
    Attach {
        /// Workspace session name (omit to use the cwd's workspace).
        workspace: Option<String>,
    },
    /// Workspace identity helpers.
    Workspace(workspace::WorkspaceArgs),
    /// Emit generic events into the workspace ledger.
    Event(event::EventArgs),
    /// Feed primitives: ask, push, list, show, resolve, dismiss.
    Feed(feed::FeedArgs),
    /// Pane primitives backed by the selected mux backend.
    Pane(pane::PaneArgs),
    /// Manage the per-machine resolver allowlist.
    Resolver(resolver::ResolverArgs),
    /// Sidebar helper API. The sidebar calls these; humans usually do not.
    #[command(hide = true)]
    Sidebar(sidebar::SidebarArgs),
    /// Install/uninstall agent hooks. Internal hook entrypoints live here too.
    Hooks(hooks::HooksArgs),
    /// Environment + backend report.
    Doctor,
    /// Machine-readable liveness check (prints `ok`).
    Ping,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Path to use as the workspace cwd.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

fn parse_mux(value: &str) -> std::result::Result<MuxName, String> {
    value.parse::<MuxName>().map_err(|err| err.to_string())
}

fn start(args: StartArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&args.path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", args.path.display()))?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    backend.ensure_session(&workspace.session_name)?;
    let spec = backend.attach_command(&workspace.session_name);
    // Print the attach command so the caller can decide whether to exec it.
    tracing::info!(
        workspace = %workspace.workspace_id,
        session = %workspace.session_name,
        mux = %mux,
        "workspace ready; run the attach command below to enter it",
    );
    print_attach_command(&spec);
    Ok(())
}

fn attach(workspace_name: Option<String>, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let session = workspace_name.unwrap_or(workspace.session_name);
    let spec = rimz::mux::backend_for(mux).attach_command(&session);
    print_attach_command(&spec);
    Ok(())
}

fn print_attach_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{} {}", spec.program, spec.args.join(" "));
    }
}

/// Open a ledger from a resolved workspace; convenience for command bodies.
pub(crate) fn open_ledger(workspace: &rimz::ResolvedWorkspace) -> Result<Ledger> {
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    Ledger::open(paths, runtime).context("opening ledger")
}
