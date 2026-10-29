//! Published provider and workspace spending cache shapes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::aggregate::{DaySpend, SpendTally, SpendWindow, Spending};
use super::cache::peek_cache_version;
use super::{SPENDING_TTL, ScopedSpending, SpendingWalkResult, local_day_index};

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
/// heatmap and `rimz stats` token totals count every token. v10: a local-day
/// per-provider window supports account daily caps independently of headline.
/// v11: provider session headlines share the machine-global activity burst.
/// v12 publishes per-tool counts in windows and model/agent breakdowns.
/// v13: per-account local-day windows for account daily caps.
// v14: per-login headline and trailing-window tallies for dashboard panels.
// v15: per-login maps are keyed by history pool; a shared account's spend is
// under its kind's `default` key.
// v16: response timing counters in per-model windows for the stats tok/s figure.
// v17: local-date history with its timezone and durable session-role attribution.
// v18: calendar-day windows on every tally, independent of the headline.
// v19: per-model producing kind, subagent-only role tallies, and the ledger's first record time.
// v20: profile-attributed agent tallies alongside subagents.
// v21: team-role tallies split from solo agent profiles, with active-time and message
// effort windows.
// v22: bounded daily and weekly model-dollar chart cells.
// v23: UTC-calendar-month spend per history pool for account monthly caps.
// v24: local-day store windows and raw hourly model-dollar chart cells.
// v25: native session keys count Claude per session, with subagents folded under their parent.
pub(crate) const PROVIDER_SPENDING_VERSION: u32 = 25;

/// Aggregate version for the per-workspace cockpit tally cache. This is
/// independent of the shared raw-entry cache version: a semantic change here
/// can force a cheap re-aggregate without re-reading transcripts.
/// v2: the default headline window changed from trailing 24 hours to session.
/// v3: scoped tallies read per-file origin instead of per-entry origin.
/// v4: live headline carry and baselines publish atomically with the scoped
/// walk. v5: live card sessions are excluded from walked headline USD and
/// added back from live cards. v6: a local-day window supports room daily caps
/// independently of the configured headline. v7: walked per-session baselines
/// replace live-session exclusion for exact flush-lag overlays.
/// v8: the scoped tally carries an independent calendar-day window.
/// v9: store windows align with history-zone local days, with a 364-day year.
/// v10: native session keys count Claude per session, with subagents folded under their parent.
pub(crate) const WORKSPACE_SPENDING_VERSION: u32 = 10;

/// The published provider-spending cache: the aggregated [`Spending`] plus the
/// stamp the producer's `SPENDING_TTL` gate reads. A wrapper rather than a
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
    /// Machine-wide cockpit session start, independent of the headline mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start_secs: Option<u64>,
    /// Account-local calendar-day spend by provider kind.
    #[serde(default)]
    pub day_by_provider: BTreeMap<String, SpendWindow>,
    /// Account-local calendar-day spend by history pool, for account daily caps.
    #[serde(default)]
    pub day_by_login: BTreeMap<crate::ids::LoginKey, SpendWindow>,
    /// Account-day midnight in the configured/system zone. Budget and accounts
    /// readers compare it with their own midnight, not with the history zone.
    #[serde(default)]
    pub day_cutoff_secs: u64,
    /// UTC-calendar-month spend by history pool, independent of the headline.
    #[serde(default)]
    pub month_by_login: BTreeMap<crate::ids::LoginKey, SpendWindow>,
    /// 00:00 UTC on the first day of the walk's month.
    #[serde(default)]
    pub month_cutoff_secs: u64,
    /// Named history zone for day keys and publication freshness; UTC when the
    /// configured/system zone has no IANA name.
    #[serde(default)]
    pub timezone: String,
    /// Account-global local-date buckets keyed by epoch day, published so the stats
    /// command can render its heatmap without walking transcripts.
    #[serde(default)]
    pub days: BTreeMap<i64, DaySpend>,
    /// Stored model dollars for 8 local days of raw hours, 30 daily cells, and
    /// 52 seven-day blocks ending today.
    #[serde(default)]
    pub chart: super::ChartSpend,
    /// Account-global model buckets by local-day window, published so
    /// `rimz stats` can scope its model breakdown without walking transcripts.
    #[serde(default)]
    pub models: BTreeMap<String, super::ModelTally>,
    /// Solo-profile, team-role, and subagent spend/effort and their retained history times.
    #[serde(default)]
    pub roles: super::RoleTallies,
    #[serde(flatten)]
    pub spending: Spending,
}

impl ProviderSpendingCache {
    pub(crate) fn from_walk(result: &SpendingWalkResult, refreshed_at_ms: u64) -> Self {
        Self {
            version: PROVIDER_SPENDING_VERSION,
            refreshed_at_ms,
            session_start_secs: result.session_start_secs,
            day_by_provider: result.provider_day.clone(),
            day_by_login: result.login_day.clone(),
            day_cutoff_secs: result.day_cutoff_secs,
            month_by_login: result.login_month.clone(),
            month_cutoff_secs: result.month_cutoff_secs,
            days: result.days.clone(),
            chart: result.chart.clone(),
            timezone: result.timezone.clone(),
            models: result.models.clone(),
            roles: result.roles.clone(),
            spending: result.spending.clone(),
        }
    }

    /// Whether this cache carries the current published aggregate shape.
    pub fn is_current_version(&self) -> bool {
        self.version == PROVIDER_SPENDING_VERSION
    }

    /// Whether the walk's history day is today in its published timezone.
    pub fn is_from_today(&self, now_secs: u64) -> bool {
        Some(self.chart.today) == local_day_index(now_secs, Some(&self.timezone))
    }

    /// Whether the published walk is young enough that the producer skips the
    /// transcript walk this tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.is_fresh_within(now_ms, SPENDING_TTL)
    }

    /// Whether the published walk is no older than `max_age` and from today in
    /// its history zone. The history day belongs to aggregation, not completion,
    /// so crossing that zone's midnight or the UTC month turn expires it at once.
    /// Saturating age keeps a backwards clock within that day fresh rather than
    /// re-walking every tick.
    pub(crate) fn is_fresh_within(&self, now_ms: u64, max_age: Duration) -> bool {
        self.is_current_version()
            && now_ms.saturating_sub(self.refreshed_at_ms) <= max_age.as_millis() as u64
            && self.is_from_today(now_ms / 1000)
            && super::utc_month_start_secs(now_ms / 1000) == Some(self.month_cutoff_secs)
    }
}

/// Atomic write of the aggregated `Spending`, stamped `refreshed_at_ms`, so
/// consumer sidebar tabs read the fleet and per-provider totals — and the
/// producer its own `SPENDING_TTL` gate — without re-walking the JSONL
/// transcript history. Follows the same temp-then-rename durability contract
/// as [`super::write_spending_cache`].
pub fn write_provider_spending_cache(path: &Path, cache: &ProviderSpendingCache) -> bool {
    let cache = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        ..cache.clone()
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
    let _ =
        crate::disk::atomic::sweep_stale_temp_siblings(path, std::time::Duration::from_secs(3_600));
    match crate::disk::atomic::write_temp_then_rename_cache(path, &cache) {
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

pub(crate) fn cached_provider_spending(path: &Path) -> Arc<ProviderSpendingCache> {
    thread_local! {
        static CACHE: crate::disk::parse_cache::KeyedParseCache<ProviderSpendingCache> =
            crate::disk::parse_cache::KeyedParseCache::default();
    }
    CACHE.with(|cache| cache.read_json(path).ok().flatten().unwrap_or_default())
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
    /// Workspace-local calendar-day spend.
    #[serde(default)]
    pub day: SpendWindow,
    #[serde(default)]
    pub day_cutoff_secs: u64,
    #[serde(default)]
    pub headline_cutoff_secs: u64,
    #[serde(default)]
    pub live_baselines: BTreeMap<String, f64>,
}

impl WorkspaceSpendingCache {
    pub(super) fn from_scoped(
        scope_hash: &str,
        refreshed_at_ms: u64,
        scoped: ScopedSpending,
    ) -> Self {
        Self {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            scope_hash: scope_hash.to_owned(),
            tally: scoped.tally,
            headline_cutoff_secs: scoped.headline_cutoff_secs,
            day: scoped.day,
            day_cutoff_secs: scoped.day_cutoff_secs,
            live_baselines: scoped.live_baselines,
        }
    }

    pub fn is_current_version(&self) -> bool {
        self.version == WORKSPACE_SPENDING_VERSION
    }

    pub fn is_fresh(&self, now_ms: u64, scope_hash: &str) -> bool {
        self.is_current_version()
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
    let _ = crate::disk::atomic::write_temp_then_rename_cache(path, &cache);
}

pub fn read_workspace_spending_cache(path: &Path) -> WorkspaceSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WorkspaceSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}
