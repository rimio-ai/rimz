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

use std::path::PathBuf;

use jiff::Timestamp;

use crate::agents::context::{AgentContext, AgentContextRecord, AgentTurnError};
use crate::agents::{
    AgentSpec, AgentState, AgentStatus, LocalContextRefresh, LocallyPricedTurnCost,
};
use crate::disk::atomic;
use crate::disk::paths::RuntimePaths;
use crate::ids::MessageId;
use crate::store::sidecar;

impl sidecar::SidecarRecord for AgentContextRecord {
    const FILE_PREFIX: &'static str = "ctx";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
}

/// Sidecar file for one session's record; test fixture access only.
#[cfg(any(test, feature = "testkit"))]
pub fn path_for(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> PathBuf {
    sidecar::path(
        &runtime.agent_context_dir,
        <AgentContextRecord as sidecar::SidecarRecord>::FILE_PREFIX,
        kind,
        agent_id,
    )
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
        record.apply_context_refresh(kind, agent_id, context.clone())
    })
    .map(|_| ())
}

/// Attach provider rest certificates to raw-active root sessions before a
/// caller makes an ownership or reaping decision. Missing sidecars leave the
/// durable lifecycle status authoritative.
pub(crate) fn attach_rest_certificates<'a>(
    runtime: &RuntimePaths,
    agents: impl IntoIterator<Item = &'a mut AgentState>,
) {
    for agent in agents {
        if agent.ended_at.is_some()
            || agent.is_provider_subagent()
            || !matches!(agent.status, AgentStatus::Running | AgentStatus::Waiting)
        {
            continue;
        }
        agent.context = read_one(runtime, agent.kind.as_str(), agent.agent_id.as_str())
            .map(|record| record.context);
    }
}

/// Persist a fully-shaped sidecar fixture while preserving concurrently owned
/// cost and spend state. Production mutations use [`update_record`].
#[doc(hidden)]
#[cfg(any(test, feature = "testkit"))]
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
        |current, existed| current.apply_fixture(record.clone(), existed),
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
    sidecar::update(
        &runtime.agent_context_dir,
        kind,
        agent_id,
        || AgentContextRecord::new(kind, agent_id, AgentContext::new(kind, observed_at)),
        update,
    )
}

/// Read one sidecar directly from disk, bypassing the long-lived parse cache.
/// Writers use this before a read-modify-write so they merge against the latest
/// published bytes, not the last value a sidebar consumer happened to parse.
pub fn read_one(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> Option<AgentContextRecord> {
    sidecar::read_one(&runtime.agent_context_dir, kind, agent_id)
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
    update_record(
        runtime,
        definition.kind,
        agent_id,
        observed_at,
        |record, _| record.apply_local_refresh(definition, refresh, observed_at),
    )
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
        record.merge_observed(kind, context, observed_at)
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
    priced: &LocallyPricedTurnCost,
) -> Result<bool, atomic::AtomicErr> {
    if priced.turn_id.trim().is_empty() || !priced.cost_usd.is_finite() || priced.cost_usd <= 0.0 {
        return Ok(false);
    }
    let observed_at = Timestamp::now();
    update_record(runtime, kind, agent_id, observed_at, |record, _| {
        record.apply_locally_priced_turn(kind, priced, observed_at)
    })
}

thread_local! {
    /// Per-thread parse cache. Every update lands via atomic rename of a
    /// freshly-written temp file, so `(mtime, len)` validates content; the
    /// long-lived consumer fetch thread re-reads these sidecars on every
    /// wakeup, and this caps its steady-state cost at one stat per file.
    static CONTEXT_PARSE_CACHE: sidecar::ParseCache<AgentContextRecord> =
        sidecar::ParseCache::default();
}

/// Read every session's context sidecar. Tolerant: an unreadable or malformed
/// file is skipped, never fatal — enrichment, not correctness. Liveness gating
/// happens at the rollup join.
/// Steady-state cost on a long-lived thread is one stat per file; only a
/// changed file re-reads and re-parses (see [`CONTEXT_PARSE_CACHE`]).
pub fn read_all(runtime: &RuntimePaths) -> Vec<AgentContextRecord> {
    CONTEXT_PARSE_CACHE.with(|cache| sidecar::read_all(&runtime.agent_context_dir, cache))
}

/// Read context only for identities already present in the snapshot.
pub(super) fn read_for_keys<'a>(
    runtime: &RuntimePaths,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<AgentContextRecord> {
    CONTEXT_PARSE_CACHE
        .with(|cache| sidecar::read_for_keys(&runtime.agent_context_dir, keys, cache))
}

/// Remove a session's sidecar on `SessionEnd` or reap. Best-effort:
/// a missing file is success.
pub fn remove(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> std::io::Result<()> {
    sidecar::remove_locked::<AgentContextRecord>(&runtime.agent_context_dir, kind, agent_id)
        .map_err(|error| match error {
            atomic::AtomicErr::Io { source, .. } => source,
            atomic::AtomicErr::Json(source) => std::io::Error::other(source),
        })
}

#[cfg(test)]
mod tests;
