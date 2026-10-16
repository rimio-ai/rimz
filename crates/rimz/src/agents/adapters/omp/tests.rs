use super::*;
use crate::agents::lifecycle::{LifecycleSignal, LifecycleState, PriorTurnIds, TurnPhase, step};
use serde_json::json;

fn decode(event: &str, payload: Value) -> HookOutput {
    OmpAdapter.decode_hook(event, &payload).unwrap()
}

fn signal(event: &str, payload: Value) -> Option<LifecycleSignal> {
    decode(event, payload).lifecycle().map(|o| o.signal.clone())
}

fn apply(
    state: Option<&LifecycleState>,
    key: Option<&str>,
    event: &str,
    payload: Value,
) -> LifecycleState {
    let observation = decode(event, payload);
    let Some(observation) = observation.lifecycle() else {
        return state.cloned().unwrap_or(LifecycleState {
            status: AgentStatus::Idle,
            phase: TurnPhase::Reasoning,
            compacting: false,
        });
    };
    step(state, key, PriorTurnIds::default(), &observation.signal).next
}

#[test]
fn registration_and_turn_continuation_follow_the_real_fold() {
    let idle = apply(None, None, "session_start", json!({"session_id":"s"}));
    assert_eq!(idle.status, AgentStatus::Idle);
    let running = apply(
        Some(&idle),
        None,
        "before_agent_start",
        json!({"session_id":"s","prompt":"fix it"}),
    );
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(
        signal("agent_end", json!({"session_id":"s","will_continue":true})),
        None
    );
    for (stop, expected) in [
        ("stop", AgentStatus::Success),
        ("aborted", AgentStatus::Idle),
        ("error", AgentStatus::Failed),
    ] {
        let ended = apply(
            Some(&running),
            None,
            "agent_end",
            json!({"session_id":"s","stop_reason":stop}),
        );
        assert_eq!(ended.status, expected, "{stop}");
    }
}

#[test]
fn shutdown_ends_the_row_and_prompt_is_recorded() {
    let output = decode(
        "before_agent_start",
        json!({"session_id":"s","prompt":"  fix it  ","cwd":"/work"}),
    );
    assert_eq!(
        output.lifecycle().and_then(|o| o.prompt.as_deref()),
        Some("fix it")
    );
    assert_eq!(
        signal("session_shutdown", json!({"session_id":"s"})),
        Some(LifecycleSignal::Ended)
    );
}

#[test]
fn questions_and_permissions_are_keyed_and_headless_is_neutral() {
    let question = json!({"session_id":"s","has_ui":true,"tool_name":"ask","tool_call_id":"q","tool_input":{"questions":[{"id":"route","question":"Which?","multi":true,"options":[{"label":"Safe"},{"label":"Fast"}]}]}});
    let output = decode("tool_call", question.clone());
    assert_eq!(output.questions().len(), 1);
    assert_eq!(output.questions()[0].question, "Which?");
    assert!(output.questions()[0].multi_select);
    assert!(
        matches!(signal("tool_call", question.clone()), Some(LifecycleSignal::AwaitingInput { kind: AskKind::Question, native_key: Some(key), .. }) if key == "q")
    );
    let mut headless = question;
    headless["has_ui"] = json!(false);
    assert_eq!(signal("tool_call", headless), None);
    let request = json!({"session_id":"s","has_ui":true,"tool_call_id":"p","tool_name":"bash","reason":"Allow command?"});
    assert!(
        matches!(signal("tool_approval_requested", request), Some(LifecycleSignal::AwaitingInput { kind: AskKind::Permission, native_key: Some(key), .. }) if key == "p")
    );
    let running = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    let waiting = apply(
        Some(&running),
        None,
        "tool_approval_requested",
        json!({"session_id":"s","has_ui":true,"tool_call_id":"p"}),
    );
    assert_eq!(waiting.status, AgentStatus::Waiting);
    let sibling = apply(
        Some(&waiting),
        Some("p"),
        "tool_execution_end",
        json!({"session_id":"s","tool_name":"read","tool_call_id":"other"}),
    );
    assert_eq!(sibling.status, AgentStatus::Waiting);
    assert_eq!(
        apply(
            Some(&waiting),
            Some("p"),
            "tool_approval_resolved",
            json!({"session_id":"s","tool_call_id":"p"})
        )
        .status,
        AgentStatus::Running
    );
    let headless = apply(
        Some(&running),
        None,
        "tool_approval_requested",
        json!({"has_ui":false}),
    );
    assert_ne!(
        apply(
            Some(&headless),
            None,
            "tool_approval_resolved",
            json!({"approved":false})
        )
        .status,
        AgentStatus::Waiting
    );
}

#[test]
fn passive_blocking_events_and_malformed_payloads_are_silent() {
    insta::allow_duplicates! {
    for event in ["tool_call", "tool_approval_requested"] {
        let output = decode(event, json!({"tool_name":"ask","has_ui":true}));
        insta::assert_snapshot!(format!("{:?}", output.json_reply()), @"None");
        let malformed = decode(event, json!({"session_id":42,"tool_input":[],"has_ui":"yes"}));
        insta::assert_snapshot!(format!("{:?}", malformed.json_reply()), @"None");
    }
    }
}

#[test]
fn compaction_and_tool_classification_are_omp_specific() {
    assert_eq!(
        signal("session_before_compact", json!({})),
        Some(LifecycleSignal::Compacting)
    );
    assert_eq!(
        signal("session_compact", json!({})),
        Some(LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false
        })
    );
    for payload in [
        json!({"aborted":true}),
        json!({"error_message":"failed"}),
        json!({"skipped":true}),
    ] {
        assert_eq!(
            signal("auto_compaction_end", payload),
            Some(LifecycleSignal::CompactionEnded {
                auto: Some(true),
                failed: true
            })
        );
    }
    assert_eq!(signal("auto_compaction_end", json!({})), None);
    assert!(matches!(
        signal("tool_execution_end", json!({"tool_name":"ast_edit"})),
        Some(LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            ..
        })
    ));
    assert!(matches!(
        signal("tool_execution_end", json!({"tool_name":"eval"})),
        Some(LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
            ..
        })
    ));
    for tool in ["security_scan", "task"] {
        assert!(
            matches!(
                signal("tool_execution_end", json!({"tool_name":tool})),
                Some(LifecycleSignal::ToolUsed { mutates: true, .. })
            ),
            "{tool}"
        );
    }
}

#[test]
fn zero_context_is_distinct_from_unknown_and_live_dollars_stay_absent() {
    assert!(
        OmpAdapter
            .observe_context("omp", &json!({"session_id":"s"}))
            .is_none()
    );
    let observation = OmpAdapter.observe_context("omp", &json!({"session_id":"s","context_pct":0,"context_window":32768,"total_tokens":0,"model":"probe","effort":"high","total_cost_usd":100}));
    assert!(observation.is_some());
    let context = observation.unwrap().context;
    assert_eq!(
        context.tokens.as_ref().and_then(|t| t.used_percentage),
        Some(0)
    );
    assert_eq!(context.model_id.as_deref(), Some("probe"));
    assert!(context.cost.is_none());
}

#[test]
fn install_uses_login_home_and_xdg_does_not_move_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = BTreeMap::from([("HOME".into(), dir.path().to_str().unwrap().into())]);
    let managed = OmpAdapter.managed_integration();
    assert!(managed.is_some());
    let managed = managed.unwrap();
    let path = dir.path().join(".omp/agent/extensions/rimz.ts");
    assert!(!managed.installed(&env));
    let preview = managed.preview(&env).unwrap();
    assert_eq!(preview.files[0].candidate, EXTENSION_SOURCE);
    managed.install(&env).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(managed.installed(&env));
    env.insert(
        "XDG_CONFIG_HOME".into(),
        dir.path().join("pinned").to_str().unwrap().into(),
    );
    assert!(managed.installed(&env));
    managed.uninstall(&env).unwrap();
    assert!(!path.exists());
}

#[test]
fn relocated_agent_dirs_are_refused_without_writes() {
    let managed = OmpAdapter.managed_integration();
    assert!(managed.is_some());
    let managed = managed.unwrap();
    for (key, value) in [
        ("PI_CODING_AGENT_DIR", "/x"),
        ("OMP_PROFILE", "work"),
        ("PI_PROFILE", "work"),
        ("PI_CONFIG_DIR", ".other"),
    ] {
        let env = BTreeMap::from([(key.into(), value.into())]);
        let refusal = managed.hooks_disabled(&env);
        assert!(refusal.is_some(), "{key}");
        let Some(HookPreflightErr::HooksDisabled { reason, fix }) = refusal else {
            panic!("expected a hooks-disabled refusal for {key}");
        };
        assert!(reason.contains(key) && reason.contains(value) && reason.contains("~/.omp/agent"));
        assert!(fix.contains("unset") && fix.contains(key));
    }
    for env in [
        BTreeMap::new(),
        BTreeMap::from([("OMP_PROFILE".into(), "".into())]),
        BTreeMap::from([("OMP_PROFILE".into(), " default ".into())]),
        BTreeMap::from([
            ("OMP_PROFILE".into(), "".into()),
            ("PI_PROFILE".into(), "work".into()),
        ]),
    ] {
        assert!(managed.hooks_disabled(&env).is_none());
    }
}

#[test]
fn main_session_spend_counts_messages_and_model_usage_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("2026-06-02T10-00-00-000Z_019e9161-0000-7000-8000-000000000001.jsonl");
    let body = concat!(
        "{\"type\":\"title\",\"title\":\"fixture\"}\n",
        "{\"type\":\"session\",\"id\":\"session-one\",\"cwd\":\"/work\"}\n",
        "{\"type\":\"message\",\"timestamp\":\"2026-06-02T10:00:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"probe\",\"content\":[{\"type\":\"toolCall\",\"name\":\"read\"}],\"usage\":{\"input\":100,\"output\":50,\"cost\":{\"total\":0.42}}}}\n",
        "{\"type\":\"message\",\"timestamp\":\"2026-06-02T10:01:00Z\",\"message\":{\"role\":\"assistant\",\"usage\":{\"output\":10,\"cost\":{\"total\":0.08}}}}\n",
        "{\"type\":\"model_usage\",\"timestamp\":\"2026-06-02T10:02:00Z\",\"purpose\":\"compaction\",\"model\":\"probe\",\"usage\":{\"output\":20,\"cost\":{\"total\":0.12}}}\n",
        "{\"type\":\"compaction\",\"timestamp\":\"2026-06-02T10:02:00Z\",\"usage\":{\"output\":20,\"cost\":{\"total\":99}}}\n",
        "{\"type\":\"branch_summary\",\"timestamp\":\"2026-06-02T10:02:00Z\",\"usage\":{\"output\":20,\"cost\":{\"total\":99}}}\n",
    );
    std::fs::write(&path, body).unwrap();
    let prices = pricing::PriceBook::from_litellm_json("{}");
    let parsed = OmpAdapter.parse_spend(&path, None, &prices);
    assert_eq!(parsed.entries.len(), 3);
    assert!((parsed.entries.iter().map(|e| e.cost_usd).sum::<f64>() - 0.62).abs() < 1e-9);
    assert_eq!(parsed.origin.as_deref(), Some(Path::new("/work")));
    assert_eq!(parsed.entries[0].tool_calls.get("read"), Some(&1));
    let child = dir.path().join("task-123.jsonl");
    std::fs::write(&child, body).unwrap();
    assert!(
        OmpAdapter
            .parse_spend(&child, None, &prices)
            .entries
            .is_empty()
    );
    std::fs::write(
        &path,
        "{\"type\":\"session\",\"id\":\"replacement\",\"cwd\":\"/new\"}\n",
    )
    .unwrap();
    let replaced = OmpAdapter.parse_spend(&path, Some(&parsed.cursor), &prices);
    assert!(matches!(replaced.update, spending::SpendUpdate::Replace));
    assert_eq!(replaced.origin.as_deref(), Some(Path::new("/new")));
}

#[test]
fn transcript_skips_title_and_header_and_preserves_visible_text() {
    let lines = "{\"type\":\"title\",\"title\":\"fixture\"}\n{\"type\":\"session\",\"cwd\":\"/work\"}\n{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"fix it\"}}\n{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hidden\"},{\"type\":\"text\",\"text\":\"Done\"}]}}\n";
    let messages = OmpAdapter.parse_transcript_messages(lines);
    assert_eq!(
        messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["fix it", "Done"]
    );
}

#[test]
fn extension_guards_children_and_never_blocks_tools() {
    assert!(
        EXTENSION_SOURCE
            .lines()
            .next()
            .unwrap()
            .contains("_rimz_managed")
    );
    for marker in [
        "ctx.agent?.kind === \"sub\"",
        "RIMZ_LAUNCH_REMINDERS",
        "delete process.env",
        "[\"hooks\", \"feed\", \"--source\", \"omp\"]",
        "RIMZ_AGENT_PID",
        "will_continue",
        "session_switch",
    ] {
        assert!(EXTENSION_SOURCE.contains(marker), "{marker}");
    }
    assert!(!EXTENSION_SOURCE.contains("block: true"));
    assert!(!EXTENSION_SOURCE.contains("total_cost_usd"));
}

#[test]
fn empty_truncation_replaces_cost_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("2026-06-02T10-00-00-000Z_019e9161-0000-7000-8000-000000000001.jsonl");
    std::fs::write(&path, "{\"type\":\"session\",\"id\":\"original\"}\n").unwrap();
    let prices = pricing::PriceBook::from_litellm_json("{}");
    let parsed = OmpAdapter.parse_spend(&path, None, &prices);
    std::fs::write(&path, "").unwrap();
    let replaced = OmpAdapter.parse_spend(&path, Some(&parsed.cursor), &prices);
    assert!(matches!(replaced.update, spending::SpendUpdate::Replace));
    assert_eq!(replaced.cursor.offset, 0);
}
