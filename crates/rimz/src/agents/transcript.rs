//! Provider-agnostic transcript messages and timeline shaping.
//!
//! Adapters normalize their native JSONL into [`TranscriptMessage`]. The CLI
//! asks this module to group those messages into turns or build a channel chat
//! log, keeping adapter-specific parsing out of presentation.

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
pub struct ChatEntry {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChat {
    pub handle: String,
    pub messages: Vec<TranscriptMessage>,
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

/// Build a channel chat log from each agent's native transcript. `classify`
/// inverts the message-system prefix: `Some((sender, body))` marks a routed
/// agent-to-agent delivery, `None` a human prompt. Per agent, each turn:
/// - routed opener emits one `sender -> @handle` entry, and the turn's assistant
///   replies are dropped.
/// - human opener emits a `user -> @handle` entry, then the turn-final assistant
///   reply, or every assistant reply when `details`.
///
/// Missing timestamps inherit the last seen in that agent's transcript; entries
/// sort by timestamp, missing last, stable within equal or unknown times.
pub fn build_chat(
    per_agent: Vec<AgentChat>,
    details: bool,
    classify: impl Fn(&str) -> Option<(String, String)>,
) -> Vec<ChatEntry> {
    #[derive(Clone)]
    struct Indexed {
        entry: ChatEntry,
        index: usize,
    }

    let mut indexed = Vec::new();
    {
        let mut push_entry = |entry| {
            let index = indexed.len();
            indexed.push(Indexed { entry, index });
        };
        for AgentChat { handle, messages } in per_agent {
            let mut last_seen = None;
            for turn in group_turns(&messages, true) {
                let mut opener = None;
                let mut assistants = Vec::new();
                for message in turn.messages {
                    if let Some(at) = message.at {
                        last_seen = Some(at);
                    }
                    let at = message.at.or(last_seen);
                    match message.role {
                        TranscriptRole::User if opener.is_none() => {
                            opener = Some(TranscriptMessage {
                                role: message.role,
                                at,
                                text: message.text,
                            });
                        }
                        TranscriptRole::Assistant => {
                            assistants.push(TranscriptMessage {
                                role: message.role,
                                at,
                                text: message.text,
                            });
                        }
                        TranscriptRole::User => {}
                    }
                }

                if let Some(opener) = opener {
                    if let Some((sender, body)) = classify(&opener.text) {
                        push_entry(ChatEntry {
                            from: sender,
                            to: Some(handle.clone()),
                            at: opener.at,
                            text: body,
                        });
                        continue;
                    }
                    push_entry(ChatEntry {
                        from: "user".to_owned(),
                        to: Some(handle.clone()),
                        at: opener.at,
                        text: opener.text,
                    });
                }

                if details {
                    for assistant in assistants {
                        push_entry(ChatEntry {
                            from: handle.clone(),
                            to: None,
                            at: assistant.at,
                            text: assistant.text,
                        });
                    }
                } else if let Some(assistant) = assistants.into_iter().last() {
                    push_entry(ChatEntry {
                        from: handle.clone(),
                        to: None,
                        at: assistant.at,
                        text: assistant.text,
                    });
                }
            }
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

    fn classify(text: &str) -> Option<(String, String)> {
        text.strip_prefix("from @planner: ")
            .map(|body| ("@planner".to_owned(), body.to_owned()))
    }

    fn agent(handle: &str, messages: Vec<TranscriptMessage>) -> AgentChat {
        AgentChat {
            handle: handle.to_owned(),
            messages,
        }
    }

    #[test]
    fn build_chat_emits_human_turn_final_and_routed_opener_only() {
        let chat = build_chat(
            vec![agent(
                "@coder",
                vec![
                    msg(
                        TranscriptRole::User,
                        Some("2026-06-01T00:00:00Z"),
                        "human prompt",
                    ),
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
                    msg(
                        TranscriptRole::User,
                        Some("2026-06-01T00:00:03Z"),
                        "from @planner: do the thing",
                    ),
                    msg(
                        TranscriptRole::Assistant,
                        Some("2026-06-01T00:00:04Z"),
                        "routed reply",
                    ),
                ],
            )],
            false,
            classify,
        );

        assert_eq!(
            chat.iter()
                .map(|entry| {
                    (
                        entry.from.as_str(),
                        entry.to.as_deref(),
                        entry.text.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("user", Some("@coder"), "human prompt"),
                ("@coder", None, "final"),
                ("@planner", Some("@coder"), "do the thing"),
            ]
        );
    }

    #[test]
    fn build_chat_details_keeps_every_human_assistant_message() {
        let chat = build_chat(
            vec![agent(
                "@coder",
                vec![
                    msg(TranscriptRole::User, None, "human prompt"),
                    msg(TranscriptRole::Assistant, None, "draft"),
                    msg(TranscriptRole::Assistant, None, "final"),
                    msg(TranscriptRole::User, None, "from @planner: routed"),
                    msg(TranscriptRole::Assistant, None, "hidden routed reply"),
                ],
            )],
            true,
            classify,
        );

        assert_eq!(
            chat.iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["human prompt", "draft", "final", "routed"]
        );
    }

    #[test]
    fn build_chat_carries_forward_timestamps_and_sorts_missing_last() {
        let chat = build_chat(
            vec![
                agent(
                    "@a",
                    vec![
                        msg(TranscriptRole::User, Some("2026-06-01T00:00:02Z"), "a user"),
                        msg(TranscriptRole::Assistant, None, "a answer"),
                    ],
                ),
                agent(
                    "@b",
                    vec![msg(
                        TranscriptRole::Assistant,
                        Some("2026-06-01T00:00:01Z"),
                        "b answer",
                    )],
                ),
                agent("@c", vec![msg(TranscriptRole::Assistant, None, "c answer")]),
            ],
            true,
            classify,
        );

        assert_eq!(
            chat.iter()
                .map(|entry| (entry.from.as_str(), entry.text.as_str(), entry.at))
                .collect::<Vec<_>>(),
            vec![
                ("@b", "b answer", Some(ts("2026-06-01T00:00:01Z"))),
                ("user", "a user", Some(ts("2026-06-01T00:00:02Z"))),
                ("@a", "a answer", Some(ts("2026-06-01T00:00:02Z"))),
                ("@c", "c answer", None),
            ]
        );
    }
}
