//! Provider-neutral materialization of an agent's current actionable ask.
//!
//! [`AgentState::open_ask`] owns current identity
//! and summary. Structured questions join from RimZ transcript state only by
//! exact ask ID; adapter-owned safe options supply the fallback shape.

use crate::agents::{AgentErr, AgentState, AskKind, OpenAsk, definition_by_kind};
use crate::store::StatePaths;
use crate::transcript::{AskQuestion, TranscriptLogErr, latest_open_ask};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAskDetail {
    pub open: OpenAsk,
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
) -> Result<Option<OpenAskDetail>, OpenAskReadErr> {
    let Some(open) = agent
        .open_ask
        .as_ref()
        .filter(|_| agent.is_awaiting_input())
    else {
        return Ok(None);
    };
    let adapter = definition_by_kind(agent.kind.as_str())?;
    let questions = match open.kind {
        AskKind::Question | AskKind::PlanApproval => {
            latest_open_ask(paths, &agent.kind, &agent.agent_id)?
                .filter(|entry| entry.id.as_ref() == Some(&open.id))
                .map(|entry| entry.questions)
                .unwrap_or_else(|| synthetic_questions(open, adapter))
        }
        AskKind::Permission => synthetic_questions(open, adapter),
    };
    Ok(Some(OpenAskDetail {
        open: open.clone(),
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
