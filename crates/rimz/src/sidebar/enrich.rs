//! Shared sidebar projection fold over a ledger rollup and optional pane frame.
//!
//! One ordered spine serves both producer and consumer reads. It projects lane
//! caches and sidecars; it forks no subprocess and writes no cache files.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::agents::spending::SpendingCaches;
use crate::harness::auto_continue::{self, ResumeMessage};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::ledger::snapshot::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, ResumeOutcome, RuntimeReapInputs,
    SidebarPresence,
};
use crate::{
    RuntimePaths, SidebarLinkFreshness, SidebarLinkHealth, SidebarOwnView, SidebarSnapshot,
    SidebarWorktreeKind, WorktreePrState, WorktreeTrunkSync,
};
use jiff::Timestamp;
use serde::Serialize;

use super::frame::{PaneFrame, PaneMetrics};
use super::refresh::accounts::{cached_accounts_for_snapshot, read_accounts_cache};
use super::refresh::credits::apply_credits_cache;
use super::refresh::daemon_reap::read_codex_daemon_reap;
use super::refresh::git_stats::{
    DiffStatsCache, DiffStatsCacheEntry, read_diff_stats_cache, worktree_group_path,
};
use super::refresh::live_spend::apply_live_today_spend;
use super::refresh::pr::read_pr_state_cache;
use super::refresh::rate_limits::apply_rate_limit_cache;
use super::timing::{LINK_STATS_EXPIRE, LINK_STATS_STALE};

#[cfg(test)]
mod tests;

/// The repo's worktree checkout roots the producer last published, read-only
/// (no `git worktree list` fork). A consumer reuses whatever the elder cached,
/// even stale; an empty set leaves the reducer's project-root prefix test to
/// stand alone.
pub fn cached_worktree_roots(runtime: &RuntimePaths) -> Vec<PathBuf> {
    read_diff_stats_cache(&runtime.diff_stats_path())
        .worktrees
        .map(|cache| cache.roots)
        .unwrap_or_default()
}

pub(crate) fn read_auto_continue_resume_messages(
    messages_dir: Option<&Path>,
    config: &crate::config::ResumeConfig,
    outcomes: &[ResumeOutcome],
) -> Vec<ResumeMessage> {
    if config.auto_continue {
        auto_continue::read_resume_messages(messages_dir, outcomes)
    } else {
        Vec::new()
    }
}

/// Fold the remote-link stats sidecar onto the snapshot. Local rooms never have
/// this file; corrupt, unknown-version, and expired files erase the badge.
pub fn fold_link_stats(snapshot: &mut SidebarSnapshot, runtime: &RuntimePaths, now_ms: u64) {
    let path = crate::remote::link::stats_path(runtime);
    let Ok(bytes) = std::fs::read(path) else {
        snapshot.link = None;
        return;
    };
    let Ok(file) = serde_json::from_slice::<crate::remote::link::LinkStatsFile>(&bytes) else {
        snapshot.link = None;
        return;
    };
    if !file.version_ok() {
        snapshot.link = None;
        return;
    }
    let age_ms = now_ms.saturating_sub(file.received_at_ms);
    if age_ms > LINK_STATS_EXPIRE.as_millis() as u64 {
        snapshot.link = None;
        return;
    }
    let freshness = if age_ms > LINK_STATS_STALE.as_millis() as u64 {
        SidebarLinkFreshness::Stale
    } else {
        SidebarLinkFreshness::Fresh
    };
    let stats = file.stats;
    snapshot.link = Some(SidebarLinkHealth {
        rtt_ms: stats.rtt_ms,
        miss_pct: stats.miss_pct,
        tier: crate::remote::link::link_tier(stats.rtt_ms, stats.miss_pct),
        freshness,
        sampled_at_ms: file.received_at_ms,
    });
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
        let display_trunk = entry
            .trunk
            .as_deref()
            .filter(|trunk| !trunk.is_empty())
            .map(|trunk| trunk.strip_prefix("origin/").unwrap_or(trunk).to_owned());
        if let Some(display) = display_trunk.as_ref() {
            group.trunk = Some(display.clone());
        }
        if let Some(branch) = entry.branch.as_ref().filter(|branch| !branch.is_empty()) {
            group.label = branch.clone();
        }
        if let Some(clean) = entry.clean {
            group.clean = Some(clean);
        }
        group.landed = entry.landed;
        group.trunk_sync = display_trunk
            .as_deref()
            .and_then(|trunk| classify_trunk_sync(&entry, &group.label, trunk));
    }
}

pub fn project_pr_state_map(
    snapshot: &mut SidebarSnapshot,
    states: &BTreeMap<String, WorktreePrState>,
) {
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
        group.pr_state = states.get(&path).copied();
    }
}

pub(crate) fn project_cached_pr_states(snapshot: &mut SidebarSnapshot, runtime: &RuntimePaths) {
    let cache = read_pr_state_cache(&runtime.pr_state_path());
    project_pr_state_map(snapshot, &cache.states);
}

pub(crate) fn classify_trunk_sync(
    entry: &DiffStatsCacheEntry,
    label: &str,
    trunk_display: &str,
) -> Option<WorktreeTrunkSync> {
    if label == trunk_display {
        return None;
    }
    if entry.merge_in_progress == Some(true) {
        return Some(WorktreeTrunkSync::Reconciling);
    }
    if entry.clean == Some(true) && entry.landed == Some(true) && entry.did_work == Some(true) {
        return Some(WorktreeTrunkSync::Merged);
    }
    if entry.clean == Some(true)
        && entry.did_work == Some(false)
        && entry.commits == Some(0)
        && entry.behind == Some(0)
    {
        return Some(WorktreeTrunkSync::Pristine);
    }
    Some(WorktreeTrunkSync::Diverged)
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

/// Producer-vs-consumer inputs for one fold. The spine stays single: producer
/// flags only gate producer-owned side effects, and heavy lanes are plain data
/// supplied by `sidebar::refresh` or read from published caches.
pub struct FoldOpts<'a> {
    pub producing: bool,
    pub fresh_roots: Option<Vec<PathBuf>>,
    pub config: Option<Arc<crate::config::MachineConfig>>,
    pub lanes: Option<&'a crate::sidebar::refresh::RefreshedLanes>,
}

#[derive(Debug, Serialize)]
struct ProducerBindingFallbackLog<'a> {
    event: &'static str,
    at: Timestamp,
    workspace_id: &'a WorkspaceId,
    #[serde(flatten)]
    pairing: &'a LazyAgentPairingDiagnostic,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LazyPairingLogKey {
    workspace_id: WorkspaceId,
    kind: AgentKind,
    agent_id: AgentSessionId,
}

static LOGGED_LAZY_PAIRINGS: OnceLock<Mutex<HashMap<LazyPairingLogKey, u64>>> = OnceLock::new();

/// Fold the enrichments onto a base snapshot — one ordered spine for the
/// producer and every consumer, so the two paths can never drift. `frame` is
/// the live pane frame (panes plus the observed-or-produced pane stamp): the
/// producer's freshly resolved list, or the published `snapshot.json` a
/// consumer read back. `None` skips the pane overlay — a cold consumer start
/// (no publish yet) or a producer call with no live session — and leaves
/// `worktree_groups` empty while the rollup metadata remains available.
///
/// The fold reads only runtime caches and sidecars unless `opts.lanes` supplies
/// freshly refreshed account, spending, and PR values for projection.
pub fn enrich(
    mut snapshot: SidebarSnapshot,
    frame: Option<&PaneFrame>,
    runtime: &RuntimePaths,
    messages_dir: Option<&Path>,
    exclude: Option<&PaneId>,
    mut opts: FoldOpts<'_>,
    diag: &crate::diag::DiagSink,
) -> SidebarSnapshot {
    let producing = opts.producing;
    let machine_config = opts
        .config
        .take()
        .unwrap_or_else(crate::config::MachineConfig::load_lenient);
    // Attention timing is needed during pane projection, before the full config
    // fold builds provider panels and stamps context severity.
    snapshot.sidebar = machine_config.sidebar.clone();
    snapshot.theme = machine_config.theme.clone();
    snapshot.attention = machine_config.agents.attention;
    fold_link_stats(
        &mut snapshot,
        runtime,
        crate::sidebar::timing::unix_now_ms(),
    );
    let diff_cache = read_diff_stats_cache(&runtime.diff_stats_path());

    // The room's enumerated group roots — a repo room's worktree checkouts, so
    // one parked outside the project root still earns its own pod instead of
    // folding into `external`. Directory rooms get git roots from each
    // git-backed row's resolved worktree during the row fold. The producer
    // passes its fresh enumeration in; a consumer reads the cached one back.
    if let Some(roots) = opts.fresh_roots.take() {
        snapshot = snapshot.with_worktree_roots(roots);
    } else if snapshot.project_root.is_some() {
        snapshot = snapshot.with_worktree_roots(
            diff_cache
                .worktrees
                .as_ref()
                .map(|cache| cache.roots.clone())
                .unwrap_or_default(),
        );
    }
    // The repo's durable worktree home, resolved purely from the project root
    // plus the `[agents.worktree] dir` template — independent of `git worktree list`,
    // so the cockpit spend scope still counts sessions from worktrees cleanup
    // has removed. Both modes derive it the same way, so producer and consumer
    // hash the same scope and read the same workspace-spending cache.
    if let Some(root) = snapshot.project_root.clone() {
        snapshot = snapshot.with_worktree_home(
            crate::worktree::worktree_parent(&root, &machine_config.agents.worktree).ok(),
        );
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
    let episodes = super::unread::UnreadEpisodes::load(runtime);
    let read_marks = super::read_marks::ReadMarks::load_merged(runtime);
    let unread_row_ids = episodes.unread_row_ids(&read_marks);

    // Reap daemon-mode Codex ghosts the app-server no longer holds. The cache
    // refresher publishes the live daemon pids plus `thread/loaded/list` on the
    // reap TTL; the fetch and consumer lanes read that file, so they never scan
    // proc or contact the app-server. The probe itself stays gated on any root
    // daemon-hooked session, so pane-stamped daemon ghosts still trigger a
    // refresh. Best-effort and fail-safe: no daemon process, absent cache, or
    // an untrusted loaded list keeps every session.
    let daemon_inputs = read_codex_daemon_reap(runtime).unwrap_or_default();
    let reap_frame_panes = frame.as_ref().map(|frame| frame.to_pane_refs());
    snapshot.reap_runtime(RuntimeReapInputs {
        daemon_pids: &daemon_inputs.daemon_pids,
        loaded: daemon_inputs.loaded.as_ref(),
        frame_panes: reap_frame_panes.as_deref(),
        exclude_pane: exclude,
    });

    let account_budgets = crate::agents::account_budgets_from_caches(runtime, snapshot.now);
    let resume_messages = read_auto_continue_resume_messages(
        messages_dir,
        &machine_config.resume,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
    );
    let exhausted_resumes = auto_continue::exhausted_parks(
        &snapshot,
        runtime,
        &machine_config.resume,
        &resume_messages,
    );

    if let Some(frame) = frame {
        snapshot.panes_produced_at_ms = Some(frame.produced_at_ms);
        snapshot.panes_observed_at_ms = Some(frame.observed_at_ms);
        snapshot.viewed_panes = frame.viewed_panes.clone();
        snapshot.focused_pane = frame.focused_pane.clone();
        // tmux `client_activity` is the idle signal for local and remote rooms;
        // Zellij self-suppresses idle through an absent `last_input_ms`.
        let idle_threshold_ms = machine_config.sidebar.afk_after_ms();
        snapshot.presence = frame
            .presence
            .map(|sample| SidebarPresence::classify(sample, idle_threshold_ms));
        snapshot.truth_degraded = truth_notice_for_frame(frame);
        if let Some(own) = exclude {
            snapshot.own_view = SidebarOwnView::from_frame(own, frame);
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
        let (next_snapshot, diagnostics) = snapshot.with_admitted_live_panes_and_diagnostics(
            admitted_panes,
            &lazy_pairings,
            Some(&unread_row_ids),
            &account_budgets,
            &exhausted_resumes,
        );
        snapshot = next_snapshot;
        for event in diagnostics {
            diag.emit(event);
        }
        apply_pane_metrics(&mut snapshot, metrics);
    }
    // Per-machine display preferences and the per-provider dashboard are
    // environment, not ledger, so the rollup base carries neither. The
    // producer probes accounts out of band and publishes them alongside its
    // walked spending; a consumer reads both published caches back — never a
    // per-tick fork or a ledger lock. Git rides the same split: the heavy-lane
    // refresher refreshes the per-worktree facts (single-flighted), while
    // consumers and the live fetch worker project the cached ones.
    let lanes = opts.lanes;
    let (mut folded, spending_caches) =
        fold_machine_config(snapshot, runtime, &machine_config, lanes);
    project_diff_stats(&mut folded, &diff_cache);
    if let Some(lanes) = lanes {
        project_pr_state_map(&mut folded, &lanes.pr_states);
    } else {
        project_cached_pr_states(&mut folded, runtime);
    }
    snapshot = folded;
    // The fleet `value_tally` — the JSONL headline / month / trailing-year pile
    // read by the cockpit's headline figure and the bottom value corner — attaches
    // once, after every fold; `None` when nothing has ever been recorded.
    snapshot.value_tally = (!spending_caches.provider.spending.total.is_zero())
        .then_some(spending_caches.provider.spending.total.clone());
    snapshot.workspace_value_tally = (!spending_caches.workspace.tally.is_zero())
        .then_some(spending_caches.workspace.tally.clone());
    // The live overlay rides the same fold: a context sidecar push wakes the
    // consumer, the refold lands the session's fresh cost on its row, and the
    // cockpit's headline retargets in the same frame — no waiting out the
    // walk's TTL.
    apply_live_today_spend(&mut snapshot, &spending_caches.workspace);
    super::unread::derive(&mut snapshot, &episodes, &read_marks);
    // Git facts and late unread bits land after the pane fold's initial sort,
    // so publish the spine once both ranking inputs are present.
    snapshot.sort_groups_for_presentation();
    snapshot
}

fn truth_notice_for_frame(frame: &crate::sidebar::frame::PaneFrame) -> Option<crate::TruthNotice> {
    let since_ms = frame
        .carried_panes
        .iter()
        .map(|pane| pane.carried_since_ms)
        .min()?;
    Some(crate::TruthNotice {
        carried: frame.carried_panes.len(),
        since_ms,
        pane_ids: frame
            .carried_panes
            .iter()
            .map(|pane| pane.pane_id.clone())
            .collect(),
    })
}

fn log_lazy_pairing_ambiguities(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    lazy_pairings: &LazyAgentPairingResult,
) {
    let diagnostics = lazy_pairings.diagnostics();
    let mut active = HashSet::new();
    let mut append = Vec::new();
    if let Ok(mut logged) = LOGGED_LAZY_PAIRINGS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        for pairing in diagnostics {
            let key = lazy_pairing_log_key(snapshot, pairing);
            active.insert(key.clone());
            let Some(signature) = lazy_pairing_signature(pairing) else {
                append.push(pairing);
                continue;
            };
            if logged.get(&key).copied() != Some(signature) {
                logged.insert(key, signature);
                append.push(pairing);
            }
        }
        logged.retain(|key, _| key.workspace_id != snapshot.workspace_id || active.contains(key));
    } else {
        append.extend(diagnostics);
    }

    for pairing in append {
        crate::diag::binding::log(runtime).append(&ProducerBindingFallbackLog {
            event: "producer_lazy_agent_pairing",
            at: Timestamp::now(),
            workspace_id: &snapshot.workspace_id,
            pairing,
        });
    }
}

fn lazy_pairing_log_key(
    snapshot: &SidebarSnapshot,
    pairing: &LazyAgentPairingDiagnostic,
) -> LazyPairingLogKey {
    LazyPairingLogKey {
        workspace_id: snapshot.workspace_id.clone(),
        kind: pairing.kind.clone(),
        agent_id: pairing.agent_id.clone(),
    }
}

fn lazy_pairing_signature(pairing: &LazyAgentPairingDiagnostic) -> Option<u64> {
    let signature = serde_json::json!({
        "kind": &pairing.kind,
        "agent_id": &pairing.agent_id,
        "worktree_path": &pairing.worktree_path,
        "selected_pane": &pairing.selected_pane,
        "selected_pane_process_start": &pairing.selected_pane_process_start,
        "method": &pairing.method,
        "candidates": &pairing.candidates,
    });
    let bytes = serde_json::to_vec(&signature).ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
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

/// Fold the per-machine config and dashboard onto a snapshot. Without supplied
/// lanes, it reads the producer's published `accounts.json` and spending
/// caches. With supplied lanes, it projects the freshly refreshed values for
/// the final producer snapshot without re-reading those cache files.
fn fold_machine_config(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &crate::config::MachineConfig,
    lanes: Option<&crate::sidebar::refresh::RefreshedLanes>,
) -> (SidebarSnapshot, SpendingCaches) {
    let accounts_config = config.accounts.clone();
    let (accounts, spending) = if let Some(lanes) = lanes {
        (lanes.accounts.clone(), lanes.spending.clone())
    } else {
        let accounts = cached_accounts_for_snapshot(
            read_accounts_cache(&runtime.shared_accounts_path()),
            &snapshot,
        );
        // Fold the producer's published spending cache rather than re-walking
        // JSONL transcript history here.
        let spending = super::refresh::spending::consumer_spending_caches(
            runtime,
            &snapshot,
            &config.headline_spec(),
        );
        (accounts, spending)
    };
    let mut snapshot = fold_machine_config_with(
        snapshot,
        config,
        accounts,
        &spending.provider.spending.by_provider,
    );
    // Every fold merges the producer-published account windows read-only. The
    // refresh lane owns writes.
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    apply_credits_cache(&mut snapshot, runtime, &accounts_config);
    (snapshot, spending)
}

/// Apply the resolved config and already-resolved accounts onto the snapshot:
/// the per-provider `⇅ rc` flags, the dashboard aggregates, and each agent
/// row's context-severity verdict.
pub(crate) fn fold_machine_config_with(
    mut snapshot: SidebarSnapshot,
    config: &crate::config::MachineConfig,
    accounts: BTreeMap<String, crate::agents::AgentAccount>,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
) -> SidebarSnapshot {
    snapshot.sidebar = config.sidebar.clone();
    snapshot.theme = config.theme.clone();

    // Stamp each agent row's context-severity verdict now that the
    // `[theme.display.context_meter]` bands are known — classified once here, on both the
    // producer and consumer fold, so the renderer's color ramp and any future
    // signal emitter read one authority instead of re-deriving the tier.
    let bands = snapshot.theme.display.context_meter.clone();
    stamp_context_severity(&mut snapshot.worktree_groups, &bands);

    // The `⇅ rc` flag per provider comes from either Rimz's auto-launch toggle
    // or the provider's own pane-session auto-enable setting.
    let mut remote_control_flags: BTreeMap<String, bool> = BTreeMap::new();
    for adapter in crate::agents::ADAPTERS {
        let descriptor = adapter.descriptor();
        let config_toggle = config.remote_control.enabled_for(descriptor.kind);
        let pane_auto = descriptor.capabilities.remote_control.pane_sessions
            && adapter
                .remote_control_status(accounts.get(descriptor.kind))
                .pane_auto;
        remote_control_flags.insert(descriptor.kind.to_owned(), config_toggle || pane_auto);
    }

    snapshot.with_provider_aggregates(&accounts, &remote_control_flags, provider_spending)
}

/// Stamp [`SidebarRow::context_severity`] on every agent row from the
/// `[theme.display.context_meter]` bands: [`crate::agents::ContextSeverity::classify`] over
/// the row's gauge inputs, the one verdict the renderer's color ramp and any
/// future signal emitter read. Process rows carry no context and stay `None`.
pub(crate) fn stamp_context_severity(
    groups: &mut [crate::SidebarWorktreeGroup],
    bands: &crate::config::ContextMeterConfig,
) {
    for group in groups {
        for row in &mut group.rows {
            if row.is_agent() {
                let severity = crate::agents::ContextSeverity::classify(
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
