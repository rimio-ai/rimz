//! Channel name grammar and the shared channel/worktree namespace.
//!
//! A channel is never a stored object: worktree, team, explicit, and directory
//! lanes are all derived from what backs them. This file owns the two rules a
//! lane name answers to: its syntax, and that an explicit `--channel` lane and
//! a managed worktree never hold the same name.

use std::collections::BTreeSet;

use crate::agents::AgentState;
use crate::store::runtime::{AgentLiveness, agent_liveness};
use crate::workspace::{ResolvedWorkspace, RootClass};

#[derive(Debug, thiserror::Error)]
pub enum ChannelErr {
    #[error("invalid channel name `{name}`; use ASCII letters, numbers, `_`, or `-`")]
    InvalidName { name: String },
    #[error("channel `{name}` is backed by a worktree; use `--worktree {name}`")]
    WorktreeCollision { name: String },
    #[error(
        "channel `{name}` is held by live agents; pick another name, or stop them with `rimz agents stop`"
    )]
    LaneOccupied { name: String },
}

pub type Result<T> = std::result::Result<T, ChannelErr>;

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Admit an explicit `--channel` launch: no managed worktree owns the name and
/// the name is well formed. Reads Git worktree state only.
pub fn admit_launch(workspace: &ResolvedWorkspace, name: &str) -> Result<()> {
    if worktree_channel_names(workspace).contains(name) {
        return Err(ChannelErr::WorktreeCollision {
            name: name.to_owned(),
        });
    }
    if !valid_name(name) {
        return Err(ChannelErr::InvalidName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Admit a managed-worktree name: no live agent holds an explicit lane under it.
///
/// A worktree's own agents carry the same stamp as an explicit `--channel`
/// launch, so the checkout is what tells the two apart.
pub fn admit_worktree_name(agents: &[AgentState], name: &str) -> Result<()> {
    let occupied = agents.iter().any(|agent| {
        agent.ended_at.is_none()
            && agent.channel.as_deref() == Some(name)
            && agent
                .worktree_path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).file_name())
                != Some(std::ffi::OsStr::new(name))
            && agent_liveness(agent) != AgentLiveness::Dead
    });
    if occupied {
        return Err(ChannelErr::LaneOccupied {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Managed-worktree names as they appear in the shared channel namespace; an
/// unreadable worktree list claims no name.
fn worktree_channel_names(workspace: &ResolvedWorkspace) -> BTreeSet<String> {
    if workspace.root_class != RootClass::Repo {
        return BTreeSet::new();
    }
    crate::worktree::discover_owned(&workspace.project_root)
        .unwrap_or_default()
        .into_iter()
        .map(|worktree| worktree.branch.unwrap_or(worktree.marker.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
    use crate::workspace::WorkspaceResolver;
    use jiff::Timestamp;
    use tempfile::tempdir;

    fn lane_agent(channel: &str, worktree_path: &str) -> AgentState {
        AgentState {
            channel: Some(channel.to_owned()),
            worktree_path: Some(worktree_path.to_owned()),
            ..crate::testkit::agent_state("claude", "sess", Timestamp::UNIX_EPOCH)
        }
    }

    #[test]
    fn launch_admission_refuses_an_invalid_name() {
        let dir = tempdir().expect("tempdir");
        let workspace = WorkspaceResolver::resolve(dir.path(), None).expect("workspace");

        admit_launch(&workspace, "design").expect("bare name");
        let err = admit_launch(&workspace, "bad/name").expect_err("invalid name");

        assert!(matches!(err, ChannelErr::InvalidName { name } if name == "bad/name"));
    }

    #[test]
    fn worktree_name_admission_refuses_a_lane_live_agents_hold() {
        let held = [lane_agent("design", "/repo")];

        let err = admit_worktree_name(&held, "design").expect_err("occupied lane");

        assert!(matches!(err, ChannelErr::LaneOccupied { name } if name == "design"));
        admit_worktree_name(&held, "docs").expect("another name");
    }

    #[test]
    fn worktree_name_admission_ignores_ended_dead_and_worktree_agents() {
        let ended = AgentState {
            ended_at: Some(Timestamp::UNIX_EPOCH),
            ..lane_agent("design", "/repo")
        };
        let dead = AgentState {
            runtime_owner: Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess",
                u32::MAX,
                None,
            )),
            ..lane_agent("design", "/repo")
        };
        let in_worktree = lane_agent("design", "/repo/.worktrees/design");

        admit_worktree_name(&[ended, dead, in_worktree], "design").expect("free lane");
    }

    #[test]
    fn validates_bare_channel_names() {
        for name in ["design", "ops_2", "run-42"] {
            assert!(valid_name(name), "{name}");
        }
        for name in ["", "a.b", "a/b", "#ops", "two words"] {
            assert!(!valid_name(name), "{name}");
        }
    }
}
