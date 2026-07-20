//! Durable diagnostics for automatic sidebar focus repair.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, WorkspaceId};
use crate::mux::ClientPaneView;
use crate::store::RuntimePaths;
use crate::store::paths::state_home;

const NAME: &str = "focus-repairs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusRepairRecord {
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub generation: u64,
    pub evidence: Vec<ClientPaneView>,
    pub target: PaneId,
    pub outcome: FocusRepairOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusRepairOutcome {
    AcceptedUnconfirmed,
    Failed,
    Confirmed,
    Superseded,
    Invalidated,
}

pub fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn append(record: &FocusRepairRecord) {
    log(&state_home()).append(record);
}

/// Hand focus-repair evidence to a detached CLI writer so the renderer stays
/// read-only on user-global diagnostic history.
pub fn spawn_append(runtime: &RuntimePaths, record: &FocusRepairRecord) {
    let Ok(record_json) = serde_json::to_string(record) else {
        tracing::debug!("sidebar: failed to serialize focus-repair diagnostic");
        return;
    };
    let mut command = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    command.args([
        "sidebar",
        "record-focus-repair",
        "--record-json",
        &record_json,
    ]);
    if let Err(err) =
        crate::child_process::spawn_detached_reaped(&mut command, "focus-repair-diagnostic")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn focus-repair diagnostic writer",
        );
    }
}

pub fn parse(raw: &str) -> Result<FocusRepairRecord, FocusRepairParseError> {
    Ok(serde_json::from_str(raw)?)
}

#[derive(Debug, thiserror::Error)]
pub enum FocusRepairParseError {
    #[error("invalid focus-repair diagnostic record: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn recent(state_root: &Path) -> Vec<FocusRepairRecord> {
    let mut records = Vec::new();
    log(state_root).visit_records(|record| records.push(record));
    records.sort_by_key(|record| record.at);
    records
}

fn log(state_root: &Path) -> crate::diag::rotating::JsonlLog {
    crate::diag::rotating::JsonlLog::new(log_path(state_root), MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId};

    fn record() -> FocusRepairRecord {
        FocusRepairRecord {
            at: Timestamp::from_second(20).expect("timestamp"),
            nonce: Some("nonce-1".to_owned()),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace"),
            session_name: "rimz-test".to_owned(),
            generation: 7,
            evidence: vec![ClientPaneView {
                client_id: crate::mux::MuxClientId::Zellij(3),
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            }],
            target: PaneId::from_parts(MuxName::Zellij, "terminal_2"),
            outcome: FocusRepairOutcome::AcceptedUnconfirmed,
            error: None,
        }
    }

    #[test]
    fn record_round_trips_and_appends() {
        let record = record();
        let raw = serde_json::to_string(&record).expect("serialize");
        assert_eq!(parse(&raw).expect("parse"), record);

        let dir = tempfile::tempdir().expect("tempdir");
        log(dir.path()).append(&record);
        assert_eq!(recent(dir.path()), vec![record]);
    }

    #[test]
    fn parser_rejects_an_assist_record() {
        let raw = r#"{"at":"1970-01-01T00:00:20Z","assist":"auto_continue"}"#;
        assert!(parse(raw).is_err());
    }
}
