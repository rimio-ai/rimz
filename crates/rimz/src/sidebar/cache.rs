//! Runtime cache types, TTLs, and cheap cache reads for the sidebar data plane.
//!
//! These files live under the workspace runtime directory and are cache-class:
//! producers publish them with temp-file-plus-rename, consumers read them
//! opportunistically, and every value can be rebuilt from ledger truth plus live
//! mux/provider state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::ledger::parse_cache::ParseCache;
use crate::schema::pane_topology::PaneTopologyCache;
use crate::sidebar::frame::PaneFrame;
pub use crate::sidebar::timing::{
    ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL, DIFF_STATS_IDLE_TTL, DIFF_STATS_TTL, EVENT_PANE_TTL,
    GIT_ACTIVITY_WINDOW, PRESENCE_STAMP_FRESH, SNAPSHOT_CACHE_TTL, WORKTREE_ROOTS_TTL,
};

/// The producer's published provider-account map: the out-of-band login facts
/// (`claude auth status`, the `codex` auth file) the dashboard folds onto its
/// blocks. Single-flighted like the diff stats — the elder probes and publishes,
/// every other tab reads it back — so a consumer renderer forks zero subprocesses.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AccountsCache {
    /// When the producer last probed and published this map, for the TTL gate.
    pub refreshed_at_ms: u64,
    /// Probed accounts by agent kind; a logged-out provider is simply absent.
    pub accounts: BTreeMap<String, crate::agents::AgentAccount>,
    /// Whether the probe that produced this map completed without an
    /// infrastructure failure. A failed probe rides the short `ACCOUNTS_RETRY_TTL`
    /// so the producer re-forks within seconds; a successful one — including a
    /// confident logged-out — rides the long `ACCOUNTS_TTL`. Defaults to `true`
    /// so a cache written by an older build is trusted for the success window.
    #[serde(default = "accounts_probe_ok_default")]
    pub ok: bool,
}

/// The `AccountsCache::ok` default for caches written before the field existed:
/// trust them for the success window rather than forcing an immediate re-probe.
fn accounts_probe_ok_default() -> bool {
    true
}

impl AccountsCache {
    /// Whether the published map is young enough that the producer skips the
    /// re-probe this tick. A failed probe expires on the short retry TTL, a
    /// success on the long one. Saturating, so a clock that ran backwards reads
    /// fresh rather than re-probing every tick.
    pub(crate) fn is_fresh(&self, now_ms: u64) -> bool {
        let ttl = if self.ok {
            ACCOUNTS_TTL
        } else {
            ACCOUNTS_RETRY_TTL
        };
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }
}

// The shared pane frame cache is keyed to one `(workspace, session)`: the
// per-workspace runtime root scopes the workspace, and `session_name` prevents
// serving one session's panes to another during detach or session rotation. It
// caches only the expensive `list-panes` round-trip; the ledger rollup stays
// event-fresh and is folded over this frame by producer and consumer reads.
thread_local! {
    /// This thread's last `snapshot.json` parse ([`ParseCache`]). The consumer
    /// fetch worker calls [`read_snapshot_cache`] every fetch (~0.75–2s), but
    /// the producer only republishes when something changed — so most reads
    /// hit an unchanged file and skip the 100–500 KB deserialize.
    static SNAPSHOT_PARSE_CACHE: ParseCache<PaneFrame> = const { ParseCache::new() };
}

/// Read a same-session cache entry regardless of coalescing freshness. `None`
/// when it is absent, for another session, or unreadable. Used as the
/// hold-last-good base for a consumer read and the degraded-read fallback.
///
/// Skips the JSON parse when this thread already parsed a byte-identical file
/// (same path, mtime, and length). On a stat miss it re-reads and re-caches; a
/// file replaced (atomic rename) between the stat and the read just costs one
/// redundant parse next call, never a stale or torn value.
pub fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<PaneFrame> {
    let meta = std::fs::metadata(cache_path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();

    let cache = match SNAPSHOT_PARSE_CACHE.with(|cache| cache.get(cache_path, mtime, len)) {
        Some(cache) => cache,
        None => {
            let bytes = std::fs::read(cache_path).ok()?;
            let parsed: PaneFrame = serde_json::from_slice(&bytes).ok()?;
            SNAPSHOT_PARSE_CACHE.with(|cache| cache.store(cache_path, mtime, len, parsed.clone()));
            parsed
        }
    };
    (cache.session_name == session).then_some(cache)
}

/// Whether a same-session cache entry is young enough to serve without a
/// produce: younger than `ttl` *and* at or past the caller's forced-freshness
/// floor. The floor clause is load-bearing for event mode — a lifecycle or
/// resize signal carries `min_produced_at_ms`, which rejects any pre-signal
/// cache regardless of which TTL is in effect, so agent birth/death never
/// waits out [`EVENT_PANE_TTL`]. The age saturates, so a cache stamped by a
/// clock ahead of this reader serves (age 0) rather than re-producing every
/// call. Pure over its inputs so every caller — the fast path, the
/// single-flight `fresh` closure, the loser re-check — applies one verdict.
pub fn snapshot_cache_is_fresh(
    cache: &PaneFrame,
    now_ms: u64,
    min_produced_at_ms: Option<u64>,
    ttl: Duration,
) -> bool {
    let fresh = now_ms.saturating_sub(cache.produced_at_ms) <= ttl.as_millis() as u64;
    let new_enough = min_produced_at_ms.is_none_or(|min| cache.observed_or_produced_at_ms() >= min);
    fresh && new_enough
}

/// Age of the producer's published same-session frame at `now_ms`, in
/// milliseconds — the fork gate reads this to skip a fork while the frame is
/// younger than one data tick. `None` when no same-session frame exists yet
/// (cold start, or a session-handoff mismatch), which the gate reads as "no
/// usable frame: produce". The age saturates, so a clock that ran backwards
/// reads as fresh (age 0) rather than forcing a fork.
pub fn published_frame_age_ms(runtime: &RuntimePaths, session: &str, now_ms: u64) -> Option<u64> {
    published_frame_produced_at_ms(runtime, session)
        .map(|produced_at_ms| now_ms.saturating_sub(produced_at_ms))
}

/// The producer timestamp of the published same-session pane frame. `None`
/// when no usable same-session frame exists.
pub fn published_frame_produced_at_ms(runtime: &RuntimePaths, session: &str) -> Option<u64> {
    let cache_path = runtime.root.join("snapshot.json");
    read_snapshot_cache(&cache_path, session).map(|cache| cache.produced_at_ms)
}

/// The observation timestamp of the published same-session pane frame. `None`
/// when no usable same-session frame exists.
pub fn published_frame_observed_at_ms(runtime: &RuntimePaths, session: &str) -> Option<u64> {
    let cache_path = runtime.root.join("snapshot.json");
    read_snapshot_cache(&cache_path, session).map(|cache| cache.observed_or_produced_at_ms())
}

/// The presence liveness stamp refreshed by the Zellij presence plugin through
/// `rimz sidebar wake` and by the tmux control-mode watch. Its freshness gates
/// the producer's pane TTL: fresh → event mode ([`EVENT_PANE_TTL`]), stale or absent → poll mode
/// ([`SNAPSHOT_CACHE_TTL`]). Cache-class JSON in the workspace runtime root;
/// the explicit millisecond field (over a bare mtime stamp) lets `rimz doctor`
/// render a stamp age from the same value the producer's verdict reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceStamp {
    pub written_at_ms: u64,
}

/// Path of the presence stamp, beside the pane cache it gates.
pub fn presence_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("presence.stamp")
}

/// Refresh the presence stamp. Best-effort and cache-class (rename atomicity,
/// no fsync — it is rewritten every poke and survives no power cut by design);
/// a failed write only delays the channel reading as alive by one poke.
pub fn write_presence_stamp(runtime: &RuntimePaths) {
    let stamp = PresenceStamp {
        written_at_ms: unix_now_ms(),
    };
    let path = presence_stamp_path(runtime);
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(&path, &stamp) {
        tracing::debug!(path = %path.display(), error = %err, "presence stamp write failed");
    }
}

/// Age of the presence stamp in milliseconds, or `None` when it is absent or
/// unreadable (read as poll mode). One small read per produce — the producer
/// is a cold fork per tick, so the stamp lives in the file, never process
/// memory. Saturating, so a stamp written by a clock ahead of this reader
/// reads as age 0 (fresh) rather than wrapping into poll mode.
pub fn presence_stamp_age_ms(runtime: &RuntimePaths) -> Option<u64> {
    let bytes = std::fs::read(presence_stamp_path(runtime)).ok()?;
    let stamp: PresenceStamp = serde_json::from_slice(&bytes).ok()?;
    Some(unix_now_ms().saturating_sub(stamp.written_at_ms))
}

/// Whether the presence push channel is alive: the stamp exists and is younger
/// than [`PRESENCE_STAMP_FRESH`]. Pure over the age so the boundary is
/// unit-testable without touching disk; `None` (absent stamp) is poll mode.
pub fn presence_event_mode(stamp_age_ms: Option<u64>) -> bool {
    stamp_age_ms.is_some_and(|age| age <= PRESENCE_STAMP_FRESH.as_millis() as u64)
}

/// The effective pane-cache TTL for one produce: the event-mode TTL while the
/// presence channel is alive, else the poll-mode TTL. Computed once per
/// `cached_panes_or_produce` call and threaded through every freshness check —
/// the fast path, the single-flight `fresh` closure, and the loser re-check —
/// so they agree on one verdict and a loser never produces what the winner
/// skipped (the diff-stats "one shared stale() closure" rule).
pub fn effective_pane_ttl(stamp_age_ms: Option<u64>) -> Duration {
    if presence_event_mode(stamp_age_ms) {
        EVENT_PANE_TTL
    } else {
        SNAPSHOT_CACHE_TTL
    }
}

/// Path of the Zellij presence-plugin topology cache, beside the producer's
/// `snapshot.json` pane frame. The topology cache is a pre-producer latency
/// hint: it lets the Zellij backend skip the slow JSON `list-panes` enrichment
/// path, then the normal producer frame still carries the rendered view-model.
pub fn pane_topology_cache_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("pane-topology.json")
}

/// Publish the plugin-provided pane topology. Cache-class: rename atomic, no
/// fsync, rebuilt by the next presence event or by the CLI fallback.
pub fn write_pane_topology_cache(
    runtime: &RuntimePaths,
    cache: &PaneTopologyCache,
) -> crate::ledger::atomic::Result<()> {
    crate::ledger::atomic::write_temp_then_rename_cache(&pane_topology_cache_path(runtime), cache)
}

/// Read a same-session topology cache regardless of freshness. `None` means
/// absent, unreadable, or for another session.
pub fn read_pane_topology_cache(
    runtime: &RuntimePaths,
    session: &str,
) -> Option<PaneTopologyCache> {
    let bytes = std::fs::read(pane_topology_cache_path(runtime)).ok()?;
    let cache: PaneTopologyCache = serde_json::from_slice(&bytes).ok()?;
    (cache.session_name == session).then_some(cache)
}

/// Whether a same-session plugin topology payload is young enough to use as a
/// Zellij `list-panes` substitute. The window matches the presence liveness
/// window so one normal keepalive jitter does not force the slow CLI fallback,
/// and the optional floor lets lifecycle/resize events require a post-signal
/// topology just like they require a post-signal pane frame.
pub fn pane_topology_cache_is_fresh(
    cache: &PaneTopologyCache,
    now_ms: u64,
    min_produced_at_ms: Option<u64>,
) -> bool {
    let fresh =
        now_ms.saturating_sub(cache.produced_at_ms) <= PRESENCE_STAMP_FRESH.as_millis() as u64;
    let new_enough = min_produced_at_ms.is_none_or(|min| cache.produced_at_ms >= min);
    fresh && new_enough
}

/// Read a fresh same-session topology cache, or `None` when absent/stale.
pub fn read_fresh_pane_topology_cache(
    runtime: &RuntimePaths,
    session: &str,
    min_produced_at_ms: Option<u64>,
) -> Option<PaneTopologyCache> {
    let cache = read_pane_topology_cache(runtime, session)?;
    pane_topology_cache_is_fresh(&cache, unix_now_ms(), min_produced_at_ms).then_some(cache)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCache {
    pub entries: BTreeMap<String, DiffStatsCacheEntry>,
    /// The repo's worktree checkout roots, cached under [`WORKTREE_ROOTS_TTL`]
    /// (with a session-boundary refresh floor). The set changes only on
    /// `git worktree add/remove`, so grouping reuses it across ticks instead
    /// of forking `git worktree list` every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<WorktreeRootsCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRootsCache {
    pub refreshed_at_ms: u64,
    pub roots: Vec<PathBuf>,
}

impl WorktreeRootsCache {
    /// Saturating, so a clock that ran backwards reads fresh rather than
    /// re-enumerating every tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= WORKTREE_ROOTS_TTL.as_millis() as u64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffStatsCacheEntry {
    pub refreshed_at_ms: u64,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Commits the worktree carries ahead of the trunk (`rev-list --count
    /// <merge-base>..HEAD`), refreshed on the same git tick as the diff.
    #[serde(default)]
    pub commits: Option<u32>,
    /// Commits the trunk has advanced past the fork point (`rev-list --count
    /// <merge-base>..<trunk>`), refreshed on the same git tick.
    #[serde(default)]
    pub behind: Option<u32>,
    /// The trunk ref the stats compared against, as the ladder resolved it
    /// (configured `[sidebar] trunk`, else `main`/`master`/remote default).
    /// Names the header's `≡` equal and `✓` clear markers.
    #[serde(default)]
    pub trunk: Option<String>,
    /// Live branch resolved from the worktree path, cached under the same TTL
    /// as the diff stats so the group header tracks `git checkout` without a
    /// git call every tick.
    #[serde(default)]
    pub branch: Option<String>,
    /// Whether the working tree is clean — `git status --porcelain` emptiness,
    /// untracked files included — the safe-to-remove verdict both content-landed
    /// markers (`≡` at the trunk tip, `✓` behind it) require. `None` on an old
    /// cache entry or a failed status read, which the renderer treats as not
    /// proven clean.
    #[serde(default)]
    pub clean: Option<bool>,
    /// Whether committed content is proven landed on the resolved trunk.
    /// `None` means unknown or an old cache entry.
    #[serde(default)]
    pub landed: Option<bool>,
}

impl DiffStatsCacheEntry {
    /// Freshness under the caller's tier: [`DIFF_STATS_TTL`] for a hot
    /// worktree, [`DIFF_STATS_IDLE_TTL`] for the rest. Saturating, so a clock
    /// that ran backwards reads fresh rather than re-forking every tick.
    pub fn is_fresh_for(&self, now_ms: u64, ttl: Duration) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }

    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.is_fresh_for(now_ms, DIFF_STATS_TTL)
    }

    pub fn stats(&self) -> Option<DiffStats> {
        self.added
            .zip(self.removed)
            .map(|(added, removed)| DiffStats { added, removed })
    }
}

pub fn read_diff_stats_cache(path: &Path) -> DiffStatsCache {
    let Ok(bytes) = std::fs::read(path) else {
        return DiffStatsCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_topology_cache_freshness_honors_requested_floor() {
        let cache = PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: 100,
            active_panes: BTreeMap::new(),
            panes: Vec::new(),
        };

        assert!(pane_topology_cache_is_fresh(&cache, 101, Some(100)));
        assert!(!pane_topology_cache_is_fresh(&cache, 101, Some(101)));
    }
}
