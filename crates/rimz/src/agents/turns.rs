//! Per-turn transcript and spend projection.

use jiff::Timestamp;
use serde::Serialize;

use super::spending::{CachedEntry, session_entries};
use super::transcript::{TranscriptMessage, TranscriptRole};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Done,
    Open,
    Cut,
}

impl TurnOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Open => "open",
            Self::Cut => "cut",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TurnRecord {
    pub started_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    pub prompt: String,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub api_calls: usize,
    pub outcome: TurnOutcome,
}

/// Group one session's normalized transcript and spend rows by user turn.
/// User records without a timestamp cannot anchor spend boundaries and are
/// omitted rather than reported with a fabricated time.
pub fn session_turns(
    messages: &[TranscriptMessage],
    entries: &[CachedEntry],
    session_id: &str,
    session_open: bool,
) -> Vec<TurnRecord> {
    let starts = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.role == TranscriptRole::User)
                .then_some(message.at.map(|at| (index, at)))
                .flatten()
        })
        .collect::<Vec<_>>();
    let entries = session_entries(entries, session_id);

    starts
        .iter()
        .enumerate()
        .map(|(turn_index, &(message_index, started_at))| {
            let next_message_index = starts
                .get(turn_index + 1)
                .map_or(messages.len(), |(index, _)| *index);
            let next_started_at = starts.get(turn_index + 1).map(|(_, at)| *at);
            let turn_messages = &messages[message_index..next_message_index];
            let turn_entries = entries.iter().copied().filter(|entry| {
                timestamp(entry.ts_secs).is_some_and(|at| {
                    at >= started_at && next_started_at.is_none_or(|next| at < next)
                })
            });

            let mut fresh_input = 0u64;
            let mut output = 0u64;
            let mut cache_read = 0u64;
            let mut cache_write = 0u64;
            let mut cost_usd = 0.0;
            let mut api_calls = 0usize;
            let mut ended_at = turn_messages.iter().filter_map(|message| message.at).max();
            for entry in turn_entries {
                fresh_input = fresh_input.saturating_add(entry.input);
                output = output.saturating_add(entry.output);
                cache_read = cache_read.saturating_add(entry.cache_read);
                cache_write = cache_write.saturating_add(entry.cache_write);
                if entry.cost_usd.is_finite() && entry.cost_usd > 0.0 {
                    cost_usd += entry.cost_usd;
                }
                api_calls = api_calls.saturating_add(1);
                ended_at = ended_at.max(timestamp(entry.ts_secs));
            }

            let replied = turn_messages
                .iter()
                .any(|message| message.role == TranscriptRole::Assistant);
            let final_turn = turn_index + 1 == starts.len();
            let outcome = if replied {
                TurnOutcome::Done
            } else if final_turn && session_open {
                TurnOutcome::Open
            } else {
                TurnOutcome::Cut
            };
            TurnRecord {
                started_at,
                ended_at,
                prompt: messages[message_index].text.clone(),
                fresh_input,
                output,
                cache_read,
                cache_write,
                cost_usd: (cost_usd > 0.0).then_some(cost_usd),
                api_calls,
                outcome,
            }
        })
        .collect()
}

fn timestamp(seconds: u64) -> Option<Timestamp> {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| Timestamp::from_second(seconds).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).expect("timestamp")
    }

    fn message(role: TranscriptRole, seconds: i64, text: &str) -> TranscriptMessage {
        TranscriptMessage {
            role,
            at: Some(at(seconds)),
            text: text.to_owned(),
        }
    }

    fn entry(seconds: u64, thread_id: Option<&str>) -> CachedEntry {
        CachedEntry {
            ts_secs: seconds,
            cost_usd: 0.25,
            input: 10,
            output: 20,
            cache_write: 30,
            cache_read: 40,
            tool_calls: Default::default(),
            message_id: None,
            request_id: None,
            dedup_key: None,
            thread_id: thread_id.map(ToOwned::to_owned),
            is_sidechain: false,
            has_speed: false,
            model: None,
            rolled: false,
        }
    }

    #[test]
    fn groups_entries_at_user_boundaries_and_filters_session() {
        let messages = vec![
            message(TranscriptRole::User, 100, "first"),
            message(TranscriptRole::Assistant, 110, "answer"),
            message(TranscriptRole::User, 200, "second"),
        ];
        let entries = vec![
            entry(105, Some("session-a")),
            entry(150, Some("other")),
            entry(205, Some("session-a")),
        ];

        let turns = session_turns(&messages, &entries, "session-a", true);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].prompt, "first");
        assert_eq!(turns[0].fresh_input, 10);
        assert_eq!(turns[0].outcome, TurnOutcome::Done);
        assert_eq!(turns[1].fresh_input, 10);
        assert_eq!(turns[1].outcome, TurnOutcome::Open);
    }

    #[test]
    fn marks_unanswered_closed_turns_cut_and_accepts_session_scoped_entries() {
        let messages = vec![
            message(TranscriptRole::User, 100, "cut"),
            message(TranscriptRole::User, 200, "done"),
            message(TranscriptRole::Assistant, 210, "answer"),
        ];
        let entries = vec![entry(105, None), entry(205, None)];

        let turns = session_turns(&messages, &entries, "session-a", false);

        assert_eq!(turns[0].outcome, TurnOutcome::Cut);
        assert_eq!(turns[1].outcome, TurnOutcome::Done);
        assert_eq!(turns[0].api_calls, 1);
        assert_eq!(turns[1].api_calls, 1);
    }

    #[test]
    fn empty_or_undated_transcript_has_no_turns() {
        assert!(session_turns(&[], &[], "session-a", false).is_empty());
        let messages = [TranscriptMessage {
            role: TranscriptRole::User,
            at: None,
            text: "unknown time".to_owned(),
        }];
        assert!(session_turns(&messages, &[], "session-a", true).is_empty());
    }
}
