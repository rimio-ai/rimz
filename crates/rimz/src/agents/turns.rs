//! Per-turn transcript and spend projection, and the RimZ transcript entry that opened each turn.

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::payload::SanitizedPrompt;
use super::spending::{CachedEntry, session_entries};
use super::transcript::{TranscriptMessage, TranscriptRole};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId};
use crate::transcript::{TranscriptEntry, TranscriptKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Done,
    Open,
    Unknown,
}

impl TurnOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Open => "open",
            Self::Unknown => "-",
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
                    at.as_second() >= started_at.as_second()
                        && next_started_at.is_none_or(|next| at.as_second() < next.as_second())
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
                TurnOutcome::Unknown
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

/// How long before the provider's turn start the hook may record its opener entry: the
/// hook and the provider stamp the same moment from two clocks, measured at most about a
/// second apart.
const OPENER_LEAD: SignedDuration = SignedDuration::from_secs(10);

/// How long after the provider's turn start the hook may record its opener entry. The
/// hook stamps the entry when its frame applies, and Antigravity reports a steered
/// prompt on a later lifecycle frame, up to 140 s after the turn start in the room's
/// records; five minutes leaves that lag room to double.
const OPENER_LAG: SignedDuration = SignedDuration::from_secs(5 * 60);

/// The RimZ transcript entry that opened each of one agent's turns, given as the
/// provider's turn start and the text of the prompt's first section (a header's body,
/// or the whole bare section).
///
/// The text is sanitized as the hook sanitizes a prompt before recording it, then
/// compared, whitespace aside, with one entry or with the in-order run of entries one
/// submission wrote for a batch of pasted members (the entries share an `ingress` where
/// the hook frame carried one); the run's first entry is the opener. An entry pairs only
/// with a turn of its agent that started between five minutes before the entry and ten
/// seconds after it, and only once. Turns take their entries in time order, so two turns
/// of the same text pair with that text's entries in the order both were written. Only
/// the weekly buckets from the earliest turn's window to the latest turn's are read, and
/// an unreadable one is an error, never an absent entry.
pub fn turn_openers(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    turns: &[(Timestamp, &str)],
) -> crate::transcript::Result<Vec<Option<TranscriptEntry>>> {
    let mut order = (0..turns.len()).collect::<Vec<_>>();
    order.sort_by_key(|&turn| turns[turn].0);
    let (Some(&first), Some(&last)) = (order.first(), order.last()) else {
        return Ok(Vec::new());
    };
    let since = turns[first]
        .0
        .checked_sub(OPENER_LEAD)
        .unwrap_or(turns[first].0);
    let until = turns[last]
        .0
        .checked_add(OPENER_LAG)
        .unwrap_or(turns[last].0);
    let entries = crate::transcript::read_between(paths, since, until)?
        .into_iter()
        .filter(|entry| {
            &entry.kind == kind
                && &entry.agent_id == agent_id
                && !matches!(
                    entry.entry,
                    TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error
                )
        })
        .collect::<Vec<_>>();
    let mut taken = vec![false; entries.len()];
    let mut openers = vec![None; turns.len()];
    let mut window_start = 0;
    for turn in order {
        let (started_at, text) = turns[turn];
        let Some(text) = SanitizedPrompt::new(Some(text)) else {
            continue;
        };
        let words = text.split_whitespace().collect::<Vec<_>>();
        let earliest = started_at.checked_sub(OPENER_LEAD).unwrap_or(started_at);
        let latest = started_at.checked_add(OPENER_LAG).unwrap_or(started_at);
        while entries
            .get(window_start)
            .is_some_and(|entry| entry.at < earliest)
        {
            window_start += 1;
        }
        let run = (window_start..entries.len())
            .take_while(|&index| entries[index].at <= latest)
            .find_map(|index| opener_run(&entries, &taken, index, latest, &words));
        if let Some(run) = run {
            taken[run.clone()].fill(true);
            openers[turn] = Some(entries[run.start].clone());
        }
    }
    Ok(openers)
}

/// The run of untaken entries from `start` whose words, joined in order, are `words`:
/// one entry, or the members one submission wrote for a pasted batch.
fn opener_run(
    entries: &[TranscriptEntry],
    taken: &[bool],
    start: usize,
    latest: Timestamp,
    words: &[&str],
) -> Option<std::ops::Range<usize>> {
    let mut rest = words;
    for index in start..entries.len() {
        let entry = &entries[index];
        if taken[index] || entry.at > latest || entry.ingress != entries[start].ingress {
            return None;
        }
        let member = entry.text.split_whitespace().collect::<Vec<_>>();
        rest = rest
            .strip_prefix(member.as_slice())
            .filter(|_| !member.is_empty())?;
        if rest.is_empty() {
            return Some(start..index + 1);
        }
    }
    None
}

fn timestamp(seconds: u64) -> Option<Timestamp> {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| Timestamp::from_second(seconds).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EventId;
    use crate::transcript::EntryOrigin;

    fn paths() -> (tempfile::TempDir, StatePaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(id, dir.path()).expect("state paths");
        (dir, paths)
    }

    fn agent() -> (AgentKind, AgentSessionId) {
        (
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess-1"),
        )
    }

    #[test]
    fn turn_openers_pair_in_order_across_the_hook_lag_and_through_batches() {
        let (_dir, paths) = paths();
        let (kind, agent_id) = agent();
        let base: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
        let at = |seconds: i64| base + SignedDuration::from_secs(seconds);
        let [human_led, other_submission, third_submission] = [(); 3].map(|()| EventId::new());
        let line = |text: &str, seconds: i64, from: Option<&str>, ingress: Option<&EventId>| {
            let mut entry = TranscriptEntry::new(
                at(seconds),
                kind.clone(),
                agent_id.clone(),
                TranscriptKind::Prompt,
                text.to_owned(),
            );
            entry.from = from.map(str::to_owned);
            entry.ingress = ingress.cloned();
            entry
        };
        let rimz = Some(crate::transcript::HARNESS_FROM);
        let mut header_entry = line("read the plan", 2800, Some("@planner"), None);
        header_entry.entry = TranscriptKind::Message;
        let mut other_agent = line("plan the rollout", 3000, None, None);
        other_agent.agent_id = AgentSessionId::from("sess-2");
        let mut other_kind = line("plan the rollout", 3000, None, None);
        other_kind.kind = AgentKind::new_unchecked("codex");
        let mut reply = line("plan the rollout", 3000, None, None);
        reply.entry = TranscriptKind::Assistant;
        for entry in [
            line("continue", 3, None, None),
            line("continue", 7, rimz, None),
            line("reminder", 240, rimz, None),
            line("early", 999, rimz, None),
            line("too early", 1189, rimz, None),
            line("too late", 1701, rimz, None),
            line("first ask", 2001, None, Some(&human_led)),
            line("continue", 2001, rimz, Some(&human_led)),
            line("alpha", 2201, rimz, None),
            line("beta", 2201, rimz, None),
            line("one", 2401, rimz, Some(&other_submission)),
            line("two", 2401, Some("@coder"), Some(&third_submission)),
            line("retry me", 2600, Some("@coder"), None),
            header_entry,
            other_agent,
            other_kind,
            reply,
        ] {
            crate::transcript::append(&paths, &entry).unwrap();
        }
        let retry = format!(
            "retry me\n\n{}",
            super::super::payload::wrap_rimz_block(
                super::super::payload::RimzBlock::PreviousAttemptFailure,
                "A previous attempt at this task failed (exit 1).",
            )
        );
        let turns = [
            (at(0), "continue"),
            (at(4), "continue"),
            (at(100), "reminder"),
            (at(1000), "early"),
            (at(1200), "too early"),
            (at(1400), "too late"),
            (at(2000), "first ask\n\ncontinue"),
            (at(2200), "alpha\n\n\n\nbeta"),
            (at(2400), "one\n\ntwo"),
            (at(2600), retry.as_str()),
            (at(2800), "read the plan"),
            (at(2810), "read the plan"),
            (at(3000), "plan the rollout"),
        ];

        let openers = turn_openers(&paths, &kind, &agent_id, &turns).unwrap();

        assert_eq!(
            openers
                .iter()
                .map(|opener| opener.as_ref().map(|entry| (entry.at, entry.origin())))
                .collect::<Vec<_>>(),
            [
                Some((at(3), EntryOrigin::Human)),
                Some((at(7), EntryOrigin::Harness)),
                Some((at(240), EntryOrigin::Harness)),
                Some((at(999), EntryOrigin::Harness)),
                None,
                None,
                Some((at(2001), EntryOrigin::Human)),
                Some((at(2201), EntryOrigin::Harness)),
                None,
                Some((at(2600), EntryOrigin::Agent)),
                Some((at(2800), EntryOrigin::Agent)),
                None,
                None,
            ]
        );
    }

    #[test]
    fn turn_openers_read_only_the_buckets_the_turns_reach() {
        let (_dir, paths) = paths();
        let (kind, agent_id) = agent();
        let at: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
        let unreadable = |at: Timestamp| {
            let name = crate::disk::buckets::bucket_file_name(
                at,
                crate::disk::retention::TRANSCRIPT_FILE_DAYS,
            );
            std::fs::create_dir_all(paths.transcript_dir.join(name)).unwrap();
        };
        let turns = [(at, "continue")];

        unreadable(at + SignedDuration::from_hours(30 * 24));
        assert!(turn_openers(&paths, &kind, &agent_id, &turns).is_ok_and(|o| o == [None]));
        unreadable(at);
        assert!(turn_openers(&paths, &kind, &agent_id, &turns).is_err());
        assert!(turn_openers(&paths, &kind, &agent_id, &[]).is_ok_and(|o| o.is_empty()));
    }

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
            thread_id: thread_id.map(Into::into),
            is_sidechain: false,
            has_speed: false,
            fast: false,
            model: None,
            rolled: false,
            timing: None,
        }
    }

    #[test]
    fn groups_entries_at_user_boundaries_and_filters_session() {
        let messages = vec![
            message(TranscriptRole::User, 100, "first"),
            message(TranscriptRole::Steering, 105, "keep going"),
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
    fn marks_unanswered_closed_turns_unknown_and_accepts_session_scoped_entries() {
        let messages = vec![
            message(TranscriptRole::User, 100, "unanswered"),
            message(TranscriptRole::User, 200, "done"),
            message(TranscriptRole::Assistant, 210, "answer"),
        ];
        let entries = vec![entry(105, None), entry(205, None)];

        let turns = session_turns(&messages, &entries, "session-a", false);

        assert_eq!(turns[0].outcome, TurnOutcome::Unknown);
        assert_eq!(serde_json::to_value(turns[0].outcome).unwrap(), "unknown");
        assert_eq!(turns[1].outcome, TurnOutcome::Done);
        assert_eq!(turns[0].api_calls, 1);
        assert_eq!(turns[1].api_calls, 1);
    }

    #[test]
    fn fractional_turn_boundaries_match_second_precision_spend() {
        let messages = [
            (TranscriptRole::User, 100_701, "first"),
            (TranscriptRole::Assistant, 100_722, "answer"),
            (TranscriptRole::User, 101_701, "second"),
            (TranscriptRole::Assistant, 101_722, "answer"),
        ]
        .map(|(role, millis, text)| TranscriptMessage {
            role,
            at: Some(Timestamp::from_millisecond(millis).unwrap()),
            text: text.to_owned(),
        });
        let entries = [entry(99, None), entry(100, None), entry(101, None)];
        let turns = session_turns(&messages, &entries, "session-a", false);
        assert_eq!(
            turns[0].fresh_input, 10,
            "the opening second belongs to this turn"
        );
        assert_eq!(
            turns[1].fresh_input, 10,
            "the next boundary must not leak backward"
        );
        assert_eq!(turns[0].started_at, messages[0].at.unwrap());
    }

    #[test]
    fn session_turns_counts_a_repeated_provider_response_once() {
        let messages = vec![
            message(TranscriptRole::User, 100, "first"),
            message(TranscriptRole::Assistant, 110, "answer"),
            message(TranscriptRole::User, 200, "second"),
        ];
        let mut repeated = entry(105, None);
        repeated.message_id = Some("message-1".into());
        repeated.request_id = Some("request-1".into());
        let mut richer = repeated.clone();
        richer.ts_secs = 115;
        richer.cost_usd = 0.5;
        richer.input = 20;
        let mut duplicate = richer.clone();
        duplicate.ts_secs = 120;
        let entries = vec![repeated, richer, duplicate, entry(205, None)];

        let turns = session_turns(&messages, &entries, "session-a", true);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].api_calls, 1);
        assert_eq!(turns[0].fresh_input, 20);
        assert_eq!(turns[0].cost_usd, Some(0.5));
        assert_eq!(turns[0].ended_at, Some(at(115)));
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
