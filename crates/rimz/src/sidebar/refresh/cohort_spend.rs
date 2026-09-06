//! Producer-published lifetime effort for collapsed team cohorts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::AgentState;
use crate::agents::spending::{EffortParseMemo, EffortSessionRef};
use crate::store::active_time;
use crate::store::snapshot::{
    RollupCursor, SidebarCohortEffort, SidebarSeatEffort, SidebarSnapshot, SidebarWorktreeGroup,
};
use crate::{RuntimePaths, StatePaths};

use super::super::timing::COHORT_SPEND_TTL;

pub(in crate::sidebar) const COHORT_SPEND_CACHE_VERSION: u32 = 3;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CohortSpendCache {
    pub version: u32,
    pub refreshed_at_ms: u64,
    pub groups: BTreeMap<String, SidebarCohortEffort>,
}

pub fn read_cohort_spend_cache(path: &Path) -> CohortSpendCache {
    let Ok(bytes) = std::fs::read(path) else {
        return CohortSpendCache::default();
    };
    let Ok(cache) = serde_json::from_slice::<CohortSpendCache>(&bytes) else {
        return CohortSpendCache::default();
    };
    if cache.version == COHORT_SPEND_CACHE_VERSION {
        cache
    } else {
        CohortSpendCache::default()
    }
}

fn cache_due(cache: &CohortSpendCache, now_ms: u64) -> bool {
    cache.version != COHORT_SPEND_CACHE_VERSION
        || now_ms.saturating_sub(cache.refreshed_at_ms) > COHORT_SPEND_TTL.as_millis() as u64
}

pub(super) fn refresh_cohort_spend_for(
    snapshot: &SidebarSnapshot,
    state: &StatePaths,
    runtime: &RuntimePaths,
    active_grace_secs: u32,
    now_ms: u64,
    cursor: &mut RollupCursor,
    memo: &mut EffortParseMemo,
) {
    let path = runtime.cohort_spend_path();
    let current = read_cohort_spend_cache(&path);
    let needed = snapshot
        .worktree_groups
        .iter()
        .filter(|group| group.collapses())
        .map(|group| group.key.as_str())
        .collect::<BTreeSet<_>>();
    if !cache_due(&current, now_ms) && current.groups.keys().map(String::as_str).eq(needed) {
        return;
    }
    let Ok((_, agents, _)) = cursor.fold(state) else {
        return;
    };
    let prices = crate::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    let groups = match compute_cohort_effort(
        &snapshot.worktree_groups,
        &agents,
        runtime,
        snapshot.now,
        active_grace_secs,
        &prices,
        memo,
    ) {
        Ok(groups) => groups,
        Err(error) => {
            tracing::debug!(%error, "sidebar cohort-spend lane lifetime resolution failed");
            return;
        }
    };
    let refreshed = CohortSpendCache {
        version: COHORT_SPEND_CACHE_VERSION,
        refreshed_at_ms: now_ms,
        groups,
    };
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refreshed) {
        tracing::debug!(
            path = %path.display(),
            %error,
            "sidebar cohort-spend cache write failed"
        );
    }
}

fn compute_cohort_effort(
    groups: &[SidebarWorktreeGroup],
    agents: &[AgentState],
    runtime: &RuntimePaths,
    now: jiff::Timestamp,
    active_grace_secs: u32,
    prices: &crate::agents::PriceBook,
    memo: &mut EffortParseMemo,
) -> Result<BTreeMap<String, SidebarCohortEffort>, crate::worktree::WorktreeErr> {
    let lifetimes = crate::worktree::lane_lifetimes(agents.iter())?;
    let agent_refs = agents.iter().collect::<Vec<_>>();
    let slots = crate::agents::attribution::slot_groups(&agent_refs, &lifetimes);
    let active = active_time::read_for_keys(
        runtime,
        agents
            .iter()
            .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
    )
    .into_iter()
    .map(|record| ((record.kind.clone(), record.agent_id.clone()), record))
    .collect::<HashMap<_, _>>();
    let mut computed = BTreeMap::new();

    for group in groups.iter().filter(|group| group.collapses()) {
        let row_ids = group
            .rows
            .iter()
            .filter(|row| row.is_agent())
            .map(|row| row.id.as_str())
            .collect::<HashSet<_>>();
        let mut cohort = SidebarCohortEffort::default();
        for records in slots.iter().filter(|records| {
            records
                .iter()
                .any(|record| row_ids.contains(record.agent_id.as_str()))
        }) {
            let effort = crate::agents::spending::slot_effort_with_memo(
                &records
                    .iter()
                    .map(|record| EffortSessionRef::from_state(record))
                    .collect::<Vec<_>>(),
                prices,
                memo,
            );
            cohort.tokens.add_assign(effort.tokens);
            cohort.cost_usd =
                crate::agents::spending::sum_optional_cost(cohort.cost_usd, effort.cost_usd);
            let seat = SidebarSeatEffort {
                cost_usd: effort.cost_usd,
                tokens: effort.tokens,
            };
            for record in records
                .iter()
                .filter(|record| row_ids.contains(record.agent_id.as_str()))
            {
                cohort.seats.insert(record.agent_id.to_string(), seat);
            }
            let slot_active = records
                .iter()
                .filter_map(|record| {
                    active
                        .get(&(record.kind.clone(), record.agent_id.clone()))
                        .map(|active| active.display_secs(now, active_grace_secs))
                })
                .reduce(u64::saturating_add);
            cohort.active_secs = match (cohort.active_secs, slot_active) {
                (Some(total), Some(value)) => Some(total.saturating_add(value)),
                (Some(total), None) => Some(total),
                (None, Some(value)) => Some(value),
                (None, None) => None,
            };
        }
        computed.insert(group.key.clone(), cohort);
    }

    memo.retain_touched();
    Ok(computed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::sidebar::test_support::{activity_row, worktree_group};

    #[test]
    fn cache_ttl_and_version_gate_refreshes() {
        let ttl_ms = COHORT_SPEND_TTL.as_millis() as u64;
        let current = CohortSpendCache {
            version: COHORT_SPEND_CACHE_VERSION,
            refreshed_at_ms: 100,
            groups: BTreeMap::new(),
        };

        assert!(!cache_due(&current, 100 + ttl_ms));
        assert!(cache_due(&current, 101 + ttl_ms));
        assert!(cache_due(&CohortSpendCache::default(), 1));
        assert!(cache_due(
            &CohortSpendCache {
                version: 2,
                ..current
            },
            100
        ));
    }

    #[test]
    fn cache_round_trip_rejects_unknown_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cohort-spend.json");
        let cache = CohortSpendCache {
            version: COHORT_SPEND_CACHE_VERSION,
            refreshed_at_ms: 42,
            groups: BTreeMap::from([(
                "lane".to_owned(),
                SidebarCohortEffort {
                    cost_usd: Some(1.25),
                    ..SidebarCohortEffort::default()
                },
            )]),
        };
        crate::disk::atomic::write_temp_then_rename_cache(&path, &cache).unwrap();
        assert_eq!(read_cohort_spend_cache(&path), cache);

        let mut stale = cache;
        stale.version += 1;
        crate::disk::atomic::write_temp_then_rename_cache(&path, &stale).unwrap();
        assert_eq!(read_cohort_spend_cache(&path), CohortSpendCache::default());
        stale.version = 2;
        crate::disk::atomic::write_temp_then_rename_cache(&path, &stale).unwrap();
        assert_eq!(read_cohort_spend_cache(&path), CohortSpendCache::default());
    }

    #[test]
    fn collapsed_group_folds_audit_slots_with_one_transcript_memo() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let transcript = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&transcript).unwrap();
        connection
            .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
            .unwrap();
        for (id, session_id, input) in [("one", "planner", 10), ("two", "coder", 20)] {
            let data = serde_json::json!({
                "cost": 0.25,
                "modelID": "gpt",
                "providerID": "openai",
                "time": {"created": 1_780_394_400_000_u64},
                "tokens": {
                    "input": input,
                    "output": 2,
                    "cache": {"read": 3, "write": 4}
                }
            })
            .to_string();
            connection
                .execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                    (id, session_id, data),
                )
                .unwrap();
        }
        drop(connection);
        let mut agents = Vec::new();
        let mut rows = Vec::new();
        for (session_id, role) in [("planner", "planner"), ("coder", "coder")] {
            let mut agent = AgentState::stub("opencode", session_id, AgentStatus::Success);
            agent.team = Some("forge".to_owned());
            agent.role = Some(role.to_owned());
            agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
            agents.push(agent);
            let mut row = activity_row(
                true,
                Some(AgentStatus::Success),
                jiff::Timestamp::UNIX_EPOCH,
                dir.path(),
            );
            row.id = session_id.to_owned();
            rows.push(row);
        }
        let mut group = worktree_group(dir.path(), rows);
        group.finished = true;
        let mut memo = EffortParseMemo::default();

        let computed = compute_cohort_effort(
            &[group],
            &agents,
            &runtime,
            jiff::Timestamp::UNIX_EPOCH,
            180,
            &crate::agents::PriceBook::default(),
            &mut memo,
        )
        .unwrap();
        let effort = computed.values().next().unwrap();

        assert_eq!(effort.cost_usd, Some(0.5));
        assert_eq!(effort.tokens.input, 30);
        assert_eq!(effort.tokens.output, 4);
        assert_eq!(
            effort.seats.keys().map(String::as_str).collect::<Vec<_>>(),
            ["coder", "planner"]
        );
        assert_eq!(
            effort
                .seats
                .values()
                .filter_map(|seat| seat.cost_usd)
                .sum::<f64>(),
            effort.cost_usd.unwrap()
        );
        let seat_tokens = effort.seats.values().fold(
            crate::agents::spending::EffortTokens::default(),
            |mut total, seat| {
                total.add_assign(seat.tokens);
                total
            },
        );
        assert_eq!(seat_tokens, effort.tokens);
    }

    #[test]
    fn collapsed_group_counts_only_the_current_lane_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let checkout = dir.path().join("lane");
        let git_dir = checkout.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let marker = crate::worktree::WorktreeMarker {
            version: 1,
            name: "lane".to_owned(),
            branch: "lane".to_owned(),
            base_branch: Some("main".to_owned()),
            from_pr: None,
            base_ref: "main".to_owned(),
            repo_root: dir.path().to_path_buf(),
            worktree_path: checkout.clone(),
            created_at: jiff::Timestamp::from_second(100).unwrap(),
        };
        let marker_path = git_dir.join("rimz-worktree.json");
        crate::disk::atomic::write_temp_then_rename(&marker_path, &marker).unwrap();
        let transcript = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&transcript).unwrap();
        connection
            .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
            .unwrap();
        let mut agents = Vec::new();
        let mut rows = Vec::new();
        for (session_id, parent, registered_at, cost, input) in [
            ("old", None, 98, 10.0, 100),
            ("old-child", Some("old"), 99, 20.0, 200),
            ("current", None, 100, 1.0, 10),
            ("current-child", Some("current"), 101, 3.0, 30),
        ] {
            let data = serde_json::json!({
                "cost": cost,
                "modelID": "gpt",
                "providerID": "openai",
                "time": {"created": 1_780_394_400_000_u64},
                "tokens": {"input": input, "output": 2, "cache": {"read": 3, "write": 4}}
            })
            .to_string();
            connection
                .execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?1, ?2)",
                    (session_id, data),
                )
                .unwrap();
            let mut agent = AgentState::stub("opencode", session_id, AgentStatus::Success);
            agent.team = Some("forge".to_owned());
            agent.role = Some("planner".to_owned());
            agent.worktree_path = Some(checkout.to_string_lossy().into_owned());
            agent.registered_at = Some(jiff::Timestamp::from_second(registered_at).unwrap());
            agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
            if let Some(parent) = parent {
                agent.parent_agent_id = Some(parent.into());
                agent.parent_agent_kind = Some(agent.kind.clone());
                agent.launch_depth = Some(1);
            } else {
                let mut row = activity_row(
                    true,
                    Some(AgentStatus::Success),
                    marker.created_at,
                    &checkout,
                );
                row.id = session_id.to_owned();
                rows.push(row);
            }
            agents.push(agent);
        }
        drop(connection);
        let mut group = worktree_group(&checkout, rows);
        group.finished = true;
        let mut memo = EffortParseMemo::default();
        let compute = |memo: &mut EffortParseMemo| {
            compute_cohort_effort(
                std::slice::from_ref(&group),
                &agents,
                &runtime,
                marker.created_at,
                180,
                &crate::agents::PriceBook::default(),
                memo,
            )
        };
        let computed = compute(&mut memo).unwrap();
        let effort = &computed[&group.key];
        assert_eq!(effort.cost_usd, Some(4.0));
        assert_eq!(effort.tokens.input, 40);
        assert_eq!(effort.tokens.output, 4);
        assert_eq!(
            effort.seats.keys().map(String::as_str).collect::<Vec<_>>(),
            ["current"]
        );
        assert_eq!(effort.seats["current"].cost_usd, Some(4.0));
        assert_eq!(effort.seats["current"].tokens, effort.tokens);

        std::fs::write(&marker_path, "invalid marker").unwrap();
        assert!(compute(&mut memo).is_err());

        std::fs::remove_dir_all(&checkout).unwrap();
        let removed = compute(&mut memo).unwrap();
        assert_eq!(removed[&group.key], SidebarCohortEffort::default());
        assert!(transcript.exists());
    }
}
