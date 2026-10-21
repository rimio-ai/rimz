use super::*;
use crate::agents::{PendingWait, PendingWaitTrigger};

fn pending_wait() -> PendingWait {
    PendingWait {
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "wait-soon".to_owned(),
        trigger: PendingWaitTrigger::Timer {
            due: ago(-60),
            delay: None,
        },
        armed_at: Some(ago(60)),
    }
}

#[test]
fn hibernation_preserves_sleeping_in_consumer_tallies_and_ranking() {
    let mut resting = agent("claude", "frozen", AgentStatus::Success, 0).worktree("/repo/main");
    resting.hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: ago(30),
        step: 0,
        reclaimed_bytes: 200,
        wake_error: None,
    });
    resting.pending_waits = vec![pending_wait()];
    let idle = agent("claude", "idle", AgentStatus::Idle, 1).worktree("/repo/main");
    let snapshot = room_with_agent_panes(vec![resting, idle]);
    let frozen = row(&snapshot, "frozen");
    assert_eq!(frozen.status(), Some(AgentStatus::Sleeping));
    assert!(!frozen.status().unwrap().is_attention());
    assert_eq!(
        frozen.attention_score,
        crate::store::snapshot::view::score::status_weight(AgentStatus::Sleeping)
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Sleeping && count.count == 1)
    );
    assert_eq!(serde_json::to_value(frozen).unwrap()["status"], "sleeping");
}

#[test]
fn parked_working_wait_projects_running_and_keeps_phase() {
    for age_secs in [60, default_stall_secs() + 60] {
        let mut session = agent("claude", "sleeper", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .parked()
            .active_ago(age_secs);
        session.pending_waits = vec![
            pending_wait(),
            PendingWait {
                class: crate::agents::PendingWaitClass::Working,
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
        assert_eq!(projected.status(), Some(AgentStatus::Running));
        assert_eq!(projected.phase(), TurnPhase::Parked);
        assert_eq!(projected.as_agent().unwrap().pending_waits, expected_waits);
        assert_eq!(
            rollup_agent(&snapshot, "sleeper").status,
            AgentStatus::Running
        );

        let json = serde_json::to_value(projected).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["pending_waits"][0]["name"], "wait-soon");
        assert_eq!(json["pending_waits"][1]["name"], "wait-later");
        assert_eq!(
            serde_json::from_value::<SidebarRow>(json).unwrap(),
            *projected
        );
    }
}

#[test]
fn completed_turn_working_wait_keeps_its_wait_description() {
    let mut session = agent("codex", "review", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .turn_started_ago(120)
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Complete);
    session.phase = TurnPhase::Reasoning;
    let mut wait = pending_wait();
    wait.class = crate::agents::PendingWaitClass::Working;
    wait.trigger = PendingWaitTrigger::Command {
        command: "cargo test".into(),
    };
    session.pending_waits.push(wait);

    let snapshot = room_with_agent_panes(vec![session.clone()]);
    let card = row(&snapshot, "review").as_agent().unwrap();
    assert_eq!(card.status, AgentStatus::Running);
    assert_eq!(
        card.description(snapshot.now),
        crate::store::snapshot::CardDescription::Wait("wakes after cargo test".into())
    );
    let (status, phase) = session.rowless_status(&Default::default());
    let fallback = crate::store::snapshot::AgentCard {
        status,
        phase,
        ..crate::store::snapshot::AgentCard::from_agent(&session, snapshot.now)
    };
    assert_eq!(
        fallback.description(snapshot.now),
        card.description(snapshot.now)
    );
}

#[test]
fn resting_shell_displays_running_without_changing_delivery_status() {
    let mut session = agent("claude", "shell", AgentStatus::Success, 0).worktree("/repo/main");
    session
        .background_shells
        .push(crate::agents::BackgroundShell {
            id: "gate".into(),
            command: Some("cargo xtask gate".into()),
            description: None,
            started_at: ago(60),
        });
    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(row(&snapshot, "shell").status(), Some(AgentStatus::Running));
    assert_eq!(
        rollup_agent(&snapshot, "shell").effective_status(),
        AgentStatus::Success
    );
}

#[test]
fn launched_child_resting_on_shell_holds_parent_turn() {
    let parent = agent("claude", "root", AgentStatus::Success, 0).worktree("/repo/main");
    let mut child = child_state("root", "child", AgentStatus::Success, 60);
    child.launch_depth = Some(1);
    child
        .background_shells
        .push(crate::agents::BackgroundShell {
            id: "gate".into(),
            command: Some("cargo xtask gate".into()),
            description: None,
            started_at: ago(60),
        });
    let snapshot = room_with_agent_panes(vec![parent, child]);
    let parent = row(&snapshot, "root");
    assert_eq!(parent.status(), Some(AgentStatus::Running));
    assert!(parent.sub_agents()[0].holds_parent_turn());
}

#[test]
fn settled_wait_does_not_hide_the_rested_verdict() {
    let mut session = agent("claude", "rest", AgentStatus::Success, 0).worktree("/repo/main");
    session.pending_waits.push(PendingWait {
        name: "@child".into(),
        trigger: PendingWaitTrigger::Subagent {
            active_at: ago(30),
            deadline_at: None,
            settled: Some("completed".into()),
        },
        armed_at: Some(ago(60)),
        class: crate::agents::PendingWaitClass::Inert,
    });
    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(row(&snapshot, "rest").status(), Some(AgentStatus::Success));
}

#[test]
fn file_wait_displays_sleeping() {
    let mut session = agent("claude", "rest", AgentStatus::Success, 0).worktree("/repo/main");
    session.pending_waits.push(PendingWait {
        name: "file".into(),
        trigger: PendingWaitTrigger::File {
            path: "app.log".into(),
            grep: None,
        },
        armed_at: Some(ago(60)),
        class: crate::agents::PendingWaitClass::Sleeping,
    });
    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(row(&snapshot, "rest").status(), Some(AgentStatus::Sleeping));
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
