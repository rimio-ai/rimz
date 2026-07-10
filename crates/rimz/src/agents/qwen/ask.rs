use serde::Deserialize;
use serde_json::Value;

use crate::transcript::{AskOption, AskQuestion};

#[derive(Default, Deserialize)]
#[serde(default)]
struct AskInput {
    questions: Vec<Question>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Question {
    question: Option<String>,
    options: Vec<OptionItem>,
    #[serde(rename = "multiSelect")]
    multi_select: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OptionItem {
    label: Option<String>,
    description: Option<String>,
}

pub(super) fn question_detail(tool_name: &str, input: &Value) -> Option<Vec<AskQuestion>> {
    if tool_name == "exit_plan_mode" {
        let plan = input.get("plan")?.as_str()?.trim();
        return (!plan.is_empty()).then(|| {
            vec![AskQuestion {
                question: format!("Requesting plan approval:\n\n{plan}"),
                options: Vec::new(),
                multi_select: false,
                has_option_previews: false,
            }]
        });
    }
    if tool_name != "ask_user_question" {
        return None;
    }
    let parsed: AskInput = serde_json::from_value(input.clone()).ok()?;
    let questions = parsed
        .questions
        .into_iter()
        .filter_map(|question| {
            let text = question.question?.trim().to_owned();
            if text.is_empty() {
                return None;
            }
            let options = question
                .options
                .into_iter()
                .filter_map(|option| {
                    let label = option.label?.trim().to_owned();
                    (!label.is_empty()).then_some(AskOption {
                        label,
                        description: option.description.filter(|value| !value.trim().is_empty()),
                        caution: None,
                    })
                })
                .collect();
            Some(AskQuestion {
                question: text,
                options,
                multi_select: question.multi_select,
                has_option_previews: false,
            })
        })
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

pub(super) fn permission_detail(payload: &Value) -> Option<String> {
    let tool = payload.get("tool_name")?.as_str()?.trim();
    if tool.is_empty() {
        return None;
    }
    let input = payload
        .get("tool_input")
        .and_then(|value| serde_json::to_string(value).ok())
        .filter(|value| value != "{}" && value != "null")
        .map(|value| value.chars().take(160).collect::<String>());
    Some(input.map_or_else(|| tool.to_owned(), |input| format!("{tool}: {input}")))
}
