//! Whether an ended agent's session resumes or it launches fresh, and the login it runs on:
//! `rimz agents restart`, `explain`, and the resume of an ended subagent decide it here alike.

use std::path::Path;

use anyhow::Result;

use crate::agents::AgentState;
use crate::harness::launch::ExecAction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshReason {
    NoResumeSupport,
    NoRecordedConversation,
}

impl FreshReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoResumeSupport => "no resume support",
            Self::NoRecordedConversation => "no recorded conversation",
        }
    }
}

pub fn relaunch_action(
    agent: &AgentState,
    logins: &crate::agents::RoomAccounts,
    catalog: &crate::agents::LoginCatalog,
    cwd: &Path,
) -> Result<(
    crate::harness::launch::ExecAction,
    Option<crate::ids::LoginName>,
    Option<&'static str>,
)> {
    let login = crate::harness::resume::relaunch_login(agent, logins, catalog)?;
    let (action, fresh_reason) = relaunch_plan(agent, cwd)?;
    Ok((action, login, fresh_reason))
}

/// Whether the agent's session resumes, and the reason it launches fresh when it cannot.
pub fn relaunch_plan(
    agent: &AgentState,
    cwd: &Path,
) -> Result<(crate::harness::launch::ExecAction, Option<&'static str>)> {
    let adapter = crate::agents::find_definition(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let resume_support = !agent.agent_id.is_provisional()
        && agent.worktree_path.is_some()
        && crate::harness::launch::compile_provider_argv(
            adapter,
            agent.kind.as_str(),
            &ExecAction::Resume {
                session_id: agent.agent_id.to_string(),
                extra_args: Vec::new(),
            },
            cwd,
        )
        .is_ok();
    let session_present = crate::harness::resume::resume_session_present(agent);
    let fresh_reason = fresh_reason(resume_support, session_present);
    let action = if fresh_reason.is_some() {
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        }
    } else {
        ExecAction::Resume {
            session_id: agent.agent_id.to_string(),
            extra_args: Vec::new(),
        }
    };
    Ok((action, fresh_reason.map(FreshReason::as_str)))
}

fn fresh_reason(resume_support: bool, session_present: bool) -> Option<FreshReason> {
    if !resume_support {
        Some(FreshReason::NoResumeSupport)
    } else if !session_present {
        Some(FreshReason::NoRecordedConversation)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_classification_names_each_fresh_reason() {
        assert_eq!(
            fresh_reason(false, true),
            Some(FreshReason::NoResumeSupport)
        );
        assert_eq!(
            fresh_reason(true, false),
            Some(FreshReason::NoRecordedConversation)
        );
        assert_eq!(fresh_reason(true, true), None);
    }
}
