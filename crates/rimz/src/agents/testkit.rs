//! Shared fixtures for the adapter unit tests: the exhaustive lifecycle-signal
//! enumeration the totality tests sweep, and the hook-decode accessors every
//! adapter suite drives its native payloads through.

use serde_json::Value;

use crate::agents::capabilities::HookCapability;
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentLifecycleObservation, AskKind, HookOutput};

/// Decode one native hook payload, asserting the adapter accepted it.
pub(crate) fn hook_output(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> HookOutput {
    let kind = adapter.spec().kind;
    adapter
        .decode_hook(event, payload)
        .unwrap_or_else(|err| panic!("{kind} decodes {event}: {err}"))
}

/// The lifecycle observation a native hook payload produces, if any.
pub(crate) fn hook_observation(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> Option<AgentLifecycleObservation> {
    hook_output(adapter, event, payload).lifecycle().cloned()
}

/// The lifecycle observation a native hook payload must produce.
pub(crate) fn hook_lifecycle(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> AgentLifecycleObservation {
    let kind = adapter.spec().kind;
    hook_observation(adapter, event, payload)
        .unwrap_or_else(|| panic!("{kind} {event} observes lifecycle"))
}

/// The lifecycle signal a native hook payload must produce.
pub(crate) fn hook_signal(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> LifecycleSignal {
    hook_lifecycle(adapter, event, payload).signal
}

/// Every [`LifecycleSignal`] value, with each payload-carrying variant swept
/// over its full flag space — the enumeration the state machine's totality
/// tests fold through `step`.
pub(crate) fn all_signals() -> Vec<LifecycleSignal> {
    let mut signals = vec![
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted,
        LifecycleSignal::TurnInterrupted,
        LifecycleSignal::SubagentStarted,
        LifecycleSignal::Compacting,
        LifecycleSignal::Ended,
        LifecycleSignal::Lost,
    ];
    for kind in [
        AskKind::Permission,
        AskKind::PlanApproval,
        AskKind::Question,
    ] {
        signals.push(LifecycleSignal::AwaitingInput {
            kind,
            ask_id: None,
            detail: None,
            native_key: None,
        });
    }
    for auto in [None, Some(false), Some(true)] {
        signals.push(LifecycleSignal::CompactionEnded {
            auto,
            failed: false,
        });
    }
    for errored in [false, true] {
        signals.push(LifecycleSignal::SubagentStopped { errored });
    }
    for errored in [false, true] {
        for parked_on_background in [false, true] {
            signals.push(LifecycleSignal::TurnEnded {
                errored,
                parked_on_background,
            });
        }
    }
    for mutates in [false, true] {
        for edits in [false, true] {
            signals.push(LifecycleSignal::ToolUsed {
                mutates,
                edits,
                name: None,
                native_key: None,
            });
        }
    }
    signals
}
