//! Out-of-band context refresh for one session, for any provider.
//!
//! The installed hook spawns `rimz agents refresh-context` detached with fresh
//! stdio on turn-boundary events; humans never run it. The adapter reads its own
//! provider source and returns write intent
//! ([`AgentDefinition::refresh_session_context`]); this handler owns every
//! durable write and the sidebar wakeup.
//!
//! Like `statusline feed`, this path is event-log-free and workspace-lock-free,
//! and strictly best-effort: any failure (agent missing, not logged in, embedded
//! server hiccup) exits 0 with local data preserved. It never blocks a hook —
//! the hook returns before this child does any work.

use anyhow::{Context, Result};
use clap::Args;
use jiff::Timestamp;

use rimz::agents;
use rimz::ids::{PaneId, WorkspaceId};
use rimz::store::workspace_record;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

#[derive(Debug, Args)]
pub(super) struct RefreshContextArgs {
    /// Agent kind whose adapter owns the refresh.
    #[arg(long)]
    kind: String,
    /// Session id the sidecar is filed under (the provider's own session id).
    #[arg(long)]
    session_id: String,
    /// Workspace the session belongs to; the runtime dir derives from it.
    #[arg(long)]
    workspace_id: String,
    /// The session's current model id, used to resolve a display name.
    #[arg(long)]
    model: Option<String>,
    /// Embedded provider server base URL, for adapters whose plugin reports one.
    #[arg(long)]
    server_url: Option<String>,
}

impl RefreshContextArgs {
    pub(super) fn kind(&self) -> &str {
        &self.kind
    }

    pub(super) fn session_id(&self) -> &str {
        &self.session_id
    }
}

pub(super) fn run(args: RefreshContextArgs) -> Result<()> {
    let Some(definition) = agents::find_definition(&args.kind) else {
        return Ok(());
    };
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let runtime = crate::cli::runtime_paths_for(workspace_id.clone())?;

    let prior = rimz::store::agent_context::read_one(&runtime, &args.kind, &args.session_id);
    let Some(refresh) = definition.refresh_session_context(&agents::SessionContextInput {
        session_id: &args.session_id,
        model: args.model.as_deref(),
        server_url: args.server_url.as_deref(),
        prior: prior.as_ref(),
        pricing_cache_path: &runtime.shared_pricing_cache_path(),
        broker_socket: Some(&runtime.codex_app_server_socket_path()),
    }) else {
        return Ok(());
    };

    let mut wrote = false;
    if let Some(mut local) = refresh.local {
        confirm_turn_death_from_pane(
            &runtime,
            &workspace_id,
            &args.kind,
            &args.session_id,
            &mut local,
        );
        rimz::store::agent_context::merge_local_context(
            &runtime,
            definition.spec(),
            &args.session_id,
            local,
            Timestamp::now(),
        )
        .context("writing local agent-context sidecar")?;
        wrote = true;
    }

    if let Some(realtime) = refresh.realtime_usage {
        let oauth_enabled = !agents::credits::oauth_usage_offline();
        wrote |= rimz::sidebar::refresh::complete_realtime_account_usage(
            &runtime,
            &args.kind,
            oauth_enabled,
            Some(realtime),
        );
    }

    if let Some(observed) = refresh.observed {
        let observed_at = observed.observed_at;
        rimz::store::agent_context::update_record(
            &runtime,
            &args.kind,
            &args.session_id,
            observed_at,
            |record, _| definition.merge_session_context(record, &observed),
        )
        .context("writing rich agent-context sidecar")?;
        wrote = true;
    }

    if wrote {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

/// A turn death an adapter can only confirm from the live pane needs the pane
/// read before the marker lands. The adapter declares whether its own error
/// classes need it; the pane lookup is provider-neutral.
fn confirm_turn_death_from_pane(
    runtime: &RuntimePaths,
    workspace_id: &WorkspaceId,
    kind: &str,
    session_id: &str,
    refresh: &mut agents::LocalContextRefresh,
) {
    let agents::FieldPatch::Set(error) = &mut refresh.context.turn_error else {
        return;
    };
    if !agents::session::turn_death_needs_pane_confirmation(kind, error) {
        return;
    }
    let pane = session_pane(runtime, workspace_id, kind, session_id);
    rimz::sidebar::refresh::sessions::confirm_codex_turn_death_from_pane(
        runtime,
        pane.as_ref(),
        error,
    );
}

fn session_pane(
    runtime: &RuntimePaths,
    workspace_id: &WorkspaceId,
    kind: &str,
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
            pane.kind.as_str() == kind
                && pane
                    .agent_id
                    .as_ref()
                    .is_some_and(|agent_id| agent_id.as_str() == session_id)
        })
        .map(|pane| pane.pane_id)
}

#[cfg(test)]
mod tests {
    /// Fold one rich observation through OpenCode's own merge policy, the way
    /// `run` does after the adapter returns it.
    fn merge_observed(
        runtime: &RuntimePaths,
        session_id: &str,
        observed: rimz::agents::AgentContext,
    ) -> bool {
        let definition = rimz::agents::find_definition("opencode").expect("opencode registered");
        let observed_at = observed.observed_at;
        rimz::store::agent_context::update_record(
            runtime,
            "opencode",
            session_id,
            observed_at,
            |record, _| definition.merge_session_context(record, &observed),
        )
        .expect("writing opencode rich context")
    }

    use super::*;
    use rimz::agents::{
        AgentCost, AgentTokenUsage, FieldPatch, LocalContextPatch, LocalContextRefresh,
        LocalSpendFold, LocalTokenPatch, TranscriptStat,
    };
    use rimz::ids::WorkspaceId;

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
        assert!(merge_observed(&runtime, "sess-1", initial));

        let local_at = Timestamp::from_second(1_700_000_100).unwrap();
        rimz::store::agent_context::merge_local_context(
            &runtime,
            rimz::agents::spec_by_kind("opencode").unwrap(),
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
                    ..LocalSpendFold::default()
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
        assert!(merge_observed(&runtime, "sess-1", observed));
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
