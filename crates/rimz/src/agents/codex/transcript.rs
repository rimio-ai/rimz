//! Codex rollout transcript read path for local context refresh.
//!
//! This module locates session JSONL files, reads bounded tails, folds token usage, and enriches context/cost fields without touching the live app-server.

use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use jiff::Timestamp;
use serde_json::Value;

use super::DEFAULT_CONTEXT_WINDOW;
use super::install::{codex_config_path, read_existing_table};
use crate::agents::context::{
    AgentCost, AgentCurrentUsage, AgentTokenUsage, AgentTurnError, TurnErrorClass,
};
use crate::agents::pricing::PriceBook;
use crate::agents::{
    LocalContextRefresh, SessionOrigin, TranscriptStat, optional_payload_string,
    read_transcript_tail,
};

/// Refresh Codex's local transcript-derived context for one session, skipping the
/// tail read when the persisted transcript stat still matches.
pub fn refresh_transcript_context(
    session_id: &str,
    model_hint: Option<&str>,
    prior_transcript_path: Option<&str>,
    prior_transcript_stat: Option<&TranscriptStat>,
) -> Option<LocalContextRefresh> {
    let mut path = prior_transcript_path.map(PathBuf::from);
    let mut stat = path.as_deref().and_then(transcript_stat);
    if stat.is_none() {
        path = find_session_transcript(session_id);
        stat = path.as_deref().and_then(transcript_stat);
    }
    let path = path?;
    let stat = stat?;
    if prior_transcript_stat.is_some_and(|prior| *prior == stat) {
        return None;
    }

    let tail = read_transcript_tail(&path);
    let usage = tail
        .as_deref()
        .map(usage_from_transcript_tail)
        .unwrap_or_default();
    let outcome = tail.as_deref().and_then(detect_resting_turn_outcome);
    let (turn_complete, turn_error) = match outcome {
        Some(RestingTurnOutcome::Complete(at)) => (Some(at), None),
        Some(RestingTurnOutcome::Died(error)) => (None, Some(error)),
        None => (None, None),
    };
    let (tokens, cost, model_id) = transcript_enrichment(&usage, model_hint);
    Some(LocalContextRefresh {
        model_id,
        effort: usage.effort,
        tokens,
        cost,
        turn_error,
        turn_complete,
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
    })
}

/// Read a session's origin from the first rollout line. `None` means unknown:
/// the rollout was absent, unreadable, malformed, or did not start with
/// `session_meta`, so callers keep every session.
pub fn session_origin(session_id: &str) -> Option<SessionOrigin> {
    let path = find_session_transcript(session_id)?;
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    let value = serde_json::from_str::<Value>(line.trim()).ok()?;
    (value.get("type").and_then(Value::as_str) == Some("session_meta")).then(|| {
        let forked = value
            .get("payload")
            .and_then(|payload| payload.get("forked_from_id"))
            .and_then(Value::as_str)
            .is_some_and(|parent| !parent.is_empty());
        if forked {
            SessionOrigin::Forked
        } else {
            SessionOrigin::Fresh
        }
    })
}

/// Context-window usage derived from a Codex rollout tail.
#[derive(Default)]
pub(super) struct TranscriptUsage {
    /// The model's context window from the rollout's `model_context_window`
    /// (e.g. 258k for GPT-5.5) — the card's window label.
    pub(super) context_window: Option<u64>,
    pub(super) context_window_reported: bool,
    pub(super) total_tokens: Option<u64>,
    pub(super) model: Option<String>,
    /// Session's live reasoning effort from the latest `turn_context`; updates
    /// when the user changes it in the Codex TUI.
    pub(super) effort: Option<String>,
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
    let tokens = if usage.context_window.is_some() || current_usage.is_some() {
        // No baked percentage: the gauge derives it downstream from
        // `current_usage` over `context_window_size`, so the bar can never
        // disagree with the window it is drawn against.
        Some(AgentTokenUsage {
            context_window_size: usage.context_window,
            used_percentage: None,
            remaining_percentage: None,
            current_usage,
        })
    } else {
        None
    };

    let model_id = model_hint
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| usage.model.clone())
        .or_else(configured_model);
    let cost = match (
        usage.cumulative_input_tokens,
        usage.cumulative_output_tokens,
        model_id.as_deref(),
    ) {
        (Some(total_input), Some(total_output), Some(model_id)) => {
            let price_book = PriceBook::embedded();
            price_book.price(model_id).and_then(|price| {
                let uncached = total_input.saturating_sub(usage.cumulative_cached_tokens);
                let cost = price.cost(
                    uncached,
                    total_output,
                    0,
                    0,
                    usage.cumulative_cached_tokens,
                    false,
                );
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
    configured_config_path().and_then(|path| configured_reasoning_effort_at(&path))
}

pub(super) fn configured_model() -> Option<String> {
    configured_config_path().and_then(|path| configured_model_at(&path))
}

#[cfg(not(test))]
fn configured_config_path() -> Option<PathBuf> {
    codex_config_path().ok()
}

#[cfg(test)]
fn configured_config_path() -> Option<PathBuf> {
    if let Some(path) = TEST_CODEX_CONFIG
        .lock()
        .expect("test config mutex is not poisoned")
        .clone()
    {
        return Some(path);
    }
    std::env::var_os("RIMZ_CODEX_CONFIG")?;
    codex_config_path().ok()
}

#[cfg(test)]
static TEST_CODEX_CONFIG: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
static TEST_CODEX_SESSIONS_ROOT: LazyLock<Mutex<Option<PathBuf>>> =
    LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
pub(super) fn with_codex_config_path<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    struct Guard {
        prior: Option<PathBuf>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            *TEST_CODEX_CONFIG
                .lock()
                .expect("test config mutex is not poisoned") = self.prior.take();
        }
    }

    let prior = TEST_CODEX_CONFIG
        .lock()
        .expect("test config mutex is not poisoned")
        .replace(path.to_path_buf());
    let _guard = Guard { prior };
    f()
}

#[cfg(test)]
pub(super) fn with_codex_sessions_root<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    struct Guard {
        prior: Option<PathBuf>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            *TEST_CODEX_SESSIONS_ROOT
                .lock()
                .expect("test sessions mutex is not poisoned") = self.prior.take();
        }
    }

    let prior = TEST_CODEX_SESSIONS_ROOT
        .lock()
        .expect("test sessions mutex is not poisoned")
        .replace(path.to_path_buf());
    let _guard = Guard { prior };
    f()
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
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            context_window_reported: false,
            total_tokens: Some(0),
            model: None,
            effort: None,
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
    #[cfg(test)]
    if let Some(path) = TEST_CODEX_SESSIONS_ROOT
        .lock()
        .expect("test sessions mutex is not poisoned")
        .clone()
    {
        return Some(path);
    }
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
/// yields empty fields (enrichment, never correctness). Test-only: the refresh
/// path reads the tail once for usage and turn-completion together.
#[cfg(test)]
pub(super) fn usage_from_transcript(path: &Path) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(path) else {
        return TranscriptUsage::default();
    };
    usage_from_transcript_tail(&text)
}

pub(super) fn usage_from_transcript_tail(text: &str) -> TranscriptUsage {
    scan_transcript_tail(text).into_usage()
}

/// Cap on provider error text surfaced on the agent card.
const TURN_ERROR_LABEL_MAX: usize = 80;
const MESSAGELESS_TASK_COMPLETE_LABEL: &str = "turn ended with no final message";

enum RestingTurnOutcome {
    Complete(Timestamp),
    Died(AgentTurnError),
}

pub(super) fn detect_turn_error(tail: &str) -> Option<AgentTurnError> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if transcript_record_proves_recovery(&value) {
            return None;
        }
        if let Some(error) = turn_error_from_record(&value) {
            return Some(error);
        }
    }
    None
}

/// Detect a cleanly-completed Codex turn from the rollout tail. Codex closes a
/// turn — including a `/review` turn that runs in review mode and fires no
/// `Stop` hook — with an `event_msg`/`task_complete` payload. Returns that
/// record's timestamp only when the session is at rest on a no-error completion
/// whose `last_agent_message` is a non-empty string. A later
/// `user_message`/`task_started` (a fresh turn already underway), an errored
/// `task_complete` (owned by [`detect_turn_error`]), an ambiguous empty `error`
/// (`null`/`false`/`""`/`{}`), or a message-less clean-looking completion yields
/// `None`. The display-only success sibling of [`detect_turn_error`]; the
/// projection compares the timestamp against the row's `last_activity`, so a
/// stale completion never reclassifies fresh work.
#[cfg(test)]
pub(super) fn detect_turn_complete(tail: &str) -> Option<Timestamp> {
    match detect_resting_turn_outcome(tail) {
        Some(RestingTurnOutcome::Complete(at)) => Some(at),
        Some(RestingTurnOutcome::Died(_)) | None => None,
    }
}

fn detect_resting_turn_outcome(tail: &str) -> Option<RestingTurnOutcome> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if transcript_record_proves_recovery(&value) {
            return None;
        }
        if let Some(error) = turn_error_from_record(&value) {
            return Some(RestingTurnOutcome::Died(error));
        }
        let Some(payload) = event_msg_payload(&value) else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("task_complete") if payload.get("error").is_none() => {
                let at = record_timestamp(&value)?;
                if task_complete_has_final_message(payload) {
                    return Some(RestingTurnOutcome::Complete(at));
                }
                return Some(RestingTurnOutcome::Died(messageless_task_complete(at)));
            }
            Some("task_complete") => return None,
            _ => continue,
        }
    }
    None
}

pub fn turn_death_needs_pane_confirmation(error: &AgentTurnError) -> bool {
    error.class == TurnErrorClass::Unknown
        && error.label.as_deref() == Some(MESSAGELESS_TASK_COMPLETE_LABEL)
}

pub fn refine_turn_death_from_frame(error: &mut AgentTurnError, frame: &str) {
    if !turn_death_needs_pane_confirmation(error) {
        return;
    }
    let Some(label) = death_warning_from_frame(frame) else {
        return;
    };
    error.label = Some(label);
    let class = TurnErrorClass::classify_label(error.label.as_deref());
    if class != TurnErrorClass::Failed {
        error.class = class;
    }
}

pub fn death_warning_from_frame(frame: &str) -> Option<String> {
    let lines: Vec<&str> = frame.lines().collect();
    let prompt_idx = lines.iter().rposition(|line| is_codex_input_prompt(line));
    let search_len = prompt_idx.unwrap_or(lines.len());
    for idx in (0..search_len).rev() {
        let line = trim_frame_line(lines[idx]);
        let Some(first) = line.strip_prefix('⚠') else {
            continue;
        };
        let mut block = vec![first.trim()];
        for continuation in &lines[idx + 1..search_len] {
            let continuation = trim_frame_line(continuation);
            if continuation.is_empty()
                || continuation.starts_with('⚠')
                || is_codex_input_prompt_text(continuation)
            {
                break;
            }
            block.push(continuation);
        }
        return cap_turn_error_label(&block.join(" "));
    }
    None
}

fn task_complete_has_final_message(payload: &Value) -> bool {
    payload
        .get("last_agent_message")
        .and_then(Value::as_str)
        .is_some_and(|message| !message.trim().is_empty())
}

fn messageless_task_complete(at: Timestamp) -> AgentTurnError {
    AgentTurnError {
        class: TurnErrorClass::Unknown,
        at,
        label: Some(MESSAGELESS_TASK_COMPLETE_LABEL.to_owned()),
    }
}

fn turn_error_from_record(value: &Value) -> Option<AgentTurnError> {
    let payload = error_payload(value)?;
    let at = record_timestamp(value)?;
    let label = turn_error_label(payload);
    let class = classify_turn_error(codex_error_info(payload), label.as_deref());
    Some(AgentTurnError { class, at, label })
}

fn trim_frame_line(line: &str) -> &str {
    let mut text = line.trim();
    if let Some(stripped) = text.strip_prefix('│') {
        text = stripped.trim_start();
    }
    if let Some(stripped) = text.strip_suffix('│') {
        text = stripped.trim_end();
    }
    text
}

fn is_codex_input_prompt(line: &str) -> bool {
    is_codex_input_prompt_text(trim_frame_line(line))
}

fn is_codex_input_prompt_text(text: &str) -> bool {
    text.starts_with('›')
}

fn transcript_record_proves_recovery(value: &Value) -> bool {
    if value.get("type").and_then(Value::as_str) == Some("turn_context") {
        return true;
    }
    let Some(payload) = event_msg_payload(value) else {
        return false;
    };
    matches!(
        payload.get("type").and_then(Value::as_str),
        Some("agent_message" | "task_started" | "user_message")
    )
}

fn error_payload(value: &Value) -> Option<&Value> {
    if let Some(payload) = event_msg_payload(value) {
        return error_payload_from_event_payload(payload);
    }
    schema_error_payload(value)
}

fn error_payload_from_event_payload(payload: &Value) -> Option<&Value> {
    match payload.get("type").and_then(Value::as_str) {
        Some("stream_error" | "turn_error" | "error") => Some(payload),
        Some("task_complete") if has_task_complete_error(payload) => Some(payload),
        _ => schema_error_payload(payload),
    }
}

fn schema_error_payload(payload: &Value) -> Option<&Value> {
    payload.get("error").filter(|error| {
        error.get("message").and_then(Value::as_str).is_some()
            || error
                .get("codexErrorInfo")
                .or_else(|| error.get("codex_error_info"))
                .is_some()
    })
}

fn has_task_complete_error(payload: &Value) -> bool {
    match payload.get("error") {
        Some(Value::Null) | None => false,
        Some(Value::Bool(false)) => false,
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Object(object)) => !object.is_empty(),
        Some(_) => true,
    }
}

fn event_msg_payload(value: &Value) -> Option<&Value> {
    (value.get("type").and_then(Value::as_str) == Some("event_msg"))
        .then(|| value.get("payload"))
        .flatten()
}

fn record_timestamp(value: &Value) -> Option<Timestamp> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| timestamp.parse::<Timestamp>().ok())
}

fn turn_error_label(payload: &Value) -> Option<String> {
    let text = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("error_message").and_then(Value::as_str))
        .or_else(|| {
            payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| payload.get("error").and_then(Value::as_str))
        .or_else(|| payload.get("last_agent_message").and_then(Value::as_str))?;
    cap_turn_error_label(text)
}

fn cap_turn_error_label(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(TURN_ERROR_LABEL_MAX).collect())
}

fn codex_error_info(payload: &Value) -> Option<&Value> {
    payload
        .get("codexErrorInfo")
        .or_else(|| payload.get("codex_error_info"))
        .or_else(|| {
            payload
                .get("error")
                .and_then(|error| error.get("codexErrorInfo"))
        })
        .or_else(|| {
            payload
                .get("error")
                .and_then(|error| error.get("codex_error_info"))
        })
}

fn classify_turn_error(info: Option<&Value>, label: Option<&str>) -> TurnErrorClass {
    if let Some(class) = info.and_then(class_from_codex_error_info) {
        return class;
    }
    TurnErrorClass::classify_label(label)
}

fn class_from_codex_error_info(info: &Value) -> Option<TurnErrorClass> {
    if let Some(kind) = info.as_str() {
        return class_from_codex_error_kind(kind);
    }
    let object = info.as_object()?;
    object
        .keys()
        .find_map(|kind| class_from_codex_error_kind(kind))
}

fn class_from_codex_error_kind(kind: &str) -> Option<TurnErrorClass> {
    match kind {
        "usageLimitExceeded" => Some(TurnErrorClass::PausedRateLimit),
        "serverOverloaded" | "internalServerError" => Some(TurnErrorClass::PausedOverloaded),
        "contextWindowExceeded"
        | "unauthorized"
        | "badRequest"
        | "sandboxError"
        | "cyberPolicy"
        | "threadRollbackFailed"
        | "other" => Some(TurnErrorClass::Failed),
        _ => None,
    }
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
    latest_effort: Option<String>,
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
                self.latest_effort,
                cumulative_input_tokens,
                cumulative_cached_tokens,
                cumulative_output_tokens,
            ),
            None => TranscriptUsage {
                model: self.latest_model,
                effort: self.latest_effort,
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
    if scan.latest_effort.is_none() {
        scan.latest_effort = turn_context_effort(value);
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

fn turn_context_effort(value: &Value) -> Option<String> {
    (value.get("type").and_then(Value::as_str) == Some("turn_context"))
        .then(|| value.get("payload").and_then(payload_reasoning_effort))
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
    effort: Option<String>,
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
    TranscriptUsage {
        context_window: Some(context_window),
        context_window_reported,
        total_tokens: last.total,
        model,
        effort,
        last_input_tokens: last.input,
        last_cached_input_tokens: last.cached,
        last_output_tokens: last.output,
        cumulative_input_tokens,
        cumulative_cached_tokens,
        cumulative_output_tokens,
    }
}
