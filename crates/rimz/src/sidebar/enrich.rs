//! Shared sidebar projection fold over a store rollup and optional pane frame.
//!
//! One ordered spine serves both producer and consumer reads. It projects lane
//! caches and sidecars; it forks no subprocess and writes no cache files.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::RuntimePaths;
use crate::agents::spending::SpendingCaches;
use crate::harness::auto_continue::{self, ResumeMessage};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, RemoteControlBadge, ResumeOutcome,
    RuntimeReapInputs, SidebarLinkFreshness, SidebarLinkHealth, SidebarOwnView, SidebarPresence,
    SidebarProviderPanel, SidebarRow, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
    TruthNotice, WorktreePrCi, WorktreePrState, WorktreeTrunkSync, compute_lazy_agent_pairings,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::frame::{PaneFrame, PaneMetrics};
use super::refresh::accounts::{cached_accounts_for_snapshot, read_accounts_cache};
use super::refresh::cohort_spend::{CohortSpendCache, read_cohort_spend_cache};
use super::refresh::credits::apply_credits_cache;
use super::refresh::daemon_reap::read_codex_daemon_reap;
use super::refresh::git_stats::{
    DiffStatsCache, DiffStatsCacheEntry, is_trunk_branch, read_diff_stats_cache,
    worktree_group_path_fields,
};
use super::refresh::live_spend::{apply_live_day_spend, apply_live_today_spend};
use super::refresh::pr::{PrLink, read_pr_state_cache};
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
/// settles the shared-worktree "whose branch?" ambiguity. The producer
/// validates checkout paths before refreshing their entries and publishes
/// exact channel marker classifications; consumers project that cache without
/// git subprocesses or checkout metadata reads.
pub fn project_diff_stats(snapshot: &mut SidebarSnapshot, cache: &DiffStatsCache) {
    for group in &mut snapshot.worktree_groups {
        if group.kind == SidebarWorktreeKind::Channel {
            group.worktree_backed = false;
        }
        let Some(path) = cached_git_backed_worktree_path(
            group.kind,
            &group.label,
            &group.key,
            &group.rows,
            cache,
        ) else {
            continue;
        };
        if group.kind == SidebarWorktreeKind::Channel {
            group.worktree_backed = true;
        }
        let Some(entry) = cache.entries.get(path) else {
            continue;
        };
        group.pr_number = entry.from_pr;
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
        let branch_label = entry
            .branch
            .as_deref()
            .filter(|branch| !branch.is_empty())
            .unwrap_or(&group.label)
            .to_owned();
        if group.kind == SidebarWorktreeKind::Worktree
            && let Some(branch) = entry.branch.as_ref().filter(|branch| !branch.is_empty())
        {
            group.label = branch.clone();
        }
        if let Some(clean) = entry.clean {
            group.clean = Some(clean);
        }
        group.landed = entry.landed;
        group.trunk_sync = display_trunk
            .as_deref()
            .and_then(|trunk| classify_trunk_sync(entry, &branch_label, trunk));
    }
}

/// Qualify groups whose final display labels collide with the shortest
/// distinguishing trailing checkout path.
pub fn disambiguate_group_labels(snapshot: &mut SidebarSnapshot) {
    let mut by_label = BTreeMap::<String, Vec<(usize, Vec<String>, usize)>>::new();

    for (index, group) in snapshot.worktree_groups.iter_mut().enumerate() {
        group.label_qualifier = None;
        if group.kind == SidebarWorktreeKind::Channel {
            continue;
        }
        let Some(path) = worktree_group_path_fields(&group.key, &group.rows) else {
            continue;
        };
        let components = Path::new(path)
            .components()
            .filter(|component| !matches!(component, std::path::Component::RootDir))
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if !components.is_empty() {
            by_label
                .entry(group.label.clone())
                .or_default()
                .push((index, components, 1));
        }
    }

    for (label, mut candidates) in by_label
        .into_iter()
        .filter(|(_, candidates)| candidates.len() > 1)
    {
        let mut path_counts = BTreeMap::<Vec<String>, usize>::new();
        for (_, components, _) in &candidates {
            *path_counts.entry(components.clone()).or_default() += 1;
        }
        loop {
            let mut by_suffix = BTreeMap::<String, Vec<usize>>::new();
            for (candidate, (_, components, depth)) in candidates.iter().enumerate() {
                by_suffix
                    .entry(trailing_path_suffix(components, *depth))
                    .or_default()
                    .push(candidate);
            }
            let mut deepened = false;
            for collisions in by_suffix.values().filter(|indices| indices.len() > 1) {
                for &candidate in collisions {
                    let (_, components, depth) = &mut candidates[candidate];
                    if *depth < components.len() {
                        *depth += 1;
                        deepened = true;
                    }
                }
            }
            if !deepened {
                break;
            }
        }

        for (index, components, depth) in candidates {
            if path_counts.get(&components).copied().unwrap_or_default() > 1 {
                continue;
            }
            let suffix = trailing_path_suffix(&components, depth);
            if suffix != label {
                snapshot.worktree_groups[index].label_qualifier = Some(suffix);
            }
        }
    }
}

fn trailing_path_suffix(components: &[String], depth: usize) -> String {
    components[components.len().saturating_sub(depth)..].join("/")
}

pub fn project_cohort_effort(snapshot: &mut SidebarSnapshot, cache: &CohortSpendCache) {
    for group in &mut snapshot.worktree_groups {
        group.cohort_effort = cache.groups.get(&group.key).cloned();
    }
}

fn cached_git_backed_worktree_path<'a>(
    kind: SidebarWorktreeKind,
    label: &str,
    key: &'a str,
    rows: &'a [SidebarRow],
    cache: &DiffStatsCache,
) -> Option<&'a str> {
    let path = worktree_group_path_fields(key, rows)?;
    match kind {
        SidebarWorktreeKind::Worktree => Some(path),
        SidebarWorktreeKind::Channel => cache
            .worktrees
            .as_ref()?
            .marker_names
            .as_ref()?
            .get(Path::new(path))
            .is_some_and(|name| name == label)
            .then_some(path),
        SidebarWorktreeKind::Root | SidebarWorktreeKind::External => None,
    }
}

pub fn project_pr_state_map(
    snapshot: &mut SidebarSnapshot,
    states: &BTreeMap<String, PrLink>,
    branch_ci: &BTreeMap<String, WorktreePrCi>,
    diff_cache: &DiffStatsCache,
) {
    for group in &mut snapshot.worktree_groups {
        let Some(path) = cached_git_backed_worktree_path(
            group.kind,
            &group.label,
            &group.key,
            &group.rows,
            diff_cache,
        ) else {
            continue;
        };
        let trunk = diff_cache.entries.get(path).is_some_and(|entry| {
            entry
                .branch
                .as_deref()
                .is_some_and(|branch| is_trunk_branch(branch, entry.trunk.as_deref()))
        });
        if trunk {
            group.pr_state = None;
            group.pr_ci = branch_ci.get(path).copied();
            group.pr_number = None;
            group.pr_url = None;
            continue;
        }
        let link = states.get(path);
        group.pr_state = link.map(|link| link.state);
        group.pr_ci = match link {
            Some(link) if matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged) => {
                link.ci
            }
            Some(_) => None,
            None => branch_ci.get(path).copied(),
        };
        group.pr_url = None;
        if let Some(link) = link
            && let Some(number) = link.number
        {
            group.pr_number = Some(number);
            group.pr_url = link.url.clone();
        }
    }
}

pub(crate) fn project_cached_pr_states(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    diff_cache: &DiffStatsCache,
) {
    let cache = read_pr_state_cache(&runtime.pr_state_path());
    project_pr_state_map(snapshot, &cache.states, &cache.branch_ci, diff_cache);
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

/// Producer-vs-consumer inputs for one fold. The spine stays single: producer
/// flags only gate producer-owned side effects, and heavy lanes are plain data
/// supplied by `sidebar::refresh` or read from published caches.
pub struct FoldOpts<'a> {
    pub producing: bool,
    pub fresh_roots: Option<Vec<PathBuf>>,
    pub config: Option<Arc<crate::config::MachineConfig>>,
    pub lanes: Option<&'a crate::sidebar::refresh::RefreshedLanes>,
    pub agent_projection: crate::sidebar::agent_projection::AgentProjection,
}

/// Probed managed-server liveness for the rc badge. `None` means no probe was
/// available this tick (no pane frame or no reap cache yet).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RemoteControlServerHealth {
    /// A managed host pane exists *and* the provider's record shows it still
    /// serving. Pane presence alone outlives the child that answers for it.
    pub claude_host_serving: Option<bool>,
    pub codex_daemon_alive: Option<bool>,
}

/// Whether the Claude host counts as serving. The managed pane must exist, and
/// an enabled host must not have left a record saying its server died — a pane
/// outlives the child that answers for it, so presence alone reads healthy long
/// after the host stopped working. No record keeps the answer positive.
pub(crate) fn claude_host_serving(
    pane_present: bool,
    enabled: bool,
    liveness: crate::agents::runtime_control::RuntimeControlLiveness,
) -> bool {
    pane_present && !(enabled && liveness.is_down())
}

impl RemoteControlServerHealth {
    fn for_kind(self, kind: &str) -> Option<bool> {
        match kind {
            "claude" => self.claude_host_serving,
            "codex" => self.codex_daemon_alive,
            _ => None,
        }
    }
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

/// Renderer-independent sidebar projection. Call [`project_local`] before a
/// snapshot reaches rendering or notification evaluation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceSnapshot(pub(crate) SidebarSnapshot);

impl WorkspaceSnapshot {
    pub(crate) fn snapshot(&self) -> &SidebarSnapshot {
        &self.0
    }
}

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
    snapshot: SidebarSnapshot,
    frame: Option<&PaneFrame>,
    runtime: &RuntimePaths,
    messages_dir: Option<&Path>,
    exclude: Option<&PaneId>,
    opts: FoldOpts<'_>,
    diag: &crate::diag::DiagSink,
) -> SidebarSnapshot {
    project_local(
        enrich_workspace(snapshot, frame, runtime, messages_dir, opts, diag),
        frame,
        exclude,
    )
}

pub fn enrich_workspace(
    snapshot: SidebarSnapshot,
    frame: Option<&PaneFrame>,
    runtime: &RuntimePaths,
    messages_dir: Option<&Path>,
    opts: FoldOpts<'_>,
    diag: &crate::diag::DiagSink,
) -> WorkspaceSnapshot {
    WorkspaceSnapshot(enrich_core(
        snapshot,
        frame,
        runtime,
        messages_dir,
        opts,
        diag,
    ))
}

/// Apply the renderer-owned pane exclusion, own-view, and presence verdict.
pub fn project_local(
    workspace: WorkspaceSnapshot,
    frame: Option<&PaneFrame>,
    exclude: Option<&PaneId>,
) -> SidebarSnapshot {
    let mut snapshot = workspace.0;
    let Some(frame) = frame else {
        snapshot.presence = None;
        snapshot.own_view = None;
        return snapshot;
    };

    let idle_threshold_ms = snapshot.sidebar.afk_after_ms();
    let now_ms = snapshot.now.as_millisecond().max(0) as u64;
    snapshot.presence = frame
        .presence
        .map(|sample| SidebarPresence::classify(sample, now_ms, idle_threshold_ms));
    snapshot.own_view = exclude.and_then(|own| SidebarOwnView::from_frame(own, frame));
    if let Some(excluded) = exclude {
        snapshot
            .agent_panes
            .retain(|agent| agent.pane_id != *excluded);
        // `remove_pane_rows` also clears the session focus register when the
        // excluded pane owns it. Exclusion has always hidden only the row, so
        // retain the pre-exclusion focus truth for fusion and watched-state.
        let focused_pane = snapshot.focused_pane.clone();
        snapshot.remove_pane_rows(excluded);
        snapshot.focused_pane = focused_pane;
        snapshot.sort_groups_for_presentation();
    }
    snapshot
}

fn enrich_core(
    mut snapshot: SidebarSnapshot,
    frame: Option<&PaneFrame>,
    runtime: &RuntimePaths,
    messages_dir: Option<&Path>,
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
    let cohort_spend_cache = read_cohort_spend_cache(&runtime.cohort_spend_path());

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

    // Wiring state gates the live-pane fold (idle synthesis), so set it before
    // any pane-backed projection.
    snapshot.wired_kinds = opts.agent_projection.wiring.kinds;
    snapshot.wired_default_models = opts.agent_projection.wiring.default_models;

    // Bind caller-supplied provider-local observations before context/activity
    // enrichment. Discovery belongs to the room producer; every renderer keeps
    // the strict live-pane binding and discards paneless observations.
    if let Some(frame) = frame {
        let panes = SidebarSnapshot::card_admitted_live_panes(frame.to_pane_refs(), None);
        let (next_snapshot, diagnostics) = snapshot
            .with_local_sessions_and_diagnostics(&panes, opts.agent_projection.local_sessions);
        snapshot = next_snapshot;
        for event in diagnostics {
            diag.emit(event);
        }
    }

    // Fold each session's rich statusline context onto its agent state
    // (read-only; CLI producers write it). Both the context sidecar
    // and the per-tool activity heartbeats fold only onto existing agents, so
    // an empty room skips both directory scans — the common idle case.
    // Activity lands before the pane overlay so age, ranking, waiting guards,
    // and the stall window all see the truer per-tool value rather than
    // the turn-grained event timestamp.
    if !snapshot.agents.is_empty() {
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(runtime));
        snapshot =
            snapshot.with_subagent_context(crate::store::subagent_context::read_all(runtime));
        let activity = crate::agent_activity::read_for_keys(
            runtime,
            snapshot
                .agents
                .iter()
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
        );
        snapshot = snapshot.with_agent_activity(&activity);
        let active_time = crate::store::active_time::read_for_keys(
            runtime,
            snapshot
                .agents
                .iter()
                .filter(|agent| !agent.is_provider_subagent())
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
        );
        snapshot = snapshot.with_active_time(&active_time);
        crate::harness::budget::project_parks(&mut snapshot, runtime, &machine_config);
    }

    let episodes = super::unread::UnreadEpisodes::load(runtime);
    let read_marks = super::read_marks::ReadMarks::load_merged(runtime);
    let unread_row_ids = episodes.unread_row_ids(&read_marks);

    // Reap daemon-mode Codex ghosts the app-server no longer holds. The cache
    // refresher publishes the live daemon pids plus `thread/loaded/list` on the
    // reap TTL; the fetch and consumer lanes read that file, so they never scan
    // proc or contact the app-server. The probe runs for any root daemon-hooked
    // session or while the Codex rc toggle needs its health signal, so
    // pane-stamped daemon ghosts and a session-less rc host both refresh it.
    // Best-effort and fail-safe: no daemon process, absent cache, or an
    // untrusted loaded list keeps every session.
    let daemon_inputs = read_codex_daemon_reap(runtime);
    // A managed pane keeps the joined argv as its title for its whole life, so
    // pane presence alone cannot see a host whose child stopped serving. The
    // provider's own record of the serving process settles it; a host with no
    // record stays healthy, because absence of evidence is not a failure.
    let claude_rc_enabled = machine_config.remote_control.enabled_for("claude");
    let remote_control_health = RemoteControlServerHealth {
        claude_host_serving: frame.map(|frame| {
            let pane_present = crate::daemon_view::claude_host_present(&frame.to_pane_refs());
            // Probe only behind a live pane on an enabled host: a disabled or
            // paneless host has nothing the record could contradict.
            let liveness = match snapshot.project_root.as_deref() {
                Some(root) if claude_rc_enabled && pane_present => {
                    crate::agents::runtime_control::host_liveness("claude", root)
                }
                _ => crate::agents::runtime_control::RuntimeControlLiveness::Unknown,
            };
            claude_host_serving(pane_present, claude_rc_enabled, liveness)
        }),
        codex_daemon_alive: daemon_inputs
            .as_ref()
            .map(|inputs| !inputs.daemon_pids.is_empty()),
    };
    let daemon_inputs = daemon_inputs.unwrap_or_default();
    let reap_frame_panes = frame.as_ref().map(|frame| frame.to_pane_refs());
    snapshot.reap_runtime(RuntimeReapInputs {
        daemon_pids: &daemon_inputs.daemon_pids,
        loaded: daemon_inputs.loaded.as_ref(),
        frame_panes: reap_frame_panes.as_deref(),
        exclude_pane: None,
    });

    let provider_capacities = crate::agents::ProviderCapacity::read_all(runtime);
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
        snapshot.client_views = frame.client_views.clone();
        snapshot.pane_session_name = Some(frame.session_name.clone());
        snapshot.focused_pane = frame.focused_pane.clone();
        snapshot.truth_degraded = truth_notice_for_frame(frame);
        let metrics = frame.pane_metrics().collect::<Vec<_>>();
        let panes = frame.to_pane_refs();
        let admitted_panes = SidebarSnapshot::card_admitted_live_panes(panes.clone(), None);
        let lazy_pairings = compute_lazy_agent_pairings(&admitted_panes, &snapshot.agents);
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
            &provider_capacities,
            &exhausted_resumes,
        );
        snapshot = next_snapshot;
        for event in diagnostics {
            diag.emit(event);
        }
        apply_pane_metrics(&mut snapshot, metrics);
    }
    // Per-machine display preferences and the per-provider dashboard are
    // environment, not store, so the rollup base carries neither. The
    // producer probes accounts out of band and publishes them alongside its
    // walked spending; a consumer reads both published caches back — never a
    // per-tick fork or a store lock. Git rides the same split: the heavy-lane
    // refresher refreshes the per-worktree facts (single-flighted), while
    // consumers and the live fetch worker project the cached ones.
    let lanes = opts.lanes;
    let (mut folded, spending_caches) = fold_machine_config(
        snapshot,
        runtime,
        &machine_config,
        remote_control_health,
        lanes,
    );
    project_diff_stats(&mut folded, &diff_cache);
    disambiguate_group_labels(&mut folded);
    project_cohort_effort(&mut folded, &cohort_spend_cache);
    if let Some(lanes) = lanes {
        project_pr_state_map(&mut folded, &lanes.pr_states, &lanes.branch_ci, &diff_cache);
    } else {
        project_cached_pr_states(&mut folded, runtime, &diff_cache);
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
    apply_live_day_spend(&mut snapshot, &spending_caches.workspace);
    crate::harness::budget::project_budget_views(
        &mut snapshot,
        runtime,
        &machine_config,
        &spending_caches.provider,
    );
    super::unread::derive(&mut snapshot, &episodes, &read_marks);
    // Git facts and late unread bits land after the pane fold's initial sort,
    // so publish the spine once both ranking inputs are present.
    snapshot.sort_groups_for_presentation();
    snapshot
}

fn truth_notice_for_frame(frame: &crate::sidebar::frame::PaneFrame) -> Option<TruthNotice> {
    let since_ms = frame
        .carried_panes
        .iter()
        .map(|pane| pane.carried_since_ms)
        .min()?;
    Some(TruthNotice {
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
    remote_control_health: RemoteControlServerHealth,
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
        // Consumers read producer publications only. A missing workspace
        // sidecar stays absent until the elected producer supplies it.
        let spending = super::refresh::consumer_spending_caches(runtime, &snapshot);
        (accounts, spending)
    };
    let mut snapshot = fold_machine_config_with(
        snapshot,
        config,
        accounts,
        &spending.provider.spending.by_provider,
        remote_control_health,
    );
    // Every fold merges the producer-published account windows read-only. The
    // refresh lane owns writes.
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    apply_credits_cache(&mut snapshot, runtime, &accounts_config);
    (snapshot, spending)
}

/// Build the provider-dashboard projection from user-scoped published caches
/// without reading a room snapshot or starting a spending walk.
pub fn provider_panels_from_caches(
    runtime: &RuntimePaths,
    config: &crate::config::MachineConfig,
    accounts: BTreeMap<String, crate::agents::AgentAccount>,
    provider_spending: &crate::agents::spending::ProviderSpendingCache,
) -> Vec<SidebarProviderPanel> {
    let now = Timestamp::now();
    let mut snapshot =
        SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), Vec::new(), now);
    let mut config = config.clone();
    // The query surface needs every qualifying provider while retaining the
    // dashboard's usage ranking; local display caps and explicit tabs only
    // control the sidebar's screen layout.
    config.theme.display.provider_list = vec!["all".to_owned()];
    snapshot = fold_machine_config_with(
        snapshot,
        &config,
        accounts,
        &provider_spending.spending.by_provider,
        RemoteControlServerHealth::default(),
    );
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    apply_credits_cache(&mut snapshot, runtime, &config.accounts);
    crate::harness::budget::project_budget_views(
        &mut snapshot,
        runtime,
        &config,
        provider_spending,
    );
    snapshot.providers
}

/// Apply the resolved config and already-resolved accounts onto the snapshot:
/// the per-provider `⇅ rc` flags, the dashboard aggregates, and each agent
/// row's context-severity verdict.
pub(crate) fn fold_machine_config_with(
    mut snapshot: SidebarSnapshot,
    config: &crate::config::MachineConfig,
    accounts: BTreeMap<String, crate::agents::AgentAccount>,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
    remote_control_health: RemoteControlServerHealth,
) -> SidebarSnapshot {
    snapshot.sidebar = config.sidebar.clone();
    snapshot.theme = config.theme.clone();

    // Stamp each agent row's context-severity verdict now that the
    // `[theme.display.context_meter]` bands are known — classified once here, on both the
    // producer and consumer fold, so the renderer's color ramp and any future
    // signal emitter read one authority instead of re-deriving the tier.
    let bands = snapshot.theme.display.context_meter.clone();
    stamp_context_severity(&mut snapshot.worktree_groups, &bands);

    // The `⇅ rc` flag per provider comes from either RimZ's auto-launch toggle
    // or the provider's own pane-session auto-enable setting.
    let mut remote_control_flags: BTreeMap<String, RemoteControlBadge> = BTreeMap::new();
    for adapter in crate::agents::all_definitions() {
        let definition = adapter.spec();
        let config_toggle = config.remote_control.enabled_for(definition.kind);
        let pane_auto = definition.capabilities.remote_control.pane_sessions
            && adapter
                .remote_control_status(accounts.get(definition.kind))
                .pane_auto;
        remote_control_flags.insert(
            definition.kind.to_owned(),
            remote_control_badge(
                config_toggle,
                pane_auto,
                remote_control_health.for_kind(definition.kind),
            ),
        );
    }

    snapshot.with_provider_aggregates(&accounts, &remote_control_flags, provider_spending)
}

/// Derive the rc badge from enablement and the managed-server probe. An absent
/// probe stays healthy to avoid a false red flash before the first pane frame or
/// reap-cache publish; only a configured server positively observed down is red.
fn remote_control_badge(
    config_toggle: bool,
    pane_auto: bool,
    server_alive: Option<bool>,
) -> RemoteControlBadge {
    if !config_toggle && !pane_auto {
        RemoteControlBadge::Hidden
    } else if config_toggle && server_alive == Some(false) {
        RemoteControlBadge::Down
    } else {
        RemoteControlBadge::Healthy
    }
}

/// Stamp [`SidebarRow::context_severity`] on every agent row from the
/// `[theme.display.context_meter]` bands: [`crate::agents::ContextSeverity::classify`] over
/// the row's gauge inputs, the one verdict the renderer's color ramp and any
/// future signal emitter read. Process rows carry no context and stay `None`.
pub(crate) fn stamp_context_severity(
    groups: &mut [SidebarWorktreeGroup],
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
