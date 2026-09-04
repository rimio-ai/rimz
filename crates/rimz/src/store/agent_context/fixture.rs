use std::path::PathBuf;

use crate::agents::context::record::AgentContextRecord;
use crate::disk::atomic;
use crate::disk::paths::RuntimePaths;
use crate::store::sidecar;

use super::update_record;

/// Sidecar file for one session's record; test fixture access only.
pub fn path_for(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> PathBuf {
    sidecar::path(
        &runtime.agent_context_dir,
        <AgentContextRecord as sidecar::SidecarRecord>::FILE_PREFIX,
        kind,
        agent_id,
    )
}

/// Persist a fully-shaped sidecar fixture while preserving concurrently owned
/// cost and spend state. Production mutations use [`update_record`].
#[doc(hidden)]
pub fn write_record(
    runtime: &RuntimePaths,
    record: &AgentContextRecord,
) -> Result<(), atomic::AtomicErr> {
    let observed_at = record.context.observed_at;
    update_record(
        runtime,
        record.kind.as_str(),
        record.agent_id.as_str(),
        observed_at,
        |current, existed| current.apply_fixture(record.clone(), existed),
    )
    .map(|_| ())
}
