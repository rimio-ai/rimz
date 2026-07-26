//! Codex app-server broker pane.
//!
//! `rimz start` runs this as a pane in the `rimzd` daemon tab: it holds one warm
//! `codex app-server` and serves it on the workspace's broker socket, so the
//! out-of-band context refresh (`rimz agents refresh-context`) pays a socket
//! round trip instead of a cold spawn. Long-lived; humans do not run it.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use rimz::agents::runtime_control;
use rimz::ids::WorkspaceId;

use super::{GlobalFlags, runtime_paths_for};

#[derive(Debug, Args)]
pub struct CodexArgs {
    #[command(subcommand)]
    command: CodexSubcmd,
}

#[derive(Debug, Subcommand)]
enum CodexSubcmd {
    /// Manage the per-session Codex app-server broker. `rimz start` runs this as
    /// a pane in the `rimzd` daemon tab; humans do not run it.
    #[command(hide = true)]
    AppServer(AppServerArgs),
}

#[derive(Debug, Args)]
struct AppServerArgs {
    #[command(subcommand)]
    command: AppServerSubcmd,
}

#[derive(Debug, Subcommand)]
enum AppServerSubcmd {
    /// Hold a warm `codex app-server` and serve it on this session's broker
    /// socket. Long-lived: runs until the pane closes.
    Serve {
        /// Workspace the broker serves; the socket path derives from it.
        #[arg(long)]
        workspace_id: String,
        /// Session name, shown in the broker pane's status banner.
        #[arg(long)]
        session_name: Option<String>,
    },
}

impl CodexArgs {
    /// The low-cardinality command label for the Sentry command scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>) {
        match &self.command {
            CodexSubcmd::AppServer(_) => ("codex app-server", None),
        }
    }
}

pub fn run(args: CodexArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        CodexSubcmd::AppServer(args) => match args.command {
            AppServerSubcmd::Serve {
                workspace_id,
                session_name,
            } => serve_app_server(&workspace_id, session_name.as_deref()),
        },
    }
}

/// Run the per-session Codex app-server broker, bound to this workspace's socket.
fn serve_app_server(workspace_id: &str, session_name: Option<&str>) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime = runtime_paths_for(workspace_id)?;
    let socket = runtime.codex_app_server_socket_path();
    runtime_control::serve_broker(session_name, &socket).context("running codex app-server broker")
}
