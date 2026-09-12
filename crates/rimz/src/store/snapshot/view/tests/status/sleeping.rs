use super::*;
use crate::agents::{PendingWait, PendingWaitTrigger};

fn pending_wait() -> PendingWait {
    PendingWait {
        name: "wait-soon".to_owned(),
        trigger: PendingWaitTrigger::Timer {
            due: ago(-60),
            delay: None,
        },
        armed_at: Some(ago(60)),
    }
}

#[test]
fn parked_pending_wait_projects_sleeping_and_keeps_phase() {
    for age_secs in [60, default_stall_secs() + 60] {
        let mut session = agent("claude", "sleeper", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .parked()
            .active_ago(age_secs);
        session.pending_waits = vec![
            pending_wait(),
            PendingWait {
                name: "wait-later".to_owned(),
                trigger: PendingWaitTrigger::Command {
                    command: "cargo test".to_owned(),
                },
                armed_at: Some(ago(60)),
            },
        ];

        let expected_waits = session.pending_waits.clone();
        let snapshot = room_with_agent_panes(vec![session]);
        let projected = row(&snapshot, "sleeper");
        assert_eq!(projected.status(), Some(AgentStatus::Sleeping));
        assert_eq!(projected.phase(), TurnPhase::Parked);
        assert_eq!(projected.as_agent().unwrap().pending_waits, expected_waits);
        assert_eq!(
            rollup_agent(&snapshot, "sleeper").status,
            AgentStatus::Running
        );

        let json = serde_json::to_value(projected).unwrap();
        assert_eq!(json["status"], "sleeping");
        assert_eq!(json["pending_waits"][0]["name"], "wait-soon");
        assert_eq!(json["pending_waits"][1]["name"], "wait-later");
        assert_eq!(
            serde_json::from_value::<SidebarRow>(json).unwrap(),
            *projected
        );
    }
}

#[test]
fn pending_wait_only_overlays_resting_display_statuses() {
    for (status, expected) in [
        (AgentStatus::Idle, AgentStatus::Sleeping),
        (AgentStatus::Success, AgentStatus::Sleeping),
        (AgentStatus::Running, AgentStatus::Running),
        (AgentStatus::Waiting, AgentStatus::Waiting),
        (AgentStatus::Failed, AgentStatus::Failed),
        (AgentStatus::Paused, AgentStatus::Paused),
    ] {
        let mut session = agent("claude", "sleeper", status, 0).worktree("/repo/main");
        session.pending_waits = vec![pending_wait()];
        let snapshot = room_with_agent_panes(vec![session]);
        assert_eq!(
            row(&snapshot, "sleeper").status(),
            Some(expected),
            "{status:?}"
        );
    }

    for (label, class, expected) in [
        (
            "API Error: Bad Request",
            crate::agents::TurnErrorClass::Failed,
            AgentStatus::Failed,
        ),
        (
            "You've hit your usage limit",
            crate::agents::TurnErrorClass::PausedRateLimit,
            AgentStatus::Paused,
        ),
    ] {
        let mut session = agent("claude", "sleeper", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .parked()
            .active_ago(60)
            .turn_error_class(10, label, class);
        session.pending_waits = vec![pending_wait()];
        let snapshot = room_with_agent_panes(vec![session]);
        assert_eq!(
            row(&snapshot, "sleeper").status(),
            Some(expected),
            "{label}"
        );
    }
}

#[test]
fn live_child_outranks_pending_wait_until_it_settles() {
    for status in [
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Running,
    ] {
        for child_status in [AgentStatus::Running, AgentStatus::Success] {
            let mut parent = agent("claude", "sleeper", status, 100)
                .worktree("/repo/main")
                .parked();
            parent.pending_waits = vec![pending_wait()];
            let child = child_state("sleeper", "child", child_status, 5);
            let snapshot = room_with_agent_panes(vec![parent, child]);
            let expected = if child_status == AgentStatus::Running {
                AgentStatus::Running
            } else {
                AgentStatus::Sleeping
            };
            assert_eq!(
                row(&snapshot, "sleeper").status(),
                Some(expected),
                "{status:?}"
            );
        }
    }
}
