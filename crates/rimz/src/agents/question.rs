//! Shared normalization for compatible native questionnaire payloads.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::transcript::{AskOption, AskQuestion};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreviewPolicy {
    None,
    AnyValue,
    NonEmptyString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedQuestion {
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

pub(super) fn decode(
    input: &Value,
    preview_policy: PreviewPolicy,
) -> Option<Vec<NormalizedQuestion>> {
    decode_inner(input, preview_policy, false)
}

pub(super) fn decode_with_header_fallback(
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
    let mut input = input.clone();
    if let Some(questions) = input.get_mut("questions") {
        decode_container_string(questions);
        for question in questions.as_array_mut().into_iter().flatten() {
            decode_container_string(question);
            let Some(options) = question.get_mut("options") else {
                continue;
            };
            decode_container_string(options);
            for option in options.as_array_mut().into_iter().flatten() {
                decode_container_string(option);
            }
        }
    }
    let input: QuestionInput = serde_json::from_value(input).ok()?;
    let questions = input
        .questions
        .into_iter()
        .filter_map(|question| normalize(question, preview_policy, header_fallback))
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

fn decode_container_string(value: &mut Value) {
    if let Some(encoded) = value.as_str()
        && let Ok(parsed) = serde_json::from_str::<Value>(encoded)
        && (parsed.is_array() || parsed.is_object())
    {
        *value = parsed;
    }
}

pub(super) fn questions_with_header_fallback(
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

pub(super) fn questions(input: &Value, preview_policy: PreviewPolicy) -> Option<Vec<AskQuestion>> {
    decode(input, preview_policy).map(|questions| {
        questions
            .into_iter()
            .map(|question| question.question)
            .collect()
    })
}

pub(super) fn plan_question(plan: &str, options: Vec<AskOption>) -> Option<Vec<AskQuestion>> {
    let plan = non_empty(Some(plan))?;
    Some(vec![AskQuestion {
        question: format!("Requesting plan approval:\n\n{plan}"),
        options,
        multi_select: false,
        has_option_previews: false,
    }])
}

/// How a permission summary prints the input field it picked.
#[derive(Clone, Copy)]
enum TargetKind {
    /// Free text, with every whitespace run folded to one space.
    Text,
    /// A path, shown relative to the payload's `cwd` when it lies inside it.
    Path,
}

/// Input fields that name what a tool call acts on, in the order a summary
/// tries them: a search tool's `path` only scopes its `pattern`, so it is
/// last. The names cover the Claude, Qwen, and Qoder tool inputs.
const PERMISSION_TARGETS: [(&str, TargetKind); 8] = [
    ("command", TargetKind::Text),
    ("file_path", TargetKind::Path),
    ("notebook_path", TargetKind::Path),
    ("absolute_path", TargetKind::Path),
    ("url", TargetKind::Text),
    ("pattern", TargetKind::Text),
    ("query", TargetKind::Text),
    ("path", TargetKind::Path),
];

/// Longest permission summary after the tool name, in characters.
const PERMISSION_SUMMARY_CHARS: usize = 160;

/// The one-line detail of a permission ask: the tool name, then the target
/// the call acts on, or the input's compact JSON when no field in
/// [`PERMISSION_TARGETS`] holds a nonblank string.
pub(super) fn permission_detail(payload: &Value) -> Option<String> {
    let tool = non_empty(payload.get("tool_name").and_then(Value::as_str))?;
    let Some(input) = payload.get("tool_input").filter(|input| {
        !input.is_null() && input.as_object().is_none_or(|fields| !fields.is_empty())
    }) else {
        return Some(tool);
    };
    let cwd = payload.get("cwd").and_then(Value::as_str).map(Path::new);
    let summary = permission_target(input, cwd).unwrap_or_else(|| input.to_string());
    Some(format!("{tool}: {}", bounded_summary(&summary)))
}

fn permission_target(input: &Value, cwd: Option<&Path>) -> Option<String> {
    PERMISSION_TARGETS.iter().find_map(|&(field, kind)| {
        let value = input.get(field)?.as_str()?;
        match kind {
            TargetKind::Text => {
                let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
                (!text.is_empty()).then_some(text)
            }
            TargetKind::Path => {
                let path = non_empty(Some(value))?;
                Some(display_path(&path, cwd))
            }
        }
    })
}

fn display_path(path: &str, cwd: Option<&Path>) -> String {
    cwd.and_then(|cwd| Path::new(path).strip_prefix(cwd).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .map_or_else(
            || path.to_owned(),
            |relative| relative.display().to_string(),
        )
}

fn bounded_summary(summary: &str) -> String {
    if summary.chars().count() <= PERMISSION_SUMMARY_CHARS {
        return summary.to_owned();
    }
    let mut head = summary
        .chars()
        .take(PERMISSION_SUMMARY_CHARS - 1)
        .collect::<String>();
    head.push('…');
    head
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
    fn decodes_json_strings_at_questionnaire_container_levels() {
        let input = json!({"questions": [{
            "question": "Pick?", "options": [{"label": "A"}], "multiSelect": true
        }]});
        let expected = decode(&input, PreviewPolicy::None);
        for pointer in [
            "/questions",
            "/questions/0",
            "/questions/0/options",
            "/questions/0/options/0",
        ] {
            let mut encoded = input.clone();
            let value = encoded.pointer_mut(pointer).unwrap();
            *value = Value::String(value.to_string());
            assert_eq!(decode(&encoded, PreviewPolicy::None), expected, "{pointer}");
        }
        let nested = json!({"questions": serde_json::to_string(&vec![
            json!({"question": "Pick?", "options": serde_json::to_string(&vec![
                json!({"label": "A"}).to_string()
            ]).unwrap(), "multiSelect": true}).to_string()
        ]).unwrap()});
        assert_eq!(decode(&nested, PreviewPolicy::None), expected);
    }

    #[test]
    fn rejects_invalid_strings_at_questionnaire_container_levels() {
        let input = json!({"questions": [{"question": "Pick?", "options": [{"label": "A"}]}]});
        for pointer in [
            "/questions",
            "/questions/0",
            "/questions/0/options",
            "/questions/0/options/0",
        ] {
            for invalid in ["not json", "null", "42", "true", "\"text\""] {
                let mut encoded = input.clone();
                *encoded.pointer_mut(pointer).unwrap() = json!(invalid);
                assert_eq!(
                    decode(&encoded, PreviewPolicy::None),
                    None,
                    "{pointer}: {invalid}"
                );
            }
        }
        assert_eq!(decode(&json!(input.to_string()), PreviewPolicy::None), None);
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
        assert!(summary.starts_with("xxx"));
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn permission_detail_names_the_target_and_falls_back_to_json() {
        let cwd = "/work/tree";
        for (tool_input, expected) in [
            (
                json!({"command": "cargo test \\\n  --all", "description": "Run tests"}),
                "Bash: cargo test \\ --all",
            ),
            (
                json!({"file_path": "/work/tree/.rimz/team/notes.md", "old_string": "a", "new_string": ""}),
                "Bash: .rimz/team/notes.md",
            ),
            (
                // `content` sorts before `file_path` in the JSON: the path still leads.
                json!({"content": "# Review\n\nverdict", "file_path": "/work/tree/review.md"}),
                "Bash: review.md",
            ),
            (
                json!({"file_path": "/elsewhere/notes.md"}),
                "Bash: /elsewhere/notes.md",
            ),
            (json!({"path": "/work/tree"}), "Bash: /work/tree"),
            (
                json!({"path": "/work/treehouse/a"}),
                "Bash: /work/treehouse/a",
            ),
            (
                json!({"pattern": "fn main", "path": "/work/tree/src"}),
                "Bash: fn main",
            ),
            (
                json!({"url": "https://docs.rs/serde"}),
                "Bash: https://docs.rs/serde",
            ),
            (json!({"query": "  rust\tserde "}), "Bash: rust serde"),
            // A blank or non-string target field moves on to the next field.
            (
                json!({"command": "  ", "file_path": "/work/tree/a.rs"}),
                "Bash: a.rs",
            ),
            (
                json!({"command": ["ls"], "url": "https://x.test"}),
                "Bash: https://x.test",
            ),
            // No readable target: the input's compact JSON.
            (json!({"command": 7}), r#"Bash: {"command":7}"#),
            (
                json!({"prompt": "review it"}),
                r#"Bash: {"prompt":"review it"}"#,
            ),
            (json!(["ls"]), r#"Bash: ["ls"]"#),
        ] {
            let payload = json!({"tool_name": "Bash", "cwd": cwd, "tool_input": tool_input});
            assert_eq!(
                permission_detail(&payload).as_deref(),
                Some(expected),
                "{tool_input}"
            );
        }

        let without_cwd =
            json!({"tool_name": "Edit", "tool_input": {"file_path": "/work/tree/a.rs"}});
        assert_eq!(
            permission_detail(&without_cwd).as_deref(),
            Some("Edit: /work/tree/a.rs")
        );
        let short_json = permission_detail(
            &json!({"tool_name": "Task", "tool_input": {"prompt": "y".repeat(200)}}),
        )
        .expect("detail");
        let summary = short_json.strip_prefix("Task: ").expect("summary");
        assert_eq!(summary.chars().count(), 160);
        assert!(summary.starts_with(r#"{"prompt":"#));
        assert!(summary.ends_with('…'));
    }
}
