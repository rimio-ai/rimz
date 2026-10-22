//! Learned peak memory, partitioned by project, server, and executable settings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Record {
    pub at_ms: u64,
    pub root: PathBuf,
    pub project: Option<PathBuf>,
    pub server: String,
    pub settings_hash: String,
    pub peak_rss_kb: u64,
    pub ready_ms: Option<u64>,
    #[serde(default)]
    pub dormant_ms: Option<u64>,
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
                && record.ready_ms.is_some()
        })
        .take(5)
        .map(|record| record.peak_rss_kb.saturating_mul(1024))
        .max()
        .unwrap_or(fallback)
}

pub(super) fn estimate(project: &Path, server: &str, settings: &str, fallback: u64) -> u64 {
    Needs::read().estimate(project, server, settings, fallback)
}

/// Learned start needs, read once and reused across a command's registry rows.
pub struct Needs(Vec<Record>);

impl Needs {
    /// Snapshot the history log, skipping missing or malformed records.
    pub fn read() -> Self {
        Self::read_at(&crate::disk::paths::lsp_history_path())
    }

    fn read_at(path: &Path) -> Self {
        let mut records = Vec::new();
        crate::disk::rotating::visit_records(path, |record| records.push(record));
        Self(records)
    }

    /// Largest of the last five matching peaks, or the caller's fallback.
    fn estimate(&self, project: &Path, server: &str, settings: &str, fallback: u64) -> u64 {
        estimate_records(&self.0, project, server, settings, fallback)
    }

    /// What the row's next start needs: its project's learned peak, else the
    /// estimate it was last admitted on.
    pub fn for_entry(&self, entry: &super::registry::Entry) -> u64 {
        entry
            .project
            .as_deref()
            .map_or(entry.estimate_bytes, |project| {
                self.estimate(
                    project,
                    &entry.server,
                    &entry.settings_hash,
                    entry.estimate_bytes,
                )
            })
    }
}

pub(super) fn append(record: &Record) {
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
    if config.editor_check_on_save {
        value["editor-check-on-save"] = serde_json::json!(true);
    }
    value.sort_all_objects();
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_reads_once_and_keeps_keys_and_the_last_five_peaks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut log = String::from("not a record\n");
        let record = |project, server, settings, peak_rss_kb| Record {
            at_ms: 0,
            root: "/checkout".into(),
            project,
            server,
            settings_hash: settings,
            peak_rss_kb,
            ready_ms: Some(1),
            dormant_ms: None,
            reason: super::super::registry::StopReason::Idle,
        };
        for peak in [100, 2, 5, 3, 4, 1] {
            log += &serde_json::to_string(&record(
                Some("/project".into()),
                "rust".into(),
                "a".into(),
                peak,
            ))
            .unwrap();
            log.push('\n');
        }
        for (project, server, settings) in [
            (Some("/elsewhere".into()), "rust", "a"),
            (Some("/project".into()), "python", "a"),
            (Some("/project".into()), "rust", "b"),
            (None, "rust", "a"),
        ] {
            log += &serde_json::to_string(&record(project, server.into(), settings.into(), 200))
                .unwrap();
            log.push('\n');
        }
        std::fs::write(&path, &log).unwrap();
        let needs = Needs::read_at(&path);
        assert_eq!(
            needs.estimate(Path::new("/project"), "rust", "a", 99),
            5 * 1024
        );
        assert_eq!(
            needs.estimate(Path::new("/project"), "python", "a", 99),
            200 * 1024
        );
        assert_eq!(needs.estimate(Path::new("/unknown"), "rust", "a", 99), 99);
        assert_eq!(
            Needs::read_at(&dir.path().join("missing")).estimate(
                Path::new("/project"),
                "rust",
                "a",
                99
            ),
            99
        );
        log += &serde_json::to_string(&record(
            Some("/project".into()),
            "rust".into(),
            "a".into(),
            300,
        ))
        .unwrap();
        log.push('\n');
        std::fs::write(&path, log).unwrap();
        assert_eq!(
            needs.estimate(Path::new("/project"), "rust", "a", 99),
            5 * 1024
        );
        assert_eq!(
            Needs::read_at(&path).estimate(Path::new("/project"), "rust", "a", 99),
            300 * 1024
        );
    }

    #[test]
    fn editor_checks_have_separate_history_without_changing_default_hash() {
        let raw = serde_json::json!({"command":["rust-analyzer"],"extensions":["rs"],"root-markers":["Cargo.toml"],"init-options":{"checkOnSave":false}});
        let original = settings_hash(&serde_json::from_value(raw.clone()).unwrap());
        assert_eq!(
            original,
            "4dbbe4344d3ecc40ae672d1234167724fc5a4471e739e297d5763f5cef87b976"
        );
        let mut changed = raw;
        changed["editor-check-on-save"] = serde_json::json!(false);
        assert_eq!(
            settings_hash(&serde_json::from_value(changed.clone()).unwrap()),
            original
        );
        changed["editor-check-on-save"] = serde_json::json!(true);
        assert_ne!(
            settings_hash(&serde_json::from_value(changed).unwrap()),
            original
        );
    }

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
                ready_ms: Some(1),
                dormant_ms: None,
                reason: crate::lsp::registry::StopReason::Released,
            })
            .collect();
        assert_eq!(estimate_records(&records, root, "rust", "a", 99), 5 * 1024);
        assert_eq!(estimate_records(&records, root, "rust", "b", 99), 99);
    }

    #[test]
    fn learned_peak_is_shared_by_project_not_checkout() {
        let records: Vec<Record> = serde_json::from_value(serde_json::json!([
            {"at_ms": 0, "root": "/checkout-a", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 5, "ready_ms": 1, "reason": "released"},
            {"at_ms": 1, "root": "/checkout-b", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 7, "ready_ms": 1, "reason": "released"},
            {"at_ms": 2, "root": "/checkout-c", "project": "/other", "server": "rust", "settings_hash": "a", "peak_rss_kb": 100, "ready_ms": 1, "reason": "released"}
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
            {"at_ms": 0, "root": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 100, "ready_ms": 1, "reason": "released"}
        ])).unwrap();
        assert_eq!(
            estimate_records(&records, Path::new("/project"), "rust", "a", 99),
            99
        );
    }

    #[test]
    fn learned_peak_skips_partial_lifetimes_before_taking_five() {
        let project = Path::new("/project");
        let records: Vec<_> = [100, 9, 2, 3, 4, 5, 200, 1, 300, 2, 400, 3]
            .into_iter()
            .enumerate()
            .map(|(index, peak_rss_kb)| Record {
                at_ms: index as u64,
                root: project.into(),
                project: Some(project.into()),
                server: "rust".into(),
                settings_hash: "a".into(),
                peak_rss_kb,
                ready_ms: (index < 6).then_some(1),
                dormant_ms: None,
                reason: crate::lsp::registry::StopReason::MemoryPressure,
            })
            .collect();
        assert_eq!(
            estimate_records(&records, project, "rust", "a", 99),
            9 * 1024
        );
    }

    #[test]
    fn learned_peak_with_only_partial_lifetimes_uses_fallback() {
        let records: Vec<Record> = serde_json::from_value(serde_json::json!([
            {"at_ms": 0, "root": "/project", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 100, "ready_ms": null, "reason": "released"},
            {"at_ms": 1, "root": "/project", "project": "/project", "server": "rust", "settings_hash": "a", "peak_rss_kb": 200, "reason": "released"}
        ])).unwrap();
        assert_eq!(
            estimate_records(&records, Path::new("/project"), "rust", "a", 99),
            99
        );
    }
}
