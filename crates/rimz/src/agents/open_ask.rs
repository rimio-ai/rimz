//! Provider-neutral materialization of an agent's current actionable ask.
//!
//! [`AgentState::actionable_asks`] supplies current identities and summaries. Structured
//! questions and the agent's ask-time message join from RimZ transcript state
//! only by exact ask ID; adapter-owned safe options supply the fallback shape.

use crate::agents::{AgentErr, AgentState, AskDelivery, AskKind, OpenAsk, definition_by_kind};
use crate::disk::paths::StatePaths;
use crate::transcript::{AskQuestion, TranscriptKind, TranscriptLogErr, latest_open_ask, read_all};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAskDetail {
    pub open: OpenAsk,
    pub delivery: AskDelivery,
    pub context: Option<String>,
    pub questions: Vec<AskQuestion>,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenAskReadErr {
    #[error(transparent)]
    Adapter(#[from] AgentErr),
    #[error(transparent)]
    Transcript(#[from] TranscriptLogErr),
}

pub fn read_open_ask(
    paths: &StatePaths,
    agent: &AgentState,
    ask_id: Option<&crate::ids::AskId>,
) -> Result<Option<OpenAskDetail>, OpenAskReadErr> {
    let Some((open, delivery)) = agent
        .actionable_asks()
        .find(|(ask, _)| ask_id.is_none_or(|id| &ask.id == id))
    else {
        return Ok(None);
    };
    let adapter = definition_by_kind(agent.kind.as_str())?;
    let (questions, context) = match open.kind {
        AskKind::Question | AskKind::PlanApproval => {
            let entry = match delivery {
                AskDelivery::Blocking => latest_open_ask(paths, &agent.kind, &agent.agent_id)?
                    .filter(|entry| entry.id.as_ref() == Some(&open.id)),
                AskDelivery::Async => read_all(paths)?.into_iter().rev().find(|entry| {
                    entry.entry == TranscriptKind::Ask
                        && entry.kind == agent.kind
                        && entry.agent_id == agent.agent_id
                        && entry.id.as_ref() == Some(&open.id)
                }),
            };
            entry
                .map(|entry| {
                    let context = entry.text.trim();
                    (
                        entry.questions,
                        (!context.is_empty()).then(|| context.to_owned()),
                    )
                })
                .unwrap_or_else(|| (synthetic_questions(&open, adapter), None))
        }
        AskKind::Permission => (synthetic_questions(&open, adapter), None),
    };
    Ok(Some(OpenAskDetail {
        open,
        delivery,
        context,
        questions,
    }))
}

fn synthetic_questions(
    open: &OpenAsk,
    adapter: &crate::agents::AgentDefinition,
) -> Vec<AskQuestion> {
    vec![AskQuestion {
        question: open
            .detail
            .as_deref()
            .unwrap_or_else(|| open.kind.short_label())
            .to_owned(),
        options: adapter.ask_options(open.kind).unwrap_or_default(),
        multi_select: false,
        has_option_previews: false,
    }]
}
