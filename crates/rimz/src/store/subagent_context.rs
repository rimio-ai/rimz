//! Latest-wins per-subagent context sidecar.
//!
//! Claude's `subagentStatusLine` enrichment (a child's description, token count,
//! and start time) is written here by the hook/statusline CLI process — one atomic file per
//! `(kind, agent_id)` child under the runtime `subagent_context/` dir — and
//! folded into the snapshot read-side by
//! [`crate::store::snapshot::SidebarSnapshot::with_subagent_context`]. Like its
//! [`crate::store::agent_context`] sibling it never touches the durable event
//! log: this is display-only latency, not truth ("Durability first",
//! `docs/internals/store.md`).
//!
//! Ownership: the WRITER is always a RimZ CLI producer. The
//! sidebar renderer reads this data only through the snapshot JSON, never this
//! module, so "sidebar is read-only on the store" holds.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::{SubagentContext, SubagentUsageCursor};
use crate::disk::atomic;
use crate::disk::paths::RuntimePaths;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::sidecar;

/// A child's context sidecar: the enrichment plus the `(kind, agent_id)` it is
/// filed under, so a read can confirm the key — and shrug off a digest collision
/// — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentContextRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub context: SubagentContext,
    /// Resume state for incremental per-request pricing of this child's
    /// provider transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_cursor: Option<SubagentUsageCursor>,
    /// Which producer wrote the record last. A record written before the field
    /// existed came from the status line.
    #[serde(default)]
    pub reporter: SubagentReporter,
}

/// A producer of child sidecars. The mod's agent list outlives the child and
/// carries every field, so once the mod has written a child's record a status
/// line push for that child changes nothing: whole-record writes from the
/// sparser status line would otherwise erase what the mod wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentReporter {
    /// Claude's `subagentStatusLine`, through `rimz statusline feed --subagent`.
    #[default]
    StatusLine,
    /// RimZ's Claude mod, through `rimz mod feed`.
    Mod,
}

impl sidecar::SidecarRecord for SubagentContextRecord {
    const FILE_PREFIX: &'static str = "sub";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
}

/// Read-modify-write one child sidecar under its per-record advisory lock.
/// The caller receives the latest valid record and returns both the display
/// context and resumable transcript-pricing cursor to publish. A status line
/// write over a record the mod wrote is skipped, `apply` unread.
pub fn update(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    reporter: SubagentReporter,
    apply: impl FnOnce(Option<&SubagentContextRecord>) -> (SubagentContext, Option<SubagentUsageCursor>),
) -> Result<(), atomic::AtomicErr> {
    sidecar::update(
        runtime,
        &runtime.subagent_context_dir,
        kind,
        agent_id,
        || SubagentContextRecord {
            kind: AgentKind::new_unchecked(kind),
            agent_id: agent_id.into(),
            context: SubagentContext {
                usage: None,
                agent_type: None,
                model: None,
                effort: None,
                description: None,
                cost_usd: None,
                started_at: None,
                observed_at: Timestamp::now(),
            },
            usage_cursor: None,
            reporter,
        },
        |record, existed| {
            if existed
                && record.reporter == SubagentReporter::Mod
                && reporter == SubagentReporter::StatusLine
            {
                return false;
            }
            let (context, usage_cursor) = apply(existed.then_some(&*record));
            record.context = context;
            record.usage_cursor = usage_cursor;
            record.reporter = reporter;
            true
        },
    )
    .map(|_| ())
}

thread_local! {
    /// Per-thread parse cache keyed by the full file stamp, including inode.
    /// Normal reads cost one stat per file; deferred caller checks retain
    /// the parse without metadata IO.
    static SUBAGENT_PARSE_CACHE: sidecar::ParseCache<SubagentContextRecord> =
        sidecar::ParseCache::default();
}

/// Read every child's context sidecar. Tolerant: an unreadable or malformed
/// file is skipped, never fatal — enrichment, not correctness. Liveness gating
/// happens at the rollup join.
/// Steady-state cost on a long-lived thread is one stat per file; only a
/// changed file re-reads and re-parses (see `SUBAGENT_PARSE_CACHE`).
pub fn read_all(runtime: &RuntimePaths) -> Vec<SubagentContextRecord> {
    SUBAGENT_PARSE_CACHE.with(|cache| sidecar::read_all(&runtime.subagent_context_dir, cache))
}

/// Deferred keys retain the last parse without a stat; cadence belongs to the caller.
pub(crate) fn read_for_keys<'a>(
    runtime: &RuntimePaths,
    keys: impl IntoIterator<Item = (&'a str, &'a str, bool)>,
) -> Vec<SubagentContextRecord> {
    SUBAGENT_PARSE_CACHE
        .with(|cache| sidecar::read_for_keys_with_holds(&runtime.subagent_context_dir, keys, cache))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use jiff::Timestamp;

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/subctx-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn ctx(observed_at: Timestamp) -> SubagentContext {
        SubagentContext {
            usage: Some(crate::agents::AgentUsageSummary {
                fresh_input_tokens: Some(12_400),
                ..Default::default()
            }),
            agent_type: None,
            model: Some("child-model".to_owned()),
            effort: Some("high".to_owned()),
            description: Some("locate the render seam".to_owned()),
            cost_usd: None,
            started_at: Some(observed_at),
            observed_at,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        let context = ctx(now);
        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |prior| {
                (
                    context.clone(),
                    prior.and_then(|record| record.usage_cursor.clone()),
                )
            },
        )
        .unwrap();
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "child-1");
        assert_eq!(
            all[0]
                .context
                .usage
                .as_ref()
                .and_then(|usage| usage.input_context_tokens()),
            Some(12_400)
        );
        assert_eq!(all[0].context.model.as_deref(), Some("child-model"));
        assert_eq!(all[0].context.effort.as_deref(), Some("high"));
        assert_eq!(
            all[0].context.description.as_deref(),
            Some("locate the render seam")
        );
    }

    #[test]
    fn old_record_is_read_liveness_gating_is_the_rollups_job() {
        let (_dir, runtime) = runtime();
        let old = Timestamp::from_second(0).unwrap();
        let context = ctx(old);
        update(
            &runtime,
            "claude",
            "child-old",
            SubagentReporter::StatusLine,
            |prior| {
                (
                    context.clone(),
                    prior.and_then(|record| record.usage_cursor.clone()),
                )
            },
        )
        .unwrap();

        let all = read_all(&runtime);

        assert_eq!(all.len(), 1);
        assert_eq!(all[0].agent_id, "child-old");
        assert_eq!(all[0].context.observed_at, old);
    }

    #[test]
    fn usage_cursor_round_trips() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        let cursor = SubagentUsageCursor {
            anchor: None,
            last_call: None,
            transcript_path: "/tmp/parent/subagents/agent-child-1.jsonl".to_owned(),
            offset: 412,
            model: Some("child-model".to_owned()),
            cost_usd: 0.42,
            unpriced: false,
            book_fingerprint: Some("1700000000:412".to_owned()),
            last_request: None,
        };

        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |_| (ctx(now), Some(cursor.clone())),
        )
        .unwrap();

        assert_eq!(read_all(&runtime)[0].usage_cursor, Some(cursor));
    }

    #[test]
    fn usage_cursor_without_book_fingerprint_still_parses() {
        let cursor: SubagentUsageCursor = serde_json::from_str(
            r#"{
                "transcript_path":"child.jsonl",
                "offset":7,
                "cost_usd":0.12,
                "unpriced":true
            }"#,
        )
        .unwrap();

        assert_eq!(cursor.book_fingerprint, None);
        assert!(
            serde_json::to_value(&cursor)
                .unwrap()
                .get("anchor")
                .is_none()
        );
        let mut wire = serde_json::to_value(&cursor).unwrap();
        wire["anchor"] = serde_json::json!(123);
        let anchored: SubagentUsageCursor = serde_json::from_value(wire).unwrap();
        assert_eq!(serde_json::to_value(anchored).unwrap()["anchor"], 123);
    }

    #[test]
    fn pre_feature_record_parses_without_a_cursor_or_cost() {
        let record: SubagentContextRecord = serde_json::from_str(
            r#"{
                "kind":"claude",
                "agent_id":"child-1",
                "context":{
                    "description":"old record",
                    "observed_at":"2026-01-01T00:00:00Z"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(record.context.cost_usd, None);
        assert_eq!(record.context.model, None);
        assert_eq!(record.context.effort, None);
        assert_eq!(record.usage_cursor, None);
    }

    #[test]
    fn update_reads_prior_and_persists_the_returned_cursor() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        let context = ctx(now);
        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |prior| {
                (
                    context.clone(),
                    prior.and_then(|record| record.usage_cursor.clone()),
                )
            },
        )
        .unwrap();
        let cursor = SubagentUsageCursor {
            anchor: None,
            last_call: None,
            transcript_path: "child.jsonl".to_owned(),
            offset: 7,
            model: Some("child-model".to_owned()),
            cost_usd: 0.12,
            unpriced: false,
            book_fingerprint: None,
            last_request: None,
        };

        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |prior| {
                let prior = prior.expect("prior record");
                assert_eq!(
                    prior.context.description.as_deref(),
                    Some("locate the render seam")
                );
                let mut context = prior.context.clone();
                context.cost_usd = cursor.display_cost();
                (context, Some(cursor.clone()))
            },
        )
        .unwrap();

        let record = read_all(&runtime).into_iter().next().unwrap();
        assert_eq!(record.context.cost_usd, Some(0.12));
        assert_eq!(record.usage_cursor, Some(cursor));
    }

    #[test]
    fn a_mod_record_stands_against_later_status_line_pushes() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        let sparse = SubagentContext {
            description: None,
            effort: None,
            ..ctx(now)
        };
        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |_| (sparse.clone(), None),
        )
        .unwrap();
        assert_eq!(
            read_all(&runtime)[0].context,
            sparse,
            "the status line writes alone"
        );

        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::Mod,
            |prior| {
                assert_eq!(prior.unwrap().reporter, SubagentReporter::StatusLine);
                (ctx(now), None)
            },
        )
        .unwrap();
        update(
            &runtime,
            "claude",
            "child-1",
            SubagentReporter::StatusLine,
            |_| panic!("a status line push over a mod record is not applied"),
        )
        .unwrap();
        let record = read_all(&runtime).into_iter().next().unwrap();
        assert_eq!(record.reporter, SubagentReporter::Mod);
        assert_eq!(record.context, ctx(now), "the mod's fields stand");

        update(&runtime, "claude", "child-1", SubagentReporter::Mod, |_| {
            (sparse.clone(), None)
        })
        .unwrap();
        assert_eq!(
            read_all(&runtime)[0].context,
            sparse,
            "a later mod report replaces"
        );
    }
}
