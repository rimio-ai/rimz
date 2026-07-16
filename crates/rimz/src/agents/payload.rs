//! Shared native-payload helpers for agent adapters.
//!
//! Adapters own provider-specific shapes; these helpers cover the small common
//! predicates and string cleanup rules used across those mappings.

use serde_json::Value;

pub(crate) fn optional_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn non_empty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Whether a `Stop`-style turn-end payload carries an explicit error signal. A
/// `Stop` only fires after a turn ran, so a clean end is a success and an error
/// signal demotes it to a failure — but that status decision now lives in the
/// lifecycle [`step`](super::lifecycle::step) table, so this helper reports only
/// the raw `errored` bit the adapter folds into
/// [`LifecycleSignal::TurnEnded`](super::LifecycleSignal::TurnEnded).
pub(crate) fn stop_payload_errored(payload: &Value) -> bool {
    payload
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload.get("error").is_some_and(|v| !v.is_null())
        || matches!(
            payload.get("status").and_then(Value::as_str),
            Some("error" | "failed" | "failure")
        )
        || matches!(
            payload.get("subtype").and_then(Value::as_str),
            Some("error" | "error_during_execution" | "error_max_turns")
        )
}

/// Tags an agent harness injects as synthetic "user" turns — a completed
/// background task, a system reminder, a slash-command echo, or an expanded
/// skill block. Their text is not user-authored, so it must never become an
/// agent's description line (the `<task-notification>…` or `<skill name=…`
/// leak). Presence of any of these rejects the whole string. The renderer
/// backstop in `sidebar_pane::render::sections::agent_card::description` shares
/// this list so producer and presentation guards cannot drift.
pub(crate) const CONTROL_TAG_PREFIXES: &[&str] = &[
    "<task-notification>",
    "<system-reminder>",
    "<command-message>",
    "<command-name>",
    "<local-command-stdout>",
    "<skill name=",
];

/// Sanitize a raw prompt/task string before it can label a sidebar row. Trims;
/// returns `None` for an empty string, or for any text carrying a harness
/// control tag (a synthetic, non-user-authored turn). KISS: a single substring
/// scan, no partial parsing — a control tag anywhere means the whole string is
/// rejected, so a raw `<task-notification>…` or `<skill name=…>` can never
/// reach the description.
pub(crate) fn sanitize_user_prompt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty())?;
    if CONTROL_TAG_PREFIXES.iter().any(|tag| trimmed.contains(tag)) {
        return None;
    }
    Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_user_prompt_accepts_real_text_and_rejects_control_payloads() {
        for tag in CONTROL_TAG_PREFIXES {
            let injected = format!("{tag}<task-id>afdc639e18e7ebdb9</...");
            assert_eq!(sanitize_user_prompt(Some(&injected)), None, "tag {tag}");
        }
        assert_eq!(
            sanitize_user_prompt(Some("please fix <system-reminder>noise</system-reminder>")),
            None,
        );
        assert_eq!(
            sanitize_user_prompt(Some(
                "<skill name=\"merge\" Location=\"/home/u/.agents/skills/merge/SKILL.md\">body</skill>",
            )),
            None,
        );
        assert_eq!(
            sanitize_user_prompt(Some("  add a dark mode toggle  ")),
            Some("add a dark mode toggle".to_owned()),
        );
        assert_eq!(sanitize_user_prompt(None), None);
        assert_eq!(sanitize_user_prompt(Some("   ")), None);
    }
}
