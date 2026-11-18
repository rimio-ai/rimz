//! Pure upload types: the wire between the laptop's upload client and the
//! host's `rimz remote upload` receiver, and the room lane record the receiver
//! publishes for the sidebar's progress row.
//!
//! The wire is one JSON header line, one JSON reply line, the files' bytes in
//! header order, and one JSON result line. Unknown fields are ignored on both
//! sides, so either end may add one without a version change.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::RemoteTarget;
use crate::mux::CommandSpec;

const UPLOAD_SCHEMA_VERSION: &str = "rimz.upload.v1";
const UPLOAD_LANE_VERSION: &str = "rimz.upload-lane.v1";
const UPLOAD_LANE_FILE: &str = "upload.json";
/// A running record older than this belongs to a receiver that died.
pub const UPLOAD_RUNNING_STALE: Duration = Duration::from_secs(10);
/// A failure stays on the sidebar this long: the unwatched pull is at least
/// five seconds, plus one pass.
pub const UPLOAD_FAILED_HOLD: Duration = Duration::from_secs(8);

/// One file the laptop is about to send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadFile {
    pub name: String,
    pub size: u64,
}

/// The laptop's first line: the transfer's files and its size limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadHeader {
    pub v: String,
    pub limit: u64,
    pub files: Vec<UploadFile>,
}

impl UploadHeader {
    pub fn new(limit: u64, files: Vec<UploadFile>) -> Self {
        Self {
            v: UPLOAD_SCHEMA_VERSION.to_owned(),
            limit,
            files,
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == UPLOAD_SCHEMA_VERSION
    }

    pub fn total(&self) -> u64 {
        self.files
            .iter()
            .fold(0, |sum, file| sum.saturating_add(file.size))
    }
}

/// The receiver's ready line or its result line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadReply {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UploadReply {
    pub fn ready() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn done(paths: Vec<String>) -> Self {
        Self {
            ok: true,
            paths: Some(paths),
            error: None,
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            paths: None,
            error: Some(reason.into()),
        }
    }
}

/// Whether a transfer is still receiving or has failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    Running,
    Failed,
}

/// The room's upload lane record, written only by the receiver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadLane {
    pub v: String,
    pub id: String,
    pub state: UploadState,
    pub received: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at_ms: u64,
}

impl UploadLane {
    pub fn running(id: String, received: u64, total: u64, updated_at_ms: u64) -> Self {
        Self {
            v: UPLOAD_LANE_VERSION.to_owned(),
            id,
            state: UploadState::Running,
            received,
            total,
            reason: None,
            updated_at_ms,
        }
    }

    pub fn failed(id: String, total: u64, reason: String, updated_at_ms: u64) -> Self {
        Self {
            state: UploadState::Failed,
            reason: Some(reason),
            ..Self::running(id, 0, total, updated_at_ms)
        }
    }

    /// Whether the sidebar shows this record at `now_ms`, on the host's clock.
    pub fn live_at(&self, now_ms: u64) -> bool {
        let window = match self.state {
            UploadState::Running => UPLOAD_RUNNING_STALE,
            UploadState::Failed => UPLOAD_FAILED_HOLD,
        };
        self.v == UPLOAD_LANE_VERSION
            && now_ms.saturating_sub(self.updated_at_ms) <= window.as_millis() as u64
    }
}

/// The room's upload lane file, beside the link one.
pub fn lane_path(runtime: &crate::RuntimePaths) -> PathBuf {
    runtime.lane_path(UPLOAD_LANE_FILE)
}

/// The upload channel: the host's receiver for the room the attach targets,
/// as a second exec over the attach's control socket.
pub fn upload_stream_spec(target: &RemoteTarget, control_path: &Path) -> CommandSpec {
    CommandSpec::new(super::ssh_program())
        .args([
            "-S".to_owned(),
            control_path.display().to_string(),
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            // A remote tty would rewrite the binary stream, whatever the
            // user's `RequestTTY` says.
            "-T".to_owned(),
            "--".to_owned(),
        ])
        .arg(target.ssh_destination().as_str())
        .arg(format!(
            "{}; {}",
            super::remote_path_prefix(),
            target.exec_snippet("rimz remote upload --dir", "rimz remote upload --session"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upload_channel_runs_the_receiver_over_the_control_socket() {
        let target = RemoteTarget::parse("dev-box:/srv/app").expect("target");
        let spec = upload_stream_spec(&target, Path::new("/tmp/rimz-ctl"));
        let args = spec.args.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            args[..7],
            [
                "-S",
                "/tmp/rimz-ctl",
                "-o",
                "BatchMode=yes",
                "-T",
                "--",
                "dev-box"
            ]
        );
        assert!(
            args[7].contains("; exec rimz remote upload --dir ") && args[7].contains("/srv/app"),
            "{}",
            args[7]
        );
    }

    #[test]
    fn wire_lines_match_the_documented_shapes() {
        let header = UploadHeader::new(
            10,
            vec![UploadFile {
                name: "a.png".to_owned(),
                size: 3,
            }],
        );
        assert_eq!(
            serde_json::to_string(&header).unwrap(),
            r#"{"v":"rimz.upload.v1","limit":10,"files":[{"name":"a.png","size":3}]}"#
        );
        assert_eq!(header.total(), 3);
        assert_eq!(
            serde_json::to_string(&UploadReply::ready()).unwrap(),
            r#"{"ok":true}"#
        );
        assert_eq!(
            serde_json::to_string(&UploadReply::done(vec!["/r/a.png".to_owned()])).unwrap(),
            r#"{"ok":true,"paths":["/r/a.png"]}"#
        );
        assert_eq!(
            serde_json::to_string(&UploadReply::failed("full")).unwrap(),
            r#"{"error":"full"}"#
        );
    }

    #[test]
    fn unknown_fields_are_ignored_on_both_sides() {
        let header: UploadHeader = serde_json::from_str(
            r#"{"v":"rimz.upload.v1","limit":1,"files":[{"name":"a","size":1,"mode":1}],"x":2}"#,
        )
        .expect("header");
        assert!(header.version_ok());
        let reply: UploadReply = serde_json::from_str(r#"{"ok":true,"later":1}"#).expect("reply");
        assert_eq!(reply, UploadReply::ready());
    }

    #[test]
    fn lane_records_expire_by_state() {
        let running = UploadLane::running("t".to_owned(), 1, 2, 1_000);
        assert!(running.live_at(11_000));
        assert!(!running.live_at(11_001));
        let failed = UploadLane::failed("t".to_owned(), 2, "full".to_owned(), 1_000);
        assert!(failed.live_at(9_000));
        assert!(!failed.live_at(9_001));
        let unknown = UploadLane {
            v: "rimz.upload-lane.v9".to_owned(),
            ..running.clone()
        };
        assert!(!unknown.live_at(1_000));
        let parsed: UploadLane =
            serde_json::from_str(&serde_json::to_string(&running).unwrap()).unwrap();
        assert_eq!(parsed, running);
    }
}
