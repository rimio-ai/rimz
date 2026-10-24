//! Facts about a managed checkout before a new launch enters it.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::harness::schedule::takeover::{self, Takeover};
use crate::utils::path::normalize_path_lexical;
use crate::worktree::{self, WorktreeErr, WorktreeMarker, WorktreeStatus};

/// Git state and recorded activity for a managed checkout.
#[derive(Debug)]
pub struct WorktreeEntry {
    pub created_at: Timestamp,
    pub status: WorktreeStatus,
    /// Whether a non-force removal would take the tree once nothing holds it.
    pub removable: bool,
    pub last_activity: Option<Timestamp>,
}

/// Whether a live agent works in the checkout, by the takeover rule over runtime-scoped rows.
/// An occupant holding an open turn counts.
pub fn occupied(
    path: &Path,
    project_root: &Path,
    alive: &[AgentState],
) -> Result<bool, WorktreeErr> {
    Ok(match takeover::read(alive, path, project_root)? {
        Takeover::Stop(occupants) => !occupants.is_empty(),
        Takeover::Blocked(_) => true,
    })
}

/// Read a marked tree's Git state and the newest activity among audit rows at or under it.
pub fn read(
    path: &Path,
    marker: &WorktreeMarker,
    audit: &[AgentState],
) -> Result<WorktreeEntry, WorktreeErr> {
    let memo = worktree::LandedMemo::load(path);
    let state = worktree::inspect(path, marker, &Default::default(), &memo);
    memo.save();
    let state = state?;
    let checkouts = std::iter::once(path.to_path_buf())
        .chain(path.canonicalize().ok())
        .map(|path| normalize_path_lexical(&path))
        .collect::<Vec<_>>();
    Ok(WorktreeEntry {
        created_at: marker.created_at,
        removable: state.removable(),
        status: state.status,
        last_activity: last_activity(audit, &checkouts),
    })
}

fn last_activity(agents: &[AgentState], checkouts: &[PathBuf]) -> Option<Timestamp> {
    agents
        .iter()
        .filter(|agent| {
            agent.lane_path().is_some_and(|path| {
                let path = normalize_path_lexical(Path::new(path));
                checkouts.iter().any(|checkout| path.starts_with(checkout))
            })
        })
        .map(|agent| agent.last_activity)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::{AgentKind, AgentSessionId};

    #[test]
    fn activity_includes_ended_rows_and_both_path_forms_but_not_siblings() {
        let row = |id, path: Option<&str>, seconds| {
            let mut row = AgentState::seed(
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from(id),
                AgentStatus::Idle,
                Timestamp::from_second(seconds).unwrap(),
            );
            row.worktree_path = path.map(str::to_owned);
            row.ended_at = Some(row.last_activity);
            row
        };
        let agents = [
            row("old", Some("/link/demo"), 100),
            row("ended", Some("/physical/demo/src/../src"), 200),
            row("outside", Some("/link/demo-other"), 300),
            row("pathless", None, 400),
        ];
        let observed = [
            vec![PathBuf::from("/link/demo")],
            vec![PathBuf::from("/physical/demo")],
            vec![PathBuf::from("/link/demo"), PathBuf::from("/physical/demo")],
            vec![PathBuf::from("/absent")],
        ]
        .map(|paths| last_activity(&agents, &paths));
        assert_eq!(
            observed,
            [
                Some(agents[0].last_activity),
                Some(agents[1].last_activity),
                Some(agents[1].last_activity),
                None
            ]
        );
    }
}
