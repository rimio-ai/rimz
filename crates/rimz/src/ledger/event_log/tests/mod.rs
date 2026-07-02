use std::path::Path;

use serde_json::json;

use super::*;
use crate::ids::WorkspaceId;
use crate::ledger::event::EventEnvelope;

mod frame;
mod recovery;
mod rotation;
mod roundtrip;

fn test_event(method: &str) -> EventEnvelope {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    EventEnvelope::new(
        workspace,
        "session",
        "rimz",
        "cli",
        method,
        json!({ "a": 1 }),
    )
}

fn methods(events: &[EventEnvelope]) -> Vec<&str> {
    events.iter().map(|e| e.method.as_str()).collect()
}
