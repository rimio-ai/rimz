//! Pure cohort edges derived from an appended member lifecycle event.

use serde_json::{Map, Value, json};

use super::{Signal, SignalSource};
use crate::agents::{
    AgentState, AgentStatus, LifecycleEvent, LifecycleSignal, LifecycleTransition,
};
use crate::harness::target::{agent_channel, agent_handle};
use crate::store::message::MessageRecord;

/// Derive team signals from the transitioning audit row and its live cohort members.
/// `pending` is the complete pending queue, including delayed and resume-gated messages.
pub fn team_lifecycle_signals(
    event: &LifecycleEvent,
    member: &AgentState,
    live_cohort: &[&AgentState],
    pending: &[MessageRecord],
) -> Vec<Signal> {
    let Some(team) = member.team.as_deref().filter(|team| !team.is_empty()) else {
        return Vec::new();
    };
    if member.is_provider_subagent() {
        return Vec::new();
    }
    let root_terminal = matches!(event.signal, LifecycleSignal::Ended | LifecycleSignal::Lost);
    if !root_terminal && matches!(event.transition, LifecycleTransition::Ignored { .. }) {
        return Vec::new();
    }
    let terminal = root_terminal || matches!(event.signal, LifecycleSignal::SubagentStopped { .. });
    let is_member =
        |agent: &&AgentState| agent.kind == member.kind && agent.agent_id == member.agent_id;
    if !terminal && !live_cohort.iter().any(is_member) {
        return Vec::new();
    }
    let others: Vec<_> = live_cohort
        .iter()
        .copied()
        .filter(|agent| !is_member(agent) && agent.ended_at.is_none())
        .collect();
    let mut live = others.clone();
    if !terminal {
        live.push(member);
    }
    let at_rest = |status| matches!(status, AgentStatus::Idle | AgentStatus::Success);
    let has_pending = |members: &[&AgentState]| {
        pending.iter().any(|message| {
            members
                .iter()
                .any(|agent| message.same_card(agent.card_ref()))
        })
    };
    let others_at_rest = others.iter().all(|agent| at_rest(agent.status));
    let mut names = Vec::new();
    if !terminal
        && event.status == AgentStatus::Waiting
        && event.prior_status != Some(AgentStatus::Waiting)
    {
        names.push("team.waiting");
    }
    if matches!(
        event.signal,
        LifecycleSignal::TurnEnded { errored: true, .. }
    ) {
        names.push("team.failed");
    }
    let post_idle = !live.is_empty()
        && others_at_rest
        && (terminal || at_rest(event.status))
        && !has_pending(&live);
    let mut members = others;
    members.push(member);
    let prior_idle =
        others_at_rest && event.prior_status.is_some_and(at_rest) && !has_pending(&members);
    if post_idle && !prior_idle {
        names.push("team.idle");
    }
    if terminal && live.is_empty() {
        names.push("team.ended");
    }
    if names.is_empty() {
        return Vec::new();
    }
    let channel = agent_channel(member).unwrap_or_else(|| "external".to_owned());
    let payload = Map::from_iter([
        ("team".to_owned(), json!(team)),
        ("instance".to_owned(), json!(format!("{team}#{channel}"))),
        (
            "member".to_owned(),
            json!(agent_handle(member, &members, true)),
        ),
        (
            "members".to_owned(),
            Value::Array(
                members
                    .iter()
                    .map(|agent| {
                        let status = if is_member(agent) {
                            event.status
                        } else {
                            agent.status
                        };
                        json!({"handle": agent_handle(agent, &members, true), "status": status})
                    })
                    .collect(),
            ),
        ),
    ]);
    names
        .into_iter()
        .map(|name| Signal {
            // Only the four valid static signal names above reach this parser.
            name: name.parse().expect("static team signal name is valid"),
            payload: payload.clone(),
            source: SignalSource::Lifecycle,
            watch: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AskKind, TurnPhase};
    use crate::harness::target::team_cohorts;
    use crate::ids::{AgentSessionId, EventId, WorkspaceId};
    use crate::store::message::DeliveryGate;

    fn member(name: &str, channel: &str, status: AgentStatus) -> AgentState {
        let mut member = crate::sidebar::test_support::root_agent("codex", name, None);
        member.name = Some(name.to_owned());
        member.role = Some(name.to_owned());
        member.team = Some("forge".to_owned());
        member.channel = Some(channel.to_owned());
        member.status = status;
        member
    }

    fn event(member: &AgentState, prior: AgentStatus, signal: LifecycleSignal) -> LifecycleEvent {
        let mut state = member.lifecycle();
        state.status = prior;
        state.phase = TurnPhase::Idle;
        let transition = crate::agents::step(Some(&state), None, None, &signal);
        LifecycleEvent::new(
            EventId::new(),
            jiff::Timestamp::UNIX_EPOCH,
            WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            member.kind.clone(),
            member.agent_id.clone(),
            member.name.clone(),
            member.parent_agent_id.clone(),
            signal,
            Some(prior),
            transition,
        )
    }

    fn derive(
        event: &LifecycleEvent,
        rows: &[AgentState],
        pending: &[MessageRecord],
    ) -> Vec<Signal> {
        let member = rows
            .iter()
            .find(|row| row.kind == event.kind && row.agent_id == event.agent_id)
            .unwrap();
        let channel = agent_channel(member).unwrap_or_else(|| "external".to_owned());
        let cohorts = team_cohorts(rows);
        let live = cohorts
            .iter()
            .find(|cohort| Some(cohort.team) == member.team.as_deref() && cohort.channel == channel)
            .map_or(&[][..], |cohort| cohort.members.as_slice());
        team_lifecycle_signals(event, member, live, pending)
    }

    fn turn_end() -> LifecycleSignal {
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    }

    #[test]
    fn team_idle_waits_for_all_members_and_empty_queues() {
        let coder = member("coder", "auth", AgentStatus::Success);
        let mut reviewer = member("reviewer", "auth", AgentStatus::Running);
        reviewer.parent_agent_id = Some(coder.agent_id.clone());
        reviewer.parent_agent_kind = Some(coder.kind.clone());
        reviewer.launch_depth = Some(1);
        let event = event(&coder, AgentStatus::Running, turn_end());
        let mut rows = [coder, reviewer];
        assert!(derive(&event, &rows, &[]).is_empty());
        rows[1].status = AgentStatus::Idle;
        let signals = derive(&event, &rows, &[]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].name.as_str(), "team.idle");
        assert_eq!(signals[0].source, SignalSource::Lifecycle);
        assert_eq!(
            signals[0].payload["members"],
            json!([
                {"handle": "@reviewer#auth", "status": "idle"},
                {"handle": "@coder#auth", "status": "success"}
            ])
        );
        for gate in [DeliveryGate::Done, DeliveryGate::Any, DeliveryGate::Resume] {
            for row in &rows {
                let mut queued = MessageRecord::new(
                    event.workspace_id.clone(),
                    row,
                    "work".to_owned(),
                    true,
                    gate,
                );
                queued.agent_id = AgentSessionId::from("launch_provisional");
                queued.not_before = Some("2099-01-01T00:00:00Z".parse().unwrap());
                assert!(derive(&event, &rows, &[queued]).is_empty());
            }
        }
        let mut repeated = event.clone();
        repeated.prior_status = Some(AgentStatus::Success);
        assert!(derive(&repeated, &rows, &[]).is_empty());
        repeated.prior_status = Some(AgentStatus::Idle);
        assert!(derive(&repeated, &rows, &[]).is_empty());
    }

    #[test]
    fn team_signals_are_scoped_to_instance() {
        let coder = member("coder", "auth", AgentStatus::Success);
        let docs = member("writer", "docs", AgentStatus::Running);
        let mut other_team = member("planner", "auth", AgentStatus::Running);
        other_team.team = Some("review".to_owned());
        let event = event(&coder, AgentStatus::Running, turn_end());
        let queued = MessageRecord::new(
            event.workspace_id.clone(),
            &docs,
            "work".to_owned(),
            true,
            DeliveryGate::Resume,
        );
        let signals = derive(&event, &[coder, docs, other_team], &[queued]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].payload["team"], "forge");
        assert_eq!(signals[0].payload["instance"], "forge#auth");
        assert_eq!(signals[0].payload["member"], "@coder#auth");
        assert_eq!(
            signals[0].payload["members"],
            json!([
                {"handle": "@coder#auth", "status": "success"}
            ])
        );
    }

    #[test]
    fn team_waiting_fires_once_per_entry() {
        let member = member("coder", "auth", AgentStatus::Waiting);
        let waiting = LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: None,
        };
        let entered = event(&member, AgentStatus::Running, waiting.clone());
        let repeated = event(&member, AgentStatus::Waiting, waiting.clone());
        let reentered = event(&member, AgentStatus::Running, waiting);
        let rows = [member];
        assert_eq!(
            derive(&entered, &rows, &[])[0].name.as_str(),
            "team.waiting"
        );
        assert!(derive(&repeated, &rows, &[]).is_empty());
        assert_eq!(
            derive(&reentered, &rows, &[])[0].name.as_str(),
            "team.waiting"
        );
    }

    #[test]
    fn team_failed_requires_an_errored_turn_end() {
        let member = member("coder", "auth", AgentStatus::Failed);
        let failed = event(
            &member,
            AgentStatus::Running,
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        );
        let rows = [member];
        let signals = derive(&failed, &rows, &[]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].name.as_str(), "team.failed");
        assert_eq!(signals[0].payload["members"][0]["status"], "failed");
        let ignored = event(&rows[0], AgentStatus::Failed, LifecycleSignal::Compacting);
        assert!(derive(&ignored, &rows, &[]).is_empty());
    }

    #[test]
    fn team_ended_uses_the_final_audit_member_for_ignored_terminal_events() {
        for terminal in [LifecycleSignal::Ended, LifecycleSignal::Lost] {
            let mut coder = member("coder", "auth", AgentStatus::Running);
            let mut reviewer = member("reviewer", "auth", AgentStatus::Success);
            reviewer.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
            let event = event(&coder, AgentStatus::Running, terminal.clone());
            assert!(matches!(
                event.transition,
                LifecycleTransition::Ignored { .. }
            ));
            if matches!(terminal, LifecycleSignal::Ended) {
                coder.ended_at = Some(event.at);
            }
            let rows = [coder, reviewer];
            let cohorts = team_cohorts(&rows);
            if matches!(terminal, LifecycleSignal::Ended) {
                assert!(cohorts.is_empty());
            } else {
                assert!(rows[0].ended_at.is_none());
                assert_eq!(cohorts[0].members.len(), 1);
            }
            let signals = derive(&event, &rows, &[]);
            assert_eq!(signals.len(), 1);
            assert_eq!(signals[0].name.as_str(), "team.ended");
            assert_eq!(signals[0].payload["instance"], "forge#auth");
            assert_eq!(signals[0].payload["member"], "@coder#auth");
            assert_eq!(
                signals[0].payload["members"],
                json!([
                    {"handle": "@coder#auth", "status": "running"}
                ])
            );
        }
    }

    #[test]
    fn terminal_member_leaving_only_resting_members_enters_idle_not_ended() {
        for terminal in [LifecycleSignal::Ended, LifecycleSignal::Lost] {
            let mut coder = member("coder", "auth", AgentStatus::Running);
            let reviewer = member("reviewer", "auth", AgentStatus::Success);
            let event = event(&coder, AgentStatus::Running, terminal.clone());
            if matches!(terminal, LifecycleSignal::Ended) {
                coder.ended_at = Some(event.at);
            }
            let mut rows = [coder, reviewer];
            let signals = derive(&event, &rows, &[]);
            assert_eq!(signals.len(), 1);
            assert_eq!(signals[0].name.as_str(), "team.idle");
            let mut already_idle = event.clone();
            already_idle.prior_status = Some(AgentStatus::Idle);
            assert!(derive(&already_idle, &rows, &[]).is_empty());
            rows[1].status = AgentStatus::Running;
            assert!(derive(&event, &rows, &[]).is_empty());
        }
    }

    #[test]
    fn provider_children_and_non_team_members_do_not_emit_team_signals() {
        let mut member = member("child", "auth", AgentStatus::Success);
        member.parent_agent_id = Some(AgentSessionId::from("parent"));
        let ended = event(&member, AgentStatus::Running, LifecycleSignal::Ended);
        assert!(derive(&ended, &[member.clone()], &[]).is_empty());
        member.parent_agent_id = None;
        member.team = None;
        assert!(derive(&ended, &[member], &[]).is_empty());
    }

    #[test]
    fn launched_child_stop_is_terminal_but_ignored_nonterminal_events_do_not_emit() {
        let mut child = member("child", "auth", AgentStatus::Success);
        child.parent_agent_id = Some(AgentSessionId::from("parent"));
        child.parent_agent_kind = Some(child.kind.clone());
        child.launch_depth = Some(1);
        let stopped = event(
            &child,
            AgentStatus::Running,
            LifecycleSignal::SubagentStopped { errored: false },
        );
        let rows = [child];
        assert!(rows[0].ended_at.is_none());
        let signals = derive(&stopped, &rows, &[]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].name.as_str(), "team.ended");
        let mut ignored = event(&rows[0], AgentStatus::Running, turn_end());
        ignored.transition = LifecycleTransition::Ignored {
            reason: "duplicate".to_owned(),
        };
        assert!(derive(&ignored, &rows, &[]).is_empty());
    }
}
