//! Durable diagnostics for automatic sidebar focus repair.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::paths::RuntimePaths;
use crate::disk::paths::logs_dir;
use crate::ids::{PaneId, WorkspaceId};
use crate::pane::ClientPaneView;

const NAME: &str = "focus-repairs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;

#[cfg(test)]
thread_local! {
    static APPENDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_appends() -> usize {
    APPENDS.with(|appends| appends.replace(0))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusRepairRecord {
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub generation: u64,
    pub evidence: Vec<ClientPaneView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<ClientPaneView>,
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    pub target: PaneId,
    pub outcome: FocusRepairOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_attempts() -> u32 {
    1
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

fn log_path(state_root: &Path) -> PathBuf {
    state_root.join(NAME)
}

fn append_to(state_root: &Path, record: &FocusRepairRecord) {
    crate::disk::rotating::append(&log_path(state_root), MAX_BYTES, record);
}

/// Hand focus-repair evidence to a detached CLI writer so the renderer stays
/// read-only on user-global diagnostic history.
pub(crate) fn spawn_append(runtime: &RuntimePaths, record: &FocusRepairRecord) {
    #[cfg(test)]
    APPENDS.with(|appends| appends.set(appends.get() + 1));
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

pub fn append_raw(raw: &str) -> Result<(), FocusRepairParseError> {
    append_to(&logs_dir(), &serde_json::from_str(raw)?);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum FocusRepairParseError {
    #[error("invalid focus-repair diagnostic record: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn recent(state_root: &Path) -> Vec<FocusRepairRecord> {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |record: FocusRepairRecord| {
        records.push(record);
    });
    records.sort_by_key(|record| record.at);
    records
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
                client_id: crate::ids::MuxClientId::Zellij(3),
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            }],
            observed: Vec::new(),
            attempts: 1,
            target: PaneId::from_parts(MuxName::Zellij, "terminal_2"),
            outcome: FocusRepairOutcome::AcceptedUnconfirmed,
            error: None,
        }
    }

    #[test]
    fn record_round_trips_and_appends() {
        let mut record = record();
        record.observed = record.evidence.clone();
        record.attempts = 3;
        let raw = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            serde_json::from_str::<FocusRepairRecord>(&raw).expect("parse"),
            record
        );

        let dir = tempfile::tempdir().expect("tempdir");
        append_to(dir.path(), &record);
        assert!(dir.path().join("focus-repairs.log.jsonl").is_file());
        assert_eq!(recent(dir.path()), vec![record]);
    }

    #[test]
    fn old_records_deserialize_with_dispatch_defaults() {
        let mut legacy = serde_json::to_value(record()).expect("record json");
        let fields = legacy.as_object_mut().expect("record object");
        fields.remove("observed");
        fields.remove("attempts");
        let parsed: FocusRepairRecord =
            serde_json::from_value(legacy).expect("old records remain readable");
        assert!(parsed.observed.is_empty());
        assert_eq!(parsed.attempts, 1);
        assert_eq!(parsed, record());
    }

    #[test]
    fn empty_observed_sample_is_omitted_from_the_wire() {
        let value = serde_json::to_value(record()).expect("record json");
        assert!(value.get("observed").is_none());
        assert_eq!(value["attempts"], 1);
    }

    #[test]
    fn parser_rejects_an_assist_record() {
        let raw = r#"{"at":"1970-01-01T00:00:20Z","assist":"auto_continue"}"#;
        assert!(append_raw(raw).is_err());
    }
}
