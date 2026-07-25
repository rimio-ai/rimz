//! Amp private-cache discovery and whole-file spend folding.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, price_split};
use crate::agents::transcript_fs::{expand_tilde, home_dir};

use super::thread::{AmpThread, AmpUsage};

pub(super) fn data_root() -> PathBuf {
    std::env::var("AMP_DATA_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| expand_tilde(value.trim()))
        .unwrap_or_else(|| home_dir().join(".local/share/amp"))
}

pub(super) fn resolve_session_file(session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = prior_path.filter(|path| path.is_file()) {
        return Some(path.to_path_buf());
    }
    resolve_session_file_under(&data_root(), session_id)
}

fn resolve_session_file_under(root: &Path, session_id: &str) -> Option<PathBuf> {
    let session_id = session_id.trim();
    if !valid_session_id(session_id)
        || Path::new(session_id).components().count() != 1
        || !matches!(
            Path::new(session_id).components().next(),
            Some(Component::Normal(_))
        )
    {
        return None;
    }
    let path = root.join("threads").join(format!("{session_id}.json"));
    path.is_file().then_some(path)
}

pub(super) fn resolve_session_file_at(root: &Path, session_id: &str) -> Option<PathBuf> {
    resolve_session_file_under(root, session_id)
}

fn valid_session_id(session_id: &str) -> bool {
    session_id
        .strip_prefix("T-")
        .is_some_and(|suffix| !suffix.is_empty() && !suffix.contains(['/', '\\']))
}

pub(super) fn entries_from_thread(
    thread: &AmpThread,
    prices: &PriceBook,
) -> (Vec<CachedEntry>, BTreeMap<String, u64>) {
    let mut unknown_models = BTreeMap::new();
    let entries = thread
        .usage
        .iter()
        .map(|usage| entry_from_usage(&thread.id, usage, prices, &mut unknown_models))
        .collect();
    (entries, unknown_models)
}

fn entry_from_usage(
    thread_id: &str,
    usage: &AmpUsage,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> CachedEntry {
    let ts_secs = usage.at.as_second().max(0) as u64;
    let split =
        TokenSplit::new(usage.input, usage.output).cached(usage.cache_write, usage.cache_read);
    let cost_usd = price_split(prices, &usage.model, split, ts_secs, unknown_models).unwrap_or(0.0);
    CachedEntry {
        dedup_key: usage
            .native_id
            .as_ref()
            .map(|id| format!("amp:{thread_id}:{id}")),
        thread_id: Some(thread_id.to_owned()),
        model: Some(usage.model.clone()),
        ..CachedEntry::new(ts_secs, cost_usd, &split)
    }
}

pub(super) fn parse(path: &Path, prices: &PriceBook) -> SpendParse {
    let Ok(thread) = AmpThread::read(path) else {
        return SpendParse::default();
    };
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(thread.id.as_str()) {
        return SpendParse::default();
    }
    let (entries, unknown_models) = entries_from_thread(&thread, prices);
    SpendParse {
        entries,
        origin: None,
        cursor: SpendCursor::default(),
        unknown_models,
        replace_entries: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_resolution_rejects_traversal_and_reuses_a_valid_prior() {
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        let exact = threads.join("T-good.json");
        std::fs::write(&exact, "{}").unwrap();

        assert_eq!(
            resolve_session_file_under(dir.path(), "T-good"),
            Some(exact.clone())
        );
        for invalid in ["", "T-", "T-../secret", "T-good/other", "other"] {
            assert_eq!(resolve_session_file_under(dir.path(), invalid), None);
        }
        assert_eq!(
            resolve_session_file_under(dir.path(), " T-good "),
            Some(exact)
        );
    }

    #[test]
    fn whole_file_fold_replaces_valid_empty_and_preserves_on_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-good.json");
        std::fs::write(&path, r#"{"id":"T-good","messages":[]}"#).unwrap();
        let parsed = parse(&path, &PriceBook::fixture());
        assert!(parsed.replace_entries);
        assert!(parsed.entries.is_empty());
        assert_eq!(parsed.cursor, SpendCursor::default());

        std::fs::write(&path, "{").unwrap();
        assert!(!parse(&path, &PriceBook::fixture()).replace_entries);
        std::fs::write(&path, r#"{"id":"T-other","messages":[]}"#).unwrap();
        assert!(!parse(&path, &PriceBook::fixture()).replace_entries);
    }

    #[test]
    fn current_and_legacy_usage_price_and_unknown_models_keep_tokens() {
        let current = AmpThread::parse(
            r#"{"id":"T-a","messages":[{"role":"assistant","messageId":"m1","content":"ok","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","inputTokens":100,"outputTokens":20,"cacheCreationInputTokens":30,"cacheReadInputTokens":40}}]}"#,
        )
        .unwrap();
        let (entries, unknown) = entries_from_thread(&current, &PriceBook::fixture());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].thread_id.as_deref(), Some("T-a"));
        assert_eq!(entries[0].input, 100);
        assert!(entries[0].cost_usd > 0.0);
        assert!(unknown.is_empty());

        let unknown_thread = AmpThread::parse(
            r#"{"id":"T-b","usageLedger":{"events":[{"id":1,"timestamp":"2026-01-01T00:00:00Z","model":"future-model","tokens":{"total":50}}]}}"#,
        )
        .unwrap();
        let (entries, unknown) = entries_from_thread(&unknown_thread, &PriceBook::fixture());
        assert_eq!(entries[0].output, 50);
        assert_eq!(entries[0].cost_usd, 0.0);
        assert!(unknown.contains_key("future-model"));
    }

    #[test]
    fn per_thread_native_ids_get_distinct_provider_dedup_keys() {
        let usage = AmpUsage {
            at: "2026-01-01T00:00:00Z".parse().unwrap(),
            model: "gpt-5".to_owned(),
            native_id: Some("1".to_owned()),
            input: 10,
            output: 2,
            cache_write: 0,
            cache_read: 0,
        };
        let mut unknown = BTreeMap::new();
        let a = entry_from_usage("T-a", &usage, &PriceBook::fixture(), &mut unknown);
        let b = entry_from_usage("T-b", &usage, &PriceBook::fixture(), &mut unknown);

        assert_eq!(a.message_id, None);
        assert_eq!(a.dedup_key.as_deref(), Some("amp:T-a:1"));
        assert_eq!(b.dedup_key.as_deref(), Some("amp:T-b:1"));
        assert_ne!(a.dedup_key, b.dedup_key);
    }
}
