//! Learned peak memory, partitioned by checkout, server, and executable settings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub at_ms: u64,
    pub root: PathBuf,
    pub server: String,
    pub settings_hash: String,
    pub peak_rss_kb: u64,
    pub ready_ms: Option<u64>,
    pub reason: super::registry::StopReason,
}

fn estimate_records(
    records: &[Record],
    root: &Path,
    server: &str,
    settings: &str,
    fallback: u64,
) -> u64 {
    records
        .iter()
        .rev()
        .filter(|record| {
            record.root == root && record.server == server && record.settings_hash == settings
        })
        .take(5)
        .map(|record| record.peak_rss_kb.saturating_mul(1024))
        .max()
        .unwrap_or(fallback)
}

pub fn estimate(root: &Path, server: &str, settings: &str, fallback: u64) -> u64 {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&crate::disk::paths::lsp_history_path(), |record| {
        records.push(record)
    });
    estimate_records(&records, root, server, settings, fallback)
}

pub fn append(record: &Record) {
    crate::disk::rotating::append(
        &crate::disk::paths::lsp_history_path(),
        4 * 1024 * 1024,
        record,
    );
}

pub fn settings_hash(config: &crate::config::LspServerConfig) -> String {
    use sha2::{Digest, Sha256};
    let mut value =
        serde_json::json!({"command": config.command, "init-options": config.init_options});
    value.sort_all_objects();
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_peak_uses_last_five_matching_records_and_new_settings_start_fresh() {
        let root = Path::new("/checkout");
        let records: Vec<_> = [100, 2, 5, 3, 4, 1]
            .into_iter()
            .map(|peak_rss_kb| Record {
                at_ms: 0,
                root: root.into(),
                server: "rust".into(),
                settings_hash: "a".into(),
                peak_rss_kb,
                ready_ms: None,
                reason: crate::lsp::registry::StopReason::Released,
            })
            .collect();
        assert_eq!(estimate_records(&records, root, "rust", "a", 99), 5 * 1024);
        assert_eq!(estimate_records(&records, root, "rust", "b", 99), 99);
    }
}
