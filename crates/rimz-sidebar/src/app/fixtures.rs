//! Shared snapshot/pane builders for the app submodules' unit tests.

use jiff::Timestamp;
use rimz::feed::PaneRef;
use rimz::ids::PaneId;
use rimz::{MuxName, SidebarSnapshot, WorkspaceId};

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
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: None,
        is_focused: focused,
        command: Some("zsh".to_owned()),
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

pub(crate) fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: panes
            .into_iter()
            .map(|pane| rimz::SidebarRow {
                row_kind: rimz::SidebarRowKind::Process,
                id: pane.pane_id.to_string(),
                name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                status: None,
                phase: rimz::agents::TurnPhase::Idle,
                pane: Some(pane),
                request_id: None,
                surface: None,
                task: None,
                prompt: None,
                model: None,
                effort: None,
                context_pct: None,
                context_window: None,
                total_tokens: None,
                todo_done: None,
                todo_total: None,
                context: None,
                context_severity: None,
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                last_activity: Timestamp::now(),
                registered_at: None,
                resolver: None,
                options: Vec::new(),
                sub_agents: Vec::new(),
                process_active: false,
                command_detail: None,
                compacting: false,
                turn_error_label: None,
                rss_kb: None,
                cpu_pct: None,
                io_bps: None,
            })
            .collect(),
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];
    snapshot
}

pub(crate) fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    let row = rimz::SidebarRow {
        row_kind: rimz::SidebarRowKind::Agent,
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        status: Some(rimz::feed::AgentStatus::Idle),
        phase: rimz::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_9", "tab_0", false)),
        request_id: None,
        surface: None,
        task: Some("inspect auth".to_owned()),
        prompt: None,
        model: Some("Opus".to_owned()),
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        context_severity: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    };
    snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: vec![rimz::SidebarStatusCount {
            status: rimz::feed::AgentStatus::Idle,
            count: 1,
        }],
        rows: vec![row],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];
    snapshot
}
