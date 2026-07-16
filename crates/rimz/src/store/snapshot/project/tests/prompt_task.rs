use super::*;

#[test]
fn prompt_persists_past_stop_while_task_clears() {
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "s1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
            "prompt": "fix auth flow",
        }),
    );
    let prompt_ts = prompt.timestamp;
    // Stop carries neither task nor prompt: task is activity-bound and clears,
    // but the prompt persists to label the unnamed session past its turn.
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let agents = reduce_agent_states(&[prompt, stop]);
    let agent = agents.iter().find(|a| a.agent_id == "s1").expect("agent");
    assert_eq!(
        agent.turn_started_at,
        Some(prompt_ts),
        "the later Stop must not advance the turn boundary"
    );
    assert_eq!(agent.task, None, "the task clears on idle");
    assert_eq!(
        agent.prompt.as_deref(),
        Some("fix auth flow"),
        "the latest prompt persists past the Stop"
    );
    assert_eq!(agent.first_prompt.as_deref(), Some("fix auth flow"));
}

#[test]
fn first_prompt_sets_once_and_skips_control_turns() {
    let prompt = |value: &str| {
        raw_lifecycle(
            "claude",
            serde_json::json!({
                "event_name": "UserPromptSubmit",
                "agent_id": "s1",
                "signal": { "signal": "turn_started" },
                "prompt": value,
            }),
        )
    };
    let agents = reduce_agent_states(&[
        prompt("<task-notification>synthetic</task-notification>"),
        prompt("stable first prompt"),
        prompt("latest prompt"),
    ]);

    assert_eq!(
        agents[0].first_prompt.as_deref(),
        Some("stable first prompt")
    );
    assert_eq!(agents[0].prompt.as_deref(), Some("latest prompt"));
}

#[test]
fn adapter_description_replaces_launch_label_and_carries_forward() {
    let launch = raw_launch_with_description(
        AgentLaunchState::Bound,
        "s1",
        "lucid-atlas",
        None,
        Some("launch label"),
    );
    let titled = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "s1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            "description": "native title",
        }),
    );
    let later = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "s1",
            "signal": { "signal": "turn_started" },
        }),
    );

    let agents = reduce_agent_states(&[launch, titled, later]);
    assert_eq!(agents[0].description.as_deref(), Some("native title"));
}

#[test]
fn lifecycle_carries_transcript_path_and_bounded_prompt_history() {
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "s1",
            "signal": { "signal": "registered" },
            "transcript_path": "/tmp/s1.jsonl",
        }),
    );
    let mut events = vec![start];
    for index in 0..18 {
        events.push(raw_lifecycle(
            "claude",
            serde_json::json!({
                "event_name": "UserPromptSubmit",
                "agent_id": "s1",
                "signal": { "signal": "turn_started" },
                "prompt": format!("prompt {index}"),
            }),
        ));
    }

    let agents = reduce_agent_states(&events);
    let agent = agents.iter().find(|a| a.agent_id == "s1").expect("agent");

    assert_eq!(agent.transcript_path.as_deref(), Some("/tmp/s1.jsonl"));
    assert_eq!(agent.prompt.as_deref(), Some("prompt 17"));
    assert_eq!(agent.recent_prompts.len(), 16);
    assert_eq!(
        agent.recent_prompts.first().map(String::as_str),
        Some("prompt 2")
    );
    assert_eq!(
        agent.recent_prompts.last().map(String::as_str),
        Some("prompt 17")
    );
}

#[test]
fn launch_prompts_append_to_recent_prompt_history() {
    let launch_with_prompt = |prompt: &str, offset: i64| {
        let mut event = launch_event(
            "codex",
            AgentLaunchPayload {
                prompt: Some(prompt.to_owned()),
                ..launch_payload("launch-a", "lucid-atlas")
            },
        );
        event.timestamp = Timestamp::from_second(epoch().as_second() + offset).unwrap();
        event
    };

    let agents = reduce_agent_states(&[
        launch_with_prompt("plan", 1),
        launch_with_prompt("build", 2),
        launch_with_prompt("verify", 3),
    ]);

    assert_eq!(
        agents[0].recent_prompts,
        vec!["plan".to_owned(), "build".to_owned(), "verify".to_owned()]
    );
    assert_eq!(agents[0].first_prompt.as_deref(), Some("plan"));
}
