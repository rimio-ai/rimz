//! Root-session active-time ingress from normalized lifecycle hooks.

use tracing::warn;

use super::{AgentDefinition, HookOutput, LifecycleSignal, RecordedLifecycle, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTimeOp {
    Progress,
    Pulse,
    Stop,
}

pub(super) fn record(
    store: &Store,
    agent: &AgentDefinition,
    decoded: &HookOutput,
    recorded: Option<&RecordedLifecycle>,
    agent_id: Option<&str>,
    event_name: &str,
) {
    let signal = recorded.map(|recorded| &recorded.observation.signal);
    let parent_agent_id =
        recorded.and_then(|recorded| recorded.observation.parent_agent_id.as_deref());
    let Some(op) = active_time_op(
        signal,
        parent_agent_id.is_some(),
        decoded.records_progress(),
        decoded.ends_session(),
    ) else {
        return;
    };
    let agent_id = match op {
        ActiveTimeOp::Pulse => parent_agent_id,
        ActiveTimeOp::Progress | ActiveTimeOp::Stop => agent_id,
    };
    let Some(agent_id) = agent_id else { return };
    let now = jiff::Timestamp::now();
    let at = active_time_at(op, now, decoded.turn_error());
    let grace_secs = rimz::config::MachineConfig::load_lenient()
        .agents
        .attention
        .active_grace_secs
        .get();
    let result = match op {
        ActiveTimeOp::Progress => rimz::store::active_time::record_progress(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        ),
        ActiveTimeOp::Pulse => rimz::store::active_time::record_pulse(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        )
        .map(|_| ()),
        ActiveTimeOp::Stop => rimz::store::active_time::record_stop(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        ),
    };
    if let Err(err) = result {
        warn!(
            agent = agent.spec().kind,
            event = %event_name,
            error = %err,
            "lifecycle: failed to update estimated active time",
        );
    }
}

fn active_time_op(
    signal: Option<&LifecycleSignal>,
    is_child: bool,
    records_progress: bool,
    ends_session: bool,
) -> Option<ActiveTimeOp> {
    if is_child {
        return matches!(signal, Some(LifecycleSignal::SubagentStopped { .. }))
            .then_some(ActiveTimeOp::Pulse);
    }
    match signal {
        Some(
            LifecycleSignal::TurnStarted
            | LifecycleSignal::ToolUsed { .. }
            | LifecycleSignal::Compacting
            | LifecycleSignal::CompactionEnded { failed: false, .. },
        ) => Some(ActiveTimeOp::Progress),
        Some(
            LifecycleSignal::AwaitingInput { .. }
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::TurnInterrupted { .. }
            | LifecycleSignal::CompactionEnded { failed: true, .. }
            | LifecycleSignal::Ended
            | LifecycleSignal::Lost,
        ) => Some(ActiveTimeOp::Stop),
        Some(
            LifecycleSignal::Registered
            | LifecycleSignal::SubagentStarted
            | LifecycleSignal::SubagentStopped { .. },
        ) => None,
        None if ends_session => Some(ActiveTimeOp::Stop),
        None if records_progress => Some(ActiveTimeOp::Progress),
        None => None,
    }
}

fn active_time_at(
    op: ActiveTimeOp,
    now: jiff::Timestamp,
    turn_error: Option<&rimz::agents::AgentTurnError>,
) -> jiff::Timestamp {
    match op {
        ActiveTimeOp::Progress | ActiveTimeOp::Pulse => now,
        ActiveTimeOp::Stop => turn_error.map_or(now, |error| error.at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AskKind;

    #[test]
    fn mapping_covers_every_lifecycle_signal_and_bare_progress() {
        let cases = [
            (LifecycleSignal::Registered, None),
            (LifecycleSignal::TurnStarted, Some(ActiveTimeOp::Progress)),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: true,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (
                LifecycleSignal::TurnInterrupted { turn_id: None },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::SubagentStarted, None),
            (LifecycleSignal::SubagentStopped { errored: false }, None),
            (
                LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    name: None,
                    native_key: None,
                    turn_id: None,
                },
                Some(ActiveTimeOp::Progress),
            ),
            (
                LifecycleSignal::AwaitingInput {
                    kind: AskKind::Question,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::Compacting, Some(ActiveTimeOp::Progress)),
            (
                LifecycleSignal::CompactionEnded {
                    auto: None,
                    failed: false,
                },
                Some(ActiveTimeOp::Progress),
            ),
            (
                LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed: true,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::Ended, Some(ActiveTimeOp::Stop)),
            (LifecycleSignal::Lost, Some(ActiveTimeOp::Stop)),
        ];
        for (signal, expected) in cases {
            assert_eq!(
                active_time_op(Some(&signal), false, true, true),
                expected,
                "{}",
                signal.tag()
            );
        }
        assert_eq!(
            active_time_op(None, false, true, false),
            Some(ActiveTimeOp::Progress)
        );
        assert_eq!(
            active_time_op(None, false, false, true),
            Some(ActiveTimeOp::Stop)
        );
        assert_eq!(active_time_op(None, false, false, false), None);
    }

    #[test]
    fn child_mapping_pulses_only_on_subagent_stop() {
        assert_eq!(
            active_time_op(
                Some(&LifecycleSignal::SubagentStopped { errored: false }),
                true,
                false,
                false,
            ),
            Some(ActiveTimeOp::Pulse)
        );
        assert_eq!(
            active_time_op(Some(&LifecycleSignal::SubagentStarted), true, true, true,),
            None
        );
    }

    #[test]
    fn stop_uses_the_provider_error_boundary() {
        let now = jiff::Timestamp::from_second(1_000).unwrap();
        let error_at = jiff::Timestamp::from_second(900).unwrap();
        let error = rimz::agents::AgentTurnError {
            at: error_at,
            ..Default::default()
        };

        assert_eq!(
            active_time_at(ActiveTimeOp::Stop, now, Some(&error)),
            error_at
        );
        assert_eq!(active_time_at(ActiveTimeOp::Stop, now, None), now);
        assert_eq!(
            active_time_at(ActiveTimeOp::Progress, now, Some(&error)),
            now
        );
    }
}
