use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply, non_empty_trimmed};
use crate::pane::keys::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

const REQUEST_USER_INPUT_TOOL: &str = "request_user_input";
const SUBMITTED_PROMPT_MAX_CHARS: usize = 1_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AsyncQuestions {
    questions: Vec<AsyncQuestion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AsyncQuestion {
    title: String,
    #[serde(default)]
    options: Vec<String>,
}

#[derive(Deserialize)]
struct AsyncAccepted {
    accepted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionReply {
    question_item_id: String,
    answer: String,
}

pub(super) fn queued_questions(
    tool: &super::payloads::CodexPostToolUse,
) -> Option<crate::agents::AskQueueEdit> {
    if tool.tool_name.as_deref()? != "request_user_input_async" {
        return None;
    }
    let response: AsyncAccepted =
        serde_json::from_str(tool.tool_response.as_ref()?.as_str()?).ok()?;
    if !response.accepted {
        return None;
    }
    let call_id = tool.tool_use_id.as_deref()?;
    let input: AsyncQuestions = serde_json::from_value(tool.tool_input.clone()?).ok()?;
    if input.questions.is_empty() {
        return None;
    }
    Some(crate::agents::AskQueueEdit {
        queued: input
            .questions
            .into_iter()
            .enumerate()
            .map(|(index, question)| crate::agents::QueuedQuestion {
                ask_id: None,
                native_key: serde_json::json!(["request_user_input_async", call_id, index])
                    .to_string(),
                detail: question.title.lines().next().unwrap_or_default().to_owned(),
                question: AskQuestion {
                    question: question.title,
                    options: question
                        .options
                        .into_iter()
                        .map(|label| AskOption {
                            label,
                            description: None,
                            caution: None,
                        })
                        .collect(),
                    multi_select: false,
                    has_option_previews: false,
                },
            })
            .collect(),
        answered: Vec::new(),
    })
}

pub(super) fn answered_questions(prompt: &str) -> Option<crate::agents::AskQueueEdit> {
    let body = prompt
        .trim()
        .strip_prefix("<send_user_message_question_reply>")?
        .strip_suffix("</send_user_message_question_reply>")?;
    let replies: Vec<QuestionReply> = serde_json::from_str(body).unwrap_or_default();
    let answered = replies
        .into_iter()
        .filter_map(|reply| {
            let (tool, _call_id, _index): (String, String, usize) =
                serde_json::from_str(&reply.question_item_id).ok()?;
            (tool == "request_user_input_async").then_some(crate::agents::AnsweredQuestion {
                ask_id: None,
                native_key: reply.question_item_id,
                answer: reply.answer,
            })
        })
        .collect();
    Some(crate::agents::AskQueueEdit {
        queued: Vec::new(),
        answered,
    })
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
    super::super::question::questions_with_header_fallback(
        tool_input,
        super::super::question::PreviewPolicy::None,
    )
}

pub(super) fn plan_question(plan: &str) -> Option<Vec<AskQuestion>> {
    super::super::question::plan_question(plan, plan_options())
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
    let input = super::super::question::decode_with_header_fallback(
        tool_input,
        super::super::question::PreviewPolicy::None,
    )?;
    let mut response: CodexQuestionResponse = serde_json::from_value(tool_response.clone()).ok()?;
    let mut answers = Vec::new();
    for question in input {
        let Some(id) = question.native_id else {
            continue;
        };
        let Some(entry) = response.answers.remove(&id) else {
            continue;
        };
        let chosen = entry
            .answers
            .into_iter()
            .filter_map(|answer| non_empty_trimmed(&answer))
            .collect::<Vec<_>>();
        if chosen.is_empty() {
            continue;
        }
        answers.push(AskAnswer {
            question: Some(question.question.question),
            chosen,
            note: None,
        });
    }
    (!answers.is_empty()).then_some(answers)
}

pub(super) fn submitted_prompt_answer(prompt: &str) -> Option<Vec<AskAnswer>> {
    if answered_questions(prompt).is_some() {
        return None;
    }
    let prompt = non_empty_trimmed(prompt)?;
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

pub(super) const PLAN_PANE_ACTIONS: &str =
    "keep-planning, clear-context implementation, and refinement";

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
        _ => Err(AnswerPlanErr::Invalid(format!(
            "plan approvals accept only `implement`; {PLAN_PANE_ACTIONS} require the Codex pane"
        ))),
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
