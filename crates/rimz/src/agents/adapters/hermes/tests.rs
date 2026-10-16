use super::*;
use crate::agents::lifecycle::{LifecycleState, PriorTurnIds, TurnPhase, step};
use serde_json::{Value, json};

fn decode(event: &str, extra: Value) -> HookOutput {
    HermesAdapter
        .decode_hook(
            event,
            &json!({"hook_event_name":event,"session_id":"parent","cwd":"/repo","extra":extra}),
        )
        .unwrap()
}

fn advance(
    prior: Option<&LifecycleState>,
    key: Option<&str>,
    event: &str,
    extra: Value,
) -> LifecycleState {
    let output = decode(event, extra);
    step(
        prior,
        key,
        PriorTurnIds::default(),
        &output.lifecycle().expect("mapped lifecycle").signal,
    )
    .next
}

#[test]
fn session_start_and_reset_register_without_ending_the_card() {
    for event in ["on_session_start", "on_session_reset"] {
        let output = HermesAdapter
            .decode_hook(
                event,
                &json!({"session_id":"new","extra":{"reason":"new_session"}}),
            )
            .unwrap();
        assert_eq!(output.event_agent_id().unwrap().as_str(), "new");
        assert!(!output.ends_session());
        let observation = output.lifecycle().expect("session registration");
        assert_eq!(observation.signal, LifecycleSignal::Registered);
        let state = step(None, None, PriorTurnIds::default(), &observation.signal).next;
        assert_eq!(state.status, AgentStatus::Idle);
    }
}

#[test]
fn turns_and_tools_reach_the_consumer_state() {
    let running = advance(None, None, "pre_llm_call", json!({}));
    assert_eq!(running.status, AgentStatus::Running);
    for (tool, mutates, edits) in [
        ("write_file", true, true),
        ("patch", true, true),
        ("terminal", true, false),
        ("read_file", false, false),
    ] {
        let output = HermesAdapter
            .decode_hook(
                "post_tool_call",
                &json!({"session_id":"parent","tool_name":tool,"extra":{}}),
            )
            .unwrap();
        assert!(
            matches!(&output.lifecycle().expect("tool").signal, LifecycleSignal::ToolUsed {mutates:m,edits:e,..} if *m==mutates && *e==edits)
        );
        let state = step(
            Some(&running),
            None,
            PriorTurnIds::default(),
            &output.lifecycle().unwrap().signal,
        )
        .next;
        assert_eq!(
            state.phase,
            if edits {
                TurnPhase::Acting
            } else {
                TurnPhase::Reasoning
            }
        );
    }
    for (reason, status) in [
        ("completed", AgentStatus::Success),
        ("failed", AgentStatus::Failed),
        ("interrupted", AgentStatus::Success),
    ] {
        assert_eq!(
            advance(Some(&running), None, "on_session_end", json!({reason:true})).status,
            status
        );
        assert!(!decode("on_session_end", json!({reason:true})).ends_session());
    }
}

#[test]
fn keyed_prompts_survive_tools_and_late_resolution_preserves_settlement() {
    let running = advance(None, None, "pre_llm_call", json!({}));
    for (kind, expected) in [
        ("approval", AskKind::Permission),
        ("sudo", AskKind::Permission),
        ("clarify", AskKind::Question),
        ("future", AskKind::Question),
    ] {
        let output = decode(
            "on_human_input_request",
            json!({"kind":kind,"request_id":"request"}),
        );
        assert!(
            matches!(&output.lifecycle().expect("native prompt").signal, LifecycleSignal::AwaitingInput {kind,..} if *kind==expected)
        );
        let waiting = advance(
            Some(&running),
            None,
            "on_human_input_request",
            json!({"kind":kind,"request_id":"request"}),
        );
        assert_eq!(waiting.status, AgentStatus::Waiting);
        let after_tool = advance(Some(&waiting), Some("request"), "post_tool_call", json!({}));
        assert_eq!(after_tool.status, AgentStatus::Waiting);
        let resolved = advance(
            Some(&after_tool),
            Some("request"),
            "on_human_input_resolved",
            json!({"request_id":"request"}),
        );
        assert_eq!(resolved.status, AgentStatus::Running);
        let settled = advance(
            Some(&waiting),
            Some("request"),
            "on_session_end",
            json!({"completed":true}),
        );
        assert_eq!(
            advance(
                Some(&settled),
                None,
                "on_human_input_resolved",
                json!({"request_id":"request"})
            ),
            settled
        );
    }
}

#[test]
fn every_installed_event_has_empty_stdout() {
    for event in HERMES_HOOKS.iter().map(|hook| hook.event) {
        let output = decode(
            event,
            json!({"request_id":"r","kind":"approval","child_session_id":"child"}),
        );
        insta::allow_duplicates! {
            insta::assert_snapshot!(match output.reply() {HookReply::Silent => "", HookReply::Json(_) => "unexpected directive"}, @"");
        }
    }
}

#[test]
fn malformed_payloads_are_neutral_and_empty_identity_is_absent() {
    let missing = HermesAdapter.decode_hook("", &json!({})).unwrap();
    let malformed = HermesAdapter
        .decode_hook(
            "on_human_input_request",
            &json!({"session_id":"","extra":42}),
        )
        .unwrap();
    insta::assert_debug_snapshot!((missing.class(), missing.reply(), malformed.event_agent_id(), malformed.ask_kind(), malformed.reply()), @r"
    (
        Unknown,
        Silent,
        None,
        None,
        Silent,
    )
    ");
}

#[test]
fn request_context_is_occupancy_not_cumulative_usage() {
    for (extra, used, capacity) in [
        (
            json!({"model":"model","usage":{"prompt_tokens":120},"context_length":1000,"response":"private"}),
            Some(120),
            Some(1000),
        ),
        (
            json!({"model":"model","context_length":1000}),
            None,
            Some(1000),
        ),
        (
            json!({"model":"model","usage":{"prompt_tokens":40}}),
            Some(40),
            None,
        ),
        (
            json!({"model":"model","usage":{"prompt_tokens":25},"context_length":2000}),
            Some(25),
            Some(2000),
        ),
    ] {
        let output = decode("post_api_request", extra);
        let context = &output.observed_context().expect("request context").context;
        assert_eq!(context.model_id.as_deref(), Some("model"));
        let tokens = context.tokens.as_ref().unwrap();
        assert_eq!(tokens.current_context_tokens, used);
        assert_eq!(tokens.context_window_size, capacity);
        assert!(context.cost.is_none());
        assert!(!serde_json::to_string(context).unwrap().contains("private"));
    }
}

#[test]
fn child_hooks_pair_on_the_child_session() {
    let start = decode(
        "subagent_start",
        json!({"child_session_id":"child","child_subagent_id":"start-only","child_goal":"task"}),
    );
    let stop = decode(
        "subagent_stop",
        json!({"child_session_id":"child","child_status":"completed"}),
    );
    let start = start.lifecycle().expect("child start");
    let stop = stop.lifecycle().expect("child stop");
    assert_eq!(start.agent_id, stop.agent_id);
    assert_eq!(start.agent_id.as_ref().unwrap().as_str(), "child");
    assert_eq!(start.parent_agent_id.as_ref().unwrap().as_str(), "parent");
    let running = step(None, None, PriorTurnIds::default(), &start.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(
        step(Some(&running), None, PriorTurnIds::default(), &stop.signal)
            .next
            .status,
        AgentStatus::Success
    );
    assert!(decode("subagent_stop", json!({})).lifecycle().is_none());
    for status in ["failed", "error", "timeout"] {
        let stopped = decode(
            "subagent_stop",
            json!({"child_session_id":"child","child_status":status}),
        );
        assert_eq!(
            step(
                Some(&running),
                None,
                PriorTurnIds::default(),
                &stopped.lifecycle().unwrap().signal
            )
            .next
            .status,
            AgentStatus::Failed,
            "{status}"
        );
    }
}

#[test]
fn launch_and_presets_match_the_classic_parser() {
    insta::assert_json_snapshot!((HermesAdapter.launch_command(&[],Some("-x test")),HermesAdapter.resume_command("session",std::path::Path::new("/repo")),HermesAdapter.spec().launch.permission_args(PermissionMode::Yolo),HermesAdapter.spec().launch.compact_command("focus")), @r#"
    [
      [
        "hermes",
        "chat",
        "--cli",
        "--query=-x test"
      ],
      [
        "hermes",
        "chat",
        "--cli",
        "--resume",
        "session"
      ],
      [
        "--yolo"
      ],
      "/compress focus"
    ]
    "#);
    let preset = LaunchPreset {
        model: Some("model".into()),
        effort: Some("high".into()),
        ..LaunchPreset::default()
    };
    assert_eq!(
        HermesAdapter.spec().render_preset(&preset).unwrap(),
        ["--model", "model", "--reasoning", "high"]
    );
    let bad = LaunchPreset {
        effort: Some("bogus".into()),
        ..LaunchPreset::default()
    };
    assert!(HermesAdapter.spec().render_preset(&bad).is_err());
}

#[test]
fn config_home_is_explicit_xdg_independent_and_named_accounts_are_refused() {
    let env = std::collections::BTreeMap::from([
        ("HOME".into(), "/fixture/home".into()),
        ("HERMES_HOME".into(), "/selected".into()),
        ("XDG_CONFIG_HOME".into(), "/elsewhere".into()),
    ]);
    assert_eq!(
        HermesAdapter.config_home(&env),
        Some(std::path::PathBuf::from("/selected"))
    );
    assert_eq!(
        HermesAdapter.skills_home(&env),
        Some(std::path::PathBuf::from("/selected/skills"))
    );
    assert!(matches!(
        crate::agents::login::ProviderLogin::named(
            crate::ids::AgentKind::new_unchecked("hermes"),
            "work".parse().unwrap(),
            "/named".into()
        ),
        Err(crate::agents::login::LoginErr::Unsupported { .. })
    ));
}
