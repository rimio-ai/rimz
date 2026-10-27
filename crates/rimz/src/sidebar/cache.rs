//! Passive pane-frame decoding and presence/topology hints for the sidebar data plane.
//!
//! The host publishes the pane frame atomically in the workspace runtime directory. Passive readers consume it opportunistically; presence hints and the authoritative liveness probe keep their separate writers.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::disk::parse_cache::{ParseCache, StampedPath};
use crate::ids::MuxName;
use crate::mux::PRESENCE_STAMP_FRESH;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::{EVENT_PANE_TTL, HIBERNATED_CHECK_TTL, SNAPSHOT_CACHE_TTL};
use crate::utils::time::unix_now_ms;

/// Held keys check immediately, then on their own cadence; unheld keys forget the stamp.
pub(crate) fn hibernated_check_due<K: Eq + Hash>(
    checks: &mut HashMap<K, Instant>,
    key: K,
    hibernated: bool,
    now: Instant,
) -> bool {
    if !hibernated {
        checks.remove(&key);
        return true;
    }
    if checks
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < HIBERNATED_CHECK_TTL)
    {
        return false;
    }
    checks.insert(key, now);
    true
}

// The runtime root scopes the workspace; `session_name` prevents cross-session reuse during detach or rotation. Store rollups remain event-fresh independently of the pane publication.
thread_local! {
    /// Passive readers reuse this thread's last `snapshot.json` parse ([`ParseCache`]) while the atomic file identity is unchanged. The host seeds once and retains its own inputs thereafter.
    static SNAPSHOT_PARSE_CACHE: ParseCache<PaneFrame> = const { ParseCache::new() };
}

/// Read a same-session publication regardless of host freshness. Absent, unreadable, and wrong-session files return `None`; passive reads, host startup, and private degraded reads share this entry.
///
/// An unchanged atomic file identity skips parsing. A rename between stat and read costs at most one redundant parse next call, never a torn value.
pub fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<Arc<PaneFrame>> {
    let stamped = StampedPath::of(cache_path);

    let cache = match SNAPSHOT_PARSE_CACHE.with(|cache| cache.get_stamped(&stamped)) {
        Some(cache) => cache,
        None => {
            let bytes = std::fs::read(cache_path).ok()?;
            let mut parsed: PaneFrame = serde_json::from_slice(&bytes).ok()?;
            normalize_observed_stamp(&mut parsed);
            let parsed = Arc::new(parsed);
            SNAPSHOT_PARSE_CACHE.with(|cache| {
                cache.store_stamped(&stamped, Arc::clone(&parsed));
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

/// Whether the host may reuse a retained frame: publication age at most `ttl`, with `observed_at_ms` at or after the freshness floor. Lifecycle and resize floors bypass even [`EVENT_PANE_TTL`]; saturating age treats a future clock stamp as age zero.
pub(crate) fn snapshot_cache_is_fresh(
    cache: &PaneFrame,
    now_ms: u64,
    min_produced_at_ms: Option<u64>,
    ttl: Duration,
) -> bool {
    let fresh = now_ms.saturating_sub(cache.produced_at_ms) <= ttl.as_millis() as u64;
    let new_enough = min_produced_at_ms.is_none_or(|min| cache.observed_at_ms >= min);
    fresh && new_enough
}

/// The presence liveness stamp refreshed by the Zellij presence plugin through
/// `rimz sidebar wake` and by the tmux control-mode watch. Its freshness gates
/// the producer's pane TTL: fresh → event mode (`EVENT_PANE_TTL`), stale or absent → poll mode
/// (`SNAPSHOT_CACHE_TTL`). Cache-class JSON in the workspace runtime root;
/// the explicit millisecond field (over a bare mtime stamp) lets `rimz doctor`
/// render a stamp age from the same value the producer's verdict reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceStamp {
    pub written_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<MuxName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
}

/// Path of the presence stamp, beside the pane cache it gates.
pub fn presence_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.live_path("presence.stamp")
}

/// Refresh the presence stamp. Best-effort and cache-class (rename atomicity,
/// no fsync — it is rewritten every poke and survives no power cut by design);
/// a failed write only delays the channel reading as alive by one poke.
pub(crate) fn write_presence_stamp(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: Option<&str>,
) {
    let stamp = PresenceStamp {
        written_at_ms: unix_now_ms(),
        mux: Some(mux),
        session_name: session_name.map(ToOwned::to_owned),
    };
    let path = presence_stamp_path(runtime);
    if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(&path, &stamp) {
        tracing::debug!(path = %path.display(), error = %err, "presence stamp write failed");
    }
}

/// Read the presence stamp; absent or unreadable means poll mode.
pub fn read_presence_stamp(runtime: &RuntimePaths) -> Option<PresenceStamp> {
    let bytes = std::fs::read(presence_stamp_path(runtime)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn presence_stamp_age_ms(runtime: &RuntimePaths) -> Option<u64> {
    read_presence_stamp(runtime).map(|stamp| unix_now_ms().saturating_sub(stamp.written_at_ms))
}

/// Whether the presence push channel is alive: the stamp exists and is younger
/// than [`PRESENCE_STAMP_FRESH`]. Pure over the age so the boundary is
/// unit-testable without touching disk; `None` (absent stamp) is poll mode.
pub fn presence_event_mode(stamp_age_ms: Option<u64>) -> bool {
    stamp_age_ms.is_some_and(|age| age <= PRESENCE_STAMP_FRESH.as_millis() as u64)
}

/// The host's pane TTL: event mode while presence is alive or its retained prior is unwatched, otherwise poll mode. An absent prior counts as watched.
pub(super) fn effective_pane_ttl(stamp_age_ms: Option<u64>, unwatched: bool) -> Duration {
    if unwatched || presence_event_mode(stamp_age_ms) {
        EVENT_PANE_TTL
    } else {
        SNAPSHOT_CACHE_TTL
    }
}

/// Publish the sidebar supervisor's shared authoritative pane observation.
/// Cache-class: rename atomic, no fsync, and safe to discard between probes.
pub(crate) fn write_authoritative_pane_probe<T: Serialize>(
    runtime: &RuntimePaths,
    probe: &T,
) -> crate::disk::atomic::Result<()> {
    crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.authoritative_pane_probe_path(),
        probe,
    )
}

#[cfg(test)]
mod tests;
