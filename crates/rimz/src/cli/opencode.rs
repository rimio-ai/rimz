//! OpenCode rich-context persistence boundary. The installed OpenCode plugin
//! includes its embedded server URL on lifecycle envelopes; turn-boundary hooks
//! spawn `rimz opencode refresh-context` detached with fresh stdio. Provider code
//! owns observation and merge policy; this helper owns durable sidecar writes and
//! sidebar wakeups. Failures exit 0 with plugin-owned context preserved.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::RuntimePaths;
use rimz::ids::WorkspaceId;

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct OpencodeArgs {
    #[command(subcommand)]
    command: OpencodeSubcmd,
}

#[derive(Debug, Subcommand)]
enum OpencodeSubcmd {
    /// Refresh the OpenCode session's context sidecar from the embedded server.
    /// The installed plugin spawns this detached; humans do not run it.
    #[command(hide = true)]
    RefreshContext {
        /// Session id the sidecar is filed under (the OpenCode `sessionID`).
        #[arg(long)]
        session_id: String,
        /// Workspace the session belongs to; the runtime dir derives from it.
        #[arg(long)]
        workspace_id: String,
        /// Embedded OpenCode server base URL from `PluginInput.serverUrl`.
        #[arg(long)]
        server_url: String,
        /// The session's current model id, used to resolve a display name.
        #[arg(long)]
        model: Option<String>,
    },
}

impl OpencodeArgs {
    /// The low-cardinality command label and session id for the Sentry scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>) {
        match &self.command {
            OpencodeSubcmd::RefreshContext { session_id, .. } => {
                ("opencode refresh-context", Some(session_id.as_str()))
            }
        }
    }
}

pub fn run(args: OpencodeArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        OpencodeSubcmd::RefreshContext {
            session_id,
            workspace_id,
            server_url,
            model,
        } => refresh_context(&session_id, &workspace_id, &server_url, model.as_deref()),
    }
}

fn refresh_context(
    session_id: &str,
    workspace_id: &str,
    server_url: &str,
    model: Option<&str>,
) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let prior = rimz::store::agent_context::read_one(&runtime, "opencode", session_id);
    let Some(context) = rimz::agents::opencode::server::refresh_rich_context(
        server_url,
        session_id,
        model,
        prior.as_ref().map(|record| &record.context),
        prior.as_ref().and_then(|record| record.rich_observed_at),
        Timestamp::now(),
    ) else {
        return Ok(());
    };
    let observed_at = context.observed_at;
    let mut record = prior.unwrap_or_else(|| {
        rimz::store::agent_context::new_record("opencode", session_id, {
            rimz::store::agent_context::empty_context("opencode", observed_at)
        })
    });
    record.context = context;
    record.rich_observed_at = Some(observed_at);
    rimz::store::agent_context::write_record(&runtime, &record)
        .context("writing OpenCode rich-context sidecar")?;
    let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    Ok(())
}
