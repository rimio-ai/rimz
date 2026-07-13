use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

const REQUEST_USER_INPUT_TOOL: &str = "request_user_input";
const SUBMITTED_PROMPT_MAX_CHARS: usize = 1_000;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexQuestionInput {
    questions: Vec<CodexQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexQuestion {
    id: Option<String>,
    header: Option<String>,
    question: Option<String>,
    options: Vec<CodexOption>,
    #[serde(alias = "multiSelect")]
    multi_select: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexOption {
    label: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexQuestionResponse {
    answers: HashMap<String, CodexAnswerEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexAnswerEntry {
    answers: Vec<String>,
}

pub(super) fn question_detail(tool_name: &str, tool_input: &Value) -> Option<Vec<AskQuestion>> {
    if tool_name != REQUEST_USER_INPUT_TOOL {
        return None;
    }
    let parsed: CodexQuestionInput = serde_json::from_value(tool_input.clone()).ok()?;
    let questions = parsed
        .questions
        .into_iter()
        .filter_map(structured_question)
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

fn structured_question(question: CodexQuestion) -> Option<AskQuestion> {
    let question_text = non_empty(question.question.as_deref())
        .or_else(|| non_empty(question.header.as_deref()))?;
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
    Some(AskQuestion {
        question: question_text,
        options,
        multi_select: question.multi_select,
        has_option_previews: false,
    })
}

pub(super) fn plan_question(plan: &str) -> Option<Vec<AskQuestion>> {
    let plan = non_empty(Some(plan))?;
    Some(vec![AskQuestion {
        question: format!("Requesting plan approval:\n\n{plan}"),
        options: plan_options(),
        multi_select: false,
        has_option_previews: false,
    }])
}

pub(super) fn plan_options() -> Vec<AskOption> {
    vec![AskOption {
        label: "implement".to_owned(),
        description: Some(
            "Pick 'Yes, implement this plan' in Codex — switches to Default mode and submits the implementation prompt"
                .to_owned(),
        ),
        caution: Some("switches from Plan mode to Default mode".to_owned()),
    }]
}

pub(super) fn answer_detail(
    tool_name: &str,
    tool_input: &Value,
    tool_response: &Value,
) -> Option<Vec<AskAnswer>> {
    if tool_name != REQUEST_USER_INPUT_TOOL {
        return None;
    }
    let input: CodexQuestionInput = serde_json::from_value(tool_input.clone()).ok()?;
    let mut response: CodexQuestionResponse = serde_json::from_value(tool_response.clone()).ok()?;
    let mut answers = Vec::new();
    for question in input.questions {
        let Some(id) = non_empty(question.id.as_deref()) else {
            continue;
        };
        let Some(entry) = response.answers.remove(&id) else {
            continue;
        };
        let chosen = entry
            .answers
            .into_iter()
            .filter_map(|answer| non_empty(Some(&answer)))
            .collect::<Vec<_>>();
        if chosen.is_empty() {
            continue;
        }
        answers.push(AskAnswer {
            question: non_empty(question.question.as_deref())
                .or_else(|| non_empty(question.header.as_deref())),
            chosen,
            note: None,
        });
    }
    (!answers.is_empty()).then_some(answers)
}

pub(super) fn submitted_prompt_answer(prompt: &str) -> Option<Vec<AskAnswer>> {
    let prompt = non_empty(Some(prompt))?;
    let prompt = prompt.chars().take(SUBMITTED_PROMPT_MAX_CHARS).collect();
    Some(vec![AskAnswer {
        question: None,
        chosen: vec![prompt],
        note: None,
    }])
}

pub(super) fn answer_plan(
    kind: AskKind,
    questions: &[AskQuestion],
    answers: &[AskReply],
) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    match kind {
        AskKind::PlanApproval => plan_approval_answer_plan(answers),
        AskKind::Question => question_answer_plan(questions, answers),
        AskKind::Permission => Err(AnswerPlanErr::Invalid(
            "permission answers require the Codex pane".to_owned(),
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
        // Codex 0.144.3, verified 2026-07-13: the selector opens on
        // "Yes, implement this plan" and Enter submits "Implement the plan."
        [0] if answer.text.is_none() => Ok(vec![AnswerStep::Key(NamedKey::Enter)]),
        _ => Err(AnswerPlanErr::Invalid(
            "plan approvals accept only `implement`; keep-planning, clear-context implementation, and refinement require the Codex pane"
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
    for (question, answer) in questions.iter().zip(answers) {
        if question.multi_select {
            return Err(AnswerPlanErr::Invalid(
                "multi-select questions require the Codex pane".to_owned(),
            ));
        }
        if answer.text.is_some() {
            return Err(AnswerPlanErr::Invalid(
                "free-text question answers require the Codex pane".to_owned(),
            ));
        }
        let [pick] = answer.picks.as_slice() else {
            return Err(AnswerPlanErr::Invalid(
                "Codex questions require exactly one option pick".to_owned(),
            ));
        };
        let Some(option) = question.options.get(*pick) else {
            return Err(AnswerPlanErr::Invalid(format!(
                "option {} is out of range for a {}-option Codex question",
                pick + 1,
                question.options.len()
            )));
        };
        if matches!(
            option.label.to_ascii_lowercase().as_str(),
            "other" | "none of the above"
        ) {
            return Err(AnswerPlanErr::Invalid(
                "custom free-text options require the Codex pane".to_owned(),
            ));
        }
        // Codex 0.144.3, verified 2026-07-13: each tab starts on option zero;
        // Down selects by index and Enter commits, advances, and submits on the
        // final question.
        steps.extend(std::iter::repeat_n(AnswerStep::Key(NamedKey::Down), *pick));
        steps.push(AnswerStep::Key(NamedKey::Enter));
    }
    Ok(steps)
}

fn non_empty(text: Option<&str>) -> Option<String> {
    let text = text?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}
