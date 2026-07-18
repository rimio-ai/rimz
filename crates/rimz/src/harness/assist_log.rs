//! User-global history for system-initiated assistance.
//!
//! Auto-redeem and auto-continue helpers append one best-effort JSONL record
//! after attempting their intervention. Readers fold the current file and its
//! single rotated predecessor for the stats dashboard and forensic timeline.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::harness::auto_redeem::RedeemReason;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::RuntimePaths;
use crate::store::paths::state_home;

const NAME: &str = "assists.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistRecord {
    pub at: Timestamp,
    #[serde(flatten)]
    pub assist: Assist,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub enum Assist {
    AutoRedeem {
        kind: String,
        reason: RedeemReason,
        request_id: String,
        credits: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soonest_expiry: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        natural_reset: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        windows_reset: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        window_resets: Vec<AssistWindowReset>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AutoContinue {
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        park: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parked_since: Option<Timestamp>,
        delivered: bool,
        message_id: String,
    },
    FocusRepair {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        workspace_id: crate::ids::WorkspaceId,
        session_name: String,
        generation: u64,
        evidence: Vec<crate::mux::ClientPaneView>,
        target: crate::ids::PaneId,
        outcome: FocusRepairOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistWindowReset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

pub fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn append(record: &AssistRecord) {
    append_to(&state_home(), record, MAX_BYTES);
}

/// Hand focus-repair evidence to a detached CLI writer so the renderer stays
/// read-only on user-global assist history.
pub fn spawn_focus_repair_append(runtime: &RuntimePaths, record: &AssistRecord) {
    if !matches!(record.assist, Assist::FocusRepair { .. }) {
        tracing::debug!("sidebar: ignored non-focus assist passed to focus writer");
        return;
    }
    let Ok(record_json) = serde_json::to_string(record) else {
        tracing::debug!("sidebar: failed to serialize focus-repair assist");
        return;
    };
    let mut command = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    command.args([
        "sidebar",
        "record-focus-assist",
        "--record-json",
        &record_json,
    ]);
    if let Err(err) =
        crate::child_process::spawn_detached_reaped(&mut command, "focus-repair-assist")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn focus-repair assist writer",
        );
    }
}

pub fn parse_focus_repair(raw: &str) -> Result<AssistRecord, FocusRepairParseError> {
    let record = serde_json::from_str::<AssistRecord>(raw)?;
    if !matches!(record.assist, Assist::FocusRepair { .. }) {
        return Err(FocusRepairParseError::WrongAssist);
    }
    Ok(record)
}

#[derive(Debug, thiserror::Error)]
pub enum FocusRepairParseError {
    #[error("invalid focus-repair assist record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("assist record is not a focus repair")]
    WrongAssist,
}

pub fn recent(state_root: &Path, since: Option<Timestamp>) -> Vec<AssistRecord> {
    let path = log_path(state_root);
    let mut records = Vec::new();
    append_records(&rotated_path(&path), since, &mut records);
    append_records(&path, since, &mut records);
    records.sort_by_key(|record| record.at);
    records
}

fn append_to(state_root: &Path, record: &AssistRecord, max_bytes: u64) {
    crate::diag::rotating::JsonlLog::new(log_path(state_root), max_bytes).append(record);
}

fn append_records(path: &Path, since: Option<Timestamp>, records: &mut Vec<AssistRecord>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<AssistRecord>(&line) else {
            continue;
        };
        if since.is_none_or(|since| record.at >= since) {
            records.push(record);
        }
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name("assists.log.1.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
    }

    fn redeem(at: i64, request_id: impl Into<String>) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoRedeem {
                kind: "codex".to_owned(),
                reason: RedeemReason::ExpiryRescue,
                request_id: request_id.into(),
                credits: 2,
                soonest_expiry: Some(ts(30)),
                natural_reset: Some(ts(40)),
                outcome: Some("reset".to_owned()),
                windows_reset: true,
                window_resets: vec![AssistWindowReset {
                    duration_mins: Some(300),
                    resets_at: Some(ts(50)),
                }],
                error: None,
            },
        }
    }

    fn resumed(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::AutoContinue {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                park: "rate_limit_window_reset".to_owned(),
                parked_since: Some(ts(10)),
                delivered: true,
                message_id: "msg_1".to_owned(),
            },
        }
    }

    fn focus_repair(at: i64) -> AssistRecord {
        AssistRecord {
            at: ts(at),
            assist: Assist::FocusRepair {
                nonce: Some("nonce-1".to_owned()),
                workspace_id: crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567")
                    .expect("workspace"),
                session_name: "rimz-test".to_owned(),
                generation: 7,
                evidence: vec![crate::mux::ClientPaneView {
                    client_id: crate::mux::MuxClientId::Zellij(3),
                    pane_id: crate::ids::PaneId::from_parts(
                        crate::ids::MuxName::Zellij,
                        "terminal_1",
                    ),
                }],
                target: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_2"),
                outcome: FocusRepairOutcome::AcceptedUnconfirmed,
                error: None,
            },
        }
    }

    #[test]
    fn variants_round_trip_through_the_wire_shape() {
        for record in [redeem(20, "request-1"), resumed(20), focus_repair(20)] {
            let json = serde_json::to_string(&record).expect("serialize");
            let decoded: AssistRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn focus_repair_parser_accepts_only_focus_assists() {
        let focus = focus_repair(20);
        let raw = serde_json::to_string(&focus).expect("serialize focus repair");
        assert_eq!(parse_focus_repair(&raw).expect("focus repair"), focus);

        let raw = serde_json::to_string(&resumed(20)).expect("serialize auto continue");
        assert!(matches!(
            parse_focus_repair(&raw),
            Err(FocusRepairParseError::WrongAssist)
        ));
    }

    #[test]
    fn append_rotates_and_reader_folds_both_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = redeem(10, "x".repeat(256));
        append_to(dir.path(), &first, 1);
        let second = resumed(20);
        append_to(dir.path(), &second, 1);

        assert!(rotated_path(&log_path(dir.path())).exists());
        assert_eq!(recent(dir.path(), None), vec![first, second]);
    }

    #[test]
    fn recent_filters_by_inclusive_timestamp_and_skips_bad_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            format!(
                "{}\nnot-json\n{}\n",
                serde_json::to_string(&redeem(10, "old")).expect("old"),
                serde_json::to_string(&resumed(20)).expect("new")
            ),
        )
        .expect("write log");

        assert_eq!(recent(dir.path(), Some(ts(20))), vec![resumed(20)]);
    }
}
