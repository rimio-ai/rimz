//! Structured parser for Claude Code's statusline JSON.
//!
//! Claude `exec`s its configured `statusLine` command on every render and pipes
//! a rich JSON blob to its stdin (see `docs/internals/agents/claude.md`). This module
//! is a tolerant serde model of that blob plus the projection onto the
//! agent-agnostic [`AgentContext`]. Every field is optional and unknown keys
//! are ignored, so a newer Claude that adds or drops a field still parses —
//! enrichment is never correctness.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits, AgentTokenUsage,
    AgentTurnError, RateLimitWindow, TurnErrorClass, WindowSource, clamp_pct,
};
use crate::agents::{
    sanitize_user_prompt,
    transcript::{TranscriptMessage, TranscriptRole},
};

/// The statusline payload Claude pipes on stdin. Only the fields Rimz projects
/// are modelled; `#[serde(default)]` on every level keeps a sparse or
/// evolved payload parseable, and the absence of `deny_unknown_fields` lets new
/// keys ride along untouched.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StatuslinePayload {
    pub session_id: Option<String>,
    /// User-set session name (`--name` / `/rename`); absent until named.
    session_name: Option<String>,
    model: ModelField,
    cost: CostField,
    context_window: ContextWindowField,
    exceeds_200k_tokens: Option<bool>,
    effort: EffortField,
    thinking: ThinkingField,
    rate_limits: RateLimitsField,
    vim: VimField,
    version: Option<String>,
    output_style: OutputStyleField,
    pr: PrField,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelField {
    id: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CostField {
    total_cost_usd: Option<f64>,
    total_duration_ms: Option<u64>,
    total_api_duration_ms: Option<u64>,
    total_lines_added: Option<u64>,
    total_lines_removed: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindowField {
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    remaining_percentage: Option<f64>,
    /// Older Claude reports null before the first API call and right after
    /// `/compact`; newer Claude reports the same state as explicit zeros.
    /// Both shapes project to `None`.
    current_usage: Option<CurrentUsageField>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CurrentUsageField {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EffortField {
    level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThinkingField {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RateLimitsField {
    five_hour: Option<RateWindowField>,
    seven_day: Option<RateWindowField>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RateWindowField {
    used_percentage: Option<f64>,
    /// Unix epoch seconds in Claude's schema.
    resets_at: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct VimField {
    mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OutputStyleField {
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PrField {
    number: Option<u64>,
    url: Option<String>,
    review_state: Option<String>,
}

fn non_empty<T: Default + PartialEq>(value: T) -> Option<T> {
    (value != T::default()).then_some(value)
}

fn parse_rate_window(
    field: Option<RateWindowField>,
    duration_mins: u32,
) -> Option<RateLimitWindow> {
    let field = field?;
    let resets_at = field.resets_at.and_then(|s| Timestamp::from_second(s).ok());
    super::account::budget_window(
        field.used_percentage,
        resets_at,
        duration_mins,
        WindowSource::BestEffort,
    )
}

fn current_usage(field: Option<CurrentUsageField>) -> Option<AgentCurrentUsage> {
    let field = field?;
    let usage = AgentCurrentUsage {
        input_tokens: field.input_tokens,
        output_tokens: field.output_tokens,
        cache_creation_input_tokens: field.cache_creation_input_tokens,
        cache_read_input_tokens: field.cache_read_input_tokens,
    };
    (!usage.is_zero()).then_some(usage)
}

/// Cap on the surfaced error text. The upstream message is one short line
/// ("API Error: Overloaded"); the cap only guards a pathological entry.
pub(crate) const TURN_ERROR_LABEL_MAX: usize = 80;

pub(crate) fn cap_turn_error_label(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(TURN_ERROR_LABEL_MAX).collect())
}

enum RestingTurnOutcome {
    Interrupted(Timestamp),
    Died(AgentTurnError),
}

/// Detect a turn that died on a provider API error with no `Stop` hook to
/// record it. Claude aborts such a turn by writing an `assistant` transcript
/// entry flagged `isApiErrorMessage: true` (followed by a `system` /
/// `turn_duration` record) and firing no hook, so the transcript tail is the
/// only machine-readable death certificate.
///
/// Scanning the bounded tail newest-first, the first conversation-bearing
/// entry — `type` of `assistant`/`user`, not a sidechain, carrying a parseable
/// `timestamp` — decides: flagged means the turn died at that instant;
/// anything else means the newest turn is alive or recovered, so `None`.
/// Non-conversation records (`system`, `file-history-snapshot`, `summary`),
/// sidechain replay, and unparseable lines are passed over, never decisive.
pub(crate) fn detect_turn_error(tail: &str) -> Option<AgentTurnError> {
    match detect_resting_turn_outcome(tail) {
        Some(RestingTurnOutcome::Died(error)) => Some(error),
        Some(RestingTurnOutcome::Interrupted(_)) | None => None,
    }
}

/// Detect a turn interrupted without a `Stop` hook from Claude's transcript
/// tail. Esc writes a `user` entry beginning with `[Request interrupted by
/// user` for both ordinary and tool-use interruptions. The entry's timestamp
/// anchors the same self-clear guard the display projection uses for Codex.
pub(crate) fn detect_turn_interrupted(tail: &str) -> Option<Timestamp> {
    match detect_resting_turn_outcome(tail) {
        Some(RestingTurnOutcome::Interrupted(at)) => Some(at),
        Some(RestingTurnOutcome::Died(_)) | None => None,
    }
}

fn detect_resting_turn_outcome(tail: &str) -> Option<RestingTurnOutcome> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A truncated leading line from the tail seek fails to parse; skip it.
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Subagent replay never decides the parent's turn.
        if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let entry_type = value.get("type").and_then(Value::as_str);
        if !matches!(entry_type, Some("assistant" | "user")) {
            continue;
        }
        // A conversation entry with no clock cannot anchor the self-clear
        // guard the projection runs against `last_activity`; keep scanning.
        let Some(at) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|ts| ts.parse::<Timestamp>().ok())
        else {
            continue;
        };
        // The first conversation-bearing, timestamped entry decides.
        if entry_type == Some("assistant")
            && value.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        {
            let label = turn_error_label(&value);
            return Some(RestingTurnOutcome::Died(AgentTurnError {
                class: TurnErrorClass::classify_label(label.as_deref()),
                at,
                label,
            }));
        }
        if entry_type == Some("user")
            && conversation_text(&value)
                .is_some_and(|text| text.starts_with("[Request interrupted by user"))
        {
            return Some(RestingTurnOutcome::Interrupted(at));
        }
        return None;
    }
    None
}

/// Extract Claude's latest main-thread assistant message from a transcript
/// tail. Sidechain entries are child-agent replay and ignored. A genuine user
/// prompt bounds the walk; tool results and meta entries are mid-turn plumbing
/// and skipped. A provider API error marker is decisive but not product output,
/// so it returns `None` instead of walking back into an earlier turn.
pub(crate) fn last_assistant_message(tail: &str) -> Option<String> {
    for line in tail.lines().rev() {
        let Some(value) = conversation_entry(line) else {
            continue;
        };
        let entry_type = value.get("type").and_then(Value::as_str);
        if !matches!(entry_type, Some("assistant" | "user")) {
            continue;
        }
        if entry_type == Some("user") {
            if value.get("isMeta").and_then(Value::as_bool) == Some(true)
                || tool_result_entry(&value)
            {
                continue;
            }
            return None;
        }
        if value.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        if let Some(text) = conversation_text(&value) {
            return Some(text);
        }
    }
    None
}

pub(crate) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let value = conversation_entry(line)?;
            let role = match value.get("type").and_then(Value::as_str) {
                Some("user") => {
                    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        return None;
                    }
                    TranscriptRole::User
                }
                Some("assistant")
                    if value.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true) =>
                {
                    TranscriptRole::Assistant
                }
                _ => return None,
            };
            let text = match role {
                TranscriptRole::User => sanitize_user_prompt(conversation_text(&value).as_deref())?,
                TranscriptRole::Assistant => conversation_text(&value)?,
            };
            Some(TranscriptMessage {
                role,
                at: timestamp(&value),
                text,
            })
        })
        .collect()
}

fn conversation_entry(line: &str) -> Option<Value> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return None;
    };
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    Some(value)
}

/// The error entry's text ("API Error: Overloaded"): the first text block of
/// `message.content` (or a flat string), trimmed and capped. `None` when the
/// shape is unfamiliar — the marker still escalates, just unlabeled.
fn turn_error_label(entry: &Value) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    let text = match content {
        Value::String(text) => text.as_str(),
        Value::Array(blocks) => blocks
            .iter()
            .find_map(|block| block.get("text").and_then(Value::as_str))?,
        _ => return None,
    };
    cap_turn_error_label(text)
}

fn conversation_text(entry: &Value) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    content_text(content)
}

/// A `user`-typed transcript entry that carries a tool_result block is the
/// harness returning tool output mid-turn, not the human speaking.
fn tool_result_entry(entry: &Value) -> bool {
    entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

fn timestamp(entry: &Value) -> Option<Timestamp> {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse().ok())
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => non_empty_text(text),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn non_empty_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

impl StatuslinePayload {
    /// Project the parsed payload onto the agent-agnostic record. `observed_at`
    /// is stamped by the caller so the parser stays pure and deterministic in
    /// tests. Empty sub-objects collapse to `None` rather than serializing as
    /// `{}`.
    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let cost = non_empty(AgentCost {
            total_cost_usd: self.cost.total_cost_usd,
            total_duration_ms: self.cost.total_duration_ms,
            total_api_duration_ms: self.cost.total_api_duration_ms,
            total_lines_added: self.cost.total_lines_added,
            total_lines_removed: self.cost.total_lines_removed,
            ..AgentCost::default()
        });
        let tokens = non_empty(AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage: current_usage(self.context_window.current_usage),
            session_usage: None,
        });
        let windows: Vec<RateLimitWindow> = [
            parse_rate_window(self.rate_limits.five_hour, super::account::FIVE_HOUR_MINS),
            parse_rate_window(self.rate_limits.seven_day, super::account::SEVEN_DAY_MINS),
        ]
        .into_iter()
        .flatten()
        .collect();
        let rate_limits =
            (!windows.is_empty()).then(|| AgentRateLimits { windows }.stamped_at(observed_at));
        let pr = non_empty(AgentPullRequest {
            number: self.pr.number,
            url: self.pr.url,
            review_state: self.pr.review_state,
        });
        AgentContext {
            session_name: self.session_name,
            model_id: self.model.id,
            model_display_name: self.model.display_name,
            effort: self.effort.level,
            thinking_enabled: self.thinking.enabled,
            output_style: self.output_style.name,
            vim_mode: self.vim.mode,
            agent_version: self.version,
            exceeds_200k_tokens: self.exceeds_200k_tokens,
            cost,
            tokens,
            rate_limits,
            pr,
            ..AgentContext::new(source, observed_at)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::account;
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> AgentContext {
        let payload: StatuslinePayload = serde_json::from_value(value).unwrap();
        payload.into_context("claude", Timestamp::from_second(1_700_000_000).unwrap())
    }

    /// The window stamped with `mins` minutes — Claude's two named wire windows
    /// map to fixed durations.
    fn window_by_mins(rate: &AgentRateLimits, mins: u32) -> &RateLimitWindow {
        rate.windows
            .iter()
            .find(|window| window.duration_mins == Some(mins))
            .expect("window present for duration")
    }

    #[test]
    fn full_payload_projects_every_field() {
        let ctx = parse(json!({
            "session_id": "abc123",
            "session_name": "store-refactor",
            "model": { "id": "claude-opus-4-8", "display_name": "Opus" },
            "cost": {
                "total_cost_usd": 0.01234,
                "total_duration_ms": 45000,
                "total_api_duration_ms": 2300,
                "total_lines_added": 156,
                "total_lines_removed": 23
            },
            "context_window": {
                "total_input_tokens": 15500,
                "total_output_tokens": 1200,
                "context_window_size": 200000,
                "used_percentage": 8,
                "remaining_percentage": 92,
                "current_usage": {
                    "input_tokens": 8500,
                    "output_tokens": 1200,
                    "cache_creation_input_tokens": 5000,
                    "cache_read_input_tokens": 2000
                }
            },
            "exceeds_200k_tokens": false,
            "effort": { "level": "high" },
            "thinking": { "enabled": true },
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600i64 },
                "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600i64 }
            },
            "vim": { "mode": "NORMAL" },
            "version": "2.1.90",
            "output_style": { "name": "default" },
            "pr": { "number": 1234, "url": "https://example/pr/1234", "review_state": "pending" }
        }));

        assert_eq!(ctx.source, "claude");
        assert_eq!(ctx.session_name.as_deref(), Some("store-refactor"));
        assert_eq!(ctx.model_id.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(ctx.model_display_name.as_deref(), Some("Opus"));
        assert_eq!(ctx.effort.as_deref(), Some("high"));
        assert_eq!(ctx.thinking_enabled, Some(true));
        assert_eq!(ctx.output_style.as_deref(), Some("default"));
        assert_eq!(ctx.vim_mode.as_deref(), Some("NORMAL"));
        assert_eq!(ctx.agent_version.as_deref(), Some("2.1.90"));
        assert_eq!(ctx.exceeds_200k_tokens, Some(false));

        let cost = ctx.cost.unwrap();
        assert_eq!(cost.total_cost_usd, Some(0.01234));
        assert_eq!(cost.total_lines_added, Some(156));

        // `total_input_tokens` / `total_output_tokens` ride the wire above but
        // are not captured — `current_usage` carries the same window split.
        let tokens = ctx.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(200000));
        assert_eq!(tokens.used_percentage, Some(8));
        assert_eq!(tokens.remaining_percentage, Some(92));
        let usage = tokens.current_usage.unwrap();
        assert_eq!(usage.input_tokens, Some(8500));
        assert_eq!(usage.output_tokens, Some(1200));
        assert_eq!(usage.cache_creation_input_tokens, Some(5000));
        assert_eq!(usage.cache_read_input_tokens, Some(2000));

        let rate = ctx.rate_limits.unwrap();
        let five = window_by_mins(&rate, account::FIVE_HOUR_MINS);
        // 23.5 rounds to 24.
        assert_eq!(five.used_percentage, Some(24));
        assert_eq!(five.resets_at, Timestamp::from_second(1738425600).ok());
        assert!(
            five.source.is_best_effort(),
            "a statusline reading is best-effort — a drop is confirmed before it lowers the bar"
        );
        assert_eq!(
            five.observed_at,
            Some(ctx.observed_at),
            "into_context stamps the capture instant onto each window"
        );
        assert_eq!(
            window_by_mins(&rate, account::SEVEN_DAY_MINS).used_percentage,
            Some(41)
        );

        let pr = ctx.pr.unwrap();
        assert_eq!(pr.number, Some(1234));
        assert_eq!(pr.review_state.as_deref(), Some("pending"));
    }

    #[test]
    fn payload_tolerates_sparse_null_unknown_and_clamped_fields() {
        let ctx = parse(json!({ "session_id": "s", "model": {} }));
        assert_eq!(ctx.source, "claude");
        assert!(ctx.model_id.is_none());
        assert!(ctx.cost.is_none());
        assert!(ctx.tokens.is_none());
        assert!(ctx.rate_limits.is_none());
        assert!(ctx.pr.is_none());

        // `current_usage` is null before the first API call in older Claude;
        // the rest of the context window still projects.
        let ctx = parse(json!({
            "context_window": { "used_percentage": 12, "current_usage": null }
        }));
        let tokens = ctx.tokens.unwrap();
        assert_eq!(tokens.used_percentage, Some(12));
        assert!(tokens.current_usage.is_none());

        // An all-null usage object carries nothing, so it collapses rather than
        // serializing as `{}`.
        let ctx = parse(json!({
            "context_window": { "used_percentage": 5, "current_usage": {} }
        }));
        assert!(ctx.tokens.unwrap().current_usage.is_none());

        // Newer Claude reports the same state as explicit zeros.
        let ctx = parse(json!({
            "context_window": { "used_percentage": 5, "current_usage": { "input_tokens": 0 } }
        }));
        assert!(ctx.tokens.unwrap().current_usage.is_none());

        // A newer Claude that adds keys must still parse.
        let ctx = parse(json!({ "model": { "id": "m" }, "brand_new_field": { "x": 1 } }));
        assert_eq!(ctx.model_id.as_deref(), Some("m"));

        let ctx = parse(json!({ "context_window": { "used_percentage": 250 } }));
        assert_eq!(ctx.tokens.unwrap().used_percentage, Some(100));

        // i64::MIN is out of Timestamp's range; the reset drops, the pct stays.
        let ctx = parse(json!({
            "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": i64::MIN } }
        }));
        let rate = ctx.rate_limits.unwrap();
        let five = window_by_mins(&rate, account::FIVE_HOUR_MINS);
        assert_eq!(five.used_percentage, Some(10));
        assert!(five.resets_at.is_none());

        let ctx = parse(json!({
            "rate_limits": {
                "five_hour": { "used_percentage": 99.5, "resets_at": 1738425600i64 },
                "seven_day": { "used_percentage": 100.0, "resets_at": 1738857600i64 }
            }
        }));
        let rate = ctx.rate_limits.unwrap();
        assert_eq!(
            window_by_mins(&rate, account::FIVE_HOUR_MINS).used_percentage,
            Some(99),
            "99.5% used still leaves visible remaining budget"
        );
        assert_eq!(
            window_by_mins(&rate, account::SEVEN_DAY_MINS).used_percentage,
            Some(100),
            "exactly 100% used remains exhausted"
        );
    }

    /// The verbatim shape an API-error abort writes (observed live 2026-06-04):
    /// the flagged assistant entry, then a `system`/`turn_duration` record 4ms
    /// later, then nothing — and no `Stop` hook.
    const API_ERROR_ENTRY: &str = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{"role":"assistant","content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
    const TURN_DURATION_ENTRY: &str =
        r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-06-04T02:56:32.923Z"}"#;
    const NORMAL_ASSISTANT_ENTRY: &str = r#"{"type":"assistant","timestamp":"2026-06-04T03:00:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#;
    const INTERRUPTED_ENTRY: &str = r#"{"type":"user","timestamp":"2026-06-04T03:01:00.000Z","message":{"role":"user","content":"[Request interrupted by user]"}}"#;

    #[test]
    fn verified_incident_shape_marks_turn_error() {
        // The newer `turn_duration` record is a non-conversation entry: passed
        // over, never decisive, so the flagged assistant entry decides.
        let tail = format!("{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n");
        let error = detect_turn_error(&tail).expect("the dead turn is detected");
        assert_eq!(
            error.at,
            "2026-06-04T02:56:32.919Z".parse::<Timestamp>().unwrap(),
            "the marker carries the error entry's own wall-clock instant"
        );
        assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
        assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));
    }

    #[test]
    fn interruption_sentinels_mark_the_turn_at_rest() {
        assert_eq!(
            detect_turn_interrupted(INTERRUPTED_ENTRY),
            Some("2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap())
        );
        // Verbatim content-block shape observed in Claude's transcript JSONL;
        // this is a text block, distinct from the Messages API wire shape.
        let tool_use = r#"{"type":"user","timestamp":"2026-06-04T03:02:00.000Z","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        assert_eq!(
            detect_turn_interrupted(tool_use),
            Some("2026-06-04T03:02:00Z".parse::<Timestamp>().unwrap())
        );
    }

    #[test]
    fn resting_turn_scan_lets_the_newest_conversation_entry_decide() {
        assert!(detect_turn_interrupted(API_ERROR_ENTRY).is_none());
        assert!(detect_turn_error(API_ERROR_ENTRY).is_some());

        assert!(detect_turn_interrupted(NORMAL_ASSISTANT_ENTRY).is_none());
        assert!(detect_turn_error(INTERRUPTED_ENTRY).is_none());

        let ordinary_user = r#"{"type":"user","timestamp":"2026-06-04T03:03:00.000Z","message":{"content":"keep going"}}"#;
        let tail = format!("{INTERRUPTED_ENTRY}\n{ordinary_user}\n");
        assert!(detect_turn_interrupted(&tail).is_none());
    }

    #[test]
    fn resting_turn_scan_skips_sidechain_and_nonconversation_records() {
        let sidechain = r#"{"type":"user","isSidechain":true,"timestamp":"2026-06-04T03:02:00.000Z","message":{"content":"[Request interrupted by user]"}}"#;
        let tail = format!(
            "{INTERRUPTED_ENTRY}\n{{\"type\":\"system\",\"timestamp\":\"2026-06-04T03:01:01.000Z\"}}\n{sidechain}\n"
        );
        assert!(detect_turn_interrupted(&tail).is_some());

        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\nnot-json\n");
        assert!(detect_turn_interrupted(&tail).is_none());
    }

    #[test]
    fn turn_error_label_classifies_paused_and_failed_errors() {
        let entry = |text: &str| {
            format!(
                r#"{{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
            )
        };
        let temporary_500 = concat!(
            "API Error: 500 Internal server error. ",
            "This is a server-side issue, usually temporary — try again in a moment."
        );

        assert_eq!(
            detect_turn_error(&entry("You've hit your usage limit"))
                .unwrap()
                .class,
            TurnErrorClass::PausedRateLimit
        );
        assert_eq!(
            detect_turn_error(&entry("You've hit your monthly spend limit."))
                .unwrap()
                .class,
            TurnErrorClass::PausedSpendLimit
        );
        assert_eq!(
            detect_turn_error(&entry(
                "You've hit your session limit · resets 10:50am (UTC)"
            ))
            .unwrap()
            .class,
            TurnErrorClass::PausedRateLimit
        );
        assert_eq!(
            detect_turn_error(&entry("API Error: rate limit exceeded"))
                .unwrap()
                .class,
            TurnErrorClass::PausedRateLimit
        );
        assert_eq!(
            detect_turn_error(&entry("API Error: Server Error"))
                .unwrap()
                .class,
            TurnErrorClass::PausedOverloaded
        );
        assert_eq!(
            detect_turn_error(&entry(
                "API Error: Response stalled mid-stream. The response above may be incomplete."
            ))
            .unwrap()
            .class,
            TurnErrorClass::PausedOverloaded
        );
        assert_eq!(
            detect_turn_error(&entry(temporary_500)).unwrap().class,
            TurnErrorClass::PausedOverloaded
        );
        assert_eq!(
            detect_turn_error(&entry("API Error: Bad Request"))
                .unwrap()
                .class,
            TurnErrorClass::Failed
        );
    }

    #[test]
    fn turn_error_scan_skips_recovered_sidechain_and_nonconversation_records() {
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n");
        assert!(detect_turn_error(&tail).is_none());

        // A normal conversation entry newer than the error means the session
        // moved on (a resume, a rewind, a fresh prompt): alive, not dead.
        let tail = format!("{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n{NORMAL_ASSISTANT_ENTRY}\n");
        assert!(detect_turn_error(&tail).is_none());

        // Rewind/fork artifacts (`file-history-snapshot`, no timestamp) and
        // `summary` records ride the tail; the scan passes over them to the
        // newest conversation entry.
        let tail = format!(
            "{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n{{\"type\":\"file-history-snapshot\"}}\n{{\"type\":\"summary\",\"summary\":\"t\"}}\n"
        );
        assert!(detect_turn_error(&tail).is_some());

        // A subagent replay's API error is the child's problem; the parent's
        // newest own entry (older, normal) decides.
        let sidechain = r#"{"type":"assistant","isSidechain":true,"isApiErrorMessage":true,"timestamp":"2026-06-04T03:01:00.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\n");
        assert!(detect_turn_error(&tail).is_none());
    }

    #[test]
    fn assistant_message_readers_keep_main_thread_signal() {
        let earlier = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"old"}]}}"#;
        let latest = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}"#;
        let tail = format!("{earlier}\n{latest}\n");

        assert_eq!(
            last_assistant_message(&tail).as_deref(),
            Some("hello\nworld")
        );

        let sidechain = r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"child answer"}]}}"#;
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\n");
        assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

        let tool_only = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#;
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{tool_only}\n");
        assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

        let tool_call = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"pwd"}}]}}"#;
        let tool_result =
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{tool_call}\n{tool_result}\n{tool_only}\n");
        assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"generated context"}}"#;
        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{meta}\n{tool_only}\n");
        assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

        let prior_turn = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"previous turn"}]}}"#;
        let user =
            r#"{"type":"user","message":{"content":[{"type":"text","text":"current prompt"}]}}"#;
        let tail = format!("{prior_turn}\n{user}\n{tool_only}\n");
        assert!(last_assistant_message(&tail).is_none());

        let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{API_ERROR_ENTRY}\n");
        assert!(last_assistant_message(&tail).is_none());

        let tail = format!("{prior_turn}\n{user}\n{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n");

        assert!(last_assistant_message(&tail).is_none());

        let messages = parse_messages(include_str!("tests/fixtures/stream-transcript.jsonl"))
            .into_iter()
            .filter(|message| message.role == TranscriptRole::Assistant)
            .map(|message| message.text)
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["first update", "second\nline"]);
    }

    #[test]
    fn parse_messages_reads_user_assistant_and_timestamps() {
        let lines = concat!(
            r#"{"type":"user","timestamp":"2026-06-04T03:00:00.000Z","message":{"content":[{"type":"text","text":"fix auth"}]}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"timestamp":"2026-06-04T03:00:00.500Z","message":{"content":"Caveat: generated context"}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-04T03:00:00.750Z","message":{"content":"<local-command-stdout>pwd</local-command-stdout>"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-04T03:00:01.000Z","message":{"content":[{"type":"text","text":"done"}]}}"#,
            "\n",
            r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T03:00:02.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-06-04T03:00:03.000Z","message":{"content":[{"type":"text","text":"child"}]}}"#,
            "\n",
        );
        let messages = parse_messages(lines);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[0].text, "fix auth");
        assert_eq!(
            messages[0].at,
            Some("2026-06-04T03:00:00Z".parse::<Timestamp>().unwrap())
        );
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        assert_eq!(messages[1].text, "done");
    }

    #[test]
    fn turn_error_scan_tolerates_truncated_unclocked_and_empty_tail() {
        // The 64KB tail seek can split the first line mid-JSON; it fails to
        // parse and is passed over.
        let tail = format!("age\":{{\"truncated\":true}}}}\n{API_ERROR_ENTRY}\n");
        assert!(detect_turn_error(&tail).is_some());

        // No clock, no self-clear guard: the scan passes over it rather than
        // emitting a marker the projection could never expire.
        let unclocked = r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
        assert!(detect_turn_error(&format!("{unclocked}\n")).is_none());

        assert!(detect_turn_error("").is_none());
        assert!(detect_turn_error("\n\n").is_none());
    }

    #[test]
    fn turn_error_labels_are_capped_and_accept_flat_content() {
        let long = "x".repeat(500);
        let entry = format!(
            r#"{{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let error = detect_turn_error(&entry).expect("detected");
        assert_eq!(error.label.unwrap().chars().count(), TURN_ERROR_LABEL_MAX);

        // Tolerate a flat-string `message.content` alongside the block array.
        let entry = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{"content":"API Error: Overloaded"}}"#;
        let error = detect_turn_error(entry).expect("detected");
        assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));
    }
}
