//! Retention policy shared by state writers and lifetime-class collectors.
//!
//! Protocol liveness deadlines stay with their protocols, not in this policy.

use std::time::Duration;

pub const DEFAULT_RETENTION_ARG: &str = "14d";
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(14 * 86_400);
pub const DEFAULT_EVENT_LOG_ROTATE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_OLDER_THAN: Duration = Duration::from_secs(7 * 86_400);
pub const OWNED_GRACE: Duration = Duration::from_secs(7 * 86_400);
pub const AUDIT_RETENTION: Duration = Duration::from_secs(30 * 86_400);
pub const AUDIT_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const ROTATING_LOG_MAX_BYTES: u64 = 1_048_576;
pub const TRANSCRIPT_FILE_DAYS: u32 = 7;

// Existing writer bounds remain until the audit-class collector replaces them.
pub(crate) const HISTORY_MAX_BYTES: u64 = 512 * 1024;
pub(crate) const HISTORY_KEEP_RECORDS: usize = 500;
pub(crate) const DIAG_FRAME_RING: usize = 8;
pub(crate) const CRASH_ARCHIVE_RETENTION: usize = 5;
