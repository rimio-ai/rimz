//! Codex realtime-details refresh. The installed Codex hook spawns
//! `rimz codex refresh-context` detached (fresh stdio) on turn-boundary events;
//! it reads the app-server's read-only enrichment (rate-limit windows, model
//! display name + effort, version) and writes the per-session `AgentContext`
//! sidecar.
//!
//! Like `statusline feed`, this path is ledger-free and lock-free, and strictly
//! best-effort: any failure (codex missing, not logged in, app-server hiccup)
//! exits 0 with nothing written. It never blocks a hook — the hook returns
//! before this child does any work.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::RuntimePaths;
use rimz::agents::codex;
use rimz::ids::WorkspaceId;

use super::GlobalFlags;

/// Skip a refresh when this session's sidecar was written within this window,
/// so two close turn boundaries (a quick `UserPromptSubmit` then `Stop`) don't
/// each spawn an app-server. Rate-limit windows move on the scale of minutes, so
/// this loses no meaningful freshness.
const REFRESH_THROTTLE_SECS: i64 = 20;

#[derive(Debug, Args)]
pub struct CodexArgs {
    #[command(subcommand)]
    command: CodexSubcmd,
}

#[derive(Debug, Subcommand)]
enum CodexSubcmd {
    /// Refresh the Codex session's context sidecar from the app-server. The
    /// installed hook spawns this detached; humans do not run it.
    #[command(hide = true)]
    RefreshContext {
        /// Session id the sidecar is filed under (the Codex `session_id`).
        #[arg(long)]
        session_id: String,
        /// Workspace the session belongs to; the runtime dir derives from it.
        #[arg(long)]
        workspace_id: String,
        /// The session's current model id, used to resolve a display name.
        #[arg(long)]
        model: Option<String>,
    },
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

pub fn run(args: CodexArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        CodexSubcmd::RefreshContext {
            session_id,
            workspace_id,
            model,
        } => refresh_context(&session_id, &workspace_id, model.as_deref()),
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
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    let socket = runtime.codex_app_server_socket_path();
    rimz::agents::codex_broker::serve(rimz::agents::codex_broker::BrokerInfo {
        session: session_name,
        socket_path: &socket,
    })
    .context("running codex app-server broker")
}

fn refresh_context(session_id: &str, workspace_id: &str, model: Option<&str>) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    if recent_sidecar(&runtime, session_id, REFRESH_THROTTLE_SECS) {
        return Ok(());
    }

    // Prefer this session's warm broker socket; `refresh_context` falls back to
    // the per-user daemon then a cold-spawn when it isn't up.
    let broker_socket = runtime.codex_app_server_socket_path();
    let Some(context) = codex::refresh_context(model, Some(&broker_socket)) else {
        // App-server unreachable / nothing to record. Best-effort: succeed.
        return Ok(());
    };
    rimz::ledger::agent_context::write(&runtime, "codex", session_id, &context)
        .context("writing agent-context sidecar")?;
    Ok(())
}

/// Whether this session already has a sidecar refreshed within `within` seconds.
fn recent_sidecar(runtime: &RuntimePaths, session_id: &str, within: i64) -> bool {
    let now = Timestamp::now().as_second();
    rimz::ledger::agent_context::read_all(runtime)
        .iter()
        .any(|record| {
            record.kind == "codex"
                && record.agent_id == session_id
                && now - record.context.observed_at.as_second() < within
        })
}
