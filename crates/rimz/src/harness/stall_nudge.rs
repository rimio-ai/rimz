//! Once-per-visit nudges for teams whose stuck-stage projection has fired.
//! Leader routing comes from the host's room-root-resolved team table, never the checkout.

use std::cmp::Reverse;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::config::{MachineConfig, TeamsConfig};
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};

pub mod helper;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StallNudgeRequest {
    pub workspace_id: WorkspaceId,
    pub checkout: PathBuf,
    pub team: String,
    pub leader_role: Option<String>,
    pub stage: String,
    pub visit_started_at: Timestamp,
    pub stalled_since: Timestamp,
    pub target: (AgentKind, AgentSessionId),
    pub members: Vec<(AgentKind, AgentSessionId)>,
}

#[derive(Serialize, Deserialize)]
struct FireRecord {
    fired_for: Timestamp,
}

const NUDGE_AFTER_SECS: i64 = 55 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Off,
    On,
    Shadowed,
}

pub(crate) fn decision(config: &MachineConfig) -> Decision {
    if !config.harness.stall_nudge {
        return Decision::Off;
    }
    if i64::from(config.agents.attention.stalled_after_secs.get()) >= NUDGE_AFTER_SECS {
        return Decision::Shadowed;
    }
    Decision::On
}

pub(crate) fn nudge_stalled_teams(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    teams: &TeamsConfig,
) {
    nudge_stalled_teams_with(snapshot, runtime, config, teams, |request| {
        let args = crate::child_process::agent_helper_argv("stall-nudge", request);
        match crate::child_process::spawn_detached_rimz(runtime, args, "team-stall-nudge") {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, team = %request.team, "failed to spawn team stall nudge");
                false
            }
        }
    });
}

fn nudge_stalled_teams_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    teams: &TeamsConfig,
    mut spawn: impl FnMut(&StallNudgeRequest) -> bool,
) {
    if decision(config) != Decision::On {
        return;
    }
    for group in snapshot.worktree_groups.iter() {
        let Some(stalled_since) = group.stalled_since else {
            continue;
        };
        if snapshot.now.duration_since(stalled_since).as_secs() < NUDGE_AFTER_SECS {
            continue;
        }
        let Some(pipeline) = group.pipeline.as_ref() else {
            continue;
        };
        let (Some(visit_started_at), Some(team), Some(owner)) = (
            pipeline.stage_started_at,
            group.team.as_ref(),
            group.pipeline_owner(),
        ) else {
            continue;
        };
        let Some(team_config) = teams.0.get(team) else {
            continue;
        };
        let Some(checkout) = owner.lane_path() else {
            continue;
        };
        let unit = group.unit_key(owner);
        let members = group.rows.iter().filter(|row| group.unit_key(row) == unit);
        let Some(target) = members.clone().max_by_key(|row| {
            (
                row.last_activity,
                row.id == owner.id,
                Reverse(row.id.as_str()),
            )
        }) else {
            continue;
        };
        let record_path = runtime.live_path("stall-nudge").join(format!(
            "{}.json",
            crate::store::sidecar::digest("group", &group.key)
        ));
        let record = std::fs::read(&record_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FireRecord>(&bytes).ok());
        if record.is_some_and(|record| record.fired_for == stalled_since) {
            continue;
        }
        let request = StallNudgeRequest {
            workspace_id: runtime.workspace_id.clone(),
            checkout: checkout.into(),
            team: team.clone(),
            leader_role: team_config.leader_role().map(str::to_owned),
            stage: pipeline.stage.clone(),
            visit_started_at,
            stalled_since,
            target: row_identity(target),
            members: members.map(row_identity).collect(),
        };
        if spawn(&request)
            && let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(
                &record_path,
                &FireRecord {
                    fired_for: stalled_since,
                },
            )
        {
            tracing::warn!(%error, "failed to record team stall nudge pacing");
        }
    }
}

fn row_identity(row: &SidebarRow) -> (AgentKind, AgentSessionId) {
    (
        AgentKind::new_unchecked(row.name.as_str()),
        row.id.as_str().into(),
    )
}

#[cfg(test)]
mod tests;
