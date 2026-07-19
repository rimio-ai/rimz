use serde_json::Value;

use crate::transcript::AskQuestion;

pub(super) fn question_detail(tool_name: &str, input: &Value) -> Option<Vec<AskQuestion>> {
    match tool_name {
        "exit_plan_mode" => {
            super::super::question::plan_question(input.get("plan")?.as_str()?, Vec::new())
        }
        "ask_user_question" => {
            super::super::question::questions(input, super::super::question::PreviewPolicy::None)
        }
        _ => None,
    }
}
