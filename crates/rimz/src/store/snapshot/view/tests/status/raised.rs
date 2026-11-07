use super::*;
use crate::agents::state::{CardAlert, RaisedAsk, RaisedAskState};

fn raised() -> RaisedAsk {
    RaisedAsk {
        ask_id: crate::ids::AskId::new(),
        state: RaisedAskState::Open,
        raised_at: ago(1),
        basis_at: ago(2),
        opened_at: Some(ago(1)),
        text: Some("The implementation is ready. Which option?\nMore context.".into()),
        raised_by: Some("@lead".into()),
        options: vec![],
    }
}

#[test]
fn raised_asks_lift_only_clean_rest_without_changing_native_input_ownership() {
    for status in [
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Running,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Sleeping,
    ] {
        let mut state = agent("claude", "raised", status, 0).worktree("/repo/main");
        state.raised_ask = Some(raised());
        assert!(!state.is_awaiting_input());
        let snapshot = room_with_agent_panes(vec![state]);
        let row = row(&snapshot, "raised");
        let clean = matches!(status, AgentStatus::Idle | AgentStatus::Success);
        assert_eq!(
            row.status(),
            Some(if clean { AgentStatus::Waiting } else { status }),
            "{status:?}"
        );
        if clean {
            assert_eq!(
                row.as_agent().unwrap().ask_detail.as_deref(),
                Some("Which option?")
            );
            assert_eq!(row.attention_status(), Some(AgentStatus::Waiting));
        }
    }
    let mut state = agent("claude", "parked", AgentStatus::Success, 0).worktree("/repo/main");
    state.phase = TurnPhase::Parked;
    state.raised_ask = Some(raised());
    let snapshot = room_with_agent_panes(vec![state]);
    assert_ne!(
        row(&snapshot, "parked").status(),
        Some(AgentStatus::Waiting)
    );
}

#[test]
fn alerts_override_work_and_results_but_native_waiting_wins() {
    for status in [
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Waiting,
    ] {
        let mut state = agent("claude", "alerted", status, 0).worktree("/repo/main");
        state.raised_ask = Some(raised());
        state.alert = Some(CardAlert {
            text: "Please check the result".into(),
            raised_at: ago(1),
            raised_by: None,
        });
        let snapshot = room_with_agent_panes(vec![state]);
        let row = row(&snapshot, "alerted");
        if status == AgentStatus::Waiting {
            assert_eq!(row.status(), Some(AgentStatus::Waiting));
            assert_ne!(row.turn_error_label(), Some("Please check the result"));
        } else {
            assert_eq!(row.status(), Some(AgentStatus::Failed));
            assert_eq!(row.turn_error_label(), Some("Please check the result"));
        }
    }
}

#[test]
fn an_old_raised_row_stays_actionable_instead_of_sinking() {
    let mut state = agent("claude", "old", AgentStatus::Idle, 0)
        .worktree("/repo/main")
        .active_ago(7200);
    state.raised_ask = Some(raised());
    let snapshot = room_with_agent_panes(vec![state]);
    let row = row(&snapshot, "old");
    assert_eq!(row.attention_status(), Some(AgentStatus::Waiting));
    assert!(!row.inactive && !row.archived);
}

#[test]
fn needs_you_uses_the_displayed_raised_question_and_its_attention_clock() {
    let mut state = agent("claude", "raised", AgentStatus::Success, 0)
        .worktree("/repo/main")
        .active_ago(100);
    state.description = Some("old task".into());
    state.raised_ask = Some(raised());
    let snapshot = room_with_agent_panes(vec![state.clone()]);
    let row = row(&snapshot, "raised");
    let attention =
        crate::store::snapshot::needs_you(&state, Some(row), &Default::default(), snapshot.now)
            .unwrap();
    assert_eq!(attention.reason.as_deref(), Some("Which option?"));
    assert_eq!(
        attention.since,
        row.as_agent().unwrap().overlay_raised_at.unwrap()
    );
}

#[test]
fn raised_card_lines_use_the_first_nonblank_line_and_cap_unicode_text() {
    for (text, expected) in [
        (
            "\n  Please   review\nMore context.".to_owned(),
            "Please review".to_owned(),
        ),
        ("界".repeat(200), "界".repeat(160)),
    ] {
        let mut state = agent("claude", "raised", AgentStatus::Success, 0).worktree("/repo/main");
        state.raised_ask = Some(RaisedAsk {
            text: Some(text),
            ..raised()
        });
        let snapshot = room_with_agent_panes(vec![state]);
        assert_eq!(
            row(&snapshot, "raised")
                .as_agent()
                .unwrap()
                .ask_detail
                .as_deref(),
            Some(expected.as_str())
        );
    }
}
