//! Sidebar timing constants.
//!
//! This module owns every sidebar cadence and TTL so the runtime, tests, and
//! docs answer "how fresh is this lane?" from named constants. Callers may still
//! re-export values through their local facades when that keeps older paths
//! stable.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::SignedDuration;

/// The realtime event store's receiver-clock TTL. Events are a latency hint:
/// a missed or expired event falls back to the next producer pull, so the TTL
/// must outlive the longest supersession window — [`EVENT_PANE_TTL`] from the
/// last pane read, plus the data tick and produce latency that deliver the
/// verifying frame. The 2s grace covers that delivery: without it an overlay
/// (a focus move, say) landing just after a pull could expire up to a tick
/// before the superseding frame folds in, briefly reverting to the stale
/// baseline.
pub const EVENT_STORE_TTL: Duration = Duration::from_secs(EVENT_PANE_TTL.as_secs() + 2);

/// Maximum age of a `FocusStranded` action event. It moves focus rather than
/// fusing display state, so late delivery must degrade to a no-op instead of
/// yanking the user's current pane.
pub const FOCUS_STRANDED_EVENT_TTL: Duration = Duration::from_secs(2);

/// Minimum gap between repaints of an off-screen attached sidebar. Hidden
/// panes refresh only when their glanceable roster/status/unread projection
/// changes, keeping the buffer near-current without running animations.
pub const BACKGROUND_PAINT_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum extra staleness an off-screen consumer renderer accepts before
/// folding identity-free store/pane nudges. Watched renderers and the elected
/// producer stay immediate.
pub const UNWATCHED_FOLD_CLAMP: Duration = Duration::from_secs(1);

/// How long the interactive sidebar keeps its last row/group order after a
/// jump, tab-switch, browse, or the focused agent's ask being answered before
/// re-sorting to live rank. Long enough to read the card you landed on, not just
/// glance at it, so a watched row holds still while you take it in; each
/// interaction re-arms it, so a rapid triage burst holds one stable list and it
/// tidies once you settle.
pub const REORDER_HOLD: Duration = Duration::from_secs(5);

/// How long a never-seen-sibling sidebar must report zero working siblings
/// before the renderer exits and lets its pane close. A tab that already had a
/// working pane exits on the first producer-verified zero; this receiver-side
/// confirmation protects only startup/resurrection and any remaining
/// single-frame pane-list flap before a sibling has been observed.
pub const SELF_CLOSE_EMPTY_CONFIRM: Duration = Duration::from_secs(5);

/// How long a jump scroll anchor stays applicable. Long enough for the
/// destination tab to refold and adopt the focus after the jump's nudge. This
/// also bounds how long an unconfirmed jump intent outranks observed focus; an
/// older anchor is a stale jump and is ignored.
pub const FOCUS_ANCHOR_FRESH: Duration = Duration::from_millis(2500);

/// How long the user must stay in a tab before its unread siblings clear.
/// A card you focus reads instantly; a sibling you never clicked reads only
/// once you have dwelled long enough to have seen it. Leaving before this
/// elapses leaves it unread, so a pass-through jump never flickers a read
/// wash off the neighbours as a click side effect.
pub const TAB_READ_DWELL: Duration = Duration::from_millis(2500);

/// How long a completed agent keeps the transient done glyph in its mux tab
/// name. Attention and working states clear through lifecycle changes; success
/// alone decays so an old completion never reads as current indefinitely.
pub const TAB_SUCCESS_STATUS_TTL: SignedDuration = SignedDuration::from_mins(5);

/// Coalescing window for the shared snapshot cache — the **poll-mode** pane
/// TTL, in effect whenever the presence push channel is dead or absent. Just
/// under the default 1s data tick: when one store-delta wakeup wakes every
/// sidebar at once, the first produces the heavy snapshot and the rest read it
/// back within this window instead of each running their own mux roster read.
/// Short enough that live pane/git drift (which fires no store delta) still
/// surfaces inside one tick. While the presence stamp is fresh the producer
/// uses [`EVENT_PANE_TTL`] instead.
pub const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// Pane-cache TTL while the presence push channel is alive (the presence stamp
/// is fresh). Typed topology events force a fresh pane frame, trusting the
/// event-carrying wake's own topology write; exact command/focus overlays live
/// in each renderer's in-memory event store until the verifying pull supersedes
/// them. The poll is only the backstop for a *lost* event — pane truth can
/// stale at most this long before it heals, while steady-state roster reads
/// drop ~10× versus [`SNAPSHOT_CACHE_TTL`]. Forced pane-frame freshness
/// (`min_pane_cache_ms`) overrides it, while topology freshness floors stay
/// reserved for explicit structural repair.
pub const EVENT_PANE_TTL: Duration = Duration::from_secs(10);

/// How often the producer re-samples tmux client activity while an idle-capable
/// client is attached, independent of the heavy pane cache TTL, so the AFK
/// badge clears within this bound of a keypress.
pub const PRESENCE_SAMPLE_TTL: Duration = Duration::from_secs(1);

/// Maximum time a pane omitted by the mux source may be carried from the last
/// good frame while `/proc` still proves the old pane root alive. Long enough
/// to cover several bad pane pulls, short enough that a persistently lying mux
/// source cannot freeze the room indefinitely.
pub const PANE_CARRY_TTL: Duration = Duration::from_secs(30);

/// Runtime pane-carry TTL. Tests may set `RIMZ_TEST_PANE_CARRY_MS` to shorten
/// the liveness carry window around a spawned sidebar process.
pub(crate) fn pane_carry_ttl() -> Duration {
    let Some(value) = std::env::var_os("RIMZ_TEST_PANE_CARRY_MS").filter(|value| !value.is_empty())
    else {
        return PANE_CARRY_TTL;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(PANE_CARRY_TTL)
}

/// Tolerance when comparing a pane's recorded process start against the live
/// `/proc` start. The check works at whole-second granularity, so this absorbs
/// timestamp-source rounding without letting reused pids keep stale identity.
pub const PROCESS_START_MATCH_TOLERANCE: Duration = Duration::from_secs(2);

/// How young the presence stamp must be for the producer to trust the push
/// channel and use [`EVENT_PANE_TTL`]. 2.5× the plugin's 60s keepalive — two
/// missed keepalives of slack, the same ratio the sidebar heartbeat TTL keeps
/// over its write cadence. Past this the channel reads as dead and the
/// producer reverts to [`SNAPSHOT_CACHE_TTL`] poll mode, byte-identical to a
/// session with no push channel. tmux's control-mode watch writes the stamp
/// while attached and lapses back to poll mode after true silence.
pub const PRESENCE_STAMP_FRESH: Duration = Duration::from_secs(150);

/// How long a *hot* worktree's git diff-stats stay cached before the
/// per-worktree `git` forks behind them are re-run. A working-tree edit fires
/// no store delta, so this column is never push-refreshed — it rides this TTL
/// plus the sidebar's backstop poll.
pub const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// How long a focused worktree's edit-sensitive git facts stay cached:
/// working-tree churn, dirty/untracked state, and live branch label. A viewed
/// worktree gets a near-realtime edit tick while the rest of the fleet stays on
/// the hot/idle tiers.
pub const DIFF_STATS_FOCUSED_LOCAL_TTL: Duration = Duration::from_secs(3);

/// How long a focused worktree's commit/PR-shaped git facts stay cached:
/// ahead/behind counts and landed markers. No commit difference means no PR
/// progress to report, and the landed verdict is the heaviest fork, so the
/// focused path keeps it slower than edit-sensitive facts.
pub const DIFF_STATS_FOCUSED_COMMIT_TTL: Duration = Duration::from_secs(10);

/// How long an *idle* worktree's diff stats stay cached. A worktree with no
/// running or recently-active agent decays to this slow cadence — almost all
/// of a large fleet's git forks were measuring worktrees nothing had touched.
/// A human hand-editing an idle worktree sees header stats lag up to this
/// bound; accepted, the headers track fleet progress, not keystrokes.
pub const DIFF_STATS_IDLE_TTL: Duration = Duration::from_secs(60);

/// How often a finished collapsed cohort rechecks its durable transcript effort.
pub const COHORT_SPEND_TTL: Duration = Duration::from_secs(60);

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

/// How long the producer trusts a successful idle-tier repo PR-state probe
/// before it asks the forge CLI again. Pull-request state changes on human
/// time and the probe may hit the network, so idle repos ride a long
/// producer-only TTL.
pub const PR_STATE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long the producer trusts a successful hot/focused repo PR-state probe.
/// A hot sweep is affordable because it enumerates open PRs once per repo, not
/// once per worktree.
pub const PR_STATE_HOT_TTL: Duration = Duration::from_secs(20);

/// Retry cadence after the PR-state probe cannot run (missing CLI, logged out,
/// non-zero command, or malformed output). Short enough to recover after login,
/// long enough to avoid a per-frame failing network fork.
pub const PR_STATE_RETRY_TTL: Duration = Duration::from_secs(30);

/// How long an extra-credits/account-usage reading stays displayable. Paid
/// usage is a coarse monthly signal; after a day without a refresh, the
/// dashboard drops the provider-supplied figures rather than showing stale
/// balance as current.
pub const CREDITS_DISPLAY_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Cadence for the authoritative OAuth account-usage probe, independent of the
/// realtime/app-server credits freshness.
pub const OAUTH_USAGE_TTL: Duration = Duration::from_secs(5 * 60);

/// Retry cadence once an OAuth probe settled as an auth failure (missing or
/// provider-rejected credentials). The credential-file stamp fast-path retries
/// sooner when the user re-logs-in.
pub const OAUTH_USAGE_SETTLED_TTL: Duration = Duration::from_secs(60 * 60);

/// Lease for one direct account-usage worker. Realtime publication renews it
/// before direct provider work, so each valid bounded segment fits while a dead
/// child still recovers without a long-lived orphan claim.
pub const ACCOUNT_USAGE_CLAIM_TTL: Duration = Duration::from_secs(90);

/// How often the producer samples a pane attached clients are currently
/// viewing. Focus, not process activity, buys the fast `/proc` lane so the
/// pane under the user's eyes stays live.
pub const METRICS_FOCUSED_SAMPLE_TTL: Duration = Duration::from_secs(1);

/// How often the producer takes a fresh two-sample `/proc` reading for a
/// non-viewed pane. Rate sampling needs a steady clock of its own — never the
/// pane-read cadence, which event-paced pane updates make a topology clock —
/// and the carried display values bound `/proc` IO to once per window
/// regardless of produce rate.
pub const METRICS_BACKGROUND_SAMPLE_TTL: Duration = Duration::from_secs(3);

/// How long one process must remain in uninterruptible sleep without advancing
/// CPU or I/O counters before its pane reads as stuck. Healthy large-file
/// copies and link steps can sit in `D` across several metric samples while the
/// kernel completes one blocking syscall; the attention verdict belongs to a
/// sustained stall rather than that ordinary I/O window.
pub const PROCESS_D_STATE_STUCK_AFTER: Duration = Duration::from_secs(10);

/// Maximum extra staleness a hidden consumer accepts for metrics-only pane
/// publications. The consumer folds on the same cadence that produces the
/// underlying background samples.
pub const UNWATCHED_METRICS_FOLD_CLAMP: Duration = METRICS_BACKGROUND_SAMPLE_TTL;

/// Minimum gap between out-of-band session context refreshes for one target.
/// The producer checks every data tick, but budget windows move on the scale of
/// minutes.
pub const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Reap grace for the per-session context-refresh throttle stamp. A live
/// session re-touches its stamp within [`SESSION_REFRESH_INTERVAL`] plus the
/// producer fold cadence, so a stamp older than this is dead.
pub const SESSION_PROBE_MARKER_TTL: Duration = Duration::from_secs(5 * 60);

/// Runtime `shared/` filename prefix for per-session context-refresh throttle
/// stamps.
pub const SESSION_PROBE_MARKER_PREFIX: &str = "session-context-probe.";

/// Minimum gap between Codex daemon ghost-reap probes. A failed daemon control
/// socket attempt can burn the full 2s deadline, so success and failure share
/// one coarse cache stamp and the fetch lane only reads it.
pub const CODEX_DAEMON_REAP_TTL: Duration = Duration::from_secs(30);

/// Link stats are stale after three missed two-second publishes plus slack.
/// Stale renders as dim unknown (`⇄ remote ?`) rather than red: during a hard
/// drop the remote-rendered sidebar cannot reach the user, and a second local
/// viewer of the same room should not see a false hard failure.
pub const LINK_STATS_STALE: Duration = Duration::from_secs(10);

/// Link stats older than this are ignored entirely. A cleanly ended probe
/// stream removes its sidecar; this is the backstop for hard drops and killed
/// publishers so they do not leave a permanent "remote room" badge behind.
pub const LINK_STATS_EXPIRE: Duration = Duration::from_secs(120);

/// Maximum age of a sidebar heartbeat before launch, election, and wakeup
/// fanout treat the instance as dead and skip it.
pub const SIDEBAR_HEARTBEAT_TTL: Duration = Duration::from_secs(5);

/// How often a renderer re-stamps its heartbeat. 2s keeps two missed writes of
/// slack under [`SIDEBAR_HEARTBEAT_TTL`] — the same 2.5× ratio
/// [`PRESENCE_STAMP_FRESH`] keeps over the plugin keepalive — while avoiding an
/// atomic file write for every store-delta fetch in a busy fleet.
pub const HEARTBEAT_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// How long `rimz reload` waits for signaled renderers to publish a heartbeat
/// stamped with the staged build before reporting them unconverged.
pub const RELOAD_CONVERGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-call bound for reload's best-effort convergence pane/layout reads.
pub const RECONCILE_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence while `rimz reload` waits for build-stamped heartbeats.
pub const RELOAD_CONVERGE_POLL: Duration = Duration::from_millis(150);

/// Watchdog interval for the self-close backstop: when no resize event arrives
/// (e.g. background Zellij sessions that omit SIGWINCH after a pane closes),
/// this asks the normal snapshot path for a fresh own-view count. Sized at 2s
/// so cleanup stays prompt even when a caller configured a much slower data tick.
pub const SELF_CLOSE_WATCHDOG: Duration = Duration::from_secs(2);

/// Maximum time a grow-resize paint hold may suppress frames while waiting for
/// a post-resize pane observation. Matched to [`SELF_CLOSE_WATCHDOG`] so the
/// close-verdict refresh and the hold expiry share one user-visible ceiling.
pub const RESIZE_PAINT_HOLD_CEILING: Duration = Duration::from_secs(2);

/// How long the refresh loop may stay continuously degraded before the renderer
/// gives up and asks its supervisor to respawn it in place. Generous so a
/// transient mux hiccup or the sub-second gap while `cargo install` swaps
/// `rimz` does not churn the worker; short enough that a genuinely broken
/// renderer (deleted store, dead mux, or an old build past the current runtime
/// contract) enters bounded retry instead of lingering on stale data.
pub const GIVE_UP_AFTER_DEGRADED: Duration = Duration::from_secs(30);

/// Settling window and maximum delay for an ambiguous same-command exit.
/// Immediate recovery reads stay inside the window regardless of their count;
/// the render loop wakes at this deadline and forces a non-skippable fold.
pub const ACCEPT_REGRESSION_AFTER: Duration = Duration::from_secs(1);

/// How long the gate keeps serving the last known spend tally after an
/// incoming fold carries none. A collapsed tally means the workspace spending
/// cache was unreadable for that fold — a scope-hash miss or a walk in
/// flight — and the figure returns on the producer's next publication, so the
/// carry spans two [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL)
/// cycles. A spend total moves by entries ageing out of a trailing year, never
/// by dropping to nothing, so carrying costs no accuracy while it keeps the
/// cockpit's headline from blanking between publications.
pub const CARRY_COLLAPSED_SPEND_FOR: Duration = Duration::from_secs(30);

/// Consecutive refresh failures before the renderer surfaces a degraded
/// health alert. A single transient fetch hiccup must not flash a scary
/// banner: the loop already holds the last good frame, so the first failures
/// absorb silently while a sustained failure still surfaces promptly (~one
/// tick apart). The alert edges land in the diagnostics channel as
/// `health_alert` records.
pub const HEALTH_ALERT_AFTER_FAILURES: u32 = 2;

/// Observer window for a populated roster that empties and refills.
pub const OBSERVE_ROSTER_FLAP_WINDOW: Duration = Duration::from_secs(10);

/// Observer window for one row disappearing and returning, or being born and
/// vanishing quickly.
pub const OBSERVE_ROW_FLAP_WINDOW: Duration = Duration::from_secs(7);

/// Observer window for exact A→B→A value oscillations.
pub const OBSERVE_VALUE_OSC_WINDOW: Duration = Duration::from_secs(5);

/// Observer window for dashboard aggregate A→B→A oscillations. Spend cache
/// refills can lag one producer walk tick, so this is wider than per-row
/// value oscillation.
pub const OBSERVE_AGGREGATE_OSC_WINDOW: Duration = Duration::from_secs(12);

/// Observer window for rendered row order A→B→A flaps inside one stable group.
pub const OBSERVE_ORDER_FLAP_WINDOW: Duration = Duration::from_secs(7);

/// Observer window for sustained status transition churn.
pub const OBSERVE_STATUS_CHURN_WINDOW: Duration = Duration::from_secs(30);

/// Startup/reload grace before windowed observer anomalies are emitted.
pub const OBSERVE_WARMUP: Duration = Duration::from_secs(10);

/// Minimum gap between writer-side real-world cross-check passes.
pub const OBSERVE_CROSSCHECK_TTL: Duration = Duration::from_secs(5);

/// Consecutive writer-side `/proc` observations required before a dead PID is
/// logged.
pub const OBSERVE_DEADPID_CONFIRMATIONS: u32 = 2;

/// Consecutive writer-side `/proc` observations required before an agent card
/// without a matching hosted process is logged.
pub(super) const OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS: u32 = 2;

/// Default render base grid: 100ms, or 10Hz.
pub const DEFAULT_REFRESH_MS: u16 = 100;

/// Minimum accepted render base grid. Prevents accidental busy-spins from
/// config typos while leaving room for faster test or local tuning.
pub const MIN_REFRESH_MS: u16 = 16;

/// Maximum accepted render base grid. Higher values make input and overlay
/// event latency visibly worse, so keep slow data polling on `--tick-seconds`.
pub const MAX_REFRESH_MS: u16 = 1_000;

/// The breath/blink animation cadence. It stays close to the base grid so the
/// smooth breathe's truecolor lightness ramp does not visibly band, and is
/// clamped at runtime to never be faster than the configured base grid.
pub const BREATH_ANIMATION_FRAME: Duration = Duration::from_millis(120);

/// Configured base render frame.
pub fn animation_frame(refresh_ms: u16) -> Duration {
    Duration::from_millis(u64::from(refresh_ms))
}

/// The breath/blink animation frame, clamped so it never runs faster than the
/// configured base grid.
pub fn breath_animation_frame(refresh_ms: u16) -> Duration {
    BREATH_ANIMATION_FRAME.max(animation_frame(refresh_ms))
}

/// Money roll frame. The odometer clicks every `click_phases` base phases, so
/// this stays structurally coupled to the renderer's phase counter.
pub fn money_animation_frame(refresh_ms: u16, click_phases: u64) -> Duration {
    Duration::from_millis(u64::from(refresh_ms) * click_phases)
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
