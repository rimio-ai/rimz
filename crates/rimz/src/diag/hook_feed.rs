//! Machine-level hook feed failures, including failures before room resolution.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Record {
    pub at: jiff::Timestamp,
    pub source: String,
    pub event: Option<String>,
    pub error: String,
}

pub fn append(record: &Record) {
    crate::disk::rotating::append(
        &crate::disk::paths::logs_dir().join("hook-feed.log.jsonl"),
        4 * 1024 * 1024,
        record,
    );
}
