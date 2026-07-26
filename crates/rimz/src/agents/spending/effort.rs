//! Lifetime effort folded for one logical agent slot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, TranscriptStat, find_definition};

use super::aggregate::{DedupPayload, SidechainDedup};
use super::{CachedEntry, PriceBook, session_entries};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortTokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl EffortTokens {
    pub fn absorb_entry(&mut self, entry: &CachedEntry) {
        self.input = self.input.saturating_add(entry.input);
        self.output = self.output.saturating_add(entry.output);
        self.cache_write = self.cache_write.saturating_add(entry.cache_write);
        self.cache_read = self.cache_read.saturating_add(entry.cache_read);
    }

    pub fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
    }

    pub fn display_total(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SlotEffort {
    pub tokens: EffortTokens,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub struct EffortSessionRef<'a> {
    pub kind: &'a str,
    pub session_id: &'a str,
    pub transcript_path: Option<&'a str>,
}

impl<'a> EffortSessionRef<'a> {
    pub fn from_state(agent: &'a AgentState) -> Self {
        Self {
            kind: agent.kind.as_str(),
            session_id: agent.agent_id.as_str(),
            transcript_path: agent.transcript_path.as_deref(),
        }
    }
}

#[derive(Default)]
pub struct EffortParseMemo {
    files: HashMap<PathBuf, MemoEntry>,
}

struct MemoEntry {
    stat: Option<TranscriptStat>,
    entries: Vec<CachedEntry>,
}

struct SelectedEntry<'a>(&'a CachedEntry);

impl DedupPayload for SelectedEntry<'_> {
    fn entry(&self) -> &CachedEntry {
        self.0
    }
}

pub fn slot_effort(sessions: &[EffortSessionRef<'_>], prices: &PriceBook) -> SlotEffort {
    slot_effort_with_memo(sessions, prices, &mut EffortParseMemo::default())
}

pub fn slot_effort_with_memo(
    sessions: &[EffortSessionRef<'_>],
    prices: &PriceBook,
    memo: &mut EffortParseMemo,
) -> SlotEffort {
    let resolved = sessions
        .iter()
        .filter_map(|session| {
            let adapter = find_definition(session.kind)?;
            let prior_path = session
                .transcript_path
                .filter(|path| !path.is_empty())
                .map(Path::new);
            let path = adapter.session_transcript(session.session_id, prior_path)?;
            Some((session.session_id, adapter, path))
        })
        .collect::<Vec<_>>();

    for (_, adapter, path) in &resolved {
        let stat = TranscriptStat::from_path(path);
        let unchanged = memo.files.get(path).is_some_and(|entry| entry.stat == stat);
        if !unchanged {
            let parsed = adapter.parse_spend(path, None, prices);
            memo.files.insert(
                path.clone(),
                MemoEntry {
                    stat,
                    entries: parsed.entries,
                },
            );
        }
    }

    fold_entries(resolved.iter().flat_map(|(session_id, _, path)| {
        memo.files
            .get(path)
            .into_iter()
            .flat_map(|parsed| session_entries(&parsed.entries, session_id))
    }))
}

fn fold_entries<'a>(entries: impl IntoIterator<Item = &'a CachedEntry>) -> SlotEffort {
    let mut deduped = SidechainDedup::default();
    for entry in entries {
        deduped.insert(SelectedEntry(entry));
    }
    deduped
        .into_counted()
        .into_iter()
        .fold(SlotEffort::default(), |mut effort, entry| {
            effort.tokens.absorb_entry(entry.0);
            effort.cost_usd = sum_optional_cost(
                effort.cost_usd,
                (entry.0.cost_usd.is_finite() && entry.0.cost_usd > 0.0)
                    .then_some(entry.0.cost_usd),
            );
            effort
        })
}

pub fn sum_optional_cost(total: Option<f64>, value: Option<f64>) -> Option<f64> {
    match (total, value) {
        (Some(total), Some(value)) => Some(total + value),
        (Some(total), None) => Some(total),
        (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, input: u64, cost_usd: f64) -> CachedEntry {
        CachedEntry {
            cost_usd,
            input,
            output: 3,
            cache_write: 4,
            cache_read: 5,
            dedup_key: Some(key.to_owned()),
            ..CachedEntry::default()
        }
    }

    #[test]
    fn fold_deduplicates_provider_keys_and_keeps_unpriced_tokens() {
        let first = entry("same", 10, 0.25);
        let replay = first.clone();
        let unpriced = entry("unpriced", 20, 0.0);

        let effort = fold_entries([&first, &replay, &unpriced]);

        assert_eq!(
            effort.tokens,
            EffortTokens {
                input: 30,
                output: 6,
                cache_write: 8,
                cache_read: 10,
            }
        );
        assert_eq!(effort.cost_usd, Some(0.25));
    }

    #[test]
    fn fold_deduplicates_message_replays_across_sessions() {
        let first = CachedEntry {
            message_id: Some("message".to_owned()),
            request_id: Some("request".to_owned()),
            input: 12,
            cost_usd: 0.5,
            ..CachedEntry::default()
        };
        let replay = first.clone();

        let effort = fold_entries([&first, &replay]);

        assert_eq!(effort.tokens.input, 12);
        assert_eq!(effort.cost_usd, Some(0.5));
    }

    #[test]
    fn slot_effort_selects_sessions_from_one_store_and_reuses_its_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
            .unwrap();
        for (id, session_id, input) in [("one", "s1", 10), ("two", "s2", 100)] {
            let data = serde_json::json!({
                "cost": 0.25,
                "modelID": "gpt",
                "providerID": "openai",
                "time": {"created": 1_780_394_400_000_u64},
                "tokens": {
                    "input": input,
                    "output": 2,
                    "cache": {"read": 3, "write": 4}
                }
            })
            .to_string();
            connection
                .execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                    (id, session_id, data),
                )
                .unwrap();
        }
        drop(connection);
        let path = path.to_string_lossy().into_owned();
        let sessions = [
            EffortSessionRef {
                kind: "opencode",
                session_id: "s1",
                transcript_path: Some(&path),
            },
            EffortSessionRef {
                kind: "opencode",
                session_id: "s2",
                transcript_path: Some(&path),
            },
        ];
        let mut memo = EffortParseMemo::default();

        let first = slot_effort_with_memo(&sessions, &PriceBook::default(), &mut memo);
        let second = slot_effort_with_memo(&sessions, &PriceBook::default(), &mut memo);

        assert_eq!(first, second);
        assert_eq!(first.tokens.input, 110);
        assert_eq!(first.cost_usd, Some(0.5));
        assert_eq!(memo.files.len(), 1);
    }

    #[test]
    fn effort_tokens_display_total_and_add_assign_cover_all_components() {
        let mut tokens = EffortTokens {
            input: 1,
            output: 2,
            cache_write: 3,
            cache_read: 4,
        };
        tokens.add_assign(EffortTokens {
            input: 10,
            output: 20,
            cache_write: 30,
            cache_read: 40,
        });

        assert_eq!(tokens.display_total(), 110);
    }
}
