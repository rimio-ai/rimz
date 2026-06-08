//! Shared snapshot/pane builders for the app submodules' unit tests.

use crate::feed::PaneRef;
use crate::ids::PaneId;
use crate::{MuxName, SidebarSnapshot, WorkspaceId};
use jiff::Timestamp;

use super::state::placeholder_snapshot;

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

pub(crate) fn snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    placeholder_snapshot(ws.clone())
}

pub(crate) fn pane(raw: &str, view: &str, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: None,
        is_focused: focused,
        command: Some("zsh".to_owned()),
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
    }
}

pub(crate) fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: panes
            .into_iter()
            .map(|pane| crate::SidebarRow {
                id: pane.pane_id.to_string(),
                name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                pane: Some(pane),
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                last_activity: Timestamp::now(),
                card: crate::RowCard::Process(crate::ProcessCard::default()),
            })
            .collect(),
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];
    snapshot
}

pub(crate) fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    let row = crate::SidebarRow {
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        pane: Some(pane("terminal_9", "tab_0", false)),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: Some(crate::feed::AgentStatus::Idle),
            phase: crate::agents::TurnPhase::Idle,
            task: Some("inspect auth".to_owned()),
            model: Some("Opus".to_owned()),
            ..crate::AgentCard::default()
        })),
    };
    snapshot.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: vec![crate::SidebarStatusCount {
            status: crate::feed::AgentStatus::Idle,
            count: 1,
        }],
        rows: vec![row],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];
    snapshot
}
