//! Learned peak memory, partitioned by project, server, and executable settings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub at_ms: u64,
    pub root: PathBuf,
    pub project: Option<PathBuf>,
    pub server: String,
    pub settings_hash: String,
    pub peak_rss_kb: u64,
    pub ready_ms: Option<u64>,
    pub reason: super::registry::StopReason,
}

fn estimate_records(
    records: &[Record],
    project: &Path,
    server: &str,
    settings: &str,
    fallback: u64,
) -> u64 {
    records
        .iter()
        .rev()
        .filter(|record| {
            record.project.as_deref() == Some(project)
                && record.server == server
                && record.settings_hash == settings
        })
        .take(5)
        .map(|record| record.peak_rss_kb.saturating_mul(1024))
        .max()
        .unwrap_or(fallback)
}

pub fn estimate(project: &Path, server: &str, settings: &str, fallback: u64) -> u64 {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&crate::disk::paths::lsp_history_path(), |record| {
        records.push(record)
    });
    estimate_records(&records, project, server, settings, fallback)
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
                project: Some(root.into()),
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

    #[test]
    fn learned_peak_is_shared_by_project_not_checkout() {
        let records: Vec<Record> = serde_json::from_value(serde_json::json!([
            {"at_ms": 0, "root": "/checkout-a", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 5, "ready_ms": null, "reason": "released"},
            {"at_ms": 1, "root": "/checkout-b", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 7, "ready_ms": null, "reason": "released"},
            {"at_ms": 2, "root": "/checkout-c", "project": "/other", "server": "rust", "settings_hash": "a", "peak_rss_kb": 100, "ready_ms": null, "reason": "released"}
        ])).unwrap();
        assert_eq!(
            estimate_records(&records, Path::new("/project"), "rust", "a", 99),
            7 * 1024
        );
        assert_eq!(
            estimate_records(&records, Path::new("/other"), "rust", "a", 99),
            100 * 1024
        );
        assert_eq!(
            estimate_records(&records, Path::new("/new"), "rust", "a", 99),
            99
        );
        assert_eq!(
            estimate_records(&records, Path::new("/project"), "other", "a", 99),
            99
        );
    }

    #[test]
    fn learned_peak_skips_legacy_records_without_project() {
        let records: Vec<Record> = serde_json::from_value(serde_json::json!([
            {"at_ms": 0, "root": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 100, "ready_ms": null, "reason": "released"}
        ])).unwrap();
        assert_eq!(
            estimate_records(&records, Path::new("/project"), "rust", "a", 99),
            99
        );
    }
}
