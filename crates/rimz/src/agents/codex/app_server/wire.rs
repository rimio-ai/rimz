//! Typed Codex app-server response models and projection helpers.
//!
//! JSON compatibility and tolerant response parsing stay here, while `app_server.rs` owns the request sequence and context merge policy.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::ExtraCredits;
use crate::agents::context::{
    AgentAccount, AgentContext, AgentRateLimits, RateLimitWindow, WindowSource,
};

use super::transport::AppServerErr;

// --- wire models (tolerant: camelCase, defaulted, unknown fields ignored) ---

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RateLimitsResponse {
    #[serde(default)]
    pub(super) rate_limits: RateLimitSnapshot,
    #[serde(default)]
    pub(super) credits: Option<CreditsWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RateLimitSnapshot {
    #[serde(default)]
    pub(super) primary: Option<RawWindow>,
    #[serde(default)]
    pub(super) secondary: Option<RawWindow>,
    /// The account's plan tier (`plus`, `pro`, `team`, …), reported alongside
    /// the windows. Account-scoped, so the provider dashboard reads it from the
    /// freshest session and uses it to label the block + mark it metered.
    #[serde(default)]
    pub(super) plan_type: Option<String>,
    #[serde(default)]
    pub(super) credits: Option<CreditsWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreditsWire {
    #[serde(default)]
    balance: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawWindow {
    #[serde(default)]
    used_percent: Option<i64>,
    #[serde(default)]
    resets_at: Option<i64>,
    #[serde(default)]
    window_duration_mins: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelListResponse {
    #[serde(default)]
    pub(super) data: Vec<RawModel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawModel {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) model: String,
    #[serde(default)]
    pub(super) display_name: String,
}

/// A model from `model/list` matched to the session's model hint.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MatchedModel {
    pub(super) id: String,
    pub(super) display_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadReadResponse {
    #[serde(default)]
    pub(super) thread: Option<RawThreadSummary>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadListResponse {
    #[serde(default)]
    pub(super) data: Vec<RawThreadSummary>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawThreadSummary {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) session_id: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ThreadSummary {
    pub(super) preview: Option<String>,
    pub(super) name: Option<String>,
}

/// Extract the loaded thread ids from a `thread/loaded/list` result, trusting only
/// recognized shapes. The documented response is a flat list of ids; accept it
/// under any of the likely keys (or as a bare array), and tolerate id-bearing
/// objects. A response carrying none of these is **untrusted** — return an error
/// so the daemon-liveness caller keeps every session rather than reaping against a
/// shape it could not read (the fix plan's "do not mass-reap when the response
/// cannot be trusted").
pub(super) fn parse_loaded_threads(result: &Value) -> Result<Vec<String>, AppServerErr> {
    const ID_LIST_KEYS: [&str; 4] = ["threadIds", "threads", "loadedThreadIds", "ids"];
    for key in ID_LIST_KEYS {
        if let Some(array) = result.get(key).and_then(Value::as_array) {
            return ids_from_array(array);
        }
    }
    if let Some(array) = result.as_array() {
        return ids_from_array(array);
    }
    Err(AppServerErr::Protocol(
        "thread/loaded/list: no recognized thread-id field".to_owned(),
    ))
}

/// Map a recognized id array to its ids. An empty array is a trusted "zero
/// loaded" — every daemon session is reapable against it. A *non-empty* array we
/// could read no id from is a wire-shape drift, not zero, so it is **untrusted**:
/// error rather than hand the caller an empty set that would mass-reap every
/// daemon session against a list it never actually read.
fn ids_from_array(array: &[Value]) -> Result<Vec<String>, AppServerErr> {
    let ids: Vec<String> = array.iter().filter_map(extract_thread_id).collect();
    if ids.is_empty() && !array.is_empty() {
        return Err(AppServerErr::Protocol(
            "thread/loaded/list: array entries carry no recognized thread id".to_owned(),
        ));
    }
    Ok(ids)
}

/// One loaded-thread entry: a bare string id, or an object carrying it under a
/// known key. `None` for an empty or shapeless entry.
fn extract_thread_id(value: &Value) -> Option<String> {
    if let Some(id) = value.as_str() {
        return (!id.is_empty()).then(|| id.to_owned());
    }
    for key in ["id", "threadId", "thread_id"] {
        if let Some(id) = value
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            return Some(id.to_owned());
        }
    }
    None
}

pub(super) fn thread_matches_session(thread: &RawThreadSummary, session_id: &str) -> bool {
    thread.id == session_id || thread.session_id.as_deref() == Some(session_id)
}

pub(super) fn thread_summary_from_raw(thread: RawThreadSummary) -> Option<ThreadSummary> {
    let preview = nonempty_trimmed(thread.preview);
    let name = nonempty_trimmed(thread.name);
    (preview.is_some() || name.is_some()).then_some(ThreadSummary { preview, name })
}

fn nonempty_trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Project the gathered read-only parts onto the transport-agnostic record.
/// Pure and deterministic so it is unit-testable from canned JSON; `observed_at`
/// is stamped by the caller. Codex has no read-only source for the actual
/// reasoning effort, tokens, cost, PR, thinking toggle, output style, or vim mode
/// — those stay `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn into_context(
    source: &str,
    rate_limits: Option<AgentRateLimits>,
    account: Option<AgentAccount>,
    model: Option<MatchedModel>,
    thread: Option<ThreadSummary>,
    agent_version: Option<String>,
    observed_at: Timestamp,
) -> AgentContext {
    AgentContext {
        source: source.to_owned(),
        session_name: thread.as_ref().and_then(|thread| thread.name.clone()),
        session_preview: thread.as_ref().and_then(|thread| thread.preview.clone()),
        model_id: model.as_ref().map(|model| model.id.clone()),
        model_display_name: model.as_ref().map(|model| model.display_name.clone()),
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: rate_limits.map(|limits| limits.stamped_at(observed_at)),
        pr: None,
        account,
        turn_error: None,
        // App-server enrichment carries no turn boundary; the rollout-tail
        // refresh path stamps `turn_complete`, never this fold.
        turn_complete: None,
        observed_at,
    }
}

/// Map Codex's positional rate-limit windows onto the provider-agnostic shape.
/// Each window carries its own `windowDurationMins`, so they need no bucketing —
/// the dashboard labels and ages each by its length. Codex reports a 5-hour
/// (`primary`) and a 7-day (`secondary`) window; carrying the raw duration means a
/// server-side change in count or length (e.g. a transient single ~30-day window)
/// maps without special-casing. The wire order is preserved here and sorted
/// short→long downstream by the producer.
pub(super) fn collect_windows(
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
) -> Option<AgentRateLimits> {
    let windows: Vec<RateLimitWindow> = [primary, secondary]
        .into_iter()
        .flatten()
        .map(|window| RateLimitWindow {
            used_percentage: window.used_percent.map(clamp_pct),
            resets_at: window
                .resets_at
                .and_then(|secs| Timestamp::from_second(secs).ok()),
            duration_mins: window
                .window_duration_mins
                .and_then(|mins| u32::try_from(mins).ok()),
            // The app-server queries Codex's official usage API, so its reading
            // is authoritative — it may lower the bar at once. `observed_at` is
            // stamped in `into_context`.
            observed_at: None,
            source: WindowSource::Authoritative,
        })
        .collect();
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

pub(super) fn collect_credits(parsed: &RateLimitsResponse) -> Option<ExtraCredits> {
    let balance = parsed
        .credits
        .as_ref()
        .and_then(CreditsWire::balance_usd)
        .or_else(|| {
            parsed
                .rate_limits
                .credits
                .as_ref()
                .and_then(CreditsWire::balance_usd)
        });
    balance.map(|remaining| ExtraCredits::known(None, Some(remaining), None))
}

impl CreditsWire {
    fn balance_usd(&self) -> Option<f64> {
        match self.balance.as_ref()? {
            Value::Number(value) => value.as_f64().filter(|value| value.is_finite()),
            Value::String(value) => value.trim().parse::<f64>().ok(),
            _ => None,
        }
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
    }
}

fn clamp_pct(value: i64) -> u8 {
    value.clamp(0, 100) as u8
}

/// Extract the Codex version from the server's `userAgent`. The first token is
/// `"<clientName>/<version>"`; the version is what we surface. `None` when the
/// shape is unexpected.
pub(super) fn codex_version_from_user_agent(user_agent: &str) -> Option<String> {
    user_agent
        .split_whitespace()
        .next()
        .and_then(|token| token.split('/').nth(1))
        .filter(|version| !version.is_empty())
        .map(ToOwned::to_owned)
}
