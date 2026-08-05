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

pub(crate) const COHORT_SPEND_CACHE_VERSION: u32 = 2;

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

pub(crate) fn refresh_cohort_spend_for(
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
    let groups = compute_cohort_effort(
        &snapshot.worktree_groups,
        &agents,
        runtime,
        snapshot.now,
        active_grace_secs,
        &prices,
        memo,
    );
    let refreshed = CohortSpendCache {
        version: COHORT_SPEND_CACHE_VERSION,
        refreshed_at_ms: now_ms,
        groups,
    };
    if let Err(error) = crate::store::atomic::write_temp_then_rename_cache(&path, &refreshed) {
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
) -> BTreeMap<String, SidebarCohortEffort> {
    let agent_refs = agents.iter().collect::<Vec<_>>();
    let slots = crate::agents::attribution::slot_groups(&agent_refs);
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
    computed
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
        crate::store::atomic::write_temp_then_rename_cache(&path, &cache).unwrap();
        assert_eq!(read_cohort_spend_cache(&path), cache);

        let mut stale = cache;
        stale.version += 1;
        crate::store::atomic::write_temp_then_rename_cache(&path, &stale).unwrap();
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
        );
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
}
