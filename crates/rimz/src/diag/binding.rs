//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace runtime
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::path::PathBuf;

use super::JsonlLog;
use crate::store::paths::RuntimePaths;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";
const BINDING_LOG_MAX_BYTES: u64 = 1_048_576;

fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(BINDING_LOG_NAME)
}

pub fn log(runtime: &RuntimePaths) -> JsonlLog {
    JsonlLog::new(path(runtime), BINDING_LOG_MAX_BYTES)
}
