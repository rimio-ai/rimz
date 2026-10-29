use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agents::find_definition;
use crate::agents::spending::{
    NO_BURST_CUTOFF, SESSION_GAP_SECS, SpendScope, WorkspaceSpendingCache, live_session_keys,
    origin_path,
};
use crate::store::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, PartialEq)]
struct LiveCardSpend {
    pub(crate) session_keys: Vec<String>,
    pub(crate) cost_usd: f64,
}

/// Stamp the cockpit's live headline spend onto the snapshot from the single
/// workspace cache: walked headline USD plus each live card's cost since its
/// walked session baseline. Shared by the producing CLI and consumer folds, so
/// every tab in a room paints the same figure; zero is explicit so the cockpit
/// can render `$0.00`.
pub(in crate::sidebar) fn apply_live_today_spend(
    snapshot: &mut SidebarSnapshot,
    workspace: &WorkspaceSpendingCache,
) {
    let live = if workspace.headline_cutoff_secs == NO_BURST_CUTOFF {
        0.0
    } else {
        live_spend_addback(snapshot, workspace)
    };
    snapshot.today_spend_live_usd = Some(workspace.tally.headline.usd + live);
    snapshot.today_spend_epoch_secs = Some(workspace.headline_cutoff_secs);
}

/// Stamp the room's local-calendar-day spend independently of the configured
/// headline window. This is the enforcement and daily-cap display figure.
pub fn apply_live_day_spend(snapshot: &mut SidebarSnapshot, workspace: &WorkspaceSpendingCache) {
    let live = live_spend_addback(snapshot, workspace);
    snapshot.fleet_day_spend_usd = Some(workspace.day.usd + live);
    snapshot.fleet_day_spend_epoch_secs = Some(workspace.day_cutoff_secs);
}

fn live_spend_addback(snapshot: &SidebarSnapshot, workspace: &WorkspaceSpendingCache) -> f64 {
    live_card_sessions(snapshot)
        .into_iter()
        .map(|card| {
            let baseline = card
                .session_keys
                .iter()
                .filter_map(|key| workspace.live_baselines.get(key).copied())
                .fold(0.0_f64, f64::max);
            (card.cost_usd - baseline).max(0.0)
        })
        .sum::<f64>()
}

/// Every active in-scope agent row's live statusline cost and candidate walked
/// baseline keys. Rows without an absolute worktree path, a matching agent
/// state, an absolute transcript path, or a finite cost are omitted so the
/// overlay stays aligned with the workspace-scoped transcript tally.
fn live_card_sessions(snapshot: &SidebarSnapshot) -> Vec<LiveCardSpend> {
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let agents = snapshot
        .agents
        .iter()
        .map(|agent| (agent.agent_id.as_str(), agent))
        .collect::<BTreeMap<_, _>>();
    let now_secs = snapshot.now.as_second();
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter_map(|row| {
            if now_secs.saturating_sub(row.last_activity.as_second()) >= SESSION_GAP_SECS as i64 {
                return None;
            }
            let origin = origin_path(row.lane_path())?;
            if !scope.contains(&origin) {
                return None;
            }
            let card = row.as_agent()?;
            let agent = agents.get(row.id.as_str())?;
            let usd = card
                .context
                .as_ref()
                .or(agent.context.as_ref())
                .and_then(|context| context.cost.as_ref())
                .filter(|cost| cost.coverage.contributes_to_live_spend())
                .and_then(|cost| cost.total_cost_usd)?;
            if !usd.is_finite() || usd < 0.0 {
                return None;
            }
            let adapter = find_definition(agent.kind.as_str())?;
            let transcript_path = PathBuf::from(agent.transcript_path.as_deref()?);
            if !transcript_path.is_absolute() {
                return None;
            }
            let session_keys =
                live_session_keys(adapter, agent.agent_id.as_str(), &transcript_path);
            Some(LiveCardSpend {
                session_keys,
                cost_usd: usd,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
