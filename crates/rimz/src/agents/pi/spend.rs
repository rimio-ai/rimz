//! Pi agent JSONL transcript parser.
//!
//! [`spending`](crate::agents::spending) consumes this parser through the
//! adapter's `parse_spend`, fed every Pi session fleet-wide by the producer.
//! Pi logs dollars directly (`usage.cost.total`), so each entry carries a
//! cost as parsed — no pricing table needed.
//!
//! Pi session files live under `~/.pi/agent/sessions/--<cwd-with-dashes>--/`
//! as one `<ISO-timestamp>_<uuid>.jsonl` per session (e.g.
//! `2026-06-04T06-45-56-308Z_019e9161-….jsonl`, the uuid being the session
//! id). Upstream overrides: `--session-dir` / `PI_CODING_AGENT_SESSION_DIR`;
//! the `PI_AGENT_DIR` env honored here is Rimz's own comma-separated test
//! override, not a pi variable. Upstream shapes are mirrored in
//! `docs/internals/adapter/pi-reference.md`. JSONL shape (one entry per
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

use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, origin_path};

use crate::agents::transcript_fs::{collect_jsonl, home_dir, read_spend_lines};

// ── Typed structs ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PiEntry {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<PiMessage>,
}

#[derive(Deserialize)]
struct PiSessionHeader {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    cwd: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct PiSpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: Option<String>,
    model: Option<String>,
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiUsage {
    input: Option<u64>,
    output: Option<u64>,
    #[serde(rename = "cacheWrite")]
    cache_write: Option<u64>,
    #[serde(rename = "cacheRead")]
    cache_read: Option<u64>,
    cost: Option<PiCost>,
}

#[derive(Deserialize)]
struct PiCost {
    total: Option<f64>,
}

// ── Path discovery ────────────────────────────────────────────────────────────

/// Collect all Pi session `*.jsonl` files from `~/.pi/agent/sessions/`.
///
/// Respects `PI_AGENT_DIR` (comma-separated Rimz test override) first, then
/// Pi's own `PI_CODING_AGENT_SESSION_DIR`, then the session directory below
/// `PI_CODING_AGENT_DIR` / `~/.pi/agent`. Pi sessions are not project-scoped,
/// so all sessions are included regardless of worktree paths.
pub fn pi_session_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(env_val) = std::env::var("PI_AGENT_DIR") {
        for raw in env_val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(raw);
            if p.is_dir() {
                roots.push(p);
            }
        }
    } else if let Ok(raw) = std::env::var("PI_CODING_AGENT_SESSION_DIR") {
        let candidate = PathBuf::from(raw);
        if candidate.is_dir() {
            roots.push(candidate);
        }
    } else {
        let candidate = pi_config_dir().join("sessions");
        if candidate.is_dir() {
            roots.push(candidate);
        }
    }

    let mut files = Vec::new();
    for dir in &roots {
        collect_jsonl(dir, &mut files);
    }
    files.sort();
    files.dedup();
    files
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
/// `message.role == "assistant"`.  Cost is read from `message.usage.cost.total`.
/// Lines without both `"usage"` and `"message"` keywords are skipped before
/// deserialization.
pub fn parse_pi_spend(path: &Path, resume: Option<&SpendCursor>) -> SpendParse {
    let from_offset = resume.map_or(0, |cursor| cursor.offset);
    let mut state: PiSpendState = resume
        .and_then(|cursor| cursor.state.clone())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let Some((content, next_offset)) = read_spend_lines(path, from_offset) else {
        return SpendParse {
            entries: Vec::new(),
            cursor: SpendCursor {
                offset: from_offset,
                state: serde_json::to_value(&state).ok(),
            },
            unknown_models: BTreeMap::new(),
        };
    };
    let content = String::from_utf8_lossy(&content);
    let mut out = Vec::new();

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
        let cost = msg
            .usage
            .as_ref()
            .and_then(|u| u.cost.as_ref())
            .and_then(|c| c.total)
            .unwrap_or(0.0);
        if cost <= 0.0 {
            continue;
        }
        let usage = msg.usage.as_ref();
        let Some(ts) = entry.timestamp.as_deref() else {
            continue;
        };
        let Some(ts_secs) = crate::agents::spending::iso_to_unix_secs(ts) else {
            continue;
        };
        out.push(CachedEntry {
            ts_secs,
            cost_usd: cost,
            input: usage.and_then(|u| u.input).unwrap_or(0),
            output: usage.and_then(|u| u.output).unwrap_or(0),
            cache_write: usage.and_then(|u| u.cache_write).unwrap_or(0),
            cache_read: usage.and_then(|u| u.cache_read).unwrap_or(0),
            message_id: None,
            request_id: None,
            thread_id: None,
            is_sidechain: false,
            model: msg.model.clone(),
            origin_path: state.cwd.clone(),
        });
    }
    SpendParse {
        entries: out,
        cursor: SpendCursor {
            offset: next_offset,
            state: serde_json::to_value(&state).ok(),
        },
        unknown_models: BTreeMap::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

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

        let entries = parse_pi_spend(&path, None).entries;
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

        let first = parse_pi_spend(&path, None);
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].origin_path.as_deref(), Some(cwd.as_path()));

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{{"role":"assistant","usage":{{"input":200,"output":75,"cost":{{"total":0.84}}}}}}}}"#
        )
        .unwrap();

        let second = parse_pi_spend(&path, Some(&first.cursor));
        assert_eq!(second.entries.len(), 1);
        assert!((second.entries[0].cost_usd - 0.84).abs() < 1e-9);
        assert_eq!(
            second.entries[0].origin_path.as_deref(),
            Some(cwd.as_path())
        );
    }

    #[test]
    fn skips_non_assistant_non_message_and_nonpositive_cost_lines() {
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

        assert!(parse_pi_spend(&path, None).entries.is_empty());
    }
}
