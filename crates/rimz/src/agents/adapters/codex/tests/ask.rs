use serde_json::json;

use super::super::ask;
use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::pane::keys::NamedKey;
use crate::transcript::{AskOption, AskQuestion};

#[test]
fn async_reply_envelopes_keep_known_ids_and_suppress_legacy_prompts() {
    use crate::agents::testkit::hook_observation;
    let native_key = json!(["request_user_input_async", "call", 0]).to_string();
    for ids in [vec!["legacy", native_key.as_str()], vec!["legacy"]] {
        let replies = ids
            .iter()
            .map(|id| json!({"questionItemId":id,"answer":"Yes"}))
            .collect::<Vec<_>>();
        let prompt = format!(
            "<send_user_message_question_reply>{}</send_user_message_question_reply>",
            json!(replies)
        );
        let observation = hook_observation(
            &super::super::CodexAdapter,
            "UserPromptSubmit",
            &json!({"session_id":"s", "prompt":prompt}),
        )
        .unwrap();
        assert!(
            observation.prompt.is_none(),
            "reply envelope became the card prompt"
        );
        let edit = observation.ask_queue.unwrap();
        assert_eq!(edit.answered.len(), ids.len() - 1);
        if let Some(answer) = edit.answered.first() {
            assert_eq!(answer.native_key, native_key);
        }
    }
}

#[test]
fn async_hook_queues_each_accepted_question_and_decodes_replies() {
    use crate::agents::testkit::hook_observation;
    let payload = json!({"session_id":"01a0ebeb-6ef5-7fe1-ae29-b1dd960bf58e","turn_id":"01a0ebec-0d08-7412-9982-6e0b75039794","hook_event_name":"PostToolUse","tool_name":"request_user_input_async","tool_input":{"questions":[{"title":"Which format do you prefer for brief updates?","options":["Bullet points","Short paragraph"]},{"title":"How much detail should I include in routine replies?","options":["Minimal","Moderate","Detailed"]}]},"tool_response":"{\"accepted\":true}","tool_use_id":"call_f5WQLmsqAVg8rlqr4fedugbj"});
    let observation =
        hook_observation(&super::super::CodexAdapter, "PostToolUse", &payload).unwrap();
    let value = serde_json::to_value(observation).unwrap();
    assert_eq!(value["ask_queue"]["queued"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["ask_queue"]["queued"][0]["native_key"],
        "[\"request_user_input_async\",\"call_f5WQLmsqAVg8rlqr4fedugbj\",0]"
    );
    assert_eq!(
        value["ask_queue"]["queued"][0]["question"]["options"][1],
        "Short paragraph"
    );
    let sol = json!({"session_id":"01a0ebc7-9462-73d3-95a7-cc4b47d87fd0","tool_name":"request_user_input_async","tool_use_id":"call_SiRQhM2aUzaTDnaC7apJsjBT","tool_input":{"questions":[{"title":"What would you like me to do after you capture the state?","options":["Stop after the wait","Continue with a task you send"]}]},"tool_response":"{\"accepted\":true}"});
    let sol = hook_observation(&super::super::CodexAdapter, "PostToolUse", &sol)
        .unwrap()
        .ask_queue
        .unwrap();
    assert_eq!(sol.queued.len(), 1);
    assert_eq!(
        sol.queued[0].native_key,
        "[\"request_user_input_async\",\"call_SiRQhM2aUzaTDnaC7apJsjBT\",0]"
    );
    assert_eq!(
        sol.queued[0]
            .question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Stop after the wait", "Continue with a task you send"]
    );
    let prompt = "<send_user_message_question_reply>\n[{\"answer\":\"Bullet points\",\"question\":\"Which format do you prefer for brief updates?\",\"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"call_f5WQLmsqAVg8rlqr4fedugbj\\\",0]\"}]\n</send_user_message_question_reply>";
    let value = serde_json::to_value(
        hook_observation(
            &super::super::CodexAdapter,
            "UserPromptSubmit",
            &json!({"session_id":"s", "prompt":prompt}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(value["ask_queue"]["answered"][0]["answer"], "Bullet points");
    assert_eq!(
        value["ask_queue"]["answered"][0]["native_key"],
        "[\"request_user_input_async\",\"call_f5WQLmsqAVg8rlqr4fedugbj\",0]"
    );
    assert!(value["prompt"].is_null());
    let mut text_only = payload.clone();
    text_only["tool_input"] = json!({"questions":[{"title":"Explain?"}]});
    let value = serde_json::to_value(
        hook_observation(&super::super::CodexAdapter, "PostToolUse", &text_only).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["ask_queue"]["queued"][0]["question"]["question"],
        "Explain?"
    );
    for response in [json!("{\"accepted\":false}"), json!(null)] {
        text_only["tool_response"] = response;
        let value = serde_json::to_value(
            hook_observation(&super::super::CodexAdapter, "PostToolUse", &text_only).unwrap(),
        )
        .unwrap();
        assert!(value["ask_queue"].is_null());
    }
    let value = serde_json::to_value(
        hook_observation(
            &super::super::CodexAdapter,
            "UserPromptSubmit",
            &json!({"session_id":"s","prompt":"ordinary prompt"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(value["ask_queue"].is_null());
}

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
