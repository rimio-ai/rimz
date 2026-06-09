use std::path::Path;

use super::*;

use crate::agents::lifecycle;
use crate::feed::AgentStatus;
use crate::ids::WorkspaceId;
use crate::ledger::snapshot::testkit::*;

/// Test-local shorthand over [`merge_agent_rollups_with_tombstones`]
/// with no tombstones in play.
fn merge_agent_rollups(base: &[AgentState], live: &[AgentState]) -> Vec<AgentState> {
    merge_agent_rollups_with_tombstones(base, live, &BTreeSet::new())
}

mod cache;
mod fold_correctness;
mod integrity;
mod merge;
mod rotation;
