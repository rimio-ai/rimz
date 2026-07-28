//! Lifetime effort folded across every session-spend transcript for one
//! logical agent slot.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, TranscriptStat, find_definition};

use super::aggregate::{DedupPayload, SidechainDedup, subagent_child_id};
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlotEffortBreakdown {
    pub total: SlotEffort,
    pub subagents: BTreeMap<String, SlotEffort>,
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

#[derive(Debug, Default)]
pub struct EffortParseMemo {
    files: HashMap<PathBuf, MemoEntry>,
    touched: HashSet<PathBuf>,
}

#[derive(Debug)]
struct MemoEntry {
    stat: Option<TranscriptStat>,
    entries: Vec<CachedEntry>,
}

struct SelectedEntry<'a> {
    entry: &'a CachedEntry,
    subagent: Option<String>,
}

impl DedupPayload for SelectedEntry<'_> {
    fn entry(&self) -> &CachedEntry {
        self.entry
    }
}

/// Fold every transcript file from every continuation of one durable seat.
pub fn slot_effort(sessions: &[EffortSessionRef<'_>], prices: &PriceBook) -> SlotEffort {
    slot_effort_with_memo(sessions, prices, &mut EffortParseMemo::default())
}

pub fn slot_effort_breakdown(
    sessions: &[EffortSessionRef<'_>],
    prices: &PriceBook,
) -> SlotEffortBreakdown {
    slot_effort_breakdown_with_memo(sessions, prices, &mut EffortParseMemo::default())
}

pub fn slot_effort_with_memo(
    sessions: &[EffortSessionRef<'_>],
    prices: &PriceBook,
    memo: &mut EffortParseMemo,
) -> SlotEffort {
    slot_effort_breakdown_with_memo(sessions, prices, memo).total
}

fn slot_effort_breakdown_with_memo(
    sessions: &[EffortSessionRef<'_>],
    prices: &PriceBook,
    memo: &mut EffortParseMemo,
) -> SlotEffortBreakdown {
    let resolved = sessions
        .iter()
        .filter_map(|session| {
            let adapter = find_definition(session.kind)?;
            let prior_path = session
                .transcript_path
                .filter(|path| !path.is_empty())
                .map(Path::new);
            let paths = adapter.session_spend_transcripts(session.session_id, prior_path);
            (!paths.is_empty()).then_some((session.session_id, adapter, paths))
        })
        .collect::<Vec<_>>();

    memo.touched.extend(
        resolved
            .iter()
            .flat_map(|(_, _, paths)| paths.iter().cloned()),
    );
    for (_, adapter, paths) in &resolved {
        for path in paths {
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
    }

    fold_tagged_entries(resolved.iter().flat_map(|(session_id, _, paths)| {
        paths.iter().flat_map(|path| {
            let child_id = subagent_child_id(path);
            memo.files
                .get(path)
                .into_iter()
                .flat_map(|parsed| session_entries(&parsed.entries, session_id))
                .map(move |entry| (entry, child_id.clone()))
        })
    }))
}

impl EffortParseMemo {
    pub(crate) fn retain_touched(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        self.files.retain(|path, _| touched.contains(path));
    }
}

fn fold_tagged_entries<'a>(
    entries: impl IntoIterator<Item = (&'a CachedEntry, Option<String>)>,
) -> SlotEffortBreakdown {
    let mut deduped = SidechainDedup::default();
    for (entry, subagent) in entries {
        deduped.insert(SelectedEntry { entry, subagent });
    }
    let mut breakdown = SlotEffortBreakdown::default();
    for selected in deduped.into_counted() {
        absorb_entry(&mut breakdown.total, selected.entry);
        if let Some(child_id) = selected.subagent {
            absorb_entry(
                breakdown.subagents.entry(child_id).or_default(),
                selected.entry,
            );
        }
    }
    breakdown
}

fn absorb_entry(effort: &mut SlotEffort, entry: &CachedEntry) {
    effort.tokens.absorb_entry(entry);
    effort.cost_usd = sum_optional_cost(
        effort.cost_usd,
        (entry.cost_usd.is_finite() && entry.cost_usd > 0.0).then_some(entry.cost_usd),
    );
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

        let effort =
            fold_tagged_entries([(&first, None), (&replay, None), (&unpriced, None)]).total;

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

        let effort = fold_tagged_entries([(&first, None), (&replay, None)]).total;

        assert_eq!(effort.tokens.input, 12);
        assert_eq!(effort.cost_usd, Some(0.5));
    }

    #[test]
    fn claude_slot_effort_folds_subagent_companions_and_deduplicates_replays() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("session.jsonl");
        let subagents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            &main,
            concat!(
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":1.0,"requestId":"r1","message":{"id":"m1","model":"main-model","usage":{"input_tokens":10,"output_tokens":1}}}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-a.jsonl"),
            concat!(
                r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":99.0,"requestId":"replay","isSidechain":true,"message":{"id":"m1","model":"main-model","usage":{"input_tokens":999,"output_tokens":99}}}"#,
                "\n",
                r#"{"timestamp":"2026-01-01T10:00:02.000Z","costUSD":2.0,"requestId":"r2","isSidechain":true,"message":{"id":"m2","model":"child-model","usage":{"input_tokens":20,"output_tokens":2}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let main = main.to_string_lossy().into_owned();

        let breakdown = slot_effort_breakdown(
            &[EffortSessionRef {
                kind: "claude",
                session_id: "session",
                transcript_path: Some(&main),
            }],
            &PriceBook::default(),
        );
        let effort = breakdown.total;

        assert_eq!(
            effort.tokens,
            EffortTokens {
                input: 30,
                output: 3,
                cache_write: 0,
                cache_read: 0,
            }
        );
        assert_eq!(effort.cost_usd, Some(3.0));
        assert_eq!(
            breakdown.subagents["a"].tokens,
            EffortTokens {
                input: 20,
                output: 2,
                cache_write: 0,
                cache_read: 0,
            }
        );
        assert_eq!(breakdown.subagents["a"].cost_usd, Some(2.0));
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

    #[test]
    fn parse_memo_drops_files_outside_the_current_pass() {
        let mut memo = EffortParseMemo::default();
        let keep = PathBuf::from("keep");
        let drop = PathBuf::from("drop");
        memo.files.insert(
            keep.clone(),
            MemoEntry {
                stat: None,
                entries: Vec::new(),
            },
        );
        memo.files.insert(
            drop,
            MemoEntry {
                stat: None,
                entries: Vec::new(),
            },
        );
        memo.touched.insert(keep.clone());

        memo.retain_touched();

        assert_eq!(memo.files.keys().collect::<Vec<_>>(), [&keep]);
        assert!(memo.touched.is_empty());
    }
}
