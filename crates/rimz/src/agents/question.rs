//! Shared normalization for compatible native questionnaire payloads.

use serde::Deserialize;
use serde_json::Value;

use crate::transcript::{AskOption, AskQuestion};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewPolicy {
    None,
    AnyValue,
    NonEmptyString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedQuestion {
    pub native_id: Option<String>,
    pub question: AskQuestion,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuestionInput {
    questions: Vec<QuestionWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuestionWire {
    id: Option<String>,
    header: Option<String>,
    question: Option<String>,
    options: Vec<OptionWire>,
    #[serde(alias = "multiSelect", alias = "multiple")]
    multi_select: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OptionWire {
    label: Option<String>,
    description: Option<String>,
    preview: Option<Value>,
}

pub(crate) fn decode(
    input: &Value,
    preview_policy: PreviewPolicy,
) -> Option<Vec<NormalizedQuestion>> {
    decode_inner(input, preview_policy, false)
}

pub(crate) fn decode_with_header_fallback(
    input: &Value,
    preview_policy: PreviewPolicy,
) -> Option<Vec<NormalizedQuestion>> {
    decode_inner(input, preview_policy, true)
}

fn decode_inner(
    input: &Value,
    preview_policy: PreviewPolicy,
    header_fallback: bool,
) -> Option<Vec<NormalizedQuestion>> {
    let input: QuestionInput = serde_json::from_value(input.clone()).ok()?;
    let questions = input
        .questions
        .into_iter()
        .filter_map(|question| normalize(question, preview_policy, header_fallback))
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

pub(crate) fn questions_with_header_fallback(
    input: &Value,
    preview_policy: PreviewPolicy,
) -> Option<Vec<AskQuestion>> {
    decode_with_header_fallback(input, preview_policy).map(|questions| {
        questions
            .into_iter()
            .map(|question| question.question)
            .collect()
    })
}

pub(crate) fn questions(input: &Value, preview_policy: PreviewPolicy) -> Option<Vec<AskQuestion>> {
    decode(input, preview_policy).map(|questions| {
        questions
            .into_iter()
            .map(|question| question.question)
            .collect()
    })
}

pub(crate) fn plan_question(plan: &str, options: Vec<AskOption>) -> Option<Vec<AskQuestion>> {
    let plan = non_empty(Some(plan))?;
    Some(vec![AskQuestion {
        question: format!("Requesting plan approval:\n\n{plan}"),
        options,
        multi_select: false,
        has_option_previews: false,
    }])
}

pub(crate) fn permission_detail(payload: &Value) -> Option<String> {
    let tool = non_empty(payload.get("tool_name").and_then(Value::as_str))?;
    let summary = payload
        .get("tool_input")
        .and_then(|input| serde_json::to_string(input).ok())
        .filter(|input| input != "{}" && input != "null")
        .map(|input| input.chars().take(160).collect::<String>());
    Some(match summary {
        Some(summary) => format!("{tool}: {summary}"),
        None => tool,
    })
}

fn normalize(
    question: QuestionWire,
    preview_policy: PreviewPolicy,
    header_fallback: bool,
) -> Option<NormalizedQuestion> {
    let question_text = non_empty(question.question.as_deref()).or_else(|| {
        header_fallback
            .then(|| non_empty(question.header.as_deref()))
            .flatten()
    })?;
    let has_option_previews = question
        .options
        .iter()
        .any(|option| preview_policy.matches(option.preview.as_ref()));
    let options = question
        .options
        .into_iter()
        .filter_map(|option| {
            Some(AskOption {
                label: non_empty(option.label.as_deref())?,
                description: non_empty(option.description.as_deref()),
                caution: None,
            })
        })
        .collect();
    Some(NormalizedQuestion {
        native_id: non_empty(question.id.as_deref()),
        question: AskQuestion {
            question: question_text,
            options,
            multi_select: question.multi_select,
            has_option_previews,
        },
    })
}

impl PreviewPolicy {
    fn matches(self, preview: Option<&Value>) -> bool {
        match self {
            Self::None => false,
            Self::AnyValue => preview.is_some(),
            Self::NonEmptyString => preview
                .and_then(Value::as_str)
                .is_some_and(|preview| !preview.is_empty()),
        }
    }
}

fn non_empty(text: Option<&str>) -> Option<String> {
    let text = text?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_all_multi_select_spellings_and_preview_policies() {
        let input = json!({
            "questions": [
                {
                    "id": " first ",
                    "question": " Pick? ",
                    "multi_select": true,
                    "options": [
                        {"label": " A ", "description": " useful ", "preview": 0},
                        {"label": " ", "preview": "ignored"}
                    ]
                },
                {
                    "header": " Fallback ",
                    "multiSelect": true,
                    "options": [{"label": "B", "preview": ""}]
                },
                {
                    "question": " OpenCode? ",
                    "multiple": true,
                    "options": [{"label": " C ", "description": "   ", "preview": 0}]
                },
                {"question": " "}
            ]
        });

        let any = decode_with_header_fallback(&input, PreviewPolicy::AnyValue).expect("questions");
        assert_eq!(any.len(), 3);
        assert_eq!(any[0].native_id.as_deref(), Some("first"));
        assert_eq!(any[0].question.options.len(), 1);
        assert_eq!(
            any[0].question.options[0].description.as_deref(),
            Some("useful")
        );
        assert_eq!(any[2].question.options[0].description, None);
        assert!(any.iter().all(|question| question.question.multi_select));
        assert!(
            any.iter()
                .all(|question| question.question.has_option_previews)
        );

        let strings = questions_with_header_fallback(&input, PreviewPolicy::NonEmptyString)
            .expect("questions");
        assert!(strings[0].has_option_previews);
        assert!(!strings[1].has_option_previews);
        assert!(!strings[2].has_option_previews);
        assert_eq!(questions(&input, PreviewPolicy::None).unwrap().len(), 2);
        assert!(questions(&json!({"questions": []}), PreviewPolicy::None).is_none());
    }

    #[test]
    fn permission_detail_suppresses_empty_input_and_bounds_summary() {
        for payload in [
            json!({"tool_name": " shell "}),
            json!({"tool_name": " shell ", "tool_input": {}}),
            json!({"tool_name": " shell ", "tool_input": null}),
        ] {
            assert_eq!(permission_detail(&payload).as_deref(), Some("shell"));
        }
        assert_eq!(permission_detail(&json!({})), None);
        assert_eq!(permission_detail(&json!({"tool_name": "  "})), None);

        let detail = permission_detail(&json!({
            "tool_name": "shell",
            "tool_input": {"command": "x".repeat(200)}
        }))
        .expect("detail");
        let summary = detail.strip_prefix("shell: ").expect("summary");
        assert_eq!(summary.chars().count(), 160);
        assert!(summary.starts_with(r#"{"command":"#));
    }
}
