//! Latest-wins per-session agent-context sidecar.
//!
//! High-frequency enrichment is written here by the context producers — the
//! statusline feed, hook ingestion/local transcript refresh, and detached
//! helpers (CLI paths), plus the elder renderer's producer-side triggers (the
//! in-process snapshot-produce backstop and the transcript watcher,
//! `sidebar_pane::app::transcript_watch`) — as one atomic file per
//! `(kind, agent_id)` session under the runtime `agent_context/` dir. The
//! snapshot read-side folds it in through
//! [`crate::ledger::snapshot::SidebarSnapshot::with_agent_context`]. It never
//! touches the durable event log: this is display-only latency, not truth
//! ("Ledger first", `docs/internals/sidebar/ledger.md`).
//!
//! Ownership: every renderer's fetch worker reads this module through the
//! shared enrichment fold; writes stay producer-side (CLI paths and the
//! elder's triggers above) and are cache-class — rename atomicity, no fsync,
//! rebuilt from provider state. "Sidebar is read-only on the ledger" is about
//! durable truth: nothing here reaches the event log or feed store, and the
//! `cargo xtask invariants` greps enforce that boundary.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::{AgentContext, AgentTurnError};
use crate::agents::{AgentCost, AgentTokenUsage, LocalContextRefresh, TranscriptStat};
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic;
use crate::ledger::paths::RuntimePaths;
use crate::ledger::sidecar;

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
    /// Transcript/rollout file used for the latest local context refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Stat gate for [`Self::transcript_path`], letting high-frequency hooks skip
    /// an unchanged tail without parsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stat: Option<TranscriptStat>,
}

impl sidecar::SidecarRecord for AgentContextRecord {
    const FILE_PREFIX: &'static str = "ctx";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }

    fn observed_at_secs(&self) -> i64 {
        self.context.observed_at.as_second()
    }
}

/// Drop a sidecar older than this even if its `SessionEnd` tombstone was
/// missed — matched to the snapshot's ghost-session TTL so stale cost or
/// rate-limit data cannot pin a vanished pidless session (parity pinned by
/// `context_sidecar_ttl_matches_the_ghost_session_ttl` in the view tests).
pub(crate) const CONTEXT_TTL_SECS: i64 = 3 * 60 * 60;

/// Persist (latest-wins) one session's context from a CLI producer.
/// Atomic temp+rename (no fsync — disposable sidecar) via
/// [`write_temp_then_rename_cache`].
pub fn write(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    context: &AgentContext,
) -> Result<(), atomic::AtomicErr> {
    write_record(
        runtime,
        &AgentContextRecord {
            kind: AgentKind::new_unchecked(kind),
            agent_id: agent_id.into(),
            context: context.clone(),
            rate_limits_observed_at: None,
            rich_observed_at: None,
            transcript_path: None,
            transcript_stat: None,
        },
    )
}

/// Persist a fully-shaped sidecar record. Used by merge paths that preserve
/// fields owned by different context producers.
pub fn write_record(
    runtime: &RuntimePaths,
    record: &AgentContextRecord,
) -> Result<(), atomic::AtomicErr> {
    sidecar::write_record(&runtime.agent_context_dir, record)
}

/// Read one sidecar directly from disk, bypassing the long-lived parse cache.
/// Writers use this before a read-modify-write so they merge against the latest
/// published bytes, not the last value a sidebar consumer happened to parse.
pub fn read_one(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> Option<AgentContextRecord> {
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
    }
}

/// Merge transcript/config-derived local context into a sidecar record. Local
/// refresh owns tokens, cost, model id, actual reasoning effort, and the
/// transcript stat gate; app-server/statusline-only fields are preserved.
pub fn merge_local_context(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    prior: Option<AgentContextRecord>,
    refresh: LocalContextRefresh,
    observed_at: Timestamp,
) -> Result<(), atomic::AtomicErr> {
    let mut record =
        prior.unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    let prior_model_id = record.context.model_id.clone();
    let prior_context_window = record
        .context
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.context_window_size);
    let prior_tokens = record.context.tokens.clone();
    let refresh_model_id = refresh.model_id.clone();
    record.context.source = kind.to_owned();
    if refresh.model_id.is_some() {
        record.context.model_id = refresh.model_id;
    }
    record.context.effort = refresh.effort;
    let mut refresh_tokens = refresh.tokens;
    preserve_established_tokens(kind, prior_tokens.as_ref(), &mut refresh_tokens);
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
    if refresh.cost.is_some() {
        record.context.cost = refresh.cost;
    }
    // Overwrite each refresh: a clean `task_complete` at the tail sets the
    // marker, and a fresh turn already underway clears it (the detector returns
    // `None`), so a stale completion never outlives its turn.
    record.context.turn_complete = refresh.turn_complete;
    record.context.observed_at = observed_at;
    record.transcript_path = refresh.transcript_path;
    record.transcript_stat = refresh.transcript_stat;
    write_record(runtime, &record)
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
    let mut record = read_one(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    let mut changed = false;
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
        }
    }
    if !changed {
        return Ok(false);
    }
    record.context.source = kind.to_owned();
    record.context.observed_at = observed_at;
    write_record(runtime, &record)?;
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
    let observed_at = Timestamp::now();
    let mut record = read_one(runtime, kind, agent_id)
        .unwrap_or_else(|| new_record(kind, agent_id, empty_context(kind, observed_at)));
    if record.context.turn_error.as_ref() == Some(&marker) {
        return Ok(false);
    }
    record.context.source = kind.to_owned();
    record.context.turn_error = Some(marker);
    record.context.observed_at = observed_at;
    write_record(runtime, &record)?;
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
    if let Some(current_usage) = incoming.current_usage {
        target.current_usage = Some(current_usage);
    }
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

fn preserve_established_tokens(
    kind: &str,
    prior: Option<&AgentTokenUsage>,
    refresh: &mut Option<AgentTokenUsage>,
) {
    let Some(prior) = prior.filter(|tokens| established_token_usage(tokens)) else {
        return;
    };
    match refresh {
        None => *refresh = Some(prior.clone()),
        Some(tokens) if should_preserve_tokens(kind, tokens) => *tokens = prior.clone(),
        Some(_) => {}
    }
}

fn established_token_usage(tokens: &AgentTokenUsage) -> bool {
    tokens.used_percentage.is_some_and(|pct| pct > 0)
        || tokens
            .current_usage
            .as_ref()
            .is_some_and(|usage| !usage.is_zero())
}

fn should_preserve_tokens(kind: &str, refresh: &AgentTokenUsage) -> bool {
    kind == "codex" && inferred_fresh_codex_tokens(refresh)
}

fn inferred_fresh_codex_tokens(tokens: &AgentTokenUsage) -> bool {
    // A fresh rollout tail (no `token_count` event yet) carries an all-zero
    // current usage and no percentage. Codex no longer bakes a percentage, so
    // the zeroed `current_usage` under an absent percentage is the fresh
    // sentinel — recognise it and keep the prior established record rather than
    // overwriting real context with zeros and a default window.
    tokens.used_percentage.is_none()
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
        turn_error: None,
        turn_complete: None,
        observed_at,
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

/// Read every live session's context. Tolerant: an unreadable, malformed, or
/// past-TTL file is skipped, never fatal — enrichment, not correctness.
/// Steady-state cost on a long-lived thread is one stat per file; only a
/// changed file re-reads and re-parses (see [`CONTEXT_PARSE_CACHE`]).
pub fn read_all(runtime: &RuntimePaths) -> Vec<AgentContextRecord> {
    read_all_at(runtime, Timestamp::now())
}

fn read_all_at(runtime: &RuntimePaths, now: Timestamp) -> Vec<AgentContextRecord> {
    CONTEXT_PARSE_CACHE.with(|cache| {
        sidecar::read_all(
            &runtime.agent_context_dir,
            cache,
            now.as_second(),
            CONTEXT_TTL_SECS,
        )
    })
}

/// Remove a session's sidecar (a `SessionEnd` tombstone, or reap). Best-effort:
/// a missing file is success.
pub fn remove(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> std::io::Result<()> {
    sidecar::remove::<AgentContextRecord>(&runtime.agent_context_dir, kind, agent_id)
}

#[cfg(test)]
mod tests;
