use super::*;

use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::agents::{AgentErr, AgentHookClass, AgentStatus, AnswerStep, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};
use serde_json::json;

// Capability and coverage-table honesty is cross-checked against behavior for
// every adapter in `agents::conformance`; this slice only pins what is
// pi-specific behavior beyond those flags.

#[test]
fn pi_activity_filter_excludes_the_blocking_gate_and_launch_commands_build() {
    let descriptor = PiAdapter.descriptor();
    // Completed-work events touch activity; the blocking `tool_call` gate is
    // excluded so creating the ask never instantly un-blocks the row.
    assert!(descriptor.records_activity("tool_execution_end"));
    assert!(descriptor.records_activity("agent_end"));
    assert!(descriptor.records_activity("message_update"));
    assert!(descriptor.records_activity("turn_end"));
    assert!(!descriptor.records_activity("tool_call"));
    assert!(!descriptor.records_activity("session_shutdown"));

    assert_eq!(
        PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.descriptor().launch.fork_command("0199aaf2"),
        Some(vec![
            "pi".to_owned(),
            "--fork".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.launch_command(&[], None),
        Some(vec!["pi".to_owned()])
    );
    assert_eq!(
        PiAdapter.launch_command(
            &["--model".to_owned(), "large".to_owned()],
            Some("review this"),
        ),
        Some(vec![
            "pi".to_owned(),
            "--model".to_owned(),
            "large".to_owned(),
            "--".to_owned(),
            "review this".to_owned(),
        ])
    );
}

#[test]
fn pi_question_detail_normalizes_the_rpiv_schema() {
    let questions = PiAdapter
        .decode_hook(
            "tool_call",
            &json!({
                "tool_name": "ask_user_question",
                "tool_input": {
                    "questions": [
                        {
                            "question": "  Which route?  ",
                            "header": "Route",
                            "options": [
                                {
                                    "label": "  Safe  ",
                                    "description": "  Stage the rollout  ",
                                    "preview": "## Staged"
                                },
                                {
                                    "label": "Fast",
                                    "description": "   ",
                                    "preview": ""
                                },
                                { "label": "  ", "description": "dropped" }
                            ],
                            "multiSelect": true
                        },
                        { "question": "  ", "options": [] }
                    ]
                }
            }),
        )
        .expect("test hook decodes")
        .questions;
    assert_eq!(
        questions,
        vec![AskQuestion {
            question: "Which route?".to_owned(),
            options: vec![
                AskOption {
                    label: "Safe".to_owned(),
                    description: Some("Stage the rollout".to_owned()),
                    caution: None,
                },
                AskOption::from("Fast".to_owned()),
            ],
            multi_select: true,
            has_option_previews: true,
        }]
    );

    for payload in [
        json!({ "tool_name": "bash", "tool_input": {} }),
        json!({ "tool_name": "ask_user_question", "tool_input": { "questions": "bad" } }),
        json!({
            "tool_name": "ask_user_question",
            "tool_input": { "questions": [{ "question": " ", "options": [] }] }
        }),
        json!({
            "tool_name": "ask_user_question",
            "has_ui": false,
            "tool_input": {
                "questions": [{ "question": "Hidden?", "options": [] }]
            }
        }),
    ] {
        assert!(
            PiAdapter
                .decode_hook("tool_call", &payload)
                .expect("test hook decodes")
                .questions
                .is_empty()
        );
    }
}

#[test]
fn pi_native_answer_detail_maps_rpiv_results_and_ignores_cancellation() {
    let answers = PiAdapter
        .decode_hook(
            "tool_execution_end",
            &json!({
                "tool_name": "ask_user_question",
                "tool_details": {
                    "answers": [
                        {
                            "questionIndex": 0,
                            "question": "  Route?  ",
                            "kind": "option",
                            "answer": "  Safe  ",
                            "notes": "  gradual  "
                        },
                        {
                            "questionIndex": 1,
                            "question": "Name?",
                            "kind": "custom",
                            "answer": "  Canary  "
                        },
                        {
                            "questionIndex": 2,
                            "question": "Discuss?",
                            "kind": "chat",
                            "answer": "localized label"
                        },
                        {
                            "questionIndex": 3,
                            "question": "Checks?",
                            "kind": "multi",
                            "answer": null,
                            "selected": ["  Unit  ", "Integration"]
                        },
                        {
                            "questionIndex": 4,
                            "question": "Skipped?",
                            "kind": "custom",
                            "answer": null
                        }
                    ],
                    "cancelled": false
                }
            }),
        )
        .expect("test hook decodes")
        .native_answers
        .expect("answer detail");
    assert_eq!(
        answers,
        vec![
            AskAnswer {
                question: Some("Route?".to_owned()),
                chosen: vec!["Safe".to_owned()],
                note: Some("gradual".to_owned()),
            },
            AskAnswer {
                question: Some("Name?".to_owned()),
                chosen: vec!["Canary".to_owned()],
                note: None,
            },
            AskAnswer {
                question: Some("Discuss?".to_owned()),
                chosen: vec!["Chat about this".to_owned()],
                note: None,
            },
            AskAnswer {
                question: Some("Checks?".to_owned()),
                chosen: vec!["Unit".to_owned(), "Integration".to_owned()],
                note: None,
            },
        ]
    );

    assert_eq!(
        PiAdapter
            .decode_hook(
                "tool_execution_end",
                &json!({
                    "tool_name": "ask_user_question",
                    "tool_details": {
                        "answers": [{
                            "question": "Partially answered?",
                            "kind": "option",
                            "answer": "Yes"
                        }],
                        "cancelled": true
                    }
                }),
            )
            .expect("test hook decodes")
            .native_answers,
        None,
        "cancelling after a partial answer must not record that answer"
    );
    assert_eq!(
        PiAdapter
            .decode_hook(
                "tool_execution_end",
                &json!({
                    "tool_name": "ask_user_question",
                    "tool_details": { "answers": [], "cancelled": true, "error": "no_ui" }
                }),
            )
            .expect("test hook decodes")
            .native_answers,
        None
    );
}

#[test]
fn pi_answer_plan_drives_single_pick_preview_and_free_text() {
    let plain = ask_question(3, false, false);
    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                std::slice::from_ref(&plain),
                &[AskReply {
                    picks: vec![2],
                    text: None,
                }],
            )
            .unwrap(),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    let preview = ask_question(2, false, true);
    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[preview],
                &[AskReply {
                    picks: vec![1],
                    text: None,
                }],
            )
            .unwrap(),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[ask_question(2, false, false)],
                &[AskReply {
                    picks: vec![],
                    text: Some("Use a canary".to_owned()),
                }],
            )
            .unwrap(),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Paste("Use a canary".to_owned()),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );
}

#[test]
fn pi_answer_plan_drives_multi_select_and_multi_question_submit() {
    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[ask_question(4, true, false)],
                &[AskReply {
                    picks: vec![2, 0],
                    text: None,
                }],
            )
            .unwrap(),
        vec![
            AnswerStep::Text(" ".to_owned()),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Text(" ".to_owned()),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[ask_question(2, false, false), ask_question(2, false, false),],
                &[
                    AskReply {
                        picks: vec![1],
                        text: None,
                    },
                    AskReply {
                        picks: vec![],
                        text: Some("Custom".to_owned()),
                    },
                ],
            )
            .unwrap(),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Paste("Custom".to_owned()),
            AnswerStep::Key(NamedKey::Enter),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );
}

#[test]
fn pi_answer_plan_rejects_unavailable_or_mismatched_answers() {
    for question in [ask_question(2, true, false), ask_question(2, false, true)] {
        let error = PiAdapter
            .answer_plan(
                AskKind::Question,
                &[question],
                &[AskReply {
                    picks: vec![],
                    text: Some("Custom".to_owned()),
                }],
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("suppresses the `Type something.` row")
        );
    }
    assert!(
        PiAdapter
            .answer_plan(AskKind::Question, &[ask_question(2, false, false)], &[])
            .unwrap_err()
            .to_string()
            .contains("expected 1 answers, got 0")
    );
    assert!(
        PiAdapter
            .answer_plan(
                AskKind::Permission,
                &[ask_question(2, false, false)],
                &[AskReply {
                    picks: vec![0],
                    text: None,
                }],
            )
            .unwrap_err()
            .to_string()
            .contains("only for questionnaire asks")
    );
    assert!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[ask_question(2, false, false)],
                &[AskReply {
                    picks: vec![0],
                    text: Some("Custom".to_owned()),
                }],
            )
            .unwrap_err()
            .to_string()
            .contains("cannot combine picks and text")
    );
}

fn ask_question(option_count: usize, multi_select: bool, has_option_previews: bool) -> AskQuestion {
    AskQuestion {
        question: "Choose?".to_owned(),
        options: (0..option_count)
            .map(|index| AskOption::from(format!("Option {index}")))
            .collect(),
        multi_select,
        has_option_previews,
    }
}

#[test]
fn pi_render_preset_maps_model_and_thinking() {
    use crate::agents::{LaunchPreset, PresetErr};

    assert_eq!(
        PiAdapter.descriptor().render_preset(&LaunchPreset {
            model: Some("openai/gpt-4o".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "openai/gpt-4o".to_owned(),
            "--thinking".to_owned(),
            "high".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.descriptor().render_preset(&LaunchPreset {
            system_prompt_file: Some(Path::new("/abs/prompt.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "pi",
            field: "system-prompt-file",
        })
    );
    assert_eq!(
        PiAdapter.descriptor().render_preset(&LaunchPreset {
            append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "pi",
            field: "append-system-prompt-file",
        })
    );
    assert!(
        PiAdapter
            .descriptor()
            .render_preset(&LaunchPreset::default())
            .expect("empty preset is valid")
            .is_empty()
    );
}

#[test]
fn pi_observes_lifecycle_enrichment_and_error_bits() {
    let started = PiAdapter
        .decode_hook(
            "session_start",
            &json!({
                "session_id": "sess-1",
                "cwd": "/home/u/code/query-engine",
                "model": "gpt-5.5",
                "effort": "medium",
                "context_pct": 150,
                "context_window": 272_000,
                "total_tokens": 8160,
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(started.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(started.signal, LifecycleSignal::Registered);
    assert_eq!(
        started.worktree_path.as_deref(),
        Some("/home/u/code/query-engine")
    );
    assert_eq!(started.launch.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(started.launch.effort.as_deref(), Some("medium"));
    assert_eq!(started.context_pct, Some(100));
    assert_eq!(started.context_window, Some(272_000));
    assert_eq!(started.total_tokens, Some(8160));
    assert_eq!(started.parent_agent_id, None);

    let prompt = PiAdapter
        .decode_hook(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "  add a dark mode toggle  " }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("add a dark mode toggle"));
    assert_eq!(prompt.task.as_deref(), Some("add a dark mode toggle"));

    let injected = PiAdapter
        .decode_hook(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "<system-reminder>noise" }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);

    let skill = PiAdapter
        .decode_hook(
            "before_agent_start",
            &json!({
                "session_id": "sess-1",
                "prompt": "<skill name=\"merge\" Location=\"/home/u/.agents/skills/merge/SKILL.md\">\nmerge the branch\n</skill>"
            }),
        ).expect("test hook decodes").lifecycle
        .expect("observation");
    assert_eq!(skill.prompt, None);
    assert_eq!(skill.task, None);

    let clean = PiAdapter
        .decode_hook(
            "agent_settled",
            &json!({
                "session_id": "sess-1",
                "stop_reason": "stop",
                "model": "gpt-5",
                "total_tokens": 4200,
                "input_tokens": 100,
                "cache_write_input_tokens": 40,
                "cache_read_input_tokens": 30,
                "output_tokens": 20,
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(
        clean.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    assert_eq!(clean.launch.model.as_deref(), Some("gpt-5"));
    assert_eq!(clean.total_tokens, Some(4200));
    assert_eq!(clean.fresh_input_tokens, Some(100));
    assert_eq!(clean.cache_write_input_tokens, Some(40));
    assert_eq!(clean.cache_read_input_tokens, Some(30));
    assert_eq!(clean.output_tokens, Some(20));

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1", "stop_reason": "aborted" }),
            LifecycleSignal::TurnInterrupted,
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "error" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "stop", "error_message": "boom" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        let observation = PiAdapter
            .decode_hook("agent_settled", &payload)
            .expect("test hook decodes")
            .lifecycle
            .expect("observation");
        assert_eq!(observation.signal, expected, "payload {payload}",);
    }
}

#[test]
fn pi_carries_final_assistant_text_through_the_settled_boundary() {
    let payload = json!({
        "session_id": "sess-1",
        "last_assistant_message": "  Fixed the parser.  "
    });
    assert_eq!(
        PiAdapter
            .decode_hook("agent_settled", &payload)
            .expect("test hook decodes")
            .final_message
            .as_deref(),
        Some("Fixed the parser.")
    );
    assert_eq!(
        PiAdapter
            .decode_hook("agent_end", &payload)
            .expect("test hook decodes")
            .final_message,
        None,
        "agent_end is enrichment-only and must not complete output early"
    );
}

#[test]
fn pi_observes_rich_context_from_the_extension_envelope() {
    let context = normalized_context(json!({
        "model": "gpt-5.5",
        "session_name": "Parser cleanup",
        "effort": "high",
        "context_pct": 42,
        "context_window": 272_000,
        "total_tokens": 114_000,
        "total_cost_usd": 0.125,
        "input_tokens": 10,
        "cache_write_input_tokens": 4,
        "cache_read_input_tokens": 30,
        "output_tokens": 2,
        "rate_limits": [
            {
                "used_percentage": 72,
                "resets_at": 1_700_018_000i64,
                "duration_mins": 300,
                "observed_at": 1_700_000_000i64
            },
            {
                "used_percentage": 35,
                "resets_at": 1_700_604_800i64,
                "duration_mins": 10_080,
                "observed_at": 1_700_000_000i64
            }
        ]
    }))
    .expect("rich context");
    insta::assert_json_snapshot!(context, @r###"
        {
          "source": "pi",
          "session_name": "Parser cleanup",
          "model_id": "gpt-5.5",
          "effort": "high",
          "cost": {
            "total_cost_usd": 0.125
          },
          "tokens": {
            "context_window_size": 272000,
            "used_percentage": 42,
            "current_usage": {
              "input_tokens": 10,
              "output_tokens": 2,
              "cache_creation_input_tokens": 4,
              "cache_read_input_tokens": 30
            }
          },
          "rate_limits": {
            "windows": [
              {
                "used_percentage": 72,
                "resets_at": "2023-11-15T03:13:20Z",
                "duration_mins": 300,
                "observed_at": "2023-11-14T22:13:20Z"
              },
              {
                "used_percentage": 35,
                "resets_at": "2023-11-21T22:13:20Z",
                "duration_mins": 10080,
                "observed_at": "2023-11-14T22:13:20Z"
              }
            ]
          },
          "observed_at": "2023-11-14T22:13:20Z"
        }
        "###);

    let without_cost = normalized_context(json!({
        "context_pct": 7,
        "context_window": 128_000,
        "input_tokens": 9
    }))
    .expect("context without cost");
    assert!(without_cost.cost.is_none());
    assert_eq!(
        without_cost.tokens.as_ref().unwrap().used_percentage,
        Some(7)
    );

    let without_rate_limits = normalized_context(json!({
        "context_pct": 12,
        "context_window": 128_000,
        "input_tokens": 6,
        "output_tokens": 1
    }))
    .expect("context without windows");
    assert!(without_rate_limits.rate_limits.is_none());
    assert_eq!(
        without_rate_limits
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(6)
    );

    let zero_split = normalized_context(json!({
        "context_pct": 0,
        "context_window": 128_000,
        "input_tokens": 0,
        "cache_write_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": 0
    }))
    .expect("zero split still carries the window");
    assert!(
        zero_split.tokens.as_ref().unwrap().current_usage.is_none(),
        "all-zero token split drops the per-call breakdown"
    );

    assert!(
        PiAdapter
            .observe_context("pi", &json!({ "context_window": "not a number" }))
            .is_none(),
        "malformed context payloads degrade to no enrichment"
    );
}

#[test]
fn pi_rate_limit_wire_is_tolerant_and_compatible() {
    let context = normalized_context(json!({
        "model": "gpt-5",
        "rateLimits": [
            {
                "usedPercent": "101.4",
                "resetsAt": "2023-11-15T03:13:20Z",
                "durationMins": 300,
                "observedAt": "1700000000"
            },
            {
                "used_percentage": -2.0,
                "resets_at": "1700018000"
            },
            { "used_percentage": "NaN", "duration_mins": "bad" },
            { "observed_at": 1700000000 },
            "invalid"
        ]
    }))
    .unwrap();
    let windows = context.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].used_percentage, Some(100));
    assert_eq!(windows[0].duration_mins, Some(300));
    assert_eq!(
        windows[0].resets_at.unwrap().to_string(),
        "2023-11-15T03:13:20Z"
    );
    assert_eq!(windows[1].used_percentage, Some(0));
    assert_eq!(
        windows[1].resets_at.unwrap().to_string(),
        "2023-11-15T03:13:20Z"
    );

    for rate_limits in [json!([]), json!({"bad": true})] {
        let context = normalized_context(json!({
            "model": "kept",
            "rate_limits": rate_limits
        }))
        .unwrap();
        assert_eq!(context.model_id.as_deref(), Some("kept"));
        assert!(context.rate_limits.is_none());
    }

    let context = normalized_context(json!({
        "total_cost_usd": "malformed sibling",
        "rate_limits": [{"used_percentage": 50}]
    }))
    .unwrap();
    assert_eq!(
        context.rate_limits.unwrap().windows[0].used_percentage,
        Some(50),
        "a malformed sibling field must not discard independently valid windows"
    );
}

#[test]
fn model_select_is_enrichment_only() {
    let payload = json!({ "session_id": "s", "model": "gpt-5.5", "effort": "high" });
    assert_eq!(
        PiAdapter
            .decode_hook("model_select", &payload)
            .expect("test hook decodes")
            .class,
        AgentHookClass::Lifecycle
    );
    assert!(
        PiAdapter
            .decode_hook("model_select", &payload)
            .expect("test hook decodes")
            .lifecycle
            .is_none()
    );
    assert_eq!(
        PiAdapter
            .observe_context("pi", &payload)
            .unwrap()
            .model_id
            .as_deref(),
        Some("gpt-5.5")
    );
}

fn normalized_context(payload: serde_json::Value) -> Option<AgentContext> {
    let mut context = PiAdapter.observe_context("pi", &payload)?;
    context.observed_at = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    Some(context)
}

#[test]
fn pi_questionnaire_lifecycle_opens_only_with_ui_and_clears_on_completion() {
    let ask_payload = json!({
        "session_id": "sess-1",
        "tool_call_id": "ask-call",
        "tool_name": "ask_user_question",
        "tool_input": {
            "questions": [{
                "question": "Which route?",
                "options": [
                    { "label": "Safe", "description": "Stage it" },
                    { "label": "Fast", "description": "Ship it" }
                ]
            }]
        }
    });
    assert_eq!(
        PiAdapter
            .decode_hook("tool_call", &ask_payload)
            .expect("test hook decodes")
            .lifecycle
            .map(|observation| observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: Some("ask-call".to_owned()),
        })
    );
    assert_eq!(
        PiAdapter
            .decode_hook("tool_call", &ask_payload)
            .expect("test hook decodes")
            .class,
        AgentHookClass::AwaitingUser
    );
    assert_eq!(
        PiAdapter
            .decode_hook(
                "tool_call",
                &json!({ "session_id": "sess-1", "tool_name": "bash" }),
            )
            .expect("test hook decodes")
            .class,
        AgentHookClass::Unknown
    );

    let mut headless = ask_payload;
    headless["has_ui"] = json!(false);
    assert_eq!(
        PiAdapter
            .decode_hook("tool_call", &headless)
            .expect("test hook decodes")
            .lifecycle,
        None
    );
    assert_eq!(
        PiAdapter
            .decode_hook("tool_call", &headless)
            .expect("test hook decodes")
            .class,
        AgentHookClass::Unknown
    );

    assert_eq!(
        PiAdapter
            .decode_hook(
                "tool_execution_end",
                &json!({
                    "session_id": "sess-1",
                    "tool_call_id": "ask-call",
                    "tool_name": "ask_user_question",
                    "tool_details": { "answers": [], "cancelled": true }
                }),
            )
            .expect("test hook decodes")
            .lifecycle
            .map(|observation| observation.signal),
        Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: Some("ask-call".to_owned()),
        })
    );
}

#[test]
fn pi_observes_normalized_subagent_lifecycle() {
    let started = PiAdapter
        .decode_hook(
            "subagent_started",
            &json!({
                "session_id": "parent-1",
                "cwd": "/work/project",
                "subagent_id": "run-7#1",
                "subagent_label": " reviewer ",
                "subagent_source": "pi-session"
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("started observation");
    assert_eq!(started.agent_id.as_deref(), Some("run-7#1"));
    assert_eq!(started.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(started.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(started.task.as_deref(), Some("reviewer"));
    assert_eq!(started.worktree_path.as_deref(), Some("/work/project"));
    assert_eq!(started.total_tokens, None);

    let stopped = PiAdapter
        .decode_hook(
            "subagent_stopped",
            &json!({
                "session_id": "parent-1",
                "cwd": "/work/project",
                "subagent_id": "run-7#1",
                "subagent_label": "reviewer",
                "subagent_source": "pi-session",
                "errored": true,
                "total_tokens": 1234
            }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("stopped observation");
    assert_eq!(stopped.agent_id.as_deref(), Some("run-7#1"));
    assert_eq!(stopped.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(
        stopped.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(stopped.task.as_deref(), Some("reviewer"));
    assert_eq!(stopped.total_tokens, Some(1234));
}

#[test]
fn pi_quarantines_malformed_subagent_identity() {
    for payload in [
        json!({
            "session_id": "parent-1",
            "subagent_label": "missing child"
        }),
        json!({
            "session_id": "same-id",
            "subagent_id": "same-id",
            "subagent_label": "same child and parent"
        }),
    ] {
        assert_eq!(
            PiAdapter
                .decode_hook("subagent_started", &payload)
                .expect("test hook decodes")
                .lifecycle,
            None,
            "payload {payload}"
        );
    }
}

#[test]
fn pi_tool_compaction_shutdown_and_unknown_events_map_cleanly() {
    for (tool_name, expected) in [
        (
            "edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
                native_key: Some("sibling-call".to_owned()),
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                native_key: Some("sibling-call".to_owned()),
            }),
        ),
        ("read", None),
    ] {
        let observed = PiAdapter
            .decode_hook(
                "tool_execution_end",
                &json!({
                    "session_id": "sess-1",
                    "tool_call_id": "sibling-call",
                    "tool_name": tool_name
                }),
            )
            .expect("test hook decodes")
            .lifecycle;
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool_name}");
    }

    let running = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    let edit = PiAdapter
        .decode_hook(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": "edit" }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(
        step(Some(&running), None, &edit.signal).next.phase,
        TurnPhase::Acting
    );

    let compacting = PiAdapter
        .decode_hook("session_before_compact", &json!({ "session_id": "sess-1" }))
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    for (reason, expected) in [
        (Some("manual"), Some(false)),
        (Some("threshold"), Some(true)),
        (Some("overflow"), Some(true)),
        (Some("future"), None),
        (None, None),
    ] {
        let mut payload = json!({ "session_id": "sess-1" });
        if let Some(reason) = reason {
            payload["compaction_reason"] = json!(reason);
        }
        let compacted = PiAdapter
            .decode_hook("session_compact", &payload)
            .expect("test hook decodes")
            .lifecycle
            .expect("observation");
        assert_eq!(
            compacted.signal,
            LifecycleSignal::CompactionEnded { auto: expected },
            "{reason:?}"
        );
    }
    assert_eq!(
        PiAdapter
            .decode_hook(
                "agent_end",
                &json!({ "session_id": "sess-1", "stop_reason": "error" }),
            )
            .expect("test hook decodes")
            .lifecycle,
        None
    );
    let settled = PiAdapter
        .decode_hook(
            "agent_settled",
            &json!({ "session_id": "sess-1", "stop_reason": "error" }),
        )
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(
        settled.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );
    let ended = PiAdapter
        .decode_hook("session_shutdown", &json!({ "session_id": "sess-1" }))
        .expect("test hook decodes")
        .lifecycle
        .expect("observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);

    assert_eq!(
        PiAdapter
            .decode_hook("tool_call", &json!({ "session_id": "sess-1" }))
            .expect("test hook decodes")
            .lifecycle,
        None
    );
    assert_eq!(
        PiAdapter
            .decode_hook("bogus", &json!({}))
            .expect("test hook decodes")
            .lifecycle,
        None
    );

    // Only a real shutdown ends the session.
    assert!(PiAdapter.descriptor().ends_session("session_shutdown"));
    assert!(!PiAdapter.descriptor().ends_session("agent_end"));
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = PiAdapter
        .decode_hook("agent_end", &Value::Null)
        .expect("test hook decodes")
        .neutral;
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn install_preview_and_uninstall_only_own_managed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions").join("rimz.ts");

    let report = PI_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.agent, "pi");
    assert!(!report.files[0].existed);
    assert_eq!(report.installed_events, managed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(PI_MANAGED_SOURCE.installed_at(&path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));

    let stale = "// still _rimz_managed\n// older RimZ source\n";
    std::fs::write(&path, stale).unwrap();
    assert!(PI_MANAGED_SOURCE.installed_at(&path));
    assert!(PI_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(PI_MANAGED_SOURCE.install_into(&path).unwrap().files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));

    let preview = PI_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.agent, "pi");
    assert!(preview.files[0].existed);
    assert_eq!(preview.files[0].candidate, EXTENSION_SOURCE);

    let removed = PI_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(removed.files[0].existed);
    assert_eq!(removed.removed_events, managed_event_names());
    assert!(!path.exists());
    assert!(!PI_MANAGED_SOURCE.installed_at(&path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(!PI_MANAGED_SOURCE.uninstall_from(&path).unwrap().files[0].existed);

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// the user's own extension\n").unwrap();
    assert!(matches!(
        PI_MANAGED_SOURCE.install_into(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    assert!(matches!(
        PI_MANAGED_SOURCE.preview_at(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    let report = PI_MANAGED_SOURCE.uninstall_from(&user_path).unwrap();
    assert!(report.files[0].existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own extension\n"
    );
    assert!(!PI_MANAGED_SOURCE.installed_at(&user_path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&user_path));
}

fn managed_event_names() -> Vec<String> {
    PI_HOOKS.iter().map(|hook| hook.event.to_owned()).collect()
}

#[test]
fn extension_source_wires_every_event() {
    assert!(EXTENSION_SOURCE.contains("_rimz_managed"));
    assert!(EXTENSION_SOURCE.contains(r#"["hooks", "feed", "--source", "pi"]"#));
    assert!(EXTENSION_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(EXTENSION_SOURCE.contains("RIMZ_BIN"));
    assert!(EXTENSION_SOURCE.contains("PI_VERSION"));
    assert!(EXTENSION_SOURCE.contains("hasAgentSettled"));
    assert!(EXTENSION_SOURCE.contains("getContextUsage"));
    assert!(EXTENSION_SOURCE.contains("Math.round"));
    assert!(EXTENSION_SOURCE.contains("costBySession"));
    assert!(EXTENSION_SOURCE.contains("verdictBySession"));
    assert!(EXTENSION_SOURCE.contains("visibleAssistantText"));
    assert!(EXTENSION_SOURCE.contains("last_assistant_message"));
    assert!(EXTENSION_SOURCE.contains("total_cost_usd"));
    assert!(EXTENSION_SOURCE.contains("getBranch"));
    assert!(EXTENSION_SOURCE.contains("session_name"));
    assert!(EXTENSION_SOURCE.contains("messageSignature"));
    assert!(EXTENSION_SOURCE.contains("setTimeout"));
    assert!(EXTENSION_SOURCE.contains("cache_write_input_tokens"));
    assert!(EXTENSION_SOURCE.contains("rate_limits"));
    assert!(EXTENSION_SOURCE.contains("compaction_reason"));
    assert!(EXTENSION_SOURCE.contains("compaction_will_retry"));
    assert!(EXTENSION_SOURCE.contains("has_ui: ctx?.hasUI === true"));
    assert!(EXTENSION_SOURCE.contains(r#"const PARENT_SESSION_ENV = "RIMZ_PI_PARENT_SESSION""#));
    assert!(EXTENSION_SOURCE.contains(r#"Symbol.for("rimz.pi.primary-session")"#));
    assert!(EXTENSION_SOURCE.contains("!isPrimary && id && parentId && parentId !== id"));
    assert!(EXTENSION_SOURCE.contains("process.env.PI_SUBAGENT_CHILD_AGENT"));
    assert!(EXTENSION_SOURCE.contains("feedChildStart(ctx)"));
    assert!(EXTENSION_SOURCE.contains("feedChildStop(ctx, verdict)"));
    assert!(EXTENSION_SOURCE.contains(r#"subagent_source: "pi-session""#));
    assert!(!EXTENSION_SOURCE.contains("pi.events.on"));
    assert!(!EXTENSION_SOURCE.contains("pi-subagents:manager"));
    assert!(EXTENSION_SOURCE.contains("tool_details:"));
    assert!(EXTENSION_SOURCE.contains("ev?.result?.details"));
    assert!(
        !EXTENSION_SOURCE.contains("addSessionCost(sessionId(ctx), last?.usage"),
        "agent_end's last message is the final turn_end usage and must not add cost again"
    );
    for hook in PI_HOOKS {
        let event = hook.event;
        let registered = match event {
            "subagent_started" | "subagent_stopped" => {
                EXTENSION_SOURCE.contains(&format!("feedSubagent(\"{event}\""))
            }
            _ => EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
        };
        assert!(registered, "extension registers {event}",);
    }
    assert!(EXTENSION_SOURCE.contains("block: true"));
    assert!(EXTENSION_SOURCE.contains(r#"ev?.reason === "reload""#));
}
