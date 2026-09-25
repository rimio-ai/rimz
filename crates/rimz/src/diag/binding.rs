//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace audit
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::path::PathBuf;

use crate::disk::paths::StatePaths;
use crate::disk::retention::ROTATING_LOG_MAX_BYTES as BINDING_LOG_MAX_BYTES;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";

fn path(state: &StatePaths) -> PathBuf {
    state.audit_path(BINDING_LOG_NAME)
}

pub fn append(state: &StatePaths, record: &impl serde::Serialize) {
    crate::disk::rotating::append(&path(state), BINDING_LOG_MAX_BYTES, record);
}
