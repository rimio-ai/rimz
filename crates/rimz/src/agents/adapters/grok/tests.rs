use serde_json::json;

use super::*;

use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output, hook_signal};
use crate::agents::{AgentHookClass, ClassificationSample, ClassifiedHook};

#[test]
fn named_login_scopes_spending_and_session_transcripts() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let named = temp.path().join("work");
    let native_file = home.join(".grok/sessions/project/shared-session/updates.jsonl");
    let named_file = named.join("sessions/project/shared-session/updates.jsonl");
    for path in [&native_file, &named_file] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let ambient = BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())]);
    let login_env = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("grok"),
        "work".parse().unwrap(),
        named,
    )
    .unwrap()
    .env(&ambient);
    let files = GrokAdapter
        .spending_sources(&login_env)
        .into_iter()
        .flat_map(|source| source.complete_files())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![named_file.clone()]);
    assert_eq!(
        GrokAdapter.session_transcript("shared-session", Some(&native_file), &login_env),
        Some(named_file.clone()),
    );
    let pricing = temp.path().join("pricing.json");
    for (current, prior) in [(Some(&named_file), None), (None, Some(&named_file))] {
        let ctx = LocalContextRefreshCtx {
            login_env: &login_env,
            agent_id: "shared-session",
            model_hint: None,
            prior_session_name: None,
            current_transcript_path: current.and_then(|path| path.to_str()),
            prior_transcript_path: prior.and_then(|path| path.to_str()),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: &pricing,
        };
        assert_eq!(
            GrokAdapter
                .local_context_refresh(RefreshTrigger::Tick, &ctx)
                .unwrap()
                .transcript_path,
            Some(named_file.to_string_lossy().into_owned()),
        );
    }
}

#[test]
fn login_home_auth_file_decides_account() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("auth.json"),
        r#"{"work":{"auth_mode":"oidc","key":"token","user_id":"work-user"}}"#,
    )
    .unwrap();
    let env = BTreeMap::from([(
        "GROK_HOME".to_owned(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    let crate::agents::account::AccountProbe::Found(account) = GrokAdapter.probe_account(&env)
    else {
        panic!("login's auth file must be used");
    };
    assert_eq!(account.account_id.as_deref(), Some("work-user"));
}

#[test]
fn login_auth_path_overrides_home_auth_file() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("auth.json"), "malformed").unwrap();
    let path = temp.path().join("override.json");
    std::fs::write(
        &path,
        r#"{"work":{"auth_mode":"external","key":"token","user_id":"override-user"}}"#,
    )
    .unwrap();
    let env = BTreeMap::from([
        (
            "GROK_HOME".to_owned(),
            temp.path().to_string_lossy().into_owned(),
        ),
        (
            "GROK_AUTH_PATH".to_owned(),
            path.to_string_lossy().into_owned(),
        ),
    ]);
    let crate::agents::account::AccountProbe::Found(account) = GrokAdapter.probe_account(&env)
    else {
        panic!("login's auth path must win");
    };
    assert_eq!(account.account_id.as_deref(), Some("override-user"));
}

#[test]
fn login_api_keys_count_without_an_auth_file() {
    for key in ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"] {
        let env = BTreeMap::from([(key.to_owned(), "login-key".to_owned())]);
        let crate::agents::account::AccountProbe::Found(account) = GrokAdapter.probe_account(&env)
        else {
            panic!("{key} in the login env must count");
        };
        assert_eq!(account.plan.as_deref(), Some("API key"));
    }
}

#[test]
fn login_flag_disables_stored_and_environment_api_keys() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("auth.json"),
        r#"{"work":{"auth_mode":"api_key","key":"token"}}"#,
    )
    .unwrap();
    let mut env = BTreeMap::from([
        (
            "GROK_HOME".to_owned(),
            temp.path().to_string_lossy().into_owned(),
        ),
        ("XAI_API_KEY".to_owned(), "login-key".to_owned()),
    ]);
    assert!(matches!(
        GrokAdapter.probe_account(&env),
        crate::agents::account::AccountProbe::Found(_)
    ));
    env.insert("GROK_DISABLE_API_KEY_AUTH".to_owned(), " true ".to_owned());
    assert!(matches!(
        GrokAdapter.probe_account(&env),
        crate::agents::account::AccountProbe::LoggedOut
    ));
}

#[test]
fn native_hook_envelope_recognition_is_owned_by_grok() {
    let envelope = json!({"hookEventName":"session_start", "workspaceRoot":"/project"});
    for definition in crate::agents::registry::all_definitions() {
        let is_grok = definition.spec().kind == "grok";
        assert_eq!(definition.recognizes_hook_payload(&envelope), is_grok);
        assert_eq!(definition.is_foreign_hook_payload(&envelope), !is_grok);
        for payload in [
            json!({"hookEventName":"session_start"}),
            json!({"workspaceRoot":"/project"}),
            json!({"hook_event_name":"SessionStart", "cwd":"/project"}),
        ] {
            assert!(!definition.recognizes_hook_payload(&payload));
            assert!(!definition.is_foreign_hook_payload(&payload));
        }
    }
}

#[test]
fn deadline_context_reply_matches_native_post_tool_contract() {
    let (post, other) = crate::agents::testkit::deadline_context_replies("grok", "PostToolUse");
    insta::assert_json_snapshot!(post, @r#"
    {
      "hookSpecificOutput": {
        "additionalContext": "deadline context"
      }
    }
    "#);
    insta::assert_json_snapshot!(other, @"null");
}

pub(super) fn classification_corpus() -> Vec<ClassificationSample> {
    let mut samples = crate::agents::hook_types::catalog_classification_corpus(GROK_HOOKS);
    samples.extend([
        sample(
            "Notification",
            json!({"sessionId":"s1", "notificationType":"idle_prompt", "timestamp":"2026-01-01T00:00:00Z"}),
            AgentHookClass::Lifecycle,
            None,
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Plan approval requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Diff review requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Permission),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"elicitation_dialog","message":"User question requested"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Question),
            "Notification",
        ),
        sample(
            "notification",
            json!({"notificationType":"permission_prompt","message":"Tool Permission Requested"}),
            AgentHookClass::Lifecycle,
            None,
            "Notification",
        ),
        sample(
            "postToolUse",
            json!({"toolName":"run_terminal_command"}),
            AgentHookClass::Lifecycle,
            None,
            "PostToolUse",
        ),
        sample(
            "session_start",
            json!({"sessionId":"s1"}),
            AgentHookClass::Lifecycle,
            None,
            "SessionStart",
        ),
    ]);
    samples
}

fn sample(
    event_name: &'static str,
    payload: Value,
    class: AgentHookClass,
    ask_kind: Option<AskKind>,
    canonical: &str,
) -> ClassificationSample {
    ClassificationSample {
        event_name,
        payload,
        expected: ClassifiedHook {
            class,
            ask_kind,
            event_name: canonical.to_owned(),
        },
    }
}

#[test]
fn launch_keeps_streaming_flags_out_of_interactive_sessions() {
    let adapter = GrokAdapter;
    assert_eq!(
        adapter.launch_command(&[], None),
        Some(vec!["grok".to_owned()])
    );
    assert_eq!(
        adapter.launch_command(&[], Some("")),
        Some(vec!["grok".to_owned()])
    );
    assert_eq!(
        adapter.launch_command(&["--max-turns".to_owned(), "3".to_owned()], Some("ping")),
        Some(vec![
            "grok".to_owned(),
            "--max-turns".to_owned(),
            "3".to_owned(),
            "-p".to_owned(),
            "ping".to_owned(),
            "--output-format".to_owned(),
            "streaming-json".to_owned(),
        ])
    );
    assert_eq!(
        adapter.resume_command("session-1", Path::new("/tmp")),
        Some(vec![
            "grok".to_owned(),
            "--resume".to_owned(),
            "session-1".to_owned(),
        ])
    );
    let id = "3b129972-4ff9-46dc-9539-aef06169231b";
    for flags in [
        format!("-r {id}"),
        format!("--resume {id}"),
        format!("--resume={id}"),
    ] {
        assert_eq!(
            resumed_session_id(&format!("/usr/local/bin/grok {flags}")).as_deref(),
            Some(id)
        );
    }
    for flags in [
        "--resume",
        "--resume --yolo",
        "--resume \"my title\"",
        "--resume=my-title",
    ] {
        assert!(resumed_session_id(&format!("grok {flags}")).is_none());
    }
    for command in [
        format!("grok --resume {id} --fork-session"),
        format!("grok --fork-session --resume={id}"),
        format!("other --resume {id}"),
    ] {
        assert!(resumed_session_id(&command).is_none());
    }
}

#[test]
fn lifecycle_maps_exact_asks_and_ignores_shutdown() {
    let adapter = GrokAdapter;
    let ask = hook_lifecycle(
        &adapter,
        "Notification",
        &json!({
            "sessionId":"s1",
            "notificationType":"permission_prompt",
            "message":"Plan approval requested"
        }),
    );
    assert!(matches!(
        ask.signal,
        LifecycleSignal::AwaitingInput {
            kind: AskKind::PlanApproval,
            ..
        }
    ));
    assert!(
        hook_observation(
            &adapter,
            "Stop",
            &json!({"sessionId":"s1","reason":"shutdown"})
        )
        .is_none()
    );
    assert!(hook_observation(&adapter, "SubagentStart", &json!({"sessionId":"s1"})).is_none());
    let decoded = hook_output(&adapter, "permission_denied", &json!({}));
    assert_eq!(decoded.class(), AgentHookClass::Unknown);
    assert_eq!(decoded.ask_kind(), None);
    assert_eq!(decoded.event_name(), "PermissionDenied");
    assert_eq!(
        hook_output(&adapter, "future_event", &json!({})).event_name(),
        "FutureEvent"
    );
}

#[test]
fn turn_end_reports_settle_the_turn_through_the_state_machine() {
    use crate::agents::AgentStatus;
    use crate::agents::lifecycle::{LifecycleState, PriorTurnIds, TurnPhase, step};

    let adapter = GrokAdapter;
    let running = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    for (event, payload, status) in [
        (
            "stop",
            json!({"sessionId":"s1","reason":"end_turn","lastAssistantMessage":"done"}),
            AgentStatus::Success,
        ),
        (
            "stop_failure",
            json!({"sessionId":"s1","error":"invalid_request","errorDetails":"HTTP 400"}),
            AgentStatus::Failed,
        ),
        (
            "stop_cancelled",
            json!({"sessionId":"s1","reason":"user_interrupt","cancelledBy":"user"}),
            AgentStatus::Idle,
        ),
        (
            "stop_cancelled",
            json!({"sessionId":"s1","reason":"max_turns","cancelledBy":"runtime"}),
            AgentStatus::Idle,
        ),
    ] {
        let signal = hook_signal(&adapter, event, &payload);
        assert_eq!(
            step(Some(&running), None, PriorTurnIds::default(), &signal)
                .next
                .status,
            status,
            "{event} {payload}"
        );
    }

    let prompt = |event, payload: Value| hook_signal(&adapter, event, &payload);
    assert_eq!(
        prompt(
            "UserPromptSubmit",
            json!({"sessionId":"s1","promptId":"p2","prompt":"next"})
        ),
        LifecycleSignal::TurnStarted {
            turn_id: Some("p2".to_owned())
        }
    );
    for (event, payload) in [
        (
            "Stop",
            json!({"sessionId":"s1","promptId":"p1","reason":"end_turn"}),
        ),
        (
            "StopFailure",
            json!({"sessionId":"s1","promptId":"p1","error":"unknown"}),
        ),
        (
            "StopCancelled",
            json!({"sessionId":"s1","promptId":"p1","reason":"user_interrupt"}),
        ),
    ] {
        let late = prompt(event, payload);
        let ids = PriorTurnIds {
            started: Some("p2"),
            superseded: Some("p1"),
            closed: None,
        };
        assert_eq!(
            step(Some(&running), None, ids, &late).next,
            running,
            "{event} for p1 after p2 started: {late:?}"
        );
    }

    let failure = hook_output(
        &adapter,
        "StopFailure",
        &json!({"sessionId":"s1","error":"rate_limit","errorDetails":"HTTP 429","lastAssistantMessage":"partial"}),
    );
    let error = failure.turn_error().cloned().unwrap();
    assert_eq!(error.class, TurnErrorClass::PausedRateLimit);
    assert_eq!(error.label.as_deref(), Some("HTTP 429"));
    assert_eq!(failure.final_message(), Some("partial"));
    assert_eq!(
        hook_output(
            &adapter,
            "StopFailure",
            &json!({"sessionId":"s1","error":"server_error"})
        )
        .turn_error()
        .map(|error| error.class),
        Some(TurnErrorClass::PausedOverloaded)
    );
    assert_eq!(
        hook_output(
            &adapter,
            "Stop",
            &json!({"sessionId":"s1","reason":"end_turn","lastAssistantMessage":"  done  "})
        )
        .final_message(),
        Some("done")
    );
    assert!(
        hook_observation(
            &adapter,
            "Stop",
            &json!({"sessionId":"s1","reason":"channel_closed","stopHookActive":false})
        )
        .is_none()
    );
}

#[test]
fn child_session_hooks_resolve_the_child_without_claiming_a_parent() {
    let adapter = GrokAdapter;
    let gate = hook_lifecycle(
        &adapter,
        "SubagentStop",
        &json!({"sessionId":"child-1","subagentId":"child-1","subagentType":"explore","phase":"gate","lastAssistantMessage":"found it"}),
    );
    assert_eq!(gate.agent_id.as_deref(), Some("child-1"));
    assert_eq!(gate.parent_agent_id, None);
    assert_eq!(gate.signal, TURN_COMPLETED);

    let failure = hook_output(
        &adapter,
        "StopFailure",
        &json!({"sessionId":"child-1","subagentType":"explore","error":"server_error","lastAssistantMessage":"partial"}),
    );
    assert!(matches!(
        failure.lifecycle().map(|observation| &observation.signal),
        Some(LifecycleSignal::TurnEnded { errored: true, .. })
    ));
    assert_eq!(failure.final_message(), None);

    for (cancelled_by, errored) in [("user", false), ("runtime", true)] {
        assert_eq!(
            hook_signal(
                &adapter,
                "StopCancelled",
                &json!({"sessionId":"child-1","subagentType":"explore","promptId":"child-prompt","reason":"no_progress","cancelledBy":cancelled_by})
            ),
            LifecycleSignal::TurnEnded {
                errored,
                parked_on_background: false,
                turn_id: None,
            }
        );
    }
    assert!(
        hook_observation(
            &adapter,
            "SessionEnd",
            &json!({"sessionId":"child-1","subagentType":"explore","reason":"shutdown"})
        )
        .is_none()
    );
    assert_eq!(
        hook_signal(
            &adapter,
            "SessionEnd",
            &json!({"sessionId":"s1","reason":"shutdown"})
        ),
        LifecycleSignal::Ended
    );
}

#[test]
fn launch_appends_reminders_through_rules_and_compacts_with_guidance() {
    let adapter = GrokAdapter;
    assert_eq!(
        adapter.append_system_text_channel(),
        Some(SystemTextChannel::TextFlag {
            flags: vec!["--rules".to_owned(), "--append-system-prompt".to_owned()],
        })
    );
    assert_eq!(
        adapter
            .spec()
            .launch
            .compact_command
            .map(|command| command.instruction),
        Some(super::super::CompactInstruction::Trailing)
    );
}

#[test]
fn lifecycle_classifies_tool_effects_and_compaction_source() {
    let adapter = GrokAdapter;
    for (tool, mutates, edits) in [
        ("apply_patch", true, true),
        ("run_terminal_command", true, false),
        ("read", false, false),
    ] {
        let observation = hook_lifecycle(
            &adapter,
            "PostToolUse",
            &json!({"sessionId":"s1","toolName":tool}),
        );
        assert_eq!(
            observation.signal,
            LifecycleSignal::ToolUsed {
                mutates,
                edits,
                name: None,
                native_key: None,
                turn_id: None,
            }
        );
    }
    assert_eq!(
        hook_signal(
            &adapter,
            "PostToolUseFailure",
            &json!({"sessionId":"s1","toolName":"apply_patch","error":"denied"})
        ),
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
            turn_id: None,
        }
    );
    assert_eq!(
        hook_signal(
            &adapter,
            "PostCompact",
            &json!({"sessionId":"s1","source":"manual"})
        ),
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: false,
        }
    );
}

#[test]
fn managed_catalog_is_passive_and_excludes_pre_tool_use() {
    assert_eq!(install::catalog().len(), 13);
    assert!(install::catalog().iter().all(|hook| !hook.synchronous));
    assert!(
        !install::catalog()
            .iter()
            .any(|hook| hook.event == "PreToolUse")
    );
    assert!(
        hook_output(&GrokAdapter, "Notification", &Value::Null)
            .json_reply()
            .cloned()
            .is_none()
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz.json");
    let preview = install::MANAGED_SOURCE.preview_at(&path).unwrap();
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(RIMZ_HOOK_COMMAND, "rimz hooks feed --source grok");
    for hook in install::catalog() {
        let entries = candidate["hooks"][hook.event].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let handler = &entries[0]["hooks"][0];
        assert_eq!(handler["command"], RIMZ_HOOK_COMMAND);
        assert!(!handler["command"].as_str().unwrap().contains('$'));
    }
}

#[test]
fn completed_usage_preserves_occupancy_and_estimates_missing_cost() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let updates = session.join("updates.jsonl");
    let pricing = dir.path().join("pricing-cache.json");
    std::fs::create_dir(&session).unwrap();
    let lines = [
        json!({
            "timestamp": 1_700_000_000_u64,
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {"type": "text", "text": "ping"},
                    "_meta": {"promptIndex": 0}
                }
            }
        }),
        json!({
            "timestamp": 1_700_000_001_u64,
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "thinking"}
                },
                "_meta": {"totalTokens": 4_000}
            }
        }),
        json!({
            "timestamp": 1_700_000_002_u64,
            "method": "_x.ai/session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "prompt_id": "prompt-1",
                    "stop_reason": "end_turn",
                    "usage": {
                        "inputTokens": 17_869,
                        "modelCalls": 2,
                        "cachedReadTokens": 0,
                        "outputTokens": 32,
                        "totalTokens": 17_901,
                        "modelUsage": {
                            "grok-4.5-build-free": {
                                "inputTokens": 17_869,
                                "cachedReadTokens": 0,
                                "outputTokens": 32,
                                "totalTokens": 17_901
                            }
                        }
                    }
                }
            }
        }),
    ]
    .map(|row| row.to_string())
    .join("\n");
    std::fs::write(&updates, format!("{lines}\n")).unwrap();
    std::fs::write(
        session.join("signals.json"),
        r#"{"contextTokensUsed":3000,"contextWindowTokens":10000}"#,
    )
    .unwrap();
    crate::agents::PriceBook::write_fixture_cache(&pricing);
    let ctx = LocalContextRefreshCtx {
        login_env: &std::collections::BTreeMap::new(),
        agent_id: "session-1",
        model_hint: Some("grok-4.5"),
        prior_session_name: None,
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    };

    let refresh = refresh_resolved_context(&updates, &ctx).unwrap();
    assert_eq!(refresh.context.settle, FieldPatch::Keep);
    let tokens = refresh.context.tokens.clone().into_value().unwrap();
    assert_eq!(tokens.current_context_tokens, Some(4_000));
    assert_eq!(tokens.used_percentage, Some(40));
    assert!(tokens.current_usage.is_none());
    assert!(
        refresh
            .context
            .cost
            .as_set()
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0)
    );

    let mut context = crate::agents::AgentContext::default();
    refresh.context.apply(&mut context, GrokAdapter.spec());
    let rewind = json!({
        "method":"_x.ai/session/update",
        "params":{"update":{"sessionUpdate":"rewind_marker", "target_prompt_index":0}}
    });
    std::fs::write(&updates, format!("{lines}\n{rewind}\n")).unwrap();
    let refresh = refresh_resolved_context(&updates, &ctx).unwrap();
    refresh.context.apply(&mut context, GrokAdapter.spec());
    let tokens = context.tokens.unwrap();
    assert_eq!(tokens.current_context_tokens, None);
    assert_eq!(tokens.used_percentage, None);
}

#[test]
fn idle_notification_settles_only_activity_older_than_its_timestamp() {
    use crate::agents::AgentStatus;
    let activity = Timestamp::from_second(1_000).unwrap();
    let mut state = crate::testkit::agent_state("grok", "s1", activity);
    state.status = AgentStatus::Running;
    for (at, status, open) in [
        (990, AgentStatus::Running, true),
        (1_010, AgentStatus::Idle, false),
    ] {
        let mut decoded = hook_output(
            &GrokAdapter,
            "Notification",
            &json!({
                "sessionId":"s1", "notificationType":"idle_prompt",
                "timestamp": Timestamp::from_second(at).unwrap().to_string()
            }),
        );
        assert!(decoded.lifecycle().is_none());
        let context = decoded.take_observed_context().expect("idle marker");
        assert_eq!(context.agent_id.as_str(), "s1");
        state.context = Some(context.context);
        assert_eq!(state.effective_status(), status);
        assert_eq!(state.holds_open_turn(), open);
    }
    for payload in [
        json!({"sessionId":"s1", "notificationType":"idle_prompt"}),
        json!({"sessionId":"s1", "notificationType":"idle_prompt", "timestamp":"bad"}),
        json!({"sessionId":"s1", "notificationType":"idle_prompt", "timestamp":"2026-01-01T00:00:00Z", "subagentType":"explore"}),
    ] {
        assert!(
            hook_output(&GrokAdapter, "Notification", &payload)
                .take_observed_context()
                .is_none()
        );
    }
}

#[test]
fn only_failure_hooks_contribute_turn_errors() {
    let adapter = GrokAdapter;
    assert!(
        hook_output(&adapter, "Stop", &json!({"reason":"end_turn"}))
            .turn_error()
            .cloned()
            .is_none()
    );
    assert!(
        hook_output(
            &adapter,
            "Notification",
            &json!({"notificationType":"permission_prompt"})
        )
        .turn_error()
        .cloned()
        .is_none()
    );
    assert_eq!(
        hook_output(
            &adapter,
            "Notification",
            &json!({"notificationType":"agent_error","message":"provider failed"})
        )
        .turn_error()
        .cloned()
        .and_then(|error| error.label),
        Some("provider failed".to_owned())
    );
}
