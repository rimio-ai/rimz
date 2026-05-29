//! Structured parser for Claude Code's statusline JSON.
//!
//! Claude `exec`s its configured `statusLine` command on every render and pipes
//! a rich JSON blob to its stdin (see `docs/internals/agent.md`). This module
//! is a tolerant serde model of that blob plus the projection onto the
//! agent-agnostic [`AgentContext`]. Every field is optional and unknown keys
//! are ignored, so a newer Claude that adds or drops a field still parses —
//! enrichment is never correctness.

use jiff::Timestamp;
use serde::Deserialize;

use super::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits, AgentTokenUsage,
    RateLimitWindow,
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
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    remaining_percentage: Option<f64>,
    /// Null before the first API call and right after `/compact`, so it stays
    /// `Option` even though the surrounding object is present.
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

/// Round and clamp a reported percentage to the `0..=100` gauge range.
fn clamp_pct(value: Option<f64>) -> Option<u8> {
    value.map(|v| v.round().clamp(0.0, 100.0) as u8)
}

fn non_empty<T: Default + PartialEq>(value: T) -> Option<T> {
    (value != T::default()).then_some(value)
}

fn rate_window(field: Option<RateWindowField>) -> Option<RateLimitWindow> {
    let field = field?;
    non_empty(RateLimitWindow {
        used_percentage: clamp_pct(field.used_percentage),
        resets_at: field.resets_at.and_then(|s| Timestamp::from_second(s).ok()),
    })
}

fn current_usage(field: Option<CurrentUsageField>) -> Option<AgentCurrentUsage> {
    let field = field?;
    non_empty(AgentCurrentUsage {
        input_tokens: field.input_tokens,
        output_tokens: field.output_tokens,
        cache_creation_input_tokens: field.cache_creation_input_tokens,
        cache_read_input_tokens: field.cache_read_input_tokens,
    })
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
        });
        let tokens = non_empty(AgentTokenUsage {
            total_input_tokens: self.context_window.total_input_tokens,
            total_output_tokens: self.context_window.total_output_tokens,
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage: current_usage(self.context_window.current_usage),
        });
        let rate_limits = non_empty(AgentRateLimits {
            five_hour: rate_window(self.rate_limits.five_hour),
            seven_day: rate_window(self.rate_limits.seven_day),
        });
        let pr = non_empty(AgentPullRequest {
            number: self.pr.number,
            url: self.pr.url,
            review_state: self.pr.review_state,
        });
        AgentContext {
            source: source.to_owned(),
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
            observed_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> AgentContext {
        let payload: StatuslinePayload = serde_json::from_value(value).unwrap();
        payload.into_context("claude", Timestamp::from_second(1_700_000_000).unwrap())
    }

    #[test]
    fn full_payload_projects_every_field() {
        let ctx = parse(json!({
            "session_id": "abc123",
            "session_name": "ledger-refactor",
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
        assert_eq!(ctx.session_name.as_deref(), Some("ledger-refactor"));
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

        let tokens = ctx.tokens.unwrap();
        assert_eq!(tokens.total_input_tokens, Some(15500));
        assert_eq!(tokens.context_window_size, Some(200000));
        assert_eq!(tokens.used_percentage, Some(8));
        assert_eq!(tokens.remaining_percentage, Some(92));
        let usage = tokens.current_usage.unwrap();
        assert_eq!(usage.input_tokens, Some(8500));
        assert_eq!(usage.cache_creation_input_tokens, Some(5000));
        assert_eq!(usage.cache_read_input_tokens, Some(2000));

        let rate = ctx.rate_limits.unwrap();
        let five = rate.five_hour.unwrap();
        // 23.5 rounds to 24.
        assert_eq!(five.used_percentage, Some(24));
        assert_eq!(five.resets_at, Timestamp::from_second(1738425600).ok());
        assert_eq!(rate.seven_day.unwrap().used_percentage, Some(41));

        let pr = ctx.pr.unwrap();
        assert_eq!(pr.number, Some(1234));
        assert_eq!(pr.review_state.as_deref(), Some("pending"));
    }

    #[test]
    fn sparse_payload_tolerates_missing_fields() {
        let ctx = parse(json!({ "session_id": "s", "model": {} }));
        assert_eq!(ctx.source, "claude");
        assert!(ctx.model_id.is_none());
        assert!(ctx.cost.is_none());
        assert!(ctx.tokens.is_none());
        assert!(ctx.rate_limits.is_none());
        assert!(ctx.pr.is_none());
    }

    #[test]
    fn null_current_usage_drops_only_that_field() {
        // `current_usage` is null before the first API call; the rest of the
        // context window still projects.
        let ctx = parse(json!({
            "context_window": { "used_percentage": 12, "current_usage": null }
        }));
        let tokens = ctx.tokens.unwrap();
        assert_eq!(tokens.used_percentage, Some(12));
        assert!(tokens.current_usage.is_none());
    }

    #[test]
    fn empty_current_usage_collapses_to_none() {
        // An all-null usage object carries nothing, so it collapses rather than
        // serializing as `{}`.
        let ctx = parse(json!({
            "context_window": { "used_percentage": 5, "current_usage": {} }
        }));
        assert!(ctx.tokens.unwrap().current_usage.is_none());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A newer Claude that adds keys must still parse.
        let ctx = parse(json!({ "model": { "id": "m" }, "brand_new_field": { "x": 1 } }));
        assert_eq!(ctx.model_id.as_deref(), Some("m"));
    }

    #[test]
    fn out_of_range_percentage_is_clamped() {
        let ctx = parse(json!({ "context_window": { "used_percentage": 250 } }));
        assert_eq!(ctx.tokens.unwrap().used_percentage, Some(100));
    }

    #[test]
    fn unparseable_resets_at_drops_only_that_field() {
        // i64::MIN is out of Timestamp's range; the reset drops, the pct stays.
        let ctx = parse(json!({
            "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": i64::MIN } }
        }));
        let five = ctx.rate_limits.unwrap().five_hour.unwrap();
        assert_eq!(five.used_percentage, Some(10));
        assert!(five.resets_at.is_none());
    }
}
