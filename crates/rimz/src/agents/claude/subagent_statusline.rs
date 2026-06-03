//! Structured parser for Claude Code's `subagentStatusLine` JSON.
//!
//! Claude `exec`s its configured `subagentStatusLine` command to render each
//! child row in the agent panel and pipes it `columns` plus a `tasks` array (see
//! `docs/internals/adapter/claude-reference.md`). This module is a tolerant
//! serde model of that blob and the projection onto one
//! [`SubagentObservation`] per attributable task. Every field is optional,
//! unknown keys ride along, and one malformed value drops only its own datum —
//! enrichment is never correctness.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{SubagentContext, SubagentObservation};

/// The payload Claude pipes on stdin: only the `tasks` array matters to Rimz
/// (`columns` and the common hook fields are ignored). `#[serde(default)]` keeps
/// a sparse or evolved payload parseable; the absence of `deny_unknown_fields`
/// lets new keys ride along untouched.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SubagentStatuslinePayload {
    tasks: Vec<SubagentTask>,
}

/// One child row. `name`, `status`, `label`, `cwd`, and `tokenSamples` are
/// carried by the upstream payload but Rimz ignores them; `type`, `description`,
/// `tokenCount`, and `startTime` are the four it paints.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SubagentTask {
    /// The task id, equal to the child's `agent_id` — the key its sidecar files
    /// under. A task with no id can't be attributed to a row, so it's dropped.
    id: Option<String>,
    /// The agent's type label (`Explore`, `review`, …). Folds onto
    /// `AgentState.task` when the lifecycle hook never provided one — common
    /// for fork agents that carry no `agent_type` in `SubagentStart`.
    #[serde(rename = "type")]
    r#type: Option<String>,
    /// What the parent asked the child to do.
    description: Option<String>,
    /// `startTime`. The unit is unpinned upstream, so it is held as a raw value
    /// and parsed tolerantly (RFC 3339 string, or an epoch number/string in
    /// seconds or milliseconds).
    #[serde(rename = "startTime")]
    start_time: Option<Value>,
    /// `tokenCount`. Usually a number; held as a raw value so a string or odd
    /// shape drops only this figure, not the whole task.
    #[serde(rename = "tokenCount")]
    token_count: Option<Value>,
}

impl SubagentStatuslinePayload {
    /// Project each task carrying an `id` into a per-child observation.
    /// `observed_at` is stamped by the caller so the parser stays pure and
    /// deterministic in tests.
    pub(crate) fn into_observations(self, observed_at: Timestamp) -> Vec<SubagentObservation> {
        self.tasks
            .into_iter()
            .filter_map(|task| {
                let agent_id = task.id.filter(|id| !id.is_empty())?;
                Some(SubagentObservation {
                    agent_id,
                    context: SubagentContext {
                        agent_type: task.r#type.filter(|t| !t.is_empty()),
                        description: task.description.filter(|d| !d.is_empty()),
                        token_count: task.token_count.as_ref().and_then(value_as_u64),
                        started_at: task.start_time.as_ref().and_then(value_as_timestamp),
                        observed_at,
                    },
                })
            })
            .collect()
    }
}

/// A JSON number (or stringified integer) as `u64`; anything else is `None`. A
/// fractional number floors to the unit, a negative one clamps to zero.
fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// `startTime` as a `Timestamp`, tolerating the unit drift the upstream doc
/// leaves open: an RFC 3339 string, or an epoch number/stringified-number in
/// seconds or milliseconds (disambiguated by magnitude).
fn value_as_timestamp(value: &Value) -> Option<Timestamp> {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .and_then(epoch_to_timestamp),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<Timestamp>()
                .ok()
                .or_else(|| s.parse::<i64>().ok().and_then(epoch_to_timestamp))
        }
        _ => None,
    }
}

/// An epoch integer to a `Timestamp`, treating a value at or past the threshold
/// as milliseconds and a smaller one as seconds. The cut (1e11) sits well above
/// any plausible seconds epoch (~1.7e9) and well below the same instant in
/// milliseconds (~1.7e12), so the two never alias.
fn epoch_to_timestamp(value: i64) -> Option<Timestamp> {
    const EPOCH_MS_THRESHOLD: i64 = 100_000_000_000;
    if value.abs() >= EPOCH_MS_THRESHOLD {
        Timestamp::from_millisecond(value).ok()
    } else {
        Timestamp::from_second(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observe(value: serde_json::Value) -> Vec<SubagentObservation> {
        let payload: SubagentStatuslinePayload = serde_json::from_value(value).unwrap();
        payload.into_observations(Timestamp::from_second(1_700_000_100).unwrap())
    }

    #[test]
    fn full_payload_projects_every_task() {
        let obs = observe(json!({
            "columns": 80,
            "tasks": [
                {
                    "id": "child-1",
                    "type": "Explore",
                    "status": "running",
                    "description": "locate the render seam",
                    "startTime": 1_700_000_000,
                    "tokenCount": 12_400
                },
                {
                    "id": "child-2",
                    "type": "review",
                    "description": "audit the trust hash",
                    "startTime": 1_700_000_055,
                    "tokenCount": 3_100
                }
            ]
        }));
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].agent_id, "child-1");
        assert_eq!(obs[0].context.agent_type.as_deref(), Some("Explore"));
        assert_eq!(
            obs[0].context.description.as_deref(),
            Some("locate the render seam")
        );
        assert_eq!(obs[0].context.token_count, Some(12_400));
        assert_eq!(
            obs[0].context.started_at,
            Some(Timestamp::from_second(1_700_000_000).unwrap())
        );
        assert_eq!(obs[1].agent_id, "child-2");
        assert_eq!(obs[1].context.agent_type.as_deref(), Some("review"));
    }

    #[test]
    fn task_without_id_is_dropped() {
        let obs = observe(json!({ "tasks": [{ "description": "orphan", "tokenCount": 5 }] }));
        assert!(obs.is_empty());
    }

    #[test]
    fn sparse_task_keeps_only_present_fields() {
        let obs = observe(json!({ "tasks": [{ "id": "child-1" }] }));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].context.agent_type, None);
        assert_eq!(obs[0].context.description, None);
        assert_eq!(obs[0].context.token_count, None);
        assert_eq!(obs[0].context.started_at, None);
    }

    #[test]
    fn empty_type_is_dropped() {
        // An empty `type` string is not a useful label — treat it as absent so
        // fork agents without a type don't render as an empty name.
        let obs = observe(json!({ "tasks": [{ "id": "c", "type": "" }] }));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].context.agent_type, None);
    }

    #[test]
    fn empty_payload_yields_nothing() {
        assert!(observe(json!({})).is_empty());
        assert!(observe(json!({ "tasks": [] })).is_empty());
    }

    #[test]
    fn start_time_in_milliseconds_is_recognized() {
        let obs = observe(json!({
            "tasks": [{ "id": "c", "startTime": 1_700_000_000_000_i64 }]
        }));
        assert_eq!(
            obs[0].context.started_at,
            Some(Timestamp::from_second(1_700_000_000).unwrap())
        );
    }

    #[test]
    fn start_time_as_rfc3339_string_parses() {
        let obs = observe(json!({
            "tasks": [{ "id": "c", "startTime": "2023-11-14T22:13:20Z" }]
        }));
        assert_eq!(
            obs[0].context.started_at,
            Some(Timestamp::from_second(1_700_000_000).unwrap())
        );
    }

    #[test]
    fn start_time_as_numeric_string_parses() {
        let obs = observe(json!({
            "tasks": [{ "id": "c", "startTime": "1700000000" }]
        }));
        assert_eq!(
            obs[0].context.started_at,
            Some(Timestamp::from_second(1_700_000_000).unwrap())
        );
    }

    #[test]
    fn garbage_start_time_and_token_count_drop_only_themselves() {
        let obs = observe(json!({
            "tasks": [{
                "id": "c",
                "description": "still here",
                "startTime": "not-a-date",
                "tokenCount": "lots"
            }]
        }));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].context.description.as_deref(), Some("still here"));
        assert_eq!(obs[0].context.started_at, None);
        assert_eq!(obs[0].context.token_count, None);
    }

    #[test]
    fn unknown_keys_ride_along() {
        let obs = observe(json!({
            "tasks": [{ "id": "c", "label": "x", "cwd": "/tmp", "tokenSamples": [1, 2] }]
        }));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].agent_id, "c");
    }
}
