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

use rimz::RuntimePaths;
use rimz::agents::AgentContext;
use rimz::agents::codex;
use rimz::ids::WorkspaceId;

use super::GlobalFlags;

/// Skip an app-server refresh when this session's app-server-owned fields were
/// written within this window, so two close turn boundaries (a quick
/// `UserPromptSubmit` then `Stop`) don't each spawn an app-server. Transcript
/// context is stat-gated separately and always gets a chance to merge.
const REFRESH_THROTTLE_SECS: i64 = 20;

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
    /// Refresh the account's rate-limit windows into the shared cache,
    /// account-scoped (no session). The sidebar producer spawns this detached for
    /// a logged-in but idle provider so its budgets paint without a live session;
    /// humans do not run it.
    #[command(hide = true)]
    RefreshRateLimits {
        /// Workspace whose runtime cache the windows are written into.
        #[arg(long)]
        workspace_id: String,
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
        CodexSubcmd::RefreshRateLimits { workspace_id } => refresh_rate_limits(&workspace_id),
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

    let prior = rimz::ledger::agent_context::read_one(&runtime, "codex", session_id);
    let transcript_model_hint = model.or_else(|| {
        prior
            .as_ref()
            .and_then(|record| record.context.model_id.as_deref())
    });
    let mut wrote = false;
    let transcript_refresh = codex::refresh_transcript_context(
        session_id,
        transcript_model_hint,
        prior
            .as_ref()
            .and_then(|record| record.context.effort.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
    );
    if let Some(refresh) = transcript_refresh {
        rimz::ledger::agent_context::merge_local_context(
            &runtime,
            "codex",
            session_id,
            prior,
            refresh,
            Timestamp::now(),
        )
        .context("writing transcript agent-context sidecar")?;
        wrote = true;
    }

    let prior = rimz::ledger::agent_context::read_one(&runtime, "codex", session_id);
    if !app_server_due(prior.as_ref(), REFRESH_THROTTLE_SECS) {
        if wrote {
            let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
        }
        return Ok(());
    }

    // Prefer this session's warm broker socket; the app-server read falls back to
    // the per-user daemon then a cold-spawn when it isn't up.
    let broker_socket = runtime.codex_app_server_socket_path();
    let Some(context) =
        codex::refresh_app_server_context(Some(session_id), model, Some(&broker_socket))
    else {
        // App-server unreachable / nothing to record. Transcript context, if it
        // changed, was already written above.
        if wrote {
            let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
        }
        return Ok(());
    };
    merge_app_server_context(&runtime, session_id, context)
        .context("writing app-server agent-context sidecar")?;
    let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    Ok(())
}

/// Fetch the account's rate-limit windows from the app-server (account-scoped, no
/// session/thread) and merge them into the shared `rate_limits.json` cache, so a
/// logged-in but idle provider's budget bars paint from the next frame. Best-effort
/// like `refresh_context`: an unreachable app-server, a logged-out or API-key
/// account (no windows), or a write hiccup all succeed silently with nothing
/// merged.
fn refresh_rate_limits(workspace_id: &str) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let broker_socket = runtime.codex_app_server_socket_path();
    let Some(context) = codex::refresh_app_server_context(None, None, Some(&broker_socket)) else {
        return Ok(());
    };
    if let Some(rate_limits) = context.rate_limits {
        rimz::sidebar::enrich::merge_account_rate_limits(&runtime, "codex", rate_limits);
        let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

fn app_server_due(
    record: Option<&rimz::ledger::agent_context::AgentContextRecord>,
    within: i64,
) -> bool {
    let now = Timestamp::now().as_second();
    record
        .and_then(|record| record.rate_limits_observed_at)
        .is_none_or(|observed_at| now - observed_at.as_second() >= within)
}

fn merge_app_server_context(
    runtime: &RuntimePaths,
    session_id: &str,
    context: AgentContext,
) -> Result<()> {
    let observed_at = context.observed_at;
    let prior = rimz::ledger::agent_context::read_one(runtime, "codex", session_id);
    let mut record = prior.unwrap_or_else(|| {
        rimz::ledger::agent_context::new_record("codex", session_id, {
            rimz::ledger::agent_context::empty_context("codex", observed_at)
        })
    });

    record.context.source = context.source;
    if context.session_name.is_some() {
        record.context.session_name = context.session_name;
    }
    if context.session_preview.is_some() {
        record.context.session_preview = context.session_preview;
    }
    if context.model_id.is_some() {
        record.context.model_id = context.model_id;
    }
    record.context.model_display_name = context.model_display_name;
    record.context.agent_version = context.agent_version;
    record.context.rate_limits = context.rate_limits;
    record.context.account = context.account;
    record.context.observed_at = observed_at;
    record.rate_limits_observed_at = Some(observed_at);
    rimz::ledger::agent_context::write_record(runtime, &record)
        .context("writing merged app-server context")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::{
        AgentAccount, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage,
        LocalContextRefresh, RateLimitWindow, TranscriptStat,
    };

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    #[test]
    fn app_server_due_uses_app_server_stamp_not_whole_sidecar() {
        let now = Timestamp::now();
        let mut record = rimz::ledger::agent_context::new_record(
            "codex",
            "sess-1",
            rimz::ledger::agent_context::empty_context("codex", now),
        );
        assert!(app_server_due(None, REFRESH_THROTTLE_SECS));
        assert!(
            app_server_due(Some(&record), REFRESH_THROTTLE_SECS),
            "a fresh transcript-only sidecar has no app-server stamp and is due"
        );

        record.rate_limits_observed_at = Some(now);
        assert!(!app_server_due(Some(&record), REFRESH_THROTTLE_SECS));

        record.rate_limits_observed_at =
            Some(Timestamp::from_second(now.as_second() - REFRESH_THROTTLE_SECS - 1).unwrap());
        assert!(app_server_due(Some(&record), REFRESH_THROTTLE_SECS));
    }

    #[test]
    fn app_server_merge_preserves_transcript_owned_fields() {
        let (_dir, runtime) = runtime();
        let transcript_at = Timestamp::from_second(1_700_000_000).unwrap();
        rimz::ledger::agent_context::merge_local_context(
            &runtime,
            "codex",
            "sess-1",
            None,
            LocalContextRefresh {
                model_id: Some("gpt-5".to_owned()),
                effort: Some("xhigh".to_owned()),
                tokens: Some(AgentTokenUsage {
                    context_window_size: Some(1000),
                    used_percentage: Some(25),
                    remaining_percentage: Some(75),
                    current_usage: Some(AgentCurrentUsage {
                        input_tokens: Some(200),
                        output_tokens: Some(50),
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: Some(50),
                    }),
                }),
                cost: Some(AgentCost {
                    total_cost_usd: Some(0.42),
                    ..AgentCost::default()
                }),
                transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
                transcript_stat: Some(TranscriptStat {
                    mtime_secs: 10,
                    mtime_nanos: 20,
                    len: 30,
                }),
            },
            transcript_at,
        )
        .unwrap();

        let app_at = Timestamp::from_second(1_700_000_050).unwrap();
        merge_app_server_context(
            &runtime,
            "sess-1",
            AgentContext {
                source: "codex".to_owned(),
                session_name: Some("TUI prototype".to_owned()),
                session_preview: Some("Create a TUI".to_owned()),
                model_id: Some("gpt-5".to_owned()),
                model_display_name: Some("GPT-5".to_owned()),
                effort: Some("high".to_owned()),
                thinking_enabled: None,
                output_style: None,
                vim_mode: None,
                agent_version: Some("1.2.3".to_owned()),
                exceeds_200k_tokens: None,
                cost: None,
                tokens: None,
                rate_limits: Some(AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(55),
                        resets_at: None,
                        duration_mins: Some(300),
                    }],
                }),
                pr: None,
                account: Some(AgentAccount {
                    plan: Some("pro".to_owned()),
                    metered: Some(true),
                    version: None,
                    sub_provider: None,
                }),
                turn_error: None,
                observed_at: app_at,
            },
        )
        .unwrap();

        let merged = rimz::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
        assert_eq!(
            merged
                .context
                .tokens
                .as_ref()
                .and_then(|t| t.used_percentage),
            Some(25)
        );
        assert_eq!(
            merged
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.42)
        );
        assert_eq!(
            merged.transcript_path.as_deref(),
            Some("/tmp/rollout.jsonl")
        );
        assert_eq!(merged.context.model_display_name.as_deref(), Some("GPT-5"));
        assert_eq!(
            merged.context.session_preview.as_deref(),
            Some("Create a TUI")
        );
        assert_eq!(
            merged.context.session_name.as_deref(),
            Some("TUI prototype")
        );
        assert_eq!(merged.context.effort.as_deref(), Some("xhigh"));
        assert_eq!(
            merged
                .context
                .rate_limits
                .as_ref()
                .and_then(|limits| limits.windows.first())
                .and_then(|window| window.used_percentage),
            Some(55)
        );
        assert_eq!(merged.rate_limits_observed_at, Some(app_at));
    }
}
