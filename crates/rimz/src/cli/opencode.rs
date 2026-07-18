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
    let Some(observed) = rimz::agents::opencode::server::refresh_rich_context(
        server_url,
        session_id,
        model,
        prior.as_ref().map(|record| &record.context),
        prior.as_ref().and_then(|record| record.rich_observed_at),
        Timestamp::now(),
    ) else {
        return Ok(());
    };
    let wrote = merge_observation(&runtime, session_id, observed)?;
    if wrote {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

fn merge_observation(
    runtime: &RuntimePaths,
    session_id: &str,
    observed: rimz::agents::AgentContext,
) -> Result<bool> {
    let observed_at = observed.observed_at;
    rimz::store::agent_context::update_record(
        runtime,
        "opencode",
        session_id,
        observed_at,
        |record, _| {
            if !rimz::agents::opencode::server::merge_rich_context(&mut record.context, &observed) {
                return false;
            }
            record.rich_observed_at = Some(observed_at);
            true
        },
    )
    .context("writing OpenCode rich-context sidecar")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::{
        AgentCost, AgentTokenUsage, FieldPatch, LocalContextPatch, LocalContextRefresh,
        LocalSpendFold, LocalTokenPatch, TranscriptStat,
    };

    #[test]
    fn rich_observation_merges_into_latest_local_record() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let rich_at = Timestamp::from_second(1_700_000_050).unwrap();
        let mut initial = rimz::agents::AgentContext::new("opencode", rich_at);
        initial.session_name = Some("Existing name".to_owned());
        initial.model_display_name = Some("Old model".to_owned());
        initial.agent_version = Some("1.0".to_owned());
        assert!(merge_observation(&runtime, "sess-1", initial).unwrap());

        let local_at = Timestamp::from_second(1_700_000_100).unwrap();
        rimz::store::agent_context::merge_local_context(
            &runtime,
            rimz::agents::descriptor_by_kind("opencode").unwrap(),
            "sess-1",
            LocalContextRefresh {
                context: LocalContextPatch {
                    model_id: FieldPatch::Set("openai/gpt-5".to_owned()),
                    effort: FieldPatch::Set("high".to_owned()),
                    tokens: LocalTokenPatch::ReplaceCurrentPreservingSession(Some(
                        AgentTokenUsage {
                            used_percentage: Some(25),
                            ..Default::default()
                        },
                    )),
                    cost: FieldPatch::Set(AgentCost {
                        total_cost_usd: Some(0.42),
                        ..Default::default()
                    }),
                    ..LocalContextPatch::default()
                },
                transcript_path: Some("/tmp/opencode.db".to_owned()),
                transcript_stat: Some(TranscriptStat {
                    mtime_secs: 10,
                    mtime_nanos: 20,
                    len: 30,
                    companion: None,
                }),
                spend_fold: FieldPatch::Set(LocalSpendFold {
                    cursor: rimz::agents::spending::SpendCursor {
                        offset: 42,
                        state: None,
                    },
                    total_usd: 0.42,
                }),
            },
            local_at,
        )
        .unwrap();
        let opener = rimz::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
        rimz::store::agent_context::merge_turn_opened_by(
            &runtime,
            "opencode",
            "sess-1",
            vec![opener.clone()],
        )
        .unwrap();

        let mut observed = rimz::agents::AgentContext::new("opencode", rich_at);
        observed.agent_version = Some("2.0".to_owned());
        assert!(merge_observation(&runtime, "sess-1", observed).unwrap());
        let merged = rimz::store::agent_context::read_one(&runtime, "opencode", "sess-1").unwrap();
        assert_eq!(
            merged.context.session_name.as_deref(),
            Some("Existing name")
        );
        assert_eq!(merged.context.model_display_name, None);
        assert_eq!(merged.context.agent_version.as_deref(), Some("2.0"));
        assert_eq!(merged.context.model_id.as_deref(), Some("openai/gpt-5"));
        assert_eq!(merged.context.effort.as_deref(), Some("high"));
        assert_eq!(
            merged
                .context
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.used_percentage),
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
        assert_eq!(merged.context.turn_opened_by, vec![opener]);
        assert_eq!(merged.transcript_path.as_deref(), Some("/tmp/opencode.db"));
        assert_eq!(merged.transcript_stat.unwrap().len, 30);
        assert_eq!(merged.spend_fold.unwrap().total_usd, 0.42);
        assert_eq!(merged.rate_limits_observed_at, None);
        assert_eq!(merged.rich_observed_at, Some(rich_at));
        assert_eq!(merged.context.observed_at, rich_at);
    }
}
