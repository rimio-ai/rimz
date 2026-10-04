//! Who occupies a resident launch checkout, and whether a takeover may stop
//! them.
//!
//! Pure over the agent rows: the CLI helper reads the snapshot and the owned
//! worktrees, stops what this returns, and presents. One snapshot decides for
//! every occupant, so an agent that starts a turn between that read and its
//! pane close is still stopped.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::utils::path::normalize_path_lexical;

/// The open turn that keeps a takeover from stopping an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTurn {
    Working,
    WaitingOnUser,
    Paused,
    Compacting,
}

impl OpenTurn {
    const fn phrase(self) -> &'static str {
        match self {
            Self::Working => "is working",
            Self::WaitingOnUser => "is waiting on you",
            Self::Paused => "is paused on a provider limit",
            Self::Compacting => "is compacting",
        }
    }
}

/// What a takeover of one checkout does with the agents already in it.
#[derive(Debug)]
pub enum Takeover<'a> {
    /// Every occupant is settled. Roots come before launched children, which
    /// a parent's stop already takes with it.
    Stop(Vec<&'a AgentState>),
    /// An occupant, or a live child one launched, holds an open turn; nothing
    /// is stopped.
    Blocked(Vec<(&'a AgentState, OpenTurn)>),
}

/// Decide the takeover of `checkout`. An occupant is a live pane-backed
/// launch recorded at or under the checkout, read through the one row that
/// currently is that launch; `owned` worktrees nested inside it keep their own
/// agents. The snapshot must carry rest certificates.
pub fn plan<'a>(
    agents: &'a [AgentState],
    checkout: &Path,
    owned: &[PathBuf],
    now: Timestamp,
) -> Takeover<'a> {
    let checkout = normalize_path_lexical(checkout);
    let nested: Vec<PathBuf> = owned
        .iter()
        .map(|path| normalize_path_lexical(path))
        .filter(|path| *path != checkout && path.starts_with(&checkout))
        .collect();
    let mut occupants: Vec<&AgentState> = crate::address::launch_occupants(agents)
        .into_iter()
        .filter(|agent| occupies(agent, &checkout, &nested))
        .collect();
    occupants.sort_by_key(|agent| agent.is_launched_child());
    let mut blockers = Vec::new();
    for occupant in &occupants {
        collect_open_turns(agents, occupant, now, &mut blockers);
    }
    if blockers.is_empty() {
        Takeover::Stop(occupants)
    } else {
        Takeover::Blocked(blockers)
    }
}

/// The skip reason for a blocked takeover, one clause per blocker handle.
pub(super) fn blocked_reason(blockers: &[(String, OpenTurn)]) -> String {
    blockers
        .iter()
        .map(|(handle, turn)| format!("{handle} {}", turn.phrase()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn occupies(agent: &AgentState, checkout: &Path, nested: &[PathBuf]) -> bool {
    if agent.ended_at.is_some() || agent.pane.is_none() || agent.is_provider_subagent() {
        return false;
    }
    let Some(path) = agent.worktree_path.as_deref() else {
        return false;
    };
    let path = normalize_path_lexical(Path::new(path));
    path.starts_with(checkout) && !nested.iter().any(|worktree| path.starts_with(worktree))
}

fn collect_open_turns<'a>(
    agents: &'a [AgentState],
    agent: &'a AgentState,
    now: Timestamp,
    blockers: &mut Vec<(&'a AgentState, OpenTurn)>,
) {
    if blockers.iter().any(|(seen, _)| std::ptr::eq(*seen, agent)) {
        return;
    }
    if let Some(turn) = open_turn(agent, now) {
        blockers.push((agent, turn));
    }
    for child in crate::address::launched_children(agents, agent)
        .into_iter()
        .filter(|child| child.ended_at.is_none())
    {
        collect_open_turns(agents, child, now, blockers);
    }
}

fn open_turn(agent: &AgentState, now: Timestamp) -> Option<OpenTurn> {
    if agent.is_awaiting_input() {
        return Some(OpenTurn::WaitingOnUser);
    }
    match agent.effective_status() {
        AgentStatus::Running => Some(OpenTurn::Working),
        AgentStatus::Waiting => Some(OpenTurn::WaitingOnUser),
        AgentStatus::Paused => Some(OpenTurn::Paused),
        AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed | AgentStatus::Sleeping => {
            agent.is_compacting(now).then_some(OpenTurn::Compacting)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{PendingWait, PendingWaitTrigger, TurnPhase};
    use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
    use crate::pane::PaneRef;

    fn now() -> Timestamp {
        Timestamp::from_second(10_000).expect("timestamp")
    }

    fn agent(id: &str, status: AgentStatus, path: &str) -> AgentState {
        let mut agent = AgentState::seed(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from(id),
            status,
            now(),
        );
        agent.worktree_path = Some(path.to_owned());
        agent.pane = Some(PaneRef::from_id(PaneId::from_parts(
            MuxName::Zellij,
            format!("terminal_{id}"),
        )));
        agent
    }

    fn child_of(parent: &AgentState, id: &str, status: AgentStatus, path: &str) -> AgentState {
        let mut child = agent(id, status, path);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        child
    }

    fn ids(agents: &[&AgentState]) -> Vec<String> {
        agents
            .iter()
            .map(|agent| agent.agent_id.as_str().to_owned())
            .collect()
    }

    fn decide(agents: &[AgentState]) -> Result<Vec<String>, Vec<(String, OpenTurn)>> {
        match plan(
            agents,
            Path::new("/repo/wt"),
            &[PathBuf::from("/repo/wt/.worktrees/lane")],
            now(),
        ) {
            Takeover::Stop(occupants) => Ok(ids(&occupants)),
            Takeover::Blocked(blockers) => Err(blockers
                .into_iter()
                .map(|(agent, turn)| (agent.agent_id.as_str().to_owned(), turn))
                .collect()),
        }
    }

    #[test]
    fn each_status_either_settles_or_blocks() {
        let mut sleeping = agent("a", AgentStatus::Idle, "/repo/wt");
        sleeping.pending_waits.push(PendingWait {
            name: "wait-command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo test".to_owned(),
            },
            armed_at: Some(now()),
        });
        assert_eq!(sleeping.effective_status(), AgentStatus::Sleeping);
        let mut background = agent("a", AgentStatus::Running, "/repo/wt");
        background.phase = TurnPhase::Parked;
        assert_eq!(background.effective_status(), AgentStatus::Success);
        for settled in [
            agent("a", AgentStatus::Idle, "/repo/wt"),
            agent("a", AgentStatus::Success, "/repo/wt"),
            agent("a", AgentStatus::Failed, "/repo/wt"),
            sleeping,
            background,
        ] {
            let status = settled.effective_status();
            assert_eq!(decide(&[settled]), Ok(vec!["a".to_owned()]), "{status:?}");
        }

        let mut waiting = agent("a", AgentStatus::Waiting, "/repo/wt");
        waiting.waiting_since = Some(now());
        assert!(waiting.is_awaiting_input());
        let mut paused = agent("a", AgentStatus::Running, "/repo/wt");
        paused.budget_park = Some(crate::agents::BudgetPark {
            cap_usd: 5.0,
            spend_usd: 5.25,
            window: crate::agents::BudgetWindow::Day,
            at: now(),
            scope: crate::agents::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
        assert_eq!(paused.effective_status(), AgentStatus::Paused);
        let mut compacting = agent("a", AgentStatus::Idle, "/repo/wt");
        compacting.compacting_since = Some(now());
        for (open, turn) in [
            (
                agent("a", AgentStatus::Running, "/repo/wt"),
                OpenTurn::Working,
            ),
            (
                agent("a", AgentStatus::Waiting, "/repo/wt"),
                OpenTurn::WaitingOnUser,
            ),
            (waiting, OpenTurn::WaitingOnUser),
            (paused, OpenTurn::Paused),
            (compacting, OpenTurn::Compacting),
        ] {
            assert_eq!(decide(&[open]), Err(vec![("a".to_owned(), turn)]));
        }
    }

    #[test]
    fn occupants_are_live_pane_sessions_at_or_under_the_checkout() {
        let root = agent("root", AgentStatus::Idle, "/repo/wt/./");
        let subdir = agent("subdir", AgentStatus::Idle, "/repo/wt/crates/core");
        let nested = agent(
            "nested",
            AgentStatus::Running,
            "/repo/wt/.worktrees/lane/src",
        );
        let sibling = agent("sibling", AgentStatus::Running, "/repo/wt-other");
        let parent_dir = agent("parent-dir", AgentStatus::Running, "/repo");
        let mut paneless = agent("paneless", AgentStatus::Running, "/repo/wt");
        paneless.pane = None;
        let mut ended = agent("ended", AgentStatus::Running, "/repo/wt");
        ended.ended_at = Some(now());
        let mut provider_subagent = agent("native", AgentStatus::Running, "/repo/wt");
        provider_subagent.parent_agent_id = Some(root.agent_id.clone());
        let mut pathless = agent("pathless", AgentStatus::Running, "/repo/wt");
        pathless.worktree_path = None;
        let child = child_of(&root, "child", AgentStatus::Idle, "/repo/wt");
        let mut root = root;
        root.launch_id = Some(AgentSessionId::from("launch-root"));
        root.runtime_owner = Some(crate::pane::RuntimeOwner {
            kind: crate::pane::RuntimeOwnerKind::Agent,
            subject_id: "root".to_owned(),
            pid: 42,
            process_start: None,
        });
        let mut cleared = agent("cleared", AgentStatus::Idle, "/repo/wt");
        cleared.launch_id = root.launch_id.clone();
        cleared.pane = root.pane.clone();
        cleared.runtime_owner = root.runtime_owner.clone();
        cleared.last_activity = Timestamp::from_second(9_000).expect("timestamp");

        assert_eq!(
            decide(&[
                child,
                cleared,
                root,
                subdir,
                nested,
                sibling,
                parent_dir,
                paneless,
                ended,
                provider_subagent,
                pathless,
            ]),
            Ok(vec![
                "root".to_owned(),
                "subdir".to_owned(),
                "child".to_owned()
            ])
        );
    }

    #[test]
    fn a_busy_child_elsewhere_blocks_through_its_parent() {
        let parent = agent("parent", AgentStatus::Idle, "/repo/wt");
        let child = child_of(&parent, "child", AgentStatus::Running, "/elsewhere");
        let grandchild = child_of(&child, "grandchild", AgentStatus::Waiting, "/elsewhere");
        let mut finished = child_of(&parent, "finished", AgentStatus::Running, "/elsewhere");
        finished.ended_at = Some(now());
        let idle = agent("idle", AgentStatus::Idle, "/repo/wt");

        assert_eq!(
            decide(&[parent, child, grandchild, finished, idle]),
            Err(vec![
                ("child".to_owned(), OpenTurn::Working),
                ("grandchild".to_owned(), OpenTurn::WaitingOnUser),
            ])
        );
    }

    #[test]
    fn blocked_reason_names_each_blocker_in_the_users_words() {
        assert_eq!(
            blocked_reason(&[
                ("@coder".to_owned(), OpenTurn::Working),
                ("@fox".to_owned(), OpenTurn::WaitingOnUser),
                ("@owl".to_owned(), OpenTurn::Paused),
                ("@elk".to_owned(), OpenTurn::Compacting),
            ]),
            "@coder is working, @fox is waiting on you, @owl is paused on a provider limit, @elk is compacting"
        );
    }
}
