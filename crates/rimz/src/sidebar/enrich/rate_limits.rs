use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::agents::{AgentRateLimits, RateLimitWindow};
use crate::sidebar::cache::unix_now_ms;
use crate::{RuntimePaths, SidebarSnapshot};

/// How long a best-effort drop — a candidate mid-window free reset with no
/// authoritative reading and no reset-timer change to corroborate it — must
/// persist before the bar follows it down. Shorter, a single lagging or garbled
/// sample could dip the bar; longer is needless lag on a real refill. Tuned
/// against captured reset traces (see [`trace_rate_limits`]).
pub(crate) const REFILL_CONFIRM_SECS: i64 = 120;
/// Coarse backstop: a live candidate captured longer ago than this is ignored.
/// Content-staleness is already caught upstream — the snapshot view drops a
/// reading whose shortest window has reset — so this only guards a wildly old
/// reading slipping through.
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
    /// Last-known windows by agent kind. Holds *ground truth* — the fused most
    /// recent reading, carrying its `observed_at`/`source` — never the
    /// synthesized full window, which is a read-time projection recomputed each
    /// frame. A logged-out kind is absent.
    pub windows: BTreeMap<String, AgentRateLimits>,
    /// In-flight best-effort refill candidates by kind, one per window duration.
    /// A statusline drop that no authoritative reading or reset-timer change
    /// corroborates is parked here until it persists [`REFILL_CONFIRM_SECS`],
    /// then it becomes truth. Empty in steady state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending: BTreeMap<String, Vec<PendingRefill>>,
}

/// A best-effort drop awaiting confirmation: the bar holds its higher prior
/// value until this candidate has stood for [`REFILL_CONFIRM_SECS`], so one
/// lagging or garbled low sample can't dip a live budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRefill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
    pub used_percentage: u8,
    pub first_seen_at: Timestamp,
}

/// Read the producer's published rate-limit window cache, or an empty cache on a
/// cold or corrupt file. Read-only and fork-free — every tab's idle fallback.
pub(crate) fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Whether `kind`'s shortest account-scoped budget window is currently running
/// its clock. Window-priming callers (autoping) read this to skip a ping when
/// the window has already started — the token is then spent only to *start* a
/// window, never on one already counting down.
///
/// Reads the shared cache and projects the shortest window to `now` exactly as
/// the dashboard does, so a window that refilled while idle reads as not-yet-
/// started. The result is a deliberate tri-state: `Some(true)` means the clock
/// is running (skip the ping), `Some(false)` means it has not started (prime
/// it), and `None` means the state is unknown — no cached reading for the kind,
/// an unknown bar, or no countdown to trust — so a priming caller acts rather
/// than skips. Best-effort by nature: a cold cache (no sidebar ran recently)
/// simply reads unknown and the ping proceeds.
pub fn shortest_window_running(runtime: &RuntimePaths, kind: &str, now: Timestamp) -> Option<bool> {
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    shortest_window_running_in(&cache, kind, now)
}

fn shortest_window_running_in(cache: &RateLimitsCache, kind: &str, now: Timestamp) -> Option<bool> {
    let shortest = cache
        .windows
        .get(kind)?
        .windows
        .iter()
        .min_by_key(|window| window.duration_mins.unwrap_or(u32::MAX))?;
    let projected = project_idle_window(shortest.clone(), now);
    // An unknown reading (no percentage) tells us nothing — leave it to the caller.
    projected.used_percentage?;
    if projected.not_started(now) {
        return Some(false);
    }
    // The clock has begun (or is unjudgeable as not-started). Call it running only
    // with a real future reset to count down to; otherwise it is indeterminate.
    match projected.resets_at {
        Some(reset) if reset > now => Some(true),
        _ => None,
    }
}

/// Publish the rate-limit window cache for every tab to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
pub(crate) fn write_rate_limits_cache(path: &Path, cache: &RateLimitsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
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
/// Best-effort and racy by contract: the producer rewrites this file each frame
/// from the panels (live-reading-or-prior), so a write here can be clobbered by a
/// concurrent producer frame. It converges within a frame or two because the
/// producer carries the prior reading forward, and the out-of-band fetch is
/// throttled — so a lost write is simply retried. Used by the detached
/// `rimz agents refresh-usage` helper, never on the per-tick path.
pub fn merge_account_rate_limits(runtime: &RuntimePaths, kind: &str, windows: AgentRateLimits) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) = try_rate_limits_cache_lock(&runtime.shared_rate_limits_lock()) else {
        return;
    };
    let mut cache = read_rate_limits_cache(&path);
    cache.refreshed_at_ms = unix_now_ms();
    // Authoritative fetch: stamp the capture instant so the fusion ranks it as
    // truth, and clear any in-flight best-effort refill for this kind — the
    // official reading settles the question the debounce was waiting on.
    let windows = windows.stamped_at(Timestamp::now());
    cache.windows.insert(kind.to_owned(), windows);
    cache.pending.remove(kind);
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
            observed_at: cached.observed_at,
            source: cached.source,
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
        observed_at: cached.observed_at,
        source: cached.source,
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
        let Some(_guard) = try_rate_limits_cache_lock(&runtime.shared_rate_limits_lock()) else {
            let cached = read_rate_limits_cache(&path);
            apply_rate_limit_cache_with(snapshot, &cached, false, None);
            return;
        };
        let cached = read_rate_limits_cache(&path);
        let trace = rate_limits_trace_path(runtime);
        if let Some(next) = apply_rate_limit_cache_with(snapshot, &cached, true, trace.as_deref()) {
            write_rate_limits_cache(&path, &next);
        }
        return;
    }

    let cached = read_rate_limits_cache(&path);
    apply_rate_limit_cache_with(snapshot, &cached, false, None);
}

/// Clear the published cache once every provider has logged out, so a later
/// re-login paints from live readings rather than stale budgets. A no-op when
/// the cache is already empty or the RMW lock is held — a contending producer's
/// frame reaps it instead. Producer-only.
fn reset_logged_out_rate_limits_cache(runtime: &RuntimePaths) {
    let path = runtime.shared_rate_limits_path();
    let Some(_guard) = try_rate_limits_cache_lock(&runtime.shared_rate_limits_lock()) else {
        return;
    };
    let cached = read_rate_limits_cache(&path);
    if cached.windows.is_empty() && cached.pending.is_empty() {
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
    trace: Option<&Path>,
) -> Option<RateLimitsCache> {
    // The snapshot's single projection clock, so the idle-window reset
    // projection agrees with the dashboard windows resolved on the same frame.
    let now = snapshot.now;
    let mut next = RateLimitsCache {
        refreshed_at_ms: unix_now_ms(),
        windows: BTreeMap::new(),
        pending: BTreeMap::new(),
    };

    for panel in &mut snapshot.providers {
        // Index this kind's live (this-frame) and cached (last-known) readings by
        // window duration, so each duration is fused independently.
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
        let prev_pending: BTreeMap<Option<u32>, PendingRefill> = cached
            .pending
            .get(&panel.kind)
            .into_iter()
            .flatten()
            .map(|refill| (refill.duration_mins, refill.clone()))
            .collect();
        let durations: BTreeSet<Option<u32>> = live.keys().chain(prev.keys()).copied().collect();

        // Fuse each duration to its ground truth and carry or advance its
        // debounce marker. A live reading drives the fusion; absent one, the
        // prior truth is carried unchanged for the idle projection below.
        let mut truth: BTreeMap<Option<u32>, RateLimitWindow> = BTreeMap::new();
        let mut pending: Vec<PendingRefill> = Vec::new();
        for duration in &durations {
            let (window, refill) = fuse_window(
                prev.get(duration),
                live.get(duration),
                prev_pending.get(duration),
                now,
                persist,
            );
            if let (Some(window), Some(path)) = (window.as_ref(), trace) {
                trace_rate_limits(path, &panel.kind, live.get(duration), window, now);
            }
            if let Some(window) = window {
                truth.insert(*duration, window);
            }
            if let Some(refill) = refill {
                pending.push(refill);
            }
        }
        let cache_unknown = live.is_empty() && longest_cached_window_expired(&truth, now);

        // Persist ground truth only: the fused readings and any in-flight refill.
        // The synthesized full or unknown windows below are never written — they
        // are recomputed each frame.
        if persist {
            if !truth.is_empty() {
                next.windows.insert(
                    panel.kind.clone(),
                    AgentRateLimits {
                        windows: truth.values().cloned().collect(),
                    },
                );
            }
            if !pending.is_empty() {
                next.pending.insert(panel.kind.clone(), pending);
            }
        }

        // Display: the fused truth as-is where a live reading drove it; otherwise
        // the carried truth, projected (reset-to-max) or shown unknown once the
        // longest window has aged out. Sorted short→long for a stable paint order.
        let mut display: Vec<RateLimitWindow> = truth
            .into_iter()
            .map(|(duration, window)| {
                if live.contains_key(&duration) {
                    window
                } else if cache_unknown {
                    unknown_idle_window(window)
                } else {
                    project_idle_window(window, now)
                }
            })
            .collect();
        display.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
        panel.windows = display;
    }

    persist.then_some(next)
}

/// Fuse one window duration's prior truth with this frame's live reading into
/// the new ground truth, carrying or advancing the debounce marker that guards a
/// best-effort refill.
///
/// Usage only climbs within a live window, so a reading at or above the prior is
/// real consumption and is adopted at once — stable against parallel sessions
/// reporting the same budget at different instants. A *drop* is a refill, earned
/// rather than assumed, in order:
/// - an authoritative-source reading lowers the bar immediately, but only when
///   its capture is no older than the prior's (an out-of-order sidecar can't
///   undo a newer reading);
/// - a later reset instant (a new window epoch) lowers it immediately;
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
    // Coarse backstop: ignore a wildly old live reading (content-staleness is
    // already filtered upstream by the snapshot view's reading-level check).
    if let Some(observed_at) = live.observed_at
        && now.duration_since(observed_at).as_secs() > LIVE_HORIZON_SECS
    {
        return (prior.cloned(), pending.cloned());
    }
    let Some(prior) = prior else {
        // First reading for this duration: adopt it, nothing pending.
        return (Some(live.clone()), None);
    };
    let prior_used = prior.used_percentage.unwrap_or(0);
    let live_used = live.used_percentage.unwrap_or(0);

    // Climb or steady: adopt at once and drop any parked refill.
    if live_used >= prior_used {
        return (Some(live.clone()), None);
    }

    // --- a drop is a refill, earned not assumed ---

    // The official API is truth, but only when its capture is at least as recent
    // as the prior: an out-of-order sidecar with an older `observed_at` must not
    // lower a newer bar. A stale authoritative reading holds the prior and never
    // seeds the best-effort debounce below.
    if live.source.is_authoritative() {
        return if authoritative_supersedes(live, prior) {
            (Some(live.clone()), None)
        } else {
            (Some(prior.clone()), pending.cloned())
        };
    }

    // A best-effort reading whose reset instant advanced is a free reset with a
    // moved timer — a new window epoch, trusted at once.
    if reset_advanced(prior.resets_at, live.resets_at) {
        return (Some(live.clone()), None);
    }

    // A best-effort drop is a refill candidate only when it lands at or below the
    // reset floor (near-full). A mid-range drop is jitter — hold the most-drained
    // prior, carrying any in-flight marker untouched.
    if live_used > REFILL_FLOOR_PCT {
        return (Some(prior.clone()), pending.cloned());
    }

    // Best-effort refill candidate, no authoritative or epoch corroboration.
    if !allow_confirm {
        // A consumer holds the producer's persisted (higher) truth.
        return (Some(prior.clone()), pending.cloned());
    }
    // Producer debounce: hold the prior until the drop has persisted, then adopt
    // the current low reading. A drop that vanishes (a climb back to/above prior)
    // takes the branch above and clears the marker, so one stray sample can't
    // dip the bar.
    let first_seen_at = pending.map_or(now, |parked| parked.first_seen_at);
    if now.duration_since(first_seen_at).as_secs() >= REFILL_CONFIRM_SECS {
        (Some(live.clone()), None)
    } else {
        (
            Some(prior.clone()),
            Some(PendingRefill {
                duration_mins: live.duration_mins,
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
