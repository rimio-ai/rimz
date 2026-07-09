use std::collections::{BTreeMap, BTreeSet};

use crate::agents::AgentState;
use crate::ids::{AgentSessionId, PaneId};
use crate::pane::PaneRef;
use crate::remote_control;
use crate::store::session_death;
use crate::store::snapshot::panes::is_daemon_owned;
use crate::store::snapshot::process::command_is_sidebar_chrome;

use super::SidebarSnapshot;

/// Inputs for the runtime-side session reaps. Every field is optional and an
/// absent input keeps every session (the tri-state fail-safe).
pub struct RuntimeReapInputs<'a> {
    pub daemon_pids: &'a BTreeSet<u32>,
    pub loaded: Option<&'a BTreeSet<String>>,
    pub frame_panes: Option<&'a [PaneRef]>,
    pub exclude_pane: Option<&'a PaneId>,
}

impl SidebarSnapshot {
    /// Reap daemon-mode sessions the per-user app-server daemon no longer
    /// holds in memory. A daemon-backed session records the shared daemon's pid,
    /// not its own CLI's, so process liveness keeps it while the daemon lives
    /// and can never reap it. Without this a closed remote-control conversation
    /// lingers as a ghost and binds its stale status, model, and tokens onto a
    /// live `codex` pane by cwd
    /// ([`agent_pane_for_pane`]).
    ///
    /// Tri-state, and fail-safe by construction (the loaded-thread set and live
    /// panes are liveness improvements, not a perfect pane signal, so they never
    /// mass-reap):
    /// - `loaded` is `None` — the daemon was unreachable or its `thread/loaded/list`
    ///   could not be trusted — keep every session;
    /// - `daemon_pids` is empty — no daemon is running, so every session is
    ///   standalone — keep every session;
    /// - a session is daemon-owned ([`is_daemon_owned`]), its id is absent from
    ///   `loaded`, and it has no pane or its stamped pane is absent from the
    ///   admitted live-pane set — reap it;
    /// - a root agent stamped on a daemon-dashboard host pane is hidden with
    ///   its subagents;
    /// - anything else — keep it.
    ///
    /// Callers run this before the live-pane fold, so a reaped session can
    /// neither render a row nor attach stale stats to a live pane.
    pub fn reap_runtime(&mut self, inputs: RuntimeReapInputs<'_>) {
        let admitted_panes = inputs
            .frame_panes
            .map(|panes| Self::card_admitted_live_panes(panes.to_vec(), inputs.exclude_pane));
        if let Some(frame_panes) = inputs.frame_panes {
            self.drop_host_pane_agents(frame_panes);
        }
        self.drop_dead_daemon_sessions(
            inputs.daemon_pids,
            inputs.loaded,
            admitted_panes.as_deref(),
        );
    }

    fn drop_dead_daemon_sessions(
        &mut self,
        daemon_pids: &BTreeSet<u32>,
        loaded: Option<&BTreeSet<String>>,
        admitted_live_panes: Option<&[PaneRef]>,
    ) {
        let Some(loaded) = loaded else { return };
        if daemon_pids.is_empty() {
            return;
        }
        let live_pane_ids = admitted_live_panes.map(|panes| {
            panes
                .iter()
                .map(|pane| pane.pane_id.clone())
                .collect::<Vec<_>>()
        });
        self.agents.retain(|agent| {
            let absent_from_daemon =
                is_daemon_owned(agent, daemon_pids) && !loaded.contains(agent.agent_id.as_str());
            let pane_absent = match agent.pane.as_ref() {
                None => true,
                Some(pane) => live_pane_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.iter().any(|id| id == &pane.pane_id)),
            };
            let reapable = absent_from_daemon && pane_absent;
            !reapable
        });
    }

    fn drop_host_pane_agents(&mut self, frame_panes: &[PaneRef]) {
        let host_pane_ids = frame_panes
            .iter()
            .filter(|pane| remote_control::pane_is_host(pane))
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        if host_pane_ids.is_empty() {
            return;
        }
        let dropped_roots = self
            .agents
            .iter()
            .filter(|agent| {
                agent.parent_agent_id.is_none()
                    && agent
                        .pane
                        .as_ref()
                        .is_some_and(|pane| host_pane_ids.iter().any(|id| id == &pane.pane_id))
            })
            .map(|agent| agent.agent_id.clone())
            .collect::<BTreeSet<AgentSessionId>>();
        if dropped_roots.is_empty() {
            return;
        }
        self.agents.retain(|agent| {
            if agent.parent_agent_id.is_none() {
                return !dropped_roots.contains(&agent.agent_id);
            }
            agent
                .parent_agent_id
                .as_ref()
                .is_none_or(|parent| !dropped_roots.contains(parent))
        });
    }

    /// Reap ghost sessions from the agent rollup. This filters the *derived*
    /// rollup only; the append-only event log is untouched, so it complements
    /// the workspace-level `rimz gc`. Three rules, all safe for the
    /// one-pane-one-row invariant:
    ///
    /// (a) a **pidless** session past [`session_death::GHOST_SESSION_TTL_SECS`] — it never
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
    /// (c) an older **fresh-lineage** conversation superseded by a strictly-newer
    ///     fresh-lineage conversation of the same kind on the same stamped pane.
    ///     Codex `/clear` / `/new` changes the session id inside one terminal
    ///     process, so owner pids cannot prove a relaunch. A fork carries
    ///     `Forked` lineage and survives; unknown lineage keeps both sessions.
    pub fn reap_stale_sessions(&mut self) {
        let now = self.now;
        retain_unsuperseded(&mut self.agents, session_death::supersedes);
        self.agents.retain(|agent| {
            // Subagents are never reaped here — kept until their parent leaves,
            // when the projection's orphan-drop hides them.
            if agent.parent_agent_id.is_some() {
                return true;
            }
            !(session_death::agent_is_pidless(agent)
                && session_death::session_age_secs(now, agent)
                    > session_death::GHOST_SESSION_TTL_SECS)
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

/// Retain only root sessions not superseded under `supersedes(older, newer)`;
/// subagents always remain because they leave transitively with their parent.
fn retain_unsuperseded(
    agents: &mut Vec<AgentState>,
    supersedes: impl Fn(&AgentState, &AgentState) -> bool,
) {
    // Mark each superseded older session by position, borrowing `agents`
    // read-only. Runs on every snapshot rebuild, so an owned key set would add
    // avoidable string allocations per call; the parallel `Vec<bool>` keeps it
    // allocation-free per agent.
    let superseded: Vec<bool> = agents
        .iter()
        .map(|older| {
            older.parent_agent_id.is_none()
                && agents
                    .iter()
                    .any(|newer| newer.parent_agent_id.is_none() && supersedes(older, newer))
        })
        .collect();
    // `Vec::retain` visits each element once, front to back, so a cursor over
    // `superseded` stays aligned with `agents` without a hand-rolled index.
    let mut superseded = superseded.into_iter();
    agents.retain(|agent| {
        let is_superseded = superseded.next().unwrap_or(false);
        agent.parent_agent_id.is_some() || !is_superseded
    });
}
