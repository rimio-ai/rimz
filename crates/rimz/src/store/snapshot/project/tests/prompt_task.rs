use super::*;

#[test]
fn lifecycle_rows_bound_full_pane_stamps_and_prompt_labels() {
    let prompt = "p".repeat(6 * 1024);
    let mut stamp = pane("%1", "claude", "/repo");
    stamp.spawn_command = Some("rimz agents exec ".repeat(100));
    stamp.foreground_cmdline = stamp.spawn_command.clone();
    let event = raw_lifecycle(
        "claude",
        json!({
            "event_name": "SessionStart",
            "agent_id": "s1",
            "signal": { "signal": "registered" },
            "pane_stamp": stamp,
            "prompt": prompt,
            "task": prompt,
        }),
    );
    let agents = reduce_agent_states(&[event]);
    let agent = &agents[0];
    let stamp = agent.pane.as_ref().unwrap();
    assert!(stamp.spawn_command.is_none());
    assert!(stamp.foreground_cmdline.is_none());
    assert_eq!(stamp.cwd.as_deref(), Some("/repo"));
    for label in [&agent.prompt, &agent.first_prompt, &agent.task] {
        assert_eq!(
            label.as_ref().unwrap().len(),
            crate::agents::state::PROMPT_BYTES_LIMIT
        );
    }
}

#[test]
fn launch_rows_bound_prompt_and_task_before_registration() {
    let prompt = "p".repeat(6 * 1024);
    let agents = reduce_agent_states(&[launch_event(
        "codex",
        AgentLaunchPayload {
            prompt: Some(prompt),
            ..launch_payload("launch-a", "lucid-atlas")
        },
    )]);
    for label in [&agents[0].prompt, &agents[0].first_prompt, &agents[0].task] {
        assert_eq!(
            label.as_ref().unwrap().len(),
            crate::agents::state::PROMPT_BYTES_LIMIT
        );
    }
}

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
fn lifecycle_carries_transcript_path_and_latest_prompt() {
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
    assert_eq!(agent.first_prompt.as_deref(), Some("prompt 0"));
}

#[test]
fn launch_prompts_replace_latest_and_preserve_first_prompt() {
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

    assert_eq!(agents[0].prompt.as_deref(), Some("verify"));
    assert_eq!(agents[0].first_prompt.as_deref(), Some("plan"));
}
