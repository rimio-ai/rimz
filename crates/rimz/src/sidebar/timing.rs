//! Sidebar timing constants and cadence registry.
//!
//! This module owns every sidebar cadence and TTL so the runtime, tests, and
//! docs answer "how fresh is this lane?" from one table. Callers may still
//! re-export values through their local facades when that keeps older paths
//! stable.

use std::time::Duration;

/// The realtime event store's receiver-clock TTL. Events are a latency hint:
/// a missed or expired event falls back to the next producer pull, so the TTL
/// only has to outlive the longest pull window ([`EVENT_PANE_TTL`]) that
/// supersedes it.
pub const EVENT_STORE_TTL: Duration = Duration::from_secs(10);

/// Coalescing window for the shared snapshot cache — the **poll-mode** pane
/// TTL, in effect whenever the presence push channel is dead or absent. Just
/// under the default 1s data tick: when one ledger-delta wakeup wakes every
/// sidebar at once, the first produces the heavy snapshot and the rest read it
/// back within this window instead of each spawning their own `list-panes`.
/// Short enough that live pane/git drift (which fires no ledger delta) still
/// surfaces inside one tick. While the presence stamp is fresh the producer
/// uses [`EVENT_PANE_TTL`] instead.
pub const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// Pane-cache TTL while the presence push channel is alive (the presence stamp
/// is fresh). Typed topology events force a fresh pane list, while exact
/// command/focus overlays live in each renderer's in-memory event store until
/// the verifying pull supersedes them. The poll is only the backstop for a
/// *lost* event — pane truth can stale at most this long before it heals,
/// while steady-state `list-panes` action clients drop ~10× versus
/// [`SNAPSHOT_CACHE_TTL`]. Forced freshness (`min_pane_cache_ms`) overrides
/// it, so lifecycle/resize floors still pull a fresh pane list in event mode.
pub const EVENT_PANE_TTL: Duration = Duration::from_secs(10);

/// How young the presence stamp must be for the producer to trust the push
/// channel and use [`EVENT_PANE_TTL`]. 2.5× the plugin's 60s keepalive — two
/// missed keepalives of slack, the same ratio the sidebar heartbeat TTL keeps
/// over its write cadence. Past this the channel reads as dead and the
/// producer reverts to [`SNAPSHOT_CACHE_TTL`] poll mode, byte-identical to a
/// session with no plugin. tmux never writes the stamp, so tmux is always
/// poll mode by construction.
pub const PRESENCE_STAMP_FRESH: Duration = Duration::from_secs(150);

/// How long a *hot* worktree's git diff-stats stay cached before the
/// per-worktree `git` forks behind them are re-run. A working-tree edit fires
/// no ledger delta, so this column is never push-refreshed — it rides this TTL
/// plus the sidebar's backstop poll.
pub const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// How long an *idle* worktree's diff stats stay cached. A worktree with no
/// running or recently-active agent decays to this slow cadence — almost all
/// of a large fleet's git forks were measuring worktrees nothing had touched.
/// A human hand-editing an idle worktree sees header stats lag up to this
/// bound; accepted, the headers track fleet progress, not keystrokes.
pub const DIFF_STATS_IDLE_TTL: Duration = Duration::from_secs(60);

/// How recently one of a worktree's agent rows must have been active for the
/// worktree to count as hot — refreshed on [`DIFF_STATS_TTL`] rather than
/// decaying to [`DIFF_STATS_IDLE_TTL`]. Generous against the fast TTL so a
/// worktree stays hot across an agent's think pauses, not just its tool calls.
pub const GIT_ACTIVITY_WINDOW: Duration = Duration::from_secs(60);

/// How long the producer trusts the cached `git worktree list` enumeration.
/// The set changes only on `git worktree add/remove`, so a coarse TTL keeps
/// the fork off the per-tick path; a session boundary forces re-enumeration
/// through the same `min_pane_cache_ms` floor the pane cache honours, so a new
/// worktree's first agent groups correctly on its first snapshot.
pub const WORKTREE_ROOTS_TTL: Duration = Duration::from_secs(60);

/// How long the producer trusts a *successful* provider-account map before it
/// re-probes. A subscription tier and login state change about never, so a
/// coarse TTL keeps the `claude auth status` subprocess off the per-tick
/// produce path while still picking up a login or logout within a few minutes.
/// A confident logged-out answer rides this same window.
pub const ACCOUNTS_TTL: Duration = Duration::from_secs(10 * 60);

/// How long the producer waits before re-probing after a *failed* probe (a
/// binary that would not run, a non-zero exit, an unreadable file). Far shorter
/// than the success TTL so a transient `claude auth status` error — or a binary
/// installed just after the first probe — recovers within seconds instead of
/// pinning an empty dashboard for the full success window.
pub const ACCOUNTS_RETRY_TTL: Duration = Duration::from_secs(10);

/// How often the producer takes a fresh two-sample `/proc` reading per pane.
/// Rate sampling needs a steady clock of its own — never the pane-read cadence,
/// which event-paced pane updates make a topology clock — and the carried
/// display values bound `/proc` IO to once per window regardless of produce
/// rate. A ~3s two-sample window also smooths the rates a 1s window made
/// jumpy; a new pane's stats warm up one window later.
pub const METRICS_SAMPLE_TTL: Duration = Duration::from_secs(3);

/// How long the producer trusts a published fleet-spending walk before
/// re-walking every provider's transcript tree. Spend is display-only (the
/// eased odometer roll absorbs the step) and the walk — discovery readdirs,
/// per-file stats, the cursor-map parse, the price-book load — is the
/// producer's largest steady cost, so a coarse TTL pays for itself. One TTL,
/// no retry split like [`ACCOUNTS_RETRY_TTL`]: the walk is per-file
/// best-effort and an empty fleet prices to zero cheaply, so there is no
/// infrastructure-failure state to re-probe fast — a partial read is a
/// smaller-than-true figure that heals on the next due walk.
pub const SPENDING_TTL: Duration = Duration::from_secs(15);

/// Minimum gap between out-of-band Codex rate-limit refreshes for one target
/// (active session sidecar or idle account cache). The producer checks every
/// data tick, but budget windows move on the scale of minutes.
pub const CODEX_RATE_LIMIT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum age of a sidebar heartbeat before launch, election, and wakeup
/// fanout treat the instance as dead and skip it.
pub const SIDEBAR_HEARTBEAT_TTL: Duration = Duration::from_secs(5);

/// Watchdog interval for the self-close backstop: when no resize event arrives
/// (e.g. background Zellij sessions that omit SIGWINCH after a pane closes),
/// this asks the normal snapshot path for a fresh own-view count. Sized at 2s
/// so cleanup stays prompt even when a caller configured a much slower data tick.
pub const SELF_CLOSE_WATCHDOG: Duration = Duration::from_secs(2);

/// How long the refresh loop may stay continuously degraded before the renderer
/// gives up and exits. Generous so a transient mux hiccup or the sub-second gap
/// while `cargo install` swaps `rimz` never closes a healthy sidebar; short
/// enough that a genuinely broken renderer (deleted ledger, dead mux, or an old
/// build past the current runtime contract) heals on the next reload/attach
/// instead of lingering for minutes.
pub const GIVE_UP_AFTER_DEGRADED: Duration = Duration::from_secs(30);

/// Consecutive regression-gate holds before the escape hatch accepts the
/// regression anyway. Each reject fires one immediate self-heal refetch, and
/// the rollup is read fresh from the atomic `latest.json` each fold, so the
/// gate needs to absorb only a single slipped frame: two holds confirm a
/// *genuine* exit and demote it promptly, while a true one-frame flicker
/// recovers on the first reject's refetch and is never accepted.
pub const ACCEPT_REGRESSION_AFTER_REJECTS: u32 = 2;

/// Hard wall-clock ceiling on a regression-hold episode — the load-bearing
/// hatch, since a slow poll cadence could otherwise stretch the count out.
/// One second caps a genuine exit on the producer tab (whose reject-refetches
/// each pay a `list-panes` round-trip) while staying above a single such
/// round-trip, and well under [`GIVE_UP_AFTER_DEGRADED`].
pub const ACCEPT_REGRESSION_AFTER: Duration = Duration::from_secs(1);

/// Default render base grid: 100ms, or 10Hz.
pub const DEFAULT_REFRESH_MS: u16 = 100;

/// Minimum accepted render base grid. Prevents accidental busy-spins from
/// config typos while leaving room for faster test or local tuning.
pub const MIN_REFRESH_MS: u16 = 16;

/// Maximum accepted render base grid. Higher values make input and overlay
/// event latency visibly worse, so keep slow data polling on `--tick-seconds`.
pub const MAX_REFRESH_MS: u16 = 1_000;

/// Slow cosmetic animation cadence. It stays a human-perception constant and
/// is clamped at runtime to never be faster than the configured base grid.
pub const SLOW_ANIMATION_FRAME: Duration = Duration::from_millis(300);

/// Cap on one visible effects step. A calm room can paint rarely; clamping
/// makes a newly spawned flash play on visible frames instead of expiring.
pub const EFFECT_MAX_STEP_MS: u64 = 300;

/// Declarative pull cadence entry for docs and future diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullCadence {
    pub name: &'static str,
    pub ttl: Duration,
    pub idle_ttl: Option<Duration>,
    pub retry_ttl: Option<Duration>,
}

/// The pull-side cadence registry. It is descriptive: individual enrichment
/// lanes still own their gating logic because idle and retry semantics differ.
pub const PULL_CADENCES: &[PullCadence] = &[
    PullCadence {
        name: "panes.poll",
        ttl: SNAPSHOT_CACHE_TTL,
        idle_ttl: Some(EVENT_PANE_TTL),
        retry_ttl: None,
    },
    PullCadence {
        name: "presence.stamp",
        ttl: PRESENCE_STAMP_FRESH,
        idle_ttl: None,
        retry_ttl: None,
    },
    PullCadence {
        name: "git.diff_stats",
        ttl: DIFF_STATS_TTL,
        idle_ttl: Some(DIFF_STATS_IDLE_TTL),
        retry_ttl: None,
    },
    PullCadence {
        name: "git.activity_window",
        ttl: GIT_ACTIVITY_WINDOW,
        idle_ttl: None,
        retry_ttl: None,
    },
    PullCadence {
        name: "git.worktree_roots",
        ttl: WORKTREE_ROOTS_TTL,
        idle_ttl: None,
        retry_ttl: None,
    },
    PullCadence {
        name: "accounts",
        ttl: ACCOUNTS_TTL,
        idle_ttl: None,
        retry_ttl: Some(ACCOUNTS_RETRY_TTL),
    },
    PullCadence {
        name: "metrics.sample",
        ttl: METRICS_SAMPLE_TTL,
        idle_ttl: None,
        retry_ttl: None,
    },
    PullCadence {
        name: "spending",
        ttl: SPENDING_TTL,
        idle_ttl: None,
        retry_ttl: None,
    },
    PullCadence {
        name: "codex.rate_limit",
        ttl: CODEX_RATE_LIMIT_REFRESH_INTERVAL,
        idle_ttl: None,
        retry_ttl: None,
    },
];

/// Configured base render frame.
pub fn animation_frame(refresh_ms: u16) -> Duration {
    Duration::from_millis(u64::from(refresh_ms))
}

/// Slow cosmetic frame, clamped so it never runs faster than the configured
/// base grid.
pub fn slow_animation_frame(refresh_ms: u16) -> Duration {
    SLOW_ANIMATION_FRAME.max(animation_frame(refresh_ms))
}

/// Money roll frame. The odometer clicks every `click_phases` base phases, so
/// this stays structurally coupled to the renderer's phase counter.
pub fn money_animation_frame(refresh_ms: u16, click_phases: u64) -> Duration {
    Duration::from_millis(u64::from(refresh_ms) * click_phases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_cadence_registry_points_at_the_named_constants() {
        let accounts = PULL_CADENCES
            .iter()
            .find(|cadence| cadence.name == "accounts")
            .expect("accounts cadence is registered");
        assert_eq!(accounts.ttl, ACCOUNTS_TTL);
        assert_eq!(accounts.retry_ttl, Some(ACCOUNTS_RETRY_TTL));

        let panes = PULL_CADENCES
            .iter()
            .find(|cadence| cadence.name == "panes.poll")
            .expect("pane cadence is registered");
        assert_eq!(panes.ttl, SNAPSHOT_CACHE_TTL);
        assert_eq!(panes.idle_ttl, Some(EVENT_PANE_TTL));
    }
}
