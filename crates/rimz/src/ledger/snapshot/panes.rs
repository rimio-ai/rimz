//! Pane binding: which ledger agent owns which live pane, the own-view
//! projection, and the daemon-view predicates.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::process::command_agent_kind;
use super::process::{pane_command_is_known, row_from_process};
use super::row::{AgentCard, RowCard, SidebarRow};
use crate::agents::AgentDescriptor;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, AgentStatus, PaneRef};
use crate::ids::{AgentKind, AgentSessionId, PaneId};

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
    /// from it (see `sidebar_renderer::app::selection`) — same-tab by
    /// construction, defined whether or not a client is viewing the tab.
    pub active_pane_id: Option<PaneId>,
    /// The view's working (non-sidebar) sibling pane ids — the only panes a
    /// fused focus event may retarget `active_pane_id` onto. A `FocusChanged`
    /// patch is session-broadcast and carries every view's per-view marks, so
    /// fusion filters against this set; empty (an older producer's frame)
    /// degrades to pull-only baseline updates. `#[serde(default)]` keeps the
    /// wire shape stable.
    #[serde(default)]
    pub working_pane_ids: Vec<PaneId>,
    /// Whether the caller's own view is the `rimzd` daemon view: its siblings,
    /// after dropping any sidebar pane, are non-empty and all managed hosts
    /// ([`crate::remote_control::pane_is_host`]). `#[serde(default)]` keeps the
    /// wire shape stable for older producers.
    #[serde(default)]
    pub own_view_is_daemon: bool,
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

/// The card-admission verdict for one live pane: admitted, or the named reason
/// it renders nothing. Exactly the panes that exist *for* the room rather than
/// *in* it are excluded — the caller's own pane, sidebar chrome, and the
/// managed remote-control / app-server hosts. Everything else is admitted: a
/// pane whose command is still unreadable (a raced or mid-birth read) stays in,
/// because an agent stamp binds by pane id alone and must keep rendering its
/// row — the no-row guard for the *unbound* residue is the fold's
/// `pane_command_is_known`, never admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAdmission {
    Admitted,
    ExcludedPaneId,
    SidebarChrome,
    RemoteControlOrAppServerHost,
}

impl CardAdmission {
    pub(super) fn admits(self) -> bool {
        self == Self::Admitted
    }
}

pub(super) fn pane_admits_card(pane: &PaneRef, exclude: Option<&PaneId>) -> CardAdmission {
    if exclude.is_some_and(|excluded| pane.pane_id == *excluded) {
        return CardAdmission::ExcludedPaneId;
    }
    if pane
        .command
        .as_deref()
        .is_some_and(super::process::command_is_sidebar_chrome)
    {
        return CardAdmission::SidebarChrome;
    }
    if crate::remote_control::pane_is_host(pane) {
        return CardAdmission::RemoteControlOrAppServerHost;
    }
    CardAdmission::Admitted
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

/// The agent that stamped this exact pane id, if one is still unbound. Non-lazy
/// agents bind by stamped pane id alone — never by foreground command or cwd —
/// so a pane can only ever host the agent that ran in it (`agent_binds_only_by_
/// stamped_pane_id` pins this). Lazy-registering agents additionally respect the
/// live pane process start and a known live foreground command: when the session
/// predates the pane start, or the command no longer classifies as that kind, the
/// pane renders through the later ladder steps. A missing command is still
/// treated as an unknown raced read and keeps the stamped row. When a stale
/// rollup holds more than one claimant for a pane id (a relaunch the reaper has
/// not yet collapsed), the most-recently-active wins, keeping the bind
/// deterministic. The one relaxation — a lazy-registering agent whose session
/// arrives unstamped (Codex) — lives in the separate, tightly-scoped
/// `lazy_agent_for_pane`, never here.
pub(super) fn agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    bound: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Option<&'a AgentState> {
    agents
        .iter()
        // Cheap pane match first: only agents stamped on this exact pane reach
        // the allocating `bound` lookup, so the common miss costs no clones.
        .filter(|agent| agent.pane.as_ref().is_some_and(|stamped| {
            stamped_agent_matches_live_pane(agent, stamped, pane)
        }))
        // A subagent runs in its parent's pane and is stamped with the parent's
        // pane id; it nests under the parent via `attach_sub_agents` and must
        // never win the pane as a top-level row. Panes bind root agents only.
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .max_by_key(|agent| agent.last_activity)
}

fn stamped_agent_matches_live_pane(agent: &AgentState, stamped: &PaneRef, pane: &PaneRef) -> bool {
    if stamped.pane_id != pane.pane_id || !pane_start_matches(stamped, pane) {
        return false;
    }
    let Some(descriptor) = crate::agents::descriptor_by_kind(agent.kind.as_str()) else {
        return true;
    };
    if !descriptor.capabilities.registers_lazily {
        return true;
    }
    if !pane_start_allows_bind(agent.last_activity, pane) {
        return false;
    }
    match pane.command.as_deref() {
        Some(command) => command_agent_kind(command) == Some(agent.kind.as_str()),
        None => true,
    }
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
    bound: &BTreeSet<(AgentKind, AgentSessionId)>,
    wired_lazy_kinds: &[String],
    lazy_agent_default_models: &BTreeMap<String, String>,
    now: Timestamp,
) -> Option<LazyAgentRow<'a>> {
    let kind = command_agent_kind(pane.command.as_deref()?)?;
    let descriptor = crate::agents::descriptor_by_kind(kind)?;
    if !descriptor.capabilities.registers_lazily {
        return None;
    }
    let cwd = pane.cwd.as_deref().filter(|cwd| !cwd.is_empty())?;
    if let Some(agent) = agents
        .iter()
        .filter(|agent| agent.pane.is_none() && agent.kind == kind)
        .filter(|agent| agent.worktree_path.as_deref() == Some(cwd))
        .filter(|agent| pane_start_allows_bind(agent.last_activity, pane))
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .max_by_key(|agent| agent.last_activity)
    {
        return Some(LazyAgentRow::Agent(agent));
    }
    wired_lazy_kinds.iter().any(|wired| wired == kind).then(|| {
        LazyAgentRow::Idle(Box::new(idle_agent_row(
            pane,
            descriptor,
            lazy_agent_default_models
                .get(kind)
                .map(String::as_str)
                .or(descriptor.default_model),
            now,
        )))
    })
}

/// Defensive guard for lazy-registering binds: when the pane's process start is
/// known, a session whose `last_activity` predates that start belongs to an older
/// instance, not the process now in the pane — so it must not bind. A daemon-mode
/// Codex session records the shared app-server daemon's pid, so process liveness
/// alone cannot tell a stale session from the live one; this keeps that residue
/// off a freshly-started pane even when a mux rebirth reuses the old pane id. The
/// producer supplies the start from `/proc` for backends that report none
/// natively (Zellij; see [`crate::remote_control::in_pane_agent_start`]), so the
/// guard fires on both backends. Only a pane with no readable in-pane agent start
/// — another user's, or an unrecoverable raced read — still falls back to the
/// stamped pane id or most-recently-active cwd match. Hook ingestion shares this
/// predicate to decide when a prior session's stamp still plausibly owns a pane
/// (`cli::hooks` focus recovery), so the bind and recovery verdicts can't drift.
pub fn pane_start_allows_bind(last_activity: Timestamp, pane: &PaneRef) -> bool {
    pane.pane_process_start
        .is_none_or(|start| last_activity >= start)
}

/// The resting row for a wired lazy-agent pane that no session claimed: `○ <kind>`
/// with adapter-owned model/window defaults when known (the first turn swaps in
/// the real bound agent row). Keyed on the pane id — no session id exists, and
/// pane ids and agent ids are disjoint, so `attach_sub_agents` can never
/// mis-nest a child onto it.
fn idle_agent_row(
    pane: &PaneRef,
    descriptor: &AgentDescriptor,
    default_model: Option<&str>,
    now: Timestamp,
) -> SidebarRow {
    SidebarRow {
        id: pane.pane_id.to_string(),
        name: descriptor.kind.to_owned(),
        pane: Some(pane.clone()),
        worktree_path: pane.cwd.clone(),
        worktree_branch: None,
        last_activity: pane.pane_process_start.unwrap_or(now),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Idle),
            phase: TurnPhase::Idle,
            request_id: None,
            surface: None,
            task: None,
            prompt: None,
            model: default_model.map(ToOwned::to_owned),
            effort: None,
            // Agent rows draw the started-session gauge at `Some(0)` — matching
            // a freshly-bound session.
            context_pct: Some(0),
            context_window: descriptor.default_context_window,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            context_severity: None,
            // No session yet — the pane's process start is this row's spawn key.
            registered_at: None,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            compacting: false,
            turn_error_label: None,
        })),
    }
}

pub(crate) fn row_from_frame_pane(
    pane: &PaneRef,
    wired_lazy_kinds: &[String],
    lazy_agent_default_models: &BTreeMap<String, String>,
    now: Timestamp,
) -> Option<SidebarRow> {
    let command = pane.command.as_deref()?;
    let kind = command_agent_kind(command);
    if let Some(kind) = kind
        && let Some(descriptor) = crate::agents::descriptor_by_kind(kind)
        && descriptor.capabilities.registers_lazily
        && wired_lazy_kinds.iter().any(|wired| wired == kind)
    {
        return Some(idle_agent_row(
            pane,
            descriptor,
            lazy_agent_default_models
                .get(kind)
                .map(String::as_str)
                .or(descriptor.default_model),
            now,
        ));
    }
    pane_command_is_known(pane).then(|| row_from_process(pane, now))
}

pub(super) fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::ids::{MuxName, PaneId};

    use crate::ledger::snapshot::SidebarSnapshot;

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
        }
    }

    #[test]
    fn only_daemon_view_true_when_only_the_daemon_view_remains() {
        // rimzd view: sidebar + two managed hosts; no working view left.
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
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
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
            pane_cmd(
                "terminal_1",
                "tab_0",
                "claude remote-control --spawn worktree",
                None,
            ),
            pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
            pane_cmd("terminal_4", "tab_1", "zsh", None),
        ];
        assert!(!SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn only_daemon_view_false_when_no_daemon_view() {
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
            pane_cmd("terminal_1", "tab_0", "zsh", None),
        ];
        assert!(!SidebarSnapshot::only_daemon_view(&panes));
    }

    #[test]
    fn only_daemon_view_false_on_empty_session() {
        assert!(!SidebarSnapshot::only_daemon_view(&[]));
    }

    #[test]
    fn card_admission_accepts_a_working_pane() {
        let pane = pane_cmd("terminal_1", "tab_0", "zsh", None);
        assert_eq!(pane_admits_card(&pane, None), CardAdmission::Admitted);
    }

    #[test]
    fn card_admission_names_excluded_pane_id() {
        let pane = pane_cmd("terminal_1", "tab_0", "zsh", None);
        assert_eq!(
            pane_admits_card(&pane, Some(&pane.pane_id)),
            CardAdmission::ExcludedPaneId
        );
    }

    #[test]
    fn card_admission_names_sidebar_chrome() {
        let pane = pane_cmd("terminal_1", "tab_0", "rimz-sidebar", None);
        assert_eq!(pane_admits_card(&pane, None), CardAdmission::SidebarChrome);
    }

    #[test]
    fn card_admission_names_remote_control_hosts() {
        let pane = pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None);
        assert_eq!(
            pane_admits_card(&pane, None),
            CardAdmission::RemoteControlOrAppServerHost
        );
    }

    #[test]
    fn card_admission_keeps_an_identityless_pane_in_the_fold() {
        // A raced or mid-birth read with no command stays admitted: an agent
        // stamp binds by pane id alone, so admission must not eat the row. The
        // unbound no-row decision lives in the fold (`pane_command_is_known`).
        let unreadable = crate::feed::PaneRef {
            command: None,
            ..pane_cmd("terminal_1", "tab_0", "zsh", None)
        };
        assert_eq!(pane_admits_card(&unreadable, None), CardAdmission::Admitted);
    }

    #[test]
    fn only_daemon_view_ignores_a_sidebar_only_limbo_view() {
        // The working tab's last working pane just exited; its sidebar is mid
        // self-close. That sidebar-only view counts as neither, so detach fires.
        let panes = vec![
            pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
            pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None),
            pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
        ];
        assert!(SidebarSnapshot::only_daemon_view(&panes));
    }
}
