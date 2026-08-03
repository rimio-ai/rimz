//! Shared snapshot/pane builders for the app submodules' unit tests.

use crate::ids::PaneId;
use crate::pane::PaneRef;
use crate::sidebar_pane::app::ServeConfig;
use crate::{MuxName, SidebarInstanceId, WorkspaceId, store::snapshot::SidebarSnapshot};
use jiff::Timestamp;

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

pub(crate) fn serve_config(ws: &WorkspaceId) -> ServeConfig {
    ServeConfig {
        workspace_id: ws.clone(),
        mux: crate::MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 1,
        refresh_ms_override: None,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: crate::config::NotificationsPrefs::default(),
        nav_keys: crate::sidebar_pane::app::NavKeymap::from_config(
            &crate::config::SidebarKeys::default(),
        ),
        own_pane: None,
    }
}

pub(crate) fn snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(ws.clone(), Vec::new(), Timestamp::UNIX_EPOCH)
}

pub(crate) fn pane(raw: &str, view: &str, _focused: bool) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: None,
        title: None,
        is_floating: false,
        command: Some("zsh".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

pub(crate) fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.pane_session_name = Some("rimz-test".to_owned());
    snapshot.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        status_counts: Vec::new(),
        rows: panes
            .into_iter()
            .map(|pane| crate::store::snapshot::SidebarRow {
                id: pane.pane_id.to_string(),
                name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                pane: Some(pane),
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                channel: None,
                unread: false,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: Timestamp::now(),
                card: crate::store::snapshot::RowCard::Process(
                    crate::store::snapshot::ProcessCard::default(),
                ),
            })
            .collect(),
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
        pr_url: None,
    }];
    snapshot
}

pub(crate) fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    let row = crate::store::snapshot::SidebarRow {
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        pane: Some(pane("terminal_9", "tab_0", false)),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: Timestamp::now(),
        card: crate::store::snapshot::RowCard::Agent(Box::new(crate::store::snapshot::AgentCard {
            status: crate::agents::AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            task: Some("inspect auth".to_owned()),
            model: Some("Opus".to_owned()),
            ..crate::store::snapshot::AgentCard::default()
        })),
    };
    snapshot.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        status_counts: vec![crate::store::snapshot::SidebarStatusCount {
            status: crate::agents::AgentStatus::Idle,
            count: 1,
        }],
        rows: vec![row],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
        pr_url: None,
    }];
    snapshot
}
