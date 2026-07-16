//! Codex realtime-details refresh. The hook ingestion path first merges the
//! local rollout-derived tokens/cost inline when the stat gate says they changed.
//! The installed Codex hook also spawns `rimz codex refresh-context` detached
//! (fresh stdio) on turn-boundary events; that helper repeats the cheap rollout
//! merge, then reads the app-server's read-only enrichment (rate-limit windows,
//! model display name, version) when app-server-owned fields are due.
//!
//! Like `statusline feed`, this path is event-log-free and workspace-lock-free,
//! and strictly best-effort: any failure (codex missing, not logged in,
//! app-server hiccup) exits 0 with local transcript data preserved. It never
//! blocks a hook — the hook returns before this child does any work.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::agents;
use rimz::agents::codex;
use rimz::ids::{PaneId, WorkspaceId};
use rimz::store::workspace_record;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct CodexArgs {
    #[command(subcommand)]
    command: CodexSubcmd,
}

#[derive(Debug, Subcommand)]
enum CodexSubcmd {
    /// Refresh the Codex session's context sidecar from the local rollout and
    /// due app-server fields. The installed hook spawns this detached; humans do
    /// not run it.
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

impl CodexArgs {
    /// The low-cardinality command label and, for a session-scoped helper, its
    /// session id — for the Sentry command scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>) {
        match &self.command {
            CodexSubcmd::RefreshContext { session_id, .. } => {
                ("codex refresh-context", Some(session_id.as_str()))
            }
            CodexSubcmd::AppServer(_) => ("codex app-server", None),
        }
    }
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
    rimz::agents::codex::broker::serve(rimz::agents::codex::broker::BrokerInfo {
        session: session_name,
        socket_path: &socket,
    })
    .context("running codex app-server broker")
}

fn refresh_context(session_id: &str, workspace_id: &str, model: Option<&str>) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let prior = rimz::store::agent_context::read_one(&runtime, "codex", session_id);
    let transcript_model_hint = model.or_else(|| {
        prior
            .as_ref()
            .and_then(|record| record.context.model_id.as_deref())
    });
    let mut wrote = false;
    let mut transcript_refresh = codex::refresh_transcript_context(
        session_id,
        transcript_model_hint,
        prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
        &runtime.shared_pricing_cache_path(),
    );
    if let Some(refresh) = transcript_refresh.as_mut() {
        confirm_codex_turn_death(&runtime, &workspace_id, session_id, refresh);
    }
    if let Some(refresh) = transcript_refresh {
        rimz::store::agent_context::merge_local_context(
            &runtime,
            "codex",
            session_id,
            refresh,
            Timestamp::now(),
        )
        .context("writing transcript agent-context sidecar")?;
        wrote = true;
    }

    let prior = rimz::store::agent_context::read_one(&runtime, "codex", session_id);
    if !codex::app_server_due(prior.as_ref(), codex::REFRESH_THROTTLE_SECS) {
        if wrote {
            let _ = rimz::store::wakeup::wake_sidebars(&runtime);
        }
        return Ok(());
    }

    // Prefer this session's warm broker socket; the app-server read falls back to
    // the per-user daemon then a cold-spawn when it isn't up.
    let broker_socket = runtime.codex_app_server_socket_path();
    let oauth_enabled = !agents::credits::oauth_usage_offline();
    let enrichment =
        codex::refresh_app_server_enrichment(Some(session_id), model, Some(&broker_socket));
    let realtime = enrichment
        .as_ref()
        .map(|enrichment| rimz::AccountUsageSnapshot {
            plan: enrichment
                .context
                .account
                .as_ref()
                .and_then(|account| account.plan.clone()),
            rate_limits: enrichment.context.rate_limits.clone(),
            extra_credits: enrichment.extra_credits.clone(),
            reset_credits: enrichment.reset_credits.clone(),
        });
    wrote |= rimz::sidebar::refresh::complete_realtime_account_usage(
        &runtime,
        "codex",
        oauth_enabled,
        realtime,
    );
    if let Some(enrichment) = enrichment {
        codex::merge_app_server_context(&runtime, session_id, enrichment.context)
            .context("writing app-server agent-context sidecar")?;
        wrote = true;
    }
    if wrote {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

fn confirm_codex_turn_death(
    runtime: &RuntimePaths,
    workspace_id: &WorkspaceId,
    session_id: &str,
    refresh: &mut agents::LocalContextRefresh,
) {
    let Some(error) = refresh.turn_error.as_mut() else {
        return;
    };
    if !codex::turn_death_needs_pane_confirmation(error) {
        return;
    }
    let pane = codex_session_pane(runtime, workspace_id, session_id);
    rimz::sidebar::refresh::sessions::confirm_codex_turn_death_from_pane(
        runtime,
        pane.as_ref(),
        error,
    );
}

fn codex_session_pane(
    runtime: &RuntimePaths,
    workspace_id: &WorkspaceId,
    session_id: &str,
) -> Option<PaneId> {
    let paths = StatePaths::for_workspace(workspace_id.clone()).ok()?;
    let record = workspace_record::read(&paths.workspace_record).ok()?;
    let store = Store::open(paths, runtime.clone()).ok()?;
    let workspace = ResolvedWorkspace {
        workspace_id: workspace_id.clone(),
        project_root: record.project_root.clone(),
        root_class: record.root_class,
        worktree_root: record.project_root,
        worktree_branch: None,
        session_name: record.session_name,
        mux_hint: None,
    };
    let snapshot = rimz::sidebar::produce::resolution_snapshot(&workspace, &store, None).ok()?;
    snapshot
        .agent_panes
        .into_iter()
        .find(|pane| {
            pane.kind.as_str() == "codex"
                && pane
                    .agent_id
                    .as_ref()
                    .is_some_and(|agent_id| agent_id.as_str() == session_id)
        })
        .map(|pane| pane.pane_id)
}
