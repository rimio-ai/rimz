use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskUserQuestionResponse {
    #[serde(deserialize_with = "null_to_default")]
    annotations: Map<String, Value>,
    #[serde(deserialize_with = "null_to_default")]
    answers: Map<String, Value>,
    #[serde(deserialize_with = "null_to_default")]
    questions: Value,
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
    super::super::question::questions(tool_input, super::super::question::PreviewPolicy::AnyValue)
}

fn exit_plan_mode_detail(tool_input: &Value) -> Option<Vec<AskQuestion>> {
    super::super::question::plan_question(tool_input.get("plan")?.as_str()?, plan_options())
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
    let normalized = super::super::question::decode(
        &serde_json::json!({"questions": parsed.questions}),
        super::super::question::PreviewPolicy::AnyValue,
    )
    .unwrap_or_default();
    for question in normalized {
        let question_text = question.question.question;
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
    fn question_detail_normalizes_supported_prompts() {
        let cases = [
            (
                "AskUserQuestion",
                json!({
                    "questions": [
                        {
                            "question": " Choose deployment path? ",
                            "multiSelect": true,
                            "options": [
                                {
                                    "label": " safe ",
                                    "description": " Use staged rollout ",
                                    "preview": { "command": "deploy --staged" }
                                },
                                { "label": " fast ", "description": "   " },
                                { "label": "  ", "description": "ignored" }
                            ]
                        },
                        {
                            "question": " Notify team? ",
                            "options": []
                        }
                    ]
                }),
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
                        has_option_previews: true,
                    },
                    AskQuestion {
                        question: "Notify team?".to_owned(),
                        options: Vec::new(),
                        multi_select: false,
                        has_option_previews: false,
                    },
                ]),
            ),
            (
                "ExitPlanMode",
                json!({ "plan": " 1. Edit parser\n2. Run tests " }),
                Some(vec![AskQuestion {
                    question: "Requesting plan approval:\n\n1. Edit parser\n2. Run tests"
                        .to_owned(),
                    options: vec![AskOption {
                        label: "approve".to_owned(),
                        description: Some("Approve in Claude with auto-accept edits".to_owned()),
                        caution: Some("enables auto-accept for subsequent edits".to_owned()),
                    }],
                    multi_select: false,
                    has_option_previews: false,
                }]),
            ),
            ("AskUserQuestion", json!({ "questions": "invalid" }), None),
            ("ExitPlanMode", json!({ "plan": "   " }), None),
            ("Bash", json!({ "command": "true" }), None),
        ];

        for (tool_name, input, expected) in cases {
            assert_eq!(question_detail(tool_name, &input), expected, "{tool_name}");
        }
    }

    #[test]
    fn answer_detail_preserves_live_question_context() {
        let cases = [
            (
                "observed answer map",
                json!({
                    "annotations": {
                        "Choose deployment path?": { "notes": " use prod window " }
                    },
                    "answers": {
                        "Choose scopes?": ["a", { "label": "b" }],
                        "Notify team?": "yes",
                        "Choose deployment path?": "safe"
                    },
                    "questions": [
                        {
                            "question": "Choose deployment path?",
                            "header": "Path",
                            "options": [{ "label": "safe" }, { "label": "fast" }]
                        },
                        { "question": "Notify team?" },
                        { "question": "Choose scopes?" }
                    ]
                }),
                vec![
                    AskAnswer {
                        question: Some("Choose deployment path?".to_owned()),
                        chosen: vec!["safe".to_owned()],
                        note: Some("use prod window".to_owned()),
                    },
                    AskAnswer {
                        question: Some("Notify team?".to_owned()),
                        chosen: vec!["yes".to_owned()],
                        note: None,
                    },
                    AskAnswer {
                        question: Some("Choose scopes?".to_owned()),
                        chosen: vec!["a".to_owned(), "b".to_owned()],
                        note: None,
                    },
                ],
            ),
            (
                "nullable live fields",
                json!({
                    "annotations": null,
                    "answers": { "Choose deployment path?": "safe" },
                    "questions": null
                }),
                vec![AskAnswer {
                    question: Some("Choose deployment path?".to_owned()),
                    chosen: vec!["safe".to_owned()],
                    note: None,
                }],
            ),
        ];

        for (case, response, expected) in cases {
            assert_eq!(
                answer_detail("AskUserQuestion", &response),
                Some(expected),
                "{case}"
            );
        }
    }

    #[test]
    fn answer_detail_keeps_readable_fallbacks() {
        let cases = [
            (
                "AskUserQuestion",
                json!("safe"),
                Some(vec![AskAnswer {
                    question: None,
                    chosen: vec!["safe".to_owned()],
                    note: None,
                }]),
            ),
            (
                "AskUserQuestion",
                json!({ "unexpected": ["shape"] }),
                Some(vec![AskAnswer {
                    question: None,
                    chosen: vec![r#"{"unexpected":["shape"]}"#.to_owned()],
                    note: None,
                }]),
            ),
            (
                "ExitPlanMode",
                json!({}),
                Some(vec![AskAnswer {
                    question: None,
                    chosen: vec!["approved plan".to_owned()],
                    note: None,
                }]),
            ),
            ("Bash", json!("ok"), None),
        ];

        for (tool_name, response, expected) in cases {
            assert_eq!(answer_detail(tool_name, &response), expected, "{tool_name}");
        }
    }

    #[test]
    fn question_answer_plan_emits_supported_menu_sequences() {
        let cases = [
            (
                "preview selection",
                vec![question("Path?", &["safe", "fast"], false, true)],
                vec![AskReply {
                    picks: vec![0],
                    ..AskReply::default()
                }],
                vec![
                    AnswerStep::Text("1".to_owned()),
                    AnswerStep::Key(NamedKey::Enter),
                ],
            ),
            (
                "free-text Other",
                vec![question("Path?", &["safe", "fast"], false, false)],
                vec![AskReply {
                    text: Some("stage it".to_owned()),
                    ..AskReply::default()
                }],
                vec![
                    AnswerStep::Text("3".to_owned()),
                    AnswerStep::Paste("stage it".to_owned()),
                    AnswerStep::Key(NamedKey::Enter),
                ],
            ),
            (
                "ordinary and multiselect review",
                vec![
                    question("Path?", &["safe", "fast"], false, false),
                    question("Scopes?", &["read", "write"], true, false),
                ],
                vec![
                    AskReply {
                        picks: vec![1],
                        ..AskReply::default()
                    },
                    AskReply {
                        picks: vec![0, 1],
                        ..AskReply::default()
                    },
                ],
                vec![
                    AnswerStep::Text("2".to_owned()),
                    AnswerStep::Text("1".to_owned()),
                    AnswerStep::Text("2".to_owned()),
                    AnswerStep::Key(NamedKey::Down),
                    AnswerStep::Key(NamedKey::Down),
                    AnswerStep::Key(NamedKey::Down),
                    AnswerStep::Key(NamedKey::Enter),
                    AnswerStep::Key(NamedKey::Enter),
                ],
            ),
        ];

        for (case, questions, answers, expected) in cases {
            assert_eq!(
                answer_plan(AskKind::Question, &questions, &answers).unwrap(),
                expected,
                "{case}"
            );
        }

        let error = answer_plan(
            AskKind::Question,
            &[question("Path?", &["safe", "fast"], false, false)],
            &[AskReply {
                picks: vec![0],
                text: Some("stage it".to_owned()),
            }],
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "picks and text can be combined only on a multi-select question"
        );
    }

    fn question(
        question: &str,
        options: &[&str],
        multi_select: bool,
        has_option_previews: bool,
    ) -> AskQuestion {
        AskQuestion {
            question: question.to_owned(),
            options: options
                .iter()
                .map(|option| AskOption::from((*option).to_owned()))
                .collect(),
            multi_select,
            has_option_previews,
        }
    }
}
