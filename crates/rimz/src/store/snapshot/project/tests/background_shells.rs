use super::*;

#[test]
fn registration_clears_shells_before_applying_its_report() {
    let shell = |id| json!({"id": id, "command": "sleep 30", "started_at": epoch()});
    let events = [
        lifecycle(
            0,
            json!({"signal": {"signal": "registered"}, "background_shells": {"kind": "started", "shell": shell("old")}}),
        ),
        lifecycle(
            1,
            json!({"signal": {"signal": "registered"}, "background_shells": {"kind": "started", "shell": shell("new")}}),
        ),
    ];
    assert_eq!(
        ids(&reduce_agent_states(&events[..1])[0]),
        vec![("old", epoch().as_second())]
    );
    assert_eq!(
        ids(&reduce_agent_states(&events)[0]),
        vec![("new", epoch().as_second())]
    );
}

#[test]
fn background_script_reports_fold_carry_and_clear() {
    let started = |id: &str, at: i64| json!({"kind": "started", "script": {"id": id, "description": "long sleep", "started_at": Timestamp::from_second(at).unwrap()}});
    let scripts = |events: &[EventEnvelope]| -> Vec<(String, i64)> {
        reduce_agent_states(events)[0]
            .background_scripts
            .iter()
            .map(|script| (script.id.clone(), script.started_at.as_second()))
            .collect()
    };
    let owned = |pairs: &[(&str, i64)]| -> Vec<(String, i64)> {
        pairs
            .iter()
            .map(|(id, at)| ((*id).to_owned(), *at))
            .collect()
    };
    let tool = json!({"signal": "tool_used", "mutates": false, "edits": false, "name": "REPL"});
    let parked = json!({"signal": "turn_ended", "errored": false, "parked_on_background": true});
    let mut events = vec![
        lifecycle(0, json!({"signal": {"signal": "registered"}})),
        lifecycle(
            1,
            json!({"signal": tool, "background_scripts": started("a", 100)}),
        ),
        lifecycle(
            2,
            json!({"signal": tool, "background_scripts": started("a", 150)}),
        ),
        lifecycle(
            3,
            json!({"signal": tool, "background_scripts": started("b", 200)}),
        ),
        lifecycle(4, json!({"signal": parked})),
    ];
    assert_eq!(scripts(&events), owned(&[("a", 100), ("b", 200)]));

    events.push(lifecycle(
        5,
        json!({"signal": tool, "background_scripts": {"kind": "finished", "id": "a"}}),
    ));
    assert_eq!(scripts(&events), owned(&[("b", 200)]));

    events.push(lifecycle(
        6,
        json!({"signal": parked, "background_scripts": {"kind": "none_running"}}),
    ));
    assert!(scripts(&events).is_empty());

    events.push(lifecycle(
        7,
        json!({"signal": tool, "background_scripts": started("c", 300)}),
    ));
    assert_eq!(scripts(&events), owned(&[("c", 300)]));
    events.push(lifecycle(8, json!({"signal": {"signal": "ended"}})));
    assert!(scripts(&events).is_empty());

    let registered = [
        lifecycle(
            0,
            json!({"signal": tool, "background_scripts": started("old", 100)}),
        ),
        lifecycle(
            1,
            json!({"signal": {"signal": "registered"}, "background_scripts": started("new", 200)}),
        ),
    ];
    assert_eq!(scripts(&registered), owned(&[("new", 200)]));
}

fn lifecycle(secs: i64, params: serde_json::Value) -> EventEnvelope {
    let mut params = params;
    params["agent_id"] = json!("sess-1");
    raw_lifecycle_at("claude", secs, params)
}

fn ids(state: &AgentState) -> Vec<(&str, i64)> {
    state
        .background_shells
        .iter()
        .map(|shell| (shell.id.as_str(), shell.started_at.as_second()))
        .collect()
}

#[test]
fn background_shell_reports_fold_carry_and_clear_at_session_end() {
    let started = |id: &str, at: i64| json!({"id": id, "command": "sleep 30", "started_at": Timestamp::from_second(at).unwrap()});
    let mut events = vec![
        lifecycle(0, json!({"signal": {"signal": "registered"}})),
        lifecycle(
            1,
            json!({
                "signal": {"signal": "tool_used", "mutates": true, "edits": false, "name": "Bash"},
                "background_shells": {"kind": "started", "shell": started("a", 100)},
            }),
        ),
        lifecycle(
            2,
            json!({
                "signal": {"signal": "turn_ended", "errored": false, "parked_on_background": true},
                "background_shells": {"kind": "snapshot", "shells": [started("a", 200), started("b", 200)]},
            }),
        ),
        lifecycle(3, json!({"signal": {"signal": "turn_started"}})),
    ];
    let agents = reduce_agent_states(&events);
    assert_eq!(ids(&agents[0]), vec![("a", 100), ("b", 200)]);

    events.push(lifecycle(
        4,
        json!({
            "signal": {"signal": "turn_started"},
            "background_shells": {"kind": "finished", "ids": ["a"]},
        }),
    ));
    let agents = reduce_agent_states(&events);
    assert_eq!(ids(&agents[0]), vec![("b", 200)]);

    events.push(lifecycle(5, json!({"signal": {"signal": "ended"}})));
    let agents = reduce_agent_states(&events);
    assert!(agents[0].background_shells.is_empty());
}

#[test]
fn antigravity_shell_hooks_fold_across_parked_stop_kill_finish_and_idle_repair() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let conv = "0c100c27-850f-4226-b07a-72701c1f9d19";
    let task2 = format!("{conv}/task-2");
    let task4 = format!("{conv}/task-4");
    let t2 = "2026-11-24T15:06:23Z";
    let t4 = "2026-11-24T15:06:30Z";
    let transcript = home
        .join("brain")
        .join(conv)
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let transcript_rows = [
        json!({
            "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE",
            "created_at": "2026-11-24T15:06:12Z", "content": "start tasks"
        }),
        json!({
            "step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "RUNNING",
            "created_at": t2,
            "content": format!("Created At: {t2}\nTool is running as a background task with task id: {task2}\nTask Description: sleep 240 && echo AAA\n")
        }),
        json!({
            "step_index": 4, "source": "MODEL", "type": "GENERIC", "status": "RUNNING",
            "created_at": t4,
            "content": format!("Created At: {t4}\nTool is running as a background task with task id: {task4}\nTask Description: sleep 900 && echo BBB\n")
        }),
    ];
    std::fs::write(
        &transcript,
        transcript_rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let adapter = crate::agents::definition_by_kind("antigravity").expect("antigravity adapter");
    let login_env = BTreeMap::from([(
        "RIMZ_ANTIGRAVITY_HOME".to_owned(),
        home.display().to_string(),
    )]);
    let decode_event = |secs: i64, event: &str, payload: serde_json::Value| {
        let obs = adapter
            .decode_hook_in(event, &payload, &login_env)
            .expect("decode hook")
            .lifecycle()
            .cloned()
            .expect("lifecycle observation");
        let mut envelope =
            EventEnvelope::agent_lifecycle(workspace(), "session", "antigravity", event, &obs);
        envelope.timestamp = Timestamp::from_second(epoch().as_second() + secs).unwrap();
        envelope
    };

    let mut events = vec![
        decode_event(
            0,
            "PreInvocation",
            json!({
                "conversationId": conv,
                "workspacePaths": ["/workspace/project"],
                "transcriptPath": transcript.display().to_string(),
                "invocationNum": 0,
            }),
        ),
        decode_event(
            1,
            "Stop",
            json!({
                "conversationId": conv,
                "workspacePaths": ["/workspace/project"],
                "transcriptPath": transcript.display().to_string(),
                "fullyIdle": false,
                "terminationReason": "NO_TOOL_CALL",
            }),
        ),
    ];
    let agents = reduce_agent_states(&events);
    assert_eq!(agents[0].status, AgentStatus::Running);
    assert_eq!(agents[0].phase, TurnPhase::Parked);
    assert_eq!(
        ids(&agents[0]),
        vec![
            (task2.as_str(), t2.parse::<Timestamp>().unwrap().as_second()),
            (task4.as_str(), t4.parse::<Timestamp>().unwrap().as_second()),
        ]
    );

    // manage_task kill via PostToolUse:observed drops task-4.
    events.push(decode_event(
        2,
        "PostToolUse:observed",
        json!({
            "conversationId": conv,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": transcript.display().to_string(),
            "stepIdx": 10,
            "error": "",
            "toolCall": { "name": "manage_task", "args": { "Action": "kill", "TaskId": task4 } },
        }),
    ));
    let agents = reduce_agent_states(&events);
    assert_eq!(
        ids(&agents[0]),
        vec![(task2.as_str(), t2.parse::<Timestamp>().unwrap().as_second())]
    );

    // run_command PostToolUse:mutating drops task-2.
    events.push(decode_event(
        3,
        "PostToolUse:mutating",
        json!({
            "conversationId": conv,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": transcript.display().to_string(),
            "stepIdx": 2,
            "error": "",
            "toolCall": { "name": "run_command", "args": { "CommandLine": "sleep 240 && echo AAA" } },
        }),
    ));
    let agents = reduce_agent_states(&events);
    assert!(agents[0].background_shells.is_empty());

    // Missed finish: re-parked Stop lists both again, then fullyIdle: true empties the list.
    events.push(decode_event(
        4,
        "Stop",
        json!({
            "conversationId": conv,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": transcript.display().to_string(),
            "fullyIdle": false,
            "terminationReason": "NO_TOOL_CALL",
        }),
    ));
    assert_eq!(reduce_agent_states(&events)[0].background_shells.len(), 2);
    events.push(decode_event(
        5,
        "Stop",
        json!({
            "conversationId": conv,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": transcript.display().to_string(),
            "fullyIdle": true,
            "terminationReason": "NO_TOOL_CALL",
        }),
    ));
    let agents = reduce_agent_states(&events);
    assert_eq!(agents[0].status, AgentStatus::Success);
    assert!(agents[0].background_shells.is_empty());
}
