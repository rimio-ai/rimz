use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskUserQuestionInput {
    questions: Vec<AskQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskQuestion {
    question: Option<String>,
    options: Vec<AskOption>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskOption {
    label: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExitPlanModeInput {
    plan: Option<String>,
}

pub(super) fn question_summary(tool_name: &str, tool_input: &Value) -> Option<String> {
    match tool_name {
        "AskUserQuestion" => ask_user_question_summary(tool_input),
        "ExitPlanMode" => exit_plan_mode_summary(tool_input),
        _ => None,
    }
}

pub(super) fn answer_summary(tool_name: &str, tool_response: &Value) -> Option<String> {
    match tool_name {
        "AskUserQuestion" => ask_user_question_answer(tool_response),
        "ExitPlanMode" => Some("approved plan".to_owned()),
        _ => None,
    }
}

fn ask_user_question_summary(tool_input: &Value) -> Option<String> {
    let parsed: AskUserQuestionInput = serde_json::from_value(tool_input.clone()).ok()?;
    let text = parsed
        .questions
        .into_iter()
        .filter_map(render_question)
        .collect::<Vec<_>>()
        .join("\n");
    non_empty(Some(&text))
}

fn render_question(question: AskQuestion) -> Option<String> {
    let mut text = non_empty(question.question.as_deref())?;
    let labels = question
        .options
        .into_iter()
        .filter_map(|option| non_empty(option.label.as_deref()))
        .collect::<Vec<_>>();
    if !labels.is_empty() {
        text.push_str(" [");
        text.push_str(&labels.join(", "));
        text.push(']');
    }
    Some(text)
}

fn exit_plan_mode_summary(tool_input: &Value) -> Option<String> {
    let parsed: ExitPlanModeInput = serde_json::from_value(tool_input.clone()).ok()?;
    let plan = non_empty(parsed.plan.as_deref())?;
    Some(format!("Requesting plan approval:\n\n{plan}"))
}

fn ask_user_question_answer(tool_response: &Value) -> Option<String> {
    // Claude Code has not documented this PostToolUse shape yet
    // (anthropics/claude-code#12605); refine these fields as the wire settles.
    value_text(tool_response)
        .or_else(|| object_answer_field(tool_response, "answers"))
        .or_else(|| object_answer_field(tool_response, "choices"))
        .or_else(|| object_answer_field(tool_response, "selectedOptions"))
        .or_else(|| serde_json::to_string(tool_response).ok())
}

fn object_answer_field(value: &Value, key: &str) -> Option<String> {
    let field = value.get(key)?;
    answer_value_text(field).or_else(|| {
        let values = field
            .as_array()?
            .iter()
            .filter_map(answer_value_text)
            .collect::<Vec<_>>();
        (!values.is_empty()).then(|| values.join(", "))
    })
}

fn answer_value_text(value: &Value) -> Option<String> {
    value_text(value).or_else(|| {
        value.as_object()?.iter().find_map(|(key, value)| {
            matches!(
                key.as_str(),
                "answer" | "choice" | "label" | "text" | "value" | "name"
            )
            .then(|| value_text(value))
            .flatten()
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_user_question_summary_renders_questions_and_options() {
        let summary = question_summary(
            "AskUserQuestion",
            &json!({
                "questions": [
                    {
                        "question": "Choose deployment path?",
                        "options": [
                            { "label": "safe" },
                            { "label": "fast" }
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
            summary.as_deref(),
            Some("Choose deployment path? [safe, fast]\nNotify team?")
        );
    }

    #[test]
    fn exit_plan_mode_summary_renders_plan_approval_request() {
        let summary = question_summary(
            "ExitPlanMode",
            &json!({ "plan": "1. Edit parser\n2. Run tests" }),
        );

        assert_eq!(
            summary.as_deref(),
            Some("Requesting plan approval:\n\n1. Edit parser\n2. Run tests")
        );
    }

    #[test]
    fn ask_user_question_answer_prefers_readable_fields() {
        assert_eq!(
            answer_summary("AskUserQuestion", &json!("safe")).as_deref(),
            Some("safe")
        );
        assert_eq!(
            answer_summary(
                "AskUserQuestion",
                &json!({ "selectedOptions": [{ "label": "fast" }, { "label": "notify" }] })
            )
            .as_deref(),
            Some("fast, notify")
        );
        assert_eq!(
            answer_summary("AskUserQuestion", &json!({ "unexpected": ["shape"] })).as_deref(),
            Some(r#"{"unexpected":["shape"]}"#)
        );
    }

    #[test]
    fn answer_summary_handles_plan_approval() {
        assert_eq!(
            answer_summary("ExitPlanMode", &json!({})).as_deref(),
            Some("approved plan")
        );
        assert!(answer_summary("Bash", &json!("ok")).is_none());
    }
}
