//! Root-session active-time ingress from normalized lifecycle hooks.

use tracing::warn;

use super::{AgentDefinition, HookOutput, LifecycleSignal, RecordedLifecycle, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTimeOp {
    Progress,
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
    if recorded.is_some_and(|recorded| recorded.observation.parent_agent_id.is_some()) {
        return;
    }
    let signal = recorded.map(|recorded| &recorded.observation.signal);
    let Some(op) = active_time_op(signal, decoded.records_progress(), decoded.ends_session())
    else {
        return;
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
    records_progress: bool,
    ends_session: bool,
) -> Option<ActiveTimeOp> {
    match signal {
        Some(
            LifecycleSignal::TurnStarted
            | LifecycleSignal::ToolUsed { .. }
            | LifecycleSignal::Compacting
            | LifecycleSignal::CompactionEnded { .. },
        ) => Some(ActiveTimeOp::Progress),
        Some(
            LifecycleSignal::AwaitingInput { .. }
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::TurnInterrupted
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
        ActiveTimeOp::Progress => now,
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
            (LifecycleSignal::TurnInterrupted, Some(ActiveTimeOp::Stop)),
            (LifecycleSignal::SubagentStarted, None),
            (LifecycleSignal::SubagentStopped { errored: false }, None),
            (
                LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: None,
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
                LifecycleSignal::CompactionEnded { auto: None },
                Some(ActiveTimeOp::Progress),
            ),
            (LifecycleSignal::Ended, Some(ActiveTimeOp::Stop)),
            (LifecycleSignal::Lost, Some(ActiveTimeOp::Stop)),
        ];
        for (signal, expected) in cases {
            assert_eq!(
                active_time_op(Some(&signal), true, true),
                expected,
                "{}",
                signal.tag()
            );
        }
        assert_eq!(
            active_time_op(None, true, false),
            Some(ActiveTimeOp::Progress)
        );
        assert_eq!(active_time_op(None, false, true), Some(ActiveTimeOp::Stop));
        assert_eq!(active_time_op(None, false, false), None);
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
