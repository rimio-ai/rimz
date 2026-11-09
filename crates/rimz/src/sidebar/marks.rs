//! Shared target resolution and runtime read/unread marks for operator commands.

use crate::address::AddressContext;
use crate::diag::DiagSink;
use crate::disk::{atomic, paths::RuntimePaths};
use crate::ids::{AgentKind, AgentSessionId, MuxName};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};
use crate::{ResolvedWorkspace, Store};

#[derive(Debug, thiserror::Error)]
pub enum MarkErr {
    #[error(transparent)]
    Target(#[from] crate::address::TargetErr),
    #[error(transparent)]
    Produce(#[from] super::produce::ProduceErr),
    #[error("target {0} has no rendered sidebar row")]
    MissingRow(String),
}

pub fn resolve(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &str,
    worktree: Option<&str>,
    channel: &AddressContext,
    mux: Option<MuxName>,
) -> Result<Vec<SidebarRow>, MarkErr> {
    let scoped;
    let target = if target.starts_with('#') {
        scoped = format!("@all{target}");
        &scoped
    } else {
        target
    };
    crate::address::require_mention(target)?;
    if let Ok(snapshot) = super::consumer::PublishedSnapshotReader::new(
        store.runtime_paths().clone(),
        workspace.session_name.clone(),
        None,
    )
    .read(store.paths())
        && let Ok(rows) = resolve_rows(&snapshot, target, worktree, channel)
        && !rows.is_empty()
    {
        return Ok(rows);
    }
    let snapshot = super::produce::resolution_snapshot(workspace, store, mux)?;
    resolve_rows(&snapshot, target, worktree, channel)
}

fn resolve_rows(
    snapshot: &SidebarSnapshot,
    target: &str,
    worktree: Option<&str>,
    channel: &AddressContext,
) -> Result<Vec<SidebarRow>, MarkErr> {
    crate::address::resolve_targets(snapshot, target, worktree, channel)?
        .into_iter()
        .map(|target| {
            snapshot
                .rows()
                .find(|row| {
                    row.pane
                        .as_ref()
                        .is_some_and(|pane| pane.pane_id == target.pane_id)
                })
                .cloned()
                .ok_or_else(|| MarkErr::MissingRow(target.label()))
        })
        .collect()
}

pub fn read(
    runtime: &RuntimePaths,
    rows: &[SidebarRow],
    diag: &DiagSink,
    at_ms: i64,
) -> atomic::Result<()> {
    super::read_marks::write_read_marks(runtime, rows.iter().map(|row| row.id.clone()), at_ms)?;
    let cleared = super::unread::update_episodes(runtime, |episodes| {
        let cleared = rows
            .iter()
            .filter(|row| episodes.remove_reached_for_row(row, at_ms))
            .collect::<Vec<_>>();
        let write = !cleared.is_empty();
        (cleared, write)
    })?;
    for row in cleared {
        diag.trace_notify(crate::diag::notify::NotifyTraceEvent::UnreadCleared {
            row_id: row.id.clone(),
            label: Some(super::unread::row_label(row)),
            agent_kind: Some(AgentKind::new_unchecked(row.name.clone())),
            agent_id: Some(AgentSessionId::from(row.id.clone())),
            worktree: row
                .worktree_branch
                .clone()
                .or_else(|| row.lane_path().map(ToOwned::to_owned)),
            pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
            cause: super::unread::UnreadClearCause::MarkRead
                .as_str()
                .to_owned(),
            cleared_at_ms: Some(at_ms),
        });
    }
    wake(runtime);
    Ok(())
}

pub fn unread(
    runtime: &RuntimePaths,
    rows: &[SidebarRow],
    diag: &DiagSink,
    at_ms: i64,
) -> atomic::Result<()> {
    for item in super::unread::mark_rows_unread(runtime, rows, at_ms)? {
        diag.trace_notify(crate::diag::notify::NotifyTraceEvent::UnreadMarked {
            row_id: item.row_id,
            label: Some(item.label),
            agent_kind: Some(item.agent_kind),
            agent_id: Some(item.agent_id),
            worktree: item.worktree,
            pane_id: item.pane_id,
            status: item.status.as_str().to_owned(),
            episode_ms: item.episode_ms,
        });
    }
    wake(runtime);
    Ok(())
}

fn wake(runtime: &RuntimePaths) {
    if let Err(err) = crate::wakeup::wake_store_delta(runtime, None, None) {
        tracing::debug!(error = %err, "sidebar unread wake failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::store::snapshot::{AgentCard, RowCard};

    #[test]
    fn mark_read_waits_for_the_episode_transaction() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        let row = SidebarRow {
            checkout: None,
            id: "a".to_owned(),
            name: "claude".to_owned(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            channel: None,
            unread: true,
            inactive: false,
            archived: false,
            attention_score: 0,
            attention_at_ms: None,
            last_activity: jiff::Timestamp::from_millisecond(1_000).expect("timestamp"),
            card: RowCard::Agent(std::sync::Arc::new(AgentCard {
                status: AgentStatus::Success,
                ..AgentCard::default()
            })),
        };
        super::super::unread::mark_rows_unread(&runtime, std::slice::from_ref(&row), 1_000)
            .expect("episode");
        let lock = crate::disk::lock::WorkspaceLock::acquire_with_timeout(
            &runtime.unread_lock_path(),
            std::time::Duration::from_millis(250),
        )
        .expect("unread lock");

        let out = read(
            &runtime,
            std::slice::from_ref(&row),
            &DiagSink::disabled(),
            2_000,
        );

        drop(lock);
        assert!(
            out.is_err(),
            "mark read replaced unread.json under a held lock"
        );
        assert!(
            std::fs::read_to_string(runtime.unread_path())
                .expect("episodes")
                .contains("\"a\"")
        );
    }
}
