//! Shared sidebar enrichment fold over a ledger rollup and an optional pane frame.
//!
//! One ordered spine serves both producer and consumer reads. Producer-only work
//! arrives through [`EnrichMode::Producing`]; consumer reads project published
//! runtime caches and sidecars only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::agents::spending::{ProviderSpendingCache, read_provider_spending_cache};
use crate::feed::AgentStatus;
use crate::ids::{PaneId, WorkspaceId};
use crate::ledger::snapshot::{LazyAgentPairingDiagnostic, LazyAgentPairingResult};
use crate::{
    RuntimePaths, SidebarOwnView, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::cache::{DiffStatsCache, GIT_ACTIVITY_WINDOW, read_diff_stats_cache};
use super::frame::{PaneFrame, PaneMetrics};
#[cfg(test)]
pub(crate) use crate::sidebar::timing::CODEX_RATE_LIMIT_REFRESH_INTERVAL;

mod accounts;
mod codex_refresh;
mod live_spend;
mod rate_limits;

#[cfg(test)]
pub(crate) use accounts::accounts_cache_missing_versions;
pub use codex_refresh::refresh_codex_transcript_context;
#[cfg(test)]
pub(crate) use codex_refresh::{
    CodexRateLimitRefresh, codex_rate_limit_probe_due, codex_rate_limit_probe_marker,
    codex_rate_limit_refreshes, provider_has_out_of_band_windows,
};
pub use live_spend::{apply_live_today_spend, live_row_costs};
pub(crate) use rate_limits::apply_rate_limit_cache;
pub use rate_limits::{RateLimitsCache, merge_account_rate_limits};
#[cfg(test)]
pub(crate) use rate_limits::{
    project_idle_window, read_rate_limits_cache, write_rate_limits_cache,
};

use accounts::{produce_accounts, read_accounts_cache};
use codex_refresh::refresh_codex_rate_limits;
use live_spend::refresh_live_spend_baselines;

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
/// agents (not a broad any-agent hook check), so a Claude-only install
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
