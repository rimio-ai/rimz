//! Provider-neutral local-session and daemon evidence services.

use std::collections::BTreeSet;
#[cfg(feature = "testkit")]
use std::path::Path;
use std::path::PathBuf;

use jiff::Timestamp;

use crate::ids::AgentSessionId;

use super::{AgentTurnError, ProviderCapacity};

/// Provider-local sessions that live panes in one workspace can bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSessionDemand {
    /// Absolute, lexically normalized workspace carried by every result.
    pub workspace: PathBuf,
    /// Every admitted live pane in this workspace that may host the kind.
    pub panes: Vec<PaneSessionNeed>,
}

/// Named identities and the unsettled discovery window for one live pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSessionNeed {
    /// Its stamped root and newest non-provisional launch of this kind.
    /// Finding either settles the pane, including a launched child.
    pub own: Vec<NamedSession>,
    /// Further stamped roots and the pane's command-line resume identity.
    pub named: Vec<NamedSession>,
    /// Process start, else launch registration; absent for a resume pane or
    /// a pane with neither clock. Used only while no own session was found.
    pub fresh_since: Option<Timestamp>,
}

/// A session id with an untrusted, hook-reported transcript path hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSession {
    pub id: AgentSessionId,
    /// The adapter validates this hint against its account home and id.
    pub transcript_path: Option<PathBuf>,
}

impl PaneSessionDemand {
    /// Earliest clock of panes with no found own session; `None` when every
    /// pane is settled or has no clock.
    pub fn fresh_window(&self, found: impl Fn(&AgentSessionId) -> bool) -> Option<Timestamp> {
        self.panes
            .iter()
            .filter(|pane| !pane.own.iter().any(|session| found(&session.id)))
            .filter_map(|pane| pane.fresh_since)
            .min()
    }
}

/// Result of an exact-id lookup in one provider account home.
#[derive(Debug)]
pub enum LocalSessionLookup {
    Unsupported,
    NotFound,
    Found(Box<super::LocalSessionObservation>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonSessionEvidence {
    pub pids: BTreeSet<u32>,
    pub loaded_session_ids: Option<BTreeSet<String>>,
}

pub(crate) fn daemon_session_evidence(
    kind: &str,
    login_env: &std::collections::BTreeMap<String, String>,
) -> DaemonSessionEvidence {
    super::find_definition(kind).map_or_else(DaemonSessionEvidence::default, |definition| {
        definition.daemon_session_evidence(login_env)
    })
}

pub fn turn_death_needs_pane_confirmation(kind: &str, error: &AgentTurnError) -> bool {
    super::find_definition(kind)
        .is_some_and(|definition| definition.turn_death_needs_pane_confirmation(error))
}

pub(crate) fn refine_turn_death_from_frame(kind: &str, error: &mut AgentTurnError, frame: &str) {
    if let Some(definition) = super::find_definition(kind) {
        definition.refine_turn_death_from_frame(error, frame);
    }
}

pub(crate) fn infer_turn_death_from_spent_window(
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
mod tests {
    use super::*;

    #[test]
    fn fresh_window_tracks_only_unsettled_panes() {
        let early = Timestamp::from_second(10).unwrap();
        let late = Timestamp::from_second(20).unwrap();
        let named = |id| NamedSession {
            id: AgentSessionId::from(id),
            transcript_path: None,
        };
        let mut demand = PaneSessionDemand {
            workspace: PathBuf::from("/repo"),
            panes: vec![
                PaneSessionNeed {
                    own: vec![named("root"), named("launch")],
                    named: vec![named("co-resident")],
                    fresh_since: Some(early),
                },
                PaneSessionNeed {
                    own: vec![named("other")],
                    named: Vec::new(),
                    fresh_since: Some(late),
                },
                PaneSessionNeed {
                    own: Vec::new(),
                    named: vec![named("resumed")],
                    fresh_since: None,
                },
            ],
        };
        assert_eq!(demand.fresh_window(|_| false), Some(early));
        for id in ["root", "launch"] {
            assert_eq!(demand.fresh_window(|found| found == id), Some(late));
        }
        assert_eq!(demand.fresh_window(|id| id == "other"), Some(early));
        assert_eq!(demand.fresh_window(|id| id == "co-resident"), Some(early));
        assert_eq!(demand.fresh_window(|id| id == "resumed"), Some(early));
        assert_eq!(demand.fresh_window(|_| true), None);
        demand.panes.clear();
        assert_eq!(demand.fresh_window(|_| false), None);
    }
}
