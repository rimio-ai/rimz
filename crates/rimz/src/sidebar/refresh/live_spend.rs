use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::SidebarSnapshot;
use crate::agents::find_adapter;
use crate::agents::spending::{
    SESSION_GAP_SECS, SpendScope, WorkspaceSpendingCache, live_session_keys, origin_path,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveCardSpend {
    pub(crate) row_id: String,
    pub(crate) session_keys: Vec<String>,
    pub(crate) cost_usd: f64,
    pub(crate) registered_at_ms: Option<u64>,
}

/// Stamp the cockpit's live headline spend onto the snapshot from the single
/// workspace cache: walked headline USD with live card sessions excluded, plus
/// the full current cost of those cards and cards born after the cache publish.
/// Shared by the producing CLI and consumer folds, so every tab in a room
/// paints the same figure; zero is explicit so the cockpit can render `$0.00`.
pub fn apply_live_today_spend(snapshot: &mut SidebarSnapshot, workspace: &WorkspaceSpendingCache) {
    let live = live_card_sessions(snapshot)
        .into_iter()
        .filter(|card| {
            card.session_keys
                .iter()
                .any(|key| workspace.live_excluded.contains(key))
                || card
                    .registered_at_ms
                    .is_some_and(|at| at > workspace.refreshed_at_ms)
        })
        .map(|card| card.cost_usd.max(0.0))
        .sum::<f64>();
    snapshot.today_spend_live_usd = Some(workspace.tally.headline.usd + live);
    snapshot.today_spend_epoch_secs = Some(workspace.headline_cutoff_secs);
}

pub(crate) fn live_excluded_sessions(cards: &[LiveCardSpend]) -> BTreeSet<String> {
    cards
        .iter()
        .flat_map(|card| card.session_keys.iter().cloned())
        .collect()
}

/// Every active in-scope agent row's live statusline cost and the transcript
/// session keys the walker can exclude. Rows without an absolute worktree path,
/// a matching agent state, an absolute transcript path, or a finite cost are
/// omitted so the overlay stays aligned with the workspace-scoped transcript
/// tally.
pub(crate) fn live_card_sessions(snapshot: &SidebarSnapshot) -> Vec<LiveCardSpend> {
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
            let origin = origin_path(row.worktree_path.as_deref())?;
            if !scope.contains(&origin) {
                return None;
            }
            let card = row.as_agent()?;
            let usd = card
                .context
                .as_ref()
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)?;
            if !usd.is_finite() || usd < 0.0 {
                return None;
            }
            let agent = agents.get(row.id.as_str())?;
            let adapter = find_adapter(agent.kind.as_str())?;
            let transcript_path = PathBuf::from(agent.transcript_path.as_deref()?);
            if !transcript_path.is_absolute() {
                return None;
            }
            let session_keys =
                live_session_keys(adapter, agent.agent_id.as_str(), &transcript_path);
            let registered_at_ms = card
                .registered_at
                .or(agent.registered_at)
                .map(|at| at.as_millisecond().max(0) as u64);
            Some(LiveCardSpend {
                row_id: row.id.clone(),
                session_keys,
                cost_usd: usd,
                registered_at_ms,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::RuntimePaths;
    use crate::agents::{AgentState, AgentStatus, TurnPhase};
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::sidebar::enrich::{FoldOpts, enrich};
    use crate::sidebar::refresh::AccountsCache;
    use crate::sidebar::test_support::{activity_row, worktree_group};
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::atomic;

    fn cached_opts() -> FoldOpts<'static> {
        FoldOpts {
            producing: false,
            fresh_roots: None,
            config: None,
            lanes: None,
        }
    }

    fn cost_row(id: &str, usd: Option<f64>, registered_at: Option<Timestamp>) -> crate::SidebarRow {
        cost_row_at(id, usd, registered_at, Path::new("/repo/wt"), None)
    }

    fn cost_row_at(
        id: &str,
        usd: Option<f64>,
        registered_at: Option<Timestamp>,
        worktree_path: &Path,
        last_activity: Option<Timestamp>,
    ) -> crate::SidebarRow {
        let now = Timestamp::from_second(1_750_000_000).unwrap();
        let mut row = activity_row(
            true,
            Some(AgentStatus::Running),
            last_activity.unwrap_or(now),
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
            turn_interrupted: None,
            observed_at: now,
        });
        row
    }

    fn agent_state(
        id: &str,
        worktree_path: &Path,
        transcript_path: &Path,
        now: Timestamp,
    ) -> AgentState {
        AgentState {
            agent_id: id.into(),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            name_explicit: false,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some(worktree_path.display().to_string()),
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: Some(transcript_path.display().to_string()),
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
            waiting_since: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }

    fn transcript(id: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/claude/{id}/chat.jsonl"))
    }

    #[test]
    fn consumer_enrich_reads_live_excluded_workspace_cache_without_writing() {
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
        let build_snapshot = || -> SidebarSnapshot {
            let mut snapshot =
                SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), published)
                    .with_project_root(Some(dir.path().to_path_buf()));
            snapshot.agents = vec![
                agent_state("baselined", &wt, &transcript("baselined"), published),
                agent_state("external", external, &transcript("external"), published),
            ];
            snapshot.worktree_groups = vec![worktree_group(
                &wt,
                vec![
                    cost_row_at("baselined", Some(2.00), Some(before), &wt, None),
                    cost_row_at("external", Some(9.00), Some(before), external, None),
                ],
            )];
            snapshot
        };
        let config = crate::config::MachineConfig::load_lenient();
        let worktree_home =
            crate::worktree::worktree_parent(dir.path(), &config.agents.worktree).ok();
        let scope = SpendScope::for_workspace(Some(dir.path()), &[], worktree_home.as_deref());
        let scope_hash = scope.hash();

        crate::agents::spending::write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            walk_ms,
            &crate::agents::spending::Spending::default(),
        );
        let mut tally = crate::agents::spending::SpendTally::default();
        tally.headline.usd = 10.0;
        let workspace_cache = crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: walk_ms,
            scope_hash: scope_hash.clone(),
            tally,
            live_excluded: BTreeSet::from(["claude:baselined".to_owned()]),
            ..Default::default()
        };
        let workspace_path = runtime.workspace_spending_path(&scope_hash);
        crate::agents::spending::write_workspace_spending_cache(&workspace_path, &workspace_cache);
        let before_bytes = std::fs::read(&workspace_path).unwrap();

        let enriched = enrich(
            build_snapshot(),
            None,
            &runtime,
            None,
            None,
            cached_opts(),
            &crate::diag::DiagSink::disabled(),
        );
        assert_eq!(std::fs::read(&workspace_path).unwrap(), before_bytes);
        assert_eq!(enriched.today_spend_live_usd, Some(12.0));
    }

    #[test]
    fn apply_live_today_spend_adds_excluded_cards_and_newborns() {
        let published = Timestamp::from_second(1_750_000_000).unwrap();
        let published_ms = published.as_millisecond() as u64;
        let before = published - SignedDuration::from_secs(600);
        let after = published + SignedDuration::from_secs(5);
        let stale = published - SignedDuration::from_secs(SESSION_GAP_SECS as i64 + 1);
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
        snapshot.agents = [
            "baselined",
            "newborn",
            "unbaselined",
            "costless",
            "stale",
            "linked-newborn",
        ]
        .into_iter()
        .map(|id| {
            let path = if id == "linked-newborn" { linked } else { wt };
            agent_state(id, path, &transcript(id), published)
        })
        .collect();
        snapshot.worktree_groups = vec![
            worktree_group(
                wt,
                vec![
                    cost_row("baselined", Some(5.50), Some(before)),
                    cost_row("newborn", Some(0.30), Some(after)),
                    cost_row("unbaselined", Some(2.00), Some(before)),
                    cost_row("costless", None, Some(before)),
                    cost_row_at("stale", Some(9.00), Some(before), wt, Some(stale)),
                ],
            ),
            worktree_group(
                linked,
                vec![cost_row_at(
                    "linked-newborn",
                    Some(0.20),
                    Some(after),
                    linked,
                    None,
                )],
            ),
        ];

        let mut tally = crate::agents::spending::SpendTally::default();
        tally.headline.usd = 10.0;
        let workspace = WorkspaceSpendingCache {
            refreshed_at_ms: published_ms,
            tally,
            headline_cutoff_secs: 123,
            live_excluded: BTreeSet::from(["claude:baselined".to_owned()]),
            ..Default::default()
        };

        apply_live_today_spend(&mut snapshot, &workspace);
        let live = snapshot.today_spend_live_usd.expect("live spend stamps");
        assert!((live - 16.00).abs() < 1e-9);
        assert_eq!(snapshot.today_spend_epoch_secs, Some(123));

        let mut empty = SidebarSnapshot::build(
            WorkspaceId::from_project_root(wt),
            Vec::new(),
            Vec::new(),
            published,
        )
        .with_project_root(Some(Path::new("/repo").to_path_buf()));
        apply_live_today_spend(&mut empty, &WorkspaceSpendingCache::default());
        assert_eq!(empty.today_spend_live_usd, Some(0.0));
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
        snapshot.agents = vec![
            agent_state(
                "external-new",
                other,
                &transcript("external-new"),
                published,
            ),
            agent_state(
                "external-baselined",
                other,
                &transcript("external-baselined"),
                published,
            ),
        ];
        snapshot.worktree_groups = vec![worktree_group(
            other,
            vec![
                cost_row_at("external-new", Some(2.00), Some(after), other, None),
                cost_row_at("external-baselined", Some(5.50), Some(before), other, None),
            ],
        )];

        let workspace = WorkspaceSpendingCache {
            refreshed_at_ms: published_ms,
            live_excluded: BTreeSet::from(["claude:external-baselined".to_owned()]),
            ..Default::default()
        };

        apply_live_today_spend(&mut snapshot, &workspace);
        assert_eq!(snapshot.today_spend_live_usd, Some(0.0));
    }
}
