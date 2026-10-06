//! Producer-published keep-warm horizons. Definitions resolve once per heavy pass here, so the
//! card's cache clock learns which agents a horizon holds without re-reading definitions per tick.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::config::HarnessConfig;
use crate::harness::cache_keepalive::{KeepWarmPolicy, holds};
use crate::store::snapshot::{CacheClock, SidebarSnapshot};

const KEEP_WARM_CACHE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct KeepWarmCache {
    pub version: u32,
    /// Admitted horizon seconds by [`horizon_key`].
    pub horizons: BTreeMap<String, u64>,
}

pub(in crate::sidebar) fn horizon_key(agent: &AgentState) -> String {
    format!("{}:{}", agent.kind, agent.agent_id)
}

pub(in crate::sidebar) fn read_keep_warm_cache(path: &Path) -> KeepWarmCache {
    let cache: KeepWarmCache = crate::disk::atomic::read_json_cache(path);
    if cache.version == KEEP_WARM_CACHE_VERSION {
        cache
    } else {
        KeepWarmCache::default()
    }
}

pub(super) fn publish(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    harness: &HarnessConfig,
    policy: &KeepWarmPolicy,
) {
    let refreshed = KeepWarmCache {
        version: KEEP_WARM_CACHE_VERSION,
        horizons: snapshot
            .agents
            .iter()
            .filter_map(|agent| {
                let horizon = policy.horizon(agent, harness)?;
                Some((horizon_key(agent), horizon.as_secs()))
            })
            .collect(),
    };
    let path = runtime.keep_warm_path();
    if refreshed == read_keep_warm_cache(&path) {
        return;
    }
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refreshed) {
        tracing::debug!(path = %path.display(), %error, "sidebar keep-warm cache write failed");
    }
}

/// Stamp each agent card's [`CacheClock`]: the provider's prompt-cache TTL as
/// the age pin's ceiling, plus the held window when keep-warm is holding the
/// agent under its published horizon ([`holds`]) — judged here on every fold,
/// since status and harness settings move faster than the heavy lane.
pub(in crate::sidebar) fn project_cache_clocks(
    snapshot: &mut SidebarSnapshot,
    cache: &KeepWarmCache,
    harness: &HarnessConfig,
) {
    let now = snapshot.now;
    let clocks = snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            let ttl = harness.prompt_cache_ttl(&agent.kind)?;
            let published = cache.horizons.get(&horizon_key(agent));
            let horizon = published.map(|&secs| std::time::Duration::from_secs(secs));
            let held = holds(agent, horizon, harness, now)
                .zip(agent.turn_ended_at)
                .and_then(|(horizon, ended)| Some((ended.checked_add(horizon).ok()?, ended)));
            let clock = CacheClock {
                ceiling_secs: u32::try_from(ttl.as_secs()).unwrap_or(u32::MAX),
                last_request_at: agent.last_request_at(),
                warm_until: held.map(|(until, _)| until),
                held_since: held.map(|(_, ended)| ended),
            };
            Some((horizon_key(agent), clock))
        })
        .collect::<BTreeMap<_, _>>();
    for row in snapshot.rows_mut() {
        let key = format!("{}:{}", row.name, row.id);
        if let Some(card) = row.as_agent_mut() {
            card.cache = clocks.get(&key).copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use jiff::Timestamp;

    use super::*;
    use crate::config::{KeepWarm, Profile};

    fn agent(kind: &str, id: &str, profile: Option<&str>, ended: Option<i64>) -> AgentState {
        let mut agent = AgentState::stub(kind, id, crate::agents::AgentStatus::Idle);
        agent.profile = profile.map(str::to_owned);
        if let Some(secs) = ended {
            let ended = Timestamp::from_second(secs).unwrap();
            agent.turn_started_at = Some(ended);
            agent.turn_ended_at = Some(ended);
        }
        agent
    }

    fn policy() -> KeepWarmPolicy {
        let mut profile: Profile = toml::from_str("agent = \"claude\"").unwrap();
        profile.keep_warm = Some(KeepWarm::For(Duration::from_secs(7200)));
        let mut policy = KeepWarmPolicy::default();
        policy.profiles.0.insert("warm".into(), profile);
        policy
    }

    fn snapshot(runtime: &RuntimePaths, agents: Vec<AgentState>) -> SidebarSnapshot {
        let now = Timestamp::from_second(100).unwrap();
        let root = std::path::Path::new("/repo/main");
        let rows = agents
            .iter()
            .map(|agent| {
                let mut row = crate::sidebar::test_support::activity_row(
                    true,
                    Some(crate::agents::AgentStatus::Idle),
                    now,
                    root,
                );
                row.id = agent.agent_id.to_string();
                row.name = agent.kind.to_string();
                row
            })
            .collect();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), agents, now);
        snapshot.worktree_groups = vec![crate::sidebar::test_support::worktree_group(root, rows)];
        snapshot
    }

    #[test]
    fn publish_writes_admitted_horizons_once_and_gates_versions() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let harness = HarnessConfig::default();
        let snapshot = snapshot(
            &runtime,
            vec![
                agent("claude", "warm-1", Some("warm"), Some(10)),
                agent("claude", "plain-1", None, Some(10)),
            ],
        );
        publish(&snapshot, &runtime, &harness, &policy());
        let path = runtime.keep_warm_path();
        let cache = read_keep_warm_cache(&path);
        assert_eq!(
            cache.horizons,
            BTreeMap::from([("claude:warm-1".to_owned(), 7200)])
        );

        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        publish(&snapshot, &runtime, &harness, &policy());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            old,
            "an unchanged publication leaves the consumer stamp alone"
        );

        let stale = KeepWarmCache {
            version: KEEP_WARM_CACHE_VERSION + 1,
            ..cache.clone()
        };
        crate::disk::atomic::write_temp_then_rename_cache(&path, &stale).unwrap();
        assert_eq!(read_keep_warm_cache(&path), KeepWarmCache::default());
        publish(&snapshot, &runtime, &harness, &KeepWarmPolicy::default());
        assert!(
            read_keep_warm_cache(&path).horizons.is_empty(),
            "a horizon that leaves the definitions stops being published"
        );
    }

    #[test]
    fn cache_clock_reads_the_provider_ttl_and_the_published_hold() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        let mut snapshot = snapshot(
            &runtime,
            vec![
                agent("claude", "held-1", Some("warm"), Some(10)),
                agent("claude", "unended-1", Some("warm"), None),
                agent("codex", "plain-1", None, Some(10)),
                agent("amp", "unknown-1", None, Some(10)),
            ],
        );
        let cache = KeepWarmCache {
            version: KEEP_WARM_CACHE_VERSION,
            horizons: BTreeMap::from([
                ("claude:held-1".to_owned(), 7200),
                ("claude:unended-1".to_owned(), 7200),
            ]),
        };
        project_cache_clocks(&mut snapshot, &cache, &HarnessConfig::default());
        let clock = |id: &str| {
            snapshot
                .rows()
                .find(|row| row.id == id)
                .and_then(|row| row.as_agent())
                .unwrap_or_else(|| panic!("{id} has a card"))
                .cache
        };
        let ended = Timestamp::from_second(10).unwrap();
        assert_eq!(
            clock("held-1"),
            Some(CacheClock {
                ceiling_secs: 3600,
                last_request_at: Some(ended),
                warm_until: Some(Timestamp::from_second(7210).unwrap()),
                held_since: Some(ended),
            })
        );
        assert_eq!(
            clock("unended-1"),
            Some(CacheClock {
                ceiling_secs: 3600,
                last_request_at: None,
                warm_until: None,
                held_since: None,
            }),
            "no real turn has ended, so there is nothing to hold yet"
        );
        assert_eq!(
            clock("plain-1"),
            Some(CacheClock {
                ceiling_secs: 1800,
                last_request_at: Some(ended),
                warm_until: None,
                held_since: None,
            })
        );
        assert_eq!(
            clock("unknown-1"),
            None,
            "no known TTL keeps the hour clock"
        );
    }

    #[test]
    fn the_cache_clock_counts_from_the_last_ping_and_drops_a_hold_once_cold() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        let mut pinged = agent("claude", "pinged-1", Some("warm"), Some(-7000));
        pinged.pinged_at = Some(Timestamp::from_second(-3400).unwrap());
        let mut snapshot = snapshot(
            &runtime,
            vec![pinged, agent("claude", "cold-1", Some("warm"), Some(-3600))],
        );
        let cache = KeepWarmCache {
            version: KEEP_WARM_CACHE_VERSION,
            horizons: BTreeMap::from([
                ("claude:pinged-1".to_owned(), 7200),
                ("claude:cold-1".to_owned(), 7200),
            ]),
        };
        project_cache_clocks(&mut snapshot, &cache, &HarnessConfig::default());
        let clock = |id: &str| {
            snapshot
                .rows()
                .find(|row| row.id == id)
                .and_then(|row| row.as_agent())
                .unwrap()
                .cache
                .unwrap()
        };
        let pinged = clock("pinged-1");
        assert_eq!(
            pinged.last_request_at,
            Some(Timestamp::from_second(-3400).unwrap())
        );
        assert_eq!(
            pinged.held_since,
            Some(Timestamp::from_second(-7000).unwrap())
        );
        let cold = clock("cold-1");
        assert_eq!(cold.warm_until, None, "the window was missed at 0");
        assert_eq!(
            cold.last_request_at,
            Some(Timestamp::from_second(-3600).unwrap())
        );
    }

    #[test]
    fn a_published_horizon_holds_only_an_agent_the_pings_can_maintain() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        let board = tempfile::tempdir().unwrap();
        std::fs::write(board.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        let mut done = agent("claude", "done-1", Some("warm"), Some(10));
        done.team = Some("forge".into());
        done.role = Some("coder".into());
        done.worktree_path = Some(board.path().to_string_lossy().into_owned());
        let mut parked = agent("claude", "parked-1", Some("warm"), Some(10));
        parked.budget_park = Some(crate::agents::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 1.0,
            window: crate::agents::BudgetWindow::Session,
            at: Timestamp::UNIX_EPOCH,
            scope: crate::agents::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
        let ids = ["done-1", "parked-1", "held-1"];
        let mut snapshot = snapshot(
            &runtime,
            vec![
                done,
                parked,
                agent("claude", "held-1", Some("warm"), Some(10)),
            ],
        );
        let cache = KeepWarmCache {
            version: KEEP_WARM_CACHE_VERSION,
            horizons: ids
                .into_iter()
                .map(|id| (format!("claude:{id}"), 7200))
                .collect(),
        };
        let mut clocks = |harness: &str| {
            project_cache_clocks(&mut snapshot, &cache, &toml::from_str(harness).unwrap());
            ids.map(|id| {
                snapshot
                    .rows()
                    .find(|row| row.id == id)
                    .and_then(|row| row.as_agent())
                    .unwrap()
                    .cache
            })
        };
        let unheld = |ceiling_secs| {
            Some(CacheClock {
                ceiling_secs,
                last_request_at: Some(Timestamp::from_second(10).unwrap()),
                warm_until: None,
                held_since: None,
            })
        };
        let [done, parked, held] = clocks("");
        assert_eq!(done, unheld(3600), "a Done cohort is not painted held");
        assert_eq!(parked, unheld(3600), "a budget-parked agent is not held");
        assert!(held.unwrap().warm_until.is_some());
        assert_eq!(clocks("cache_keepalive = false")[2], unheld(3600));
        assert_eq!(
            clocks("[prompt_cache_ttl]\nclaude = \"5m\"")[2],
            unheld(300),
            "a horizon published under an older floor stops holding"
        );
    }
}
