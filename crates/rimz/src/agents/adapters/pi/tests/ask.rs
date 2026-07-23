use super::*;

use crate::agents::{AnswerStep, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

fn ask_payload() -> Value {
    json!({
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
    })
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
fn questionnaire_opens_only_with_ui_and_closes_on_tool_end() {
    assert_eq!(
        signal("tool_call", &ask_payload()),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: Some("ask-call".to_owned()),
        })
    );
    assert_eq!(
        decode("tool_call", &ask_payload()).class(),
        AgentHookClass::AwaitingUser
    );

    // Pi runs ordinary tools unasked, so only the rpiv questionnaire blocks.
    assert_eq!(
        decode(
            "tool_call",
            &json!({ "session_id": "sess-1", "tool_name": "bash" }),
        )
        .class(),
        AgentHookClass::Unknown
    );

    // A headless call has no pane UI to answer in and must not strand a
    // waiting row.
    let mut headless = ask_payload();
    headless["has_ui"] = json!(false);
    assert_eq!(signal("tool_call", &headless), None);
    assert_eq!(
        decode("tool_call", &headless).class(),
        AgentHookClass::Unknown
    );

    // The matching tool end clears the wait, carrying the same native key.
    assert_eq!(
        signal(
            "tool_execution_end",
            &json!({
                "session_id": "sess-1",
                "tool_call_id": "ask-call",
                "tool_name": "ask_user_question",
                "tool_details": { "answers": [], "cancelled": true }
            }),
        ),
        Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: Some("ask_user_question".to_owned()),
            native_key: Some("ask-call".to_owned()),
        })
    );
}

#[test]
fn question_detail_carries_the_rpiv_preview_policy() {
    // Schema normalization itself belongs to `agents::question`; pi only picks
    // the preview policy, under which a non-empty string preview counts and an
    // empty one does not.
    let questions = decode(
        "tool_call",
        &json!({
            "tool_name": "ask_user_question",
            "tool_input": {
                "questions": [{
                    "question": "Which route?",
                    "options": [
                        { "label": "Safe", "description": "Stage the rollout", "preview": "## Staged" },
                        { "label": "Fast", "preview": "" }
                    ],
                    "multiSelect": true
                }]
            }
        }),
    )
    .questions()
    .to_vec();
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
            decode("tool_call", &payload).questions().is_empty(),
            "payload {payload}"
        );
    }
}

/// The mixed-kind payload is `fc1573069 test(pi): cover mixed questionnaire
/// answers` — every rpiv answer kind in one result, which is what the pane
/// actually sends back for a multi-question ask.
#[test]
fn native_answer_detail_maps_every_rpiv_answer_kind() {
    let answers = decode(
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
    .native_answers()
    .map(<[_]>::to_vec)
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
        decode(
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
        .native_answers(),
        None,
        "cancelling after a partial answer must not record that answer"
    );
}

#[test]
fn answer_plan_drives_single_multi_and_free_text() {
    let plan = |question: AskQuestion, reply: AskReply| {
        PiAdapter
            .answer_plan(AskKind::Question, &[question], &[reply])
            .expect("answer plan")
    };

    assert_eq!(
        plan(
            ask_question(3, false, false),
            AskReply {
                picks: vec![2],
                text: None,
            },
        ),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    assert_eq!(
        plan(
            ask_question(2, false, true),
            AskReply {
                picks: vec![1],
                text: None,
            },
        ),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    // Free text lives one row past the last option.
    assert_eq!(
        plan(
            ask_question(2, false, false),
            AskReply {
                picks: vec![],
                text: Some("Use a canary".to_owned()),
            },
        ),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Paste("Use a canary".to_owned()),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );

    // Multi-select toggles in ascending order, then walks to submit.
    assert_eq!(
        plan(
            ask_question(4, true, false),
            AskReply {
                picks: vec![2, 0],
                text: None,
            },
        ),
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

    // A multi-question ask needs one extra Enter to submit the whole form.
    assert_eq!(
        PiAdapter
            .answer_plan(
                AskKind::Question,
                &[ask_question(2, false, false), ask_question(2, false, false)],
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
            .expect("answer plan"),
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
fn answer_plan_rejects_unavailable_or_mismatched_answers() {
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
