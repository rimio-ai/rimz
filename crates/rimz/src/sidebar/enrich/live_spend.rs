use std::collections::BTreeMap;

use crate::agents::spending::{
    LiveSpendBaselines, SpendScope, origin_path, read_live_spend_baselines, today_spend_live_usd,
    write_live_spend_baselines,
};
use crate::{RuntimePaths, SidebarSnapshot};

/// Stamp the cockpit's live headline spend onto the snapshot: the published
/// walk's exact figure plus each live row's overshoot over its publish-time
/// baseline ([`today_spend_live_usd`]), so the headline tracks every
/// context sidecar push instead of waiting out the walk's TTL. Shared by the
/// producing CLI and the consumer fold, so every tab in a room paints the same
/// figure; zero — an empty room in an unspent window — stays `None` and the
/// cockpit keeps its bare `¤` line.
pub fn apply_live_today_spend(
    snapshot: &mut SidebarSnapshot,
    walked_headline_usd: f64,
    published_at_ms: u64,
    baselines: &BTreeMap<String, f64>,
) {
    let live = today_spend_live_usd(
        walked_headline_usd,
        live_row_costs(snapshot),
        baselines,
        published_at_ms,
    );
    snapshot.today_spend_live_usd = (live > 0.0).then_some(live);
}

pub(super) fn refresh_live_spend_baselines(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    observed_walk_ms: u64,
    persist: bool,
) -> LiveSpendBaselines {
    let path = runtime.live_spend_baselines_path();
    let mut baselines = read_live_spend_baselines(&path);
    // Producer-only: the elected elder captures the per-room baselines at each
    // new walk; consumer tabs read what it wrote.
    if persist && observed_walk_ms > 0 && observed_walk_ms > baselines.observed_walk_ms {
        baselines = LiveSpendBaselines {
            observed_walk_ms,
            baselines: live_row_costs(snapshot)
                .map(|(id, usd, _)| (id.to_owned(), usd))
                .collect(),
        };
        write_live_spend_baselines(&path, &baselines);
    }
    baselines
}

/// Every in-scope agent row's live statusline cost: `(row id,
/// total_cost_usd, registered-at ms)` triples — the overlay's per-session
/// input, and (collected to a map) the baseline set the producer stamps at each
/// walk publish. Rows without an absolute worktree path, or whose path sits
/// outside the room's project root plus grouped worktree roots, are omitted so
/// the overlay stays aligned with the workspace-scoped transcript tally.
pub fn live_row_costs(
    snapshot: &SidebarSnapshot,
) -> impl Iterator<Item = (&str, f64, Option<u64>)> {
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter_map(move |row| {
            let origin = origin_path(row.worktree_path.as_deref())?;
            if !scope.contains(&origin) {
                return None;
            }
            let usd = row
                .as_agent()
                .and_then(|agent| agent.context.as_ref())
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)?;
            let registered_ms = row
                .as_agent()
                .and_then(|agent| agent.registered_at)
                .map(|at| at.as_millisecond().max(0) as u64);
            Some((row.id.as_str(), usd, registered_ms))
        })
}
