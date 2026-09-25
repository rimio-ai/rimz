//! Wrapper-side process lease registration and explicit resident-wrapper release.

use super::{LspErr, Result, registry};
use std::path::Path;

pub fn register(root: &Path, launch_id: Option<&str>, pid: u32) -> Result<()> {
    let start_token = crate::proc::process_start_token(pid)
        .ok_or_else(|| LspErr::Protocol(format!("cannot identify lease process {pid}")))?;
    registry::acknowledge_all(
        registry::live_for_checkout(root)?,
        &serde_json::json!({"op": "lease", "launch_id": launch_id, "pid": pid, "start_token": start_token}),
    )
}

pub fn release(root: &Path, launch_id: Option<&str>, pid: u32) -> Result<()> {
    let root = std::fs::canonicalize(root)
        .unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(root));
    registry::acknowledge_all(
        registry::read_entries()?
            .into_iter()
            .filter(|entry| entry.root == root),
        &serde_json::json!({"op": "release", "launch_id": launch_id, "pid": pid}),
    )
}
