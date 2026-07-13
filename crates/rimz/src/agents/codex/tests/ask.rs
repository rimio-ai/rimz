use serde_json::json;

use super::super::ask;
use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskOption, AskQuestion};

#[test]
fn question_detail_normalizes_verified_codex_schema() {
    let questions = ask::question_detail(
        "request_user_input",
        &json!({
            "questions": [
                {
                    "id": "path",
                    "header": "Migration",
                    "question": " Pick a path? ",
                    "options": [
                        { "label": " Blue ", "description": " Safer " },
                        { "label": "Green", "description": "" }
                    ]
                },
                {
                    "id": "notify",
                    "header": " Notify users ",
                    "multiSelect": true,
                    "options": [{ "label": "Email" }]
                }
            ]
        }),
    )
    .expect("structured questions");

    assert_eq!(questions[0].question, "Pick a path?");
    assert_eq!(questions[0].options[0].label, "Blue");
    assert_eq!(
        questions[0].options[0].description.as_deref(),
        Some("Safer")
    );
    assert_eq!(questions[0].options[1].description, None);
    assert_eq!(questions[1].question, "Notify users");
    assert!(questions[1].multi_select);
    assert!(!questions[0].has_option_previews);
}

#[test]
fn native_answer_map_uses_ids_and_input_question_order() {
    let answers = ask::answer_detail(
        "request_user_input",
        &json!({
            "questions": [
                { "id": "path", "question": "Pick a path?" },
                { "id": "notify", "question": "Notify users?" }
            ]
        }),
        &json!({
            "answers": {
                "notify": { "answers": ["Email"] },
                "path": { "answers": ["Blue"] }
            }
        }),
    )
    .expect("native answers");

    assert_eq!(answers[0].question.as_deref(), Some("Pick a path?"));
    assert_eq!(answers[0].chosen, vec!["Blue"]);
    assert_eq!(answers[1].question.as_deref(), Some("Notify users?"));
    assert_eq!(answers[1].chosen, vec!["Email"]);
}

#[test]
fn submitted_prompt_answer_trims_and_caps_native_plan_reply() {
    let long = format!("  {}  ", "x".repeat(1_100));
    let answers = ask::submitted_prompt_answer(&long).expect("submitted prompt");
    assert_eq!(answers[0].chosen[0].chars().count(), 1_000);
    assert!(ask::submitted_prompt_answer("   ").is_none());
}

#[test]
fn plan_and_single_select_answer_steps_match_codex_01443() {
    assert_eq!(
        ask::answer_plan(
            AskKind::PlanApproval,
            &[],
            &[AskReply {
                picks: vec![0],
                text: None,
            }],
        )
        .unwrap(),
        vec![AnswerStep::Key(NamedKey::Enter)]
    );

    let questions = vec![
        question("First?", &["A", "B"]),
        question("Second?", &["X", "Y", "Z"]),
    ];
    let answers = vec![
        AskReply {
            picks: vec![1],
            text: None,
        },
        AskReply {
            picks: vec![2],
            text: None,
        },
    ];
    assert_eq!(
        ask::answer_plan(AskKind::Question, &questions, &answers).unwrap(),
        vec![
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Down),
            AnswerStep::Key(NamedKey::Enter),
        ]
    );
}

#[test]
fn answer_plan_rejects_unverified_codex_interactions() {
    let mut multi = question("Many?", &["A", "B"]);
    multi.multi_select = true;
    assert_invalid(
        ask::answer_plan(
            AskKind::Question,
            &[multi],
            &[AskReply {
                picks: vec![0, 1],
                text: None,
            }],
        ),
        "multi-select",
    );
    assert_invalid(
        ask::answer_plan(
            AskKind::Question,
            &[question("Other?", &["A", "None of the above"])],
            &[AskReply {
                picks: vec![1],
                text: None,
            }],
        ),
        "free-text",
    );
    assert_invalid(
        ask::answer_plan(
            AskKind::Permission,
            &[],
            &[AskReply {
                picks: vec![0],
                text: None,
            }],
        ),
        "Codex pane",
    );
}

fn question(text: &str, labels: &[&str]) -> AskQuestion {
    AskQuestion {
        question: text.to_owned(),
        options: labels
            .iter()
            .map(|label| AskOption::from((*label).to_owned()))
            .collect(),
        multi_select: false,
        has_option_previews: false,
    }
}

fn assert_invalid(result: Result<Vec<AnswerStep>, AnswerPlanErr>, needle: &str) {
    let AnswerPlanErr::Invalid(message) = result.expect_err("interaction must be rejected") else {
        panic!("expected Invalid");
    };
    assert!(
        message.contains(needle),
        "{message:?} should contain {needle:?}"
    );
}
