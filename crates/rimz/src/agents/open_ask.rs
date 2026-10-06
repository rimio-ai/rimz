//! Provider-neutral materialization of an agent's current actionable ask.
//!
//! [`AgentState::actionable_asks`] supplies current identities and summaries. Structured
//! questions and the agent's ask-time message join from RimZ transcript state
//! only by exact ask ID; adapter-owned safe options supply the fallback shape.

use crate::agents::{
    AgentErr, AgentState, AnswerPlanErr, AskDelivery, AskKind, OpenAsk, definition_by_kind,
};
use crate::disk::paths::StatePaths;
use crate::transcript::{AskQuestion, TranscriptKind, TranscriptLogErr, latest_open_ask, read_all};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskRoute {
    Shell,
    Pane,
    AsyncPane,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAskDetail {
    pub open: OpenAsk,
    pub delivery: AskDelivery,
    pub context: Option<String>,
    pub questions: Vec<AskQuestion>,
    pub route: AskRoute,
    pub pane_actions: Option<&'static str>,
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
    let route = if delivery == AskDelivery::Async {
        AskRoute::AsyncPane
    } else if matches!(
        adapter.answer_plan(open.kind, &questions, &[]),
        Err(AnswerPlanErr::Unsupported(_))
    ) || (matches!(open.kind, AskKind::Permission | AskKind::PlanApproval)
        && questions
            .first()
            .is_none_or(|question| question.options.is_empty()))
    {
        AskRoute::Pane
    } else {
        AskRoute::Shell
    };
    let pane_actions = adapter.pane_actions(open.kind);
    Ok(Some(OpenAskDetail {
        open,
        delivery,
        context,
        questions,
        route,
        pane_actions,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(provider: &str, kind: AskKind, asynchronous: bool) -> OpenAskDetail {
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::ids::WorkspaceId::from_project_root(root.path());
        let paths = StatePaths::under(workspace, root.path()).unwrap();
        let mut agent = AgentState::stub(provider, "session", crate::agents::AgentStatus::Waiting);
        agent.waiting_since = Some(agent.last_activity);
        agent.open_ask = Some(OpenAsk {
            id: crate::ids::AskId::parse("ask_0123456789abcdef").unwrap(),
            kind,
            detail: None,
            native_key: None,
            since: agent.last_activity,
        });
        if asynchronous {
            let open = agent.open_ask.take().unwrap();
            agent.queued_asks.push(crate::agents::QueuedAsk {
                id: open.id,
                detail: "Question?".to_owned(),
                native_key: "question".to_owned(),
                since: open.since,
            });
        }
        read_open_ask(&paths, &agent, None).unwrap().unwrap()
    }

    #[test]
    fn codex_permission_routes_to_pane() {
        assert_eq!(
            detail("codex", AskKind::Permission, false).route,
            AskRoute::Pane
        );
    }

    #[test]
    fn registered_adapter_without_planner_routes_to_pane() {
        assert_eq!(
            detail("qwen", AskKind::Question, false).route,
            AskRoute::Pane
        );
    }

    #[test]
    fn async_question_routes_to_async_pane() {
        assert_eq!(
            detail("codex", AskKind::Question, true).route,
            AskRoute::AsyncPane
        );
    }

    #[test]
    fn claude_permission_is_shell_answerable_with_pane_actions() {
        let ask = detail("claude", AskKind::Permission, false);
        assert_eq!(ask.route, AskRoute::Shell);
        assert_eq!(ask.pane_actions, Some("deny and persistent grants"));
    }

    #[test]
    fn codex_plan_is_shell_answerable_with_pane_actions() {
        let ask = detail("codex", AskKind::PlanApproval, false);
        assert_eq!(ask.route, AskRoute::Shell);
        assert_eq!(
            ask.pane_actions,
            Some("keep-planning, clear-context implementation, and refinement")
        );
    }
}
