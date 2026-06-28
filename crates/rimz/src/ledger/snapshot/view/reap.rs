use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::agents::codex::SessionOrigin;
use crate::feed::FeedItem;
use crate::ledger::snapshot::panes::{agent_owner_pid, is_daemon_mode_codex};
use crate::ledger::snapshot::process::{
    command_is_sidebar_chrome, pane_agent_kind, pane_worktree_path,
};
use crate::pane::PaneRef;
use crate::remote_control;

use super::SidebarSnapshot;
use super::rows::agent_id_from_item;

impl SidebarSnapshot {
    /// Apply best-effort process liveness to agent overlays that published a
    /// PID. Hook protocols do not all expose a session-exit event; when a hook
    /// command can record the agent process identity, the sidebar uses it to
    /// suppress stale ledger overlays without scraping pane contents.
    pub fn drop_dead_agents_with(&mut self, mut is_alive: impl FnMut(u32, Option<&str>) -> bool) {
        self.agents.retain(|agent| {
            if let Some(owner) = &agent.runtime_owner {
                return is_alive(owner.pid, owner.process_start.as_deref());
            }
            agent
                .agent_pid
                .is_none_or(|pid| is_alive(pid, agent.agent_process_start.as_deref()))
        });
    }

    /// Reap daemon-mode Codex sessions the per-user app-server daemon no longer
    /// holds in memory. A daemon-backed session records the shared daemon's pid,
    /// not its own CLI's, so process liveness — which keeps it while the daemon
    /// lives ([`drop_dead_agents_with`]) — can never reap it. Without this a closed
    /// remote-control conversation lingers as a ghost and binds its stale
    /// status, model, tokens, and pending ask onto a live `codex` pane by cwd
    /// ([`agent_pane_for_pane`]).
    ///
    /// Tri-state, and fail-safe by construction (the loaded-thread set is a
    /// liveness improvement, not a perfect pane signal, so it never mass-reaps):
    /// - `loaded` is `None` — the daemon was unreachable or its `thread/loaded/list`
    ///   could not be trusted — keep every session;
    /// - `daemon_pids` is empty — no daemon is running, so every session is
    ///   standalone — keep every session;
    /// - a session is daemon-mode ([`is_daemon_mode_codex`]) and its id is absent
    ///   from `loaded` — reap it;
    /// - anything else — keep it.
    ///
    /// The render lanes run this before the live-pane fold, so a reaped session
    /// can neither render a row nor attach stale stats to a live pane.
    pub fn drop_dead_daemon_sessions(
        &mut self,
        daemon_pids: &BTreeSet<u32>,
        loaded: Option<&BTreeSet<String>>,
    ) {
        let Some(loaded) = loaded else { return };
        if daemon_pids.is_empty() {
            return;
        }
        self.agents.retain(|agent| {
            let reapable = is_daemon_mode_codex(agent, daemon_pids)
                && !loaded.contains(agent.agent_id.as_str());
            !reapable
        });
    }

    /// Reap Codex roots superseded by strictly-newer same-live-pane roots whose
    /// carried rollout lineage proves both sessions are fresh `/clear` / `/new`
    /// conversations. Unknown lineage keeps both sessions, so `/side` / `/btw`
    /// forks never cause the primary to drop.
    pub fn drop_cleared_codex_sessions(&mut self, live_panes: &[PaneRef]) {
        let live_panes = live_codex_panes_by_worktree(live_panes);
        if live_panes.is_empty() {
            return;
        }
        let superseded: Vec<bool> = self
            .agents
            .iter()
            .map(|older| {
                self.agents
                    .iter()
                    .any(|newer| cleared_codex_session_supersedes(older, newer, &live_panes))
            })
            .collect();
        let mut superseded = superseded.into_iter();
        self.agents.retain(|_| !superseded.next().unwrap_or(false));
    }

    /// Reap ghost sessions from the agent rollup. This filters the *derived*
    /// rollup only; the append-only event log is untouched, so it complements
    /// the workspace-level `rimz gc`. Two rules, both safe for the
    /// one-pane-one-row invariant:
    ///
    /// (a) a **pidless** session past [`GHOST_SESSION_TTL_SECS`] — it never
    ///     captured a pid, so process liveness can never reap it, yet it has
    ///     not reported in hours. A recent pidless session (a just-launched
    ///     agent) is kept.
    /// (b) an older session **superseded** by a strictly-newer same-kind
    ///     session that *relaunched* in its pane — a provably different process
    ///     ([`older_yields_pane`]) — or, for two paneless remnants, its
    ///     `(worktree_path, worktree_branch)`. This collapses relaunch-in-place
    ///     and shared-pid ghosts to the newest while never dropping a concurrent
    ///     agent that owns its own pane, nor an in-pane thread fork (Codex
    ///     `/side` / `/btw`) that shares the primary's live process.
    pub fn reap_stale_sessions(&mut self) {
        let now = self.now;
        // Mark each superseded older session by position, borrowing `agents`
        // read-only. Runs on every snapshot rebuild, so the old approach — a
        // `BTreeSet` of owned `(kind, agent_id)` tuples plus a second clone per
        // agent in `retain` — meant up to ~3×N string allocations per call; the
        // parallel `Vec<bool>` keeps it allocation-free per agent.
        //
        // Both reap rules are root-only. A subagent is paneless and pidless by
        // construction and shares no worktree key with its parent, so the
        // supersession rule would collapse two live parallel siblings and the
        // pidless-TTL rule would reap an idle child — both wrong. A subagent
        // `older` therefore maps to `false` (never superseded), and the retain
        // below keeps every subagent outright; they leave the rollup only
        // transitively once the parent is gone.
        let superseded: Vec<bool> = self
            .agents
            .iter()
            .map(|older| {
                older.parent_agent_id.is_none()
                    && self.agents.iter().any(|newer| {
                        newer.parent_agent_id.is_none()
                            && newer.kind == older.kind
                            && newer.agent_id != older.agent_id
                            && newer.last_activity > older.last_activity
                            && older_yields_pane(older, newer)
                    })
            })
            .collect();
        // `Vec::retain` visits each element once, front to back, so a cursor over
        // `superseded` stays aligned with `agents` without a hand-rolled index.
        let mut superseded = superseded.into_iter();
        self.agents.retain(|agent| {
            // Advance the cursor once per agent, before any early return, so it
            // stays aligned with `agents` even when a subagent short-circuits.
            let is_superseded = superseded.next().unwrap_or(false);
            // Subagents are never reaped here — kept until their parent leaves,
            // when the projection's orphan-drop hides them.
            if agent.parent_agent_id.is_some() {
                return true;
            }
            if is_superseded {
                return false;
            }
            !(agent_is_pidless(agent) && session_age_secs(now, agent) > GHOST_SESSION_TTL_SECS)
        });
    }

    /// Whether every live, non-sidebar view in `panes` is the `rimzd` daemon
    /// view — i.e. the user has nothing left but the managed daemon dashboard. A
    /// view is a *daemon* view iff, after dropping its sidebar pane, it is
    /// non-empty and every remaining pane is daemon-dashboard infrastructure
    /// ([`crate::remote_control::pane_is_host`]); a *working* view iff it holds
    /// any non-sidebar, non-dashboard pane. A sidebar-only view (a working tab
    /// mid-self-close) counts as neither, so it neither trips nor blocks the
    /// signal. Returns `false` for an empty or not-yet-born session.
    ///
    /// Keys on `view_id` + `pane_is_host` (which reads the command marker or
    /// the `rimzd` view name — both backends report the view name), so it
    /// behaves identically on Zellij and tmux.
    pub fn only_daemon_view(panes: &[PaneRef]) -> bool {
        // Per view_id: (dashboard pane count, working pane count). Sidebar panes
        // are dropped but still register the view, so a sidebar-only view exists
        // as an entry with zero of each — counted as neither daemon nor working.
        let mut views: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
        for pane in panes {
            let Some(view_id) = pane.view_id.as_deref() else {
                continue;
            };
            let entry = views.entry(view_id).or_default();
            let is_sidebar = pane
                .command
                .as_deref()
                .is_some_and(command_is_sidebar_chrome);
            if is_sidebar {
                continue;
            }
            if remote_control::pane_is_host(pane) {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
        let mut saw_daemon = false;
        for (hosts, working) in views.values() {
            if *working > 0 {
                return false;
            }
            if *hosts > 0 {
                saw_daemon = true;
            }
        }
        saw_daemon
    }
}

/// Age in seconds after which a pidless agent session is reaped as a ghost.
/// A session that never captured a pid can't be reaped by process liveness, so
/// without a TTL it would linger forever; a few hours is long enough that a
/// genuinely live but pidless session (rare) survives, short enough that an
/// abandoned one clears on its own.
pub(super) const GHOST_SESSION_TTL_SECS: i64 = 3 * 60 * 60;

fn agent_is_pidless(agent: &AgentState) -> bool {
    agent.runtime_owner.is_none() && agent.agent_pid.is_none()
}

fn session_age_secs(now: Timestamp, agent: &AgentState) -> i64 {
    now.duration_since(agent.last_activity).as_secs()
}

/// True when reaping `older` cannot drop a concurrently-live agent. The pane is
/// the unit of identity: an older session yields when the newer one *relaunched*
/// in its exact pane — a provably different process ([`relaunched_in_pane`]),
/// regardless of any branch checkout between the two — or when both are paneless
/// remnants of the same worktree (indistinguishable daemon/shared-pid ghosts).
/// An older paneless session does not yield to a newer distinctly stamped pane:
/// it may still be the occupant of another same-cwd lazy agent pane that only
/// the projection can bind.
fn older_yields_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (older.pane.as_ref(), newer.pane.as_ref()) {
        (Some(older_pane), Some(newer_pane)) => {
            newer_pane.pane_id == older_pane.pane_id && relaunched_in_pane(older, newer)
        }
        (None, None) => {
            older.worktree_path == newer.worktree_path
                && older.worktree_branch == newer.worktree_branch
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// Whether the newer session is a *relaunch* of the older in their shared pane —
/// a provably different process — rather than a same-process in-pane thread fork.
/// A Codex `/side` / `/btw` fork registers a fresh session id in the live process
/// the primary still owns, so the pair shares one owner pid and must not collapse;
/// only the projection (`stamped_agent_for_pane`) arbitrates such a pair, pinning
/// the pane to its earliest-registered primary. A standalone relaunch records two
/// distinct live pids, so it still collapses here. A daemon-routed relaunch shares
/// the daemon's owner pid like a fork, but the loaded-thread reaper
/// ([`drop_dead_daemon_sessions`]) and the projection's process-start guard
/// ([`crate::ledger::snapshot::panes::pane_start_allows_bind`]) collapse it
/// instead. Unknown owners never collapse — a missing pid cannot prove a relaunch.
fn relaunched_in_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (agent_owner_pid(older), agent_owner_pid(newer)) {
        (Some(older_pid), Some(newer_pid)) => older_pid != newer_pid,
        _ => false,
    }
}

fn cleared_codex_session_supersedes(
    older: &AgentState,
    newer: &AgentState,
    live_panes: &BTreeMap<&str, Vec<&PaneRef>>,
) -> bool {
    older.parent_agent_id.is_none()
        && newer.parent_agent_id.is_none()
        && older.kind == "codex"
        && newer.kind == "codex"
        && newer.agent_id != older.agent_id
        && newer.last_activity > older.last_activity
        && older.origin == Some(SessionOrigin::Fresh)
        && newer.origin == Some(SessionOrigin::Fresh)
        && same_cleared_codex_scope(older, newer)
        && same_live_codex_pane(older, newer, live_panes)
}

fn same_cleared_codex_scope(older: &AgentState, newer: &AgentState) -> bool {
    let Some(older_path) = older
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
    else {
        return false;
    };
    if newer.worktree_path.as_deref() != Some(older_path)
        || newer.worktree_branch != older.worktree_branch
    {
        return false;
    }
    true
}

fn live_codex_panes_by_worktree(panes: &[PaneRef]) -> BTreeMap<&str, Vec<&PaneRef>> {
    let mut live: BTreeMap<&str, Vec<&PaneRef>> = BTreeMap::new();
    for pane in panes {
        if pane_agent_kind(pane) != Some("codex") {
            continue;
        }
        let Some(worktree) = pane_worktree_path(pane) else {
            continue;
        };
        live.entry(worktree).or_default().push(pane);
    }
    live
}

fn same_live_codex_pane(
    older: &AgentState,
    newer: &AgentState,
    live_panes: &BTreeMap<&str, Vec<&PaneRef>>,
) -> bool {
    let Some(worktree) = older.worktree_path.as_deref() else {
        return false;
    };
    let Some(panes) = live_panes.get(worktree) else {
        return false;
    };
    match (older.pane.as_ref(), newer.pane.as_ref()) {
        (Some(older_pane), Some(newer_pane)) => {
            older_pane.pane_id == newer_pane.pane_id
                && panes.iter().any(|pane| pane.pane_id == older_pane.pane_id)
        }
        _ => false,
    }
}

pub(super) fn is_agent_native_item(item: &FeedItem) -> bool {
    item.source_kind == "agent-hook"
}

/// True when an agent-hook ask names a session (`agent_id`/`session_id`) that is
/// no longer the live occupant of its pane. The rollup is the liveness source of
/// truth — gated by `SessionEnd` and process-liveness — so an ask is stale when
/// either its session has left the rollup entirely, or a strictly-newer root
/// session of the same kind has taken over the worktree. The latter reaps the
/// zombie case: a pidless `SessionStart`-only session never ends and never gets
/// reaped by process liveness, so without supersession its old permission prompt
/// pins itself onto the freshly launched session sharing the pane. Subagents
/// never supersede their parent: they share the parent's pane and worktree but do
/// not own the human decision surface. Asks with no session id can't be proven
/// stale and are kept.
pub(super) fn agent_hook_session_stale(item: &FeedItem, agents: &[AgentState]) -> bool {
    if item.source_kind != "agent-hook" {
        return false;
    }
    let Some(agent_id) = agent_id_from_item(item) else {
        return false;
    };
    let Some(session) = agents
        .iter()
        .find(|agent| agent.kind == item.source && agent.agent_id == agent_id)
    else {
        return true;
    };
    if session.parent_agent_id.is_some() {
        return false;
    }
    agents.iter().any(|other| {
        other.parent_agent_id.is_none()
            && other.kind == session.kind
            && other.agent_id != session.agent_id
            && other.worktree_path == session.worktree_path
            && other.last_activity > session.last_activity
    })
}
