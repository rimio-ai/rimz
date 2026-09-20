//! Shared native-payload helpers for agent adapters.
//!
//! Adapters own provider-specific shapes; these helpers cover the small common
//! predicates and string cleanup rules used across those mappings.

use std::borrow::Cow;

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

const TASK_NOTIFICATION_OPEN: &str = "<task-notification>";
const TASK_NOTIFICATION_CLOSE: &str = "</task-notification>";

/// The ids of background tasks a `<task-notification>` prompt reports as no
/// longer running. One prompt may carry several notification blocks; a block
/// without a task id or status is skipped. Empty for any other prompt.
pub(in crate::agents) fn finished_task_notification_ids(prompt: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = prompt.trim_start();
    while let Some(after_open) = rest.strip_prefix(TASK_NOTIFICATION_OPEN) {
        let (block, tail) = after_open
            .split_once(TASK_NOTIFICATION_CLOSE)
            .unwrap_or((after_open, ""));
        if let (Some(id), Some(status)) = (tag_text(block, "task-id"), tag_text(block, "status"))
            && status != "running"
        {
            ids.push(id.to_owned());
        }
        rest = tail.trim_start();
    }
    ids
}

/// The trimmed, non-empty text of the first `<tag>…</tag>` in `body`.
fn tag_text<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let (_, after_open) = body.split_once(&format!("<{tag}>"))?;
    let (text, _) = after_open.split_once(&format!("</{tag}>"))?;
    Some(text.trim()).filter(|text| !text.is_empty())
}

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

/// The markup an agent harness wraps a *bracketed paste* in, each tag alone on
/// its line and the close tag repeating the `id` attribute (not XML):
///
/// ```text
/// <pasted_content id="ce95">
/// …pasted text…
/// </pasted_content id="ce95">
/// ```
///
/// Observed in Claude Code 2.1.278. RimZ delivers every queued prompt by
/// bracketed paste, so a delivery arrives wrapped and the tags would otherwise
/// reach the transcript, the delivery-confirmation reason, the user-input spend
/// ledger, the turn-start reset, and the card task as the user's own text. Like
/// [`USER_QUERY_OPEN`] the envelope carries user-authored text, so it is peeled
/// and the payload kept.
const PASTE_OPEN_PREFIX: &str = "<pasted_content id=\"";
const PASTE_CLOSE_PREFIX: &str = "</pasted_content id=\"";
const PASTE_TAG_SUFFIX: &str = "\">";

/// The id of a paste tag, when `line` is exactly that tag. Surrounding
/// horizontal whitespace and a trailing `\r` are ignored; a tag sharing its
/// line with other text is not a tag.
fn paste_tag_id<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let id = line
        .trim()
        .strip_prefix(prefix)?
        .strip_suffix(PASTE_TAG_SUFFIX)?;
    (!id.is_empty() && !id.contains('"')).then_some(id)
}

/// Drop the tag lines of every balanced paste envelope, keeping their content
/// verbatim.
///
/// An opener pairs with the nearest following closer of the same id — ids
/// repeat across the pastes of one session, so sequential envelopes share one.
/// An opener with no matching closer, a closer with no opener, and a tag
/// sharing its line with other text all stay as text, which keeps a prompt that
/// merely quotes the markup intact. Only the tag lines and their own line
/// breaks go: every other byte, including interior `\r\n`, survives.
fn unwrap_paste_envelopes(text: &str) -> Cow<'_, str> {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let mut dropped = vec![false; lines.len()];
    let mut unwrapped = false;
    for open in 0..lines.len() {
        let Some(id) = paste_tag_id(lines[open], PASTE_OPEN_PREFIX) else {
            continue;
        };
        let close = (open + 1..lines.len()).find(|&line| {
            !dropped[line] && paste_tag_id(lines[line], PASTE_CLOSE_PREFIX) == Some(id)
        });
        if let Some(close) = close {
            dropped[open] = true;
            dropped[close] = true;
            unwrapped = true;
        }
    }
    if !unwrapped {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        lines
            .into_iter()
            .zip(dropped)
            .filter_map(|(line, dropped)| (!dropped).then_some(line))
            .collect(),
    )
}

/// Sanitize a raw prompt/task string before it can label a sidebar row. Peels a
/// `<user_query>` envelope and every balanced `<pasted_content id=…>` envelope,
/// trims, then returns `None` for an empty string or for any text carrying a
/// harness control tag (a synthetic, non-user-authored turn). KISS: a single
/// substring scan, no partial parsing — a control tag anywhere means the whole
/// string is rejected, so a raw `<task-notification>…` or `<skill name=…>` can
/// never reach the description.
pub(crate) fn sanitize_user_prompt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty())?;
    let unwrapped = unwrap_paste_envelopes(unwrap_user_query(trimmed));
    let cleaned = unwrapped.trim();
    if cleaned.is_empty() {
        return None;
    }
    if CONTROL_TAG_PREFIXES.iter().any(|tag| cleaned.contains(tag)) {
        return None;
    }
    Some(cleaned.to_owned())
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
    fn task_notification_prompts_yield_their_finished_task_ids() {
        let block = |id: &str, status: &str| {
            format!(
                "<task-notification>\n<task-id>{id}</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n\
                 <output-file>/tmp/{id}.output</output-file>\n<status>{status}</status>\n\
                 <summary>Background command finished</summary>\n</task-notification>"
            )
        };
        assert_eq!(
            finished_task_notification_ids(&block("b1", "completed")),
            vec!["b1".to_owned()]
        );
        assert_eq!(
            finished_task_notification_ids(&format!(
                "{}\n{}\n{}",
                block("b1", "failed"),
                block("b2", "running"),
                block("b3", "killed")
            )),
            vec!["b1".to_owned(), "b3".to_owned()]
        );
        for prompt in [
            "<task-notification><status>completed</status></task-notification>",
            "<task-notification><task-id>b1</task-id>",
            "please check <task-notification><task-id>b1</task-id><status>completed</status>",
            "",
        ] {
            assert!(
                finished_task_notification_ids(prompt).is_empty(),
                "{prompt}"
            );
        }
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
    fn sanitize_user_prompt_unwraps_paste_envelopes() {
        // A queued delivery: RimZ pastes it, so the whole message arrives
        // wrapped and the header must survive to be parsed downstream.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<pasted_content id=\"ce95\">\nType: STAGE\nFrom: @rimz\nContent:\nbody\n</pasted_content id=\"ce95\">",
            )),
            Some("Type: STAGE\nFrom: @rimz\nContent:\nbody".to_owned()),
        );
        // A human paste inside typed text: all three parts stay, in order.
        assert_eq!(
            sanitize_user_prompt(Some(
                "do you think \n\n<pasted_content id=\"77ca\">\n`cargo x`\n</pasted_content id=\"77ca\">\n\n can be run?",
            )),
            Some("do you think \n\n`cargo x`\n\n can be run?".to_owned()),
        );
        // Ids repeat across a session, so sequential envelopes share one.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<pasted_content id=\"a\">\nfirst\n</pasted_content id=\"a\">\nmid\n<pasted_content id=\"a\">\nsecond\n</pasted_content id=\"a\">",
            )),
            Some("first\nmid\nsecond".to_owned()),
        );
        // CRLF tag lines are tags; the content's own line breaks are untouched.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<pasted_content id=\"a\">\r\nkeep\r\nme\r\n</pasted_content id=\"a\">",
            )),
            Some("keep\r\nme".to_owned()),
        );
        // An envelope with no content carries no description.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<pasted_content id=\"a\">\n \n</pasted_content id=\"a\">"
            )),
            None,
        );
        // A control tag inside an envelope still rejects the whole string.
        assert_eq!(
            sanitize_user_prompt(Some(
                "<pasted_content id=\"a\">\n<system-reminder>noise</system-reminder>\n</pasted_content id=\"a\">",
            )),
            None,
        );
    }

    #[test]
    fn sanitize_user_prompt_keeps_unpaired_and_inline_paste_markup() {
        for text in [
            // No closer: not an envelope.
            "<pasted_content id=\"a\">\nbody",
            // No opener.
            "body\n</pasted_content id=\"a\">",
            // Mismatched ids.
            "<pasted_content id=\"a\">\nbody\n</pasted_content id=\"b\">",
            // The tag shares its line, so it is text the user wrote.
            "what does <pasted_content id=\"a\"> mean?",
            "<pasted_content id=\"a\"> inline </pasted_content id=\"a\">",
        ] {
            assert_eq!(
                sanitize_user_prompt(Some(text)),
                Some(text.to_owned()),
                "{text}"
            );
        }
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
