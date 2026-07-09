use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskUserQuestionInput {
    questions: Vec<ClaudeAskQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaudeAskQuestion {
    question: Option<String>,
    options: Vec<ClaudeAskOption>,
    #[serde(rename = "multiSelect")]
    multi_select: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClaudeAskOption {
    label: Option<String>,
    description: Option<String>,
    preview: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskUserQuestionResponse {
    #[serde(deserialize_with = "null_to_default")]
    annotations: Map<String, Value>,
    #[serde(deserialize_with = "null_to_default")]
    answers: Map<String, Value>,
    #[serde(deserialize_with = "null_to_default")]
    questions: Vec<ClaudeAskQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExitPlanModeInput {
    plan: Option<String>,
}

pub(super) fn question_detail(tool_name: &str, tool_input: &Value) -> Option<Vec<AskQuestion>> {
    match tool_name {
        "AskUserQuestion" => ask_user_question_detail(tool_input),
        "ExitPlanMode" => exit_plan_mode_detail(tool_input),
        _ => None,
    }
}

pub(super) fn answer_detail(tool_name: &str, tool_response: &Value) -> Option<Vec<AskAnswer>> {
    match tool_name {
        "AskUserQuestion" => ask_user_question_answer(tool_response),
        "ExitPlanMode" => Some(vec![AskAnswer {
            question: None,
            chosen: vec!["approved plan".to_owned()],
            note: None,
        }]),
        _ => None,
    }
}

fn ask_user_question_detail(tool_input: &Value) -> Option<Vec<AskQuestion>> {
    let parsed: AskUserQuestionInput = serde_json::from_value(tool_input.clone()).ok()?;
    let questions = parsed
        .questions
        .into_iter()
        .filter_map(structured_question)
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

fn structured_question(question: ClaudeAskQuestion) -> Option<AskQuestion> {
    let question_text = non_empty(question.question.as_deref())?;
    let has_option_previews = question
        .options
        .iter()
        .any(|option| option.preview.is_some());
    let options = question
        .options
        .into_iter()
        .filter_map(|option| {
            Some(AskOption {
                label: non_empty(option.label.as_deref())?,
                description: option
                    .description
                    .as_deref()
                    .and_then(|description| non_empty(Some(description))),
                caution: None,
            })
        })
        .collect::<Vec<_>>();
    Some(AskQuestion {
        question: question_text,
        options,
        multi_select: question.multi_select,
        has_option_previews,
    })
}

fn exit_plan_mode_detail(tool_input: &Value) -> Option<Vec<AskQuestion>> {
    let parsed: ExitPlanModeInput = serde_json::from_value(tool_input.clone()).ok()?;
    let plan = non_empty(parsed.plan.as_deref())?;
    Some(vec![AskQuestion {
        question: format!("Requesting plan approval:\n\n{plan}"),
        options: plan_options(),
        multi_select: false,
        has_option_previews: false,
    }])
}

pub(super) fn permission_options() -> Vec<AskOption> {
    vec![AskOption::from("allow".to_owned())]
}

pub(super) fn plan_options() -> Vec<AskOption> {
    vec![AskOption {
        label: "approve".to_owned(),
        description: Some("Approve in Claude with auto-accept edits".to_owned()),
        caution: Some("enables auto-accept for subsequent edits".to_owned()),
    }]
}

pub(super) fn permission_detail(payload: &Value) -> Option<String> {
    let tool = payload.get("tool_name")?.as_str()?.trim();
    if tool.is_empty() {
        return None;
    }
    let summary = payload
        .get("tool_input")
        .and_then(|input| serde_json::to_string(input).ok())
        .map(|input| input.chars().take(160).collect::<String>())
        .filter(|input| input != "{}" && input != "null");
    Some(match summary {
        Some(summary) => format!("{tool}: {summary}"),
        None => tool.to_owned(),
    })
}

pub(super) fn answer_plan(
    kind: AskKind,
    questions: &[AskQuestion],
    answers: &[AskReply],
) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    match kind {
        AskKind::Permission => permission_answer_plan(answers),
        AskKind::PlanApproval => plan_approval_answer_plan(answers),
        AskKind::Question => question_answer_plan(questions, answers),
    }
}

fn permission_answer_plan(answers: &[AskReply]) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    let [answer] = answers else {
        return Err(AnswerPlanErr::Invalid(
            "permission asks require exactly one answer".to_owned(),
        ));
    };
    match answer.picks.as_slice() {
        [0] if answer.text.is_none() => Ok(vec![AnswerStep::Text("1".to_owned())]),
        _ => Err(AnswerPlanErr::Invalid(
            "permission asks accept only `allow`; deny and persistent grants require the Claude pane"
                .to_owned(),
        )),
    }
}

fn plan_approval_answer_plan(answers: &[AskReply]) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    let [answer] = answers else {
        return Err(AnswerPlanErr::Invalid(
            "plan approvals require exactly one answer".to_owned(),
        ));
    };
    match answer.picks.as_slice() {
        [0] if answer.text.is_none() => Ok(vec![AnswerStep::Key(NamedKey::ShiftTab)]),
        _ => Err(AnswerPlanErr::Invalid(
            "plan approvals accept only `approve`; keep-planning, refinement text, and manual-review approval require the Claude pane"
                .to_owned(),
        )),
    }
}

fn question_answer_plan(
    questions: &[AskQuestion],
    answers: &[AskReply],
) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    if questions.len() != answers.len() {
        return Err(AnswerPlanErr::Invalid(format!(
            "expected {} answers, got {}",
            questions.len(),
            answers.len()
        )));
    }
    let mut steps = Vec::new();
    let mut needs_review = questions.len() > 1;
    for (question, answer) in questions.iter().zip(answers) {
        if answer.text.is_some() && !answer.picks.is_empty() && !question.multi_select {
            return Err(AnswerPlanErr::Invalid(
                "picks and text can be combined only on a multi-select question".to_owned(),
            ));
        }
        if let Some(text) = answer.text.as_ref() {
            steps.push(AnswerStep::Text((question.options.len() + 1).to_string()));
            steps.push(AnswerStep::Paste(text.clone()));
            steps.push(AnswerStep::Key(NamedKey::Enter));
        }
        for pick in &answer.picks {
            steps.push(AnswerStep::Text((pick + 1).to_string()));
            if question.has_option_previews {
                steps.push(AnswerStep::Key(NamedKey::Enter));
            }
        }
        if question.multi_select {
            needs_review = true;
            let down_count = if answer.text.is_some() {
                1
            } else {
                question.options.len() + 1
            };
            steps.extend(std::iter::repeat_n(
                AnswerStep::Key(NamedKey::Down),
                down_count,
            ));
            steps.push(AnswerStep::Key(NamedKey::Enter));
        }
    }
    if needs_review {
        steps.push(AnswerStep::Key(NamedKey::Enter));
    }
    Ok(steps)
}

fn ask_user_question_answer(tool_response: &Value) -> Option<Vec<AskAnswer>> {
    // Claude Code has not documented this PostToolUse shape yet
    // (anthropics/claude-code#12605); refine these fields as the wire settles.
    value_text(tool_response)
        .map(single_answer)
        .or_else(|| answers_map_detail(tool_response))
        .or_else(|| object_answer_field(tool_response, "answers").map(single_answer))
        .or_else(|| object_answer_field(tool_response, "choices").map(single_answer))
        .or_else(|| object_answer_field(tool_response, "selectedOptions").map(single_answer))
        .or_else(|| serde_json::to_string(tool_response).ok().map(single_answer))
}

fn single_answer(text: String) -> Vec<AskAnswer> {
    vec![AskAnswer {
        question: None,
        chosen: vec![text],
        note: None,
    }]
}

fn answers_map_detail(tool_response: &Value) -> Option<Vec<AskAnswer>> {
    let parsed: AskUserQuestionResponse = serde_json::from_value(tool_response.clone()).ok()?;
    if parsed.answers.is_empty() {
        return None;
    }

    let mut answers = parsed.answers;
    let mut entries = Vec::new();
    for question in parsed.questions {
        let Some(question_text) = non_empty(question.question.as_deref()) else {
            continue;
        };
        if let Some(value) = answers.remove(&question_text)
            && let Some(answer) = answer_entry(Some(question_text), &value, &parsed.annotations)
        {
            entries.push(answer);
        }
    }
    for (question, value) in answers {
        if let Some(answer) = answer_entry(Some(question), &value, &parsed.annotations) {
            entries.push(answer);
        }
    }

    (!entries.is_empty()).then_some(entries)
}

fn answer_entry(
    question: Option<String>,
    value: &Value,
    annotations: &Map<String, Value>,
) -> Option<AskAnswer> {
    let chosen = answer_value_choices(value);
    if chosen.is_empty() {
        return None;
    }
    let note = question
        .as_deref()
        .and_then(|question| answer_note(question, annotations));
    Some(AskAnswer {
        question,
        chosen,
        note,
    })
}

fn answer_note(question: &str, annotations: &Map<String, Value>) -> Option<String> {
    annotations
        .get(question)?
        .get("notes")
        .and_then(Value::as_str)
        .and_then(|notes| non_empty(Some(notes)))
}

fn object_answer_field(value: &Value, key: &str) -> Option<String> {
    let field = value.get(key)?;
    answer_value_text(field)
}

fn answer_value_choices(value: &Value) -> Vec<String> {
    if let Some(values) = value.as_array() {
        return values.iter().filter_map(answer_value_text).collect();
    }
    answer_value_text(value).into_iter().collect()
}

fn answer_value_text(value: &Value) -> Option<String> {
    value_text(value).or_else(|| {
        if let Some(values) = value.as_array() {
            let rendered = values
                .iter()
                .filter_map(answer_value_text)
                .collect::<Vec<_>>()
                .join(", ");
            (!rendered.is_empty()).then_some(rendered)
        } else {
            value.as_object()?.iter().find_map(|(key, value)| {
                matches!(
                    key.as_str(),
                    "answer" | "choice" | "label" | "text" | "value" | "name"
                )
                .then(|| answer_value_text(value))
                .flatten()
            })
        }
    })
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty(Some(text)),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn non_empty(text: Option<&str>) -> Option<String> {
    let text = text?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_user_question_detail_renders_questions_and_options() {
        let questions = question_detail(
            "AskUserQuestion",
            &json!({
                "questions": [
                    {
                        "question": "Choose deployment path?",
                        "multiSelect": true,
                        "options": [
                            {
                                "label": "safe",
                                "description": "Use staged rollout"
                            },
                            { "label": "fast", "description": "   " },
                            { "label": "  " }
                        ]
                    },
                    {
                        "question": "Notify team?",
                        "options": []
                    }
                ]
            }),
        );

        assert_eq!(
            questions,
            Some(vec![
                AskQuestion {
                    question: "Choose deployment path?".to_owned(),
                    options: vec![
                        AskOption {
                            label: "safe".to_owned(),
                            description: Some("Use staged rollout".to_owned()),
                            caution: None,
                        },
                        AskOption {
                            label: "fast".to_owned(),
                            description: None,
                            caution: None,
                        },
                    ],
                    multi_select: true,
                    has_option_previews: false,
                },
                AskQuestion {
                    question: "Notify team?".to_owned(),
                    options: Vec::new(),
                    multi_select: false,
                    has_option_previews: false,
                },
            ])
        );
    }

    #[test]
    fn exit_plan_mode_detail_renders_plan_approval_request() {
        let questions = question_detail(
            "ExitPlanMode",
            &json!({ "plan": "1. Edit parser\n2. Run tests" }),
        );

        assert_eq!(
            questions,
            Some(vec![AskQuestion {
                question: "Requesting plan approval:\n\n1. Edit parser\n2. Run tests".to_owned(),
                options: plan_options(),
                multi_select: false,
                has_option_previews: false,
            }])
        );
    }

    #[test]
    fn ask_user_question_answer_prefers_readable_fields() {
        assert_eq!(
            answer_detail("AskUserQuestion", &json!("safe")),
            Some(vec![AskAnswer {
                question: None,
                chosen: vec!["safe".to_owned()],
                note: None,
            }])
        );
        assert_eq!(
            answer_detail(
                "AskUserQuestion",
                &json!({ "selectedOptions": [{ "label": "fast" }, { "label": "notify" }] })
            ),
            Some(vec![AskAnswer {
                question: None,
                chosen: vec!["fast, notify".to_owned()],
                note: None,
            }])
        );
        assert_eq!(
            answer_detail("AskUserQuestion", &json!({ "unexpected": ["shape"] })),
            Some(vec![AskAnswer {
                question: None,
                chosen: vec![r#"{"unexpected":["shape"]}"#.to_owned()],
                note: None,
            }])
        );
    }

    #[test]
    fn ask_user_question_answer_renders_live_answer_map() {
        let answer = answer_detail(
            "AskUserQuestion",
            &json!({
                "annotations": {},
                "answers": { "Choose deployment path?": "Live repro first" },
                "questions": [{
                    "question": "Choose deployment path?",
                    "header": "Path",
                    "options": [{ "label": "safe" }, { "label": "fast" }]
                }]
            }),
        );

        assert_eq!(
            answer,
            Some(vec![AskAnswer {
                question: Some("Choose deployment path?".to_owned()),
                chosen: vec!["Live repro first".to_owned()],
                note: None,
            }])
        );
    }

    #[test]
    fn ask_user_question_answer_orders_live_answer_map_by_questions() {
        let answer = answer_detail(
            "AskUserQuestion",
            &json!({
                "annotations": {},
                "answers": {
                    "Notify team?": "yes",
                    "Choose deployment path?": "safe"
                },
                "questions": [
                    { "question": "Choose deployment path?" },
                    { "question": "Notify team?" }
                ]
            }),
        );

        assert_eq!(
            crate::transcript::answers_text(&answer.expect("answer")),
            "safe\nyes"
        );
    }

    #[test]
    fn ask_user_question_answer_renders_multiselect_arrays() {
        let answer = answer_detail(
            "AskUserQuestion",
            &json!({
                "annotations": {},
                "answers": {
                    "Choose scopes?": ["a", { "label": "b" }]
                },
                "questions": [{ "question": "Choose scopes?" }]
            }),
        );

        assert_eq!(
            answer,
            Some(vec![AskAnswer {
                question: Some("Choose scopes?".to_owned()),
                chosen: vec!["a".to_owned(), "b".to_owned()],
                note: None,
            }])
        );
    }

    #[test]
    fn ask_user_question_answer_appends_annotation_notes() {
        let answer = answer_detail(
            "AskUserQuestion",
            &json!({
                "annotations": {
                    "Choose deployment path?": { "notes": "use prod window" }
                },
                "answers": {
                    "Choose deployment path?": "safe"
                },
                "questions": [{ "question": "Choose deployment path?" }]
            }),
        );

        assert_eq!(
            answer,
            Some(vec![AskAnswer {
                question: Some("Choose deployment path?".to_owned()),
                chosen: vec!["safe".to_owned()],
                note: Some("use prod window".to_owned()),
            }])
        );
    }

    #[test]
    fn ask_user_question_answer_tolerates_null_live_fields() {
        let answer = answer_detail(
            "AskUserQuestion",
            &json!({
                "annotations": null,
                "answers": { "Choose deployment path?": "safe" },
                "questions": null
            }),
        );

        assert_eq!(
            answer,
            Some(vec![AskAnswer {
                question: Some("Choose deployment path?".to_owned()),
                chosen: vec!["safe".to_owned()],
                note: None,
            }])
        );
    }

    #[test]
    fn answer_summary_handles_plan_approval() {
        assert_eq!(
            answer_detail("ExitPlanMode", &json!({})),
            Some(vec![AskAnswer {
                question: None,
                chosen: vec!["approved plan".to_owned()],
                note: None,
            }])
        );
        assert!(answer_detail("Bash", &json!("ok")).is_none());
    }

    #[test]
    fn permission_and_plan_answers_use_confirmed_menu_actions() {
        assert_eq!(
            answer_plan(
                AskKind::Permission,
                &[],
                &[AskReply {
                    picks: vec![0],
                    ..AskReply::default()
                }],
            )
            .unwrap(),
            vec![AnswerStep::Text("1".to_owned())]
        );
        assert_eq!(
            answer_plan(
                AskKind::PlanApproval,
                &[],
                &[AskReply {
                    picks: vec![0],
                    ..AskReply::default()
                }],
            )
            .unwrap(),
            vec![AnswerStep::Key(NamedKey::ShiftTab)]
        );
    }

    #[test]
    fn permission_and_plan_answers_reject_unlisted_actions() {
        for (kind, message) in [
            (AskKind::Permission, "deny"),
            (AskKind::PlanApproval, "keep-planning"),
        ] {
            let error = answer_plan(
                kind,
                &[],
                &[AskReply {
                    picks: vec![1],
                    ..AskReply::default()
                }],
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(message));
            assert!(error.contains("Claude pane"));
        }
    }

    #[test]
    fn question_answer_plan_selects_and_confirms() {
        let questions = vec![
            AskQuestion {
                question: "Path?".to_owned(),
                options: vec![
                    AskOption::from("safe".to_owned()),
                    AskOption::from("fast".to_owned()),
                ],
                multi_select: false,
                has_option_previews: false,
            },
            AskQuestion {
                question: "Scopes?".to_owned(),
                options: vec![
                    AskOption::from("read".to_owned()),
                    AskOption::from("write".to_owned()),
                ],
                multi_select: true,
                has_option_previews: false,
            },
        ];
        let answers = vec![
            AskReply {
                picks: vec![1],
                ..AskReply::default()
            },
            AskReply {
                picks: vec![0, 1],
                ..AskReply::default()
            },
        ];

        assert_eq!(
            answer_plan(AskKind::Question, &questions, &answers).unwrap(),
            vec![
                AnswerStep::Text("2".to_owned()),
                AnswerStep::Text("1".to_owned()),
                AnswerStep::Text("2".to_owned()),
                AnswerStep::Key(NamedKey::Down),
                AnswerStep::Key(NamedKey::Down),
                AnswerStep::Key(NamedKey::Down),
                AnswerStep::Key(NamedKey::Enter),
                AnswerStep::Key(NamedKey::Enter),
            ]
        );
    }

    #[test]
    fn question_answer_plan_respects_preview_and_other_input_contracts() {
        let ordinary = AskQuestion {
            question: "Path?".to_owned(),
            options: vec![
                AskOption::from("safe".to_owned()),
                AskOption::from("fast".to_owned()),
            ],
            multi_select: false,
            has_option_previews: false,
        };
        assert_eq!(
            answer_plan(
                AskKind::Question,
                std::slice::from_ref(&ordinary),
                &[AskReply {
                    picks: vec![1],
                    ..AskReply::default()
                }],
            )
            .unwrap(),
            vec![AnswerStep::Text("2".to_owned())]
        );

        let preview = AskQuestion {
            has_option_previews: true,
            ..ordinary.clone()
        };
        assert_eq!(
            answer_plan(
                AskKind::Question,
                &[preview],
                &[AskReply {
                    picks: vec![0],
                    ..AskReply::default()
                }],
            )
            .unwrap(),
            vec![
                AnswerStep::Text("1".to_owned()),
                AnswerStep::Key(NamedKey::Enter),
            ]
        );

        assert_eq!(
            answer_plan(
                AskKind::Question,
                &[ordinary],
                &[AskReply {
                    text: Some("stage it".to_owned()),
                    ..AskReply::default()
                }],
            )
            .unwrap(),
            vec![
                AnswerStep::Text("3".to_owned()),
                AnswerStep::Paste("stage it".to_owned()),
                AnswerStep::Key(NamedKey::Enter),
            ]
        );
    }
}
