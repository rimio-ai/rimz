use super::*;

#[test]
fn turn_started_tracks_prompt_never_stop() {
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let prompt_ts = prompt.timestamp;
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let agents = reduce_agent_states(&[start, prompt, stop]);
    // The boundary is the prompt; the later Stop must not advance it (that is
    // what keeps a finished child visible until the *next* prompt).
    assert_eq!(agents[0].turn_started_at, Some(prompt_ts));
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
    // Stop carries neither task nor prompt: task is activity-bound and clears,
    // but the prompt persists to label the unnamed session past its turn.
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let agents = reduce_agent_states(&[prompt, stop]);
    let agent = agents.iter().find(|a| a.agent_id == "s1").expect("agent");
    assert_eq!(agent.task, None, "the task clears on idle");
    assert_eq!(
        agent.prompt.as_deref(),
        Some("fix auth flow"),
        "the latest prompt persists past the Stop"
    );
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
