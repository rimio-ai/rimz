//! Machine-level language-server refusals, queue timeouts, kills, and crashes.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub at: jiff::Timestamp,
    pub root: PathBuf,
    pub server: String,
    pub event: String,
    pub details: serde_json::Value,
}

pub fn append(record: &Record) {
    crate::disk::rotating::append(
        &crate::disk::paths::logs_dir().join("lsp.log.jsonl"),
        4 * 1024 * 1024,
        record,
    );
}

pub fn recent() -> Vec<Record> {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(
        &crate::disk::paths::logs_dir().join("lsp.log.jsonl"),
        |record| records.push(record),
    );
    records
}
