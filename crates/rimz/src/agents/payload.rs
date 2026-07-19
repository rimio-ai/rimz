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

/// The envelope an agent harness wraps a real user turn in. Unlike
/// [`CONTROL_TAG_PREFIXES`], this tag carries *user-authored* text, so the
/// envelope is peeled and the payload kept rather than rejected.
const USER_QUERY_OPEN: &str = "<user_query>";
const USER_QUERY_CLOSE: &str = "</user_query>";

/// Peel a `<user_query>…</user_query>` envelope off a prompt.
///
/// Only a string that *opens* with the tag is an envelope; that keeps a prompt
/// which merely quotes the tag (asking about it, pasting it as an example)
/// intact instead of silently rewriting it to its inner span. Trailing text
/// after the close tag is harness noise appended to the turn, so the inner span
/// alone is the user's text.
fn unwrap_user_query(trimmed: &str) -> &str {
    let Some(rest) = trimmed.strip_prefix(USER_QUERY_OPEN) else {
        return trimmed;
    };
    let Some((inner, _)) = rest.split_once(USER_QUERY_CLOSE) else {
        return trimmed;
    };
    inner.trim()
}

/// Sanitize a raw prompt/task string before it can label a sidebar row. Peels a
/// `<user_query>` envelope, trims, then returns `None` for an empty string or
/// for any text carrying a harness control tag (a synthetic, non-user-authored
/// turn). KISS: a single substring scan, no partial parsing — a control tag
/// anywhere means the whole string is rejected, so a raw `<task-notification>…`
/// or `<skill name=…>` can never reach the description.
pub(crate) fn sanitize_user_prompt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty())?;
    let trimmed = unwrap_user_query(trimmed);
    if trimmed.is_empty() {
        return None;
    }
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

    #[test]
    fn sanitize_user_prompt_peels_the_user_query_envelope() {
        assert_eq!(
            sanitize_user_prompt(Some("<user_query> ping </user_query>")),
            Some("ping".to_owned()),
        );
        assert_eq!(
            sanitize_user_prompt(Some(
                "<user_query>\n  add a dark mode toggle\n</user_query>"
            )),
            Some("add a dark mode toggle".to_owned()),
        );
        // Trailing harness noise after the envelope is dropped with it.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<user_query>ship it</user_query>\n<system-reminder>noise</system-reminder>",
            )),
            Some("ship it".to_owned()),
        );
        // A control tag *inside* the envelope still rejects the whole string.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<user_query><task-notification>done</task-notification></user_query>",
            )),
            None,
        );
        // An empty envelope carries no description.
        assert_eq!(
            sanitize_user_prompt(Some("<user_query>  </user_query>")),
            None
        );
    }

    #[test]
    fn sanitize_user_prompt_keeps_a_prompt_that_only_quotes_the_tag() {
        // The user is asking *about* the tag; rewriting this to "ping" would
        // silently discard the real request.
        let quoted = "the description flashed <user_query> ping </user_query> on submit. Parse it.";
        assert_eq!(sanitize_user_prompt(Some(quoted)), Some(quoted.to_owned()));
        // An unterminated envelope is not an envelope.
        assert_eq!(
            sanitize_user_prompt(Some("<user_query>truncated")),
            Some("<user_query>truncated".to_owned()),
        );
    }
}
