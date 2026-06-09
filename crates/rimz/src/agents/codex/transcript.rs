//! Codex rollout transcript read path for local context refresh.
//!
//! This module locates session JSONL files, reads bounded tails, folds token usage, and enriches context/cost fields without touching the live app-server.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::DEFAULT_CONTEXT_WINDOW;
use super::install::{codex_config_path, read_existing_table};
use crate::agents::context::{AgentCost, AgentCurrentUsage, AgentTokenUsage};
use crate::agents::pricing::PriceBook;
use crate::agents::{
    LocalContextRefresh, TranscriptStat, optional_payload_string, read_transcript_tail,
};

/// Refresh Codex's local transcript-derived context for one session, skipping the
/// tail read when the persisted transcript stat still matches.
pub fn refresh_transcript_context(
    session_id: &str,
    model_hint: Option<&str>,
    prior_effort: Option<&str>,
    prior_transcript_path: Option<&str>,
    prior_transcript_stat: Option<&TranscriptStat>,
) -> Option<LocalContextRefresh> {
    let effort = configured_reasoning_effort();
    let mut path = prior_transcript_path.map(PathBuf::from);
    let mut stat = path.as_deref().and_then(transcript_stat);
    if stat.is_none() {
        path = find_session_transcript(session_id);
        stat = path.as_deref().and_then(transcript_stat);
    }
    let path = path?;
    let stat = stat?;
    if prior_transcript_stat.is_some_and(|prior| *prior == stat)
        && prior_effort == effort.as_deref()
    {
        return None;
    }

    let usage = usage_from_transcript(&path);
    let (tokens, cost, model_id) = transcript_enrichment(&usage, model_hint);
    Some(LocalContextRefresh {
        model_id,
        effort,
        tokens,
        cost,
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
    })
}

/// Context-window usage derived from a Codex rollout tail.
#[derive(Default)]
pub(super) struct TranscriptUsage {
    pub(super) context_pct: Option<u8>,
    /// The model's context window from the rollout's `model_context_window`
    /// (e.g. 258k for GPT-5.5) — the card's window label.
    pub(super) context_window: Option<u64>,
    pub(super) context_window_reported: bool,
    pub(super) total_tokens: Option<u64>,
    pub(super) model: Option<String>,
    /// The latest call's full input from `last_token_usage.input_tokens` —
    /// the cached slice included, so this is the window numerator the
    /// composition line splits.
    pub(super) last_input_tokens: Option<u64>,
    /// The cached slice of the latest call's input from
    /// `last_token_usage.cached_input_tokens` — the card's `◌` cache-read
    /// figure. The protocol has no per-call cache-write.
    pub(super) last_cached_input_tokens: Option<u64>,
    /// The latest call's output from `last_token_usage.output_tokens`.
    pub(super) last_output_tokens: Option<u64>,
    /// Cumulative session input tokens from the most-recent `total_token_usage`
    /// block — the billable input total, used to estimate the session cost.
    pub(super) cumulative_input_tokens: Option<u64>,
    /// Cumulative cached input tokens from `total_token_usage`.
    pub(super) cumulative_cached_tokens: u64,
    /// Cumulative output tokens from `total_token_usage`.
    pub(super) cumulative_output_tokens: Option<u64>,
}

pub(super) fn transcript_enrichment(
    usage: &TranscriptUsage,
    model_hint: Option<&str>,
) -> (Option<AgentTokenUsage>, Option<AgentCost>, Option<String>) {
    let current_usage = if usage.last_input_tokens.is_some()
        || usage.last_cached_input_tokens.is_some()
        || usage.last_output_tokens.is_some()
    {
        Some(AgentCurrentUsage {
            input_tokens: usage
                .last_input_tokens
                .map(|input| input.saturating_sub(usage.last_cached_input_tokens.unwrap_or(0))),
            output_tokens: usage.last_output_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: usage.last_cached_input_tokens,
        })
    } else {
        None
    };
    let tokens =
        if usage.context_window.is_some() || usage.context_pct.is_some() || current_usage.is_some()
        {
            Some(AgentTokenUsage {
                context_window_size: usage.context_window,
                used_percentage: usage.context_pct,
                remaining_percentage: usage.context_pct.map(|pct| 100u8.saturating_sub(pct)),
                current_usage,
            })
        } else {
            None
        };

    let model_id = model_hint
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| usage.model.clone());
    let cost = match (
        usage.cumulative_input_tokens,
        usage.cumulative_output_tokens,
        model_id.as_deref(),
    ) {
        (Some(total_input), Some(total_output), Some(model_id)) => {
            let price_book = PriceBook::embedded();
            price_book.price(model_id).and_then(|price| {
                let uncached = total_input.saturating_sub(usage.cumulative_cached_tokens);
                let cost = uncached as f64 * price.input
                    + usage.cumulative_cached_tokens as f64 * price.cache_read
                    + total_output as f64 * price.output;
                (cost > 0.0).then_some(AgentCost {
                    total_cost_usd: Some(cost),
                    ..AgentCost::default()
                })
            })
        }
        _ => None,
    };
    (tokens, cost, model_id)
}

pub(super) fn transcript_stat(path: &Path) -> Option<TranscriptStat> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(TranscriptStat {
        mtime_secs: since_epoch.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: since_epoch.subsec_nanos(),
        len: meta.len(),
    })
}

pub(super) fn payload_reasoning_effort(payload: &Value) -> Option<String> {
    optional_payload_string(
        payload,
        &["model_reasoning_effort", "reasoning_effort", "effort"],
    )
}

pub(super) fn configured_reasoning_effort() -> Option<String> {
    #[cfg(test)]
    std::env::var_os("RIMZ_CODEX_CONFIG")?;
    codex_config_path()
        .ok()
        .and_then(|path| configured_reasoning_effort_at(&path))
}

pub(super) fn configured_model() -> Option<String> {
    #[cfg(test)]
    std::env::var_os("RIMZ_CODEX_CONFIG")?;
    codex_config_path()
        .ok()
        .and_then(|path| configured_model_at(&path))
}

pub(super) fn configured_model_at(path: &Path) -> Option<String> {
    read_existing_table(path).ok().and_then(|root| {
        root.get("model")
            .and_then(toml::Value::as_str)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned)
    })
}

pub(super) fn configured_reasoning_effort_at(path: &Path) -> Option<String> {
    read_existing_table(path).ok().and_then(|root| {
        root.get("model_reasoning_effort")
            .and_then(toml::Value::as_str)
            .filter(|effort| !effort.is_empty())
            .map(ToOwned::to_owned)
    })
}

impl TranscriptUsage {
    /// A rollout that opened cleanly but carries no `token_count` event yet —
    /// a brand-new session. Report an explicit zero so the gauge draws an
    /// empty bar at 0% instead of vanishing until the first turn completes. A
    /// rollout that cannot be read stays `default()` (all `None`): unknown,
    /// not zero. Mirrors the Claude adapter's `fresh()` semantics.
    fn fresh() -> Self {
        Self {
            context_pct: Some(0),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            context_window_reported: false,
            total_tokens: Some(0),
            model: None,
            last_input_tokens: Some(0),
            last_cached_input_tokens: Some(0),
            last_output_tokens: Some(0),
            cumulative_input_tokens: None,
            cumulative_cached_tokens: 0,
            cumulative_output_tokens: None,
        }
    }

    pub(super) fn reported_context_window(&self) -> Option<u64> {
        if self.context_window_reported {
            self.context_window
        } else {
            None
        }
    }
}

/// Root directory holding Codex rollout JSONL files. Honours
/// `RIMZ_CODEX_SESSIONS` so tests can point at a tempdir without touching the
/// real `~/.codex/sessions/` tree.
fn codex_sessions_root() -> Option<PathBuf> {
    if let Some(raw) = env::var_os("RIMZ_CODEX_SESSIONS").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(raw));
    }
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".codex").join("sessions"))
}

/// Locate the rollout JSONL for a Codex session by its `session_id`. Codex
/// writes one file per session at
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*-{session_id}.jsonl`, so the walk
/// descends the date hierarchy newest-first and stops at the first match.
pub(super) fn find_session_transcript(session_id: &str) -> Option<PathBuf> {
    find_session_transcript_under(&codex_sessions_root()?, session_id)
}

/// Same walk as [`find_session_transcript`] but rooted at an explicit
/// directory — kept separate so tests can pass a tempdir without setting
/// `HOME` or `RIMZ_CODEX_SESSIONS` in-process. Bounded by a day-directory
/// budget so a hook never stalls on a large archive.
pub(super) fn find_session_transcript_under(root: &Path, session_id: &str) -> Option<PathBuf> {
    const DAY_BUDGET: usize = 16;
    let needle = format!("{session_id}.jsonl");
    let mut budget = DAY_BUDGET;
    for year in sorted_subdirs_desc(root) {
        for month in sorted_subdirs_desc(&year) {
            for day in sorted_subdirs_desc(&month) {
                if budget == 0 {
                    return None;
                }
                budget -= 1;
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().ends_with(&needle) {
                        return Some(entry.path());
                    }
                }
            }
        }
    }
    None
}

fn sorted_subdirs_desc(path: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.reverse();
    entries
}

/// Derive context-window usage from the tail of a Codex rollout JSONL. Codex
/// emits an `event_msg`/`token_count` payload after every assistant turn with
/// the current `model_context_window`, `last_token_usage` (gauge), and
/// `total_token_usage` (cumulative billing totals). This reads a bounded tail
/// and takes the most recent record. Best-effort: any IO or parse failure
/// yields empty fields (enrichment, never correctness).
pub(super) fn usage_from_transcript(path: &Path) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(path) else {
        return TranscriptUsage::default();
    };
    scan_transcript_tail(&text).into_usage()
}

struct LastUsage {
    input: Option<u64>,
    total: Option<u64>,
    window: u64,
    cached: Option<u64>,
    output: Option<u64>,
}

#[derive(Default)]
struct TranscriptScan {
    latest_model: Option<String>,
    latest_usage: Option<LastUsage>,
    latest_cumulative: Option<(u64, u64, u64)>,
}

impl TranscriptScan {
    fn complete(&self) -> bool {
        self.latest_model.is_some()
            && self.latest_usage.is_some()
            && self.latest_cumulative.is_some()
    }

    fn into_usage(self) -> TranscriptUsage {
        let (cumulative_input_tokens, cumulative_cached_tokens, cumulative_output_tokens) =
            match self.latest_cumulative {
                Some((i, c, o)) => (Some(i), c, Some(o)),
                None => (None, 0, None),
            };
        match self.latest_usage {
            Some(last) => usage_from_last_record(
                last,
                self.latest_model,
                cumulative_input_tokens,
                cumulative_cached_tokens,
                cumulative_output_tokens,
            ),
            None => TranscriptUsage {
                model: self.latest_model,
                cumulative_input_tokens,
                cumulative_cached_tokens,
                cumulative_output_tokens,
                ..TranscriptUsage::fresh()
            },
        }
    }
}

fn scan_transcript_tail(text: &str) -> TranscriptScan {
    let mut scan = TranscriptScan::default();
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        scan_transcript_record(&value, &mut scan);
        if scan.complete() {
            break;
        }
    }
    scan
}

fn scan_transcript_record(value: &Value, scan: &mut TranscriptScan) {
    if scan.latest_model.is_none() {
        scan.latest_model = turn_context_model(value);
    }
    if scan.latest_usage.is_some() && scan.latest_cumulative.is_some() {
        return;
    }
    let Some(payload) = value.get("payload") else {
        return;
    };
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return;
    }
    let info = payload.get("info");
    if scan.latest_usage.is_none() {
        scan.latest_usage = last_usage_from_info(info);
    }
    if scan.latest_cumulative.is_none() {
        scan.latest_cumulative = cumulative_usage_from_info(info);
    }
}

fn turn_context_model(value: &Value) -> Option<String> {
    (value.get("type").and_then(Value::as_str) == Some("turn_context"))
        .then(|| {
            value
                .get("payload")
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .map(ToOwned::to_owned)
        })
        .flatten()
}

fn last_usage_from_info(info: Option<&Value>) -> Option<LastUsage> {
    let window = info
        .and_then(|info| info.get("model_context_window"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let last = info.and_then(|info| info.get("last_token_usage"));
    let input = last
        .and_then(|last| last.get("input_tokens"))
        .and_then(Value::as_u64);
    let total = last
        .and_then(|last| last.get("total_tokens"))
        .and_then(Value::as_u64);
    let cached = last
        .and_then(|last| last.get("cached_input_tokens"))
        .and_then(Value::as_u64);
    let output = last
        .and_then(|last| last.get("output_tokens"))
        .and_then(Value::as_u64);
    (window > 0 || input.unwrap_or(0) > 0 || total.is_some()).then_some(LastUsage {
        input,
        total,
        window,
        cached,
        output,
    })
}

fn cumulative_usage_from_info(info: Option<&Value>) -> Option<(u64, u64, u64)> {
    let total_usage = info.and_then(|info| info.get("total_token_usage"));
    let cum_input = total_usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(Value::as_u64);
    let cum_output = total_usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_u64);
    let cum_cached = total_usage
        .and_then(|u| {
            u.get("cached_input_tokens")
                .or_else(|| u.get("cache_read_input_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    match (cum_input, cum_output) {
        (Some(i), Some(o)) => Some((i, cum_cached, o)),
        _ => None,
    }
}

fn usage_from_last_record(
    last: LastUsage,
    model: Option<String>,
    cumulative_input_tokens: Option<u64>,
    cumulative_cached_tokens: u64,
    cumulative_output_tokens: Option<u64>,
) -> TranscriptUsage {
    let context_window_reported = last.window > 0;
    let context_window = if context_window_reported {
        last.window
    } else {
        DEFAULT_CONTEXT_WINDOW
    };
    let context_pct = last
        .input
        .unwrap_or(0)
        .saturating_mul(100)
        .checked_div(context_window)
        .map(|pct| pct.min(100) as u8);
    TranscriptUsage {
        context_pct,
        context_window: Some(context_window),
        context_window_reported,
        total_tokens: last.total,
        model,
        last_input_tokens: last.input,
        last_cached_input_tokens: last.cached,
        last_output_tokens: last.output,
        cumulative_input_tokens,
        cumulative_cached_tokens,
        cumulative_output_tokens,
    }
}
