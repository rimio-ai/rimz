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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::ledger::atomic;
    use crate::sidebar::cache::{AccountsCache, unix_now_ms};
    use crate::sidebar::enrich::{EnrichMode, HeavyLanes, enrich};
    use crate::sidebar::test_support::{activity_row, worktree_group};

    /// A cost-bearing agent row for the overlay glue: `id`, the statusline
    /// `total_cost_usd`, and the registration stamp are the three fields
    /// [`live_row_costs`] projects.
    fn cost_row(id: &str, usd: Option<f64>, registered_at: Option<Timestamp>) -> crate::SidebarRow {
        cost_row_at(id, usd, registered_at, Path::new("/repo/wt"))
    }

    fn cost_row_at(
        id: &str,
        usd: Option<f64>,
        registered_at: Option<Timestamp>,
        worktree_path: &Path,
    ) -> crate::SidebarRow {
        let mut row = activity_row(
            true,
            Some(AgentStatus::Running),
            Timestamp::from_second(1_750_000_000).unwrap(),
            worktree_path,
        );
        row.id = id.to_owned();
        let agent = row.as_agent_mut().unwrap();
        agent.registered_at = registered_at;
        agent.context = usd.map(|usd| crate::agents::AgentContext {
            source: "claude".to_owned(),
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
            cost: Some(crate::agents::AgentCost {
                total_cost_usd: Some(usd),
                ..Default::default()
            }),
            tokens: None,
            rate_limits: None,
            pr: None,
            account: None,
            turn_error: None,
            turn_complete: None,
            observed_at: Timestamp::from_second(1_750_000_000).unwrap(),
        });
        row
    }

    #[test]
    fn live_spend_baselines_are_written_only_by_producer_enrich() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        atomic::write_temp_then_rename_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                refreshed_at_ms: unix_now_ms(),
                accounts: BTreeMap::new(),
                ok: true,
            },
        )
        .unwrap();

        let published = Timestamp::from_second(1_750_000_000).unwrap();
        let walk_ms = published.as_millisecond() as u64;
        let before = published - SignedDuration::from_secs(60);
        let wt = dir.path().join("wt");
        let external = Path::new("/tmp/rimz-other-project");
        let build_snapshot = || {
            let mut snapshot =
                SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), published)
                    .with_project_root(Some(dir.path().to_path_buf()));
            snapshot.worktree_groups = vec![worktree_group(
                &wt,
                vec![
                    cost_row_at("baselined", Some(2.00), Some(before), &wt),
                    cost_row_at("external", Some(9.00), Some(before), external),
                ],
            )];
            snapshot
        };

        let spending = crate::agents::spending::Spending::default();
        crate::agents::spending::write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            walk_ms,
            &spending,
        );
        let baseline_path = runtime.live_spend_baselines_path();

        let _ = enrich(
            build_snapshot(),
            None,
            &runtime,
            None,
            EnrichMode::Cached,
            None,
        );
        assert!(
            !baseline_path.exists(),
            "consumer folds read baselines but never create the sidecar"
        );

        let stale = crate::agents::spending::LiveSpendBaselines {
            observed_walk_ms: 10,
            baselines: BTreeMap::from([("old".to_owned(), 0.50)]),
        };
        crate::agents::spending::write_live_spend_baselines(&baseline_path, &stale);
        let _ = enrich(
            build_snapshot(),
            None,
            &runtime,
            None,
            EnrichMode::Cached,
            None,
        );
        assert_eq!(
            crate::agents::spending::read_live_spend_baselines(&baseline_path),
            stale,
            "consumer folds do not advance an existing baseline sidecar"
        );

        let compute_spending = |_: &SidebarSnapshot| crate::agents::spending::SpendingCaches {
            workspace: crate::agents::spending::WorkspaceSpendingCache {
                refreshed_at_ms: walk_ms,
                ..Default::default()
            },
            ..Default::default()
        };
        let refresh_git = |_: &mut SidebarSnapshot| {};
        let _ = enrich(
            build_snapshot(),
            None,
            &runtime,
            None,
            EnrichMode::Producing {
                roots: None,
                heavy: HeavyLanes::Refresh {
                    compute_spending: &compute_spending,
                    refresh_git: &refresh_git,
                },
                config: Box::new(crate::config::MachineConfig::default()),
            },
            None,
        );
        let advanced = crate::agents::spending::read_live_spend_baselines(&baseline_path);
        assert_eq!(advanced.observed_walk_ms, walk_ms);
        assert_eq!(advanced.baselines.get("baselined"), Some(&2.00));
        assert!(
            !advanced.baselines.contains_key("external"),
            "producer baselines are captured from the workspace-scoped live rows"
        );
        assert!(
            !advanced.baselines.contains_key("old"),
            "a producer walk replaces the prior baseline set for the new stamp"
        );
    }

    /// The consumer overlay glue end-to-end over a built snapshot:
    /// [`live_row_costs`] projects each agent row's `(id, statusline cost,
    /// registered-at)` triple and [`apply_live_today_spend`] stamps the walked
    /// floor plus per-session overshoot — exercising the row-id ↔ baseline join
    /// the producer and every consumer tab rely on, the new-session rule against
    /// the cache's publish stamp, and the zero gate.
    #[test]
    fn apply_live_today_spend_stamps_overshoot_over_the_walked_floor() {
        let published = Timestamp::from_second(1_750_000_000).unwrap();
        let published_ms = published.as_millisecond() as u64;
        let before = published - SignedDuration::from_secs(600);
        let after = published + SignedDuration::from_secs(5);
        let wt = Path::new("/repo/wt");
        let linked = Path::new("/linked/wt");

        let mut snapshot = SidebarSnapshot::build(
            WorkspaceId::from_project_root(wt),
            Vec::new(),
            Vec::new(),
            published,
        )
        .with_project_root(Some(Path::new("/repo").to_path_buf()))
        .with_worktree_roots(vec![linked.to_path_buf()]);
        snapshot.worktree_groups = vec![
            worktree_group(
                wt,
                vec![
                    // Baselined at $5.00, now $5.50: contributes the $0.50 overshoot.
                    cost_row("baselined", Some(5.50), Some(before)),
                    // Born after the publish: the walk never saw it, whole cost counts.
                    cost_row("newborn", Some(0.30), Some(after)),
                    // Pre-publish but unbaselined (a race): fails safe to zero.
                    cost_row("unbaselined", Some(2.00), Some(before)),
                    // No statusline cost yet: skipped by the projection.
                    cost_row("costless", None, Some(before)),
                ],
            ),
            worktree_group(
                linked,
                vec![cost_row_at(
                    "linked-newborn",
                    Some(0.20),
                    Some(after),
                    linked,
                )],
            ),
        ];

        let baselines = BTreeMap::from([("baselined".to_owned(), 5.00)]);

        apply_live_today_spend(&mut snapshot, 10.0, published_ms, &baselines);
        let live = snapshot.today_spend_live_usd.expect("a spent day stamps");
        assert!(
            (live - 11.00).abs() < 1e-9,
            "walked 10.00 + 0.50 + 0.30 + 0.20"
        );

        // The zero gate: an empty room on an unspent day keeps the field bare so
        // the cockpit holds its bare `¤` line.
        let mut empty = SidebarSnapshot::build(
            WorkspaceId::from_project_root(wt),
            Vec::new(),
            Vec::new(),
            published,
        )
        .with_project_root(Some(Path::new("/repo").to_path_buf()));
        apply_live_today_spend(&mut empty, 0.0, 0, &BTreeMap::new());
        assert_eq!(empty.today_spend_live_usd, None);
    }

    #[test]
    fn apply_live_today_spend_excludes_out_of_scope_live_rows() {
        let published = Timestamp::from_second(1_750_000_000).unwrap();
        let published_ms = published.as_millisecond() as u64;
        let before = published - SignedDuration::from_secs(600);
        let after = published + SignedDuration::from_secs(5);
        let project = Path::new("/repo/main");
        let other = Path::new("/tmp/other");

        let mut snapshot = SidebarSnapshot::build(
            WorkspaceId::from_project_root(project),
            Vec::new(),
            Vec::new(),
            published,
        )
        .with_project_root(Some(project.to_path_buf()));
        snapshot.worktree_groups = vec![worktree_group(
            other,
            vec![
                cost_row_at("external-new", Some(2.00), Some(after), other),
                cost_row_at("external-baselined", Some(5.50), Some(before), other),
            ],
        )];

        let baselines = BTreeMap::from([("external-baselined".to_owned(), 5.00)]);

        apply_live_today_spend(&mut snapshot, 0.0, published_ms, &baselines);
        assert_eq!(
            snapshot.today_spend_live_usd, None,
            "out-of-scope live rows do not add newborn cost or baseline deltas"
        );
    }
}
