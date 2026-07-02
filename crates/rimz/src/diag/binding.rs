//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace runtime
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::path::PathBuf;

use super::JsonlLog;
use crate::ledger::paths::RuntimePaths;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";
const BINDING_LOG_MAX_BYTES: u64 = 1_048_576;

fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(BINDING_LOG_NAME)
}

pub fn log(runtime: &RuntimePaths) -> JsonlLog {
    JsonlLog::new(path(runtime), BINDING_LOG_MAX_BYTES)
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

        let log = log(&runtime);
        log.append(&serde_json::json!({ "event": "selected" }));

        let bytes = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(bytes, "{\"event\":\"selected\"}\n");
    }
}
