//! Producer-published keep-warm horizons, admitted by the TTL floor and stage condition each heavy pass. The card shares that gate without reading definitions or boards per tick.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::config::HarnessConfig;
use crate::harness::cache_keepalive::{SeatPolicy, holds};
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
    policy: &SeatPolicy,
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

/// Stamp each agent card's [`CacheClock`]: its resolved cache lifetime as
/// the age pin's ceiling, plus the held window when keep-warm is holding the
/// agent under its published horizon ([`holds`]) — judged here on every fold,
/// since status and harness settings move faster than the heavy lane.
/// Price the card's current context with the memoized book, without refreshing it.
pub(in crate::sidebar) fn project_cache_clocks(
    snapshot: &mut SidebarSnapshot,
    cache: &KeepWarmCache,
    harness: &HarnessConfig,
    runtime: &RuntimePaths,
) {
    let now = snapshot.now;
    let prices = crate::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    let clocks = snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            let ttl = harness.prompt_cache_lifetime(agent)?;
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
                rewarm_cost_usd: None,
            };
            Some((horizon_key(agent), clock))
        })
        .collect::<BTreeMap<_, _>>();
    for row in snapshot.rows_mut() {
        let key = format!("{}:{}", row.name, row.id);
        if let Some(card) = row.as_agent_mut() {
            card.cache = clocks.get(&key).copied().map(|mut clock| {
                clock.rewarm_cost_usd = card.context_used_tokens().and_then(|tokens| {
                    let model = card
                        .context
                        .as_ref()
                        .and_then(|context| context.model_id.as_deref())
                        .or(card.model.as_deref())?;
                    crate::agents::pricing::cache_rewarm_cost_usd(
                        &prices,
                        model,
                        tokens,
                        card.context
                            .as_ref()
                            .and_then(|context| context.last_cache_write),
                    )
                });
                clock
            });
        }
    }
}

#[cfg(test)]
mod tests;
