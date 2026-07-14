//! OpenCode rich-context refresh. The installed OpenCode plugin includes its
//! embedded server URL on lifecycle envelopes; turn-boundary hooks spawn
//! `rimz opencode refresh-context` detached with fresh stdio. The helper reads
//! only read-only HTTP routes and merges display-only metadata into the
//! agent-context sidecar. Failures exit 0 with plugin-owned tokens and cost
//! preserved.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::Timestamp;

use rimz::RuntimePaths;
use rimz::agents::AgentContext;
use rimz::ids::WorkspaceId;

use super::GlobalFlags;

const REFRESH_THROTTLE_SECS: i64 = 20;

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
    if !rich_context_due(prior.as_ref(), REFRESH_THROTTLE_SECS) {
        return Ok(());
    }
    let model_hint = model.or_else(|| {
        prior
            .as_ref()
            .and_then(|record| record.context.model_id.as_deref())
    });
    let context = rimz::agents::opencode::server::observe(
        server_url,
        Some(session_id),
        model_hint,
        Timestamp::now(),
    );
    if merge_opencode_context(&runtime, session_id, context)
        .context("writing OpenCode rich-context sidecar")?
    {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

fn rich_context_due(
    record: Option<&rimz::store::agent_context::AgentContextRecord>,
    within: i64,
) -> bool {
    let now = Timestamp::now().as_second();
    record
        .and_then(|record| record.rich_observed_at)
        .is_none_or(|observed_at| now - observed_at.as_second() >= within)
}

fn merge_opencode_context(
    runtime: &RuntimePaths,
    session_id: &str,
    context: AgentContext,
) -> Result<bool> {
    if !has_rich_fields(&context) {
        return Ok(false);
    }
    let observed_at = context.observed_at;
    let prior = rimz::store::agent_context::read_one(runtime, "opencode", session_id);
    let mut record = prior.unwrap_or_else(|| {
        rimz::store::agent_context::new_record("opencode", session_id, {
            rimz::store::agent_context::empty_context("opencode", observed_at)
        })
    });

    record.context.source = context.source;
    if context.session_name.is_some() {
        record.context.session_name = context.session_name;
    }
    record.context.model_display_name = context.model_display_name;
    record.context.agent_version = context.agent_version;
    record.context.observed_at = observed_at;
    record.rich_observed_at = Some(observed_at);
    rimz::store::agent_context::write_record(runtime, &record)
        .context("writing merged OpenCode context")?;
    Ok(true)
}

fn has_rich_fields(context: &AgentContext) -> bool {
    context.session_name.is_some()
        || context.model_display_name.is_some()
        || context.agent_version.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::{
        AgentCost, AgentCurrentUsage, AgentTokenUsage, LocalContextRefresh, TranscriptStat,
    };

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    #[test]
    fn rich_context_due_uses_rich_stamp_not_whole_sidecar() {
        let now = Timestamp::now();
        let mut record = rimz::store::agent_context::new_record(
            "opencode",
            "sess-1",
            rimz::store::agent_context::empty_context("opencode", now),
        );
        assert!(rich_context_due(None, REFRESH_THROTTLE_SECS));
        assert!(
            rich_context_due(Some(&record), REFRESH_THROTTLE_SECS),
            "a fresh local-only sidecar has no rich-context stamp and is due"
        );

        record.rich_observed_at = Some(now);
        assert!(!rich_context_due(Some(&record), REFRESH_THROTTLE_SECS));

        record.rich_observed_at =
            Some(Timestamp::from_second(now.as_second() - REFRESH_THROTTLE_SECS - 1).unwrap());
        assert!(rich_context_due(Some(&record), REFRESH_THROTTLE_SECS));
    }

    #[test]
    fn opencode_merge_preserves_push_owned_fields() {
        let (_dir, runtime) = runtime();
        seed_push_context(&runtime);
        let rich_at = Timestamp::from_second(1_700_000_050).unwrap();
        assert!(
            merge_opencode_context(&runtime, "sess-1", rich_context(rich_at)).unwrap(),
            "rich fields write the sidecar"
        );
        let merged = rimz::store::agent_context::read_one(&runtime, "opencode", "sess-1").unwrap();

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
        assert_eq!(merged.context.model_id.as_deref(), Some("gpt-5"));
        assert_eq!(merged.context.effort.as_deref(), Some("xhigh"));
        assert_eq!(merged.context.model_display_name.as_deref(), Some("GPT-5"));
        assert_eq!(merged.context.agent_version.as_deref(), Some("1.17.9"));
        assert_eq!(merged.context.session_name.as_deref(), Some("Fix auth"));
        assert!(merged.context.session_preview.is_none());
        assert_eq!(merged.rich_observed_at, Some(rich_at));
    }

    #[test]
    fn empty_observation_skips_write_and_throttle_stamp() {
        let (_dir, runtime) = runtime();
        let observed_at = Timestamp::from_second(1_700_000_050).unwrap();
        assert!(
            !merge_opencode_context(
                &runtime,
                "sess-1",
                rimz::store::agent_context::empty_context("opencode", observed_at)
            )
            .unwrap()
        );
        assert!(rimz::store::agent_context::read_one(&runtime, "opencode", "sess-1").is_none());
    }

    fn seed_push_context(runtime: &RuntimePaths) {
        let push_at = Timestamp::from_second(1_700_000_000).unwrap();
        rimz::store::agent_context::merge_local_context(
            runtime,
            "opencode",
            "sess-1",
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
                turn_error: None,
                turn_complete: None,
                plan_proposed: None,
                turn_interrupted: None,
                transcript_path: Some("/tmp/opencode.db".to_owned()),
                transcript_stat: Some(TranscriptStat {
                    mtime_secs: 10,
                    mtime_nanos: 20,
                    len: 30,
                }),
            },
            push_at,
        )
        .unwrap();
    }

    fn rich_context(observed_at: Timestamp) -> AgentContext {
        AgentContext {
            source: "opencode".to_owned(),
            session_name: Some("Fix auth".to_owned()),
            session_preview: None,
            model_id: None,
            model_display_name: Some("GPT-5".to_owned()),
            effort: None,
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: Some("1.17.9".to_owned()),
            exceeds_200k_tokens: None,
            cost: None,
            tokens: None,
            rate_limits: None,
            pr: None,
            account: None,
            turn_opened_by: Vec::new(),
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            native_permission_wait: None,
            turn_interrupted: None,
            observed_at,
        }
    }
}
