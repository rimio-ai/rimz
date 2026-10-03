//! Prompt-submit Environment context for a launch stamped with the runtime switch.

use super::*;
use rimz::harness::runtime_env::{PromptSubmit, sample};
use rimz::store::message::{HarnessNotice, MessageSender, PromptSection};

/// Attach the sampled Environment block to a root agent's prompt-submit reply. Enrichment: every miss leaves the reply as it was.
pub(super) fn attach_runtime_env(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &mut HookOutput,
    recorded: &RecordedLifecycle,
    sections: &[PromptSection<'_>],
    ingress_owner: rimz::agents::HookIngressOwner,
) {
    let observation = &recorded.observation;
    if !matches!(observation.signal, LifecycleSignal::TurnStarted { .. })
        || observation.parent_agent_id.is_some()
        || ingress_owner.kind == rimz::pane::RuntimeOwnerKind::Daemon
    {
        return;
    }
    let Some(session) = observation.agent_id.as_ref() else {
        return;
    };
    let switch = agent_identity_env(
        observation.agent_pid,
        rimz::harness::launch::ENV_RUNTIME_ENV,
        validate_non_empty_identity_env,
    );
    if switch.as_deref() != Some("1") {
        return;
    }
    let team = agent_state(store, agent, session)
        .filter(rimz::agents::AgentState::is_team_seat)
        .and_then(|state| state.team);
    let block = sample(&PromptSubmit {
        workspace,
        runtime: store.runtime_paths(),
        kind: agent.spec().kind,
        session: session.as_str(),
        owner: observation.runtime_owner.as_ref(),
        team: team.as_deref(),
        stage_notice: carries_stage_notice(sections),
    });
    if let Some(block) = block {
        agent.attach_prompt_context(decoded, &block);
    }
}

fn carries_stage_notice(sections: &[PromptSection<'_>]) -> bool {
    sections.iter().any(|section| {
        section.record.is_some_and(|record| {
            matches!(
                record.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::Stage
                }
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::store::message::MessageRecord;
    use rimz::transcript::SectionOrigin;

    #[test]
    fn only_a_delivered_stage_notice_counts() {
        let state =
            rimz::agents::AgentState::stub("claude", "sess", rimz::agents::AgentStatus::Idle);
        let record = |sender| {
            MessageRecord::new(
                rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/p")),
                &state,
                "The team moved to Review.".to_owned(),
                rimz::store::message::DeliveryGate::Done,
            )
            .with_sender(sender)
        };
        let stage = record(MessageSender::Harness {
            notice: HarnessNotice::Stage,
        });
        let system = record(MessageSender::System);
        let section = |record| PromptSection {
            text: "text".to_owned(),
            origin: SectionOrigin::Human,
            record,
        };
        assert!(!carries_stage_notice(&[
            section(None),
            section(Some(&system))
        ]));
        assert!(carries_stage_notice(&[
            section(None),
            section(Some(&stage))
        ]));
    }
}
