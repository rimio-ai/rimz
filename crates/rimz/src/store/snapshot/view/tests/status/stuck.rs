use super::*;
use crate::store::snapshot::{SidebarPipeline, SidebarUnitKey, WorktreeTrunkSync};

fn staged(owner: AgentStatus, other: AgentStatus, age: i64) -> SidebarSnapshot {
    let members = [("coder", owner), ("reviewer", other)].map(|(role, status)| {
        let mut member = agent("claude", role, status, 0)
            .worktree("/repo/main")
            .active_ago(age);
        member.team = Some("crew".into());
        member.role = Some(role.into());
        member
    });
    let mut snapshot = room_with_agent_panes(members.into());
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    group.pipeline = Some(SidebarPipeline {
        stages: vec!["Implement".into()],
        stage: "Implement".into(),
        owner: Some("coder".into()),
        started_at: Some(ago(age)),
        stage_started_at: Some(ago(age)),
        stage_prior_secs: 0,
        visited: Default::default(),
        done_at: None,
    });
    snapshot
}

#[test]
fn stuck_stage_fails_only_the_owner_and_refreshes_group_facts() {
    let mut snapshot = staged(
        AgentStatus::Success,
        AgentStatus::Idle,
        default_stall_secs(),
    );
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    group.trunk_sync = Some(WorktreeTrunkSync::Merged);
    group.finished = true;
    snapshot.project_stuck_stages();
    assert_eq!(row(&snapshot, "coder").status(), Some(AgentStatus::Failed));
    assert_eq!(
        row(&snapshot, "coder").turn_error_label(),
        Some("stage Implement: nothing running 30m")
    );
    assert_eq!(row(&snapshot, "reviewer").status(), Some(AgentStatus::Idle));
    let group = &snapshot.worktree_groups[0];
    assert!(!group.finished);
    assert_eq!(group.rows[0].id, row(&snapshot, "coder").id);
    assert!(
        group
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1)
    );
    let units = group.unit_statuses();
    let unit = &units[&SidebarUnitKey::Team("crew")];
    assert_eq!(unit.buckets(), &[AgentStatus::Failed]);
    assert!(unit.carries(row(&snapshot, "coder"), AgentStatus::Failed));
    assert!(!unit.carries(row(&snapshot, "reviewer"), AgentStatus::Failed));
}

#[test]
fn stuck_stage_preserves_a_waiting_childs_attention_and_jump() {
    let mut snapshot = staged(
        AgentStatus::Idle,
        AgentStatus::Success,
        default_stall_secs() + 60,
    );
    let child_pane = row(&snapshot, "reviewer")
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    let card = std::sync::Arc::make_mut(&mut group.rows)
        .iter_mut()
        .find(|row| row.id.contains("coder"))
        .unwrap()
        .as_agent_mut()
        .unwrap();
    card.sub_agents.push(
        serde_json::from_value(serde_json::json!({
            "id": "asking-child", "name": "claude", "status": "waiting",
            "pane": child_pane, "last_activity": ago(default_stall_secs() + 60),
        }))
        .unwrap(),
    );

    snapshot.project_stuck_stages();

    let owner = row(&snapshot, "coder");
    assert_eq!(owner.attention_status(), Some(AgentStatus::Waiting));
    assert_eq!(owner.status(), Some(AgentStatus::Idle));
    assert_eq!(owner.turn_error_label(), None);
    assert_eq!(owner.attention_pane(), Some(&child_pane));
    let units = snapshot.worktree_groups[0].unit_statuses();
    assert_eq!(
        units[&SidebarUnitKey::Team("crew")].buckets(),
        &[AgentStatus::Waiting]
    );
}

#[test]
fn stuck_stage_window_uses_the_latest_member_or_stage_activity() {
    for source in ["window", "member", "stage"] {
        let mut snapshot = staged(
            AgentStatus::Success,
            AgentStatus::Idle,
            default_stall_secs() + 60,
        );
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
        match source {
            "window" => snapshot.now = ago(61),
            "member" => {
                std::sync::Arc::make_mut(&mut group.rows)
                    .iter_mut()
                    .find(|row| row.id.contains("reviewer"))
                    .unwrap()
                    .last_activity = ago(1)
            }
            "stage" => group.pipeline.as_mut().unwrap().stage_started_at = Some(ago(1)),
            _ => unreachable!(),
        }
        snapshot.project_stuck_stages();
        assert_eq!(
            row(&snapshot, "coder").status(),
            Some(AgentStatus::Success),
            "{source}"
        );
        assert_eq!(row(&snapshot, "coder").turn_error_label(), None);
        let units = snapshot.worktree_groups[0].unit_statuses();
        assert_eq!(
            units[&SidebarUnitKey::Team("crew")].buckets(),
            &[AgentStatus::Idle]
        );
    }
}

#[test]
fn stuck_stage_requires_resting_members_and_an_unfinished_owned_board() {
    for status in [
        AgentStatus::Running,
        AgentStatus::Sleeping,
        AgentStatus::Waiting,
        AgentStatus::Paused,
        AgentStatus::Failed,
    ] {
        for owner in [false, true] {
            let mut snapshot = staged(
                AgentStatus::Idle,
                AgentStatus::Success,
                default_stall_secs() + 60,
            );
            let name = if owner { "coder" } else { "reviewer" };
            let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
            std::sync::Arc::make_mut(&mut group.rows)
                .iter_mut()
                .find(|row| row.id.contains(name))
                .unwrap()
                .as_agent_mut()
                .unwrap()
                .status = status;
            snapshot.project_stuck_stages();
            assert_eq!(
                row(&snapshot, "coder").status(),
                Some(if owner { status } else { AgentStatus::Idle })
            );
            assert_eq!(row(&snapshot, "coder").turn_error_label(), None);
        }
    }
    for missing in ["done", "owner", "pipeline", "ambiguous"] {
        let mut snapshot = staged(
            AgentStatus::Idle,
            AgentStatus::Success,
            default_stall_secs() + 60,
        );
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
        match missing {
            "done" => group.pipeline.as_mut().unwrap().stage = "Done".into(),
            "owner" => group.pipeline.as_mut().unwrap().owner = Some("absent".into()),
            "pipeline" => group.pipeline = None,
            "ambiguous" => group.team = None,
            _ => unreachable!(),
        }
        snapshot.project_stuck_stages();
        assert_eq!(
            row(&snapshot, "coder").status(),
            Some(AgentStatus::Idle),
            "{missing}"
        );
    }
}
