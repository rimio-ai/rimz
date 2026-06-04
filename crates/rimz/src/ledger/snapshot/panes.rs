//! Pane binding: which ledger agent owns which live pane, the own-view
//! projection, and the daemon-view predicates.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::process::{command_agent_kind, program_label};
use super::view::{SidebarRow, SidebarRowKind};
use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, AgentStatus, PaneRef};
use crate::ids::PaneId;

/// One sidebar's view of the panes sharing its tab/window. `None` on the
/// snapshot means the count could not be determined (no `--exclude-pane-id`, or
/// the caller's pane was absent from the live list); the renderer treats that
/// as "never self-close".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarOwnView {
    pub sibling_count: usize,
    /// True iff the sidebar's own pane is its view's active pane — the user
    /// focused the sidebar itself, so the derived baseline is `None` and the
    /// renderer holds its last selection.
    pub own_is_active: bool,
    /// The view's active working pane: the non-sidebar sibling carrying the
    /// per-view `is_focused` mark. The renderer derives its selection baseline
    /// from it (see `rimz-sidebar`'s `reconcile_selection`) — same-tab by
    /// construction, defined whether or not a client is viewing the tab.
    pub active_pane_id: Option<PaneId>,
    /// Whether the caller's own view is the `rimzd` daemon view: its siblings,
    /// after dropping any sidebar pane, are non-empty and all managed hosts
    /// ([`crate::remote_control::pane_is_host`]). The daemon-view sidebar gates
    /// the session-exit detach on this so a working-tab sidebar never triggers
    /// it. `#[serde(default)]` keeps the wire shape stable for older producers.
    #[serde(default)]
    pub own_view_is_daemon: bool,
}

impl SidebarOwnView {
    /// Summarize the panes sharing `own`'s view (tab/window) from a live pane
    /// list. Pure and backend-agnostic: callers own pane discovery and pass the
    /// result in. Returns `None` when `own` is absent from `panes` — the caller
    /// cannot reason about a view it cannot find itself in, so it must not
    /// self-close.
    pub fn from_panes(own: &PaneId, panes: &[PaneRef]) -> Option<Self> {
        let own_pane = panes.iter().find(|pane| pane.pane_id == *own)?;
        let own_view = own_pane.view_id.as_deref();
        let siblings = panes
            .iter()
            .filter(|pane| pane.pane_id != *own && pane.view_id.as_deref() == own_view)
            .collect::<Vec<_>>();
        // The own view is the daemon view iff, after dropping sidebar panes, its
        // remaining siblings are non-empty and all managed hosts. Keys on
        // `pane_is_host`, never `view_name`, for Zellij/tmux parity.
        let non_sidebar_siblings: Vec<&PaneRef> = siblings
            .iter()
            .copied()
            .filter(|pane| {
                pane.command
                    .as_deref()
                    .is_none_or(|command| program_label(command) != "rimz-sidebar")
            })
            .collect();
        // The view's *active* pane (`is_focused` — exactly one per view on both
        // backends), not the client focus: it is defined even when no client is
        // viewing this tab, and it stays one deterministic value per tab under
        // multiplayer Zellij — the only shape a shared sidebar pane can render.
        let active_pane_id = non_sidebar_siblings
            .iter()
            .find(|pane| pane.is_focused)
            .map(|pane| pane.pane_id.clone());
        let own_view_is_daemon = !non_sidebar_siblings.is_empty()
            && non_sidebar_siblings
                .iter()
                .all(|&pane| crate::remote_control::pane_is_host(pane));
        Some(Self {
            sibling_count: siblings.len(),
            own_is_active: own_pane.is_focused,
            active_pane_id,
            own_view_is_daemon,
        })
    }
}

/// Whether `agent` is a daemon-mode Codex session: a root (non-subagent) `codex`
/// session with no stamped pane whose recorded hook owner is the shared app-server
/// daemon ([`crate::remote_control::codex_daemon_pids`]). Subagents are excluded —
/// their ids are not root threads, so they never appear in `thread/loaded/list` —
/// and so are standalone sessions, whose owner pid is their own in-pane CLI rather
/// than a daemon.
pub(super) fn is_daemon_mode_codex(agent: &AgentState, daemon_pids: &BTreeSet<u32>) -> bool {
    if agent.kind != "codex" || agent.pane.is_some() || agent.parent_agent_id.is_some() {
        return false;
    }
    agent_owner_pid(agent).is_some_and(|pid| daemon_pids.contains(&pid))
}

/// The pid the hook recorded as this session's owner: the runtime owner when one
/// was captured, else the legacy `agent_pid`. In daemon mode this is the shared
/// app-server daemon; in standalone mode it is the session's own process.
fn agent_owner_pid(agent: &AgentState) -> Option<u32> {
    agent
        .runtime_owner
        .as_ref()
        .map(|owner| owner.pid)
        .or(agent.agent_pid)
}

/// The agent that stamped this exact pane id, if one is still unbound. Binding
/// is by stamped pane id alone — never by foreground command or cwd — so a pane
/// can only ever host the agent that ran in it (`agent_binds_only_by_stamped_
/// pane_id` pins this). When a stale rollup holds more than one claimant for a
/// pane id (a relaunch the reaper has not yet collapsed), the most-recently-
/// active wins, keeping the bind deterministic. The one relaxation — a lazy-
/// registering agent whose session arrives unstamped (Codex) — lives in the
/// separate, tightly-scoped `lazy_agent_for_pane`, never here.
pub(super) fn agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    bound: &BTreeSet<(String, String)>,
) -> Option<&'a AgentState> {
    agents
        .iter()
        // Cheap pane match first: only agents stamped on this exact pane reach
        // the allocating `bound` lookup, so the common miss costs no clones.
        .filter(|agent| {
            agent.pane.as_ref().is_some_and(|stamped| {
                stamped.pane_id == pane.pane_id && pane_start_matches(stamped, pane)
            })
        })
        // A subagent runs in its parent's pane and is stamped with the parent's
        // pane id; it nests under the parent via `attach_sub_agents` and must
        // never win the pane as a top-level row. Panes bind root agents only.
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .max_by_key(|agent| agent.last_activity)
}

/// What a live pane running a lazy-registering agent resolves to. Such an agent
/// ([`crate::agents::Capabilities::registers_lazily`]) can be present without a stamped
/// session — it registers lazily and/or routes hooks through a daemon — so it
/// can't bind through `agent_for_pane` (stamped-id only); this is where its pane
/// resolves instead. Codex is the only such agent today.
pub(super) enum LazyAgentRow<'a> {
    /// An unstamped session bound to this pane by exact worktree cwd (most-
    /// recently-active wins; the reaper collapses a stale one). Rendered through the
    /// shared `push_agent_row`, so it reads identically to a stamped agent — its
    /// real status, model, and pending ask.
    Agent(&'a AgentState),
    /// A wired instance with no session bound yet — a lazy agent registers its
    /// session on the first turn, so the pane has no rollup entry. Synthesize an
    /// idle row so it reads as a proper idle *agent* at rest — `○ <kind>` with its
    /// started-session gauge and a cockpit tally — instead of a bare, dim process
    /// row. The first turn then swaps in the real bound `Agent` row. Boxed so the
    /// rare synthesized row doesn't bloat the common `Agent` (a thin reference).
    Idle(Box<SidebarRow>),
}

/// Resolve a live pane running a lazy-registering agent ([`LazyAgentRow`]) to its
/// row — the relaxation of stamped-id binding, kept tightly scoped here:
/// - the pane's own command must read a lazy agent kind, so a shell or a `git` the
///   session spawned in the worktree never binds, and a non-lazy agent (Claude)
///   returns `None` and falls through to a process row — a pane-less Claude agent
///   is genuinely gone, since Claude always stamps a live pane;
/// - a pane-less session of that kind whose worktree equals the pane's cwd
///   *exactly* (not containment, so a parent checkout never captures a nested
///   worktree's pane) binds as the real `Agent`;
/// - failing that, when the kind is wired (`wired_lazy_kinds`) a pane with no
///   session yet synthesizes an `Idle` row; an *unwired* agent reports no status,
///   so it stays a process row (agents are invisible until their hooks are wired).
///
/// `None` for a non-agent pane, a non-lazy agent, an empty cwd, or an unwired lazy
/// agent with no session. Broker / remote-control / `rimzd` host panes carry an
/// agent name in their command but are dropped upstream by `with_live_panes`, so
/// they never reach here.
pub(super) fn lazy_agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    bound: &BTreeSet<(String, String)>,
    wired_lazy_kinds: &[String],
) -> Option<LazyAgentRow<'a>> {
    let kind = command_agent_kind(pane.command.as_deref()?)?;
    let registers_lazily = crate::agents::descriptor_by_kind(kind)
        .is_some_and(|descriptor| descriptor.capabilities.registers_lazily);
    if !registers_lazily {
        return None;
    }
    let cwd = pane.cwd.as_deref().filter(|cwd| !cwd.is_empty())?;
    if let Some(agent) = agents
        .iter()
        .filter(|agent| agent.pane.is_none() && agent.kind == kind)
        .filter(|agent| agent.worktree_path.as_deref() == Some(cwd))
        .filter(|agent| pane_start_allows_bind(agent, pane))
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .max_by_key(|agent| agent.last_activity)
    {
        return Some(LazyAgentRow::Agent(agent));
    }
    wired_lazy_kinds
        .iter()
        .any(|wired| wired == kind)
        .then(|| LazyAgentRow::Idle(Box::new(idle_agent_row(pane, kind))))
}

/// Defensive guard for the cwd fallback: when the pane's process start is known,
/// a session whose `last_activity` predates that start belongs to an older
/// instance that once ran in this worktree, not the process now in the pane — so
/// it must not bind. A daemon-mode Codex session records the shared app-server
/// daemon's pid, so process liveness alone cannot tell a stale session from the
/// live one; this keeps that residue off a freshly-started pane in the same cwd.
/// The producer supplies the start from `/proc` for backends that report none
/// natively (Zellij; see [`crate::remote_control::in_pane_agent_start`]), so the
/// guard fires on both backends. Only a pane whose cwd has no readable in-pane
/// agent process — another user's — still falls back to most-recently-active.
pub(super) fn pane_start_allows_bind(agent: &AgentState, pane: &PaneRef) -> bool {
    pane.pane_process_start
        .is_none_or(|start| agent.last_activity >= start)
}

/// The resting row for a wired lazy-agent pane that no session claimed: `○ <kind>`
/// with no model or context yet (the first turn swaps in the real bound agent
/// row). Keyed on the pane id — no session id exists, and pane ids and agent ids
/// are disjoint, so `attach_sub_agents` can never mis-nest a child onto it.
fn idle_agent_row(pane: &PaneRef, kind: &str) -> SidebarRow {
    SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id: pane.pane_id.to_string(),
        name: kind.to_owned(),
        status: Some(AgentStatus::Idle),
        phase: TurnPhase::Idle,
        pane: Some(pane.clone()),
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        // Agent rows draw the started-session gauge at `Some(0)` (see the
        // `SidebarRow.context_pct` doc) — matching a freshly-bound session.
        context_pct: Some(0),
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        worktree_path: pane.cwd.clone(),
        worktree_branch: None,
        last_activity: pane.pane_process_start.unwrap_or_else(Timestamp::now),
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: pane.rss_kb,
        cpu_pct: pane.cpu_pct,
        io_bps: pane.io_bps,
    }
}

pub(super) fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

/// Build a minimal `PaneRef` carrying just the normalized pane id. The reducer
/// only needs identity for binding an agent to its live pane; the live
/// multiplexer overlay fills in command/cwd/focus when it joins.
pub(super) fn pane_ref_from_id(pane_id: PaneId) -> PaneRef {
    PaneRef {
        pane_id,
        session_name: String::new(),
        view_id: None,
        view_kind: None,
        view_name: None,
        is_focused: false,
        command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::ids::{MuxName, PaneId};

    use crate::ledger::snapshot::SidebarSnapshot;

    /// A sibling fixture whose `focused` flag sets both the per-view active bit
    /// and the per-client focus bit — the common single-client case where the
    /// pane the user looks at is also its tab's active pane. The divergence test
    /// below splits them to prove the active bit alone drives the baseline.
    fn view_pane(raw: &str, view: &str, focused: bool) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(crate::ids::ViewKind::Tab),
            view_name: None,
            is_focused: focused,
            command: Some("zsh".to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }
    }

    #[test]
    fn own_view_counts_only_siblings_sharing_the_view() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let focused_here = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let panes = vec![
            view_pane("terminal_1", "tab_0", false),
            view_pane("terminal_2", "tab_0", true),
            view_pane("terminal_3", "tab_1", true), // another tab — not a sibling
        ];

        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane is present");

        assert_eq!(view.sibling_count, 1);
        assert!(!view.own_is_active);
        assert_eq!(view.active_pane_id, Some(focused_here));
    }

    #[test]
    fn own_view_marks_when_the_sidebar_itself_is_active() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let panes = vec![
            view_pane("terminal_1", "tab_0", true),
            view_pane("terminal_2", "tab_0", false),
        ];

        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane is present");

        assert!(view.own_is_active);
        assert_eq!(view.active_pane_id, None);
    }

    #[test]
    fn own_view_is_none_when_own_pane_is_absent() {
        // A view the caller cannot find itself in is unknowable — never close.
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_404");
        let panes = vec![view_pane("terminal_1", "tab_0", true)];

        assert!(SidebarOwnView::from_panes(&own, &panes).is_none());
    }

    #[test]
    fn own_view_picks_the_view_active_pane_without_a_client() {
        // The tab has an active pane (`is_focused`) but no client is looking at
        // it. The baseline is the per-view active pane, defined regardless of
        // where any client is — so the sidebar in an unviewed tab still points
        // at the pane the user would land on.
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_52");
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_53");
        let sibling = PaneRef {
            is_focused: true, // the active pane of this tab
            ..view_pane("terminal_53", "tab_11", false)
        };
        let panes = vec![view_pane("terminal_52", "tab_11", false), sibling];

        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane is present");

        assert!(!view.own_is_active);
        assert_eq!(view.active_pane_id, Some(active));
    }

    /// A pane fixture with an explicit command and optional window name, so a
    /// test can build daemon hosts, sidebars, and working shells across views.
    fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(crate::ids::ViewKind::Tab),
            view_name: view_name.map(str::to_owned),
            is_focused: false,
            command: Some(command.to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }
    }

    #[test]
    fn only_daemon_view_true_when_only_the_daemon_view_remains() {
        // rimzd view: sidebar + two managed hosts; no working view left.
        let panes = vec![
            pane_cmd(
                "terminal_0",
                "tab_0",
                "rimz-sidebar serve --workspace-id ws_x",
                None,
            ),
            pane_cmd(
                "terminal_1",
                "tab_0",
                "claude remote-control --spawn worktree",
                None,
            ),
            pane_cmd("terminal_2", "tab_0", "rimz codex app-server serve", None),
        ];
        assert!(SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn only_daemon_view_false_while_a_working_view_exists() {
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar serve", None),
            pane_cmd(
                "terminal_1",
                "tab_0",
                "claude remote-control --spawn worktree",
                None,
            ),
            pane_cmd("terminal_3", "tab_1", "rimz-sidebar serve", None),
            pane_cmd("terminal_4", "tab_1", "zsh", None),
        ];
        assert!(!SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn only_daemon_view_false_when_no_daemon_view() {
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar serve", None),
            pane_cmd("terminal_1", "tab_0", "zsh", None),
        ];
        assert!(!SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn only_daemon_view_false_on_empty_session() {
        assert!(!SidebarSnapshot::only_daemon_view(&[]));
    }

    #[test]
    fn only_daemon_view_ignores_a_sidebar_only_limbo_view() {
        // The working tab's last working pane just exited; its sidebar is mid
        // self-close. That sidebar-only view counts as neither, so detach fires.
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar serve", None),
            pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None),
            pane_cmd("terminal_3", "tab_1", "rimz-sidebar serve", None),
        ];
        assert!(SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn own_view_is_daemon_true_in_the_rimzd_view_zellij() {
        // Zellij leaves view_name None; the daemon view is recognised by the
        // host command markers alone.
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_0");
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar serve", None),
            pane_cmd(
                "terminal_1",
                "tab_0",
                "claude remote-control --spawn worktree",
                None,
            ),
            pane_cmd("terminal_2", "tab_0", "rimz codex app-server serve", None),
        ];
        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane present");
        assert!(view.own_view_is_daemon);
    }

    #[test]
    fn own_view_is_daemon_true_in_the_rimzd_view_tmux() {
        // tmux: a host pane is recognised by the window-name fallback even when
        // its command carries no marker.
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_0");
        let panes = vec![
            pane_cmd("terminal_0", "rimzd", "rimz-sidebar serve", Some("rimzd")),
            pane_cmd("terminal_1", "rimzd", "claude", Some("rimzd")),
        ];
        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane present");
        assert!(view.own_view_is_daemon);
    }

    #[test]
    fn own_view_is_daemon_false_in_a_working_view() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_0");
        let panes = vec![
            pane_cmd("terminal_0", "tab_1", "rimz-sidebar serve", None),
            pane_cmd("terminal_1", "tab_1", "zsh", None),
        ];
        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane present");
        assert!(!view.own_view_is_daemon);
    }
}
