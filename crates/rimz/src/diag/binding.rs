//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace runtime
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::path::PathBuf;

use serde::Serialize;

use crate::ledger::paths::RuntimePaths;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";
const BINDING_LOG_MAX_BYTES: u64 = 1_048_576;

pub fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(BINDING_LOG_NAME)
}

pub fn append<T: Serialize>(runtime: &RuntimePaths, record: &T) {
    let path = path(runtime);
    if let Err(err) = super::rotating::append_rotating_jsonl(&path, BINDING_LOG_MAX_BYTES, record) {
        tracing::debug!(path = %path.display(), error = %err, "binding log append failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ids::WorkspaceId;

    #[test]
    fn append_writes_jsonl_record() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");

        append(&runtime, &serde_json::json!({ "event": "selected" }));

        let bytes = std::fs::read_to_string(path(&runtime)).unwrap();
        assert_eq!(bytes, "{\"event\":\"selected\"}\n");
    }
}
