//! Retention policy shared by state writers and lifetime-class collectors.
//!
//! Protocol liveness deadlines stay with their protocols, not in this policy.

use std::time::Duration;

pub const DEFAULT_RETENTION_ARG: &str = "14d";
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(14 * 86_400);
pub const CARRYOVER_RETENTION: Duration = Duration::from_secs(7 * 86_400);
pub const DEFAULT_EVENT_LOG_ROTATE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const DEFAULT_OLDER_THAN: Duration = Duration::from_secs(7 * 86_400);
pub(crate) const OWNED_GRACE: Duration = Duration::from_secs(7 * 86_400);
pub(crate) const AUDIT_RETENTION: Duration = Duration::from_secs(30 * 86_400);
pub(crate) const AUDIT_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const ROTATING_LOG_MAX_BYTES: u64 = 1_048_576;
pub(crate) const TRANSCRIPT_FILE_DAYS: u32 = 7;
pub(crate) const RESUME_OUTCOME_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
pub(crate) const TRACE_MAX_BYTES: u64 = 1_048_576;
