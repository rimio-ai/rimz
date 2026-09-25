//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace runtime
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::path::PathBuf;

use crate::disk::paths::RuntimePaths;
use crate::disk::retention::ROTATING_LOG_MAX_BYTES as BINDING_LOG_MAX_BYTES;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";

fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(BINDING_LOG_NAME)
}

pub fn append(runtime: &RuntimePaths, record: &impl serde::Serialize) {
    crate::disk::rotating::append(&path(runtime), BINDING_LOG_MAX_BYTES, record);
}
