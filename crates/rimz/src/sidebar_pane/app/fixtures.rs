//! Shared snapshot/pane builders for the app submodules' unit tests.

use crate::ids::PaneId;
use crate::pane::PaneRef;
use crate::sidebar_pane::app::{RoomConfig, ServeConfig};
use crate::store::snapshot::SidebarSnapshot;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};
use jiff::Timestamp;
use std::sync::Arc;

pub(super) fn fold(
    snapshot: SidebarSnapshot,
    own: Option<&PaneId>,
    sidebars: Vec<(PaneId, String)>,
) -> super::fetch::WorkspaceFold {
    let frame = snapshot.own_view.as_ref().map(|view| {
        let mut panes: Vec<_> = snapshot.rows().filter_map(|row| row.pane.clone()).collect();
        let tab = panes
            .iter()
            .find(|pane| view.working_pane_ids.contains(&pane.pane_id))
            .and_then(|pane| pane.view_id.clone())
            .unwrap_or_else(|| "tab_sidebar".into());
        // The own view names the sidebar's tab; fixture rows default every pane onto
        // one tab, so move the rest off it.
        for other in panes
            .iter_mut()
            .filter(|pane| !view.working_pane_ids.contains(&pane.pane_id))
            .filter(|pane| pane.view_id.as_deref() == Some(tab.as_str()))
        {
            other.view_id = Some(format!("{tab}_other"));
        }
        for working in panes
            .iter_mut()
            .filter(|pane| view.working_pane_ids.contains(&pane.pane_id))
        {
            working.view_id = Some(tab.clone());
        }
        for id in &view.working_pane_ids {
            if !panes.iter().any(|pane| pane.pane_id == *id) {
                let mut working = pane(id.raw(), &tab, false);
                working.pane_id = id.clone();
                panes.push(working);
            }
        }
        for (id, tab) in own
            .cloned()
            .map(|own| (own, tab.clone()))
            .into_iter()
            .chain(sidebars)
        {
            if !panes.iter().any(|pane| pane.pane_id == id) {
                let mut sidebar = pane(id.raw(), &tab, false);
                sidebar.pane_id = id;
                sidebar.command = Some("rimz-sidebar".into());
                panes.push(sidebar);
            }
        }
        let siblings = panes
            .iter()
            .filter(|pane| pane.view_id.as_ref() == Some(&tab) && Some(&pane.pane_id) != own)
            .count();
        for index in siblings..view.sibling_count {
            let mut chrome = pane(&format!("fixture_chrome_{index}"), &tab, false);
            if let Some(own) = own {
                chrome.pane_id = PaneId::from_parts(own.mux(), chrome.pane_id.raw());
            }
            chrome.command = Some("rimz-sidebar".into());
            panes.push(chrome);
        }
        let mut frame = crate::sidebar::frame::assemble_frame(
            panes,
            snapshot.panes_observed_at_ms.unwrap_or(1),
            "rimz-test",
        );
        frame.client_views = snapshot.client_views.clone();
        frame.presence = snapshot.presence.map(|presence| {
            use crate::store::snapshot::{PresenceSample, SidebarPresence};
            PresenceSample {
                human_clients: usize::from(presence != SidebarPresence::Detached),
                last_input_ms: match presence {
                    SidebarPresence::Idle { last_input_ms } => Some(last_input_ms),
                    _ => None,
                },
                sampled_at_ms: snapshot.now.as_millisecond().max(0) as u64,
            }
        });
        Arc::new(frame)
    });
    super::fetch::WorkspaceFold {
        sampled_at: std::time::Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
        frame,
    }
}

pub(super) fn focus_fixture() -> (SidebarSnapshot, PaneId, PaneId, PaneId) {
    let ws = workspace();
    let sidebar = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let first_work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    let second_work = PaneId::from_parts(MuxName::Zellij, "terminal_12");
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_11", "tab_1", false),
            pane("terminal_12", "tab_1", false),
        ],
    );
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 3,
        working_pane_ids: vec![first_work.clone(), second_work.clone()],
        own_view_is_daemon: false,
    });
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    snapshot.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: sidebar.clone(),
    }];
    (snapshot, sidebar, first_work, second_work)
}

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

pub(in crate::sidebar_pane) fn remember_fold(
    plane: &super::DataPlane,
    snapshot: SidebarSnapshot,
    frame: crate::sidebar::frame::PaneFrame,
) {
    plane.remember(super::fetch::WorkspaceFold {
        sampled_at: std::time::Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
        frame: Some(std::sync::Arc::new(frame)),
    });
}

pub(in crate::sidebar_pane) fn publish_fold(
    plane: &super::DataPlane,
    snapshot: SidebarSnapshot,
    frame: crate::sidebar::frame::PaneFrame,
) {
    plane.publish_fold(super::fetch::WorkspaceFold {
        sampled_at: std::time::Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
        frame: Some(Arc::new(frame)),
    });
}

pub(crate) fn serve_config(ws: &WorkspaceId) -> ServeConfig {
    ServeConfig {
        room: RoomConfig {
            workspace_id: ws.clone(),
            mux: crate::MuxName::Zellij,
            session_name: "rimz-test".to_owned(),
            tick_seconds: 1,
            timezone: jiff::tz::TimeZone::UTC,
            notification_prefs: crate::config::NotificationsPrefs::default(),
        },
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 1,
        refresh_ms_override: None,
        own_pane: PaneId::from_parts(crate::MuxName::Zellij, "sidebar_test"),
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
        hosted_agent_lineage: Vec::new(),
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
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        status_counts: Vec::new(),
        rows: panes
            .into_iter()
            .map(|pane| crate::store::snapshot::SidebarRow {
                checkout: None,
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
            .collect::<Vec<_>>()
            .into(),
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
        pr_queue: None,
        ci: None,
        pr_number: None,
        pr_url: None,
    }]
    .into();
    snapshot
}

pub(crate) fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    let row = crate::store::snapshot::SidebarRow {
        checkout: None,
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
        card: crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
            crate::store::snapshot::AgentCard {
                status: crate::agents::AgentStatus::Idle,
                phase: crate::agents::TurnPhase::Idle,
                task: Some("inspect auth".to_owned()),
                model: Some("Opus".to_owned()),
                ..crate::store::snapshot::AgentCard::default()
            },
        )),
    };
    snapshot.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        status_counts: vec![crate::store::snapshot::SidebarStatusCount {
            status: crate::agents::AgentStatus::Idle,
            count: 1,
        }],
        rows: vec![row].into(),
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
        pr_queue: None,
        ci: None,
        pr_number: None,
        pr_url: None,
    }]
    .into();
    snapshot
}
