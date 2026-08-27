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
use jiff::Timestamp;

use rimz::agents::{self, LifecycleRefreshRequest};
use rimz::ids::{PaneId, WorkspaceId};
use rimz::store::workspace_record;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

pub(super) fn run(request: LifecycleRefreshRequest) -> Result<()> {
    let Some(definition) = agents::find_definition(request.kind.as_str()) else {
        return Ok(());
    };
    let workspace_id = request.workspace_id;
    let runtime = crate::cli::runtime_paths_for(workspace_id.clone())?;

    let kind = request.kind.as_str();
    let session_id = request.session_id.as_str();
    let prior = rimz::store::agent_context::read_one(&runtime, kind, session_id);
    let Some(refresh) = definition.refresh_session_context(&agents::SessionContextInput {
        session_id,
        model: request.model.as_deref(),
        server_url: request.server_url.as_deref(),
        prior: prior.as_ref(),
        pricing_cache_path: &runtime.shared_pricing_cache_path(),
        broker_socket: Some(&runtime.codex_app_server_socket_path()),
    }) else {
        return Ok(());
    };

    let mut wrote = false;
    if let Some(mut local) = refresh.local {
        confirm_turn_death_from_pane(&runtime, &workspace_id, kind, session_id, &mut local);
        rimz::store::agent_context::merge_local_context(
            &runtime,
            definition.spec(),
            session_id,
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
            kind,
            oauth_enabled,
            Some(realtime),
        );
    }

    if let Some(observed) = refresh.observed {
        let observed_at = observed.observed_at;
        rimz::store::agent_context::update_record(
            &runtime,
            kind,
            session_id,
            observed_at,
            |record, _| definition.merge_session_context(record, &observed),
        )
        .context("writing rich agent-context sidecar")?;
        wrote = true;
    }

    if wrote {
        let _ = rimz::sidebar::wakeup::wake_store_delta(&runtime, None, None);
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
        cwd_project_root: None,
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
