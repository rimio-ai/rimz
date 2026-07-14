//! Read-only Qwen Code JSONL token-spend parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, origin_path, record_unknown_model,
};
use crate::agents::transcript_fs::home_dir;

use super::payloads::{TranscriptRecord, fold_transcript};

fn runtime_base() -> PathBuf {
    std::env::var_os("QWEN_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("QWEN_HOME").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".qwen"))
}

pub(crate) fn all_jsonl_files() -> Vec<PathBuf> {
    session_files_under(&runtime_base())
}

fn session_files_under(runtime: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = std::fs::read_dir(runtime.join("projects")) else {
        return files;
    };
    for project in projects.filter_map(Result::ok) {
        if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(chats) = std::fs::read_dir(project.path().join("chats")) else {
            continue;
        };
        for entry in chats.filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Qwen rewinds can invalidate any earlier root assistant, so a readable file
/// is cold-folded and atomically replaces that file's cached entry set.
pub(crate) fn parse_qwen_spend(
    path: &Path,
    _resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SpendParse::default();
    };
    let folded = fold_transcript(&text);
    let mut entries = Vec::new();
    let mut origin = None;
    let mut unknown_models = BTreeMap::new();
    for record in folded
        .active_root()
        .filter(|record| record.is_sidechain != Some(true) && record.agent_id.is_none())
        .chain(
            folded
                .physical
                .iter()
                .filter(|record| record.is_sidechain == Some(true) || record.agent_id.is_some()),
        )
    {
        let Some(entry) = priced_entry(record, prices, &mut unknown_models) else {
            continue;
        };
        if origin.is_none() {
            origin = origin_path(record.cwd.as_deref());
        }
        entries.push(entry);
    }
    SpendParse {
        entries,
        origin,
        cursor: SpendCursor::default(),
        unknown_models,
        cost_estimated: false,
        replace_entries: true,
    }
}

fn priced_entry(
    record: &TranscriptRecord,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    if record.r#type.as_deref() != Some("assistant") {
        return None;
    }
    let usage = record.usage_metadata.as_ref()?;
    if usage.prompt_token_count.is_none()
        && usage.cached_content_token_count.is_none()
        && usage.candidates_token_count.is_none()
        && usage.thoughts_token_count.is_none()
    {
        return None;
    }
    let model = record.model.as_deref()?.trim();
    if model.is_empty() {
        return None;
    }
    let timestamp = record.timestamp.as_deref().and_then(iso_to_unix_secs)?;
    let input = usage.uncached_prompt();
    let output = usage.output();
    let cached = usage.cache_read();
    let cost_usd = match prices.price(model) {
        Some(price) => price.cost(input, output, 0, 0, cached, false),
        None => {
            record_unknown_model(unknown_models, model, timestamp);
            0.0
        }
    };
    Some(CachedEntry {
        ts_secs: timestamp,
        cost_usd,
        input,
        output,
        cache_write: 0,
        cache_read: cached,
        message_id: record.uuid.clone(),
        request_id: None,
        dedup_key: None,
        thread_id: record.session_id.clone(),
        is_sidechain: record.is_sidechain == Some(true) || record.agent_id.is_some(),
        has_speed: false,
        model: Some(model.to_owned()),
        rolled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_retracts_rewound_root_and_prices_known_categories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"uuid":"u1","type":"user"}
{"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":25,"candidatesTokenCount":10,"thoughtsTokenCount":5}}
{"uuid":"u2","parentUuid":"a1","type":"user"}
{"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2026-06-02T10:01:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":200,"candidatesTokenCount":20}}"#,
        )
        .unwrap();
        let first = parse_qwen_spend(&path, None, &PriceBook::embedded());
        assert_eq!(first.entries.len(), 2);
        assert!(
            first
                .entries
                .iter()
                .map(|entry| entry.cost_usd)
                .sum::<f64>()
                > 0.0,
            "known Qwen models keep non-zero local pricebook estimates"
        );
        assert!(first.entries.iter().all(|entry| {
            entry.model.as_deref() == Some("qwen3-coder-plus")
                && entry.input + entry.cache_read + entry.output > 0
        }));
        assert!(
            !first.cost_estimated,
            "local Qwen dollars remain eligible for the existing budget aggregate"
        );

        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(
            concat!(
                "\n",
                r#"{"uuid":"rewind","parentUuid":"a1","type":"system"}"#,
                "\n",
                r#"{"uuid":"u3","parentUuid":"rewind","type":"user"}"#,
                "\n",
                r#"{"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2026-06-02T10:02:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":"120","cachedContentTokenCount":"20","candidatesTokenCount":"12","thoughtsTokenCount":false}}"#,
            )
            .as_bytes(),
        )
        .unwrap();

        let resumed = SpendCursor {
            offset: 999,
            state: Some("ignored".into()),
        };
        let second = parse_qwen_spend(&path, Some(&resumed), &PriceBook::embedded());
        assert!(second.replace_entries);
        assert_eq!(second.cursor, SpendCursor::default());
        assert_eq!(second.entries.len(), 2);
        assert!(
            second
                .entries
                .iter()
                .all(|entry| entry.message_id.as_deref() != Some("a2"))
        );
        let replacement = second
            .entries
            .iter()
            .find(|entry| entry.message_id.as_deref() == Some("a3"))
            .unwrap();
        assert_eq!(replacement.input, 100);
        assert_eq!(replacement.cache_read, 20);
        assert_eq!(replacement.output, 12);
    }

    #[test]
    fn total_only_usage_does_not_fabricate_spend() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"uuid":"a1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"totalTokenCount":100}}"#,
        )
        .unwrap();
        let parsed = parse_qwen_spend(&path, None, &PriceBook::embedded());
        assert!(parsed.entries.is_empty());
        assert!(parsed.replace_entries);
    }

    #[test]
    fn prices_overlapping_deepseek_thoughts_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"uuid":"a1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"deepseek-v4-pro","usageMetadata":{"promptTokenCount":38727,"cachedContentTokenCount":38656,"candidatesTokenCount":85,"thoughtsTokenCount":77,"totalTokenCount":38812}}"#,
        )
        .unwrap();
        let prices = PriceBook::from_litellm_json(
            r#"{"deepseek-v4-pro":{"input_cost_per_token":0.000001,"output_cost_per_token":0.000002,"cache_read_input_token_cost":0.0000002}}"#,
        );

        let parsed = parse_qwen_spend(&path, None, &prices);
        let entry = &parsed.entries[0];
        assert_eq!(entry.input, 71);
        assert_eq!(entry.cache_read, 38_656);
        assert_eq!(entry.output, 85);
        assert!((entry.cost_usd - 0.007_972_2).abs() < 1e-9);
    }

    #[test]
    fn preserves_physical_sidechain_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"uuid":"u1","type":"user"}
{"uuid":"a1","parentUuid":"u1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":10}}
{"uuid":"child","parentUuid":"a1","timestamp":"2026-06-02T10:01:00Z","type":"assistant","agentId":"child-1","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":20}}"#,
        )
        .unwrap();
        let parsed = parse_qwen_spend(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 2);
        assert!(!parsed.entries[0].is_sidechain);
        assert!(parsed.entries[1].is_sidechain);
    }

    #[test]
    fn unreadable_transcript_is_not_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse_qwen_spend(dir.path(), None, &PriceBook::embedded());
        assert!(!parsed.replace_entries);
    }

    #[test]
    fn discovery_accepts_only_direct_chat_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("projects/project-a/chats");
        std::fs::create_dir_all(chats.join("nested")).unwrap();
        std::fs::create_dir_all(dir.path().join("projects/project-b/other/chats")).unwrap();
        std::fs::write(chats.join("session.jsonl"), "{}\n").unwrap();
        std::fs::write(chats.join("session.json"), "{}").unwrap();
        std::fs::write(chats.join("session.jsonl.sidecar"), "{}").unwrap();
        std::fs::write(chats.join("nested/child.jsonl"), "{}\n").unwrap();
        std::fs::write(
            dir.path()
                .join("projects/project-b/other/chats/outside.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_eq!(
            session_files_under(dir.path()),
            [chats.join("session.jsonl")]
        );
    }
}
