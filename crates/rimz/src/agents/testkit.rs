//! Shared fixtures for the adapter unit tests: the canonical bridge ask each
//! adapter renders decisions against, and the exhaustive lifecycle-signal
//! enumeration the totality tests sweep.

use std::path::Path;

use crate::agents::lifecycle::LifecycleSignal;
use crate::feed::{FeedItem, FeedKind, Surface};
use crate::ids::WorkspaceId;

/// The canonical pending bridge ask for one agent kind — the item every
/// adapter's `render_decision` goldens start from.
pub(crate) fn feed_item(kind: FeedKind, agent_kind: &str) -> FeedItem {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
    FeedItem::new(
        workspace,
        Surface::Bridge,
        kind,
        "allow?",
        agent_kind,
        "agent-hook",
    )
}

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
    ];
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
