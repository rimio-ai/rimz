//! Read-only Qwen Code JSONL token-spend parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::capabilities::LaunchCapability;
use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, SpendUpdate, iso_to_unix_secs, origin_path, price_split,
};

use super::payloads::{TranscriptRecord, fold_transcript};

/// The Qwen runtime output directory for one login env, or `None` when neither
/// the env's runtime dir, a system or system-defaults `runtimeOutputDir`, nor
/// a home names one.
pub(super) fn runtime_base(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let env_path = |key: &str| {
        env.get(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(path) = env_path("QWEN_RUNTIME_DIR") {
        return Some(path);
    }
    super::settings::layers(None, env)
        .iter()
        .rev()
        .find_map(|layer| {
            layer
                .value
                .pointer("/advanced/runtimeOutputDir")
                .filter(|value| !value.is_null())
        })
        .and_then(|value| {
            let path = Path::new(value.as_str()?);
            if path.is_absolute() {
                return Some(path.to_owned());
            }
            let relative = path.strip_prefix("~").ok()?;
            Some(env_path("HOME")?.join(relative))
        })
        .or_else(|| super::QwenAdapter.config_home(env))
}

/// Qwen rewinds can invalidate any earlier root assistant, so a readable file
/// is cold-folded and atomically replaces that file's cached entry set.
pub(super) fn parse_qwen_spend(
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
        update: SpendUpdate::Replace,
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
    let split = TokenSplit::new(input, output).cached(0, cached);
    let cost_usd = price_split(prices, model, split, timestamp, unknown_models).unwrap_or(0.0);
    Some(CachedEntry {
        message_id: record.uuid.clone().map(String::into_boxed_str),
        thread_id: record.session_id.clone().map(String::into_boxed_str),
        is_sidechain: record.is_sidechain == Some(true) || record.agent_id.is_some(),
        model: Some(model.into()),
        ..CachedEntry::new(timestamp, cost_usd, &split)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_output_dir_selects_spend_root_with_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let user = home.join(".qwen");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("settings.json"),
            r#"{"advanced":{"runtimeOutputDir":"~/runtime"}}"#,
        )
        .unwrap();
        let mut env = BTreeMap::from([
            ("HOME".into(), home.display().to_string()),
            (
                "QWEN_CODE_SYSTEM_SETTINGS_PATH".into(),
                home.join("settings.json").display().to_string(),
            ),
        ]);
        assert_eq!(runtime_base(&env), Some(home.join("runtime")));
        env.insert(
            "QWEN_RUNTIME_DIR".into(),
            home.join("override").display().to_string(),
        );
        assert_eq!(runtime_base(&env), Some(home.join("override")));
    }

    #[test]
    fn runtime_output_dir_highest_defining_layer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let user = home.join("user");
        std::fs::create_dir_all(&user).unwrap();
        let env = BTreeMap::from([
            ("HOME".into(), home.display().to_string()),
            ("QWEN_HOME".into(), user.display().to_string()),
            (
                "QWEN_CODE_SYSTEM_SETTINGS_PATH".into(),
                home.join("settings.json").display().to_string(),
            ),
        ]);
        let write = |path: PathBuf, value: &str| {
            std::fs::write(
                path,
                serde_json::json!({"advanced":{"runtimeOutputDir":value}}).to_string(),
            )
            .unwrap();
        };
        write(home.join("system-defaults.json"), "/defaults");
        assert_eq!(runtime_base(&env).as_deref(), Some(Path::new("/defaults")));
        write(user.join("settings.json"), "/user");
        assert_eq!(runtime_base(&env).as_deref(), Some(Path::new("/user")));
        write(home.join("settings.json"), "/system");
        assert_eq!(runtime_base(&env).as_deref(), Some(Path::new("/system")));
        for unresolvable in ["", "relative", "~someone/runtime"] {
            write(home.join("settings.json"), unresolvable);
            assert_eq!(
                runtime_base(&env).as_deref(),
                Some(user.as_path()),
                "{unresolvable:?}"
            );
        }
        std::fs::write(
            home.join("settings.json"),
            r#"{"advanced":{"runtimeOutputDir":null}}"#,
        )
        .unwrap();
        assert_eq!(runtime_base(&env).as_deref(), Some(Path::new("/user")));
    }

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
        let first = parse_qwen_spend(&path, None, &PriceBook::fixture());
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

        let resumed = SpendCursor::with_state(999, &"ignored");
        let second = parse_qwen_spend(&path, Some(&resumed), &PriceBook::fixture());
        assert_eq!(second.update, SpendUpdate::Replace);
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
        let parsed = parse_qwen_spend(&path, None, &PriceBook::fixture());
        assert!(parsed.entries.is_empty());
        assert_eq!(parsed.update, SpendUpdate::Replace);
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
        let parsed = parse_qwen_spend(&path, None, &PriceBook::fixture());
        assert_eq!(parsed.entries.len(), 2);
        assert!(!parsed.entries[0].is_sidechain);
        assert!(parsed.entries[1].is_sidechain);
    }

    #[test]
    fn unreadable_transcript_is_not_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse_qwen_spend(dir.path(), None, &PriceBook::fixture());
        assert_ne!(parsed.update, SpendUpdate::Replace);
    }
}
