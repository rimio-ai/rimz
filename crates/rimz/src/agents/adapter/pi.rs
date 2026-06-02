//! Pi agent JSONL transcript parser.
//!
//! Staged ahead of its consumer — see [`super`] for the layer-wide rationale.
//! The parser yields a `costUSD` directly and is already dispatched from
//! `parse_jsonl` in [`spending`](super::super::spending); it only awaits the
//! live path feeding it Pi session files (pending with the upcoming
//! transcript-history analysis).
//!
//! Pi session files live at `~/.pi/agent/sessions/` (or `PI_AGENT_DIR` env).
//! JSONL shape (one entry per assistant turn):
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

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::spending::CachedEntry;

use super::{collect_jsonl, home_dir};

// ── Typed structs ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PiEntry {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<PiMessage>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: Option<String>,
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiUsage {
    cost: Option<PiCost>,
}

#[derive(Deserialize)]
struct PiCost {
    total: Option<f64>,
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Extract a Pi session ID from a JSONL file path.
///
/// Pi filenames follow the pattern `agent_<session-id>.jsonl`; the `agent_`
/// prefix is stripped to return the bare session ID.  Files without the prefix
/// use the full stem as the session ID.
pub fn pi_session_id_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    stem.split_once('_')
        .map_or(stem, |(_, session)| session)
        .to_string()
}

/// Extract the Pi project name from a session file path.
///
/// Pi sessions are organized as `sessions/<project>/agent_<id>.jsonl`; the
/// component immediately after `sessions/` is the project name.
pub fn pi_project_from_path(path: &Path) -> String {
    let mut after_sessions = false;
    for component in path.components() {
        let seg = component.as_os_str().to_string_lossy();
        if after_sessions {
            return seg.into_owned();
        }
        if seg == "sessions" {
            after_sessions = true;
        }
    }
    "unknown".to_string()
}

// ── Path discovery ────────────────────────────────────────────────────────────

/// Collect all Pi session `*.jsonl` files from `~/.pi/agent/sessions/`.
///
/// Respects `PI_AGENT_DIR` (comma-separated) when set.  Pi sessions are not
/// project-scoped, so all sessions are included regardless of worktree paths.
pub fn pi_session_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(env_val) = std::env::var("PI_AGENT_DIR") {
        for raw in env_val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(raw);
            if p.is_dir() {
                roots.push(p);
            }
        }
    } else {
        let candidate = home_dir().join(".pi/agent/sessions");
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

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a Pi JSONL file into `CachedEntry` values.
///
/// Accepts only lines where `"type":"message"` (or `type` is absent) and
/// `message.role == "assistant"`.  Cost is read from `message.usage.cost.total`.
/// Lines without both `"usage"` and `"message"` keywords are skipped before
/// deserialization.
pub fn parse_pi_jsonl(path: &Path) -> Vec<CachedEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for line in content.lines() {
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
        let Some(ts) = entry.timestamp.as_deref() else {
            continue;
        };
        let date = match ts.get(..10) {
            Some(d) if d.as_bytes().get(4) == Some(&b'-') => d.to_string(),
            _ => continue,
        };
        out.push(CachedEntry {
            date,
            cost_usd: cost,
            message_id: None,
            request_id: None,
            is_sidechain: false,
        });
    }
    out
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

        let entries = parse_pi_jsonl(&path);
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.42).abs() < 1e-9);
        assert_eq!(entries[0].date, "2026-06-02");
        assert!(entries[0].message_id.is_none());
        assert!(!entries[0].is_sidechain);
    }

    #[test]
    fn skips_user_role_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"user","usage":{{"cost":{{"total":1.0}}}}}}}}"#
        )
        .unwrap();

        assert!(parse_pi_jsonl(&path).is_empty());
    }

    #[test]
    fn skips_non_message_type() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"tool_call","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","usage":{{"cost":{{"total":1.0}}}}}}}}"#
        )
        .unwrap();

        assert!(parse_pi_jsonl(&path).is_empty());
    }

    #[test]
    fn pi_session_id_strips_agent_prefix() {
        assert_eq!(
            pi_session_id_from_path(Path::new(
                "/home/me/.pi/agent/sessions/project-a/agent_abc123.jsonl"
            )),
            "abc123"
        );
    }

    #[test]
    fn pi_session_id_no_prefix() {
        assert_eq!(
            pi_session_id_from_path(Path::new("/sessions/session-xyz.jsonl")),
            "session-xyz"
        );
    }

    #[test]
    fn pi_project_from_path_extracts_component() {
        assert_eq!(
            pi_project_from_path(Path::new(
                "/home/me/.pi/agent/sessions/my-project/agent_abc.jsonl"
            )),
            "my-project"
        );
    }

    #[test]
    fn skips_zero_and_negative_costs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","usage":{{"cost":{{"total":0.0}}}}}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"message","timestamp":"2026-06-02T11:00:00.000Z","message":{{"role":"assistant","usage":{{"cost":{{"total":-1.0}}}}}}}}"#).unwrap();

        assert!(parse_pi_jsonl(&path).is_empty());
    }
}
