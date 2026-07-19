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
    AgentCost, AgentSpec, AgentTokenUsage, LocalContextRefresh, LocalSpendFold, TranscriptStat,
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
    /// Resumable per-request pricing state for the local transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_fold: Option<LocalSpendFold>,
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
    let observed_at = context.observed_at;
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        let mut context = context.clone();
        let observed_cost = context.cost.is_some();
        // Statusline producers own their observed fields, while lifecycle
        // confirmation owns turn causality. Preserve that independently-written
        // field across whole-context statusline refreshes.
        context.turn_opened_by = record.context.turn_opened_by.clone();
        if context.cost.is_none() {
            context.cost.clone_from(&record.context.cost);
        }
        let mut locally_priced_cost = record.locally_priced_cost.clone();
        if observed_cost {
            locally_priced_cost.owns_context_cost = false;
        }
        *record = AgentContextRecord {
            kind: AgentKind::new_unchecked(kind),
            agent_id: agent_id.into(),
            context,
            rate_limits_observed_at: None,
            rich_observed_at: None,
            transcript_path: None,
            transcript_stat: None,
            spend_fold: record.spend_fold.clone(),
            locally_priced_cost,
        };
        true
    })
    .map(|_| ())
}

/// Persist a fully-shaped sidecar fixture while preserving concurrently owned
/// cost and spend state. Production mutations use [`update_record`].
#[doc(hidden)]
pub fn write_record(
    runtime: &RuntimePaths,
    record: &AgentContextRecord,
) -> Result<(), atomic::AtomicErr> {
    let observed_at = record.context.observed_at;
    update_record(
        runtime,
        record.kind.as_str(),
        record.agent_id.as_str(),
        observed_at,
        |current, existed| {
            let mut next = record.clone();
            let observed_cost =
                next.context.cost.is_some() && !next.locally_priced_cost.owns_context_cost;
            if next.context.cost.is_none() {
                next.context.cost.clone_from(&current.context.cost);
            }
            if next.locally_priced_cost.is_empty() {
                next.locally_priced_cost = current.locally_priced_cost.clone();
            }
            // Local folds advance only through merge_local_context under this lock;
            // whole-record writers preserve the latest fold if their read raced it.
            if existed {
                next.spend_fold.clone_from(&current.spend_fold);
            }
            if observed_cost {
                next.locally_priced_cost.owns_context_cost = false;
            }
            *current = next;
            true
        },
    )
    .map(|_| ())
}

/// Mutate one session record against the latest published bytes while holding
/// its per-record lock. The closure receives whether valid prior bytes existed;
/// a `false` result leaves the sidecar untouched.
pub fn update_record(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    observed_at: Timestamp,
    update: impl FnOnce(&mut AgentContextRecord, bool) -> bool,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let prior = read_one_unlocked(runtime, kind, agent_id);
    let existed = prior.is_some();
    let mut record =
        prior.unwrap_or_else(|| new_record(kind, agent_id, AgentContext::new(kind, observed_at)));
    if !update(&mut record, existed) {
        return Ok(false);
    }
    write_record_unlocked(runtime, &record)?;
    Ok(true)
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
        spend_fold: None,
        locally_priced_cost: LocallyPricedCostState::default(),
    }
}

/// Merge transcript-derived local context into a sidecar record. Local refresh
/// owns tokens, cost, model identity/display, observed reasoning effort, and
/// the transcript stat gate; a tail that misses effort preserves the prior value.
pub fn merge_local_context(
    runtime: &RuntimePaths,
    definition: &AgentSpec,
    agent_id: &str,
    refresh: LocalContextRefresh,
    observed_at: Timestamp,
) -> Result<(), atomic::AtomicErr> {
    let kind = definition.kind;
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        record.context.source = kind.to_owned();
        let cost_replaced = !refresh.context.cost.is_keep();
        refresh.context.apply(&mut record.context, definition);
        if cost_replaced {
            record.locally_priced_cost.owns_context_cost = false;
        }
        record.context.observed_at = observed_at;
        refresh.spend_fold.apply(&mut record.spend_fold);
        record.transcript_path = refresh.transcript_path;
        record.transcript_stat = refresh.transcript_stat;
        true
    })
    .map(|_| ())
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
    let observed_at = context.observed_at;
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
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
        if changed {
            record.context.source = kind.to_owned();
            record.context.observed_at = observed_at;
        }
        changed
    })
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
    let observed_at = Timestamp::now();
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        if record.context.turn_error.as_ref() == Some(&marker) {
            return false;
        }
        record.context.source = kind.to_owned();
        record.context.turn_error = Some(marker);
        record.context.observed_at = observed_at;
        true
    })
}

/// Replace the delivered messages that opened the current turn. An empty set
/// is meaningful: a human-opened turn clears causality from the prior turn.
pub fn merge_turn_opened_by(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    message_ids: Vec<MessageId>,
) -> Result<bool, atomic::AtomicErr> {
    let observed_at = Timestamp::now();
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        if record.context.turn_opened_by == message_ids {
            return false;
        }
        record.context.source = kind.to_owned();
        record.context.turn_opened_by = message_ids;
        record.context.observed_at = observed_at;
        true
    })
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
    let observed_at = Timestamp::now();
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        if record.locally_priced_cost.last_turn_id.as_deref() == Some(priced.turn_id.as_str()) {
            return false;
        }
        let cumulative = record.locally_priced_cost.cumulative_usd + priced.cost_usd;
        if !cumulative.is_finite() || cumulative < 0.0 {
            return false;
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
        true
    })
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
    crate::agents::context::merge_session_usage(&mut target.session_usage, incoming.session_usage);
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
