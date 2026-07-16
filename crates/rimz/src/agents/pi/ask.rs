use serde::Deserialize;
use serde_json::Value;

use crate::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AskInput {
    questions: Vec<PiAskQuestion>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PiAskQuestion {
    question: Option<String>,
    options: Vec<PiAskOption>,
    #[serde(rename = "multiSelect")]
    multi_select: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PiAskOption {
    label: Option<String>,
    description: Option<String>,
    preview: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolDetails {
    answers: Vec<PiAskAnswer>,
    cancelled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PiAskAnswer {
    question: Option<String>,
    kind: Option<String>,
    answer: Option<String>,
    selected: Option<Vec<String>>,
    notes: Option<String>,
}

pub(super) fn question_detail(tool_name: &str, tool_input: &Value) -> Option<Vec<AskQuestion>> {
    if tool_name != "ask_user_question" {
        return None;
    }
    let parsed: AskInput = serde_json::from_value(tool_input.clone()).ok()?;
    let questions = parsed
        .questions
        .into_iter()
        .filter_map(structured_question)
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(questions)
}

fn structured_question(question: PiAskQuestion) -> Option<AskQuestion> {
    let question_text = question.question.as_deref().and_then(non_empty)?;
    let has_option_previews = question.options.iter().any(|option| {
        option
            .preview
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|preview| !preview.is_empty())
    });
    let options = question
        .options
        .into_iter()
        .filter_map(|option| {
            Some(AskOption {
                label: option.label.as_deref().and_then(non_empty)?,
                description: option.description.as_deref().and_then(non_empty),
                caution: None,
            })
        })
        .collect();
    Some(AskQuestion {
        question: question_text,
        options,
        multi_select: question.multi_select,
        has_option_previews,
    })
}

pub(super) fn answer_detail(payload: &Value) -> Option<Vec<AskAnswer>> {
    let details: ToolDetails = serde_json::from_value(payload.get("tool_details")?.clone()).ok()?;
    if details.cancelled {
        return None;
    }
    let answers = details
        .answers
        .into_iter()
        .filter_map(|answer| {
            let chosen = match answer.kind.as_deref()? {
                "option" | "custom" => {
                    vec![answer.answer.as_deref().and_then(non_empty)?]
                }
                "chat" => vec!["Chat about this".to_owned()],
                "multi" => answer
                    .selected
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|selected| non_empty(&selected))
                    .collect(),
                _ => return None,
            };
            if chosen.is_empty() {
                return None;
            }
            Some(AskAnswer {
                question: answer.question.as_deref().and_then(non_empty),
                chosen,
                note: answer.notes.as_deref().and_then(non_empty),
            })
        })
        .collect::<Vec<_>>();
    (!answers.is_empty()).then_some(answers)
}

pub(super) fn answer_plan(
    kind: AskKind,
    questions: &[AskQuestion],
    answers: &[AskReply],
) -> Result<Vec<AnswerStep>, AnswerPlanErr> {
    if kind != AskKind::Question {
        return Err(AnswerPlanErr::Invalid(
            "pi supports answers only for questionnaire asks".to_owned(),
        ));
    }
    if questions.len() != answers.len() {
        return Err(AnswerPlanErr::Invalid(format!(
            "expected {} answers, got {}",
            questions.len(),
            answers.len()
        )));
    }

    let mut steps = Vec::new();
    for (question, answer) in questions.iter().zip(answers) {
        if answer.text.is_some() && !answer.picks.is_empty() {
            return Err(AnswerPlanErr::Invalid(
                "pi questionnaire answers cannot combine picks and text".to_owned(),
            ));
        }
        if let Some(text) = answer.text.as_ref() {
            if question.multi_select || question.has_option_previews {
                return Err(AnswerPlanErr::Invalid(
                    "pi suppresses the `Type something.` row for preview-carrying and multi-select questions"
                        .to_owned(),
                ));
            }
            steps.extend(std::iter::repeat_n(
                AnswerStep::Key(NamedKey::Down),
                question.options.len(),
            ));
            steps.push(AnswerStep::Paste(text.clone()));
            steps.push(AnswerStep::Key(NamedKey::Enter));
            continue;
        }

        let mut picks = answer.picks.clone();
        picks.sort_unstable();
        if picks.is_empty()
            || picks
                .last()
                .is_some_and(|pick| *pick >= question.options.len())
        {
            return Err(AnswerPlanErr::Invalid(
                "pi questionnaire answer contains no usable option pick".to_owned(),
            ));
        }
        if picks.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(AnswerPlanErr::Invalid(
                "pi questionnaire options can be selected only once".to_owned(),
            ));
        }

        if question.multi_select {
            let mut current = 0;
            for pick in picks {
                steps.extend(std::iter::repeat_n(
                    AnswerStep::Key(NamedKey::Down),
                    pick - current,
                ));
                steps.push(AnswerStep::Text(" ".to_owned()));
                current = pick;
            }
            steps.extend(std::iter::repeat_n(
                AnswerStep::Key(NamedKey::Down),
                question.options.len() - current,
            ));
            steps.push(AnswerStep::Key(NamedKey::Enter));
        } else {
            let [pick] = picks.as_slice() else {
                return Err(AnswerPlanErr::Invalid(
                    "pi single-select questions require exactly one option".to_owned(),
                ));
            };
            steps.extend(std::iter::repeat_n(AnswerStep::Key(NamedKey::Down), *pick));
            steps.push(AnswerStep::Key(NamedKey::Enter));
        }
    }
    if questions.len() > 1 {
        steps.push(AnswerStep::Key(NamedKey::Enter));
    }
    Ok(steps)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
