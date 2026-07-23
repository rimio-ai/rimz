//! Codex rollout transcript read path for local context refresh.
//!
//! This module locates session JSONL files, reads bounded tails, folds token usage, and enriches context/cost fields without touching the live app-server. Costs use the shared cached price book, so refreshed prices heal post-release models.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{LazyLock, Mutex};

use jiff::Timestamp;
use serde_json::Value;

use super::DEFAULT_CONTEXT_WINDOW;
use super::install::{codex_config_path, read_existing_table};
use super::rollout::{
    CodexTaskComplete, RolloutError, RolloutKind, RolloutRecord, decode_line, read_rollout_header,
};
use super::spend::resume_live_fold;
use crate::agents::context::{
    AgentCost, AgentCurrentUsage, AgentTokenUsage, AgentTurnError, TurnErrorClass, TurnSettle,
    TurnSettleOutcome,
};
use crate::agents::pricing;
use crate::agents::{
    FieldPatch, LocalContextPatch, LocalContextRefresh, LocalSpendFold, LocalTokenPatch,
    ProviderCapacity, SessionOrigin, TranscriptStat, optional_payload_string, read_transcript_tail,
};

/// Refresh Codex's local transcript-derived context for one session, skipping the
/// tail read when the persisted transcript stat still matches.
pub fn refresh_transcript_context(
    session_id: &str,
    model_hint: Option<&str>,
    prior_transcript_path: Option<&str>,
    prior_transcript_stat: Option<&TranscriptStat>,
    prior_spend_fold: Option<&LocalSpendFold>,
    pricing_cache_path: &Path,
) -> Option<LocalContextRefresh> {
    let mut path = prior_transcript_path.map(PathBuf::from);
    let mut stat = path.as_deref().and_then(TranscriptStat::from_path);
    if stat.is_none() {
        path = find_session_transcript(session_id);
        stat = path.as_deref().and_then(TranscriptStat::from_path);
    }
    let path = path?;
    let stat = stat?;
    if prior_transcript_stat.is_some_and(|prior| *prior == stat) {
        return None;
    }

    let prices = pricing::cached_book(pricing_cache_path);
    let prior_spend_fold = (prior_transcript_path.map(Path::new) == Some(path.as_path()))
        .then_some(prior_spend_fold)
        .flatten();
    let spend_fold = resume_live_fold(&path, prior_spend_fold, stat.len, &prices);
    let tail = read_transcript_tail(&path);
    let (usage, outcome, _) = tail
        .as_deref()
        .map(|tail| scan_transcript_tail(tail, TranscriptScanNeed::UsageAndOutcome).into_parts())
        .unwrap_or_default();
    let (settle, turn_error) = match outcome {
        Some(RestingTurnOutcome::Complete(at)) => {
            (Some(TurnSettle::new(at, TurnSettleOutcome::Complete)), None)
        }
        Some(RestingTurnOutcome::PlanProposed(plan)) => (
            Some(TurnSettle::new(plan.at, TurnSettleOutcome::PlanProposed)),
            None,
        ),
        Some(RestingTurnOutcome::Interrupted(at)) => (
            Some(TurnSettle::new(at, TurnSettleOutcome::Interrupted)),
            None,
        ),
        Some(RestingTurnOutcome::Died(error)) => (None, Some(error)),
        None => (None, None),
    };
    let (mut tokens, model_id) = transcript_enrichment(&usage, model_hint);
    if let Some(session_usage) = spend_fold.session_usage() {
        tokens
            .get_or_insert_with(AgentTokenUsage::default)
            .session_usage = Some(session_usage);
    }
    let cost = (spend_fold.total_usd > 0.0).then_some(AgentCost {
        total_cost_usd: Some(spend_fold.total_usd),
        ..AgentCost::default()
    });
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            model_id: model_id.map_or(FieldPatch::Keep, FieldPatch::Set),
            effort: usage.effort.map_or(FieldPatch::Keep, FieldPatch::Set),
            tokens: LocalTokenPatch::PreserveEstablished(tokens),
            cost: cost.map_or(FieldPatch::Keep, FieldPatch::Set),
            turn_error: turn_error.map_or(FieldPatch::Clear, FieldPatch::Set),
            settle: settle.map_or(FieldPatch::Clear, FieldPatch::Set),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        spend_fold: FieldPatch::Set(spend_fold),
    })
}

/// Read a session's origin from the first rollout line. `None` means unknown:
/// the rollout was absent, unreadable, malformed, or did not start with
/// `session_meta`, so callers keep every session.
pub fn session_origin(session_id: &str) -> Option<SessionOrigin> {
    let path = find_session_transcript(session_id)?;
    let header = read_rollout_header(&path)?;
    Some(if header.forked_from_id.is_some() {
        SessionOrigin::Forked
    } else {
        SessionOrigin::Fresh
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
}

pub(super) fn transcript_enrichment(
    usage: &TranscriptUsage,
    model_hint: Option<&str>,
) -> (Option<AgentTokenUsage>, Option<String>) {
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
            current_context_tokens: None,
            current_usage,
            session_usage: None,
        })
    } else {
        None
    };

    let model_id = model_hint
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| usage.model.clone())
        .or_else(configured_model);
    (tokens, model_id)
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
pub(crate) fn with_codex_sessions_root<T>(path: &Path, f: impl FnOnce() -> T) -> T {
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
/// descends the date hierarchy newest-first, then checks the flat sibling
/// `archived_sessions/` directory.
pub(super) fn find_session_transcript(session_id: &str) -> Option<PathBuf> {
    let sessions = codex_sessions_root()?;
    find_session_transcript_under(&sessions, session_id).or_else(|| {
        if sessions.file_name().and_then(|name| name.to_str()) != Some("sessions") {
            return None;
        }
        let archived = sessions.parent()?.join("archived_sessions");
        find_session_transcript_under(&archived, session_id)
    })
}

/// Return the abort time only when this exact rollout is resting on a
/// `turn_aborted` outcome. Later live records clear the evidence.
pub(super) fn resting_interruption(session_id: &str) -> Option<Timestamp> {
    let path = find_session_transcript(session_id)?;
    let tail = read_transcript_tail(&path)?;
    detect_turn_interrupted(&tail)
}

/// Same active-tree or flat-directory walk as [`find_session_transcript`] but
/// rooted at an explicit directory — kept separate so tests can pass a tempdir
/// without setting `HOME` or `RIMZ_CODEX_SESSIONS` in-process. Bounded by a
/// day-directory budget so a hook never stalls on a large active history.
pub(super) fn find_session_transcript_under(root: &Path, session_id: &str) -> Option<PathBuf> {
    const DAY_BUDGET: usize = 16;
    let needle = format!("{session_id}.jsonl");
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|file_type| file_type.is_file())
                && entry.file_name().to_string_lossy().ends_with(&needle)
            {
                return Some(entry.path());
            }
        }
    }
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
/// the current `model_context_window` and `last_token_usage` (gauge). This reads
/// a bounded tail and takes the most recent record. Best-effort: any IO or parse
/// failure yields empty fields (enrichment, never correctness). Test-only: the
/// refresh path reads the tail once for usage and turn-completion together.
#[cfg(test)]
pub(super) fn usage_from_transcript(path: &Path) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(path) else {
        return TranscriptUsage::default();
    };
    scan_transcript_tail(&text, TranscriptScanNeed::UsageOnly).into_usage()
}

/// Cap on provider error text surfaced on the agent card.
const TURN_ERROR_LABEL_MAX: usize = 80;
const MESSAGELESS_TASK_COMPLETE_LABEL: &str = "turn ended with no final message";

pub(super) enum RestingTurnOutcome {
    Complete(Timestamp),
    PlanProposed(PlanProposal),
    Interrupted(Timestamp),
    Died(AgentTurnError),
}

fn non_empty_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlanProposal {
    pub(super) text: String,
    pub(super) at: Timestamp,
}

/// Detect a completed Codex planning turn resting on the client-side
/// "Implement this plan?" selector. The authoritative evidence is a clean
/// `task_complete` and a same-turn `item_completed` whose item is a non-empty
/// `Plan`; `update_plan` tool calls use a different record shape and cannot
/// match. Records after the completion clear the marker through the same live
/// vocabulary as the other resting-turn detectors.
#[cfg(test)]
pub(super) fn detect_plan_proposed(tail: &str) -> Option<PlanProposal> {
    match scan_transcript_tail(tail, TranscriptScanNeed::UsageAndOutcome).into_outcome() {
        Some(RestingTurnOutcome::PlanProposed(plan)) => Some(plan),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn detect_turn_error(tail: &str) -> Option<AgentTurnError> {
    scan_transcript_tail(tail, TranscriptScanNeed::UsageAndOutcome).into_raw_error()
}

fn detect_resting_turn_outcome(tail: &str) -> Option<RestingTurnOutcome> {
    scan_transcript_tail(tail, TranscriptScanNeed::UsageAndOutcome).into_outcome()
}

fn completed_turn_fallback(task: &CodexTaskComplete<'_>, at: Timestamp) -> RestingTurnOutcome {
    if task_complete_has_final_message(task) {
        RestingTurnOutcome::Complete(at)
    } else {
        RestingTurnOutcome::Died(messageless_task_complete(at))
    }
}

fn plan_proposal_from_record(
    record: &RolloutRecord<'_>,
    completed_turn_id: &str,
    at: Timestamp,
) -> Option<PlanProposal> {
    let RolloutKind::ItemCompleted(item) = &record.kind else {
        return None;
    };
    if item.turn_id.as_deref() != Some(completed_turn_id) {
        return None;
    }
    item.plan_text
        .as_deref()
        .and_then(non_empty_text)
        .map(|text| PlanProposal { text, at })
}

fn terminal_outcome_from_record(record: &RolloutRecord<'_>) -> Option<Option<RestingTurnOutcome>> {
    if record.proves_recovery() {
        return Some(None);
    }
    if let Some(error) = turn_error_from_record(record) {
        return Some(Some(RestingTurnOutcome::Died(error)));
    }
    match &record.kind {
        RolloutKind::TurnAborted => Some(
            record
                .event_timestamp()
                .map(RestingTurnOutcome::Interrupted),
        ),
        RolloutKind::TaskComplete(task) if !task.error_field_present => Some(
            record
                .event_timestamp()
                .map(|at| completed_turn_fallback(task, at)),
        ),
        RolloutKind::TaskComplete(_) => Some(None),
        _ => None,
    }
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
        Some(RestingTurnOutcome::PlanProposed(_))
        | Some(RestingTurnOutcome::Interrupted(_))
        | Some(RestingTurnOutcome::Died(_))
        | None => None,
    }
}

/// Detect a turn interrupted without a `Stop` hook from a resting rollout tail.
/// Codex writes `event_msg`/`turn_aborted` for Esc and `/clear` of a running
/// turn. Any abort reason counts; a later live record makes the session no
/// longer "at rest" and clears the marker.
pub(super) fn detect_turn_interrupted(tail: &str) -> Option<Timestamp> {
    match detect_resting_turn_outcome(tail) {
        Some(RestingTurnOutcome::Interrupted(at)) => Some(at),
        Some(RestingTurnOutcome::Complete(_))
        | Some(RestingTurnOutcome::PlanProposed(_))
        | Some(RestingTurnOutcome::Died(_))
        | None => None,
    }
}

pub fn turn_death_needs_pane_confirmation(error: &AgentTurnError) -> bool {
    error.class == TurnErrorClass::Unknown
        && error.label.as_deref() == Some(MESSAGELESS_TASK_COMPLETE_LABEL)
}

pub fn refine_turn_death_from_frame(error: &mut AgentTurnError, frame: &str) {
    if !turn_death_needs_pane_confirmation(error) {
        return;
    }
    let Some(warning) = death_warning_from_frame_scan(frame) else {
        return;
    };
    error.label = Some(warning.label);
    error.class = warning.class;
}

/// Infer a generic messageless Codex turn death from the fused account budget
/// when pane text cannot prove it. [`ProviderCapacity::latest_spent_window_reset`]
/// is the same clock `resume_park` arms against, so the inferred pause class and
/// auto-resume deadline cannot disagree.
pub(crate) fn infer_turn_death_from_spent_window(
    error: &mut AgentTurnError,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) {
    if !turn_death_needs_pane_confirmation(error) {
        return;
    }
    if capacity
        .and_then(|capacity| capacity.latest_spent_window_reset(now))
        .is_some()
    {
        error.class = TurnErrorClass::PausedRateLimit;
        error.label = Some("usage limit inferred (rate-limit window spent)".to_owned());
    }
}

#[cfg(test)]
pub(super) fn death_warning_from_frame(frame: &str) -> Option<String> {
    death_warning_from_frame_scan(frame).map(|warning| warning.label)
}

struct DeathWarning {
    label: String,
    class: TurnErrorClass,
}

fn death_warning_from_frame_scan(frame: &str) -> Option<DeathWarning> {
    let lines: Vec<&str> = frame.lines().collect();
    let prompt_idx = lines.iter().rposition(|line| is_codex_input_prompt(line));
    let search_len = prompt_idx.unwrap_or(lines.len());
    for idx in (0..search_len).rev() {
        let line = trim_frame_line(lines[idx]);
        let banner = line.starts_with('⚠');
        if line.is_empty() || is_codex_input_prompt_text(line) {
            continue;
        }
        let class = TurnErrorClass::classify_label(Some(line));
        if class == TurnErrorClass::Failed && !banner {
            continue;
        }
        let anchor = trim_banner_ornaments(line);
        let mut block = vec![anchor];
        for continuation in &lines[idx + 1..search_len] {
            let continuation = trim_frame_line(continuation);
            if continuation.is_empty()
                || is_codex_input_prompt_text(continuation)
                || continuation
                    .chars()
                    .next()
                    .is_some_and(|ch| !ch.is_alphanumeric())
            {
                break;
            }
            block.push(continuation);
        }
        let label = cap_turn_error_label(&block.join(" "))?;
        return Some(DeathWarning { label, class });
    }
    None
}

fn task_complete_has_final_message(task: &CodexTaskComplete<'_>) -> bool {
    task.last_agent_message
        .as_deref()
        .is_some_and(|message| !message.trim().is_empty())
}

fn messageless_task_complete(at: Timestamp) -> AgentTurnError {
    AgentTurnError {
        class: TurnErrorClass::Unknown,
        at,
        label: Some(MESSAGELESS_TASK_COMPLETE_LABEL.to_owned()),
    }
}

fn turn_error_from_record(record: &RolloutRecord<'_>) -> Option<AgentTurnError> {
    let error = record.error.as_ref()?;
    let at = record.event_timestamp()?;
    let label = turn_error_label(error);
    let class = classify_turn_error(&error.kinds, label.as_deref());
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

fn trim_banner_ornaments(line: &str) -> &str {
    line.trim_start_matches(|ch: char| !ch.is_alphanumeric())
        .trim_start()
}

fn is_codex_input_prompt(line: &str) -> bool {
    is_codex_input_prompt_text(trim_frame_line(line))
}

fn is_codex_input_prompt_text(text: &str) -> bool {
    text.starts_with('›')
}

fn turn_error_label(error: &RolloutError<'_>) -> Option<String> {
    error.label.as_deref().and_then(cap_turn_error_label)
}

fn cap_turn_error_label(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(TURN_ERROR_LABEL_MAX).collect())
}

fn classify_turn_error(kinds: &[std::borrow::Cow<'_, str>], label: Option<&str>) -> TurnErrorClass {
    if let Some(class) = kinds
        .iter()
        .find_map(|kind| class_from_codex_error_kind(kind))
    {
        return class;
    }
    TurnErrorClass::classify_label(label)
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptScanNeed {
    UsageOnly,
    UsageAndOutcome,
}

#[derive(Default)]
enum OutcomeScan {
    #[default]
    Searching,
    SeekingPlan {
        turn_id: String,
        at: Timestamp,
        fallback: Option<RestingTurnOutcome>,
    },
    Resolved(Option<RestingTurnOutcome>),
}

#[derive(Default)]
enum RawErrorScan {
    #[default]
    Searching,
    Resolved(Option<AgentTurnError>),
}

#[derive(Default)]
pub(super) struct TranscriptScan {
    latest_model: Option<String>,
    latest_effort: Option<String>,
    latest_usage: Option<LastUsage>,
    outcome: OutcomeScan,
    raw_error: RawErrorScan,
}

impl TranscriptScan {
    fn usage_complete(&self) -> bool {
        self.latest_model.is_some() && self.latest_usage.is_some()
    }

    fn complete(&self, need: TranscriptScanNeed) -> bool {
        self.usage_complete()
            && (need == TranscriptScanNeed::UsageOnly
                || matches!(self.outcome, OutcomeScan::Resolved(_))
                    && matches!(self.raw_error, RawErrorScan::Resolved(_)))
    }

    pub(super) fn into_usage(self) -> TranscriptUsage {
        match self.latest_usage {
            Some(last) => usage_from_last_record(last, self.latest_model, self.latest_effort),
            None => TranscriptUsage {
                model: self.latest_model,
                effort: self.latest_effort,
                ..TranscriptUsage::fresh()
            },
        }
    }

    pub(super) fn into_outcome(self) -> Option<RestingTurnOutcome> {
        match self.outcome {
            OutcomeScan::Resolved(outcome) => outcome,
            OutcomeScan::SeekingPlan { fallback, .. } => fallback,
            OutcomeScan::Searching => None,
        }
    }

    #[cfg(test)]
    pub(super) fn into_raw_error(self) -> Option<AgentTurnError> {
        match self.raw_error {
            RawErrorScan::Resolved(error) => error,
            RawErrorScan::Searching => None,
        }
    }

    pub(super) fn into_parts(
        mut self,
    ) -> (
        TranscriptUsage,
        Option<RestingTurnOutcome>,
        Option<AgentTurnError>,
    ) {
        let outcome = match std::mem::take(&mut self.outcome) {
            OutcomeScan::Resolved(outcome) => outcome,
            OutcomeScan::SeekingPlan { fallback, .. } => fallback,
            OutcomeScan::Searching => None,
        };
        let raw_error = match std::mem::take(&mut self.raw_error) {
            RawErrorScan::Resolved(error) => error,
            RawErrorScan::Searching => None,
        };
        (self.into_usage(), outcome, raw_error)
    }
}

pub(super) fn scan_transcript_tail(text: &str, need: TranscriptScanNeed) -> TranscriptScan {
    let mut scan = TranscriptScan::default();
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(record) = decode_line(line.as_bytes()) else {
            continue;
        };
        scan_transcript_record(&record, &mut scan, need);
        if scan.complete(need) {
            break;
        }
    }
    scan.outcome = match std::mem::take(&mut scan.outcome) {
        OutcomeScan::SeekingPlan { fallback, .. } => OutcomeScan::Resolved(fallback),
        OutcomeScan::Searching => OutcomeScan::Resolved(None),
        resolved => resolved,
    };
    if matches!(scan.raw_error, RawErrorScan::Searching) {
        scan.raw_error = RawErrorScan::Resolved(None);
    }
    scan
}

fn scan_transcript_record(
    record: &RolloutRecord<'_>,
    scan: &mut TranscriptScan,
    need: TranscriptScanNeed,
) {
    if let RolloutKind::TurnContext(context) = &record.kind {
        if scan.latest_model.is_none() {
            scan.latest_model = context.model().map(ToOwned::to_owned);
        }
        if scan.latest_effort.is_none() {
            scan.latest_effort = context.effort().map(ToOwned::to_owned);
        }
    }
    if scan.latest_usage.is_none()
        && let RolloutKind::TokenCount(token_count) = &record.kind
    {
        scan.latest_usage = last_usage_from_info(token_count.info());
    }
    if need == TranscriptScanNeed::UsageAndOutcome {
        scan_outcome_record(record, &mut scan.outcome);
        scan_raw_error_record(record, &mut scan.raw_error);
    }
}

fn scan_raw_error_record(record: &RolloutRecord<'_>, state: &mut RawErrorScan) {
    if !matches!(state, RawErrorScan::Searching) {
        return;
    }
    if record.proves_recovery() {
        *state = RawErrorScan::Resolved(None);
    } else if let Some(error) = turn_error_from_record(record) {
        *state = RawErrorScan::Resolved(Some(error));
    }
}

fn scan_outcome_record(record: &RolloutRecord<'_>, state: &mut OutcomeScan) {
    match state {
        OutcomeScan::Resolved(_) => {}
        OutcomeScan::Searching => {
            let Some(outcome) = terminal_outcome_from_record(record) else {
                return;
            };
            let RolloutKind::TaskComplete(task) = &record.kind else {
                *state = OutcomeScan::Resolved(outcome);
                return;
            };
            let clean_completion = !task.error_field_present;
            if !clean_completion {
                *state = OutcomeScan::Resolved(outcome);
                return;
            }
            let Some(turn_id) = task.turn_id.as_deref().and_then(non_empty_text) else {
                *state = OutcomeScan::Resolved(outcome);
                return;
            };
            let Some(at) = record.event_timestamp() else {
                *state = OutcomeScan::Resolved(None);
                return;
            };
            *state = OutcomeScan::SeekingPlan {
                turn_id,
                at,
                fallback: outcome,
            };
        }
        OutcomeScan::SeekingPlan {
            turn_id,
            at,
            fallback,
        } => {
            if matches!(record.kind, RolloutKind::TaskComplete(_)) {
                *state = OutcomeScan::Resolved(fallback.take());
                return;
            }
            if let Some(plan) = plan_proposal_from_record(record, turn_id, *at) {
                *state = OutcomeScan::Resolved(Some(RestingTurnOutcome::PlanProposed(plan)));
            }
        }
    }
}
fn last_usage_from_info(info: Option<&super::rollout::CodexUsageInfo<'_>>) -> Option<LastUsage> {
    let window = info.and_then(|info| info.model_context_window).unwrap_or(0);
    let last = info.and_then(|info| info.last_token_usage);
    let input = last
        .filter(|usage| usage.input_reported())
        .map(|usage| usage.input_tokens);
    let total = last
        .filter(|usage| usage.total_reported())
        .map(|usage| usage.total_tokens);
    let cached = last
        .filter(|usage| usage.cached_reported())
        .map(|usage| usage.cached_input_tokens);
    let output = last
        .filter(|usage| usage.output_reported())
        .map(|usage| usage.output_tokens);
    (window > 0 || input.unwrap_or(0) > 0 || total.is_some()).then_some(LastUsage {
        input,
        total,
        window,
        cached,
        output,
    })
}

fn usage_from_last_record(
    last: LastUsage,
    model: Option<String>,
    effort: Option<String>,
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
    }
}
