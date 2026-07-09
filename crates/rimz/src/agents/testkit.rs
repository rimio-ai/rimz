//! Shared fixtures for the adapter unit tests: the exhaustive lifecycle-signal
//! enumeration the totality tests sweep.

use crate::agents::AskKind;
use crate::agents::lifecycle::LifecycleSignal;

/// Every [`LifecycleSignal`] value, with each payload-carrying variant swept
/// over its full flag space — the enumeration the state machine's totality
/// tests fold through `step`.
pub(crate) fn all_signals() -> Vec<LifecycleSignal> {
    let mut signals = vec![
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted,
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
        });
    }
    for auto in [None, Some(false), Some(true)] {
        signals.push(LifecycleSignal::CompactionEnded { auto });
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
            signals.push(LifecycleSignal::ToolUsed { mutates, edits });
        }
    }
    signals
}
