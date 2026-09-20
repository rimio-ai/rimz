//! Structured parser for Claude Code's statusline JSON.
//!
//! Claude `exec`s its configured `statusLine` command on every render and pipes
//! a rich JSON blob to its stdin (see `docs/internals/agents/adapter_claude.md`). This module
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

/// The statusline payload Claude pipes on stdin. Only the fields RimZ projects
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
    current_usage: Option<AgentCurrentUsage>,
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

fn current_usage(field: Option<AgentCurrentUsage>) -> Option<AgentCurrentUsage> {
    field.filter(|usage| !usage.is_zero())
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

/// Classify one Claude API-error turn from its structured fields and label.
/// `StopFailure` and the transcript tail both carry Claude's `error` value, so
/// both paths call this and cannot disagree whichever writes the marker last.
/// A spend-limit label wins because Claude tags spend caps `rate_limit`/429 too;
/// otherwise `rate_limit`/429 pauses on the rate window whatever the text says
/// ("You've reached your Fable limit"), and `overloaded` pauses as transient.
pub(super) fn classify_api_error(
    error: Option<&str>,
    status: Option<u64>,
    label: Option<&str>,
) -> TurnErrorClass {
    let label_class = TurnErrorClass::classify_label(label);
    if label_class == TurnErrorClass::PausedSpendLimit {
        return label_class;
    }
    match (error.map(str::trim), status) {
        (Some("rate_limit"), _) | (_, Some(429)) => TurnErrorClass::PausedRateLimit,
        (Some("overloaded"), _) => TurnErrorClass::PausedOverloaded,
        _ => label_class,
    }
}

enum RestingTurnOutcome {
    Interrupted(Timestamp),
    Died(AgentTurnError),
}

#[derive(Clone, Copy)]
enum TranscriptScope<'a> {
    Root,
    Subagent(&'a str),
}

/// Detect a turn that died on a provider API error from the transcript tail.
/// Claude aborts such a turn by writing an `assistant` entry flagged
/// `isApiErrorMessage: true` (followed by a `system` / `turn_duration` record).
/// Current Claude also fires `StopFailure`; the tail is the backstop for older
/// builds and late-installed hooks, and every statusline push re-derives the
/// marker from it, so both paths share [`classify_api_error`].
///
/// Scanning the bounded tail newest-first, the first conversation-bearing
/// entry — `type` of `assistant`/`user`, not a sidechain, carrying a parseable
/// `timestamp` — decides: flagged means the turn died at that instant;
/// anything else means the newest turn is alive or recovered, so `None`.
/// Non-conversation records (`system`, `file-history-snapshot`, `summary`),
/// sidechain replay, and unparseable lines are passed over, never decisive.
pub(crate) fn detect_turn_error(tail: &str) -> Option<AgentTurnError> {
    match detect_resting_turn_outcome(tail, TranscriptScope::Root) {
        Some(RestingTurnOutcome::Died(error)) => Some(error),
        Some(RestingTurnOutcome::Interrupted(_)) | None => None,
    }
}

/// Detect a turn interrupted without a `Stop` hook from Claude's transcript
/// tail. Esc writes a `user` entry beginning with `[Request interrupted by
/// user` for both ordinary and tool-use interruptions. The entry's timestamp
/// anchors the same self-clear guard the display projection uses for Codex.
pub(crate) fn detect_turn_interrupted(tail: &str) -> Option<Timestamp> {
    match detect_resting_turn_outcome(tail, TranscriptScope::Root) {
        Some(RestingTurnOutcome::Interrupted(at)) => Some(at),
        Some(RestingTurnOutcome::Died(_)) | None => None,
    }
}

/// Prove from the transcript tail that the call `tool_use_id` resolved: the
/// `user` entry carrying its `tool_result`. Claude writes one for a rejection
/// as well as for a completion, so the block's presence is the whole test and
/// no outcome is read from it. Sidechain replay is a child's call, never the
/// root's, and a torn final record never reaches the tail.
pub(super) fn tool_result_recorded(tail: &str, tool_use_id: &str) -> bool {
    tail.lines()
        .rev()
        .filter_map(conversation_entry)
        .any(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("user")
                && entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block.get("type").and_then(Value::as_str) == Some("tool_result")
                                && block.get("tool_use_id").and_then(Value::as_str)
                                    == Some(tool_use_id)
                        })
                    })
        })
}

/// Prove that one child transcript ended at Claude's interruption marker.
/// Child records are sidechains by construction, so identity replaces the
/// root scan's sidechain exclusion and keeps nested-child replay out.
pub(super) fn detect_subagent_interrupted(tail: &str, agent_id: &str) -> bool {
    matches!(
        detect_resting_turn_outcome(tail, TranscriptScope::Subagent(agent_id)),
        Some(RestingTurnOutcome::Interrupted(_))
    )
}

fn detect_resting_turn_outcome(
    tail: &str,
    scope: TranscriptScope<'_>,
) -> Option<RestingTurnOutcome> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A truncated leading line from the tail seek fails to parse; skip it.
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let in_scope = match scope {
            // Subagent replay never decides the parent's turn.
            TranscriptScope::Root => {
                value.get("isSidechain").and_then(Value::as_bool) != Some(true)
            }
            TranscriptScope::Subagent(agent_id) => {
                value.get("agentId").and_then(Value::as_str) == Some(agent_id)
            }
        };
        if !in_scope {
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
                class: classify_api_error(
                    value.get("error").and_then(Value::as_str),
                    value.get("apiErrorStatus").and_then(Value::as_u64),
                    label.as_deref(),
                ),
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
            current_context_tokens: None,
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
mod tests;
