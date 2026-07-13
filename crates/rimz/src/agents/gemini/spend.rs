//! Gemini transcript discovery and token-priced spend folding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, record_unknown_model,
};
use crate::agents::transcript_fs::home_dir;

use super::payloads::{GeminiMessage, fold_transcript};

pub(super) fn gemini_session_files() -> Vec<PathBuf> {
    session_files_under(&home_dir().join(".gemini/tmp"))
}

fn session_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return files;
    };
    for project in projects.filter_map(Result::ok) {
        let chats = project.path().join("chats");
        let Ok(entries) = std::fs::read_dir(chats) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "jsonl" | "json"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Gemini rewinds and checkpoints can invalidate any prior message, so each
/// refresh cold-folds the file and atomically replaces that file's cache set.
pub(super) fn parse_gemini_spend(
    path: &Path,
    _resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SpendParse::default();
    };
    let folded = fold_transcript(&text);
    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    for message in folded
        .messages
        .iter()
        .filter(|message| message.kind.as_deref() == Some("gemini"))
    {
        if let Some(entry) = priced_entry(
            message,
            folded.session_id.as_deref(),
            prices,
            &mut unknown_models,
        ) {
            entries.push(entry);
        }
    }
    SpendParse {
        entries,
        origin: None,
        cursor: SpendCursor::default(),
        unknown_models,
        replace_entries: true,
    }
}

fn priced_entry(
    message: &GeminiMessage,
    session_id: Option<&str>,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let tokens = message.tokens.as_ref()?;
    let model = message.model.as_deref()?.trim();
    if model.is_empty() {
        return None;
    }
    let ts_secs = message.timestamp.as_deref().and_then(iso_to_unix_secs)?;
    let cached = tokens.cached.unwrap_or(0);
    let input = tokens.input.unwrap_or(0).saturating_sub(cached);
    let output = tokens
        .output
        .unwrap_or(0)
        .saturating_add(tokens.thoughts.unwrap_or(0));
    let cost_usd = match prices.price(model) {
        Some(price) => price.cost(input, output, 0, 0, cached, false),
        None => {
            record_unknown_model(unknown_models, model, ts_secs);
            0.0
        }
    };
    Some(CachedEntry {
        ts_secs,
        cost_usd,
        input,
        output,
        cache_write: 0,
        cache_read: cached,
        message_id: message.id.clone(),
        request_id: None,
        dedup_key: None,
        thread_id: session_id.map(ToOwned::to_owned),
        is_sidechain: false,
        has_speed: false,
        model: Some(model.to_owned()),
        rolled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_skips_nested_subagent_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("project/chats");
        std::fs::create_dir_all(chats.join("parent-session")).unwrap();
        std::fs::write(chats.join("session-main.jsonl"), "{}\n").unwrap();
        std::fs::write(chats.join("session-legacy.json"), "{}").unwrap();
        std::fs::write(chats.join("parent-session/child.jsonl"), "{}\n").unwrap();
        let files = session_files_under(dir.path());
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|path| !path.ends_with("child.jsonl")));
    }

    #[test]
    fn unreadable_transcript_is_not_an_authoritative_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse_gemini_spend(dir.path(), None, &PriceBook::embedded());
        assert!(!parsed.replace_entries);
    }

    #[test]
    fn spend_fold_dedups_checkpoints_rewinds_and_prices_thoughts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-12345678.jsonl");
        std::fs::write(
            &path,
            r#"{"sessionId":"sess-1"}
{"id":"a","timestamp":"2026-06-02T10:00:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":100,"output":20,"cached":40,"thoughts":5,"total":125}}
{"id":"a","timestamp":"2026-06-02T10:00:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":200,"output":30,"cached":50,"thoughts":10,"total":240}}
{"id":"b","timestamp":"2026-06-02T10:01:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":50,"output":10,"cached":0,"thoughts":0,"total":60}}
{"$rewindTo":"b"}
{"id":"c","timestamp":"2026-06-02T10:02:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":80,"output":8,"cached":20,"thoughts":2,"total":90}}"#,
        )
        .unwrap();
        let parsed = parse_gemini_spend(&path, None, &PriceBook::embedded());
        assert!(parsed.replace_entries);
        assert_eq!(parsed.cursor, SpendCursor::default());
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].input, 150);
        assert_eq!(parsed.entries[0].output, 40);
        assert_eq!(parsed.entries[0].cache_read, 50);
        assert!(
            parsed
                .entries
                .iter()
                .map(|entry| entry.cost_usd)
                .sum::<f64>()
                > 0.0
        );
    }

    #[test]
    fn legacy_whole_record_sessions_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-legacy.json");
        std::fs::write(
            &path,
            r#"{"sessionId":"sess-1","messages":[{"id":"a","timestamp":"2026-06-02T10:00:00Z","type":"gemini","model":"gemini-3-flash-preview","tokens":{"input":10,"output":2,"total":12}}]}"#,
        )
        .unwrap();
        let parsed = parse_gemini_spend(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 1);
        assert!(parsed.entries[0].cost_usd > 0.0);
    }
}
