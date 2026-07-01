//! Shared sidebar enrichment fold over a ledger rollup and an optional pane frame.
//!
//! One ordered spine serves both producer and consumer reads. Producer-only work
//! arrives through [`EnrichMode::Producing`]; consumer reads project published
//! runtime caches and sidecars only.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use crate::agents::spending::{
    HeadlineSpec, ProviderSpendingCache, SpendScope, SpendingCaches, WorkspaceSpendingCache,
    compute_scoped_tally, discover_spending_files, read_provider_spending_cache,
    read_spending_cache, read_workspace_spending_cache, unix_secs_now,
};
use crate::agents::{AccountBudget, AgentStatus};
use crate::ids::AgentKind;
use crate::ids::{PaneId, WorkspaceId};
use crate::ledger::snapshot::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, SidebarPresence,
};
use crate::{
    RuntimePaths, SidebarLinkFreshness, SidebarLinkHealth, SidebarOwnView, SidebarSnapshot,
    SidebarWorktreeGroup, SidebarWorktreeKind, WorktreePrState, WorktreeTrunkSync,
};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::cache::{
    CodexDaemonReap, DiffStatsCache, GIT_ACTIVITY_WINDOW, PrStateCache, read_codex_daemon_reap,
    read_diff_stats_cache, unix_now_ms, write_codex_daemon_reap,
};
use super::frame::{PaneFrame, PaneMetrics};
use super::timing::{LINK_STATS_EXPIRE, LINK_STATS_STALE};

mod accounts;
mod auto_continue;
mod codex_refresh;
mod credits;
mod forge;
mod live_spend;
mod rate_limits;
#[cfg(test)]
mod tests;
mod usage_refresh;

pub(crate) use auto_continue::ResumeMessage;
pub use codex_refresh::refresh_codex_transcript_context;
pub(crate) use credits::apply_credits_cache;
pub use credits::{
    CreditsCache, ProviderCreditsEntry, merge_provider_credits,
    merge_provider_credits_entry_if_due, provider_credits_entry_fresh,
};
pub use live_spend::{apply_live_today_spend, live_row_costs};
pub(crate) use rate_limits::apply_rate_limit_cache;
pub use rate_limits::{RateLimitsCache, merge_account_rate_limits, shortest_window_running};
pub use usage_refresh::merge_oauth_usage_if_due;

use accounts::{cached_accounts_for_snapshot, produce_accounts, read_accounts_cache};
use codex_refresh::refresh_codex_sessions;
use forge::{produce_pr_states, read_pr_state_cache};
use live_spend::refresh_live_spend_baselines;
use usage_refresh::refresh_account_usage;

fn account_budgets_from_caches(
    runtime: &RuntimePaths,
    now: Timestamp,
) -> BTreeMap<AgentKind, AccountBudget> {
    let rate_limits = rate_limits::read_rate_limits_cache(&runtime.shared_rate_limits_path());
    rate_limits
        .windows
        .into_iter()
        .map(|(kind, limits)| {
            (
                AgentKind::new_unchecked(kind),
                AccountBudget {
                    windows: limits
                        .windows
                        .into_iter()
                        .map(|window| window.projected_at(now))
                        .collect(),
                },
            )
        })
        .collect()
}

/// Whether a hidden resume-gated message may enter a paused agent now. The
/// check stays beside the account-budget cache reader so CLI delivery can use
/// the same fused budget projection as the sidebar producer without exposing
/// the cache shape.
pub fn resume_gate_recovered(
    runtime: &RuntimePaths,
    agent: &crate::agents::AgentState,
    now: Timestamp,
) -> bool {
    use crate::agents::{ResumeArm, TurnErrorClass, effective_turn_error_class, resume_park};

    if agent.effective_status() != AgentStatus::Paused {
        return false;
    }
    let account_budgets = account_budgets_from_caches(runtime, now);
    let budget = account_budgets.get(&agent.kind);
    match resume_park(agent, budget, now) {
        Some(ResumeArm::Overloaded { .. }) => true,
        Some(ResumeArm::RateLimit { .. }) => {
            // A still-spent window is not recovered; the recovered-budget path
            // is the `None` arm below.
            false
        }
        None => crate::agents::display_turn_error(
            agent.status,
            agent.context.as_ref(),
            agent.last_activity,
            agent.turn_started_at,
        )
        .map(effective_turn_error_class)
        .is_some_and(|class| {
            matches!(
                class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
            ) && budget.is_some_and(|budget| budget.subscription_budget_available(now))
        }),
    }
}

/// Build a detached `rimz` helper command for the sidebar producer, anchored to
/// Rimz-owned shared storage so a deleted launch CWD cannot ENOENT the spawn.
pub(super) fn detached_rimz_command(exe: PathBuf, runtime: &RuntimePaths) -> Command {
    let mut cmd = Command::new(exe);
    cmd.current_dir(&runtime.shared_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
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

pub(crate) fn read_auto_continue_resume_messages(
    runtime: &RuntimePaths,
    config: &crate::config::ResumeConfig,
) -> Vec<ResumeMessage> {
    if config.auto_continue {
        auto_continue::read_resume_messages(runtime)
    } else {
        Vec::new()
    }
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

/// The worktree paths whose edit-sensitive git facts refresh on the focused
/// tier: a `Worktree`-kind group is focused when any rendered row is bound to a
/// pane attached clients are currently viewing. Derived with the same path
/// recovery and live-dir gate as [`needed_worktree_paths`], so focused is a
/// subset of needed by construction.
pub fn focused_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let viewed: HashSet<&PaneId> = snapshot.viewed_panes.iter().collect();
    let mut focused = BTreeSet::new();
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
        if group.rows.iter().any(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| viewed.contains(&pane.pane_id))
        }) {
            focused.insert(path.to_owned());
        }
    }
    focused
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

pub fn project_pr_states(snapshot: &mut SidebarSnapshot, cache: &PrStateCache) {
    project_pr_state_map(snapshot, &cache.states);
}

pub(crate) fn project_cached_pr_states(snapshot: &mut SidebarSnapshot, runtime: &RuntimePaths) {
    let cache = read_pr_state_cache(&runtime.root.join("pr-state.json"));
    project_pr_states(snapshot, &cache);
}

pub(crate) fn classify_trunk_sync(
    entry: &super::cache::DiffStatsCacheEntry,
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
        /// How the heavyweight producer caches enter this fold.
        heavy: HeavyLanes<'a>,
        /// The per-machine config, loaded once by the caller. Boxed to keep
        /// the enum the size of its `Cached` common case.
        config: Box<crate::config::MachineConfig>,
    },
}

pub enum HeavyLanes<'a> {
    /// Fork + publish the heavy caches inline.
    Refresh {
        /// The fleet spending walk. The shared publish is account-global;
        /// per-workspace live-cost baselines are refreshed by the fold after
        /// the walk cache is available and rows hold their latest context.
        compute_spending: &'a dyn Fn(&SidebarSnapshot) -> SpendingCaches,
        /// The per-worktree git refresh over the snapshot's groups.
        refresh_git: &'a dyn Fn(&mut SidebarSnapshot),
    },
    /// Project the last cache refresh without producing stale lanes inline.
    Project,
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
/// the live pane frame (panes plus the observed-or-produced pane stamp): the
/// producer's freshly resolved list, or the published `snapshot.json` a
/// consumer read back. `None` skips the pane overlay — a cold consumer start
/// (no publish yet) or a producer call with no live session — and leaves
/// `worktree_groups` empty while the rollup metadata remains available.
///
/// [`EnrichMode::Cached`] reads only runtime caches and sidecars;
/// [`EnrichMode::Producing`] carries the producer inputs in the mode and
/// inserts the daemon reap, pane/root producer work, and either heavy-lane
/// refresh or heavy-lane projection at their named points.
pub fn enrich(
    mut snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    mut mode: EnrichMode<'_>,
    diag: Option<&crate::diag::DiagSink>,
) -> SidebarSnapshot {
    let producing = matches!(mode, EnrichMode::Producing { .. });
    let machine_config = match &mode {
        EnrichMode::Cached => crate::config::MachineConfig::load().unwrap_or_default(),
        EnrichMode::Producing { config, .. } => (**config).clone(),
    };
    // Attention timing is needed during pane projection, before the full config
    // fold builds provider panels and stamps context severity.
    snapshot.sidebar = machine_config.sidebar.clone();
    snapshot.theme = machine_config.theme.clone();
    snapshot.attention = machine_config.agents.attention;
    fold_link_stats(&mut snapshot, runtime, crate::sidebar::cache::unix_now_ms());

    // The room's enumerated group roots — a repo room's worktree checkouts, so
    // one parked outside the project root still earns its own pod instead of
    // folding into `external`. Directory rooms get git roots from each
    // git-backed row's resolved worktree during the row fold. The producer
    // passes its fresh enumeration in; a consumer reads the cached one back.
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

    // Reap daemon-mode Codex ghosts the app-server no longer holds. The
    // producer publishes the live daemon pids plus `thread/loaded/list`; the
    // cached lane reuses that file, so consumers never scan proc or spawn the
    // app-server. The probe itself stays gated on a pane-less root `codex`
    // session, so the common room pays no proc scan. Best-effort and fail-safe:
    // no daemon process, absent cache, or an untrusted loaded list keeps every
    // session.
    let daemon_inputs = if producing {
        let should_probe = snapshot.agents.iter().any(|agent| {
            agent.kind == "codex" && agent.pane.is_none() && agent.parent_agent_id.is_none()
        });
        let daemon_pids = if should_probe {
            crate::remote_control::codex_daemon_pids()
        } else {
            BTreeSet::new()
        };
        let loaded = if daemon_pids.is_empty() {
            None
        } else {
            crate::agents::codex::loaded_daemon_threads()
        };
        let inputs = CodexDaemonReap {
            produced_at_ms: unix_now_ms(),
            daemon_pids,
            loaded,
        };
        if let Err(err) = write_codex_daemon_reap(runtime, &inputs) {
            tracing::debug!(
                error = %err,
                "codex daemon reap cache write failed"
            );
        }
        inputs
    } else {
        read_codex_daemon_reap(runtime).unwrap_or_default()
    };
    snapshot.drop_dead_daemon_sessions(&daemon_inputs.daemon_pids, daemon_inputs.loaded.as_ref());

    // Codex `/clear` / `/new` starts a fresh session id in the same pane and
    // process. The rollout head lineage is carried on each root, so both lanes
    // can drop only same-live-pane fresh replacements; unknown lineage or
    // forked sessions keep both rows.
    if let Some(frame) = &frame {
        let admitted_panes =
            SidebarSnapshot::card_admitted_live_panes(frame.to_pane_refs(), exclude);
        snapshot.drop_cleared_codex_sessions(&admitted_panes);
    }

    let account_budgets = account_budgets_from_caches(runtime, snapshot.now);
    let resume_messages = read_auto_continue_resume_messages(runtime, &machine_config.resume);
    let exhausted_resumes = auto_continue::exhausted_parks(
        &snapshot,
        runtime,
        &machine_config.resume,
        &resume_messages,
    );

    if let Some(frame) = frame {
        snapshot.panes_produced_at_ms = Some(frame.produced_at_ms);
        snapshot.panes_observed_at_ms = Some(frame.observed_or_produced_at_ms());
        snapshot.focus_contested_panes = frame
            .tabs
            .iter()
            .filter(|tab| tab.focus_contested)
            .flat_map(|tab| tab.panes.iter().map(|pane| pane.pane_id.clone()))
            .collect();
        snapshot.viewed_panes = frame.viewed_panes.clone();
        // Remote rooms classify presence on the host, where tmux
        // `client_activity` only advances on input that crosses SSH. Trust the
        // idle threshold only for local rooms; an attached remote client is
        // present until it detaches.
        let idle_threshold_ms = snapshot
            .link
            .is_none()
            .then(|| machine_config.sidebar.afk_after_ms());
        snapshot.presence = frame
            .presence
            .map(|sample| SidebarPresence::classify(sample, idle_threshold_ms));
        snapshot.truth_degraded = truth_notice_for_frame(&frame);
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
        let (next_snapshot, diagnostics) = snapshot.with_admitted_live_panes_and_diagnostics(
            admitted_panes,
            &lazy_pairings,
            Some(&unread_row_ids),
            &account_budgets,
            &exhausted_resumes,
        );
        snapshot = next_snapshot;
        for event in diagnostics {
            if let Some(diag) = diag {
                diag.emit(event);
            }
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
    let is_producer = matches!(&mode, EnrichMode::Producing { .. });
    let project_heavy_caches = |snapshot: SidebarSnapshot, config: crate::config::MachineConfig| {
        let (mut snapshot, caches) = fold_machine_config_cached(snapshot, runtime, config);
        let diff_cache = read_diff_stats_cache(&runtime.root.join("diff-stats.json"));
        project_diff_stats(&mut snapshot, &diff_cache);
        project_cached_pr_states(&mut snapshot, runtime);
        (snapshot, caches)
    };
    let spending_caches = match mode {
        EnrichMode::Cached => {
            let caches;
            (snapshot, caches) = project_heavy_caches(snapshot, machine_config);
            caches
        }
        EnrichMode::Producing {
            config,
            heavy:
                HeavyLanes::Refresh {
                    compute_spending,
                    refresh_git,
                },
            ..
        } => {
            let spending = compute_spending(&snapshot);
            snapshot = fold_machine_config_producing(
                snapshot,
                runtime,
                &spending.provider.spending.by_provider,
                *config,
                &resume_messages,
            );
            refresh_git(&mut snapshot);
            spending
        }
        EnrichMode::Producing {
            config,
            heavy: HeavyLanes::Project,
            ..
        } => {
            let caches;
            (snapshot, caches) = project_heavy_caches(snapshot, *config);
            caches
        }
    };
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
    let baselines = refresh_live_spend_baselines(
        runtime,
        &snapshot,
        spending_caches.workspace.refreshed_at_ms,
        is_producer,
    );
    apply_live_today_spend(
        &mut snapshot,
        spending_caches.workspace.tally.headline.usd,
        spending_caches.workspace.refreshed_at_ms,
        &baselines.baselines,
    );
    super::unread::derive(&mut snapshot, &episodes, &read_marks);
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
pub(crate) fn fold_machine_config_producing(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    provider_spending: &BTreeMap<String, crate::agents::SpendTally>,
    config: crate::config::MachineConfig,
    resume_messages: &[ResumeMessage],
) -> SidebarSnapshot {
    let accounts_config = config.accounts.clone();
    let resume_config = config.resume.clone();
    let accounts = produce_accounts(&snapshot, runtime);
    let pr_states = produce_pr_states(&snapshot, runtime);
    let mut snapshot = fold_machine_config_with(snapshot, config, accounts, provider_spending);
    project_pr_state_map(&mut snapshot, &pr_states);
    // The producer owns the account-scoped window cache: it writes live readings
    // back so the budgets survive a session ending or going idle.
    apply_rate_limit_cache(&mut snapshot, runtime, true);
    apply_credits_cache(&mut snapshot, runtime, &accounts_config);
    // Codex's live sessions refresh their app-server-owned budget/context
    // sidecars on a coarse cadence so a long-running task does not wait for the
    // next turn boundary to repaint. The uniform driver then refreshes every
    // metered provider's idle account usage (codex included, while idle).
    refresh_codex_sessions(&snapshot, runtime);
    refresh_account_usage(&snapshot, runtime);
    // Opt-in: nudge a parked agent when its resume condition is due, so a turn
    // that stopped on a budget limit or overload picks itself back up while you
    // are away.
    auto_continue::resume_parked(&snapshot, runtime, &resume_config, resume_messages);
    snapshot
}

/// Fold the per-machine config and dashboard onto a *consumer* snapshot, reading
/// the producer's published `accounts.json` instead of probing, with live
/// context versions merged into the current frame without writing the cache. A
/// consumer forks zero subprocesses (the single-flight contract); a cold cache
/// (no producer publish yet) carries no blocks until the elder's first publish.
/// The cheap config read stays local so each tab honours its own display
/// preferences.
/// Returns the published spending cache whole — tally and stamp — so the caller
/// folds the value tally and the live headline-spend overlay from one read.
fn fold_machine_config_cached(
    snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    config: crate::config::MachineConfig,
) -> (SidebarSnapshot, SpendingCaches) {
    let accounts_config = config.accounts.clone();
    let accounts = cached_accounts_for_snapshot(
        read_accounts_cache(&runtime.shared_accounts_path()),
        &snapshot,
    );
    // Consumers read the producer's published spending cache rather than
    // re-walking the JSONL transcript history themselves.
    let cache = current_provider_spending_cache(runtime);
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let spec = config.headline_spec();
    let workspace = cached_workspace_spending(runtime, &scope, cache.refreshed_at_ms, &spec);
    let mut snapshot =
        fold_machine_config_with(snapshot, config, accounts, &cache.spending.by_provider);
    // A consumer reads the producer's published windows to fill idle gaps, but
    // never writes — the single-flight contract keeps the cache the producer's.
    apply_rate_limit_cache(&mut snapshot, runtime, false);
    apply_credits_cache(&mut snapshot, runtime, &accounts_config);
    (
        snapshot,
        SpendingCaches {
            provider: cache,
            workspace,
        },
    )
}

fn current_provider_spending_cache(runtime: &RuntimePaths) -> ProviderSpendingCache {
    let cache = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    if cache.is_current_version() {
        cache
    } else {
        ProviderSpendingCache::default()
    }
}

fn cached_workspace_spending(
    runtime: &RuntimePaths,
    scope: &SpendScope,
    source_refreshed_at_ms: u64,
    spec: &HeadlineSpec,
) -> WorkspaceSpendingCache {
    if scope.is_empty() {
        return Default::default();
    }
    let hash = scope.hash();
    let workspace = read_workspace_spending_cache(&runtime.workspace_spending_path(&hash));
    if workspace.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
        && workspace.scope_hash == hash
    {
        return workspace;
    }
    derive_workspace_spending(
        runtime,
        scope,
        hash,
        source_refreshed_at_ms,
        &discover_spending_files(),
        spec,
    )
}

fn derive_workspace_spending(
    runtime: &RuntimePaths,
    scope: &SpendScope,
    scope_hash: String,
    source_refreshed_at_ms: u64,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    spec: &HeadlineSpec,
) -> WorkspaceSpendingCache {
    let cursor_path = runtime.shared_spending_cursor_path();
    let key = WorkspaceDeriveKey {
        cursor_path: cursor_path.clone(),
        cursor_stamp: file_stamp(&cursor_path),
        files_signature: discovered_files_signature(files),
        scope_hash: scope_hash.clone(),
        source_refreshed_at_ms,
        headline: spec.clone(),
    };
    if let Ok(memo) = workspace_derive_memo().lock()
        && let Some(cached) = memo.as_ref()
        && cached.key == key
    {
        return cached.workspace.clone();
    }

    let cursor = read_spending_cache(&cursor_path);
    let workspace = WorkspaceSpendingCache {
        version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms: source_refreshed_at_ms,
        scope_hash,
        tally: compute_scoped_tally(files, &cursor, scope, unix_secs_now(), spec),
    };
    if let Ok(mut memo) = workspace_derive_memo().lock() {
        *memo = Some(WorkspaceDeriveMemo {
            key,
            workspace: workspace.clone(),
        });
    }
    workspace
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspaceDeriveMemo {
    key: WorkspaceDeriveKey,
    workspace: WorkspaceSpendingCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceDeriveKey {
    cursor_path: PathBuf,
    cursor_stamp: FileStamp,
    files_signature: u64,
    scope_hash: String,
    source_refreshed_at_ms: u64,
    headline: HeadlineSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

fn workspace_derive_memo() -> &'static Mutex<Option<WorkspaceDeriveMemo>> {
    static MEMO: OnceLock<Mutex<Option<WorkspaceDeriveMemo>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(None))
}

fn file_stamp(path: &Path) -> FileStamp {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileStamp {
            len: 0,
            modified_secs: 0,
            modified_nanos: 0,
        };
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
    FileStamp {
        len: meta.len(),
        modified_secs: modified.as_ref().map_or(0, |duration| duration.as_secs()),
        modified_nanos: modified.map_or(0, |duration| duration.subsec_nanos()),
    }
}

fn discovered_files_signature(
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (adapter, file) in files {
        adapter.descriptor().kind.hash(&mut hasher);
        file.hash(&mut hasher);
    }
    hasher.finish()
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
        theme,
        ..
    } = config;
    snapshot.sidebar = sidebar;
    snapshot.theme = theme;

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
        let config_toggle = remote_control_toggle(descriptor.kind, &remote_control);
        let pane_auto = descriptor.capabilities.remote_control.pane_sessions
            && adapter
                .remote_control_status(accounts.get(descriptor.kind))
                .pane_auto;
        remote_control_flags.insert(descriptor.kind.to_owned(), config_toggle || pane_auto);
    }

    snapshot.with_provider_aggregates(&accounts, &remote_control_flags, provider_spending)
}

fn remote_control_toggle(kind: &str, config: &crate::config::RemoteControlConfig) -> bool {
    match kind {
        "claude" => config.claude,
        "codex" => config.codex,
        _ => false,
    }
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
