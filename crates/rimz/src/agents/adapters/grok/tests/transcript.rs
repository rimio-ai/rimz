use super::*;

fn chunk(tag: &str, text: &str, prompt_index: Option<u64>) -> String {
    let mut update = serde_json::json!({
        "sessionUpdate": tag,
        "content": {"type": "text", "text": text}
    });
    if let Some(index) = prompt_index {
        update["_meta"] = serde_json::json!({"promptIndex": index});
    }
    serde_json::json!({
        "timestamp": 1_700_000_000_u64,
        "method": "session/update",
        "params": {"sessionId": "s1", "update": update}
    })
    .to_string()
}

#[test]
fn rewind_replaces_the_abandoned_prompt_branch() {
    let lines = [
        chunk("user_message_chunk", "one", Some(0)),
        chunk("agent_message_chunk", "first", None),
        chunk("user_message_chunk", "two", Some(1)),
        chunk("agent_message_chunk", "abandoned", None),
        serde_json::json!({
            "timestamp": 1_700_000_001_u64,
            "method": "_x.ai/session/update",
            "params": {"update": {
                "sessionUpdate": "rewind_marker",
                "target_prompt_index": 1
            }}
        })
        .to_string(),
        chunk("user_message_chunk", "replacement", Some(1)),
        chunk("agent_message_chunk", "kept", None),
    ]
    .join("\n");
    let messages = parse_messages(&lines);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["one", "first", "replacement", "kept"]
    );
}

#[test]
fn indexed_history_rejects_later_unmarked_phantom_prompts() {
    let lines = [
        chunk("user_message_chunk", "real", Some(0)),
        chunk("agent_message_chunk", "answer", None),
        chunk("user_message_chunk", "system echo", None),
        chunk("agent_message_chunk", "not main history", None),
    ]
    .join("\n");
    assert_eq!(
        parse_messages(&lines)
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["real", "answer"]
    );
}

#[test]
fn assistant_suffix_does_not_require_the_earlier_user_chunk() {
    let suffix = [
        chunk("agent_message_chunk", "stream", None),
        chunk("agent_thought_chunk", "hidden reasoning", None),
        chunk("agent_message_chunk", "continues", None),
    ]
    .join("\n");
    assert_eq!(parse_assistant_suffix(&suffix), ["stream continues"]);
    assert_eq!(
        parse_messages(&[chunk("user_message_chunk", "prompt", Some(0)), suffix,].join("\n"))[1]
            .text,
        "stream continues"
    );

    let rewound = format!(
        "{}\n{}",
        serde_json::json!({"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"rewind_marker","target_prompt_index":0}}}),
        chunk("agent_message_chunk", "new branch", None)
    );
    assert_eq!(parse_assistant_suffix(&rewound), ["new branch"]);
}

#[test]
fn context_measurement_follows_completed_and_in_progress_turn_order() {
    let completion = serde_json::json!({
        "timestamp": 1_700_000_001_u64,
        "method": "_x.ai/session/update",
        "params": {
            "sessionId": "s1",
            "_meta": {"totalTokens": 9},
            "update": {
                "sessionUpdate": "turn_completed",
                "prompt_id": "p1",
                "stop_reason": "end_turn",
                "usage": {"inputTokens": 100, "outputTokens": 5}
            }
        }
    })
    .to_string();
    let completed = [chunk("user_message_chunk", "one", Some(0)), completion].join("\n");
    assert_eq!(fold(&completed).latest_context_tokens(), Some(100));

    let in_progress = serde_json::json!({
        "timestamp": 1_700_000_002_u64,
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "_meta": {"totalTokens": 175},
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "thinking"}
            }
        }
    })
    .to_string();
    let second_turn = [
        completed,
        chunk("user_message_chunk", "two", Some(1)),
        in_progress,
    ]
    .join("\n");
    assert_eq!(fold(&second_turn).latest_context_tokens(), Some(175));
}

fn permission_event(at: &str, event_type: &str, tool_name: &str) -> String {
    serde_json::json!({
        "ts": at,
        "type": event_type,
        "tool_name": tool_name,
    })
    .to_string()
}

fn permission_wait(lines: &[String]) -> Option<Timestamp> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    std::fs::write(&path, lines.join("\n")).unwrap();
    native_permission_wait(&path)
}

#[test]
fn permission_fold_tracks_unmatched_and_resolved_requests() {
    let first = "2026-07-18T04:21:46.248Z";
    let second = "2026-07-18T04:21:47.248Z";
    assert_eq!(
        permission_wait(&[permission_event(
            first,
            "permission_requested",
            "run_terminal_command",
        )]),
        first.parse().ok()
    );
    assert!(
        permission_wait(&[
            permission_event(first, "permission_requested", "run_terminal_command"),
            permission_event(second, "permission_resolved", "run_terminal_command"),
        ])
        .is_none()
    );
}

#[test]
fn permission_fold_matches_tools_and_repeated_requests_in_append_order() {
    let first = "2026-07-18T04:21:46Z";
    let second = "2026-07-18T04:21:47Z";
    let third = "2026-07-18T04:21:48Z";
    assert_eq!(
        permission_wait(&[
            permission_event(first, "permission_requested", "read_file"),
            permission_event(second, "permission_requested", "run_terminal_command"),
            permission_event(third, "permission_resolved", "read_file"),
        ]),
        second.parse().ok()
    );
    assert_eq!(
        permission_wait(&[
            permission_event(first, "permission_requested", "read_file"),
            permission_event(second, "permission_requested", "read_file"),
            permission_event(third, "permission_resolved", "read_file"),
        ]),
        second.parse().ok()
    );
}

#[test]
fn permission_fold_keeps_unmatched_resolutions_inert_at_equal_timestamps() {
    let at = "2026-07-18T04:21:46Z";
    assert_eq!(
        permission_wait(&[
            permission_event(at, "permission_resolved", "read_file"),
            permission_event(at, "permission_requested", "read_file"),
        ]),
        at.parse().ok()
    );
}

#[test]
fn permission_fold_ignores_unrelated_malformed_and_invalid_records() {
    let at = "2026-07-18T04:21:46Z";
    assert_eq!(
        permission_wait(&[
            r#"{"ts":"2026-07-18T04:21:40Z","type":"phase_changed","phase":"permission_prompt"}"#
                .to_owned(),
            "not json".to_owned(),
            permission_event("not-a-timestamp", "permission_requested", "bad_time"),
            permission_event(at, "permission_requested", ""),
            permission_event(at, "permission_requested", "read_file"),
            permission_event("not-a-timestamp", "permission_resolved", "read_file"),
        ]),
        at.parse().ok()
    );
}

#[test]
fn permission_tail_accepts_a_complete_final_record_and_excludes_a_torn_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let at = "2026-07-18T04:21:46Z";
    let request = permission_event(at, "permission_requested", "read_file");
    std::fs::write(&path, &request).unwrap();
    assert_eq!(native_permission_wait(&path), at.parse().ok());

    let resolved = permission_event(at, "permission_resolved", "read_file");
    std::fs::write(
        &path,
        format!("{request}\n{resolved}\n{{\"ts\":\"{at}\",\"type\":\"permission_requested\""),
    )
    .unwrap();
    assert_eq!(
        read_transcript_tail(&path).as_deref(),
        Some(format!("{request}\n{resolved}\n").as_str())
    );
    assert!(native_permission_wait(&path).is_none());
}

#[test]
fn combined_stat_changes_when_only_events_change() {
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join("updates.jsonl");
    let events = dir.path().join("events.jsonl");
    std::fs::write(&updates, "{}\n").unwrap();

    let absent = combined_stat(&updates, None).unwrap();
    std::fs::write(&events, "").unwrap();
    let created = combined_stat(&updates, Some(&events)).unwrap();
    assert_ne!(created, absent);

    std::fs::write(
        &events,
        permission_event("2026-07-18T04:21:46Z", "permission_requested", "read_file"),
    )
    .unwrap();
    let appended = combined_stat(&updates, Some(&events)).unwrap();
    assert_ne!(appended, created);

    std::fs::remove_file(&events).unwrap();
    assert_eq!(combined_stat(&updates, None), Some(absent));
}
