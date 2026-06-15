//! Pane binding: which ledger agent owns which live pane, the own-view
//! projection, and the daemon-view predicates.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::process::{pane_agent_kind, pane_command_is_known};
use crate::feed::{AgentState, PaneRef};
use crate::ids::{AgentKind, AgentSessionId, PaneId};

mod lazy;

pub(super) use lazy::{AgentPaneRow, agent_pane_for_pane, row_from_frame_pane};
pub(crate) use lazy::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, compute_lazy_agent_pairings,
};

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
    /// from it (see `sidebar_pane::app::selection`) — same-tab by construction,
    /// defined whether or not a client is viewing the tab.
    pub active_pane_id: Option<PaneId>,
    /// The view's working (non-sidebar) sibling pane ids — the only panes a
    /// fused focus event may retarget `active_pane_id` onto. A `FocusChanged`
    /// patch is session-broadcast and carries every view's per-view marks, so
    /// fusion filters against this set; empty (an older producer's frame)
    /// degrades to pull-only baseline updates. `#[serde(default)]` keeps the
    /// wire shape stable.
    #[serde(default)]
    pub working_pane_ids: Vec<PaneId>,
    /// The own view's frame had multiple raw focus candidates. The renderer holds
    /// an existing selection baseline through this state instead of replacing it
    /// with the producer's deterministic arbitration; a fused focus event that
    /// names one of `working_pane_ids` clears it for that fused frame.
    #[serde(default)]
    pub focus_contested: bool,
    /// Whether the caller's own view is the `rimzd` daemon view: its siblings,
    /// after dropping any sidebar pane, are non-empty and all managed hosts
    /// ([`crate::remote_control::pane_is_host`]). `#[serde(default)]` keeps the
    /// wire shape stable for older producers.
    #[serde(default)]
    pub own_view_is_daemon: bool,
}

/// Whether `agent` is a daemon-mode Codex session: a root (non-subagent)
/// `codex` session with no stamped pane whose recorded hook owner is the shared
/// app-server daemon ([`crate::remote_control::codex_daemon_pids`]).
pub(super) fn is_daemon_mode_codex(agent: &AgentState, daemon_pids: &BTreeSet<u32>) -> bool {
    if agent.kind != "codex" || agent.pane.is_some() || agent.parent_agent_id.is_some() {
        return false;
    }
    agent_owner_pid(agent).is_some_and(|pid| daemon_pids.contains(&pid))
}

/// The card-admission verdict for one live pane: admitted, or the named reason
/// it renders nothing.
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

/// The pid the hook recorded as this session's owner: the runtime owner when
/// one was captured, else the legacy `agent_pid`. In daemon mode this is the
/// shared app-server daemon; in standalone mode it is the session's own process.
pub(super) fn agent_owner_pid(agent: &AgentState) -> Option<u32> {
    agent
        .runtime_owner
        .as_ref()
        .map(|owner| owner.pid)
        .or(agent.agent_pid)
}

/// The agent that stamped this exact pane id, if one is still unbound. Non-lazy
/// agents bind by stamped pane id alone — never by foreground command or cwd —
/// so a pane can only ever host the agent that ran in it.
pub(super) fn agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    bound: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Option<&'a AgentState> {
    stamped_agent_for_pane(pane, agents)
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
}

/// The root agent stamped on this exact live pane id, regardless of whether
/// another row already bound it.
pub(super) fn stamped_agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
) -> Option<&'a AgentState> {
    agents
        .iter()
        // Cheap pane match first: only agents stamped on this exact pane reach
        // the root-agent filters, so the common miss costs no clones.
        .filter(|agent| {
            agent
                .pane
                .as_ref()
                .is_some_and(|stamped| stamped_agent_matches_live_pane(agent, stamped, pane))
        })
        // A subagent runs in its parent's pane and is stamped with the parent's
        // pane id; it nests under the parent via `attach_sub_agents` and must
        // never win the pane as a top-level row. Panes bind root agents only.
        .filter(|agent| agent.parent_agent_id.is_none())
        // The card follows the pane's *primary* — the session that owned it
        // first (earliest `registered_at`). A later in-process thread fork
        // (Codex `/side` / `/btw` registers a fresh session id in the same pane
        // and process) posts newer activity but a later registration, so it can
        // never repaint the card. Safe because the process-start guard in
        // `stamped_agent_matches_live_pane` has already evicted any
        // older-instance residue: this only arbitrates between sessions
        // genuinely sharing one live process, and a real relaunch (new process)
        // still takes over because the dead predecessor is gone before this runs.
        .min_by(|a, b| {
            registered_rank(a)
                .cmp(&registered_rank(b))
                // Most-recently-active as a deterministic fallback when
                // registration order ties or is unknown (pre-field rollups).
                .then_with(|| b.last_activity.cmp(&a.last_activity))
                // Stable final tiebreak so selection is order-independent.
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        })
}

/// Registration sort key: an earlier `registered_at` ranks first (the pane's
/// primary owner), and an absent stamp (a rollup persisted before the field
/// existed) ranks last so a known primary always outranks it. `is_none()` is
/// `false` for `Some` and `true` for `None`, so `Some` sorts ahead of `None`;
/// within `Some`, the earlier timestamp sorts first.
fn registered_rank(agent: &AgentState) -> (bool, Option<Timestamp>) {
    (agent.registered_at.is_none(), agent.registered_at)
}

fn stamped_agent_matches_live_pane(agent: &AgentState, stamped: &PaneRef, pane: &PaneRef) -> bool {
    if stamped.pane_id != pane.pane_id || !pane_start_matches_agent_stamp(stamped, pane) {
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
    match pane_agent_kind(pane) {
        Some(kind) => kind == agent.kind.as_str(),
        None => !pane_command_is_known(pane),
    }
}

/// Defensive guard for read-time binds: when the pane's process start is known,
/// a session whose `last_activity` predates that start belongs to an older
/// instance, not the process now in the pane — so it must not bind.
pub fn pane_start_allows_bind(last_activity: Timestamp, pane: &PaneRef) -> bool {
    pane.pane_process_start
        .is_none_or(|start| last_activity >= start)
}

pub(super) fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

fn pane_start_matches_agent_stamp(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected <= actual,
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
            is_floating: false,
            command: Some(command.to_owned()),
            spawn_command: None,
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    #[test]
    fn only_daemon_view_classifies_view_sets() {
        for (label, panes, expected) in [
            (
                "daemon view only",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd(
                        "terminal_1",
                        "tab_0",
                        "claude remote-control --spawn worktree",
                        None,
                    ),
                    pane_cmd("terminal_2", "tab_0", "rimz codex app-server serve", None),
                ],
                true,
            ),
            (
                "working view exists",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd(
                        "terminal_1",
                        "tab_0",
                        "claude remote-control --spawn worktree",
                        None,
                    ),
                    pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
                    pane_cmd("terminal_4", "tab_1", "zsh", None),
                ],
                false,
            ),
            (
                "no daemon view",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd("terminal_1", "tab_0", "zsh", None),
                ],
                false,
            ),
            ("empty session", Vec::new(), false),
            (
                "sidebar-only limbo view ignored",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None),
                    pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
                ],
                true,
            ),
        ] {
            assert_eq!(
                SidebarSnapshot::only_daemon_view(&panes),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn card_admission_names_card_blockers() {
        let working = pane_cmd("terminal_1", "tab_0", "zsh", None);
        let unreadable = PaneRef {
            command: None,
            ..pane_cmd("terminal_2", "tab_0", "zsh", None)
        };
        for (label, pane, exclude, expected) in [
            (
                "working pane",
                working.clone(),
                None,
                CardAdmission::Admitted,
            ),
            (
                "excluded pane id",
                working.clone(),
                Some(working.pane_id.clone()),
                CardAdmission::ExcludedPaneId,
            ),
            (
                "sidebar chrome",
                pane_cmd("terminal_3", "tab_0", "rimz-sidebar", None),
                None,
                CardAdmission::SidebarChrome,
            ),
            (
                "remote-control host",
                pane_cmd("terminal_4", "tab_0", "rimz codex app-server serve", None),
                None,
                CardAdmission::RemoteControlOrAppServerHost,
            ),
            (
                "identityless pane",
                unreadable,
                None,
                CardAdmission::Admitted,
            ),
        ] {
            assert_eq!(
                pane_admits_card(&pane, exclude.as_ref()),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn agent_stamp_tolerates_floor_to_exact_start_drift_but_items_do_not() {
        let floor: Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
        let exact: Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
        let stamped = PaneRef {
            pane_process_start: Some(floor),
            ..pane_cmd("terminal_1", "tab_0", "codex", None)
        };
        let live = PaneRef {
            pane_process_start: Some(exact),
            ..pane_cmd("terminal_1", "tab_0", "codex", None)
        };

        assert!(
            pane_start_matches_agent_stamp(&stamped, &live),
            "old floor-era agent stamps still attach to the now-exact live process"
        );
        assert!(
            !pane_start_matches(&stamped, &live),
            "standalone item pane refs still require exact start identity"
        );
    }
}
