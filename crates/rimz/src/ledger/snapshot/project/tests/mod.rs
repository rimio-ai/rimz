use std::path::Path;

use super::*;

use super::super::view::{attach_sub_agents, row_from_agent, sub_agent_from_state};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::AgentStatus;
use crate::ids::WorkspaceId;
use crate::ledger::snapshot::SidebarSnapshot;
use crate::ledger::snapshot::testkit::*;
use crate::schema::event::EventEnvelope;
use jiff::Timestamp;

fn project_workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(Path::new("/tmp/x"))
}

fn raw_lifecycle(source: &str, params: serde_json::Value) -> EventEnvelope {
    raw_lifecycle_in(&project_workspace(), source, params)
}

fn raw_lifecycle_in(
    workspace: &WorkspaceId,
    source: &str,
    params: serde_json::Value,
) -> EventEnvelope {
    EventEnvelope::new(
        workspace.clone(),
        "session",
        source,
        "agent-hook",
        "agent.lifecycle",
        params,
    )
}

fn raw_lifecycle_at(
    source: &str,
    secs_after_epoch: i64,
    params: serde_json::Value,
) -> EventEnvelope {
    let mut event = raw_lifecycle(source, params);
    event.timestamp = Timestamp::from_second(epoch().as_second() + secs_after_epoch).unwrap();
    event
}

mod capability;
mod compaction;
mod enrichment;
mod pane_binding;
mod phase_status;
mod prompt_task;
mod subagents;
mod timestamps;
