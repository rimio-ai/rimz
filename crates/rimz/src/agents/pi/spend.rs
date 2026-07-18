//! Pi agent JSONL transcript parser.
//!
//! [`spending`](crate::agents::spending) consumes this parser through the
//! adapter's `parse_spend`, fed every Pi session fleet-wide by the producer.
//! Pi normally logs dollars directly (`usage.cost.total`), which stays
//! authoritative even when it is zero. Token-bearing entries whose cost is
//! absent fall back to the shared model price book.
//!
//! Pi session files live under `~/.pi/agent/sessions/--<cwd-with-dashes>--/`
//! as one `<ISO-timestamp>_<uuid>.jsonl` per session (e.g.
//! `2026-06-04T06-45-56-308Z_019e9161-….jsonl`, the uuid being the session
//! id). Upstream overrides: `--session-dir` / `PI_CODING_AGENT_SESSION_DIR`;
//! the `PI_AGENT_DIR` env honored here is RimZ's own comma-separated test
//! override, not a pi variable. Upstream shapes are mirrored in
//! `docs/externals/agent-adapter/pi-reference.md`. JSONL shape (one entry per
//! assistant turn):
//! ```json
//! { "type": "message",
//!   "timestamp": "2026-01-01T10:00:00.000Z",
//!   "message": { "role": "assistant",
//!                "model": "gpt-5",
//!                "usage": { "input": 100, "output": 50,
//!                           "cacheRead": 0, "cacheWrite": 0,
//!                           "totalTokens": 150,
//!                           "cost": { "total": 0.042 } } } }
//! ```
//!
//! No cross-file deduplication is needed: Pi sessions are per-conversation
//! single files and do not exhibit the btw/sidechain replay pattern.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, origin_path, record_unknown_model,
};

use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_object_lossy,
    deserialize_optional_string_lossy, deserialize_optional_u64_lossy, home_dir, read_spend_lines,
};

// ── Typed structs ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PiEntry {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    timestamp: Option<String>,
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    entry_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    message: Option<PiMessage>,
}

#[derive(Deserialize)]
struct PiSessionHeader {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    entry_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    cwd: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct PiSpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct PiMessage {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    role: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiUsage {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output: Option<u64>,
    #[serde(
        rename = "cacheWrite",
        default,
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_write: Option<u64>,
    #[serde(
        rename = "cacheRead",
        default,
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_read: Option<u64>,
    #[serde(
        rename = "totalTokens",
        default,
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    total_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    cost: Option<PiCost>,
}

#[derive(Deserialize)]
struct PiCost {
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    total: Option<f64>,
}

// ── Path discovery ────────────────────────────────────────────────────────────

pub(super) fn pi_session_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(env_val) = std::env::var("PI_AGENT_DIR") {
        for raw in env_val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(raw);
            roots.push(p);
        }
    } else if let Ok(raw) = std::env::var("PI_CODING_AGENT_SESSION_DIR") {
        roots.push(PathBuf::from(raw));
    } else {
        roots.push(pi_config_dir().join("sessions"));
    }

    roots
}

pub(crate) fn pi_config_dir() -> PathBuf {
    std::env::var("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".pi/agent"))
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a Pi JSONL file into `CachedEntry` values, resuming from `resume` when
/// given. The first line is a session header carrying `cwd`; the cursor stores
/// that origin so appended usage entries keep their workspace scope without
/// re-reading the header.
///
/// Accepts only lines where `"type":"message"` (or `type` is absent) and
/// `message.role == "assistant"`. A present non-negative
/// `message.usage.cost.total` is authoritative; otherwise token-bearing rows
/// are priced through `prices`. Any excess of `totalTokens` over the itemized
/// parts is folded into output so sparse records still contribute their full
/// reported total.
/// Lines without both `"usage"` and `"message"` keywords are skipped before
/// deserialization.
pub fn parse_pi_spend(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let from_offset = resume.map_or(0, |cursor| cursor.offset);
    let mut state: PiSpendState = resume
        .and_then(|cursor| cursor.state.clone())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let Some((content, next_offset)) = read_spend_lines(path, from_offset) else {
        return SpendParse {
            entries: Vec::new(),
            origin: state.cwd.clone(),
            cursor: SpendCursor {
                offset: from_offset,
                state: serde_json::to_value(&state).ok(),
            },
            unknown_models: BTreeMap::new(),
            replace_entries: false,
        };
    };
    let content = String::from_utf8_lossy(&content);
    let mut out = Vec::new();
    let mut unknown_models = BTreeMap::new();

    for line in content.lines() {
        if state.cwd.is_none()
            && let Ok(header) = serde_json::from_str::<PiSessionHeader>(line)
            && header.entry_type.as_deref() == Some("session")
        {
            state.cwd = origin_path(header.cwd.as_deref());
            continue;
        }
        if !line.contains(r#""usage""#) || !line.contains(r#""message""#) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<PiEntry>(line) else {
            continue;
        };
        if entry.entry_type.as_deref().is_some_and(|t| t != "message") {
            continue;
        }
        let Some(msg) = entry.message else { continue };
        if msg.role.as_deref() != Some("assistant") {
            continue;
        }
        let Some(usage) = msg.usage.as_ref() else {
            continue;
        };
        let Some(ts) = entry.timestamp.as_deref() else {
            continue;
        };
        let Some(ts_secs) = crate::agents::spending::iso_to_unix_secs(ts) else {
            continue;
        };
        let input = usage.input.unwrap_or(0);
        let mut output = usage.output.unwrap_or(0);
        let cache_write = usage.cache_write.unwrap_or(0);
        let cache_read = usage.cache_read.unwrap_or(0);
        // `totalTokens` is a reported grand total. Fold any excess over the
        // itemized parts into output so it is counted (and, when Pi omits a
        // direct cost, priced) as output — ccusage's `apply_total_token_fallback`
        // behavior. rimz has no separate extra-total bucket, so the gap rides
        // output rather than an unpriced side counter.
        let known_tokens = input
            .saturating_add(output)
            .saturating_add(cache_write)
            .saturating_add(cache_read);
        output =
            output.saturating_add(usage.total_tokens.unwrap_or(0).saturating_sub(known_tokens));
        let token_total = input
            .saturating_add(output)
            .saturating_add(cache_write)
            .saturating_add(cache_read);
        let direct_cost = usage
            .cost
            .as_ref()
            .and_then(|cost| cost.total)
            .filter(|cost| *cost >= 0.0);
        let cost = match direct_cost {
            Some(cost) => cost,
            None => match msg.model.as_deref().and_then(|model| prices.price(model)) {
                Some(price) => price.cost(input, output, cache_write, 0, cache_read, false),
                None => {
                    if let Some(model) = msg.model.as_deref()
                        && token_total > 0
                    {
                        record_unknown_model(&mut unknown_models, model, ts_secs);
                    }
                    0.0
                }
            },
        };
        if token_total == 0 && cost <= 0.0 {
            continue;
        }
        out.push(CachedEntry {
            ts_secs,
            cost_usd: cost,
            input,
            output,
            cache_write,
            cache_read,
            message_id: None,
            request_id: None,
            dedup_key: None,
            thread_id: None,
            is_sidechain: false,
            has_speed: false,
            model: msg.model.clone(),
            rolled: false,
        });
    }
    SpendParse {
        entries: out,
        origin: state.cwd.clone(),
        cursor: SpendCursor {
            offset: next_offset,
            state: serde_json::to_value(&state).ok(),
        },
        unknown_models,
        replace_entries: false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn prices() -> PriceBook {
        PriceBook::from_litellm_json(
            r#"{"pi-priced-model":{"input_cost_per_token":0.000001,"output_cost_per_token":0.000002,"cache_read_input_token_cost":0.0000001,"cache_creation_input_token_cost":0.00000125}}"#,
        )
    }

    #[test]
    fn parses_cost_from_usage_cost_total() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","model":"gpt-5","usage":{{"input":100,"output":50,"cost":{{"total":0.42}}}}}}}}"#
        )
        .unwrap();

        let entries = parse_pi_spend(&path, None, &prices()).entries;
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.42).abs() < 1e-9);
        assert_eq!(entries[0].input, 100);
        assert_eq!(entries[0].output, 50);
        assert_eq!(
            entries[0].ts_secs,
            crate::agents::spending::iso_to_unix_secs("2026-06-02T10:00:00.000Z").unwrap()
        );
        assert!(entries[0].message_id.is_none());
        assert!(!entries[0].is_sidechain);
    }

    #[test]
    fn carries_session_header_cwd_through_incremental_cursor() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("repo");
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"session","version":3,"id":"sess","timestamp":"2026-06-02T09:00:00.000Z","cwd":"{}"}}"#,
            cwd.display()
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","usage":{{"input":100,"output":50,"cost":{{"total":0.42}}}}}}}}"#
        )
        .unwrap();

        let first = parse_pi_spend(&path, None, &prices());
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.origin.as_deref(), Some(cwd.as_path()));

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{{"role":"assistant","usage":{{"input":200,"output":75,"cost":{{"total":0.84}}}}}}}}"#
        )
        .unwrap();

        let second = parse_pi_spend(&path, Some(&first.cursor), &prices());
        assert_eq!(second.entries.len(), 1);
        assert!((second.entries[0].cost_usd - 0.84).abs() < 1e-9);
        assert_eq!(second.origin.as_deref(), Some(cwd.as_path()));
    }

    #[test]
    fn skips_non_assistant_non_message_and_empty_nonpositive_cost_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // A non-assistant role, a non-`message` type, and zero/negative costs
        // each disqualify a usage line — none reach the entry list.
        for line in [
            r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"user","usage":{"cost":{"total":1.0}}}}"#,
            r#"{"type":"tool_call","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","usage":{"cost":{"total":1.0}}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","usage":{"cost":{"total":0.0}}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{"role":"assistant","usage":{"cost":{"total":-1.0}}}}"#,
        ] {
            writeln!(f, "{line}").unwrap();
        }

        assert!(parse_pi_spend(&path, None, &prices()).entries.is_empty());
    }

    #[test]
    fn parses_numeric_strings_and_ignores_malformed_nested_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in [
            r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","usage":{"input":"100","output":"50","cacheRead":"30","cacheWrite":"20","cost":{"total":"0.42"}}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{"role":"assistant","model":42,"usage":{"input":true,"output":25,"cacheRead":[],"cacheWrite":{},"cost":{"total":0.21}}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T12:00:00.000Z","message":{"role":"assistant","usage":{"input":10,"cost":"unavailable"}}}"#,
        ] {
            writeln!(f, "{line}").unwrap();
        }

        let entries = parse_pi_spend(&path, None, &prices()).entries;
        assert_eq!(entries.len(), 3);
        assert_eq!(
            (
                entries[0].input,
                entries[0].output,
                entries[0].cache_read,
                entries[0].cache_write,
            ),
            (100, 50, 30, 20)
        );
        assert!((entries[0].cost_usd - 0.42).abs() < 1e-9);
        assert_eq!((entries[1].input, entries[1].output), (0, 25));
        assert!((entries[1].cost_usd - 0.21).abs() < 1e-9);
        assert_eq!(entries[2].input, 10);
        assert_eq!(entries[2].cost_usd, 0.0);
    }

    #[test]
    fn keeps_zero_cost_tokens_and_prices_missing_cost_with_total_fallback() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in [
            r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"assistant","model":"pi-priced-model","usage":{"totalTokens":333}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{"role":"assistant","model":"pi-priced-model","usage":{"input":100,"output":50,"cost":{"total":0}}}}"#,
            r#"{"type":"message","timestamp":"2026-06-02T12:00:00.000Z","message":{"role":"assistant","model":"unknown-pi-model","usage":{"input":10}}}"#,
        ] {
            writeln!(f, "{line}").unwrap();
        }

        let parsed = parse_pi_spend(&path, None, &prices());

        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(
            (parsed.entries[0].input, parsed.entries[0].output),
            (0, 333)
        );
        assert!((parsed.entries[0].cost_usd - 333.0 * 0.000002).abs() < 1e-12);
        assert_eq!(parsed.entries[1].cost_usd, 0.0);
        assert_eq!(parsed.entries[1].input, 100);
        assert_eq!(parsed.entries[1].output, 50);
        assert_eq!(parsed.entries[2].input, 10);
        assert_eq!(parsed.entries[2].cost_usd, 0.0);
        assert!(parsed.unknown_models.contains_key("unknown-pi-model"));
    }

    #[test]
    fn total_token_gap_folds_into_output_when_output_is_nonzero() {
        // `totalTokens` exceeds the itemized parts while output is already
        // nonzero: the excess rides output so it is counted, and (absent a direct
        // cost) priced as output, instead of being dropped.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","model":"pi-priced-model","usage":{{"input":100,"output":20,"cacheRead":30,"totalTokens":200}}}}}}"#
        )
        .unwrap();

        let parsed = parse_pi_spend(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        // missing = 200 - (100 + 20 + 30) = 50 → output = 20 + 50 = 70.
        assert_eq!((entry.input, entry.output, entry.cache_read), (100, 70, 30));
        let expected = 100.0 * 0.000001 + 70.0 * 0.000002 + 30.0 * 0.0000001;
        assert!((entry.cost_usd - expected).abs() < 1e-12);
    }
}
