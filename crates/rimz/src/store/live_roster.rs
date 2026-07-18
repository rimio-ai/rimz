//! Persisted sidebar live roster for rebirth recovery.
//!
//! The elected sidebar producer writes the pane-backed root-agent set its mux
//! session would lose if it died. The next room birth reads this snapshot before
//! the new producer starts, scopes recovery to it, then clears it at the rebirth
//! boundary.

use std::collections::BTreeSet;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId};
use crate::store::atomic;

const LIVE_ROSTER_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRoster {
    pub version: u32,
    pub written_at: Timestamp,
    pub agents: BTreeSet<(AgentKind, AgentSessionId)>,
}

pub fn read(path: &Path) -> Option<LiveRoster> {
    let bytes = std::fs::read(path).ok()?;
    let roster: LiveRoster = serde_json::from_slice(&bytes).ok()?;
    (roster.version == LIVE_ROSTER_VERSION).then_some(roster)
}

pub fn publish(path: &Path, agents: BTreeSet<(AgentKind, AgentSessionId)>) -> atomic::Result<()> {
    atomic::write_temp_then_rename_cache(
        path,
        &LiveRoster {
            version: LIVE_ROSTER_VERSION,
            written_at: Timestamp::now(),
            agents,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_roster_round_trips_and_absent_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("live-roster.json");
        let agents = [(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess-a"),
        )]
        .into_iter()
        .collect();

        assert_eq!(read(&path), None);
        publish(&path, agents).expect("publish roster");

        let roster = read(&path).expect("read roster");
        assert_eq!(roster.version, LIVE_ROSTER_VERSION);
        assert_eq!(
            roster.agents,
            [(
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from("sess-a"),
            )]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn live_roster_ignores_bad_or_unknown_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("live-roster.json");

        std::fs::write(&path, b"not json").expect("write bad json");
        assert_eq!(read(&path), None);

        std::fs::write(
            &path,
            r#"{"version":999,"written_at":"1970-01-01T00:00:00Z","agents":[]}"#,
        )
        .expect("write unknown version");
        assert_eq!(read(&path), None);
    }
}
