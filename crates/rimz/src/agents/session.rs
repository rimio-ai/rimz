//! Provider-neutral local-session and daemon evidence services.

use std::collections::BTreeSet;
#[cfg(any(test, feature = "testkit"))]
use std::path::Path;

use jiff::Timestamp;

#[cfg(test)]
use super::adapters::codex;
use super::{AgentTurnError, ProviderCapacity};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonSessionEvidence {
    pub pids: BTreeSet<u32>,
    pub loaded_session_ids: Option<BTreeSet<String>>,
}

pub fn daemon_session_evidence(kind: &str) -> DaemonSessionEvidence {
    super::find_definition(kind).map_or_else(DaemonSessionEvidence::default, |definition| {
        definition.daemon_session_evidence()
    })
}

pub fn turn_death_needs_pane_confirmation(kind: &str, error: &AgentTurnError) -> bool {
    super::find_definition(kind)
        .is_some_and(|definition| definition.turn_death_needs_pane_confirmation(error))
}

pub fn refine_turn_death_from_frame(kind: &str, error: &mut AgentTurnError, frame: &str) {
    if let Some(definition) = super::find_definition(kind) {
        definition.refine_turn_death_from_frame(error, frame);
    }
}

pub fn infer_turn_death_from_spent_window(
    kind: &str,
    error: &mut AgentTurnError,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) {
    if let Some(definition) = super::find_definition(kind) {
        definition.infer_turn_death_from_spent_window(error, capacity, now);
    }
}

#[cfg(feature = "testkit")]
#[doc(hidden)]
pub fn discover_local_sessions_under(
    kind: &str,
    home: &Path,
    workspaces: &[&Path],
) -> Vec<super::LocalSessionObservation> {
    super::find_definition(kind).map_or_else(Vec::new, |definition| {
        definition.discover_local_sessions_under(home, workspaces)
    })
}

#[cfg(test)]
pub(crate) fn with_sessions_root<T>(kind: &str, path: &Path, f: impl FnOnce() -> T) -> T {
    match kind {
        "codex" => codex::with_codex_sessions_root(path, f),
        _ => f(),
    }
}
