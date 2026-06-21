use std::path::Path;

use super::*;

use crate::agents::AgentStatus;
use crate::agents::lifecycle;
use crate::ids::WorkspaceId;
use crate::ledger::snapshot::testkit::*;

/// Test-local shorthand over [`merge_agent_rollups_with_tombstones`]
/// with no tombstones in play.
fn merge_agent_rollups(base: &[AgentState], live: &[AgentState]) -> Vec<AgentState> {
    merge_agent_rollups_with_tombstones(base, live, &BTreeSet::new())
}

mod cache;
mod integrity;
mod merge;
mod rotation;
