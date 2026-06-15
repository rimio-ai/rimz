//! Producer-side auto-continue: resume a rate-limit-parked agent the moment its
//! 5h/7d window resets, by nudging its live pane.
//!
//! Opt-in ([`ResumeConfig::auto_continue`]). The producer arms the resume while
//! the park is fresh and fires it once the clock reaches the window's reset,
//! recording everything it needs in between so the decision never depends on the
//! ephemeral per-session context surviving the wait:
//!
//! - **Arm.** Each frame an agent is parked on a `rate_limit` certificate with a
//!   spent, unreset window ([`crate::feed::rate_limit_resume_arm`]), the producer
//!   writes a durable [`ParkRecord`] capturing the window's reset deadline and the
//!   agent's frozen `last_activity`. This happens within seconds of the park —
//!   long before the 3h context TTL or any reset — so the deadline is captured
//!   while the reading is still spent.
//! - **Fire.** Once the window resets the live reading turns over (a Codex
//!   app-server refresh rolls it forward; a Claude context sidecar may expire
//!   entirely), but the record stands. When its deadline passes and the agent is
//!   still idle (`last_activity` unchanged), the producer spawns the detached
//!   `rimz agents auto-continue` helper that types the nudge and writes the
//!   `agent.resumed` audit record.
//! - **Clear.** Any activity since the park (the nudge took, or the agent woke on
//!   its own) advances `last_activity`, and the stale record is removed.
//!
//! This module owns only the durable record, the pane join, and the spawn — the
//! arm decision is the pure, unit-tested [`crate::feed::rate_limit_resume_arm`].

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::config::ResumeConfig;
use crate::feed::{AgentState, rate_limit_resume_arm};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::atomic::write_temp_then_rename_cache;
use crate::ledger::snapshot::PaneAgent;
use crate::sidebar::timing::AUTO_CONTINUE_RETRY_INTERVAL;

use super::SidebarSnapshot;

/// A durable record of one rate-limit park: written while the park is fresh, read
/// after its window resets. It outlives the per-session context the park was first
/// seen through, so a resume survives both an expired context sidecar and a fresh
/// non-spent reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ParkRecord {
    /// The latest reset among the windows that were spent when the park was armed
    /// — the instant the turn may resume.
    deadline: Timestamp,
    /// The agent's rollup `last_activity` at arm time. Unchanged means the agent
    /// has done nothing since: still parked, safe to nudge. Advanced means it woke
    /// (our nudge took, or it resumed on its own), so the record is stale.
    parked_at_activity: Timestamp,
    /// When the last nudge was sent, throttling re-nudges so a nudge that fails to
    /// wake a still-parked agent is retried without spamming a working one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_nudge_at: Option<Timestamp>,
}

/// Arm or fire each rate-limit park when the user has opted in. Best-effort: an
/// empty nudge text, an agent with no live pane, or a spawn failure each drops one
/// resume and leaves the record for the next frame. Producer-only — one elected
/// producer drives one room, and the records live in that room's runtime dir, so a
/// window reset nudges its agent once.
pub(super) fn resume_rate_limit_parked(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
) {
    if !config.auto_continue {
        return;
    }
    let text = config.auto_continue_text.trim();
    if text.is_empty() {
        return;
    }
    let now = snapshot.now;
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        match rate_limit_resume_arm(agent, now) {
            // Still parked and spent: keep the deadline fresh while we can read it.
            Some(deadline) => arm_park(&path, deadline, agent.last_activity),
            // Either never parked, or the window has reset (or its reading turned
            // over). Fire off the durable record if one is due.
            None => fire_if_due(snapshot, runtime, agent, &path, now, text),
        }
    }
}

/// Capture (or refresh) the park's reset deadline while the reading is still
/// spent. A new park baseline — the first arm, or the agent acted and re-parked —
/// starts a fresh nudge throttle; a steady park keeps its last nudge stamp.
/// Write-if-changed, so a frozen park costs one write, not one per frame.
fn arm_park(path: &Path, deadline: Timestamp, last_activity: Timestamp) {
    let prior = read_park(path);
    let last_nudge_at = prior
        .as_ref()
        .filter(|record| record.parked_at_activity == last_activity)
        .and_then(|record| record.last_nudge_at);
    let next = ParkRecord {
        deadline,
        parked_at_activity: last_activity,
        last_nudge_at,
    };
    if prior.as_ref() != Some(&next) {
        write_park(path, &next);
    }
}

/// Fire a parked agent's resume when its recorded deadline has passed and it is
/// still idle. A woken agent (activity advanced) clears the record; a pane that
/// has not appeared yet, a deadline still ahead, or a recent nudge each waits.
fn fire_if_due(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    agent: &AgentState,
    path: &Path,
    now: Timestamp,
    text: &str,
) {
    let Some(record) = read_park(path) else {
        return;
    };
    if !still_parked(&record, agent.last_activity) {
        remove_park(path);
        return;
    }
    if !nudge_due(&record, now) {
        return;
    }
    let Some(pane_id) = live_pane(&snapshot.agent_panes, &agent.kind, &agent.agent_id) else {
        return;
    };
    spawn_auto_continue(runtime, &agent.kind, &agent.agent_id, &pane_id, text);
    write_park(
        path,
        &ParkRecord {
            last_nudge_at: Some(now),
            ..record
        },
    );
}

/// Whether the agent has done nothing since the park was armed — its rollup
/// `last_activity` still matches. A changed activity means it woke (our nudge
/// took, or it resumed on its own), so the record is stale.
fn still_parked(record: &ParkRecord, last_activity: Timestamp) -> bool {
    record.parked_at_activity == last_activity
}

/// Whether a nudge is due: the window has reset (`now >= deadline`) and the last
/// nudge, if any, is older than the retry interval.
fn nudge_due(record: &ParkRecord, now: Timestamp) -> bool {
    now >= record.deadline
        && record.last_nudge_at.is_none_or(|at| {
            now.as_second() - at.as_second() >= AUTO_CONTINUE_RETRY_INTERVAL.as_secs() as i64
        })
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
            "sidebar: failed to record rate-limit park",
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
/// is logged and dropped — the record stays until the next due frame.
fn spawn_auto_continue(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    pane_id: &PaneId,
    text: &str,
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
    let mut cmd = std::process::Command::new(exe);
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
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %kind,
        "sidebar: auto-continuing rate-limit-parked agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agent-auto-continue") {
        tracing::warn!(
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
    use crate::ids::WorkspaceId;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).expect("valid test timestamp")
    }

    fn record(deadline: i64, activity: i64, last_nudge: Option<i64>) -> ParkRecord {
        ParkRecord {
            deadline: ts(deadline),
            parked_at_activity: ts(activity),
            last_nudge_at: last_nudge.map(ts),
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

    #[test]
    fn arms_a_park_with_its_deadline_and_activity() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        arm_park(&path, ts(5_000), ts(1_000));
        assert_eq!(read_park(&path), Some(record(5_000, 1_000, None)));
    }

    #[test]
    fn a_steady_park_keeps_its_nudge_stamp() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &record(5_000, 1_000, Some(4_000)));
        // Re-arm at the same activity (the agent is still idle): the nudge stamp survives.
        arm_park(&path, ts(5_000), ts(1_000));
        assert_eq!(
            read_park(&path).and_then(|record| record.last_nudge_at),
            Some(ts(4_000))
        );
    }

    #[test]
    fn a_new_park_baseline_resets_the_throttle() {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        write_park(&path, &record(5_000, 1_000, Some(4_000)));
        // The agent acted (activity advanced) and re-parked: a fresh nudge may fire.
        arm_park(&path, ts(9_000), ts(8_000));
        assert_eq!(read_park(&path), Some(record(9_000, 8_000, None)));
    }

    #[test]
    fn still_parked_tracks_frozen_activity() {
        let record = record(5_000, 1_000, None);
        assert!(still_parked(&record, ts(1_000)));
        assert!(!still_parked(&record, ts(1_200)));
    }

    #[test]
    fn nudge_waits_for_the_deadline_then_fires() {
        let record = record(5_000, 1_000, None);
        assert!(!nudge_due(&record, ts(4_999)));
        assert!(nudge_due(&record, ts(5_000)));
    }

    #[test]
    fn a_recent_nudge_throttles_the_next() {
        // Last nudge at 5_000; the retry interval is 120s.
        let record = record(5_000, 1_000, Some(5_000));
        assert!(!nudge_due(&record, ts(5_060)));
        assert!(nudge_due(&record, ts(5_200)));
    }
}
