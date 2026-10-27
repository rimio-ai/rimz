//! Sidebar-owned cadences and TTLs only.
//!
//! Runtime, tests, and docs answer "how fresh is this lane?" from named
//! constants. Bounds on other modules' behaviour live with those modules,
//! without compatibility re-exports here.

use std::time::Duration;

pub(super) const TRUNK_STATE_TTL: Duration = Duration::from_secs(5);

use jiff::SignedDuration;

/// The realtime event store's receiver-clock TTL. Events are a latency hint:
/// a missed or expired event falls back to the next producer pull, so the TTL
/// must outlive the longest supersession window — [`EVENT_PANE_TTL`] from the
/// last pane read, plus the data tick and produce latency that deliver the
/// verifying frame. The 2s grace covers that delivery: without it an overlay
/// (a focus move, say) landing just after a pull could expire up to a tick
/// before the superseding frame folds in, briefly reverting to the stale
/// baseline.
pub(super) const EVENT_STORE_TTL: Duration = Duration::from_secs(EVENT_PANE_TTL.as_secs() + 2);

/// Maximum age of a `FocusStranded` action event. It moves focus rather than
/// fusing display state, so late delivery must degrade to a no-op instead of
/// yanking the user's current pane.
pub(crate) const FOCUS_STRANDED_EVENT_TTL: Duration = Duration::from_secs(2);

/// Re-sampling cadence for a live focus fence that disagrees with a settled
/// strand. The dispatch thread retries only within the strand's original TTL.
pub(crate) const FOCUS_REPAIR_RETRY_STEP: Duration = Duration::from_millis(100);

/// The whole-layout freeze after a sidebar interaction or landing paint, re-armed
/// by each. Order, open cards, and visible cap exemptions hold while content
/// keeps updating; the layout tidies together once the user settles.
pub(crate) const LAYOUT_FREEZE: Duration = Duration::from_secs(5);

/// The freeze cap while any client's focus is on the sidebar pane.
pub(crate) const LAYOUT_FREEZE_PARKED: Duration = Duration::from_secs(30);

/// Clicks after an automatic card move still target the frame the user saw.
pub(crate) const CLICK_GUARD: Duration = Duration::from_millis(400);

/// Consumer-side dwell before naming an off-screen focused card on tmux.
pub(crate) const FOCUS_MARKER_TMUX_DWELL: Duration = Duration::from_millis(500);

/// Consumer-side dwell before naming an off-screen focused card on Zellij.
pub(crate) const FOCUS_MARKER_ZELLIJ_DWELL: Duration = Duration::from_millis(300);

/// How long a never-seen-sibling sidebar must report zero working siblings
/// before the renderer exits and lets its pane close. A tab that already had a
/// working pane exits on the first producer-verified zero; this receiver-side
/// confirmation protects only startup/resurrection and any remaining
/// single-frame pane-list flap before a sibling has been observed.
pub(crate) const SELF_CLOSE_EMPTY_CONFIRM: Duration = Duration::from_secs(5);

/// How long the user must stay in a tab before its unread siblings clear.
/// A card you focus reads instantly; a sibling you never clicked reads only
/// once you have dwelled long enough to have seen it. Leaving before this
/// elapses leaves it unread, so a pass-through jump never flickers a read
/// wash off the neighbours as a click side effect.
pub(crate) const TAB_READ_DWELL: Duration = Duration::from_millis(2500);

/// How long a completed agent keeps the transient done glyph in its mux tab
/// name. Attention and working states clear through lifecycle changes; success
/// alone decays so an old completion never reads as current indefinitely.
pub(super) const TAB_SUCCESS_STATUS_TTL: SignedDuration = SignedDuration::from_mins(5);

/// Poll-mode TTL for the host's retained pane frame, just below the default 1s data tick so unannounced pane drift surfaces within a tick. Fresh presence or an unwatched prior selects [`EVENT_PANE_TTL`]; carried panes remain capped here.
pub(super) const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// Pane-cache TTL while the presence push channel is alive (the presence stamp
/// is fresh). Typed topology events force a fresh pane frame, trusting the
/// event-carrying wake's own topology write; exact command/focus overlays live
/// in each renderer's in-memory event store until the verifying pull supersedes
/// them. The poll is only the backstop for a *lost* event — pane truth can
/// stale at most this long before it heals, while steady-state roster reads
/// drop ~10× versus [`SNAPSHOT_CACHE_TTL`]. Forced pane-frame freshness
/// (`min_pane_cache_ms`) overrides it without requiring a new plugin publication.
pub(crate) const EVENT_PANE_TTL: Duration = Duration::from_secs(10);

/// How often the producer re-samples tmux client activity while an idle-capable
/// client is attached, independent of the heavy pane cache TTL, so the AFK
/// badge clears within this bound of a keypress.
pub(super) const PRESENCE_SAMPLE_TTL: Duration = Duration::from_secs(1);

/// Maximum time a sibling may be carried while its root remains live but its
/// whole tab is omitted, or authoritative re-pulls fall back to the cache. A
/// server listing refutes missing siblings in tabs it lists immediately.
pub(super) const PANE_CARRY_TTL: Duration = Duration::from_secs(30);

/// Tolerance when comparing a pane's recorded process start against the live
/// `/proc` start. The check works at whole-second granularity, so this absorbs
/// timestamp-source rounding without letting reused pids keep stale identity.
pub(super) const PROCESS_START_MATCH_TOLERANCE: Duration = Duration::from_secs(2);

/// How long a *hot* worktree's git diff-stats stay cached before the
/// per-worktree `git` forks behind them are re-run. A working-tree edit fires
/// no store delta, so this column is never push-refreshed — it rides this TTL
/// plus the sidebar's backstop poll.
pub(super) const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// How long a focused worktree's edit-sensitive git facts stay cached:
/// working-tree churn, dirty/untracked state, and live branch label. A viewed
/// worktree gets a near-realtime edit tick while the rest of the fleet stays on
/// the hot/idle/settled tiers.
pub(super) const DIFF_STATS_FOCUSED_LOCAL_TTL: Duration = Duration::from_secs(3);

/// How long a focused worktree's commit/PR-shaped git facts stay cached:
/// ahead/behind counts and landed markers. No commit difference means no PR
/// progress to report, and the landed verdict is the heaviest fork, so the
/// focused path keeps it slower than edit-sensitive facts.
pub(super) const DIFF_STATS_FOCUSED_COMMIT_TTL: Duration = Duration::from_secs(10);

/// How long an *idle* worktree's diff stats stay cached. A worktree with no
/// running or recently-active agent decays to this slow cadence (or to
/// [`DIFF_STATS_SETTLED_TTL`] once clean and landed) — almost all of a large
/// fleet's git forks were measuring worktrees nothing had touched.
/// A human hand-editing an idle worktree sees header stats lag up to this
/// bound; accepted, the headers track fleet progress, not keystrokes.
pub(super) const DIFF_STATS_IDLE_TTL: Duration = Duration::from_secs(60);

/// How long a *settled* worktree's diff stats stay cached: unviewed, not hot,
/// with a clean tree whose committed content is proven landed. Nothing in it
/// has work to report, so it refreshes at the pace of `PR_STATE_TTL`. A HEAD
/// move in a settled checkout reaches the display, and the PR lane's HEAD
/// nudge, up to this bound late.
pub(super) const DIFF_STATS_SETTLED_TTL: Duration = Duration::from_secs(5 * 60);

/// How often a finished collapsed cohort rechecks its durable transcript effort.
pub(super) const COHORT_SPEND_TTL: Duration = Duration::from_secs(60);

/// How recently one of a worktree's agent rows must have been active for the
/// worktree to count as hot — refreshed on [`DIFF_STATS_TTL`] rather than
/// decaying to [`DIFF_STATS_IDLE_TTL`] or [`DIFF_STATS_SETTLED_TTL`]. Generous
/// against the fast TTL so a worktree stays hot across an agent's think pauses,
/// not just its tool calls.
pub(super) const GIT_ACTIVITY_WINDOW: Duration = Duration::from_secs(60);

/// How long the producer trusts the cached `git worktree list` enumeration.
/// The set changes only on `git worktree add/remove`, so a coarse TTL keeps
/// the fork off the per-tick path; a session boundary forces re-enumeration
/// through the same `min_pane_cache_ms` floor the pane cache honours, so a new
/// worktree's first agent groups correctly on its first snapshot.
pub(super) const WORKTREE_ROOTS_TTL: Duration = Duration::from_secs(60);

/// How long the producer trusts a *successful* provider-account map before it
/// re-probes. A subscription tier and login state change rarely, so a coarse
/// TTL keeps the `claude auth status` subprocess off the per-tick produce path
/// while still picking up a login or logout within a few minutes. A confident
/// logged-out answer rides this same window. An answer derived from sessions
/// (Pi's provider) changes at a session's start instead, so an adapter that
/// stamps its account inputs re-probes on a stamp change behind
/// `ACCOUNTS_RETRY_TTL`.
pub(super) const ACCOUNTS_TTL: Duration = Duration::from_secs(10 * 60);

/// The first retry delay after a failed account probe, also the floor for
/// provisional answers and changed adapter inputs. Consecutive failures double
/// the delay up to `ACCOUNTS_TTL`, unless an inputs stamp moves. A newly installed
/// binary can therefore wait the full success window after repeated failures;
/// `rimz accounts --refresh` bypasses the wait.
pub(super) const ACCOUNTS_RETRY_TTL: Duration = Duration::from_secs(10);

/// How long the producer trusts a successful idle-tier repo PR-state probe
/// before it asks the forge CLI again. Pull-request state changes on human
/// time and the probe may hit the network, so idle repos ride a long
/// producer-only TTL.
pub(super) const PR_STATE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long the producer trusts a successful hot/focused repo PR-state probe.
/// A hot sweep is affordable because it enumerates open PRs once per repo, not
/// once per worktree.
pub(super) const PR_STATE_HOT_TTL: Duration = Duration::from_secs(20);

/// Retry cadence after the PR-state probe cannot run (missing CLI, logged out,
/// non-zero command, or malformed output). Short enough to recover after login,
/// long enough to avoid a per-frame failing network fork.
pub(super) const PR_STATE_RETRY_TTL: Duration = Duration::from_secs(30);

/// Cadence for the authoritative OAuth account-usage probe, independent of the
/// realtime/app-server credits freshness.
pub(super) const OAUTH_USAGE_TTL: Duration = Duration::from_secs(5 * 60);

/// Retry cadence once an OAuth probe settled as an auth failure (missing or
/// provider-rejected credentials). The credential-file stamp fast-path retries
/// sooner when the user re-logs-in.
pub(super) const OAUTH_USAGE_SETTLED_TTL: Duration = Duration::from_secs(60 * 60);

/// Floor between OAuth account-usage probes of an idle account, one the
/// machine declares and no agent of this room runs on. It holds after every
/// outcome, so a failing or logged-out spare account is never retried on the
/// in-use tiers above; auto-redeem's expiry rescue runs off the cached expiry
/// in between.
pub(super) const IDLE_OAUTH_USAGE_TTL: Duration = Duration::from_secs(3 * 60 * 60);

/// Lease for one direct account-usage worker. Realtime publication renews it
/// before direct provider work, so each valid bounded segment fits while a dead
/// child still recovers without a long-lived orphan claim.
pub(super) const ACCOUNT_USAGE_CLAIM_TTL: Duration = Duration::from_secs(90);

/// How often the producer samples a pane attached clients are currently
/// viewing. Focus, not process activity, buys the fast `/proc` lane so the
/// pane under the user's eyes stays live.
pub(super) const METRICS_FOCUSED_SAMPLE_TTL: Duration = Duration::from_secs(1);

/// How often the producer takes a fresh two-sample `/proc` reading for a
/// non-viewed pane. Rate sampling needs a steady clock of its own — never the
/// pane-read cadence, which event-paced pane updates make a topology clock —
/// and the carried display values bound `/proc` IO to once per window
/// regardless of produce rate.
pub(super) const METRICS_BACKGROUND_SAMPLE_TTL: Duration = Duration::from_secs(3);

/// How long one process must remain in uninterruptible sleep without advancing
/// CPU or I/O counters before its pane reads as stuck. Healthy large-file
/// copies and link steps can sit in `D` across several metric samples while the
/// kernel completes one blocking syscall; the attention verdict belongs to a
/// sustained stall rather than that ordinary I/O window.
pub(super) const PROCESS_D_STATE_STUCK_AFTER: Duration = Duration::from_secs(10);

/// Minimum gap between Codex daemon ghost-reap probes. A failed daemon control
/// socket attempt can burn the full 2s deadline, so success and failure share
/// one coarse cache stamp and the fetch lane only reads it.
pub(super) const CODEX_DAEMON_REAP_TTL: Duration = Duration::from_secs(30);

/// Reader acceptance allows two missed re-probes plus slack before frozen daemon-reap evidence falls back to keeping every session.
pub(super) const CODEX_DAEMON_REAP_STALE: Duration = Duration::from_secs(90);

/// Link stats are stale after three missed two-second publishes plus slack.
/// Stale renders as dim unknown (`⇄ remote ?`) rather than red: during a hard
/// drop the remote-rendered sidebar cannot reach the user, and a second local
/// viewer of the same room should not see a false hard failure.
pub(super) const LINK_STATS_STALE: Duration = Duration::from_secs(10);

/// Link stats older than this are ignored entirely. A cleanly ended probe
/// stream removes its sidecar; this is the backstop for hard drops and killed
/// publishers so they do not leave a permanent "remote room" badge behind.
pub(super) const LINK_STATS_EXPIRE: Duration = Duration::from_secs(120);

/// How often a renderer re-stamps its heartbeat. 2s keeps two missed writes of
/// slack under [`SIDEBAR_HEARTBEAT_TTL`](crate::wakeup::heartbeat::SIDEBAR_HEARTBEAT_TTL) — the same 2.5× ratio
/// [`PRESENCE_STAMP_FRESH`](crate::mux::PRESENCE_STAMP_FRESH) keeps over the plugin keepalive — while avoiding an
/// atomic file write for every store-delta fetch in a busy fleet.
pub(crate) const HEARTBEAT_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// How long `rimz reload` waits for signaled renderers to publish a heartbeat
/// stamped with the staged build before reporting them unconverged.
pub(crate) const RELOAD_CONVERGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence while `rimz reload` waits for build-stamped heartbeats.
pub(crate) const RELOAD_CONVERGE_POLL: Duration = Duration::from_millis(150);

/// Watchdog interval for the self-close backstop: when no resize event arrives
/// (e.g. background Zellij sessions that omit SIGWINCH after a pane closes),
/// this asks the normal snapshot path for a fresh own-view count. Sized at 2s
/// so cleanup stays prompt even when a caller configured a much slower data tick.
pub(crate) const SELF_CLOSE_WATCHDOG: Duration = Duration::from_secs(2);

/// Maximum time a grow-resize paint hold may suppress frames while waiting for
/// a post-resize pane observation. Matched to `SELF_CLOSE_WATCHDOG` so the
/// close-verdict refresh and the hold expiry share one user-visible ceiling.
pub const RESIZE_PAINT_HOLD_CEILING: Duration = Duration::from_secs(2);

/// Settling window and maximum delay for an ambiguous same-command exit.
/// Immediate recovery reads stay inside the window regardless of their count;
/// the render loop wakes at this deadline and forces a non-skippable fold.
pub(crate) const ACCEPT_REGRESSION_AFTER: Duration = Duration::from_secs(1);

/// How long the gate keeps serving the last known spend tally after an
/// incoming fold carries none. A collapsed tally means the workspace spending
/// cache was unreadable for that fold — a scope-hash miss or a walk in
/// flight — and the figure returns on the producer's next publication, so the
/// carry spans two `SPENDING_TTL`
/// cycles. A spend total moves by entries ageing out of a trailing year, never
/// by dropping to nothing, so carrying costs no accuracy while it keeps the
/// cockpit's headline from blanking between publications.
pub(crate) const CARRY_COLLAPSED_SPEND_FOR: Duration = Duration::from_secs(30);

/// Consecutive refresh failures before the renderer surfaces a degraded
/// health alert. A single transient fetch hiccup must not flash a scary
/// banner: the loop already holds the last good frame, so the first failures
/// absorb silently while a sustained failure still surfaces promptly (~one
/// tick apart). The alert edges land in the diagnostics channel as
/// `health_alert` records.
pub(crate) const HEALTH_ALERT_AFTER_FAILURES: u32 = 2;

/// Observer window for a populated roster that empties and refills.
pub(super) const OBSERVE_ROSTER_FLAP_WINDOW: Duration = Duration::from_secs(10);

/// Observer window for one row disappearing and returning, or being born and
/// vanishing quickly.
pub(super) const OBSERVE_ROW_FLAP_WINDOW: Duration = Duration::from_secs(7);

/// Observer window for exact A→B→A per-row value oscillations: how long a
/// value may be away before its return stops reading as a flip.
pub(super) const OBSERVE_VALUE_OSC_WINDOW: Duration = Duration::from_secs(5);

/// Observer window for dashboard aggregate A→B→A oscillations: how long a
/// figure, or its whole key, may be away before its return stops reading as a
/// flip. Spend cache refills can lag one producer walk tick, so this is wider
/// than per-row value oscillation.
pub(super) const OBSERVE_AGGREGATE_OSC_WINDOW: Duration = Duration::from_secs(12);

/// Observer window for rendered row order A→B→A flaps inside one stable group:
/// how long the order may be away before its return stops reading as a flip.
pub(super) const OBSERVE_ORDER_FLAP_WINDOW: Duration = Duration::from_secs(7);

/// Observer window for sustained status transition churn.
pub(super) const OBSERVE_STATUS_CHURN_WINDOW: Duration = Duration::from_secs(30);

/// Startup/reload grace before windowed observer anomalies are emitted.
pub(super) const OBSERVE_WARMUP: Duration = Duration::from_secs(10);

/// Minimum gap between writer-side real-world cross-check passes.
pub(super) const OBSERVE_CROSSCHECK_TTL: Duration = Duration::from_secs(5);

/// Consecutive writer-side `/proc` observations required before a dead PID is
/// logged.
pub(super) const OBSERVE_DEADPID_CONFIRMATIONS: u32 = 2;

/// Consecutive writer-side `/proc` observations required before an agent card
/// without a matching hosted process is logged.
pub(super) const OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS: u32 = 2;

/// The breath/blink animation cadence. It stays close to the base grid so the
/// smooth breathe's truecolor lightness ramp does not visibly band, and is
/// clamped at runtime to never be faster than the configured base grid.
pub(crate) const BREATH_ANIMATION_FRAME: Duration = Duration::from_millis(120);

/// Configured base render frame.
pub(crate) fn animation_frame(refresh_ms: u16) -> Duration {
    Duration::from_millis(u64::from(refresh_ms))
}

/// The breath/blink animation frame, clamped so it never runs faster than the
/// configured base grid.
pub(crate) fn breath_animation_frame(refresh_ms: u16) -> Duration {
    BREATH_ANIMATION_FRAME.max(animation_frame(refresh_ms))
}

/// Money roll frame. The odometer clicks every `click_phases` base phases, so
/// this stays structurally coupled to the renderer's phase counter.
pub(crate) fn money_animation_frame(refresh_ms: u16, click_phases: u64) -> Duration {
    Duration::from_millis(u64::from(refresh_ms) * click_phases)
}
