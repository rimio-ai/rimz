//! Pane-frame runtime cache and presence/topology hints for the sidebar data plane.
//!
//! These files live under the workspace runtime directory and are cache-class.
//! Producers publish them with temp-file-plus-rename, consumers read them
//! opportunistically, and every value can be rebuilt from live mux state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::mux::zellij::pane_topology::PaneTopologyCache;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::{
    EVENT_PANE_TTL, PRESENCE_STAMP_FRESH, SNAPSHOT_CACHE_TTL, unix_now_ms,
};
use crate::store::parse_cache::ParseCache;

// The shared pane frame cache is keyed to one `(workspace, session)`: the
// per-workspace runtime root scopes the workspace, and `session_name` prevents
// serving one session's panes to another during detach or session rotation. It
// caches only the expensive pane roster read; the store rollup stays
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
pub fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<Arc<PaneFrame>> {
    let meta = std::fs::metadata(cache_path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();

    let cache = match SNAPSHOT_PARSE_CACHE.with(|cache| cache.get(cache_path, mtime, len)) {
        Some(cache) => cache,
        None => {
            let bytes = std::fs::read(cache_path).ok()?;
            let mut parsed: PaneFrame = serde_json::from_slice(&bytes).ok()?;
            normalize_observed_stamp(&mut parsed);
            let parsed = Arc::new(parsed);
            SNAPSHOT_PARSE_CACHE.with(|cache| {
                cache.store(cache_path, mtime, len, Arc::clone(&parsed));
            });
            parsed
        }
    };
    (cache.session_name == session).then_some(cache)
}

fn normalize_observed_stamp(frame: &mut PaneFrame) {
    if frame.observed_at_ms == 0 {
        frame.observed_at_ms = frame.produced_at_ms;
    }
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
    let new_enough = min_produced_at_ms.is_none_or(|min| cache.observed_at_ms >= min);
    fresh && new_enough
}

/// The producer and observation timestamps of the published same-session pane
/// frame. `None` when no usable same-session frame exists.
pub fn published_frame_stamps(runtime: &RuntimePaths, session: &str) -> Option<(u64, u64)> {
    let cache_path = runtime.pane_frame_path();
    read_snapshot_cache(&cache_path, session)
        .map(|cache| (cache.produced_at_ms, cache.observed_at_ms))
}

/// Whether the published same-session frame shows no attached client viewing any
/// pane. An absent frame reads as watched so cold starts keep the responsive
/// poll-mode cadence until a producer publishes real focus state.
pub fn published_frame_unwatched(runtime: &RuntimePaths, session: &str) -> bool {
    let cache_path = runtime.pane_frame_path();
    read_snapshot_cache(&cache_path, session).is_some_and(|cache| cache.viewed_panes.is_empty())
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

/// Timestamp of the latest tmux presence probe attempt. This cache-class hint
/// throttles external `list-clients` calls; pane presence remains sourced from
/// the published frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceProbeStamp {
    pub written_at_ms: u64,
}

/// Path of the presence stamp, beside the pane cache it gates.
pub fn presence_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("presence.stamp")
}

pub fn presence_probe_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("client-presence-probe.stamp")
}

pub fn write_presence_probe_stamp(
    runtime: &RuntimePaths,
    written_at_ms: u64,
) -> crate::store::atomic::Result<()> {
    crate::store::atomic::write_temp_then_rename_cache(
        &presence_probe_stamp_path(runtime),
        &PresenceProbeStamp { written_at_ms },
    )
}

pub fn read_presence_probe_stamp(runtime: &RuntimePaths) -> Option<u64> {
    let bytes = std::fs::read(presence_probe_stamp_path(runtime)).ok()?;
    let stamp: PresenceProbeStamp = serde_json::from_slice(&bytes).ok()?;
    Some(stamp.written_at_ms)
}

/// Refresh the presence stamp. Best-effort and cache-class (rename atomicity,
/// no fsync — it is rewritten every poke and survives no power cut by design);
/// a failed write only delays the channel reading as alive by one poke.
pub fn write_presence_stamp(runtime: &RuntimePaths) {
    let stamp = PresenceStamp {
        written_at_ms: unix_now_ms(),
    };
    let path = presence_stamp_path(runtime);
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(&path, &stamp) {
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
/// presence channel is alive or the published frame is unwatched, else the
/// poll-mode TTL. Computed once per `cached_panes_or_produce` call and threaded
/// through every freshness check — the fast path, the single-flight `fresh`
/// closure, and the loser re-check — so they agree on one verdict and a loser
/// never produces what the winner skipped (the diff-stats "one shared stale()
/// closure" rule).
pub fn effective_pane_ttl(stamp_age_ms: Option<u64>, unwatched: bool) -> Duration {
    if unwatched || presence_event_mode(stamp_age_ms) {
        EVENT_PANE_TTL
    } else {
        SNAPSHOT_CACHE_TTL
    }
}

/// Path of the Zellij presence-plugin topology cache, beside the producer's
/// `snapshot.json` pane frame. The topology cache is Zellij's pane roster; the
/// normal producer frame still carries the rendered view-model.
pub fn pane_topology_cache_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("pane-topology.json")
}

/// Publish the plugin-provided pane topology. Cache-class: rename atomic, no
/// fsync, rebuilt by the next presence event or by the CLI fallback.
pub fn write_pane_topology_cache(
    runtime: &RuntimePaths,
    cache: &PaneTopologyCache,
) -> crate::store::atomic::Result<()> {
    crate::store::atomic::write_temp_then_rename_cache(&pane_topology_cache_path(runtime), cache)
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

/// Whether a same-session plugin topology payload is young enough to use as
/// Zellij's roster. The window matches the presence liveness window so one
/// normal keepalive jitter does not fail a read. Normal verification pulls use
/// no topology floor: the wake that asks for verification has already written
/// the topology payload it carries. The optional floor is reserved for explicit
/// structural repair after a local mux mutation.
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

#[cfg(test)]
mod tests;
