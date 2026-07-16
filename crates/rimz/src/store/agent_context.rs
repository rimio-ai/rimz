//! Latest-wins per-session agent-context sidecar.
//!
//! High-frequency enrichment is written here by the context producers — the
//! statusline, hook ingestion/local transcript refresh, and detached
//! helpers (CLI paths), plus the elder renderer's producer-side triggers (the
//! in-process snapshot-produce backstop and the transcript watcher,
//! `sidebar_pane::app::transcript_watch`) — as one atomic file per
//! `(kind, agent_id)` session under the runtime `agent_context/` dir. The
//! snapshot read-side folds it in through
//! [`crate::store::snapshot::SidebarSnapshot::with_agent_context`]. It never
//! touches the durable event log: this is display-only latency, not truth
//! ("Durability first", `docs/internals/store.md`).
//!
//! Ownership: every renderer's fetch worker reads this module through the
//! shared enrichment fold; writes stay producer-side (CLI paths and the
//! elder's triggers above) and are cache-class — rename atomicity, no fsync,
//! rebuilt from provider state. "Sidebar is read-only on the store" is about
//! durable truth: nothing here reaches the event log, and the
//! `cargo xtask invariants` greps enforce that boundary.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::{AgentContext, AgentTurnError};
use crate::agents::{
    AgentCost, AgentSessionUsage, AgentTokenUsage, LocalContextRefresh, TranscriptStat,
};
use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::store::atomic;
use crate::store::paths::RuntimePaths;
use crate::store::sidecar;

/// A session's context sidecar: the normalized record plus the
/// `(kind, agent_id)` it is filed under, so a read can confirm the key — and
/// shrug off a digest collision — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentContextRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub context: AgentContext,
    /// When app-server/account-scoped context was last observed. Local transcript
    /// pushes bump `context.observed_at`, so app-server throttles use this stamp
    /// instead of the whole-record freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits_observed_at: Option<Timestamp>,
    /// When a rich-context transport last wrote display-only metadata that is
    /// not rate-limit/account data. Local token/cost pushes bump
    /// `context.observed_at`, so rich-context throttles use this stamp instead
    /// of whole-record freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_observed_at: Option<Timestamp>,
    /// Transcript, rollout, or telemetry file used for the latest local context refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Stat gate for [`Self::transcript_path`], letting high-frequency hooks skip
    /// an unchanged tail without parsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stat: Option<TranscriptStat>,
    /// Hook-priced live-session state. Private so only the idempotent merge can
    /// advance the accumulator.
    #[serde(default, skip_serializing_if = "LocallyPricedCostState::is_empty")]
    locally_priced_cost: LocallyPricedCostState,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct LocallyPricedCostState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    cumulative_usd: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    owns_context_cost: bool,
}

impl LocallyPricedCostState {
    fn is_empty(&self) -> bool {
        self.last_turn_id.is_none() && self.cumulative_usd == 0.0 && !self.owns_context_cost
    }
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

impl sidecar::SidecarRecord for AgentContextRecord {
    const FILE_PREFIX: &'static str = "ctx";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
}

/// Persist (latest-wins) one session's context from a CLI producer.
/// Atomic temp+rename (no fsync — disposable sidecar) via
/// [`write_temp_then_rename_cache`].
pub fn write(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    context: &AgentContext,
) -> Result<(), atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let mut context = context.clone();
    let observed_cost = context.cost.is_some();
    // Statusline producers own their observed fields, while lifecycle
    // confirmation owns turn causality. Preserve that independently-written
    // field across whole-context statusline refreshes.
    let prior = read_one_unlocked(runtime, kind, agent_id);
    context.turn_opened_by = prior
        .as_ref()
        .map(|record| record.context.turn_opened_by.clone())
        .unwrap_or_default();
    if context.cost.is_none() {
        context.cost = prior
            .as_ref()
            .and_then(|record| record.context.cost.clone());
    }
    let mut locally_priced_cost = prior
        .map(|record| record.locally_priced_cost)
        .unwrap_or_default();
    if observed_cost {
        locally_priced_cost.owns_context_cost = false;
    }
    write_record_unlocked(
        runtime,
        &AgentContextRecord {
            kind: AgentKind::new_unchecked(kind),
            agent_id: agent_id.into(),
            context,
            rate_limits_observed_at: None,
            rich_observed_at: None,
            transcript_path: None,
            transcript_stat: None,
            locally_priced_cost,
        },
    )
}

/// Persist a fully-shaped sidecar record. Used by merge paths that preserve
/// fields owned by different context producers.
pub fn write_record(
    runtime: &RuntimePaths,
    record: &AgentContextRecord,
) -> Result<(), atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, record.kind.as_str(), record.agent_id.as_str())?;
    let mut record = record.clone();
    let observed_cost =
        record.context.cost.is_some() && !record.locally_priced_cost.owns_context_cost;
    if let Some(prior) = read_one_unlocked(runtime, record.kind.as_str(), record.agent_id.as_str())
    {
        if record.context.cost.is_none() {
            record.context.cost = prior.context.cost;
        }
        if record.locally_priced_cost.is_empty() {
            record.locally_priced_cost = prior.locally_priced_cost;
        }
    }
    if observed_cost {
        record.locally_priced_cost.owns_context_cost = false;
    }
    write_record_unlocked(runtime, &record)
}

fn write_record_unlocked(
    runtime: &RuntimePaths,
    record: &AgentContextRecord,
) -> Result<(), atomic::AtomicErr> {
    sidecar::write_record(&runtime.agent_context_dir, record)
}

/// Read one sidecar directly from disk, bypassing the long-lived parse cache.
/// Writers use this before a read-modify-write so they merge against the latest
/// published bytes, not the last value a sidebar consumer happened to parse.
pub fn read_one(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> Option<AgentContextRecord> {
    read_one_unlocked(runtime, kind, agent_id)
}

fn read_one_unlocked(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
) -> Option<AgentContextRecord> {
    sidecar::read_one(&runtime.agent_context_dir, kind, agent_id)
}

pub fn new_record(kind: &str, agent_id: &str, context: AgentContext) -> AgentContextRecord {
    AgentContextRecord {
        kind: AgentKind::new_unchecked(kind),
        agent_id: agent_id.into(),
        context,
        rate_limits_observed_at: None,
        rich_observed_at: None,
        transcript_path: None,
        transcript_stat: None,
        locally_priced_cost: LocallyPricedCostState::default(),
    }
}

/// Merge transcript-derived local context into a sidecar record. Local refresh
/// owns tokens, cost, model identity/display, observed reasoning effort, and
/// the transcript stat gate; a tail that misses effort preserves the prior value.
pub fn merge_local_context(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    refresh: LocalContextRefresh,
    observed_at: Timestamp,
) -> Result<(), atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let mut record = read_one_unlocked(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    let prior_model_id = record.context.model_id.clone();
    let prior_model_display_name = record.context.model_display_name.clone();
    let prior_context_window = record
        .context
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.context_window_size);
    let prior_tokens = record.context.tokens.clone();
    let refresh_model_id = refresh.model_id.clone();
    let refresh_model_display_name = refresh.model_display_name.clone();
    let droid_model_changed = kind == "droid"
        && (refresh_model_id != prior_model_id
            || refresh_model_display_name != prior_model_display_name);
    record.context.source = kind.to_owned();
    if kind == "droid" {
        // A parsed Droid snapshot owns the effective selector mapping. Replacing
        // with `None` clears stale canonical identity after an unresolved model
        // switch instead of silently pricing the new selector as the old one.
        record.context.model_id = refresh.model_id;
        record.context.model_display_name = refresh.model_display_name;
    } else if refresh.model_id.is_some() {
        record.context.model_id = refresh.model_id;
        if refresh.model_display_name.is_some() {
            record.context.model_display_name = refresh.model_display_name;
        }
    } else if refresh.model_display_name.is_some() {
        record.context.model_display_name = refresh.model_display_name;
    }
    if refresh.session_preview.is_some() {
        record.context.session_preview = refresh.session_preview;
    }
    if refresh.effort.is_some() {
        record.context.effort = refresh.effort;
    }
    let mut refresh_tokens = refresh.tokens;
    if kind == "droid" {
        merge_droid_local_tokens(
            prior_tokens.as_ref(),
            &mut refresh_tokens,
            droid_model_changed,
        );
    } else {
        preserve_established_tokens(prior_tokens.as_ref(), &mut refresh_tokens);
    }
    record.context.tokens = refresh_tokens;
    preserve_cached_context_window(
        kind,
        prior_model_id.as_deref(),
        prior_context_window,
        refresh_model_id.as_deref(),
        record.context.tokens.as_mut(),
    );
    // A missing local cost means the latest transcript tail could not be priced,
    // not that the already-spent session returned to zero.
    if kind == "droid" {
        // Exact pricing is re-derived with every parsed snapshot. An unknown or
        // newly unpriced model clears the earlier local cost.
        record.context.cost = refresh.cost;
    } else if refresh.cost.is_some() {
        record.context.cost = refresh.cost;
        record.locally_priced_cost.owns_context_cost = false;
    }
    // Codex and Cursor re-derive turn death from the transcript tail each
    // refresh: a still-present marker re-stamps identically, and a fresh turn
    // clears it by returning `None`. Other local-refresh users do not run that
    // detector, so they must not clear hook/statusline-owned errors.
    if matches!(kind, "codex" | "cursor") {
        record.context.turn_error = refresh.turn_error;
    }
    // Overwrite each refresh: a clean `task_complete` at the tail sets the
    // marker, and a fresh turn already underway clears it (the detector returns
    // `None`), so a stale completion never outlives its turn.
    record.context.turn_complete = refresh.turn_complete;
    // Plan proposals use the same overwrite/self-clear rule, but settle the
    // row to waiting while Codex displays its client-side selector.
    record.context.plan_proposed = refresh.plan_proposed;
    // Read-only native prompt detectors overwrite on every refresh: a current
    // AskUser/permission record stamps the marker and a newer transcript state
    // clears it without manufacturing durable lifecycle truth.
    record.context.native_permission_wait = refresh.native_permission_wait;
    // Same latest-refresh semantics as `turn_complete`: an at-rest
    // `turn_aborted` stamps the interrupted marker, and newer live rollout
    // records clear it by returning `None`.
    record.context.turn_interrupted = refresh.turn_interrupted;
    record.context.observed_at = observed_at;
    record.transcript_path = refresh.transcript_path;
    record.transcript_stat = refresh.transcript_stat;
    write_record_unlocked(runtime, &record)
}

/// Merge a sparse hook-observed context (Pi/OpenCode envelope fields) onto the
/// session's sidecar record: field-wise latest-wins, rate-limit freshness
/// stamped, cost monotonic (a lower total never overwrites a higher one).
/// Returns whether anything changed and was written.
pub fn merge_observed(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    context: AgentContext,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let observed_at = context.observed_at;
    let mut record = read_one_unlocked(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    let mut changed = false;
    macro_rules! merge_optional {
        ($field:ident) => {
            if let Some(value) = context.$field
                && record.context.$field.as_ref() != Some(&value)
            {
                record.context.$field = Some(value);
                changed = true;
            }
        };
    }
    merge_optional!(session_name);
    merge_optional!(session_preview);
    merge_optional!(model_display_name);
    merge_optional!(thinking_enabled);
    merge_optional!(output_style);
    merge_optional!(vim_mode);
    merge_optional!(agent_version);
    merge_optional!(exceeds_200k_tokens);
    merge_optional!(pr);
    merge_optional!(account);
    merge_optional!(turn_error);
    merge_optional!(turn_complete);
    merge_optional!(turn_interrupted);
    if let Some(rate_limits) = context.rate_limits
        && record.context.rate_limits.as_ref() != Some(&rate_limits)
    {
        record.context.rate_limits = Some(rate_limits);
        record.rate_limits_observed_at = Some(observed_at);
        changed = true;
    }
    if let Some(tokens) = context.tokens {
        changed |= merge_observed_tokens(&mut record.context.tokens, tokens);
    }
    if let Some(model_id) = context.model_id
        && record.context.model_id.as_ref() != Some(&model_id)
    {
        record.context.model_id = Some(model_id);
        changed = true;
    }
    if let Some(effort) = context.effort
        && record.context.effort.as_ref() != Some(&effort)
    {
        record.context.effort = Some(effort);
        changed = true;
    }
    if let Some(cost) = context.cost
        && let Some(total_cost_usd) = cost.total_cost_usd
    {
        let prior_total_cost = record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd);
        if prior_total_cost.is_none_or(|prior| total_cost_usd >= prior) {
            changed |= merge_observed_cost(&mut record.context.cost, cost, total_cost_usd);
            record.locally_priced_cost.owns_context_cost = false;
        }
    }
    if !changed {
        return Ok(false);
    }
    record.context.source = kind.to_owned();
    record.context.observed_at = observed_at;
    write_record_unlocked(runtime, &record)?;
    Ok(true)
}

/// Merge a provider-native turn-error marker into the latest sidecar record.
/// The marker is display-only enrichment and shares the same self-clear rule as
/// transcript-detected turn errors: any newer lifecycle heartbeat moves
/// `last_activity` past `marker.at`.
pub fn merge_turn_error(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    marker: AgentTurnError,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let observed_at = Timestamp::now();
    let mut record = read_one_unlocked(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    if record.context.turn_error.as_ref() == Some(&marker) {
        return Ok(false);
    }
    record.context.source = kind.to_owned();
    record.context.turn_error = Some(marker);
    record.context.observed_at = observed_at;
    write_record_unlocked(runtime, &record)?;
    Ok(true)
}

/// Replace the delivered messages that opened the current turn. An empty set
/// is meaningful: a human-opened turn clears causality from the prior turn.
pub fn merge_turn_opened_by(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    message_ids: Vec<MessageId>,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let observed_at = Timestamp::now();
    let mut record = read_one_unlocked(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    if record.context.turn_opened_by == message_ids {
        return Ok(false);
    }
    record.context.source = kind.to_owned();
    record.context.turn_opened_by = message_ids;
    record.context.observed_at = observed_at;
    write_record_unlocked(runtime, &record)?;
    Ok(true)
}

/// Add one hook-priced turn to the session accumulator. Consecutive duplicate
/// turn ids are ignored because provider hook delivery is ordered per session.
pub fn merge_locally_priced_cost(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    priced: &crate::agents::LocallyPricedTurnCost,
) -> Result<bool, atomic::AtomicErr> {
    if priced.turn_id.trim().is_empty() || !priced.cost_usd.is_finite() || priced.cost_usd <= 0.0 {
        return Ok(false);
    }
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let observed_at = Timestamp::now();
    let mut record = read_one_unlocked(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    if record.locally_priced_cost.last_turn_id.as_deref() == Some(priced.turn_id.as_str()) {
        return Ok(false);
    }
    let cumulative = record.locally_priced_cost.cumulative_usd + priced.cost_usd;
    if !cumulative.is_finite() || cumulative < 0.0 {
        return Ok(false);
    }
    record.locally_priced_cost.last_turn_id = Some(priced.turn_id.clone());
    record.locally_priced_cost.cumulative_usd = cumulative;
    record.context.source = kind.to_owned();
    record.context.observed_at = observed_at;
    if record.locally_priced_cost.owns_context_cost || record.context.cost.is_none() {
        record.locally_priced_cost.owns_context_cost = true;
        let cost = record.context.cost.get_or_insert_with(AgentCost::default);
        cost.total_cost_usd = Some(cumulative);
    }
    write_record_unlocked(runtime, &record)?;
    Ok(true)
}

fn merge_observed_tokens(prior: &mut Option<AgentTokenUsage>, incoming: AgentTokenUsage) -> bool {
    let target = prior.get_or_insert_with(AgentTokenUsage::default);
    let before = target.clone();
    if incoming.context_window_size.is_some() {
        target.context_window_size = incoming.context_window_size;
    }
    if incoming.used_percentage.is_some() {
        target.used_percentage = incoming.used_percentage;
    }
    if incoming.remaining_percentage.is_some() {
        target.remaining_percentage = incoming.remaining_percentage;
    }
    if incoming.current_context_tokens.is_some() {
        target.current_context_tokens = incoming.current_context_tokens;
    }
    if let Some(current_usage) = incoming.current_usage {
        target.current_usage = Some(current_usage);
    }
    merge_session_usage(&mut target.session_usage, incoming.session_usage);
    *target != before
}

fn merge_observed_cost(
    prior: &mut Option<AgentCost>,
    incoming: AgentCost,
    total_cost_usd: f64,
) -> bool {
    let target = prior.get_or_insert_with(AgentCost::default);
    let before = target.clone();
    target.total_cost_usd = Some(total_cost_usd);
    target.coverage = incoming.coverage;
    if incoming.total_duration_ms.is_some() {
        target.total_duration_ms = incoming.total_duration_ms;
    }
    if incoming.total_api_duration_ms.is_some() {
        target.total_api_duration_ms = incoming.total_api_duration_ms;
    }
    if incoming.total_lines_added.is_some() {
        target.total_lines_added = incoming.total_lines_added;
    }
    if incoming.total_lines_removed.is_some() {
        target.total_lines_removed = incoming.total_lines_removed;
    }
    *target != before
}

fn merge_droid_local_tokens(
    prior: Option<&AgentTokenUsage>,
    incoming: &mut Option<AgentTokenUsage>,
    model_changed: bool,
) {
    let Some(prior) = prior else {
        return;
    };
    let Some(incoming) = incoming.as_mut() else {
        let mut preserved = prior.clone();
        preserved.used_percentage = None;
        preserved.remaining_percentage = None;
        preserved.current_context_tokens = None;
        preserved.current_usage = None;
        if model_changed {
            preserved.context_window_size = None;
        }
        *incoming = Some(preserved);
        return;
    };
    if !model_changed && incoming.context_window_size.is_none() {
        incoming.context_window_size = prior.context_window_size;
    }
    merge_session_usage(&mut incoming.session_usage, prior.session_usage.clone());
}

fn merge_session_usage(
    target: &mut Option<AgentSessionUsage>,
    incoming: Option<AgentSessionUsage>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    let target = target.get_or_insert_with(AgentSessionUsage::default);
    target.input_tokens = monotonic_count(target.input_tokens, incoming.input_tokens);
    target.output_tokens = monotonic_count(target.output_tokens, incoming.output_tokens);
    target.cache_creation_input_tokens = monotonic_count(
        target.cache_creation_input_tokens,
        incoming.cache_creation_input_tokens,
    );
    target.cache_read_input_tokens = monotonic_count(
        target.cache_read_input_tokens,
        incoming.cache_read_input_tokens,
    );
    target.thinking_tokens = monotonic_count(target.thinking_tokens, incoming.thinking_tokens);
}

fn monotonic_count(prior: Option<u64>, incoming: Option<u64>) -> Option<u64> {
    match (prior, incoming) {
        (Some(prior), Some(incoming)) => Some(prior.max(incoming)),
        (prior, incoming) => prior.or(incoming),
    }
}

fn preserve_established_tokens(
    prior: Option<&AgentTokenUsage>,
    refresh: &mut Option<AgentTokenUsage>,
) {
    let Some(prior) = prior.filter(|tokens| established_token_usage(tokens)) else {
        return;
    };
    match refresh {
        None => *refresh = Some(prior.clone()),
        Some(tokens) if should_preserve_tokens(tokens) => *tokens = prior.clone(),
        Some(_) => {}
    }
}

fn established_token_usage(tokens: &AgentTokenUsage) -> bool {
    tokens.used_percentage.is_some_and(|pct| pct > 0)
        || tokens
            .current_context_tokens
            .is_some_and(|tokens| tokens > 0)
        || tokens
            .current_usage
            .as_ref()
            .is_some_and(|usage| !usage.is_zero())
}

fn should_preserve_tokens(refresh: &AgentTokenUsage) -> bool {
    inferred_fresh_tokens(refresh)
}

fn inferred_fresh_tokens(tokens: &AgentTokenUsage) -> bool {
    // A fresh rollout tail (no `token_count` event yet) carries an all-zero
    // current usage and no percentage. The zeroed `current_usage` under an
    // absent percentage is the fresh sentinel — recognise it and keep the prior
    // established record rather than overwriting real context with zeros and a
    // default window.
    tokens.used_percentage.is_none()
        && tokens.current_context_tokens.is_none()
        && tokens
            .current_usage
            .as_ref()
            .is_some_and(|usage| usage.is_zero())
}

fn preserve_cached_context_window(
    kind: &str,
    prior_model_id: Option<&str>,
    prior_context_window: Option<u64>,
    refresh_model_id: Option<&str>,
    tokens: Option<&mut crate::agents::AgentTokenUsage>,
) {
    let Some(tokens) = tokens else {
        return;
    };
    let Some(prior_context_window) = prior_context_window else {
        return;
    };
    let Some(default_context_window) = crate::agents::descriptor_by_kind(kind)
        .and_then(|descriptor| descriptor.default_context_window)
    else {
        return;
    };
    if tokens.context_window_size != Some(default_context_window) {
        return;
    }
    if prior_context_window == default_context_window {
        return;
    }
    if refresh_model_id.is_some_and(|model| prior_model_id != Some(model)) {
        return;
    }
    tokens.context_window_size = Some(prior_context_window);
}

pub fn empty_context(source: &str, observed_at: Timestamp) -> AgentContext {
    AgentContext {
        source: source.to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at,
    }
}

struct RecordLock {
    file: File,
}

impl RecordLock {
    fn acquire(
        runtime: &RuntimePaths,
        kind: &str,
        agent_id: &str,
    ) -> Result<Self, atomic::AtomicErr> {
        std::fs::create_dir_all(&runtime.agent_context_dir).map_err(|source| {
            atomic::AtomicErr::Io {
                path: runtime.agent_context_dir.clone(),
                source,
            }
        })?;
        let path = sidecar::lock_path(
            &runtime.agent_context_dir,
            <AgentContextRecord as sidecar::SidecarRecord>::FILE_PREFIX,
            kind,
            agent_id,
        );
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| atomic::AtomicErr::Io {
                path: path.clone(),
                source,
            })?;
        file.lock()
            .map_err(|source| atomic::AtomicErr::Io { path, source })?;
        Ok(Self { file })
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

thread_local! {
    /// Per-thread parse cache. Every update lands via atomic rename of a
    /// freshly-written temp file, so `(mtime, len)` validates content; the
    /// long-lived consumer fetch thread re-reads these sidecars on every
    /// wakeup, and this caps its steady-state cost at one stat per file.
    static CONTEXT_PARSE_CACHE: RefCell<HashMap<PathBuf, sidecar::ParsedSidecar<AgentContextRecord>>> =
        RefCell::new(HashMap::new());
}

/// Read every session's context sidecar. Tolerant: an unreadable or malformed
/// file is skipped, never fatal — enrichment, not correctness. Liveness gating
/// happens at the rollup join.
/// Steady-state cost on a long-lived thread is one stat per file; only a
/// changed file re-reads and re-parses (see [`CONTEXT_PARSE_CACHE`]).
pub fn read_all(runtime: &RuntimePaths) -> Vec<AgentContextRecord> {
    CONTEXT_PARSE_CACHE.with(|cache| sidecar::read_all(&runtime.agent_context_dir, cache))
}

/// Remove a session's sidecar on `SessionEnd` or reap. Best-effort:
/// a missing file is success.
pub fn remove(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> std::io::Result<()> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id).map_err(|error| match error {
        atomic::AtomicErr::Io { source, .. } => source,
        atomic::AtomicErr::Json(source) => std::io::Error::other(source),
    })?;
    sidecar::remove::<AgentContextRecord>(&runtime.agent_context_dir, kind, agent_id)
}

#[cfg(test)]
mod tests;
