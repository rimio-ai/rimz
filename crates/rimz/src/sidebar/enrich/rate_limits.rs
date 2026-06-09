use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fs4::FileExt;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::agents::{AgentRateLimits, RateLimitWindow};
use crate::sidebar::cache::unix_now_ms;
use crate::{RuntimePaths, SidebarSnapshot};

/// The producer's published per-provider rate-limit windows, account-scoped so
/// the budgets outlive a session ending or going idle: the first frame
/// after inactivity paints the last-known bars rather than an empty dashboard.
/// User-scoped like the account cache: producers and detached helpers update it
/// under a shared read-modify-write lock, and every room reads it.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RateLimitsCache {
    /// When the producer last refreshed this map. Observability only: the
    /// reset-to-max projection ages windows on each `resets_at`, not this stamp.
    pub refreshed_at_ms: u64,
    /// Last-known windows by agent kind. Holds *ground truth* — the most recent
    /// live provider reading — never the synthesized full window, which is a
    /// read-time projection recomputed each frame. A logged-out kind is absent.
    pub windows: BTreeMap<String, AgentRateLimits>,
}

/// Read the producer's published rate-limit window cache, or an empty cache on a
/// cold or corrupt file. Read-only and fork-free — every tab's idle fallback.
pub(crate) fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the rate-limit window cache for every tab to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
pub(crate) fn write_rate_limits_cache(path: &Path, cache: &RateLimitsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(path = %path.display(), error = %err, "sidebar rate-limits cache write failed");
    }
}

/// Seed one provider kind's account-scoped windows into the cache out-of-band, so
/// a logged-in-but-idle provider's budget bars paint from the first frame instead
/// of staying blank until a live session reports. Read-modify-write over the
/// existing cache; other kinds are preserved untouched.
///
/// Best-effort and racy by contract: the producer rewrites this file each frame
/// from the panels (live-reading-or-prior), so a write here can be clobbered by a
/// concurrent producer frame. It converges within a frame or two because the
/// producer carries the prior reading forward, and the out-of-band fetch is
/// throttled — so a lost write is simply retried. Used by the detached
/// `rimz codex refresh-rate-limits` helper, never on the per-tick path.
pub fn merge_account_rate_limits(runtime: &RuntimePaths, kind: &str, windows: AgentRateLimits) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) = try_rate_limits_cache_lock(&runtime.shared_rate_limits_lock()) else {
        return;
    };
    let mut cache = read_rate_limits_cache(&path);
    cache.refreshed_at_ms = unix_now_ms();
    cache.windows.insert(kind.to_owned(), windows);
    write_rate_limits_cache(&path, &cache);
}

/// Project one idle provider's cached window for display when no live session
/// reported it this frame. Before its reset instant the last-known (most-drained)
/// reading stands unchanged; once `now` reaches that instant the window has
/// refilled, so synthesize a full window (0% used) with its reset rolled its own
/// `duration_mins` forward, so the countdown still reads sensibly until a live
/// reading overwrites it. A window with no reset, or no known duration to roll by,
/// shows as-is.
pub(crate) fn project_idle_window(cached: RateLimitWindow, now: Timestamp) -> RateLimitWindow {
    match (cached.resets_at, cached.duration_mins) {
        (Some(resets_at), Some(mins)) if resets_at <= now => RateLimitWindow {
            used_percentage: Some(0),
            resets_at: now
                .checked_add(SignedDuration::from_secs(i64::from(mins) * 60))
                .ok(),
            duration_mins: Some(mins),
        },
        _ => cached,
    }
}

/// Whether the cached account reading has aged past its longest dated window.
/// At that point Rimz no longer knows the account's budget shape: the short
/// window may have refilled several times, and the long cap may have refilled
/// too. The cache remains ground truth for persistence, but display switches to
/// unknown bars until a provider reading refreshes it.
fn longest_cached_window_expired(
    prev: &BTreeMap<Option<u32>, RateLimitWindow>,
    now: Timestamp,
) -> bool {
    prev.values()
        .filter_map(|window| Some((window.duration_mins?, window.resets_at?)))
        .max_by_key(|(mins, _)| *mins)
        .is_some_and(|(_, resets_at)| resets_at <= now)
}

/// Preserve the cached window's identity while clearing the value, so the
/// renderer can draw an honest unknown bar (`5h`, `7d`, …) without claiming a
/// refreshed or exhausted budget.
fn unknown_idle_window(cached: RateLimitWindow) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: None,
        resets_at: None,
        duration_mins: cached.duration_mins,
    }
}

/// Fold the persisted account-scoped windows onto the resolved provider panels:
/// a kind with no live reading this frame paints its last-known bars (projected
/// through [`project_idle_window`]'s reset-to-max rule) instead of an empty
/// dashboard. Once the longest cached window has reset with no live reading, the
/// display switches all cached windows to unknown bars until a provider refresh
/// succeeds. Reconciled per window duration, so each budget is carried forward
/// independently while the cache is still inside its long window. On the producer
/// (`persist`) the live readings are written back — and only the live ground
/// truth, never the synthesized full or unknown windows — so budgets survive a
/// session ending or going idle. The written cache tracks login: it is rebuilt
/// from the panels alone, so a logged-out kind (no panel) drops out. A consumer
/// reads the same cache but never writes it.
pub(crate) fn apply_rate_limit_cache(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    persist: bool,
) {
    // No dashboard, no windows: skip the cache I/O entirely. A room with no
    // logged-in provider has nothing to fall back to and nothing to persist, so
    // this stays off the per-tick path there — the same idle-room gate the
    // context/activity reads use. A logged-out provider is reaped on the next
    // frame that still has a panel (it rebuilds the cache from the panels alone).
    if snapshot.providers.is_empty() {
        return;
    }

    let path = runtime.shared_rate_limits_path();
    if persist {
        let Some(_guard) = try_rate_limits_cache_lock(&runtime.shared_rate_limits_lock()) else {
            let cached = read_rate_limits_cache(&path);
            apply_rate_limit_cache_with(snapshot, &cached, false);
            return;
        };
        let cached = read_rate_limits_cache(&path);
        if let Some(next) = apply_rate_limit_cache_with(snapshot, &cached, true) {
            write_rate_limits_cache(&path, &next);
        }
        return;
    }

    let cached = read_rate_limits_cache(&path);
    apply_rate_limit_cache_with(snapshot, &cached, false);
}

fn try_rate_limits_cache_lock(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    FileExt::try_lock(&file).ok()?;
    Some(file)
}

fn apply_rate_limit_cache_with(
    snapshot: &mut SidebarSnapshot,
    cached: &RateLimitsCache,
    persist: bool,
) -> Option<RateLimitsCache> {
    // The snapshot's single projection clock, so the idle-window reset
    // projection agrees with the dashboard windows resolved on the same frame.
    let now = snapshot.now;
    let mut next = RateLimitsCache {
        refreshed_at_ms: unix_now_ms(),
        windows: BTreeMap::new(),
    };

    for panel in &mut snapshot.providers {
        // Index this kind's live (this-frame) and cached (last-known) readings by
        // window duration, so each duration is reconciled independently.
        let live: BTreeMap<Option<u32>, RateLimitWindow> = std::mem::take(&mut panel.windows)
            .into_iter()
            .map(|window| (window.duration_mins, window))
            .collect();
        let prev: BTreeMap<Option<u32>, RateLimitWindow> = cached
            .windows
            .get(&panel.kind)
            .into_iter()
            .flat_map(|limits| limits.windows.iter())
            .map(|window| (window.duration_mins, window.clone()))
            .collect();
        let durations: BTreeSet<Option<u32>> = live.keys().chain(prev.keys()).copied().collect();
        let cache_unknown = live.is_empty() && longest_cached_window_expired(&prev, now);

        // Persist ground truth only: a live reading supersedes the cached one;
        // absent one, the prior reading is retained unchanged. The synthesized
        // full window below is never written — it is recomputed each frame.
        if persist {
            let truth: Vec<RateLimitWindow> = durations
                .iter()
                .filter_map(|duration| live.get(duration).or_else(|| prev.get(duration)).cloned())
                .collect();
            if !truth.is_empty() {
                next.windows
                    .insert(panel.kind.clone(), AgentRateLimits { windows: truth });
            }
        }

        // Display: a live reading wins; otherwise the cached reading, projected.
        // Sorted short→long for a stable paint order.
        let mut display: Vec<RateLimitWindow> = durations
            .iter()
            .filter_map(|duration| {
                live.get(duration).cloned().or_else(|| {
                    prev.get(duration).cloned().map(|window| {
                        if cache_unknown {
                            unknown_idle_window(window)
                        } else {
                            project_idle_window(window, now)
                        }
                    })
                })
            })
            .collect();
        display.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
        panel.windows = display;
    }

    persist.then_some(next)
}
