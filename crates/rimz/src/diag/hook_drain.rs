//! Durable anomaly evidence for ingress repair and dropped hook frames.

use crate::disk::paths::StatePaths;
use crate::ids::EventId;

#[derive(serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum HookDrainEvent {
    TruncateTail { start: u64, end: u64 },
    SkipRecord { start: u64, end: u64, error: String },
    ApplyFailed { ingress: EventId, error: String },
}

pub(crate) fn append(paths: &StatePaths, event: HookDrainEvent) {
    #[derive(serde::Serialize)]
    struct Record<'a> {
        at: jiff::Timestamp,
        workspace_id: &'a crate::ids::WorkspaceId,
        #[serde(flatten)]
        event: HookDrainEvent,
    }
    crate::disk::rotating::append(
        &paths.audit_path("hook-drain.log.jsonl"),
        crate::disk::retention::ROTATING_LOG_MAX_BYTES,
        &Record {
            at: jiff::Timestamp::now(),
            workspace_id: &paths.workspace_id,
            event,
        },
    );
}
