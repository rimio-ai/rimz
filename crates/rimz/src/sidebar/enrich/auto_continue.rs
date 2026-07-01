//! Producer-side auto-continue: resume a parked agent by nudging its live pane
//! after a rate-limit window resets or a retry backoff elapses.
//!
//! Opt-in ([`ResumeConfig::auto_continue`]). The producer arms the resume while
//! the park is fresh and fires it once the class-specific clock is due. The
//! durable record carries everything needed between arm and fire so the decision
//! never depends on the ephemeral per-session context surviving the wait:
//!
//! - **Arm.** Each frame an agent is parked on a resumable certificate
//!   ([`crate::agents::resume_park`]), the producer writes a durable [`ParkRecord`]
//!   capturing the park class and the agent's frozen `last_activity`. A
//!   rate-limit record captures the latest spent-window reset deadline; a
//!   backoff record carries the turn-error marker time and retry state.
//! - **Fire.** Once the window reset deadline or retry backoff is due and the
//!   agent is still idle (`last_activity` unchanged), the producer spawns the
//!   detached `rimz agents auto-continue` helper that types the nudge and writes
//!   the `agent.resumed` audit record.
//! - **Clear.** Any activity since the park (the nudge took, or the agent woke on
//!   its own) advances `last_activity`, and the stale record is removed. A
//!   recovered fused account budget also clears a clocked record whose resume
//!   condition is moot.
//!
//! This module owns only the durable record, the pane join, and the spawn — the
//! arm decision is the pure, unit-tested [`crate::agents::resume_park`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{AccountBudget, AgentState, ResumeArm, resume_park};
use crate::config::{DEFAULT_AUTO_CONTINUE_BACKOFF_SECS, ResumeConfig};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::atomic::write_temp_then_rename_cache;
use crate::ledger::snapshot::PaneAgent;
use crate::sidebar::timing::AUTO_CONTINUE_RETRY_INTERVAL;

use super::SidebarSnapshot;

/// A durable record of one park: written while the park is fresh, read after its
/// class-specific resume condition is due. It outlives the per-session context
/// the park was first seen through, so a resume survives both an expired context
/// sidecar and a fresh non-spent reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ParkRecord {
    /// The park class and its durable resume facts.
    kind: ParkKind,
    /// The agent's rollup `last_activity` at arm time. Unchanged means the agent
    /// has done nothing since: still parked, safe to nudge. Advanced means it woke
    /// (our nudge took, or it resumed on its own), so the record is stale.
    parked_at_activity: Timestamp,
    /// When the last auto-continue attempt fired, throttling re-nudges so a nudge
    /// that fails to wake a still-parked agent is retried without spamming a
    /// working one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_nudge_at: Option<Timestamp>,
    /// Auto-continue attempts for this park. Rate-limit and backoff records both
    /// use it for the retry cap.
    #[serde(default)]
    retries: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "park")]
enum ParkKind {
    RateLimit {
        deadline: Timestamp,
    },
    Overloaded {
        /// The non-clocked turn-error marker timestamp. The first retry is
        /// measured from this marker, so a late-observed park can fire
        /// immediately.
        overloaded_at: Timestamp,
    },
}

/// Arm or fire each park when live auto-continue is enabled. Best-effort: an
/// empty nudge text or an agent with no live pane waits without consuming a
/// retry; a spawn failure consumes one attempt and backs off. Producer-only —
/// one elected producer drives one room, and the records live in that room's
/// runtime dir, so one due condition nudges its agent once per retry.
pub(super) fn resume_parked(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
) {
    if !config.auto_continue {
        return;
    }
    let text = config.auto_continue_text.trim();
    let now = snapshot.now;
    let account_budgets = super::account_budgets_from_caches(runtime, now);
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        let budget = account_budgets.get(&agent.kind);
        match resume_park(agent, budget, now) {
            Some(ResumeArm::RateLimit { deadline }) => {
                arm_park(&path, ParkKind::RateLimit { deadline }, agent.last_activity);
                fire_if_due(snapshot, runtime, agent, &path, now, text, config);
            }
            Some(ResumeArm::Overloaded { overloaded_at }) => {
                arm_park(
                    &path,
                    ParkKind::Overloaded { overloaded_at },
                    agent.last_activity,
                );
                fire_if_due(snapshot, runtime, agent, &path, now, text, config);
            }
            _ => {
                // No arm this frame means "no recovering window", not "forget
                // the durable deadline". Clear only once the fused account
                // budget proves the subscription bar has recovered.
                if budget_recovered(budget, now) {
                    remove_park(&path);
                } else {
                    fire_if_due(snapshot, runtime, agent, &path, now, text, config);
                }
            }
        }
    }
}

fn budget_recovered(budget: Option<&AccountBudget>, now: Timestamp) -> bool {
    budget.is_some_and(|budget| budget.subscription_budget_available(now))
}

/// Capture (or refresh) the park while the reading is still active. A new park
/// baseline — the first arm, the agent acted and re-parked, or the park class
/// changed — starts a fresh nudge throttle and retry count; a steady park keeps
/// both. Write-if-changed, so a frozen park costs one write, not one per frame.
fn arm_park(path: &Path, kind: ParkKind, last_activity: Timestamp) {
    let prior = read_park(path);
    let carry = prior
        .as_ref()
        .filter(|record| {
            record.parked_at_activity == last_activity && same_park_class(&record.kind, &kind)
        })
        .map(|record| (record.last_nudge_at, record.retries));
    let (last_nudge_at, retries) = carry.unwrap_or((None, 0));
    let next = ParkRecord {
        kind,
        parked_at_activity: last_activity,
        last_nudge_at,
        retries,
    };
    if prior.as_ref() != Some(&next) {
        write_park(path, &next);
    }
}

fn same_park_class(left: &ParkKind, right: &ParkKind) -> bool {
    matches!(
        (left, right),
        (ParkKind::RateLimit { .. }, ParkKind::RateLimit { .. })
            | (ParkKind::Overloaded { .. }, ParkKind::Overloaded { .. })
    )
}

/// Fire a parked agent's resume when its recorded condition is due and it is
/// still idle. A woken agent (activity advanced) clears the record; a pane that
/// has not appeared yet, a condition still ahead, or a recent nudge each waits.
fn fire_if_due(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    agent: &AgentState,
    path: &Path,
    now: Timestamp,
    text: &str,
    config: &ResumeConfig,
) {
    let Some(record) = read_park(path) else {
        return;
    };
    if !still_parked(&record, agent.last_activity) {
        remove_park(path);
        return;
    }
    let reason = match &record.kind {
        ParkKind::RateLimit { .. } => "rate_limit_window_reset",
        ParkKind::Overloaded { .. } => "overloaded_backoff_retry",
    };
    if text.is_empty() {
        return;
    }
    if !nudge_due(
        &record,
        now,
        &config.auto_continue_backoff_secs,
        config.auto_continue_max_retries,
    ) {
        return;
    }
    let Some(pane_id) = live_pane(&snapshot.agent_panes, &agent.kind, &agent.agent_id) else {
        return;
    };
    spawn_auto_continue(
        runtime,
        &agent.kind,
        &agent.agent_id,
        &pane_id,
        text,
        reason,
    );
    write_park(path, &nudged_record(record, now));
}

/// Whether the agent has done nothing since the park was armed — its rollup
/// `last_activity` still matches. A changed activity means it woke (our nudge
/// took, or it resumed on its own), so the record is stale.
fn still_parked(record: &ParkRecord, last_activity: Timestamp) -> bool {
    record.parked_at_activity == last_activity
}

fn nudged_record(mut record: ParkRecord, now: Timestamp) -> ParkRecord {
    record.last_nudge_at = Some(now);
    record.retries += 1;
    record
}

fn overload_backoff(retries: u32, backoff_secs: &[u64]) -> Duration {
    let idx = (retries as usize).min(backoff_secs.len().saturating_sub(1));
    let fallback = DEFAULT_AUTO_CONTINUE_BACKOFF_SECS
        .last()
        .copied()
        .unwrap_or(180);
    Duration::from_secs(backoff_secs.get(idx).copied().unwrap_or(fallback))
}

/// Whether a nudge is due for this park class. Rate limits wait for the captured
/// deadline and then throttle repeats; backoff records wait from park time for
/// the first try, then from the prior nudge for each retry step until the cap.
fn nudge_due(record: &ParkRecord, now: Timestamp, backoff_secs: &[u64], max_retries: u32) -> bool {
    match &record.kind {
        ParkKind::RateLimit { deadline } => {
            if record.retries >= max_retries {
                return false;
            }
            now >= *deadline
                && record.last_nudge_at.is_none_or(|at| {
                    now.as_second() - at.as_second()
                        >= AUTO_CONTINUE_RETRY_INTERVAL.as_secs() as i64
                })
        }
        ParkKind::Overloaded { overloaded_at } => {
            if record.retries >= max_retries {
                return false;
            }
            let anchor = record.last_nudge_at.unwrap_or(*overloaded_at);
            now.as_second() - anchor.as_second()
                >= overload_backoff(record.retries, backoff_secs).as_secs() as i64
        }
    }
}

/// The live pane bound to one agent this frame, from the producer's pane fold. An
/// agent with no bound live pane (absent from `agent_panes`) has nothing to type
/// into.
fn live_pane(panes: &[PaneAgent], kind: &AgentKind, agent_id: &AgentSessionId) -> Option<PaneId> {
    panes
        .iter()
        .find(|pane| &pane.kind == kind && pane.agent_id.as_ref() == Some(agent_id))
        .map(|pane| pane.pane_id.clone())
}

fn read_park(path: &Path) -> Option<ParkRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_park(path: &Path, record: &ParkRecord) {
    if let Err(err) = write_temp_then_rename_cache(path, record) {
        tracing::warn!(
            tags.operation = "auto_continue.write_park",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record resumable park",
        );
    }
}

fn remove_park(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn park_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_str().as_bytes());
    let digest = hex::encode(hasher.finalize());
    runtime
        .root
        .join(format!("auto-continue.{}.json", &digest[..32]))
}

/// Spawn the detached, fresh-stdio helper that types the nudge into the parked
/// pane and writes the `agent.resumed` audit record. Best-effort: a spawn failure
/// is logged and counted as a fired attempt so a broken helper path backs off
/// instead of retrying every frame.
fn spawn_auto_continue(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    pane_id: &PaneId,
    text: &str,
    reason: &str,
) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                workspace = %runtime.workspace_id,
                tags.operation = "auto_continue.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to auto-continue agent",
            );
            return;
        }
    };
    let mut cmd = super::detached_rimz_command(exe, runtime);
    cmd.args([
        "agents",
        "auto-continue",
        "--workspace-id",
        runtime.workspace_id.as_str(),
        "--kind",
        kind.as_str(),
        "--agent-id",
        agent_id.as_str(),
        "--pane",
        &pane_id.to_string(),
        "--text",
        text,
        "--reason",
        reason,
    ]);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %kind,
        reason,
        "sidebar: auto-continuing parked agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agent-auto-continue") {
        // Best-effort enrichment on a throttled producer path. The CWD anchor
        // clears the gc'd-worktree ENOENT; a genuinely missing/replaced `rimz`
        // binary (upgrade-during-run) still fails here — an environment fact,
        // not a Rimz fault. Keep it at debug! so it never reaches Sentry.
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "auto_continue.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent auto-continue",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::lifecycle::TurnPhase;
    use crate::agents::{AgentRateLimits, AgentStatus, RateLimitWindow};
    use crate::ids::WorkspaceId;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).expect("valid test timestamp")
    }

    fn rate_record(
        deadline: i64,
        activity: i64,
        last_nudge: Option<i64>,
        retries: u32,
    ) -> ParkRecord {
        ParkRecord {
            kind: ParkKind::RateLimit {
                deadline: ts(deadline),
            },
            parked_at_activity: ts(activity),
            last_nudge_at: last_nudge.map(ts),
            retries,
        }
    }

    fn overloaded_record(
        overloaded_at: i64,
        activity: i64,
        last_nudge: Option<i64>,
        retries: u32,
    ) -> ParkRecord {
        ParkRecord {
            kind: ParkKind::Overloaded {
                overloaded_at: ts(overloaded_at),
            },
            parked_at_activity: ts(activity),
            last_nudge_at: last_nudge.map(ts),
            retries,
        }
    }

    fn due(record: &ParkRecord, now: i64, backoff_secs: &[u64], max_retries: u32) -> bool {
        nudge_due(record, ts(now), backoff_secs, max_retries)
    }

    fn window(used: u8, reset: i64) -> RateLimitWindow {
        RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(ts(reset)),
            duration_mins: Some(300),
            ..Default::default()
        }
    }

    fn temp_runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        (dir, runtime)
    }

    fn park_path(runtime: &RuntimePaths) -> PathBuf {
        park_record_path(runtime, &AgentKind::new_unchecked("claude"), &"sess".into())
    }

    fn agent(activity: i64) -> AgentState {
        AgentState {
            agent_id: "sess".into(),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            channel: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: ts(activity),
            last_activity: ts(activity),
            registered_at: Some(ts(activity)),
        }
    }

    #[test]
    fn arms_a_rate_limit_park_with_its_deadline_and_activity() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        arm_park(
            &path,
            ParkKind::RateLimit {
                deadline: ts(5_000),
            },
            ts(1_000),
        );
        assert_eq!(read_park(&path), Some(rate_record(5_000, 1_000, None, 0)));
    }

    #[test]
    fn arms_an_overloaded_park_with_its_activity() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        arm_park(
            &path,
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            ts(1_000),
        );
        assert_eq!(
            read_park(&path),
            Some(overloaded_record(1_500, 1_000, None, 0))
        );
    }

    #[test]
    fn a_steady_park_keeps_its_nudge_stamp_and_retry_count() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &overloaded_record(1_500, 1_000, Some(4_000), 3));
        // Re-arm at the same activity (the agent is still idle): retry state survives.
        arm_park(
            &path,
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            ts(1_000),
        );
        assert_eq!(
            read_park(&path),
            Some(overloaded_record(1_500, 1_000, Some(4_000), 3))
        );
    }

    #[test]
    fn a_new_park_baseline_resets_the_throttle_and_retry_count() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &overloaded_record(1_500, 1_000, Some(4_000), 3));
        // The agent acted (activity advanced) and re-parked: a fresh nudge may fire.
        arm_park(
            &path,
            ParkKind::RateLimit {
                deadline: ts(9_000),
            },
            ts(8_000),
        );
        assert_eq!(read_park(&path), Some(rate_record(9_000, 8_000, None, 0)));
    }

    #[test]
    fn a_new_park_class_resets_the_throttle_and_retry_count() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 4));
        arm_park(
            &path,
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            ts(1_000),
        );
        assert_eq!(
            read_park(&path),
            Some(overloaded_record(1_500, 1_000, None, 0))
        );
    }

    #[test]
    fn still_parked_tracks_frozen_activity() {
        let record = rate_record(5_000, 1_000, None, 0);
        assert!(still_parked(&record, ts(1_000)));
        assert!(!still_parked(&record, ts(1_200)));
    }

    #[test]
    fn rate_limit_nudge_waits_for_the_deadline_then_fires() {
        let record = rate_record(5_000, 1_000, None, 0);
        assert!(!due(&record, 4_999, &[], 10));
        assert!(due(&record, 5_000, &[], 10));
    }

    #[test]
    fn rate_limit_recent_nudge_throttles_the_next() {
        // Last nudge at 5_000; the retry interval is 120s.
        let record = rate_record(5_000, 1_000, Some(5_000), 1);
        assert!(!due(&record, 5_060, &[], 10));
        assert!(due(&record, 5_200, &[], 10));
    }

    #[test]
    fn rate_limit_retry_cap_stops_further_nudges() {
        let at_cap = rate_record(5_000, 1_000, Some(5_000), 3);
        assert!(!due(&at_cap, 9_000, &[], 3));

        let before_cap = rate_record(5_000, 1_000, Some(5_000), 2);
        assert!(due(&before_cap, 5_200, &[], 3));
    }

    #[test]
    fn overload_backoff_expands_then_repeats_the_last_step() {
        assert_eq!(overload_backoff(0, &[60, 120, 180]).as_secs(), 60);
        assert_eq!(overload_backoff(1, &[60, 120, 180]).as_secs(), 120);
        assert_eq!(overload_backoff(2, &[60, 120, 180]).as_secs(), 180);
        assert_eq!(overload_backoff(9, &[60, 120, 180]).as_secs(), 180);
        assert_eq!(overload_backoff(0, &[]).as_secs(), 300);
    }

    #[test]
    fn first_overloaded_nudge_waits_from_the_park_time() {
        let record = overloaded_record(1_000, 100, None, 0);
        assert!(!due(&record, 1_059, &[60, 120, 180], 10));
        assert!(due(&record, 1_060, &[60, 120, 180], 10));
    }

    #[test]
    fn overloaded_retries_wait_on_their_backoff_step() {
        let second = overloaded_record(1_000, 100, Some(1_060), 1);
        assert!(!due(&second, 1_179, &[60, 120, 180], 10));
        assert!(due(&second, 1_180, &[60, 120, 180], 10));

        let later = overloaded_record(1_000, 100, Some(1_180), 2);
        assert!(!due(&later, 1_359, &[60, 120, 180], 10));
        assert!(due(&later, 1_360, &[60, 120, 180], 10));
    }

    #[test]
    fn overloaded_retry_cap_stops_further_nudges() {
        let at_cap = overloaded_record(1_000, 100, Some(1_000), 10);
        assert!(!due(&at_cap, 9_000, &[60, 120, 180], 10));

        let before_cap = overloaded_record(1_000, 100, Some(1_000), 9);
        assert!(due(&before_cap, 1_180, &[60, 120, 180], 10));
    }

    #[test]
    fn recovered_budget_clears_a_stale_rate_limit_record() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 1));
        super::super::rate_limits::write_rate_limits_cache(
            &runtime.shared_rate_limits_path(),
            &super::super::rate_limits::RateLimitsCache {
                refreshed_at_ms: 0,
                windows: [(
                    "claude".to_owned(),
                    AgentRateLimits {
                        windows: vec![window(20, 9_000)],
                    },
                )]
                .into_iter()
                .collect(),
                pending: Default::default(),
            },
        );
        let mut snapshot = SidebarSnapshot::build_with_agents(
            runtime.workspace_id.clone(),
            Vec::new(),
            vec![agent(1_000)],
            ts(6_000),
        );
        snapshot.now = ts(6_000);
        resume_parked(
            &snapshot,
            &runtime,
            &ResumeConfig {
                auto_continue: true,
                ..ResumeConfig::default()
            },
        );
        assert_eq!(read_park(&path), None);
    }

    #[test]
    fn nudging_records_the_time_and_increments_retries() {
        let nudged = nudged_record(overloaded_record(1_000, 100, None, 2), ts(1_060));
        assert_eq!(nudged.last_nudge_at, Some(ts(1_060)));
        assert_eq!(nudged.retries, 3);
    }
}
