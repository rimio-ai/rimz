//! Sidebar snapshot caches and the in-process consumer read.
//!
//! The producer (the elected eldest renderer, via `rimz sidebar snapshot`)
//! publishes two runtime caches: the snapshot base (`snapshot.json`: the ledger
//! rollup plus the live pane list) and the per-worktree git facts
//! (`diff-stats.json`). Every other per-tab renderer is a *consumer* — it reads
//! those caches and folds its own pane exclusion in process, never forking a
//! `list-panes`/git of its own.
//!
//! [`read_published_snapshot`] is that consumer read: it lives in the library so
//! the native renderer calls it directly (no subprocess per tick) and the
//! `rimz sidebar snapshot --no-produce` CLI path shares one implementation. The
//! producer's write side (single-flight election, the git forks) stays in
//! `cli::sidebar`, which constructs these same cache types.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::spending::{Spending, read_provider_spending_cache};
use crate::agents::{AgentRateLimits, RateLimitWindow};
use crate::feed::PaneRef;
use crate::ids::PaneId;
use crate::{
    RuntimePaths, SidebarOwnView, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
    StatePaths,
};

/// Coalescing window for the shared snapshot cache. Well under the default 2s
/// data tick: when one ledger-delta wakeup wakes every sidebar at once, the
/// first produces the heavy snapshot and the rest read it back within this
/// window instead of each spawning their own `list-panes`. Short enough that
/// live pane/git drift (which fires no ledger delta) still surfaces inside one
/// tick — the same staleness budget the diff-stats cache already accepts.
pub const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// How long a worktree's git diff-stats stay cached before the per-worktree
/// `git` forks behind them are re-run. A working-tree edit fires no ledger
/// delta, so this column is never push-refreshed — it rides this TTL plus the
/// sidebar's backstop poll.
pub const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// How long the producer trusts a *successful* provider-account map before it
/// re-probes. A subscription tier and login state change about never, so a
/// coarse TTL keeps the `claude auth status` subprocess off the per-tick produce
/// path while still picking up a login or logout within a few minutes. A
/// confident logged-out answer rides this same window.
pub const ACCOUNTS_TTL: Duration = Duration::from_secs(10 * 60);

/// How long the producer waits before re-probing after a *failed* probe (a
/// binary that would not run, a non-zero exit, an unreadable file). Far shorter
/// than the success TTL so a transient `claude auth status` error — or a binary
/// installed just after the first probe — recovers within seconds instead of
/// pinning an empty dashboard for the full success window.
pub const ACCOUNTS_RETRY_TTL: Duration = Duration::from_secs(10);

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish before forking its own probe.
/// Matched to the diff-stats single-flight, leaning long enough to ride the
/// elder's `claude auth status` fork rather than racing it.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

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
    fn is_fresh(&self, now_ms: u64) -> bool {
        let ttl = if self.ok {
            ACCOUNTS_TTL
        } else {
            ACCOUNTS_RETRY_TTL
        };
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }
}

/// How long a consumer keeps trusting the elected producer's published frame
/// before it stops holding it and produces locally instead. A healthy producer
/// rewrites `snapshot.json` every backstop tick (~2s), so several ticks of slack
/// never trips on a live fleet; but a producer that wedged, died without a
/// taker, or was split off by a protocol bump stops advancing the file, and a
/// consumer must not freeze on that last frame forever. Past this age the
/// consumer self-heals by taking the producer path (single-flighted, so the
/// fleet still elects one local producer rather than a fork storm).
pub const PUBLISHED_FRAME_STALE_AFTER: Duration = Duration::from_secs(8);

/// Shared, single-flight pane-list cache, keyed to one `(workspace, session)` —
/// the per-workspace runtime root scopes the workspace; `session_name` guards
/// against serving one session's panes (which the Zellij backend stamps from the
/// requested session, not the true owner) to a sidebar pinned to another during
/// a detach or session-rotation handoff.
///
/// It caches only the expensive `list-panes` round-trip. The ledger *rollup* is
/// deliberately **not** stored here: it is cheap and per-event fresh in
/// `latest.json`, so producer and consumer both read it fresh each fetch
/// ([`consumer_rollup`] / `Ledger::snapshot_cached`) and fold these coalesced
/// panes over it. Fusing the two would pin a status change to the slow pane
/// cadence — the lag this split removes. Per-sidebar exclusion and own-view are
/// applied by the reader, so the panes are pre-fold.
#[derive(Clone, Serialize, Deserialize)]
pub struct SnapshotCache {
    pub produced_at_ms: u64,
    pub session_name: String,
    pub panes: Vec<PaneRef>,
}

/// One thread's last parse of a snapshot cache file, keyed by path + identity
/// (mtime, len). The consumer fetch worker is a long-lived thread that calls
/// [`read_snapshot_cache`] every fetch (~0.75–2s), but the producer only
/// republishes `snapshot.json` when something changed — so most reads hit an
/// unchanged file. Caching the parse lets a hit return a clone of the in-memory
/// value (a bulk copy) instead of re-deserializing 100–500 KB of JSON. Thread-
/// local so it needs no lock and cannot be shared across the process's threads.
struct ParsedSnapshotCache {
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    cache: SnapshotCache,
}

thread_local! {
    static SNAPSHOT_PARSE_CACHE: std::cell::RefCell<Option<ParsedSnapshotCache>> =
        const { std::cell::RefCell::new(None) };
}

/// Read a same-session cache entry regardless of coalescing freshness. `None`
/// when it is absent, for another session, or unreadable. Used as the
/// hold-last-good base for a consumer read and the degraded-read fallback.
///
/// Skips the JSON parse when this thread already parsed a byte-identical file
/// (same path, mtime, and length). On a stat miss it re-reads and re-caches; a
/// file replaced (atomic rename) between the stat and the read just costs one
/// redundant parse next call, never a stale or torn value.
pub fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<SnapshotCache> {
    let meta = std::fs::metadata(cache_path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();

    let cached = SNAPSHOT_PARSE_CACHE.with_borrow(|slot| {
        slot.as_ref().and_then(|entry| {
            (entry.path == cache_path && entry.mtime == mtime && entry.len == len)
                .then(|| entry.cache.clone())
        })
    });

    let cache = match cached {
        Some(cache) => cache,
        None => {
            let bytes = std::fs::read(cache_path).ok()?;
            let parsed: SnapshotCache = serde_json::from_slice(&bytes).ok()?;
            SNAPSHOT_PARSE_CACHE.with_borrow_mut(|slot| {
                *slot = Some(ParsedSnapshotCache {
                    path: cache_path.to_path_buf(),
                    mtime,
                    len,
                    cache: parsed.clone(),
                });
            });
            parsed
        }
    };
    (cache.session_name == session).then_some(cache)
}

/// The event-fresh ledger rollup for a consumer, read in process: `latest.json`
/// when it reflects the log (lock-free, O(snapshot)), else a full re-projection
/// (the cold/empty room, or a write that raced the freshness check). The
/// read-only twin of the producer's `Ledger::snapshot_cached`, exposed so a
/// consumer tab folds the freshest rollup over the producer's coalesced panes
/// without holding a writer handle — the rollup is what makes a status change or
/// a new agent in an existing pane repaint within one wakeup, independent of the
/// slower pane-list cadence. `None` only when the ledger itself is unreadable,
/// which the caller treats as a soft miss and holds the last good frame.
pub fn consumer_rollup(state: &StatePaths) -> Option<SidebarSnapshot> {
    crate::ledger::snapshot::read_fresh_latest(state)
        .or_else(|| crate::ledger::snapshot::build_from(state).ok())
}

/// Whether the producer's published same-session frame has gone stale — older
/// than [`PUBLISHED_FRAME_STALE_AFTER`] at `now_ms`. A consumer that sees this
/// stops holding the frame and produces locally, so a stalled producer can never
/// freeze a tab indefinitely. `false` when no same-session frame exists yet
/// (cold start, or a session-handoff mismatch): that path is the producer
/// election's job, not this self-heal's, and the renderer's degraded-exit safety
/// still covers a consumer that can never read a frame at all.
pub fn published_frame_is_stale(runtime: &RuntimePaths, session: &str, now_ms: u64) -> bool {
    let cache_path = runtime.root.join("snapshot.json");
    read_snapshot_cache(&cache_path, session)
        .is_some_and(|cache| frame_age_exceeds(cache.produced_at_ms, now_ms))
}

/// Whether `produced_at_ms` is older than [`PUBLISHED_FRAME_STALE_AFTER`] at
/// `now_ms`. Saturating, so a clock that ran backwards reads as fresh (age 0)
/// rather than wrapping to a huge age and forcing a needless local produce.
fn frame_age_exceeds(produced_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(produced_at_ms) > PUBLISHED_FRAME_STALE_AFTER.as_millis() as u64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCache {
    pub entries: BTreeMap<String, DiffStatsCacheEntry>,
    /// The repo's worktree checkout roots, cached under the same TTL as the
    /// per-worktree diff stats. The set changes only on `git worktree
    /// add/remove`, so grouping reuses it across ticks instead of forking
    /// `git worktree list` every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<WorktreeRootsCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRootsCache {
    pub refreshed_at_ms: u64,
    pub roots: Vec<PathBuf>,
}

impl WorktreeRootsCache {
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
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
    /// Live branch resolved from the worktree path, cached under the same TTL
    /// as the diff stats so the group header tracks `git checkout` without a
    /// git call every tick.
    #[serde(default)]
    pub branch: Option<String>,
}

impl DiffStatsCacheEntry {
    pub fn new(
        refreshed_at_ms: u64,
        stats: Option<DiffStats>,
        commits: Option<u32>,
        branch: Option<String>,
    ) -> Self {
        Self {
            refreshed_at_ms,
            added: stats.map(|stats| stats.added),
            removed: stats.map(|stats| stats.removed),
            commits,
            branch,
        }
    }

    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
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

/// The repo's worktree checkout roots the producer last published, read-only
/// (no `git worktree list` fork). A consumer reuses whatever the elder cached,
/// even stale; an empty set leaves the reducer's project-root prefix test to
/// stand alone.
pub fn cached_worktree_roots(runtime: &RuntimePaths) -> Vec<PathBuf> {
    read_diff_stats_cache(&runtime.root.join("diff-stats.json"))
        .worktrees
        .map(|cache| cache.roots)
        .unwrap_or_default()
}

/// The worktree path a group's rows share, if any. The group key may carry a
/// branch suffix (a path that holds more than one branch), so the bare path is
/// recovered from the rows — every row in a group shares it.
fn worktree_group_path(group: &SidebarWorktreeGroup) -> Option<&str> {
    group
        .rows
        .iter()
        .find_map(|row| row.worktree_path.as_deref())
        .filter(|path| !path.is_empty())
}

/// The live worktree paths this snapshot needs git facts for: a `Worktree`-kind
/// group whose recovered path is a live directory, de-duplicated so two
/// branch-split groups for one dir share a single git read. The producer feeds
/// this to its git refresh; projection ([`project_diff_stats`]) re-derives the
/// same live-dir set so a stale entry for a now-missing worktree never resurfaces.
pub fn needed_worktree_paths(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut needed: Vec<String> = Vec::new();
    for group in &snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group) else {
            continue;
        };
        if Path::new(path).is_dir() && !needed.iter().any(|known| known == path) {
            needed.push(path.to_owned());
        }
    }
    needed
}

/// Project the cached git facts onto each worktree group: the diff stats shown
/// on the header and the live branch label. Both are properties of the worktree
/// *path*, not of any one agent, so they belong to the group — which also
/// settles the shared-worktree "whose branch?" ambiguity. Only live-dir paths
/// carry stats, so a stale entry for a now-missing worktree never resurfaces.
/// Pure projection (no git): the producer refreshes the cache first, a consumer
/// projects whatever the elder last published.
pub fn project_diff_stats(snapshot: &mut SidebarSnapshot, cache: &DiffStatsCache) {
    for group in &mut snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group).map(ToOwned::to_owned) else {
            continue;
        };
        if !Path::new(&path).is_dir() {
            continue;
        }
        let Some(entry) = cache.entries.get(&path).cloned() else {
            continue;
        };
        if let Some(stats) = entry.stats() {
            group.diff_added = Some(stats.added);
            group.diff_removed = Some(stats.removed);
        }
        if let Some(commits) = entry.commits {
            group.commits_ahead = Some(commits);
        }
        if let Some(branch) = entry.branch.filter(|branch| !branch.is_empty()) {
            group.label = branch;
        }
    }
}

/// Whether any supported agent has its hooks installed. Environment, not ledger,
/// so the reducer can't know it — the renderer's first-run hint points at
/// `rimz hooks install` until a supported agent is wired.
pub fn agent_hooks_ready() -> bool {
    crate::agents::KNOWN_AGENTS.iter().any(|name| {
        crate::agents::integration_by_name(name)
            .map(|agent| agent.supports_hook_install() && agent.hooks_installed())
            .unwrap_or(false)
    })
}

/// The lazy-registering agent kinds whose Rimz hooks are installed — the gate for
/// the idle-instance synthesis on a wired-but-unbound agent pane. Filtered to lazy
/// agents (not `agent_hooks_ready`'s any-agent check), so a Claude-only install
/// never promotes an unwired Codex pane to an idle agent (it would otherwise read
/// as a forever-idle agent Rimz can report no status for). Environment, not ledger.
pub fn wired_lazy_kinds() -> Vec<String> {
    crate::agents::KNOWN_AGENTS
        .iter()
        .filter(|name| {
            crate::agents::integration_by_name(name)
                .map(|agent| {
                    agent.registers_session_lazily()
                        && agent.supports_hook_install()
                        && agent.hooks_installed()
                })
                .unwrap_or(false)
        })
        .map(|name| (*name).to_owned())
        .collect()
}

/// Render the published snapshot for a consumer renderer, entirely from runtime
/// caches and sidecars — no `list-panes`, no git. Reads the producer's coalesced
/// pane list from `snapshot.json`, pairs it with the **event-fresh** rollup read
/// in process from `latest.json` ([`consumer_rollup`]), folds the session's
/// statusline context and per-tool activity, overlays the panes with this
/// renderer's own-pane exclusion, and projects the cached diff stats. `None`
/// until the producer has published a pane set (or if the ledger is unreadable),
/// so the caller holds its last good frame.
///
/// Pairing fresh rollup + coalesced panes is the lag fix: a `ledger_delta` folds
/// the new agent/status in this tab within one wakeup, while the slower
/// `list-panes` cadence only governs genuine pane open/close.
///
/// This is the in-process twin of the producer's `rimz sidebar snapshot`: the
/// native renderer calls it directly each tick instead of forking, and the
/// `--no-produce` CLI path (the plugin rail's read) shares it.
pub fn read_published_snapshot(
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> Option<SidebarSnapshot> {
    let cache_path = runtime.root.join("snapshot.json");
    let cache = read_snapshot_cache(&cache_path, session)?;
    let base = consumer_rollup(state)?;
    Some(enrich_consumer(base, Some(cache.panes), runtime, exclude))
}

/// Fold the read-only enrichments onto a consumer's base snapshot: the cached
/// worktree roots, each session's statusline context and per-tool activity, the
/// live-pane overlay with this renderer's own-pane exclusion, and the cached
/// diff-stats projection. Every input is a runtime cache or sidecar read — no
/// `list-panes`, no git, no ledger lock. `panes` is `None` only on a cold start
/// (no base published yet), where the bare rollup's groups stand until the
/// producer's first publish, mirroring the producer's own pane-fold guard.
pub fn enrich_consumer(
    mut snapshot: SidebarSnapshot,
    panes: Option<Vec<PaneRef>>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
) -> SidebarSnapshot {
    if snapshot.project_root.is_some() {
        snapshot = snapshot.with_worktree_roots(cached_worktree_roots(runtime));
    }
    if !snapshot.agents.is_empty() {
        snapshot = snapshot.with_agent_context(crate::ledger::agent_context::read_all(runtime));
        let activity = crate::agent_activity::read_all(runtime);
        snapshot = snapshot.with_agent_activity(&activity);
    }
    // Wiring state gates the live-pane fold (the idle-instance synthesis), so set
    // it before folding panes, not after.
    snapshot.wired_lazy_kinds = wired_lazy_kinds();
    if let Some(panes) = panes {
        if let Some(own) = exclude {
            snapshot.own_view = SidebarOwnView::from_panes(own, &panes);
        }
        // Recompute from the published pane list (pre-exclusion) rather than
        // trusting the producer's base bit, for producer/consumer symmetry.
        snapshot.only_daemon_view_remains = SidebarSnapshot::only_daemon_view(&panes);
        snapshot = snapshot.with_live_panes(panes, exclude);
    }
    snapshot.agent_hooks_ready = agent_hooks_ready();
    // Per-machine display preferences (row density) and the per-provider
    // dashboard are environment, not ledger, so the producer's published rollup
    // carries neither. Fold them here so a consumer tab honours the user's
    // preference and paints the same provider panel — a cheap config read plus
    // the producer's published account and spending caches, never a per-tick
    // fork or a ledger lock. The account probe and JSONL walk stay on the producer.
    let spending;
    (snapshot, spending) = fold_machine_config_cached(snapshot, runtime);
    if !spending.total.is_zero() {
        snapshot.value_tally = Some(spending.total);
    }

    let cache = read_diff_stats_cache(&runtime.root.join("diff-stats.json"));
    project_diff_stats(&mut snapshot, &cache);
    snapshot
}

/// Fold the per-machine config and the per-provider dashboard onto a *producer*
/// snapshot: row density, the `⇅ rc` flags, and the account-scoped budget blocks.
/// The account facts come from a live out-of-band probe (`claude auth status`, a
/// `codex` auth-file read) — a subprocess — so this is the producer's job. The
/// probed map is published to the shared `accounts.json` cache for consumers to
/// read, mirroring the diff-stats single-flight: one fork on the elder, a cache
/// read on every other tab. Best-effort: a config read failure falls back to
/// defaults; the probe is memoized so it stays off the hot path.
pub fn fold_machine_config_producing(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
) -> SidebarSnapshot {
    let accounts = produce_accounts(&snapshot, runtime);
    let mut snapshot = fold_machine_config_with(
        snapshot,
        crate::config::MachineConfig::load().unwrap_or_default(),
        accounts,
        provider_spending,
    );
    // The producer owns the account-scoped window cache: it writes live readings
    // back so the budgets survive a session ending or going idle.
    apply_rate_limit_cache(&mut snapshot, runtime, true);
    // Codex's budget windows live behind the app-server. The producer refreshes
    // active session sidecars and the idle account cache on a coarse cadence so a
    // long-running task does not wait for the next turn boundary to repaint.
    refresh_codex_rate_limits(&snapshot, runtime);
    snapshot
}

/// Kick detached, best-effort Codex budget refreshes. A live/root Codex session
/// refreshes its `AgentContext` sidecar because provider aggregation prefers live
/// session readings over the shared cache. A logged-in metered Codex account with
/// no root session refreshes the account cache instead, so idle dashboards stay
/// current. Both paths are producer-only and throttled per target.
fn refresh_codex_rate_limits(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in codex_rate_limit_refreshes(snapshot) {
        if !codex_rate_limit_probe_due(runtime, &refresh) {
            continue;
        }
        match refresh {
            CodexRateLimitRefresh::Session {
                session_id,
                model_hint,
            } => spawn_codex_context_refresh(runtime, &session_id, model_hint.as_deref()),
            CodexRateLimitRefresh::Account => spawn_codex_account_window_fetch(runtime),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexRateLimitRefresh {
    Session {
        session_id: String,
        model_hint: Option<String>,
    },
    Account,
}

fn codex_rate_limit_refreshes(snapshot: &SidebarSnapshot) -> Vec<CodexRateLimitRefresh> {
    let sessions = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind == "codex" && agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .map(|agent| CodexRateLimitRefresh::Session {
            session_id: agent.agent_id.clone(),
            model_hint: agent
                .model
                .clone()
                .or_else(|| agent.context.as_ref().and_then(|ctx| ctx.model_id.clone())),
        })
        .collect::<Vec<_>>();
    if !sessions.is_empty() {
        return sessions;
    }

    snapshot
        .providers
        .iter()
        .filter(|panel| provider_has_out_of_band_windows(&panel.kind) && panel.metered)
        .map(|_| CodexRateLimitRefresh::Account)
        .collect()
}

/// Whether a provider kind exposes an account-scoped, sessionless rate-limit read
/// the producer can fetch out-of-band. Codex serves it from its app-server;
/// Claude has none (its windows ride a live statusline), so it never qualifies.
fn provider_has_out_of_band_windows(kind: &str) -> bool {
    kind == "codex"
}

/// Throttle one Codex rate-limit refresh target via a marker file under the
/// runtime root: skip when the last attempt is younger than the interval, touch
/// it before spawning. Windows move on the scale of minutes, so a one-minute
/// gate keeps a slow/unreachable app-server from spawning a helper every frame
/// while still updating during long-running turns.
fn codex_rate_limit_probe_due(runtime: &RuntimePaths, refresh: &CodexRateLimitRefresh) -> bool {
    let path = codex_rate_limit_probe_marker(runtime, refresh);
    let due = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= CODEX_RATE_LIMIT_REFRESH_INTERVAL);
    if due {
        // Touch first so a fetch that never publishes still backs off this target.
        let _ = std::fs::write(&path, b"");
    }
    due
}

fn codex_rate_limit_probe_marker(
    runtime: &RuntimePaths,
    refresh: &CodexRateLimitRefresh,
) -> PathBuf {
    match refresh {
        CodexRateLimitRefresh::Account => runtime.root.join("rate-limit-probe.codex"),
        CodexRateLimitRefresh::Session { session_id, .. } => {
            let mut hasher = Sha256::new();
            hasher.update(b"codex-session");
            hasher.update([0]);
            hasher.update(session_id.as_bytes());
            let digest = hex::encode(hasher.finalize());
            runtime
                .root
                .join(format!("rate-limit-probe.codex.{}", &digest[..32]))
        }
    }
}

/// Spawn the detached, fresh-stdio helper that refreshes one active Codex
/// session's `AgentContext` sidecar. Best-effort: a spawn failure is logged and
/// dropped — the dashboard keeps the prior reading until the next due frame.
fn spawn_codex_context_refresh(runtime: &RuntimePaths, session_id: &str, model_hint: Option<&str>) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(error = %err, "sidebar: cannot locate rimz to refresh codex context");
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "codex",
        "refresh-context",
        "--session-id",
        session_id,
        "--workspace-id",
        runtime.workspace_id.as_str(),
    ]);
    if let Some(model) = model_hint {
        cmd.args(["--model", model]);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Err(err) = cmd.spawn() {
        tracing::warn!(error = %err, "sidebar: failed to spawn codex context refresh");
    }
}

/// Spawn the detached, fresh-stdio helper that fetches Codex's account windows
/// and merges them into the shared cache. Best-effort: a spawn failure is logged
/// and dropped — the dashboard keeps the prior reading until the next due frame.
fn spawn_codex_account_window_fetch(runtime: &RuntimePaths) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(error = %err, "sidebar: cannot locate rimz to refresh codex windows");
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "codex",
        "refresh-rate-limits",
        "--workspace-id",
        runtime.workspace_id.as_str(),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    if let Err(err) = cmd.spawn() {
        tracing::warn!(error = %err, "sidebar: failed to spawn codex rate-limit refresh");
    }
}

/// Resolve the provider-account map for the producer, single-flighted behind
/// `accounts.lock` so a cold-start fleet — or several `ProduceLocal` losers when
/// the elder wedges — forks `claude auth status` once per refresh, not once per
/// tab. Fast path: a fresh `accounts.json` (under the success or failure TTL)
/// rides through with no lock and no fork. Slow path: elect one prober; losers
/// poll briefly for its publish, then fall back to an uncached local probe
/// rather than block on a wedged elder.
fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, crate::agents::AgentAccount> {
    let path = runtime.root.join("accounts.json");

    // Fast path: a young publish needs no lock and no fork.
    if read_accounts_cache(&path).is_fresh(unix_now_ms()) {
        return read_accounts_cache(&path).accounts;
    }

    // Slow path: elect one prober for this workspace's refresh window. The
    // freshness closure also serves coalesce's post-win re-check, so a peer that
    // published between our miss and the lock is honoured rather than re-forked.
    let lock_path = runtime.root.join("accounts.lock");
    let fresh = || {
        let cache = read_accounts_cache(&path);
        cache.is_fresh(unix_now_ms()).then_some(cache.accounts)
    };
    match crate::ledger::single_flight::coalesce(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        // A peer published a fresh map between our miss and the lock, or as we polled.
        crate::ledger::single_flight::Coalesced::Shared(accounts) => accounts,
        // We won: probe once and publish for every consumer and loser to read back.
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            let (accounts, ok) = probe_accounts(snapshot);
            write_accounts_cache(
                &path,
                &AccountsCache {
                    refreshed_at_ms: unix_now_ms(),
                    accounts: accounts.clone(),
                    ok,
                },
            );
            accounts
        }
        // The elder wedged: probe locally for our own frame, but do not publish —
        // its result will be fresher, and a failed local probe must not pin the cache.
        crate::ledger::single_flight::Coalesced::ProduceLocal => probe_accounts(snapshot).0,
    }
}

/// Fold the per-machine config and dashboard onto a *consumer* snapshot, reading
/// the producer's published `accounts.json` instead of probing. A consumer forks
/// zero subprocesses (the single-flight contract); a cold cache (no producer
/// publish yet) carries no blocks until the elder's first publish. The cheap
/// config read stays local so each tab honours its own row density.
pub fn fold_machine_config_cached(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
) -> (SidebarSnapshot, Spending) {
    let accounts = read_accounts_cache(&runtime.root.join("accounts.json")).accounts;
    // Consumers read the producer's published spending cache rather than
    // re-walking the JSONL transcript history themselves.
    let spending = read_provider_spending_cache(&runtime.root.join("provider-spending.json"));
    let mut snapshot = fold_machine_config_with(
        snapshot,
        crate::config::MachineConfig::load().unwrap_or_default(),
        accounts,
        &spending.by_provider,
    );
    // A consumer reads the producer's published windows to fill idle gaps, but
    // never writes — the single-flight contract keeps the cache the producer's.
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    (snapshot, spending)
}

/// Apply the resolved config and already-resolved accounts onto the snapshot:
/// row density, the per-provider `⇅ rc` flags, and the dashboard aggregates.
fn fold_machine_config_with(
    mut snapshot: SidebarSnapshot,
    config: crate::config::MachineConfig,
    accounts: BTreeMap<String, crate::agents::AgentAccount>,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
) -> SidebarSnapshot {
    let crate::config::MachineConfig {
        remote_control,
        sidebar,
        ..
    } = config;
    snapshot.sidebar = sidebar;

    // The `⇅ rc` flag per provider comes from the remote-control toggles.
    let mut remote_control_flags: BTreeMap<String, bool> = BTreeMap::new();
    remote_control_flags.insert("claude".to_owned(), remote_control.claude);
    remote_control_flags.insert("codex".to_owned(), remote_control.codex);

    snapshot.with_provider_aggregates(&accounts, &remote_control_flags, provider_spending)
}

/// Probe out-of-band login/account facts for every known provider plus any
/// active kind: a logged-in but idle provider still earns a dashboard block, so
/// the panel shows your accounts and budgets even between turns. A logged-out
/// provider is omitted and never appears. Returns the map alongside whether the
/// probe completed cleanly: a single `Unavailable` outcome (a binary that would
/// not run, an unreadable file) makes the whole refresh a failure so the
/// producer retries it on the short TTL. Producer-only — the probe is a
/// subprocess; consumers read the published result.
fn probe_accounts(
    snapshot: &SidebarSnapshot,
) -> (BTreeMap<String, crate::agents::AgentAccount>, bool) {
    let mut kinds: Vec<String> = crate::agents::KNOWN_AGENTS
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect();
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_none() && !kinds.iter().any(|known| known == &agent.kind) {
            kinds.push(agent.kind.clone());
        }
    }
    let mut accounts: BTreeMap<String, crate::agents::AgentAccount> = BTreeMap::new();
    let mut ok = true;
    for kind in kinds {
        match crate::agents::account::probe(&kind) {
            crate::agents::account::AccountProbe::Found(account) => {
                accounts.insert(kind, account);
            }
            crate::agents::account::AccountProbe::LoggedOut => {}
            crate::agents::account::AccountProbe::Unavailable => ok = false,
        }
    }
    (accounts, ok)
}

/// Read the producer's published account cache, or an empty cache on a cold or
/// corrupt file. Read-only and fork-free — the consumer's view of the dashboard.
fn read_accounts_cache(path: &Path) -> AccountsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the probed account cache for consumer tabs to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
fn write_accounts_cache(path: &Path, cache: &AccountsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(path = %path.display(), error = %err, "sidebar accounts cache write failed");
    }
}

/// Minimum gap between out-of-band Codex rate-limit refreshes for one target
/// (active session sidecar or idle account cache). The producer checks every
/// sidebar data tick, but budget windows move on the scale of minutes.
const CODEX_RATE_LIMIT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The producer's published per-provider rate-limit windows, account-scoped so
/// the budgets outlive a session ending or going idle: the first frame
/// after inactivity paints the last-known bars rather than an empty dashboard.
/// Single-flighted like the other runtime caches — the producer writes, every
/// tab reads — and reaped with the workspace runtime dir by `ledger::gc`.
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
fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the rate-limit window cache for every tab to read, atomically so a
/// reader never observes a half-written file. Best-effort: a write failure logs
/// and leaves the prior cache in place.
fn write_rate_limits_cache(path: &Path, cache: &RateLimitsCache) {
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
    let path = runtime.root.join("rate_limits.json");
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
fn project_idle_window(cached: RateLimitWindow, now: Timestamp) -> RateLimitWindow {
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

/// Fold the persisted account-scoped windows onto the resolved provider panels:
/// a kind with no live reading this frame paints its last-known bars (projected
/// through [`project_idle_window`]'s reset-to-max rule) instead of an empty
/// dashboard. Reconciled per window duration, so each budget is carried forward
/// independently. On the producer (`persist`) the live readings are written
/// back — and only the live ground truth, never the synthesized full window —
/// so budgets survive a session ending or going idle. The written cache tracks
/// login: it is rebuilt from the panels alone, so a logged-out kind (no panel)
/// drops out. A consumer reads the same cache but never writes it.
fn apply_rate_limit_cache(snapshot: &mut SidebarSnapshot, runtime: &RuntimePaths, persist: bool) {
    // No dashboard, no windows: skip the cache I/O entirely. A room with no
    // logged-in provider has nothing to fall back to and nothing to persist, so
    // this stays off the per-tick path there — the same idle-room gate the
    // context/activity reads use. A logged-out provider is reaped on the next
    // frame that still has a panel (it rebuilds the cache from the panels alone).
    if snapshot.providers.is_empty() {
        return;
    }

    let path = runtime.root.join("rate_limits.json");
    let cached = read_rate_limits_cache(&path);
    let now = Timestamp::now();
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
                    prev.get(duration)
                        .cloned()
                        .map(|window| project_idle_window(window, now))
                })
            })
            .collect();
        display.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
        panel.windows = display;
    }

    if persist {
        write_rate_limits_cache(&path, &next);
    }
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
    use crate::feed::{AgentState, AgentStatus, PermissionPosture};
    use crate::ids::{MuxName, WorkspaceId};
    use crate::ledger::atomic;

    fn pane(id: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, id),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: None,
            view_name: None,
            is_focused: false,
            client_focused: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    fn pane_in_tab(id: &str, view_id: &str) -> PaneRef {
        PaneRef {
            view_id: Some(view_id.to_owned()),
            ..pane(id, "zsh", "/tmp")
        }
    }

    #[test]
    fn frame_age_exceeds_is_saturating() {
        let max = PUBLISHED_FRAME_STALE_AFTER.as_millis() as u64;
        assert!(!frame_age_exceeds(100, 100), "age 0 is fresh");
        assert!(
            !frame_age_exceeds(100, 100 + max),
            "exactly at the window is fresh"
        );
        assert!(
            frame_age_exceeds(100, 100 + max + 1),
            "one ms past is stale"
        );
        // A clock that ran backwards saturates to age 0 rather than wrapping huge.
        assert!(!frame_age_exceeds(100, 50));
    }

    #[test]
    fn published_frame_is_stale_only_past_the_window_and_for_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        let produced_at_ms = 1_700_000_000_000;
        let cache = SnapshotCache {
            produced_at_ms,
            session_name: "rimz-test".to_owned(),
            panes: Vec::new(),
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &cache).unwrap();
        let max = PUBLISHED_FRAME_STALE_AFTER.as_millis() as u64;

        // Fresh within the window, stale just past it.
        assert!(!published_frame_is_stale(
            &runtime,
            "rimz-test",
            produced_at_ms + max
        ));
        assert!(published_frame_is_stale(
            &runtime,
            "rimz-test",
            produced_at_ms + max + 1
        ));
        // A frame stamped for another session never matches: the producer
        // election owns the handoff, so this never forces a spurious local
        // produce on a session mismatch.
        assert!(!published_frame_is_stale(
            &runtime,
            "other-session",
            produced_at_ms + max + 1
        ));

        // No published frame at all → not "stale" (cold start is the election's
        // job; the renderer's degraded-exit safety covers a never-readable frame).
        let empty = tempfile::tempdir().unwrap();
        let empty_rt =
            RuntimePaths::under(WorkspaceId::from_project_root(empty.path()), empty.path())
                .unwrap();
        empty_rt.ensure_dirs().unwrap();
        assert!(!published_frame_is_stale(
            &empty_rt,
            "rimz-test",
            produced_at_ms + max + 1
        ));
    }

    #[test]
    fn read_published_snapshot_folds_caches_without_forking() {
        // A real on-disk worktree so the live-dir projection fires.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let wt = worktree.to_string_lossy().into_owned();

        // Publish the rollup (project root = the worktree) to `latest.json`, where
        // the consumer reads it fresh, and the live panes to `snapshot.json`. `own`
        // is excluded; a sibling pane becomes a row.
        let mut rollup = SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new());
        rollup = rollup.with_project_root(Some(worktree.clone()));
        let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        state.ensure_dirs().unwrap();
        atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
        let panes = vec![
            pane("terminal_0", "zsh", &wt),
            pane("terminal_own", "rimz-sidebar", &wt),
        ];
        let base = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-test".to_owned(),
            panes,
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();

        // Publish diff stats for the worktree path: +7 / -2, 3 commits ahead, on
        // branch `feat`.
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            wt.clone(),
            DiffStatsCacheEntry::new(
                unix_now_ms(),
                Some(DiffStats {
                    added: 7,
                    removed: 2,
                }),
                Some(3),
                Some("feat".to_owned()),
            ),
        );
        atomic::write_temp_then_rename_cache(&runtime.root.join("diff-stats.json"), &diff).unwrap();

        let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
        let snapshot = read_published_snapshot(&state, &runtime, "rimz-test", Some(&own))
            .expect("published base");

        // The worktree group carries the cached +7/-2 and the live branch label,
        // projected from the cache with no git fork.
        let group = snapshot
            .worktree_groups
            .iter()
            .find(|group| group.kind == SidebarWorktreeKind::Worktree)
            .expect("a worktree group");
        assert_eq!(group.diff_added, Some(7));
        assert_eq!(group.diff_removed, Some(2));
        assert_eq!(group.label, "feat");
        // The own (sidebar) pane is excluded; the sibling renders as a row.
        assert!(
            snapshot
                .worktree_groups
                .iter()
                .flat_map(|group| &group.rows)
                .all(|row| {
                    row.pane
                        .as_ref()
                        .is_none_or(|pane| pane.pane_id.as_str() != own.as_str())
                }),
            "the renderer's own pane is never a row"
        );
    }

    #[test]
    fn consumer_own_view_counts_siblings_in_its_own_tab() {
        // A consumer reads the producer's session-wide pane list (`list-panes
        // -a`) and folds its own-view from it. An orphan sidebar — alone in its
        // tab — must see `Some(0)` siblings so self-close can fire, even though
        // the producer lives in another tab with its own siblings.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        let main_sb = pane_in_tab("main_sb", "@0");
        let main_term = pane_in_tab("main_term", "@0");
        let orphan_sb = pane_in_tab("orphan_sb", "@1");
        let base = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-test".to_owned(),
            panes: vec![main_sb, main_term, orphan_sb],
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();
        // The rollup the consumer folds the panes over: an empty room, published
        // to `latest.json` where the consumer reads it fresh.
        let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        state.ensure_dirs().unwrap();
        let rollup = SidebarSnapshot::build(workspace, Vec::new(), Vec::new());
        atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

        let orphan_own = PaneId::from_parts(MuxName::Zellij, "orphan_sb");
        let snapshot = read_published_snapshot(&state, &runtime, "rimz-test", Some(&orphan_own))
            .expect("base");
        assert_eq!(
            snapshot.own_view.map(|view| view.sibling_count),
            Some(0),
            "an orphan sidebar sees zero siblings in its own tab so self-close can fire"
        );
    }

    #[test]
    fn read_published_snapshot_is_none_until_the_producer_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        // No published pane set yet (the producer hasn't run), so the consumer
        // read is `None` regardless of the rollup — the caller holds last-good.
        let state = StatePaths::under(workspace, dir.path()).unwrap();
        assert!(read_published_snapshot(&state, &runtime, "rimz-test", None).is_none());
    }

    #[test]
    fn consumer_reflects_a_fresh_rollup_over_a_stale_pane_cache() {
        // The event-fresh split: the consumer reads the rollup from `latest.json`
        // each call, so a status change shows even when the producer's published
        // pane cache has not moved. Republishing `latest.json` alone changes the
        // rendered rollup.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        state.ensure_dirs().unwrap();

        // A published (and never re-published) pane cache.
        let panes = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-test".to_owned(),
            panes: Vec::new(),
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &panes).unwrap();

        let mut alpha = SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new());
        alpha.display_name = "alpha".to_owned();
        atomic::write_temp_then_rename(&state.latest_snapshot, &alpha).unwrap();
        let first = read_published_snapshot(&state, &runtime, "rimz-test", None).expect("base");
        assert_eq!(first.display_name, "alpha");

        // Republish ONLY `latest.json` (a different length so the parse cache
        // cannot mask the change); the pane cache is untouched.
        let mut bravo = SidebarSnapshot::build(workspace, Vec::new(), Vec::new());
        bravo.display_name = "bravo-the-second-rollup".to_owned();
        atomic::write_temp_then_rename(&state.latest_snapshot, &bravo).unwrap();
        let second = read_published_snapshot(&state, &runtime, "rimz-test", None).expect("base");
        assert_eq!(
            second.display_name, "bravo-the-second-rollup",
            "the consumer folds the fresh rollup, not a cached one"
        );
    }

    #[test]
    fn read_snapshot_cache_misses_a_different_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let cache = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-one".to_owned(),
            panes: Vec::new(),
        };
        atomic::write_temp_then_rename(&path, &cache).unwrap();
        assert!(read_snapshot_cache(&path, "rimz-one").is_some());
        assert!(read_snapshot_cache(&path, "rimz-other").is_none());
    }

    #[test]
    fn read_snapshot_cache_reflects_a_changed_file() {
        // The thread-local parse cache must invalidate when the file changes, or
        // a consumer would serve a stale base forever. Keyed on (mtime, len), so
        // a differently-sized rewrite is caught even if the filesystem's mtime
        // granularity is too coarse to register two fast writes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");

        let first = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-one".to_owned(),
            panes: Vec::new(),
        };
        atomic::write_temp_then_rename_cache(&path, &first).unwrap();
        // Populate this thread's parse cache.
        assert_eq!(
            read_snapshot_cache(&path, "rimz-one").map(|c| c.panes.len()),
            Some(0),
        );

        // Republish a longer, different-session frame in place.
        let second = SnapshotCache {
            produced_at_ms: unix_now_ms() + 1,
            session_name: "rimz-two".to_owned(),
            panes: vec![pane("terminal_0", "zsh", "/tmp")],
        };
        atomic::write_temp_then_rename_cache(&path, &second).unwrap();
        // The stale (rimz-one) entry must not be served; the fresh frame wins.
        assert!(read_snapshot_cache(&path, "rimz-one").is_none());
        assert_eq!(
            read_snapshot_cache(&path, "rimz-two").map(|c| c.panes.len()),
            Some(1),
        );
    }

    #[test]
    fn diff_stats_cache_entry_expires_after_ttl() {
        let entry = DiffStatsCacheEntry::new(
            1_000,
            Some(DiffStats {
                added: 2,
                removed: 1,
            }),
            Some(4),
            Some("feature-migration".to_owned()),
        );

        assert!(entry.is_fresh(1_000 + DIFF_STATS_TTL.as_millis() as u64));
        assert!(!entry.is_fresh(1_001 + DIFF_STATS_TTL.as_millis() as u64));
        assert_eq!(
            entry.stats(),
            Some(DiffStats {
                added: 2,
                removed: 1,
            })
        );
        assert_eq!(entry.commits, Some(4));
        assert_eq!(entry.branch.as_deref(), Some("feature-migration"));
    }

    /// A 5-hour budget window for tests — a known `duration_mins` so the
    /// projection and per-duration reconciliation have something to key on.
    fn rl_window(used: u8, resets_at: Option<Timestamp>) -> RateLimitWindow {
        RateLimitWindow {
            used_percentage: Some(used),
            resets_at,
            duration_mins: Some(300),
        }
    }

    fn provider_panel(kind: &str, windows: Vec<RateLimitWindow>) -> crate::SidebarProviderPanel {
        crate::SidebarProviderPanel {
            kind: kind.to_owned(),
            product_name: kind.to_owned(),
            art: Vec::new(),
            color: 0,
            version: None,
            plan: None,
            metered: true,
            remote_control: false,
            spending: None,
            windows,
        }
    }

    fn snapshot_with_panels(
        workspace: WorkspaceId,
        panels: Vec<crate::SidebarProviderPanel>,
    ) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new());
        snapshot.providers = panels;
        snapshot
    }

    fn root_agent(kind: &str, agent_id: &str, model: Option<&str>) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: agent_id.to_owned(),
            kind: kind.to_owned(),
            status: AgentStatus::Running,
            permission_posture: PermissionPosture::Default,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            model: model.map(ToOwned::to_owned),
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            turn_started_at: None,
            compacting_since: None,
            parked_on_background: false,
            last_seen: now,
            last_activity: now,
        }
    }

    /// A window whose reset instant is still in the future projects unchanged —
    /// the last-known drained reading stands while the budget is genuinely spent.
    #[test]
    fn idle_window_before_reset_shows_last_known() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let future = Timestamp::from_second(2_000_010_000).unwrap();
        let cached = rl_window(80, Some(future));
        let projected = project_idle_window(cached.clone(), now);
        assert_eq!(projected, cached, "before reset the cached reading stands");
    }

    /// Once `now` reaches the reset instant with no fresh reading, the window has
    /// refilled: it projects to full (0% used) with its reset rolled its own
    /// duration forward, so the countdown still reads sensibly.
    #[test]
    fn idle_window_past_reset_refills_to_full_and_rolls_forward() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let passed = Timestamp::from_second(1_999_990_000).unwrap();
        let projected = project_idle_window(rl_window(95, Some(passed)), now);
        assert_eq!(projected.used_percentage, Some(0), "a reset window is full");
        assert_eq!(
            projected.resets_at,
            now.checked_add(SignedDuration::from_secs(300 * 60)).ok(),
            "the reset rolls one window length (300 min) forward from now"
        );
    }

    /// A cached window that can't be aged — no reset instant, or a passed reset
    /// with no known duration to roll by — projects as-is.
    #[test]
    fn idle_window_unageable_shows_as_is() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let passed = Timestamp::from_second(1_999_990_000).unwrap();
        let undated = rl_window(40, None);
        assert_eq!(project_idle_window(undated.clone(), now), undated);
        let no_duration = RateLimitWindow {
            used_percentage: Some(90),
            resets_at: Some(passed),
            duration_mins: None,
        };
        assert_eq!(project_idle_window(no_duration.clone(), now), no_duration);
    }

    /// The producer persists a live reading as ground truth; once the session is
    /// idle (no live window), a reader projects that reading back onto the panel,
    /// so the dashboard shows last-known budgets instead of an empty bar.
    #[test]
    fn producer_persists_live_windows_for_idle_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let future = Timestamp::from_second(4_000_000_000).unwrap();

        // A live frame reports 60% used on the 5h window; the producer writes it.
        let mut producing = snapshot_with_panels(
            workspace.clone(),
            vec![provider_panel("claude", vec![rl_window(60, Some(future))])],
        );
        apply_rate_limit_cache(&mut producing, &runtime, true);
        let cache = read_rate_limits_cache(&runtime.root.join("rate_limits.json"));
        assert_eq!(
            cache
                .windows
                .get("claude")
                .and_then(|limits| limits.windows.first())
                .and_then(|window| window.used_percentage),
            Some(60),
            "the live reading is persisted as ground truth"
        );

        // The session goes idle (no live window). A reader projects the cached
        // reading back onto the panel — the dashboard is not empty.
        let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
        apply_rate_limit_cache(&mut idle, &runtime, false);
        assert_eq!(
            idle.providers[0]
                .windows
                .first()
                .and_then(|window| window.used_percentage),
            Some(60),
            "an idle frame still shows the last-known budget"
        );
    }

    /// An idle window whose reset has long passed projects to full, but the
    /// producer keeps persisting the real last reading — the synthesized full
    /// window is a read-time projection, never written back.
    #[test]
    fn idle_past_reset_shows_full_without_persisting_the_synthetic_window() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let passed = Timestamp::from_second(1_000_000_000).unwrap(); // 2001 — always past

        // Seed a drained reading whose reset has long since passed.
        let path = runtime.root.join("rate_limits.json");
        write_rate_limits_cache(
            &path,
            &RateLimitsCache {
                refreshed_at_ms: 0,
                windows: BTreeMap::from([(
                    "claude".to_owned(),
                    AgentRateLimits {
                        windows: vec![rl_window(90, Some(passed))],
                    },
                )]),
            },
        );

        // An idle producer frame with no live window: the display projects to
        // full, while the persisted ground truth stays the real 90% reading.
        let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
        apply_rate_limit_cache(&mut idle, &runtime, true);
        let shown = idle.providers[0].windows.first().expect("a full window");
        assert_eq!(shown.used_percentage, Some(0), "a reset window shows full");
        assert!(shown.resets_at.is_some(), "with a rolled-forward countdown");

        let persisted = read_rate_limits_cache(&path);
        assert_eq!(
            persisted
                .windows
                .get("claude")
                .and_then(|limits| limits.windows.first())
                .and_then(|window| window.used_percentage),
            Some(90),
            "the cache retains ground truth, not the synthesized full window"
        );
    }

    /// When one provider logs out while another stays, the logged-out kind loses
    /// its panel, so the producer's next write — rebuilt from the panels alone —
    /// drops its cached windows while the surviving kind's are kept. Cache
    /// presence tracks login, so no stale budget can flash on a later re-login.
    #[test]
    fn producer_drops_windows_for_a_logged_out_provider() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let future = Timestamp::from_second(4_000_000_000).unwrap();
        let path = runtime.root.join("rate_limits.json");
        let windows = |used| vec![rl_window(used, Some(future))];

        // Seed windows for both providers through a live frame.
        let mut seeded = snapshot_with_panels(
            workspace.clone(),
            vec![
                provider_panel("claude", windows(40)),
                provider_panel("codex", windows(30)),
            ],
        );
        apply_rate_limit_cache(&mut seeded, &runtime, true);
        let seeded_cache = read_rate_limits_cache(&path);
        assert!(seeded_cache.windows.contains_key("claude"));
        assert!(seeded_cache.windows.contains_key("codex"));

        // Codex logs out: only claude has a panel now. The next producer write
        // rebuilds the cache from the surviving panels, so codex drops out while
        // claude's windows are kept.
        let mut codex_gone =
            snapshot_with_panels(workspace, vec![provider_panel("claude", windows(40))]);
        apply_rate_limit_cache(&mut codex_gone, &runtime, true);
        let after = read_rate_limits_cache(&path);
        assert!(
            after.windows.contains_key("claude"),
            "a still-logged-in provider keeps its windows"
        );
        assert!(
            !after.windows.contains_key("codex"),
            "a logged-out provider's windows drop on the next write"
        );
    }

    /// The out-of-band helper seeds one kind's windows into the shared cache
    /// without disturbing another kind's, so an idle provider's bars paint from
    /// the next producer frame.
    #[test]
    fn merge_account_rate_limits_seeds_a_kind_without_clobbering_others() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let path = runtime.root.join("rate_limits.json");

        // Claude already has cached windows from a live session this run.
        write_rate_limits_cache(
            &path,
            &RateLimitsCache {
                refreshed_at_ms: 1,
                windows: BTreeMap::from([(
                    "claude".to_owned(),
                    AgentRateLimits {
                        windows: vec![rl_window(20, None)],
                    },
                )]),
            },
        );

        merge_account_rate_limits(
            &runtime,
            "codex",
            AgentRateLimits {
                windows: vec![rl_window(55, None)],
            },
        );

        let cache = read_rate_limits_cache(&path);
        assert_eq!(
            cache
                .windows
                .get("codex")
                .and_then(|limits| limits.windows.first())
                .and_then(|w| w.used_percentage),
            Some(55),
            "the idle provider's windows are seeded"
        );
        assert!(
            cache.windows.contains_key("claude"),
            "an existing kind's windows are preserved"
        );
    }

    #[test]
    fn active_codex_session_refreshes_context_even_when_windows_exist() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let mut snapshot = snapshot_with_panels(
            workspace,
            vec![provider_panel("codex", vec![rl_window(42, None)])],
        );
        snapshot
            .agents
            .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));

        assert_eq!(
            codex_rate_limit_refreshes(&snapshot),
            vec![CodexRateLimitRefresh::Session {
                session_id: "sess-active".to_owned(),
                model_hint: Some("gpt-5.5-codex".to_owned()),
            }],
            "active Codex sessions refresh their sidecars even when the dashboard already has windows"
        );
    }

    #[test]
    fn idle_metered_codex_refreshes_account_cache() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let snapshot = snapshot_with_panels(
            workspace,
            vec![provider_panel("codex", vec![rl_window(25, None)])],
        );

        assert_eq!(
            codex_rate_limit_refreshes(&snapshot),
            vec![CodexRateLimitRefresh::Account],
            "idle Codex accounts refresh the shared cache even with prior windows"
        );
    }

    #[test]
    fn active_codex_sessions_win_over_account_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let mut snapshot =
            snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
        snapshot
            .agents
            .push(root_agent("codex", "sess-active", None));

        assert_eq!(
            codex_rate_limit_refreshes(&snapshot),
            vec![CodexRateLimitRefresh::Session {
                session_id: "sess-active".to_owned(),
                model_hint: None,
            }],
            "a stale account cache cannot refresh underneath a live Codex sidecar"
        );
    }

    #[test]
    fn unmetered_or_non_codex_providers_do_not_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let mut unmetered_codex = provider_panel("codex", Vec::new());
        unmetered_codex.metered = false;
        let snapshot = snapshot_with_panels(
            workspace,
            vec![unmetered_codex, provider_panel("claude", Vec::new())],
        );

        assert!(
            codex_rate_limit_refreshes(&snapshot).is_empty(),
            "only metered Codex has an out-of-band budget read"
        );
    }

    /// The per-target throttle marker gates the out-of-band fetch: the first call is
    /// due (and touches the marker), the immediate next is not.
    #[test]
    fn codex_rate_limit_probe_throttles_per_target() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let account = CodexRateLimitRefresh::Account;
        let session = CodexRateLimitRefresh::Session {
            session_id: "sess/one".to_owned(),
            model_hint: None,
        };
        let other_session = CodexRateLimitRefresh::Session {
            session_id: "sess/two".to_owned(),
            model_hint: None,
        };

        assert!(codex_rate_limit_probe_due(&runtime, &account));
        assert!(
            !codex_rate_limit_probe_due(&runtime, &account),
            "a freshly-stamped account backs off"
        );
        assert!(codex_rate_limit_probe_due(&runtime, &session));
        assert!(
            !codex_rate_limit_probe_due(&runtime, &session),
            "a freshly-stamped session backs off"
        );
        assert!(
            codex_rate_limit_probe_due(&runtime, &other_session),
            "a different session has its own marker"
        );

        let old = SystemTime::now()
            .checked_sub(CODEX_RATE_LIMIT_REFRESH_INTERVAL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(codex_rate_limit_probe_marker(&runtime, &session))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(
            codex_rate_limit_probe_due(&runtime, &session),
            "the session becomes due again after the 60s interval"
        );
    }

    /// Only Codex exposes an account-scoped window read today; Claude's windows
    /// have no source outside a live statusline, so it never triggers a fetch.
    #[test]
    fn only_codex_has_an_out_of_band_window_read() {
        assert!(provider_has_out_of_band_windows("codex"));
        assert!(!provider_has_out_of_band_windows("claude"));
        assert!(!provider_has_out_of_band_windows("pi"));
    }
}
