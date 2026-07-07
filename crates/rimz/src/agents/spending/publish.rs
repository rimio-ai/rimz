//! Published provider and workspace spending cache shapes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::SPENDING_TTL;
use super::aggregate::{DaySpend, SpendTally, Spending};
use super::cache::peek_cache_version;

/// Gates the aggregate meaning in provider-spending.json, independent of the
/// raw per-file cache version. An older stamp reads as stale, so the producer
/// recomputes once from the still-current entry cache. `0` is the implicit
/// pre-versioning shape. v1: cache-write folds into `◇`/`↘`. v2:
/// live-session baselines moved to a per-workspace sidecar. v3: the published
/// aggregate carries per-day and per-model rollups for `rimz stats`. v4:
/// unpriced token usage contributes tokens/sessions with zero dollars, and
/// provider-native thread ids count multi-session stores correctly. v5: GPT-5.5
/// built-in prices heal previously zero-dollar token rows. v6: per-model
/// cache-read is published for `rimz stats`.
/// v7: the default headline window changed from trailing 24 hours to session,
/// so cached headline aggregates need a cheap re-aggregate. v8: the published
/// model rollup became per-window so the stats dashboard can scope tabs without
/// walking transcripts. v9: daily token buckets fold in cache-read so the
/// heatmap and `rimz stats` token totals count every token.
pub(crate) const PROVIDER_SPENDING_VERSION: u32 = 9;

/// Aggregate version for the per-workspace cockpit tally cache. This is
/// independent of the shared raw-entry cache version: a semantic change here
/// can force a cheap re-aggregate without re-reading transcripts.
/// v2: the default headline window changed from trailing 24 hours to session.
/// v3: scoped tallies read per-file origin instead of per-entry origin.
/// v4: live headline carry and baselines publish atomically with the scoped
/// walk. v5: live card sessions are excluded from walked headline USD and
/// added back from live cards.
pub(crate) const WORKSPACE_SPENDING_VERSION: u32 = 5;

/// The published provider-spending cache: the aggregated [`Spending`] plus the
/// stamp the producer's [`SPENDING_TTL`] gate reads. A wrapper rather than a
/// field on [`Spending`] keeps the in-memory value the fold path threads
/// stamp-free; `#[serde(flatten)]` keeps a pre-stamp file (a bare `Spending`)
/// readable — its values survive, with `version` and `refreshed_at_ms`
/// defaulting to 0 so it reads as stale and refreshes once. A later aggregate
/// semantic change bumps `version` without forcing raw JSONL re-parse.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSpendingCache {
    /// Aggregate semantic version for the TTL gate. Version must stay the
    /// first field; `peek_cache_version` reads it from the file prefix.
    #[serde(default)]
    pub version: u32,
    /// When the producer last walked and published, for the TTL gate.
    #[serde(default)]
    pub refreshed_at_ms: u64,
    /// Account-global UTC-day buckets keyed by epoch day, published so the stats
    /// command can render its heatmap without walking transcripts.
    #[serde(default)]
    pub days: BTreeMap<i64, DaySpend>,
    /// Account-global model buckets by trailing window, published so
    /// `rimz stats` can scope its model breakdown without walking transcripts.
    #[serde(default)]
    pub models: BTreeMap<String, SpendTally>,
    #[serde(flatten)]
    pub spending: Spending,
}

impl ProviderSpendingCache {
    /// Whether this cache carries the current published aggregate shape.
    pub fn is_current_version(&self) -> bool {
        self.version == PROVIDER_SPENDING_VERSION
    }

    /// Whether the published walk is young enough that the producer skips the
    /// transcript walk this tick. Saturating, so a clock that ran backwards
    /// reads fresh rather than re-walking every tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.is_current_version()
            && now_ms.saturating_sub(self.refreshed_at_ms) <= SPENDING_TTL.as_millis() as u64
    }
}

/// Atomic write of the aggregated `Spending`, stamped `refreshed_at_ms`, so
/// consumer sidebar tabs read the fleet and per-provider totals — and the
/// producer its own [`SPENDING_TTL`] gate — without re-walking the JSONL
/// transcript history. Follows the same temp-then-rename durability contract
/// as [`write_spending_cache`].
pub fn write_provider_spending_cache(
    path: &Path,
    refreshed_at_ms: u64,
    spending: &Spending,
) -> bool {
    let days = BTreeMap::new();
    let models = BTreeMap::new();
    write_provider_spending_cache_with_rollups(path, refreshed_at_ms, spending, &days, &models)
}

/// Atomic write of the provider aggregate plus the rollups consumed by
/// `rimz stats`.
pub fn write_provider_spending_cache_with_rollups(
    path: &Path,
    refreshed_at_ms: u64,
    spending: &Spending,
    days: &BTreeMap<i64, DaySpend>,
    models: &BTreeMap<String, SpendTally>,
) -> bool {
    let cache = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms,
        days: days.clone(),
        models: models.clone(),
        spending: spending.clone(),
    };
    if let Some(on_disk) = peek_cache_version(path)
        && on_disk > cache.version
    {
        debug!(
            path = %path.display(),
            on_disk,
            ours = cache.version,
            "skip provider spending cache downgrade"
        );
        return true;
    }
    let _ = crate::ledger::atomic::sweep_stale_temp_siblings(
        path,
        std::time::Duration::from_secs(3_600),
    );
    match crate::ledger::atomic::write_temp_then_rename_cache(path, &cache) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "provider spending cache write failed"
            );
            false
        }
    }
}

/// Read the provider-spending cache written by [`write_provider_spending_cache`].
/// Returns a default (stamp 0, so it reads as stale) on any error so callers
/// always get a usable value; a pre-stamp file deserializes with its spending
/// values intact.
pub fn read_provider_spending_cache(path: &Path) -> ProviderSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProviderSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}

/// The published per-workspace cockpit spending cache. The filename is keyed by
/// the scope hash; the hash rides in the file too so a stale or renamed file
/// cannot satisfy a different scope.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSpendingCache {
    /// Version must stay the first field; `peek_cache_version` reads it from
    /// the file prefix before cache writes.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub refreshed_at_ms: u64,
    #[serde(default)]
    pub scope_hash: String,
    #[serde(default)]
    pub tally: SpendTally,
    #[serde(default)]
    pub headline_cutoff_secs: u64,
    #[serde(default)]
    pub live_excluded: BTreeSet<String>,
}

impl WorkspaceSpendingCache {
    pub fn is_fresh(&self, now_ms: u64, scope_hash: &str) -> bool {
        self.version == WORKSPACE_SPENDING_VERSION
            && self.scope_hash == scope_hash
            && now_ms.saturating_sub(self.refreshed_at_ms) <= SPENDING_TTL.as_millis() as u64
    }
}

pub fn write_workspace_spending_cache(path: &Path, cache: &WorkspaceSpendingCache) {
    let cache = WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        ..cache.clone()
    };
    if let Some(on_disk) = peek_cache_version(path)
        && on_disk > cache.version
    {
        debug!(
            path = %path.display(),
            on_disk,
            ours = cache.version,
            "skip workspace spending cache downgrade"
        );
        return;
    }
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, &cache);
}

pub fn read_workspace_spending_cache(path: &Path) -> WorkspaceSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WorkspaceSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}
