use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

#[cfg(test)]
use crate::agents::ProviderAccountScope;
use crate::agents::account::{
    PendingRefill, RateLimitCacheEntry, RateLimitsCache, read_rate_limits_cache,
};
use crate::agents::context::RateLimitWindowKey;
use crate::agents::{AccountUsageIdentity, AgentRateLimits, RateLimitWindow};
use crate::sidebar::timing::unix_now_ms;
use crate::{RuntimePaths, SidebarSnapshot};

#[cfg(test)]
mod tests;

/// How long an uncorroborated drop must persist before the bar follows it down.
/// This covers a best-effort free-reset candidate and an authoritative
/// same-epoch drop contested by stamped best-effort truth. Shorter, a single
/// lagging or garbled sample could dip the bar; longer is needless lag on a real
/// refill. Tuned against captured reset traces (see [`trace_rate_limits`]).
pub(crate) const REFILL_CONFIRM_SECS: i64 = 120;
/// Coarse backstop: a live candidate captured longer ago than this is ignored.
/// Content-staleness is already caught upstream — the snapshot view drops a
/// reading whose shortest applicable window has reset — so this only guards a
/// wildly old reading slipping through.
pub(crate) const LIVE_HORIZON_SECS: i64 = 6 * 3600;
/// A later reset instant must beat the prior by more than this to count as a new
/// window epoch; sub-second parse jitter between sessions never does.
const RESET_ADVANCE_SECS: i64 = 60;
/// The used-percentage at or below which a best-effort drop reads as a free
/// reset. A refill restores the budget toward full (~0% used), so only a low
/// reading carries the reset signature and earns the confirm-before-drop path.
/// A mid-range best-effort drop above this floor (say 80% → 70%) is jitter or a
/// misread, never a reset — the bar holds its most-drained prior unless an
/// authoritative source says otherwise.
const REFILL_FLOOR_PCT: u8 = 25;

/// Publish the rate-limit window cache for every tab to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
pub(crate) fn write_rate_limits_cache(path: &Path, cache: &RateLimitsCache) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.rate_limits_write",
            error = &err as &dyn std::error::Error,
            "sidebar rate-limits cache write failed",
        );
    }
}

/// Seed one provider kind's account-scoped windows into the cache out-of-band, so
/// a logged-in-but-idle provider's budget bars paint from the first frame instead
/// of staying blank until a live session reports. Read-modify-write over the
/// existing cache; other kinds are preserved untouched.
///
/// Detached authoritative writers wait for the producer's short critical
/// section so an accepted OAuth observation is published before its five-minute
/// attempt throttle advances. Used by the detached `rimz agents refresh-usage`
/// helper, never on the per-tick path.
pub fn merge_account_rate_limits(
    runtime: &RuntimePaths,
    kind: &str,
    identity: AccountUsageIdentity,
    windows: AgentRateLimits,
) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) = acquire_rate_limits_cache_lock(
        &runtime.shared_rate_limits_lock(),
        "cache.rate_limits_merge_lock",
    ) else {
        return;
    };
    let mut cache = read_rate_limits_cache(&path);
    cache.refreshed_at_ms = unix_now_ms();
    // Stamp the authoritative capture instant, then pass every reported window
    // through the same fusion used by the sidebar producer. A same-epoch drop
    // against stamped best-effort truth must survive the shared debounce.
    let observed_at = Timestamp::now();
    let mut windows = windows.stamped_at(observed_at);
    let prior_entry = cache
        .entries
        .get(kind)
        .filter(|entry| entry.scope == identity.scope && entry.account_key == identity.account_key);
    // Carry the open unknown episode rather than closing it here. This write
    // proves an authoritative attempt landed, not that it carried usable windows;
    // the producer clears the marker once a real value paints, so a provider that
    // answers with nothing forces one fetch instead of one per frame.
    let unknown_since_ms = prior_entry.and_then(|entry| entry.unknown_since_ms);
    let prior_fused = prior_entry
        .map(|entry| entry.limits.windows.as_slice())
        .unwrap_or_default();
    let prior_bound = prior_entry
        .and_then(|entry| entry.bound_limits.as_ref())
        .map(|limits| limits.windows.as_slice())
        .unwrap_or(prior_fused);
    complete_omitted_duration_windows(prior_bound, &mut windows);
    let bound_limits = identity.account_key.is_some().then(|| windows.clone());
    let prior: BTreeMap<RateLimitWindowKey, RateLimitWindow> = prior_fused
        .iter()
        .map(|window| (window.key(), window.clone()))
        .collect();
    let prior_pending: BTreeMap<RateLimitWindowKey, PendingRefill> = prior_entry
        .into_iter()
        .flat_map(|entry| entry.pending.iter())
        .map(|pending| (pending.key(), pending.clone()))
        .collect();
    let live: BTreeMap<RateLimitWindowKey, RateLimitWindow> = windows
        .windows
        .into_iter()
        .map(|window| (window.key(), window))
        .collect();
    let mut fused = Vec::with_capacity(live.len());
    let mut pending = Vec::new();
    for (key, live) in live {
        let (truth, refill) = fuse_window(
            prior.get(&key),
            Some(&live),
            prior_pending.get(&key),
            observed_at,
            true,
        );
        fused.extend(truth);
        pending.extend(refill);
    }
    cache.entries.insert(
        kind.to_owned(),
        RateLimitCacheEntry {
            scope: identity.scope,
            account_key: identity.account_key,
            limits: AgentRateLimits { windows: fused },
            bound_limits,
            pending,
            unknown_since_ms,
        },
    );
    write_rate_limits_cache(&path, &cache);
}

/// Drop one provider kind's account-scoped windows after the local OAuth account
/// key changes. The detached writer waits for normal producer contention; a
/// real lock acquisition failure or absent kind is a no-op.
pub fn drop_kind_rate_limits(runtime: &RuntimePaths, kind: &str) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) = acquire_rate_limits_cache_lock(
        &runtime.shared_rate_limits_lock(),
        "cache.rate_limits_drop_lock",
    ) else {
        return;
    };
    let mut cache = read_rate_limits_cache(&path);
    if cache.entries.remove(kind).is_none() {
        return;
    }
    cache.refreshed_at_ms = unix_now_ms();
    write_rate_limits_cache(&path, &cache);
}

/// Project a budget window's reset-to-max roll forward to `now`: the timestamp-
/// aware refill the dashboard and the window-priming guard share. Before the
/// reset the last-known (most-drained) reading stands unchanged; once `now`
/// reaches the reset the window has refilled, so synthesize a full window (0%
/// used) with its reset rolled its own `duration_mins` forward, so the countdown
/// reads sensibly until a live reading overwrites it. A window with no reset, or
/// no known duration to roll by, shows as-is.
pub(crate) fn project_window(cached: RateLimitWindow, now: Timestamp) -> RateLimitWindow {
    cached.projected_at(now)
}

/// Whether the cached account reading has aged past its longest freshness
/// ceiling. A dated duration or named quota supplies its own deadline. If every
/// window is undated, the newest observation may live for at most the shortest
/// reported duration. Past that point RimZ no longer knows the account's budget
/// shape, so display switches to unknown bars until a provider reading refreshes
/// it while the cache remains ground truth for persistence.
fn longest_cached_window_expired(
    prev: &BTreeMap<RateLimitWindowKey, RateLimitWindow>,
    now: Timestamp,
) -> bool {
    let duration_reset = prev
        .values()
        .filter_map(|window| Some((window.duration_mins?, window.resets_at?)))
        .max_by_key(|(mins, _)| *mins)
        .map(|(_, resets_at)| resets_at);
    duration_reset
        .or_else(|| {
            prev.values()
                .filter(|window| window.scope.is_some())
                .filter_map(|window| window.resets_at)
                .max()
        })
        .or_else(|| {
            let observed_at = prev
                .values()
                .filter_map(|window| window.observed_at)
                .max()?;
            let duration_mins = prev
                .values()
                .filter_map(|window| window.duration_mins)
                .min()?;
            observed_at
                .checked_add(SignedDuration::from_mins(i64::from(duration_mins)))
                .ok()
        })
        .is_some_and(|resets_at| resets_at <= now)
}

/// Preserve the cached window's identity while clearing the value, so the
/// renderer can draw an honest unknown bar (`5h`, `7d`, …) without claiming a
/// refreshed or exhausted budget.
fn unknown_idle_window(cached: RateLimitWindow) -> RateLimitWindow {
    RateLimitWindow {
        scope: cached.scope,
        used_percentage: None,
        resets_at: None,
        duration_mins: cached.duration_mins,
        observed_at: cached.observed_at,
        source: cached.source,
        lifted: cached.lifted,
    }
}

/// Fold the persisted account-scoped windows onto the resolved provider panels:
/// a kind with no live reading this frame paints its last-known bars (projected
/// through [`project_window`]'s reset-to-max rule) instead of an empty
/// dashboard. Once the account freshness ceiling has reset with no live reading,
/// the display switches all cached windows to unknown bars until a provider
/// refresh succeeds. Reconciled per stable window identity, so each budget is
/// carried forward independently while the cache remains current. On the producer
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
    // No dashboard: nothing to project onto and nothing to fall back to. A
    // consumer is done — the same idle-room gate the context/activity reads use.
    // The producer must still reap the cache once *every* provider has logged
    // out: with no surviving panel to rebuild from, the per-panel reap below
    // never runs, so a stale cache would flash old budgets on a later re-login.
    // The reap is a no-op on an already-empty cache, keeping a logged-out room
    // off the per-tick write path.
    if snapshot.providers.is_empty() {
        if persist {
            reset_logged_out_rate_limits_cache(runtime);
        }
        return;
    }

    let path = runtime.shared_rate_limits_path();
    if persist {
        let reset_kinds = {
            let Some(_guard) =
                crate::store::lock::WorkspaceLock::try_acquire(&runtime.shared_rate_limits_lock())
                    .ok()
                    .flatten()
            else {
                let cached = read_rate_limits_cache(&path);
                let _ = apply_rate_limit_cache_with(snapshot, &cached, false, None);
                return;
            };
            let cached = read_rate_limits_cache(&path);
            let trace = rate_limits_trace_path(runtime);
            let (next, reset_kinds) =
                apply_rate_limit_cache_with(snapshot, &cached, true, trace.as_deref());
            if let Some(next) = next {
                write_rate_limits_cache(&path, &next);
            }
            reset_kinds
        };
        for kind in reset_kinds {
            super::credits::invalidate_oauth_read(runtime, &kind);
        }
        return;
    }

    let cached = read_rate_limits_cache(&path);
    let _ = apply_rate_limit_cache_with(snapshot, &cached, false, None);
}

/// Clear the published cache once every provider has logged out, so a later
/// re-login paints from live readings rather than stale budgets. A no-op when
/// the cache is already empty or the RMW lock is held — a contending producer's
/// frame reaps it instead. Producer-only.
fn reset_logged_out_rate_limits_cache(runtime: &RuntimePaths) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) =
        crate::store::lock::WorkspaceLock::try_acquire(&runtime.shared_rate_limits_lock())
            .ok()
            .flatten()
    else {
        return;
    };
    let cached = read_rate_limits_cache(&path);
    if cached.entries.is_empty() {
        return;
    }
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: unix_now_ms(),
            ..Default::default()
        },
    );
}

fn acquire_rate_limits_cache_lock(
    path: &Path,
    operation: &'static str,
) -> Option<crate::store::lock::WorkspaceLock> {
    match crate::store::lock::WorkspaceLock::acquire(path) {
        Ok(guard) => Some(guard),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                tags.operation = operation,
                error = &err as &dyn std::error::Error,
                "sidebar rate-limits cache lock failed",
            );
            None
        }
    }
}

/// Complete duration windows omitted by a stamped authoritative temporal
/// reading. Expected durations come only from same-scope persisted
/// authoritative truth; named quotas and best-effort readings cannot claim
/// that an absent limit is lifted.
fn complete_omitted_duration_windows(prior: &[RateLimitWindow], current: &mut AgentRateLimits) {
    let Some(observed_at) = current
        .windows
        .iter()
        .filter(|window| {
            window.scope.is_none()
                && window.duration_mins.is_some()
                && window.source.is_authoritative()
        })
        .filter_map(|window| window.observed_at)
        .max()
    else {
        return;
    };
    let reported: BTreeSet<u32> = current
        .windows
        .iter()
        .filter(|window| window.scope.is_none())
        .filter_map(|window| window.duration_mins)
        .collect();
    let expected: BTreeSet<u32> = prior
        .iter()
        .filter(|window| window.scope.is_none() && window.source.is_authoritative())
        .filter_map(|window| window.duration_mins)
        .collect();
    current.windows.extend(
        expected
            .into_iter()
            .filter(|duration| !reported.contains(duration))
            .map(|duration_mins| RateLimitWindow {
                duration_mins: Some(duration_mins),
                observed_at: Some(observed_at),
                source: crate::agents::context::WindowSource::Authoritative,
                lifted: true,
                ..Default::default()
            }),
    );
}

fn apply_rate_limit_cache_with(
    snapshot: &mut SidebarSnapshot,
    cached: &RateLimitsCache,
    persist: bool,
    trace: Option<&Path>,
) -> (Option<RateLimitsCache>, Vec<String>) {
    // The snapshot's single projection clock, so the idle-window reset
    // projection agrees with the dashboard windows resolved on the same frame.
    let now = snapshot.now;
    let mut next = RateLimitsCache {
        refreshed_at_ms: unix_now_ms(),
        ..Default::default()
    };
    let mut refresh_kinds = BTreeSet::new();

    for panel in &mut snapshot.providers {
        if !panel.metered {
            panel.windows.clear();
            continue;
        }
        // Complete authoritative omissions against matching persisted truth
        // before indexing the live reading, then fuse each stable duration or
        // named-quota identity independently.
        let prior_entry = cached
            .entries
            .get(&panel.kind)
            .filter(|entry| entry.scope == panel.account_scope);
        let mut live_limits = AgentRateLimits {
            windows: std::mem::take(&mut panel.windows),
        };
        let prior_authoritative = prior_entry
            .and_then(|entry| entry.bound_limits.as_ref())
            .map(|limits| limits.windows.as_slice())
            .or_else(|| prior_entry.map(|entry| entry.limits.windows.as_slice()))
            .unwrap_or_default();
        complete_omitted_duration_windows(prior_authoritative, &mut live_limits);
        let live: BTreeMap<RateLimitWindowKey, RateLimitWindow> = live_limits
            .windows
            .into_iter()
            .map(|window| (window.key(), window))
            .collect();
        let prev: BTreeMap<RateLimitWindowKey, RateLimitWindow> = prior_entry
            .into_iter()
            .flat_map(|entry| entry.limits.windows.iter())
            .map(|window| (window.key(), window.clone()))
            .collect();
        let prev_pending: BTreeMap<RateLimitWindowKey, PendingRefill> = cached
            .entries
            .get(&panel.kind)
            .filter(|entry| entry.scope == panel.account_scope)
            .into_iter()
            .flat_map(|entry| entry.pending.iter())
            .map(|refill| (refill.key(), refill.clone()))
            .collect();
        let window_keys: BTreeSet<RateLimitWindowKey> =
            live.keys().chain(prev.keys()).cloned().collect();

        // Fuse each duration to its ground truth and carry or advance its
        // debounce marker. A live reading drives the fusion; absent one, the
        // prior truth is carried unchanged for the idle projection below.
        let mut truth: BTreeMap<RateLimitWindowKey, RateLimitWindow> = BTreeMap::new();
        let mut pending: Vec<PendingRefill> = Vec::new();
        let mut kind_reset_advanced = false;
        for key in &window_keys {
            let (window, refill) = fuse_window(
                prev.get(key),
                live.get(key),
                prev_pending.get(key),
                now,
                persist,
            );
            if let (Some(window), Some(path)) = (window.as_ref(), trace) {
                trace_rate_limits(path, &panel.kind, live.get(key), window, now);
            }
            if let Some(window) = window {
                if persist
                    && prev
                        .get(key)
                        .is_some_and(|prev| reset_advanced(prev.resets_at, window.resets_at))
                {
                    kind_reset_advanced = true;
                }
                truth.insert(key.clone(), window);
            }
            if let Some(refill) = refill {
                pending.push(refill);
            }
        }
        let cache_unknown = live.is_empty() && longest_cached_window_expired(&truth, now);
        if persist && !cache_unknown && kind_reset_advanced {
            refresh_kinds.insert(panel.kind.clone());
        }

        // Display: roll every fused window's reset-to-max projection forward to
        // `now` — a no-op while its reset is future, a refill once it has passed,
        // so a live reading carrying an expired longer window never freezes at
        // `0h00m`. Once the account freshness ceiling has aged out with no live
        // reading, the cache shows unknown bars. Sorted for stable paint order.
        let mut display: Vec<RateLimitWindow> = truth
            .values()
            .cloned()
            .map(|window| {
                let expired_named_quota = window.scope.is_some()
                    && window.duration_mins.is_none()
                    && window.resets_at.is_some_and(|reset| reset <= now);
                if cache_unknown || expired_named_quota {
                    unknown_idle_window(window)
                } else {
                    project_window(window, now)
                }
            })
            .collect();
        crate::store::snapshot::sort_windows(&mut display);

        // A dashboard with no usable value is a refresh trigger, not only a paint
        // state: RimZ no longer knows this account's budget, so the authoritative
        // probe runs now rather than waiting out the OAuth read's cadence. It
        // covers every route to a blank panel — an aged-out cache, expired named
        // quotas, and a cold start whose cache was never written. The marker
        // carries the open episode so the force fires on the transition alone;
        // the durable claim keeps the fetch itself single-flight, and completion
        // restamps the read on success and failure alike, so a provider that
        // stays unreachable falls back to ordinary throttling.
        let display_unknown = persist
            && display
                .iter()
                .all(|window| window.used_percentage.is_none());
        let unknown_since_ms = match prior_entry.and_then(|entry| entry.unknown_since_ms) {
            _ if !display_unknown => None,
            Some(since) => Some(since),
            None => {
                refresh_kinds.insert(panel.kind.clone());
                Some(unix_now_ms())
            }
        };
        panel.windows = display;

        // Persist fused truth, including authoritative lifted rows, any in-flight
        // refill, and the open unknown episode's marker. Display-only reset
        // projections and unknown windows are recomputed each frame.
        if persist && (!truth.is_empty() || !pending.is_empty() || unknown_since_ms.is_some()) {
            let limits = AgentRateLimits {
                windows: truth.values().cloned().collect(),
            };
            let mut entry = if let Some(prior) = prior_entry {
                let mut entry = prior.clone();
                // A provider panel carries display scope but no credential
                // identity. Preserve a bound authoritative copy for exact-account
                // controls while publishing the fused truth for every reader.
                if entry.account_key.is_some() && entry.bound_limits.is_none() {
                    entry.bound_limits = Some(entry.limits.clone());
                }
                entry.limits = limits;
                entry.pending = pending;
                entry
            } else {
                RateLimitCacheEntry {
                    scope: panel.account_scope.clone(),
                    account_key: None,
                    limits,
                    bound_limits: None,
                    pending,
                    unknown_since_ms: None,
                }
            };
            entry.unknown_since_ms = unknown_since_ms;
            next.entries.insert(panel.kind.clone(), entry);
        }
    }

    (persist.then_some(next), refresh_kinds.into_iter().collect())
}

/// Fuse one stable window identity's prior truth with this frame's live reading
/// into the new ground truth, carrying or advancing the debounce marker that
/// guards an uncorroborated drop.
///
/// Usage only climbs within a live window, so a reading at or above the prior is
/// real consumption and is adopted at once — stable against parallel sessions
/// reporting the same budget at different instants. A *drop* is a refill, earned
/// rather than assumed, in order:
/// - an out-of-order authoritative reading cannot undo newer truth;
/// - a later reset instant (a new window epoch) lowers the bar immediately;
/// - a current authoritative reading lowers authoritative, unprovenanced, or
///   epoch-less truth immediately;
/// - a same-epoch authoritative drop against stamped best-effort truth is parked
///   until it has stood for [`REFILL_CONFIRM_SECS`];
/// - a best-effort drop toward full (at or below [`REFILL_FLOOR_PCT`], the
///   free-reset signature) is parked and the higher bar held until the drop has
///   stood for [`REFILL_CONFIRM_SECS`]; a mid-range best-effort drop is jitter
///   and holds the most-drained prior.
///
/// `allow_confirm` is the producer flag — a consumer never lowers the bar on its
/// own, it mirrors the producer's persisted truth.
pub(crate) fn fuse_window(
    prior: Option<&RateLimitWindow>,
    live: Option<&RateLimitWindow>,
    pending: Option<&PendingRefill>,
    now: Timestamp,
    allow_confirm: bool,
) -> (Option<RateLimitWindow>, Option<PendingRefill>) {
    let Some(live) = live else {
        // No live reading this frame: carry the prior truth and its marker.
        return (prior.cloned(), pending.cloned());
    };
    let Some(prior) = prior else {
        // First reading for this identity: adopt it, nothing pending. With no
        // prior to carry, the observation horizon cannot improve the result.
        return (Some(live.clone()), None);
    };
    // Coarse backstop: ignore a wildly old live reading (content-staleness is
    // already filtered upstream by the snapshot view's reading-level check).
    if let Some(observed_at) = live.observed_at
        && now.duration_since(observed_at).as_secs() > LIVE_HORIZON_SECS
    {
        return (Some(prior.clone()), pending.cloned());
    }
    let prior_used = prior.used_percentage.unwrap_or(0);
    let live_used = live.used_percentage.unwrap_or(0);

    // Climb or steady: adopt at once and drop any parked refill.
    if live_used >= prior_used {
        return (Some(live.clone()), None);
    }

    // --- a drop is a refill, earned not assumed ---

    // An out-of-order authoritative sidecar cannot lower newer truth. A current
    // reading settles an authoritative, unprovenanced, or epoch-less prior at
    // once; against stamped best-effort truth in the same epoch it must earn the
    // drop through the shared confirmation below.
    if live.source.is_authoritative() {
        if !authoritative_supersedes(live, prior) {
            return (Some(prior.clone()), pending.cloned());
        }
        if reset_advanced(prior.resets_at, live.resets_at)
            || prior.source.is_authoritative()
            || prior.observed_at.is_none()
            || prior.resets_at.is_none()
        {
            return (Some(live.clone()), None);
        }
    } else {
        // A best-effort reading whose reset instant advanced is a free reset with
        // a moved timer — a new window epoch, trusted at once.
        if reset_advanced(prior.resets_at, live.resets_at) {
            return (Some(live.clone()), None);
        }

        // A best-effort drop is a refill candidate only when it lands at or below
        // the reset floor (near-full). A mid-range drop is jitter — hold the
        // most-drained prior, carrying any in-flight marker untouched.
        if live_used > REFILL_FLOOR_PCT {
            return (Some(prior.clone()), pending.cloned());
        }
    }

    // Uncorroborated refill candidate.
    if !allow_confirm {
        // A consumer holds the producer's persisted (higher) truth.
        return (Some(prior.clone()), pending.cloned());
    }
    // Producer debounce: hold the prior until the drop has persisted, then adopt
    // the current low reading. A drop that vanishes (a climb back to/above prior)
    // takes the branch above and clears the marker, so one stray sample can't
    // dip the bar.
    let first_seen_at = pending
        .filter(|parked| parked.source == live.source)
        .map_or(now, |parked| parked.first_seen_at);
    if now.duration_since(first_seen_at).as_secs() >= REFILL_CONFIRM_SECS {
        (Some(live.clone()), None)
    } else {
        (
            Some(prior.clone()),
            Some(PendingRefill {
                scope_id: live.scope.as_ref().map(|scope| scope.id.clone()),
                duration_mins: live.duration_mins,
                source: live.source,
                used_percentage: live_used,
                first_seen_at,
            }),
        )
    }
}

/// Whether `live`'s reset is a new window epoch beyond `prior` — strictly later
/// by more than the jitter guard. A missing reset on either side is not an
/// advance.
fn reset_advanced(prior: Option<Timestamp>, live: Option<Timestamp>) -> bool {
    match (prior, live) {
        (Some(prior), Some(live)) => live.duration_since(prior).as_secs() > RESET_ADVANCE_SECS,
        _ => false,
    }
}

/// Whether an authoritative live reading may supersede the prior truth on a
/// drop. The official API is trusted only when its capture instant is no older
/// than the prior's — an out-of-order sidecar must not lower a newer bar. An
/// unprovenanced prior (a cold cache, or a best-effort reading that predates
/// stamping) yields to the authoritative reading; an authoritative reading with
/// no stamp of its own can't prove it is current, so it holds.
fn authoritative_supersedes(live: &RateLimitWindow, prior: &RateLimitWindow) -> bool {
    match (live.observed_at, prior.observed_at) {
        (Some(live_at), Some(prior_at)) => live_at >= prior_at,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The trace file when `RIMZ_RATE_LIMIT_TRACE` is set — its value as a path, or
/// a default beside the cache for a bare/`1`/`true` toggle. `None` disables the
/// trace (the default), keeping it off the per-tick path.
fn rate_limits_trace_path(runtime: &RuntimePaths) -> Option<PathBuf> {
    let raw = std::env::var_os("RIMZ_RATE_LIMIT_TRACE")?;
    let raw = raw.to_string_lossy();
    let raw = raw.trim();
    if raw.is_empty() || raw == "1" || raw == "true" {
        Some(runtime.shared_root.join("rate_limits_trace.jsonl"))
    } else {
        Some(PathBuf::from(raw))
    }
}

/// Append one fused window to the rate-limit trace: the live candidate this
/// frame and the truth it produced. Replaying the trace across a real reset is
/// how [`REFILL_CONFIRM_SECS`] and [`LIVE_HORIZON_SECS`] are tuned. Best-effort
/// and producer-only — a trace is debug instrumentation, never a precondition,
/// so errors are swallowed.
fn trace_rate_limits(
    path: &Path,
    kind: &str,
    live: Option<&RateLimitWindow>,
    truth: &RateLimitWindow,
    now: Timestamp,
) {
    let stamp = |ts: Option<Timestamp>| ts.map(|ts| ts.to_string());
    let record = serde_json::json!({
        "ts": now.to_string(),
        "kind": kind,
        "scope_id": truth.scope.as_ref().map(|scope| scope.id.as_str()),
        "duration_mins": truth.duration_mins,
        "live": live.map(|window| serde_json::json!({
            "used_percentage": window.used_percentage,
            "resets_at": stamp(window.resets_at),
            "observed_at": stamp(window.observed_at),
            "source": if window.source.is_authoritative() { "authoritative" } else { "best_effort" },
        })),
        "truth_used_percentage": truth.used_percentage,
        "truth_resets_at": stamp(truth.resets_at),
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{record}");
    }
}
