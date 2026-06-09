//! Shared sidebar enrichment fold over a ledger rollup and an optional pane frame.
//!
//! One ordered spine serves both producer and consumer reads. Producer-only work
//! arrives through [`EnrichMode::Producing`]; consumer reads project published
//! runtime caches and sidecars only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fs4::FileExt;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::codex;
use crate::agents::spending::{
    LiveSpendBaselines, ProviderSpendingCache, read_live_spend_baselines,
    read_provider_spending_cache, today_spend_live_usd, write_live_spend_baselines,
};
use crate::agents::{AgentRateLimits, RateLimitWindow};
use crate::feed::AgentStatus;
use crate::ids::{PaneId, WorkspaceId};
use crate::ledger::snapshot::{LazyAgentPairingDiagnostic, LazyAgentPairingResult};
use crate::{
    RuntimePaths, SidebarOwnView, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
};

use super::cache::{
    AccountsCache, DiffStatsCache, GIT_ACTIVITY_WINDOW, read_diff_stats_cache, unix_now_ms,
};
use super::frame::{PaneFrame, PaneMetrics};
pub(crate) use crate::sidebar::timing::CODEX_RATE_LIMIT_REFRESH_INTERVAL;

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish before forking its own probe.
/// Matched to the diff-stats single-flight, leaning long enough to ride the
/// elder's `claude auth status` fork rather than racing it.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

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

/// The checkout path a group's git reads run against. A path-keyed group —
/// per-path or root-keyed — carries it as the key's first line (the key may
/// carry a `\n<branch>` suffix when one path holds two branches), which is
/// stabler than any one row's cwd: a root-keyed pod's rows can sit in
/// different subdirs of one checkout. A non-path key (`branch:…`, the
/// `external` catch-all) falls back to the rows' shared path.
fn worktree_group_path(group: &SidebarWorktreeGroup) -> Option<&str> {
    group
        .key
        .split('\n')
        .next()
        .filter(|key| Path::new(key).is_absolute())
        .or_else(|| {
            group
                .rows
                .iter()
                .find_map(|row| row.worktree_path.as_deref())
                .filter(|path| !path.is_empty())
        })
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

/// The worktree paths whose git facts refresh on the fast [`DIFF_STATS_TTL`]:
/// a `Worktree`-kind group is hot when any of its agent rows is `Running` or
/// was active within [`GIT_ACTIVITY_WINDOW`] of `snapshot.now`. Derived from
/// the group's own rows with the same path recovery and live-dir gate as
/// [`needed_worktree_paths`], so the hot set is a subset of the needed set by
/// construction — no equality-vs-containment mismatch is possible, whatever
/// raw path an agent's payload carried. A group with only process rows is
/// cold; subagents fold under their parent's row, whose activity covers them.
/// Pure over the view-model and its one `now`.
pub fn hot_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let window = SignedDuration::try_from(GIT_ACTIVITY_WINDOW).unwrap_or(SignedDuration::MAX);
    let mut hot = BTreeSet::new();
    for group in &snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group) else {
            continue;
        };
        if !Path::new(path).is_dir() {
            continue;
        }
        // A future-stamped row reads as a negative age — within the window,
        // the safe (hot) direction, matching the saturating TTL convention.
        let any_hot = group.rows.iter().any(|row| {
            row.is_agent()
                && (row.status() == Some(AgentStatus::Running)
                    || snapshot.now.duration_since(row.last_activity) <= window)
        });
        if any_hot {
            hot.insert(path.to_owned());
        }
    }
    hot
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
        if let Some(behind) = entry.behind {
            group.commits_behind = Some(behind);
        }
        // A remote-default trunk resolves as `origin/<name>`; the header's
        // `≡`/`✓` markers name the branch, so the remote prefix is display
        // noise.
        if let Some(trunk) = entry.trunk.filter(|trunk| !trunk.is_empty()) {
            let display = trunk.strip_prefix("origin/").unwrap_or(&trunk).to_owned();
            group.trunk = Some(display);
        }
        if let Some(branch) = entry.branch.filter(|branch| !branch.is_empty()) {
            group.label = branch;
        }
        if let Some(clean) = entry.clean {
            group.clean = Some(clean);
        }
    }
}

/// The lazy-registering agent kinds whose Rimz hooks are installed — the gate for
/// the idle-instance synthesis on a wired-but-unbound agent pane. Filtered to lazy
/// agents (not `agent_hooks_ready`'s any-agent check), so a Claude-only install
/// never promotes an unwired Codex pane to an idle agent (it would otherwise read
/// as a forever-idle agent Rimz can report no status for). Environment, not ledger.
pub fn wired_lazy_kinds() -> Vec<String> {
    crate::agents::ADAPTERS
        .iter()
        .filter(|agent| {
            let capabilities = agent.descriptor().capabilities;
            capabilities.registers_lazily && capabilities.hook_install && agent.hooks_installed()
        })
        .map(|agent| agent.descriptor().kind.to_owned())
        .collect()
}

/// Launch-model defaults for wired lazy-registering agents, used only for
/// synthesized idle rows before a real session reports its model.
pub fn wired_lazy_default_models() -> BTreeMap<String, String> {
    crate::agents::ADAPTERS
        .iter()
        .filter(|agent| {
            let capabilities = agent.descriptor().capabilities;
            capabilities.registers_lazily && capabilities.hook_install && agent.hooks_installed()
        })
        .filter_map(|agent| {
            agent
                .default_launch_model()
                .map(|model| (agent.descriptor().kind.to_owned(), model))
        })
        .collect()
}

/// How one [`enrich`] call resolves its producer-vs-consumer differences. The
/// fold order is one spine; the mode names each insertion point.
pub enum EnrichMode<'a> {
    /// A consumer tab's read-only fold: every input is a published runtime
    /// cache or sidecar — the cached worktree roots, the published accounts
    /// and spending caches, the cached diff-stats projection. No `list-panes`,
    /// no git, no subprocess, no ledger lock.
    Cached,
    /// The elected producer's fold. The caller supplies the producer-only
    /// inputs and callbacks, and the spine sequences them into the shared
    /// order.
    Producing {
        /// The room's freshly enumerated group roots; `None` when the
        /// snapshot carries no project root.
        roots: Option<Vec<PathBuf>>,
        /// The fleet spending walk. The shared publish is account-global;
        /// per-workspace live-cost baselines are refreshed by the fold after
        /// the walk cache is available and rows hold their latest context.
        compute_spending: &'a dyn Fn(&SidebarSnapshot) -> ProviderSpendingCache,
        /// The per-machine config, loaded once by the caller — the config
        /// fold consumes it here and the git refresh closure has already
        /// taken the preferred trunk from it. Boxed to keep the enum the size
        /// of its `Cached` common case.
        config: Box<crate::config::MachineConfig>,
        /// The per-worktree git refresh over the snapshot's groups.
        refresh_git: &'a dyn Fn(&mut SidebarSnapshot),
    },
}

#[derive(Debug, Serialize)]
struct ProducerBindingFallbackLog<'a> {
    event: &'static str,
    at: Timestamp,
    workspace_id: &'a WorkspaceId,
    #[serde(flatten)]
    pairing: &'a LazyAgentPairingDiagnostic,
}

/// Fold the enrichments onto a base snapshot — one ordered spine for the
/// producer and every consumer, so the two paths can never drift. `frame` is
/// the live pane frame (panes plus the `produced_at_ms` read stamp): the
/// producer's freshly resolved list, or the published `snapshot.json` a
/// consumer read back. `None` skips the pane overlay — a cold consumer start
/// (no publish yet) or a producer call with no live session — and leaves
/// `worktree_groups` empty while the rollup metadata remains available.
///
/// [`EnrichMode::Cached`] reads only runtime caches and sidecars;
/// [`EnrichMode::Producing`] carries the producer inputs in the mode and inserts
/// the daemon reap, the account probe, and the git refresh at their named
/// points.
pub fn enrich(
    mut snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    mut mode: EnrichMode<'_>,
) -> SidebarSnapshot {
    let producing = matches!(mode, EnrichMode::Producing { .. });
    let machine_config = match &mode {
        EnrichMode::Cached => crate::config::MachineConfig::load().unwrap_or_default(),
        EnrichMode::Producing { config, .. } => (**config).clone(),
    };
    // Attention timing is needed during pane projection, before the full config
    // fold builds provider panels and stamps context severity.
    snapshot.sidebar = machine_config.sidebar.clone();

    // The room's group roots — a repo room's worktree checkouts (so one parked
    // outside the project root still earns its own pod instead of folding into
    // `external`), a directory room's depth-1 child repos. The producer passes
    // its fresh enumeration in; a consumer reads the cached one back.
    match &mut mode {
        EnrichMode::Cached => {
            if snapshot.project_root.is_some() {
                snapshot = snapshot.with_worktree_roots(cached_worktree_roots(runtime));
            }
        }
        EnrichMode::Producing { roots, .. } => {
            if let Some(roots) = roots.take() {
                snapshot = snapshot.with_worktree_roots(roots);
            }
        }
    }

    // Fold each session's rich statusline context onto its agent state
    // (read-only; the feed process is the writer). Both the context sidecar
    // and the per-tool activity heartbeats fold only onto existing agents, so
    // an empty room skips both directory scans — the common idle case.
    // Activity lands before the pane overlay so age, ranking, the ask-fold
    // guard, and the stall window all see the truer per-tool value rather than
    // the turn-grained event timestamp.
    if !snapshot.agents.is_empty() {
        snapshot = snapshot.with_agent_context(crate::ledger::agent_context::read_all(runtime));
        snapshot =
            snapshot.with_subagent_context(crate::ledger::subagent_context::read_all(runtime));
        let activity = crate::agent_activity::read_for_keys(
            runtime,
            snapshot
                .agents
                .iter()
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
        );
        snapshot = snapshot.with_agent_activity(&activity);
    }

    // Wiring state gates the live-pane fold (the idle-instance synthesis), so
    // set it before folding panes, not after.
    snapshot.wired_lazy_kinds = wired_lazy_kinds();
    snapshot.lazy_agent_default_models = wired_lazy_default_models();

    // Producer-only: reap daemon-mode Codex ghosts the app-server no longer
    // holds. A remote-control conversation records the shared daemon's pid,
    // which outlives it, so process liveness can never reap it. Gated on a
    // pane-less root `codex` session actually being present, so the common
    // room pays no proc scan or daemon probe. Best-effort and fail-safe — no
    // daemon process or an untrusted loaded list keeps every session — and run
    // before the pane fold so a ghost can neither render nor bind its stale
    // stats to a live pane.
    if producing
        && snapshot.agents.iter().any(|agent| {
            agent.kind == "codex" && agent.pane.is_none() && agent.parent_agent_id.is_none()
        })
    {
        let daemon_pids = crate::remote_control::codex_daemon_pids();
        if !daemon_pids.is_empty() {
            let loaded = crate::agents::codex::loaded_daemon_threads();
            snapshot.drop_dead_daemon_sessions(&daemon_pids, loaded.as_ref());
        }
    }

    if let Some(frame) = frame {
        snapshot.panes_produced_at_ms = Some(frame.produced_at_ms);
        if let Some(own) = exclude {
            snapshot.own_view = SidebarOwnView::from_frame(own, &frame);
        }
        let metrics = frame.pane_metrics().collect::<Vec<_>>();
        let panes = frame.to_pane_refs();
        let admitted_panes = SidebarSnapshot::card_admitted_live_panes(panes.clone(), exclude);
        let lazy_pairings =
            crate::ledger::snapshot::compute_lazy_agent_pairings(&admitted_panes, &snapshot.agents);
        if producing {
            log_lazy_pairing_ambiguities(&snapshot, runtime, &lazy_pairings);
        }
        // Recomputed from the full pane list (pre-exclusion), before
        // `with_live_panes` consumes `panes` — never trusted from the base,
        // for producer/consumer symmetry. The panes arrive with their
        // `/proc`-derived process starts already stamped at frame production
        // (`produce` stamps before the publish), so the cwd-fallback guard
        // fires identically on every path.
        snapshot.only_daemon_view_remains = SidebarSnapshot::only_daemon_view(&panes);
        snapshot = snapshot.with_admitted_live_panes(admitted_panes, &lazy_pairings);
        apply_pane_metrics(&mut snapshot, metrics);
    }
    // Per-machine display preferences and the per-provider dashboard are
    // environment, not ledger, so the rollup base carries neither. The
    // producer probes accounts out of band and publishes them alongside its
    // walked spending; a consumer reads both published caches back — never a
    // per-tick fork or a ledger lock. Git rides the same split: the producer
    // refreshes the per-worktree facts (single-flighted), a consumer projects
    // the cached ones.
    let is_producer = matches!(&mode, EnrichMode::Producing { .. });
    let spending_cache = match mode {
        EnrichMode::Cached => {
            let cache;
            (snapshot, cache) = fold_machine_config_cached(snapshot, runtime, machine_config);
            let diff_cache = read_diff_stats_cache(&runtime.root.join("diff-stats.json"));
            project_diff_stats(&mut snapshot, &diff_cache);
            cache
        }
        EnrichMode::Producing {
            compute_spending,
            config,
            refresh_git,
            ..
        } => {
            let spending = compute_spending(&snapshot);
            snapshot = fold_machine_config_producing(
                snapshot,
                runtime,
                &spending.spending.by_provider,
                *config,
            );
            refresh_git(&mut snapshot);
            spending
        }
    };
    // The fleet `value_tally` — the JSONL today / month / all-time pile read
    // by the cockpit's today figure and the bottom value corner — attaches
    // once, after every fold; `None` when nothing has ever been recorded.
    snapshot.value_tally =
        (!spending_cache.spending.total.is_zero()).then_some(spending_cache.spending.total.clone());
    // The live overlay rides the same fold: a context sidecar push wakes the
    // consumer, the refold lands the session's fresh cost on its row, and the
    // cockpit's headline retargets in the same frame — no waiting out the
    // walk's TTL.
    let baselines = refresh_live_spend_baselines(
        runtime,
        &snapshot,
        spending_cache.refreshed_at_ms,
        is_producer,
    );
    apply_live_today_spend(&mut snapshot, &spending_cache, &baselines.baselines);
    snapshot
}

fn log_lazy_pairing_ambiguities(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    lazy_pairings: &LazyAgentPairingResult,
) {
    for pairing in lazy_pairings.diagnostics() {
        crate::binding_log::append(
            runtime,
            &ProducerBindingFallbackLog {
                event: "producer_lazy_agent_pairing",
                at: Timestamp::now(),
                workspace_id: &snapshot.workspace_id,
                pairing,
            },
        );
    }
}

fn apply_pane_metrics(snapshot: &mut SidebarSnapshot, metrics: Vec<(PaneId, PaneMetrics)>) {
    if metrics.is_empty() {
        return;
    }
    let metrics: HashMap<PaneId, PaneMetrics> = metrics.into_iter().collect();
    for group in &mut snapshot.worktree_groups {
        for row in &mut group.rows {
            let Some(pane) = row.pane.as_ref() else {
                continue;
            };
            let Some(metric) = metrics.get(&pane.pane_id) else {
                continue;
            };
            if let Some(process) = row.as_process_mut() {
                process.rss_kb = metric.rss_kb;
                process.cpu_pct = metric.cpu_pct;
                process.io_bps = metric.io_bps;
                if let Some(state) = metric.process_state {
                    process.state = state;
                }
            }
        }
    }
}

/// Fold the per-machine config and the per-provider dashboard onto a *producer*
/// snapshot: the `⇅ rc` flags and the account-scoped budget blocks.
/// The account facts come from a live out-of-band probe (`claude auth status`, a
/// `codex` auth-file read) — a subprocess — so this is the producer's job. The
/// probed map is published to the shared `accounts.json` cache for consumers to
/// read, mirroring the diff-stats single-flight: one fork on the elder, a cache
/// read on every other tab. The caller loads the per-machine config once
/// (best-effort, defaults on a read failure) and threads it here and to the git
/// probe's trunk ladder; the probe is memoized so it stays off the hot path.
fn fold_machine_config_producing(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
    config: crate::config::MachineConfig,
) -> SidebarSnapshot {
    let accounts = produce_accounts(&snapshot, runtime);
    let mut snapshot = fold_machine_config_with(snapshot, config, accounts, provider_spending);
    // The producer owns the account-scoped window cache: it writes live readings
    // back so the budgets survive a session ending or going idle.
    apply_rate_limit_cache(&mut snapshot, runtime, true);
    // Codex's budget windows live behind the app-server. The producer refreshes
    // active session sidecars and the idle account cache on a coarse cadence so a
    // long-running task does not wait for the next turn boundary to repaint.
    refresh_codex_rate_limits(&snapshot, runtime);
    snapshot
}

/// Refresh Codex enrichment from the producer. A live/root Codex session first
/// refreshes its transcript-derived tokens/cost in process with a stat gate, then
/// the existing detached helper refreshes app-server-owned budget/account fields
/// on the coarse per-target cadence. A logged-in metered Codex account with no
/// root session refreshes the account cache instead, so idle dashboards stay
/// current.
fn refresh_codex_rate_limits(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in codex_rate_limit_refreshes(snapshot) {
        if let CodexRateLimitRefresh::Session {
            session_id,
            model_hint,
        } = &refresh
        {
            refresh_codex_transcript_context(runtime, session_id, model_hint.as_deref());
        }
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

/// Refresh one Codex session's transcript-derived tokens/cost into its context
/// sidecar and wake every renderer. Stat-gated: an unchanged rollout tail is a
/// no-op, so every trigger — the producer tick here, the renderer's transcript
/// watcher (`sidebar_pane::app::transcript_watch`) — can fire freely.
pub fn refresh_codex_transcript_context(
    runtime: &RuntimePaths,
    session_id: &str,
    model_hint: Option<&str>,
) {
    let prior = crate::ledger::agent_context::read_one(runtime, "codex", session_id);
    let refresh = codex::refresh_transcript_context(
        session_id,
        model_hint,
        prior
            .as_ref()
            .and_then(|record| record.context.effort.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
    );
    let Some(refresh) = refresh else {
        return;
    };
    if let Err(err) = crate::ledger::agent_context::merge_local_context(
        runtime,
        "codex",
        session_id,
        prior,
        refresh,
        Timestamp::now(),
    ) {
        tracing::warn!(error = %err, "sidebar: failed to merge codex transcript context");
        return;
    }
    let _ = crate::ledger::wakeup::wake_sidebars(runtime);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexRateLimitRefresh {
    Session {
        session_id: String,
        model_hint: Option<String>,
    },
    Account,
}

pub(crate) fn codex_rate_limit_refreshes(snapshot: &SidebarSnapshot) -> Vec<CodexRateLimitRefresh> {
    let sessions = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind == "codex" && agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .map(|agent| CodexRateLimitRefresh::Session {
            session_id: agent.agent_id.to_string(),
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
pub(crate) fn provider_has_out_of_band_windows(kind: &str) -> bool {
    kind == "codex"
}

/// Throttle one Codex rate-limit refresh target via a marker file under the
/// runtime root: skip when the last attempt is younger than the interval, touch
/// it before spawning. Windows move on the scale of minutes, so a one-minute
/// gate keeps a slow/unreachable app-server from spawning a helper every frame
/// while still updating during long-running turns.
pub(crate) fn codex_rate_limit_probe_due(
    runtime: &RuntimePaths,
    refresh: &CodexRateLimitRefresh,
) -> bool {
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

pub(crate) fn codex_rate_limit_probe_marker(
    runtime: &RuntimePaths,
    refresh: &CodexRateLimitRefresh,
) -> PathBuf {
    match refresh {
        CodexRateLimitRefresh::Account => runtime.shared_root.join("rate-limit-probe.codex"),
        CodexRateLimitRefresh::Session { session_id, .. } => {
            let mut hasher = Sha256::new();
            hasher.update(b"codex-session");
            hasher.update([0]);
            hasher.update(session_id.as_bytes());
            let digest = hex::encode(hasher.finalize());
            runtime
                .shared_root
                .join(format!("rate-limit-probe.codex.{}", &digest[..32]))
        }
    }
}

/// Spawn the detached, fresh-stdio helper that refreshes one active Codex
/// session's app-server-owned `AgentContext` fields. Transcript tokens/cost are
/// refreshed in process before this helper is considered. Best-effort: a spawn
/// failure is logged and dropped — the dashboard keeps the prior reading until
/// the next due frame.
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
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-refresh-context")
    {
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
    if let Err(err) =
        crate::child_process::spawn_detached_reaped(&mut cmd, "codex-refresh-rate-limits")
    {
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
    let path = runtime.shared_accounts_path();

    // Fast path: a young publish needs no lock and no fork.
    let cache = read_accounts_cache(&path);
    if cache.is_fresh(unix_now_ms()) && !accounts_cache_missing_versions(&cache, snapshot) {
        return cache.accounts;
    }

    // Slow path: elect one prober for this user's refresh window. The
    // freshness closure also serves coalesce's post-win re-check, so a peer that
    // published between our miss and the lock is honoured rather than re-forked.
    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        let cache = read_accounts_cache(&path);
        (cache.is_fresh(unix_now_ms()) && !accounts_cache_missing_versions(&cache, snapshot))
            .then_some(cache.accounts)
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
/// config read stays local so each tab honours its own display preferences.
/// Returns the published spending cache whole — tally and stamp — so the caller
/// folds the value tally and the live today-spend overlay from one read.
fn fold_machine_config_cached(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    config: crate::config::MachineConfig,
) -> (SidebarSnapshot, ProviderSpendingCache) {
    let accounts = read_accounts_cache(&runtime.shared_accounts_path()).accounts;
    // Consumers read the producer's published spending cache rather than
    // re-walking the JSONL transcript history themselves.
    let cache = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let mut snapshot =
        fold_machine_config_with(snapshot, config, accounts, &cache.spending.by_provider);
    // A consumer reads the producer's published windows to fill idle gaps, but
    // never writes — the single-flight contract keeps the cache the producer's.
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    (snapshot, cache)
}

/// Stamp the cockpit's live today-spend onto the snapshot: the published
/// walk's exact figure plus each live row's overshoot over its publish-time
/// baseline ([`today_spend_live_usd`]), so the headline tracks every
/// context sidecar push instead of waiting out the walk's TTL. Shared by the
/// producing CLI and the consumer fold, so every tab in a room paints the same
/// figure; zero — an empty room on an unspent day — stays `None` and the
/// cockpit keeps its bare `¤` line.
pub fn apply_live_today_spend(
    snapshot: &mut SidebarSnapshot,
    cache: &ProviderSpendingCache,
    baselines: &BTreeMap<String, f64>,
) {
    let live = today_spend_live_usd(
        cache.spending.total.today.usd,
        live_row_costs(snapshot),
        baselines,
        cache.refreshed_at_ms,
    );
    snapshot.today_spend_live_usd = (live > 0.0).then_some(live);
}

fn refresh_live_spend_baselines(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    observed_walk_ms: u64,
    persist: bool,
) -> LiveSpendBaselines {
    let path = runtime.live_spend_baselines_path();
    let mut baselines = read_live_spend_baselines(&path);
    // Producer-only: the elected elder captures the per-room baselines at each
    // new walk; consumer tabs read what it wrote.
    if persist && observed_walk_ms > 0 && observed_walk_ms > baselines.observed_walk_ms {
        baselines = LiveSpendBaselines {
            observed_walk_ms,
            baselines: live_row_costs(snapshot)
                .map(|(id, usd, _)| (id.to_owned(), usd))
                .collect(),
        };
        write_live_spend_baselines(&path, &baselines);
    }
    baselines
}

/// Every agent row's live statusline cost: `(row id, total_cost_usd,
/// registered-at ms)` triples — the overlay's per-session input, and
/// (collected to a map) the baseline set the producer stamps at each walk
/// publish.
pub fn live_row_costs(
    snapshot: &SidebarSnapshot,
) -> impl Iterator<Item = (&str, f64, Option<u64>)> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter_map(|row| {
            let usd = row
                .as_agent()
                .and_then(|agent| agent.context.as_ref())
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)?;
            let registered_ms = row
                .as_agent()
                .and_then(|agent| agent.registered_at)
                .map(|at| at.as_millisecond().max(0) as u64);
            Some((row.id.as_str(), usd, registered_ms))
        })
}

/// Apply the resolved config and already-resolved accounts onto the snapshot:
/// the per-provider `⇅ rc` flags, the dashboard aggregates, and each agent
/// row's context-severity verdict.
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

    // Stamp each agent row's context-severity verdict now that the
    // `[sidebar.context]` bands are known — classified once here, on both the
    // producer and consumer fold, so the renderer's color ramp and any future
    // signal emitter read one authority instead of re-deriving the tier.
    let bands = snapshot.sidebar.context.clone();
    stamp_context_severity(&mut snapshot.worktree_groups, &bands);

    // The `⇅ rc` flag per provider comes from the remote-control toggles.
    let mut remote_control_flags: BTreeMap<String, bool> = BTreeMap::new();
    remote_control_flags.insert("claude".to_owned(), remote_control.claude);
    remote_control_flags.insert("codex".to_owned(), remote_control.codex);

    snapshot.with_provider_aggregates(&accounts, &remote_control_flags, provider_spending)
}

/// Stamp [`SidebarRow::context_severity`] on every agent row from the
/// `[sidebar.context]` bands: [`crate::feed::ContextSeverity::classify`] over
/// the row's gauge inputs, the one verdict the renderer's color ramp and any
/// future signal emitter read. Process rows carry no context and stay `None`.
pub(crate) fn stamp_context_severity(
    groups: &mut [crate::SidebarWorktreeGroup],
    bands: &crate::config::ContextSeverityConfig,
) {
    for group in groups {
        for row in &mut group.rows {
            if row.is_agent() {
                let severity = crate::feed::ContextSeverity::classify(
                    row.context_gauge_percent().unwrap_or(0),
                    row.context_used_tokens(),
                    bands,
                );
                if let Some(agent) = row.as_agent_mut() {
                    agent.context_severity = Some(severity);
                }
            }
        }
    }
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
    let mut kinds: Vec<String> = crate::agents::known_kinds().map(str::to_owned).collect();
    let active_version_kinds = active_version_probe_kinds(snapshot);
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_none() && !kinds.iter().any(|known| agent.kind == **known) {
            kinds.push(agent.kind.to_string());
        }
    }
    let mut accounts: BTreeMap<String, crate::agents::AgentAccount> = BTreeMap::new();
    let mut ok = true;
    for kind in kinds {
        // An unregistered kind has no out-of-band login probe — nothing to retry.
        let Some(adapter) = crate::agents::find_adapter(&kind) else {
            continue;
        };
        match adapter.probe_account() {
            crate::agents::account::AccountProbe::Found(mut account) => {
                if adapter.probes_version() && account.version.is_none() {
                    account.version = adapter.probe_version();
                    if account.version.is_none() {
                        ok = false;
                    }
                }
                accounts.insert(kind, account);
            }
            crate::agents::account::AccountProbe::LoggedOut => {
                if active_version_kinds.contains(&kind) {
                    if let Some(version) = adapter.probe_version() {
                        accounts.insert(
                            kind,
                            crate::agents::AgentAccount {
                                version: Some(version),
                                ..Default::default()
                            },
                        );
                    } else {
                        ok = false;
                    }
                }
            }
            crate::agents::account::AccountProbe::Unavailable => {
                ok = false;
                if active_version_kinds.contains(&kind)
                    && let Some(version) = adapter.probe_version()
                {
                    accounts.insert(
                        kind,
                        crate::agents::AgentAccount {
                            version: Some(version),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }
    (accounts, ok)
}

fn active_version_probe_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter_map(|agent| {
            crate::agents::find_adapter(agent.kind.as_str())
                .filter(|adapter| adapter.probes_version())
                .map(|_| agent.kind.to_string())
        })
        .collect()
}

pub(crate) fn accounts_cache_missing_versions(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
) -> bool {
    // A failed probe already rides the short retry TTL. Honor that freshness
    // window instead of bypassing it every producer tick.
    if !cache.ok {
        return false;
    }
    if cache.accounts.iter().any(|(kind, account)| {
        account.version.is_none()
            && crate::agents::find_adapter(kind).is_some_and(|adapter| adapter.probes_version())
    }) {
        return true;
    }
    active_version_probe_kinds(snapshot)
        .into_iter()
        .any(|kind| {
            cache
                .accounts
                .get(&kind)
                .and_then(|account| account.version.as_ref())
                .is_none()
        })
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
