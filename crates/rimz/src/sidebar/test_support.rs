use std::path::Path;

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus, RateLimitWindow, TurnPhase};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;
use crate::{SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind};

pub(crate) fn pane(id: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, id),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: None,
        view_name: None,
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

pub(crate) fn pane_in_tab(id: &str, view_id: &str) -> PaneRef {
    PaneRef {
        view_id: Some(view_id.to_owned()),
        ..pane(id, "zsh", "/tmp")
    }
}

/// A 5-hour budget window for tests — a known `duration_mins` so the
/// projection and per-duration reconciliation have something to key on.
pub(crate) fn rl_window(used: u8, resets_at: Option<Timestamp>) -> RateLimitWindow {
    rl_window_mins(used, resets_at, 300)
}

pub(crate) fn rl_window_mins(
    used: u8,
    resets_at: Option<Timestamp>,
    duration_mins: u32,
) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at,
        duration_mins: Some(duration_mins),
        ..Default::default()
    }
}

pub(crate) fn provider_panel(
    kind: &str,
    windows: Vec<RateLimitWindow>,
) -> crate::SidebarProviderPanel {
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        color: 0,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered: true,
        remote_control: Default::default(),
        spending: None,
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
        windows,
    }
}

pub(crate) fn snapshot_with_panels(
    workspace: WorkspaceId,
    panels: Vec<crate::SidebarProviderPanel>,
) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    snapshot.providers = panels;
    snapshot
}

pub(crate) fn root_agent(kind: &str, agent_id: &str, model: Option<&str>) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        name: Some(test_agent_name(agent_id)),
        kind_ordinal: Some(test_agent_ordinal(agent_id)),
        status: AgentStatus::Running,
        model: model.map(ToOwned::to_owned),
        ..crate::testkit::agent_state(kind, agent_id, now)
    }
}

pub(crate) fn test_agent_name(agent_id: &str) -> String {
    let slug = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("test-{slug}")
}

pub(crate) fn test_agent_ordinal(agent_id: &str) -> u32 {
    agent_id
        .bytes()
        .fold(1u32, |acc, byte| acc.wrapping_add(byte as u32))
}

pub(crate) fn child_agent(kind: &str, parent_id: &str, agent_id: &str) -> AgentState {
    let mut agent = root_agent(kind, agent_id, None);
    agent.parent_agent_id = Some(parent_id.into());
    agent
}

pub(crate) fn activity_row(
    is_agent: bool,
    status: Option<AgentStatus>,
    last_activity: Timestamp,
    worktree_path: &Path,
) -> crate::SidebarRow {
    crate::SidebarRow {
        id: "row".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some(worktree_path.display().to_string()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity,
        card: if is_agent {
            crate::RowCard::Agent(Box::new(crate::AgentCard {
                status: status.unwrap_or(AgentStatus::Idle),
                phase: TurnPhase::Idle,
                ..crate::AgentCard::default()
            }))
        } else {
            crate::RowCard::Process(crate::ProcessCard::default())
        },
    }
}

pub(crate) fn worktree_group(path: &Path, rows: Vec<crate::SidebarRow>) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        key: path.display().to_string(),
        label: "wt".to_owned(),
        kind: SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows,
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
        pr_number: None,
    }
}
