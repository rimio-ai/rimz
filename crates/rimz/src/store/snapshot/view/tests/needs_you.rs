use super::*;
use crate::agents::{ParkDemotion, TurnErrorClass};
use crate::store::snapshot::view::model::{card_status, needs_you};

#[test]
fn opencode_parallel_tool_preserves_attention_and_pane_input() {
    let definition = crate::agents::registry::find_definition("opencode").unwrap();
    let observe = |name: &str, mut payload: serde_json::Value, age: u64| {
        payload["session_id"] = serde_json::json!("ses_ask");
        let mut observation = definition
            .decode_hook(name, &payload)
            .unwrap()
            .lifecycle()
            .cloned()
            .unwrap();
        if let LifecycleSignal::AwaitingInput { ask_id, .. } = &mut observation.signal {
            *ask_id = Some(crate::ids::AskId::new());
        }
        let mut event = crate::store::event::EventEnvelope::agent_lifecycle(
            workspace(),
            "session",
            "opencode",
            name,
            &observation,
        );
        event.timestamp = epoch() - std::time::Duration::from_secs(age);
        event
    };
    let mut actual = Vec::new();
    for close in ["question_replied", "question_rejected", "session_idle"] {
        let mut events = vec![
            observe("chat_message", serde_json::json!({"prompt":"search"}), 4),
            observe(
                "question_ask",
                serde_json::json!({"request_id":"form-1"}),
                3,
            ),
            observe("tool_after", serde_json::json!({"tool_name":"webfetch"}), 2),
        ];
        let states = reduce_agent_states(&events);
        let state = &states[0];
        let row = row_from_agent(state, epoch());
        let attention =
            needs_you(state, Some(&row), None, &Default::default(), epoch()).map(|ask| ask.status);
        let reserves_input = state.is_awaiting_input();
        events.push(observe(
            close,
            serde_json::json!({"request_id":"form-1"}),
            1,
        ));
        let states = reduce_agent_states(&events);
        let state = &states[0];
        let row = row_from_agent(state, epoch());
        actual.push((
            attention,
            reserves_input,
            needs_you(state, Some(&row), None, &Default::default(), epoch()).map(|ask| ask.status),
            state.is_awaiting_input(),
        ));
    }
    assert_eq!(
        actual,
        vec![(Some(AgentStatus::Waiting), true, None, false); 3]
    );
}

#[test]
fn card_projection_status_honors_card_and_rowless_demotion() {
    let now = epoch();
    let mut state = agent("codex", "status", AgentStatus::Running, 0);
    let mut context = crate::agents::AgentContext::new("codex", now);
    context.turn_error = Some(crate::agents::AgentTurnError {
        ended_turn: false,
        class: TurnErrorClass::PausedRateLimit,
        at: now + jiff::SignedDuration::from_secs(1),
        label: Some("limit reached".into()),
    });
    state.context = Some(context);
    let demotion = ParkDemotion::default();
    let mut row = row_from_agent(&state, now);
    let card = row.as_agent_mut().unwrap();
    card.status = AgentStatus::Sleeping;
    card.phase = TurnPhase::Reasoning;
    assert_eq!(
        card_status(&state, Some(&row), None, &demotion, now),
        (AgentStatus::Sleeping, TurnPhase::Reasoning)
    );
    assert_eq!(
        card_status(&state, None, None, &demotion, now),
        (AgentStatus::Paused, TurnPhase::Idle)
    );
    let spent = ParkDemotion::new(
        &Default::default(),
        [(state.kind.clone(), state.agent_id.clone())].into(),
        now,
    );
    assert_eq!(
        card_status(&state, None, None, &spent, now),
        (AgentStatus::Failed, TurnPhase::Idle)
    );
    let mut child = sub_agent_from_state(&state, now, false, default_stall_secs() as u32);
    child.status = AgentStatus::Running;
    child.phase = TurnPhase::Acting;
    assert_eq!(
        card_status(&state, None, Some(&child), &spent, now),
        (AgentStatus::Running, TurnPhase::Acting)
    );
    assert_eq!(
        card_status(&state, Some(&row), Some(&child), &spent, now),
        (AgentStatus::Sleeping, TurnPhase::Reasoning)
    );
}

#[test]
fn attention_uses_ask_age_and_error_description() {
    let mut state = agent("claude", "ask", AgentStatus::Waiting, 0);
    state.open_ask = Some(crate::agents::OpenAsk {
        extra_questions: 0,
        id: "ask_0123456789abcdef".parse().unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: Some("which strategy?".into()),
        native_key: None,
        since: ago(120),
    });
    let row = row_from_agent(&state, epoch());
    let attention = needs_you(&state, Some(&row), None, &Default::default(), epoch()).unwrap();
    assert_eq!(attention.reason.as_deref(), Some("which strategy?"));
    assert_eq!(attention.since, ago(120));

    let state = agent("claude", "error", AgentStatus::Failed, 0).turn_error(10, "bad request");
    let mut row = row_from_agent(&state, epoch());
    row.as_agent_mut().unwrap().status = AgentStatus::Failed;
    row.as_agent_mut().unwrap().turn_error_label = Some("bad request".into());
    let attention = needs_you(&state, Some(&row), None, &Default::default(), epoch()).unwrap();
    assert_eq!(attention.reason.as_deref(), Some("bad request"));
    assert_eq!(attention.since, row.last_activity);
}

#[test]
fn attention_lifts_native_child_waiting() {
    let state = agent("claude", "root", AgentStatus::Idle, 0).worktree("/repo/main");
    let snapshot = room_with_agent_panes(vec![
        state.clone(),
        child_state("root", "child", AgentStatus::Waiting, 0),
    ]);
    let attention = needs_you(
        &state,
        Some(row(&snapshot, "root")),
        None,
        &Default::default(),
        epoch(),
    )
    .unwrap();
    assert_eq!(attention.status, AgentStatus::Waiting);
}

#[test]
fn attention_membership_and_rowless_fallback() {
    for status in [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        let state = agent("claude", "attention", status, 0);
        let mut row = row_from_agent(&state, epoch());
        row.as_agent_mut().unwrap().status = status;
        let attention = needs_you(&state, Some(&row), None, &Default::default(), epoch()).unwrap();
        assert_eq!(attention.status, status);
        assert_eq!(attention.since, row.last_activity);
    }
    for status in [
        AgentStatus::Running,
        AgentStatus::Idle,
        AgentStatus::Sleeping,
    ] {
        let state = agent("claude", "quiet", status, 0);
        let mut row = row_from_agent(&state, epoch());
        row.as_agent_mut().unwrap().status = status;
        assert!(needs_you(&state, Some(&row), None, &Default::default(), epoch()).is_none());
    }
    let state = agent("claude", "failed", AgentStatus::Failed, 0);
    assert_eq!(
        needs_you(&state, None, None, &Default::default(), epoch())
            .unwrap()
            .status,
        state.rowless_status(&Default::default(), epoch()).0,
    );
}
