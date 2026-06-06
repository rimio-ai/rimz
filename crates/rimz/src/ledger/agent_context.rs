//! Latest-wins per-session agent-context sidecar.
//!
//! High-frequency enrichment is written here by CLI producer paths — statusline
//! feed, hook ingestion/local transcript refresh, detached helpers, and snapshot
//! producer backstops — as one atomic file per `(kind, agent_id)` session under
//! the runtime `agent_context/` dir. The snapshot read-side folds it in through
//! [`crate::ledger::snapshot::SidebarSnapshot::with_agent_context`]. It never
//! touches the durable event log: this is display-only latency, not truth
//! ("Ledger first", `docs/internals/ledger.md`).
//!
//! Ownership: writers are `rimz` CLI producers. The sidebar renderer reads this
//! data only through the snapshot JSON, never this module, so "sidebar is
//! read-only on the ledger" holds.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::AgentContext;
use crate::agents::{LocalContextRefresh, TranscriptStat};
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic::{self, write_temp_then_rename_cache};
use crate::ledger::paths::RuntimePaths;

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
    /// Transcript/rollout file used for the latest local context refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Stat gate for [`Self::transcript_path`], letting high-frequency hooks skip
    /// an unchanged tail without parsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stat: Option<TranscriptStat>,
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
    write_temp_then_rename_cache(
        &runtime.agent_context_path(record.kind.as_str(), record.agent_id.as_str()),
        record,
    )
}

/// Read one sidecar directly from disk, bypassing the long-lived parse cache.
/// Writers use this before a read-modify-write so they merge against the latest
/// published bytes, not the last value a sidebar consumer happened to parse.
pub fn read_one(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> Option<AgentContextRecord> {
    let path = runtime.agent_context_path(kind, agent_id);
    let record: AgentContextRecord = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    if record.kind == kind && record.agent_id.as_str() == agent_id {
        Some(record)
    } else {
        None
    }
}

pub fn new_record(kind: &str, agent_id: &str, context: AgentContext) -> AgentContextRecord {
    AgentContextRecord {
        kind: AgentKind::new_unchecked(kind),
        agent_id: agent_id.into(),
        context,
        rate_limits_observed_at: None,
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
    record.context.source = kind.to_owned();
    if refresh.model_id.is_some() {
        record.context.model_id = refresh.model_id;
    }
    record.context.effort = refresh.effort;
    record.context.tokens = refresh.tokens;
    record.context.cost = refresh.cost;
    record.context.observed_at = observed_at;
    record.transcript_path = refresh.transcript_path;
    record.transcript_stat = refresh.transcript_stat;
    write_record(runtime, &record)
}

pub fn empty_context(source: &str, observed_at: Timestamp) -> AgentContext {
    AgentContext {
        source: source.to_owned(),
        session_name: None,
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
        observed_at,
    }
}

/// One parsed sidecar, gated by the stat that validated it. `record` is `None`
/// for a file that read or parsed as garbage, so a corrupt sidecar costs one
/// parse attempt, not one per tick.
struct ParsedSidecar {
    mtime: SystemTime,
    len: u64,
    record: Option<AgentContextRecord>,
}

thread_local! {
    /// Per-thread parse cache. Every update lands via atomic rename of a
    /// freshly-written temp file, so `(mtime, len)` validates content; the
    /// long-lived consumer fetch thread re-reads these sidecars on every
    /// wakeup, and this caps its steady-state cost at one stat per file.
    static CONTEXT_PARSE_CACHE: RefCell<HashMap<PathBuf, ParsedSidecar>> =
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
    let Ok(entries) = fs::read_dir(&runtime.agent_context_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    CONTEXT_PARSE_CACHE.with_borrow_mut(|cache| {
        let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let len = meta.len();
            seen.insert(path.clone());
            let record = match cache.get(&path) {
                Some(parsed) if parsed.mtime == mtime && parsed.len == len => parsed.record.clone(),
                _ => {
                    let record = fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    cache.insert(
                        path,
                        ParsedSidecar {
                            mtime,
                            len,
                            record: record.clone(),
                        },
                    );
                    record
                }
            };
            let Some(record) = record else { continue };
            // The TTL is evaluated fresh per read — a cached record still ages out.
            if now.as_second() - record.context.observed_at.as_second() > CONTEXT_TTL_SECS {
                continue;
            }
            out.push(record);
        }
        // Drop cache keys whose files vanished (SessionEnd tombstone, gc), so
        // the cache stays bounded by the live sidecar set.
        cache.retain(|path, _| seen.contains(path));
    });
    out
}

/// Remove a session's sidecar (a `SessionEnd` tombstone, or reap). Best-effort:
/// a missing file is success.
pub fn remove(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> std::io::Result<()> {
    match fs::remove_file(runtime.agent_context_path(kind, agent_id)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/ctx-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn ctx(observed_at: Timestamp) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
            session_name: None,
            model_id: Some("claude-opus-4-8".to_owned()),
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
            observed_at,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-1");
        assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn absent_merge_fields_round_trip_as_none() {
        let now = Timestamp::now();
        let record = new_record("codex", "sess-1", ctx(now));
        let mut value = serde_json::to_value(&record).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("rate_limits_observed_at");
        value.as_object_mut().unwrap().remove("transcript_path");
        value.as_object_mut().unwrap().remove("transcript_stat");

        let parsed: AgentContextRecord = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.kind, "codex");
        assert_eq!(parsed.agent_id, "sess-1");
        assert_eq!(parsed.rate_limits_observed_at, None);
        assert_eq!(parsed.transcript_path, None);
        assert_eq!(parsed.transcript_stat, None);

        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("rate_limits_observed_at"));
        assert!(!serialized.contains("transcript_path"));
        assert!(!serialized.contains("transcript_stat"));
    }

    #[test]
    fn read_one_bypasses_the_parse_cache() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        assert_eq!(
            read_all(&runtime)[0].context.model_id.as_deref(),
            Some("claude-opus-4-8")
        );

        let mut changed = ctx(now);
        changed.model_id = Some("claude-sonnet-4-5".to_owned());
        write(&runtime, "claude", "sess-1", &changed).unwrap();

        let fresh = read_one(&runtime, "claude", "sess-1").expect("fresh direct read");
        assert_eq!(fresh.context.model_id.as_deref(), Some("claude-sonnet-4-5"));
    }

    #[test]
    fn merge_local_context_preserves_app_server_fields() {
        let (_dir, runtime) = runtime();
        let app_at = Timestamp::from_second(1_700_000_000).unwrap();
        let local_at = Timestamp::from_second(1_700_000_030).unwrap();
        let mut prior_context = ctx(app_at);
        prior_context.model_id = Some("gpt-5".to_owned());
        prior_context.model_display_name = Some("GPT-5".to_owned());
        prior_context.rate_limits = Some(crate::agents::AgentRateLimits {
            windows: vec![crate::agents::RateLimitWindow {
                used_percentage: Some(12),
                resets_at: None,
                duration_mins: Some(300),
            }],
        });
        let mut prior = new_record("codex", "sess-1", prior_context);
        prior.rate_limits_observed_at = Some(app_at);
        write_record(&runtime, &prior).unwrap();

        merge_local_context(
            &runtime,
            "codex",
            "sess-1",
            read_one(&runtime, "codex", "sess-1"),
            crate::agents::LocalContextRefresh {
                model_id: Some("gpt-5.5".to_owned()),
                effort: Some("xhigh".to_owned()),
                tokens: Some(crate::agents::AgentTokenUsage {
                    context_window_size: Some(1_000),
                    used_percentage: Some(40),
                    remaining_percentage: Some(60),
                    current_usage: Some(crate::agents::AgentCurrentUsage {
                        input_tokens: Some(30),
                        output_tokens: Some(4),
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: Some(10),
                    }),
                }),
                cost: Some(crate::agents::AgentCost {
                    total_cost_usd: Some(0.12),
                    ..crate::agents::AgentCost::default()
                }),
                transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
                transcript_stat: Some(crate::agents::TranscriptStat {
                    mtime_secs: 123,
                    mtime_nanos: 456,
                    len: 789,
                }),
            },
            local_at,
        )
        .unwrap();

        let merged = read_one(&runtime, "codex", "sess-1").unwrap();
        assert_eq!(merged.context.model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(merged.context.effort.as_deref(), Some("xhigh"));
        assert_eq!(merged.context.model_display_name.as_deref(), Some("GPT-5"));
        assert_eq!(
            merged
                .context
                .rate_limits
                .as_ref()
                .and_then(|limits| limits.windows.first())
                .and_then(|window| window.used_percentage),
            Some(12)
        );
        assert_eq!(merged.rate_limits_observed_at, Some(app_at));
        assert_eq!(merged.context.observed_at, local_at);
        assert_eq!(
            merged
                .context
                .tokens
                .as_ref()
                .and_then(|t| t.used_percentage),
            Some(40)
        );
        assert_eq!(
            merged
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.12)
        );
        assert_eq!(
            merged.transcript_path.as_deref(),
            Some("/tmp/rollout.jsonl")
        );
    }

    #[test]
    fn distinct_sessions_get_distinct_files() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
        let mut ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["sess-1".to_owned(), "sess-2".to_owned()]);
    }

    #[test]
    fn corrupt_file_is_skipped() {
        let (_dir, runtime) = runtime();
        std::fs::write(
            runtime.agent_context_dir.join("ctx.bogus.json"),
            b"not json",
        )
        .unwrap();
        assert!(read_all(&runtime).is_empty());
    }

    #[test]
    fn past_ttl_record_is_skipped() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        let stale = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 60).unwrap();
        write(&runtime, "claude", "sess-1", &ctx(stale)).unwrap();
        assert!(read_all_at(&runtime, now).is_empty());
    }

    #[test]
    fn ttl_cutoff_is_boundary_exact() {
        // A missed tombstone ages out on the TTL exactly: a record *at* the
        // cutoff is still served, one second past it is gone — an off-by-one
        // in either direction fails one arm.
        let (_dir, runtime) = runtime();
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        let at_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS).unwrap();
        let past_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 1).unwrap();
        write(&runtime, "claude", "sess-at", &ctx(at_cutoff)).unwrap();
        write(&runtime, "claude", "sess-past", &ctx(past_cutoff)).unwrap();
        let ids: Vec<_> = read_all_at(&runtime, now)
            .into_iter()
            .map(|r| r.agent_id)
            .collect();
        assert_eq!(ids, vec!["sess-at".to_owned()]);
    }

    #[test]
    fn unchanged_stat_skips_the_reparse() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        let first = read_all(&runtime);
        assert_eq!(first[0].agent_id, "sess-1");

        // Rewrite the file in place with a different identity but identical
        // length, restoring the original mtime: the stat gate cannot tell it
        // changed, so the cached parse is served — which is exactly the
        // contract (every real update is an atomic rename of a fresh temp
        // file, so a same-stat file is byte-identical in production).
        let path = runtime.agent_context_path("claude", "sess-1");
        let original = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(original)
            .unwrap()
            .replace("sess-1", "sess-9");
        std::fs::write(&path, swapped).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);
        assert_eq!(
            read_all(&runtime)[0].agent_id,
            "sess-1",
            "same (mtime, len) serves the cached parse — one stat, no read"
        );

        // A moved mtime invalidates: the rewrite is now visible.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime + std::time::Duration::from_secs(3))
            .unwrap();
        drop(f);
        assert_eq!(read_all(&runtime)[0].agent_id, "sess-9");
    }

    #[test]
    fn remove_targets_one_session() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
        remove(&runtime, "claude", "sess-1").unwrap();
        let ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
        assert_eq!(ids, vec!["sess-2".to_owned()]);
        // Removing an absent session is success.
        remove(&runtime, "claude", "sess-1").unwrap();
    }
}
