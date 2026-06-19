//! Provider-agnostic transcript messages and timeline shaping.
//!
//! Adapters normalize their native JSONL into [`TranscriptMessage`]. The CLI
//! asks this module to group those messages into turns or to fuse several
//! agents' messages into a channel timeline, keeping adapter-specific parsing
//! out of presentation.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Turn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub messages: Vec<TranscriptMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TimelineEntry {
    pub agent: String,
    pub role: TranscriptRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
}

/// Group a single transcript into user-prompt turns. A user message opens a
/// turn; assistant messages up to the next user message belong to it. The
/// compact view keeps each turn's user prompt and final assistant message,
/// while `details` keeps the full ordered message log.
pub fn group_turns(messages: &[TranscriptMessage], details: bool) -> Vec<Turn> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for message in messages {
        if message.role == TranscriptRole::User && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        groups.push(current);
    }

    groups
        .into_iter()
        .map(|messages| turn_from_group(messages, details))
        .collect()
}

fn turn_from_group(messages: Vec<TranscriptMessage>, details: bool) -> Turn {
    let messages = if details {
        messages
    } else {
        summarize_turn_messages(&messages)
    };
    let at = messages.iter().find_map(|message| message.at);
    Turn { at, messages }
}

fn summarize_turn_messages(messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
    let user = messages
        .iter()
        .find(|message| message.role == TranscriptRole::User)
        .cloned();
    let assistant = messages
        .iter()
        .rev()
        .find(|message| message.role == TranscriptRole::Assistant)
        .cloned();
    user.into_iter().chain(assistant).collect()
}

/// Fuse several agents into one timeline. In compact mode each agent is first
/// reduced to turn summaries; in detail mode every normalized message is merged.
/// Missing per-message timestamps inherit the last timestamp seen in that
/// agent's transcript so Codex's sparse rollout rows still sort with their
/// surrounding turn. Entries with no timestamp at all sort last and keep their
/// input order.
pub fn fuse_timeline(
    per_agent: Vec<(String, Vec<TranscriptMessage>)>,
    details: bool,
) -> Vec<TimelineEntry> {
    #[derive(Clone)]
    struct Indexed {
        entry: TimelineEntry,
        index: usize,
    }

    let mut indexed = Vec::new();
    for (agent, messages) in per_agent {
        let messages = if details {
            messages
        } else {
            group_turns(&messages, false)
                .into_iter()
                .flat_map(|turn| turn.messages)
                .collect()
        };
        let mut last_seen = None;
        for message in messages {
            if let Some(at) = message.at {
                last_seen = Some(at);
            }
            let index = indexed.len();
            indexed.push(Indexed {
                entry: TimelineEntry {
                    agent: agent.clone(),
                    role: message.role,
                    at: message.at.or(last_seen),
                    text: message.text,
                },
                index,
            });
        }
    }

    indexed.sort_by(|left, right| match (left.entry.at, right.entry.at) {
        (Some(a), Some(b)) => a.cmp(&b).then_with(|| left.index.cmp(&right.index)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.index.cmp(&right.index),
    });
    indexed.into_iter().map(|indexed| indexed.entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn msg(role: TranscriptRole, at: Option<&str>, text: &str) -> TranscriptMessage {
        TranscriptMessage {
            role,
            at: at.map(ts),
            text: text.to_owned(),
        }
    }

    #[test]
    fn group_turns_keeps_last_assistant_by_default() {
        let turns = group_turns(
            &[
                msg(TranscriptRole::User, Some("2026-06-01T00:00:00Z"), "one"),
                msg(
                    TranscriptRole::Assistant,
                    Some("2026-06-01T00:00:01Z"),
                    "draft",
                ),
                msg(
                    TranscriptRole::Assistant,
                    Some("2026-06-01T00:00:02Z"),
                    "final",
                ),
                msg(TranscriptRole::User, Some("2026-06-01T00:00:03Z"), "two"),
            ],
            false,
        );

        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns[0]
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "final"]
        );
        assert_eq!(
            turns[1]
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["two"]
        );
    }

    #[test]
    fn group_turns_details_keeps_every_message() {
        let turns = group_turns(
            &[
                msg(TranscriptRole::User, None, "one"),
                msg(TranscriptRole::Assistant, None, "draft"),
                msg(TranscriptRole::Assistant, None, "final"),
            ],
            true,
        );
        assert_eq!(
            turns[0]
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "draft", "final"]
        );
    }

    #[test]
    fn fuse_timeline_carries_forward_timestamps_and_sorts_missing_last() {
        let timeline = fuse_timeline(
            vec![
                (
                    "@a".to_owned(),
                    vec![
                        msg(TranscriptRole::User, Some("2026-06-01T00:00:02Z"), "a user"),
                        msg(TranscriptRole::Assistant, None, "a answer"),
                    ],
                ),
                (
                    "@b".to_owned(),
                    vec![msg(
                        TranscriptRole::Assistant,
                        Some("2026-06-01T00:00:01Z"),
                        "b answer",
                    )],
                ),
                (
                    "@c".to_owned(),
                    vec![msg(TranscriptRole::Assistant, None, "c answer")],
                ),
            ],
            true,
        );

        assert_eq!(
            timeline
                .iter()
                .map(|entry| (entry.agent.as_str(), entry.text.as_str(), entry.at))
                .collect::<Vec<_>>(),
            vec![
                ("@b", "b answer", Some(ts("2026-06-01T00:00:01Z"))),
                ("@a", "a user", Some(ts("2026-06-01T00:00:02Z"))),
                ("@a", "a answer", Some(ts("2026-06-01T00:00:02Z"))),
                ("@c", "c answer", None),
            ]
        );
    }

    #[test]
    fn fuse_timeline_summarizes_each_agent_before_merging() {
        let timeline = fuse_timeline(
            vec![(
                "@a".to_owned(),
                vec![
                    msg(TranscriptRole::User, Some("2026-06-01T00:00:00Z"), "prompt"),
                    msg(
                        TranscriptRole::Assistant,
                        Some("2026-06-01T00:00:01Z"),
                        "draft",
                    ),
                    msg(
                        TranscriptRole::Assistant,
                        Some("2026-06-01T00:00:02Z"),
                        "final",
                    ),
                ],
            )],
            false,
        );
        assert_eq!(
            timeline
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["prompt", "final"]
        );
    }
}
