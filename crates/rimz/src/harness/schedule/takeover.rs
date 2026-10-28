//! Who occupies a checkout for loop takeover and workflow admission, and
//! whether a takeover may stop them.
//!
//! The caller supplies runtime-scoped agent rows before this reads owned
//! worktrees and then the clock. It checks candidate owners again because a
//! cached row may outlive its process. An occupant needs a pane stamp and an
//! owner not known dead; no multiplexer is read. The rows stay unchanged for
//! the caller's stop. The read-to-close window includes discovery and owner
//! probes: an agent starting a turn after the row read is still stopped.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::store::runtime::{AgentLiveness, agent_liveness};
use crate::utils::path::normalize_path_lexical;
use crate::worktree::{self, WorktreeErr};

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
            Self::Paused => "is paused on a limit",
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

/// Read checkout occupancy from the caller's runtime-scoped roster.
/// The caller must read its rows, including rest certificates and
/// local-session bindings, before this call. Candidate owners are checked
/// again because the cached roster can outlive them; known-dead owners do not
/// occupy. Returned agents borrow those unchanged rows; the clock is sampled
/// after owned-worktree discovery. No multiplexer is consulted.
pub fn read<'a>(
    agents: &'a [AgentState],
    checkout: &Path,
    project_root: &Path,
) -> Result<Takeover<'a>, WorktreeErr> {
    let checkouts = path_forms(checkout.to_path_buf()).collect::<Vec<_>>();
    let owned = match worktree::discover_owned(project_root) {
        Ok(owned) => owned
            .into_iter()
            .flat_map(|tree| path_forms(tree.marker.worktree_path))
            .collect(),
        Err(WorktreeErr::NotRepo) => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(plan(
        agents,
        &checkouts,
        &owned,
        Timestamp::now(),
        agent_liveness,
    ))
}

fn path_forms(path: PathBuf) -> impl Iterator<Item = PathBuf> {
    let physical = path.canonicalize().ok();
    std::iter::once(path).chain(physical)
}

/// Decide the takeover of one checkout. An occupant is a launch with a pane
/// stamp and owner liveness other than `Dead`, recorded at or under the
/// checkout, read through the one row that currently is that launch; `owned`
/// worktrees nested inside it keep their own agents. `checkouts` and `owned`
/// carry every path form `read` resolved (as launched, and physical where a
/// symlink leads there), since a row may record either. The snapshot must
/// carry rest certificates.
fn plan<'a>(
    agents: &'a [AgentState],
    checkouts: &[PathBuf],
    owned: &[PathBuf],
    now: Timestamp,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
) -> Takeover<'a> {
    let checkouts: Vec<PathBuf> = checkouts
        .iter()
        .map(|path| normalize_path_lexical(path))
        .collect();
    let nested: Vec<PathBuf> = owned
        .iter()
        .map(|path| normalize_path_lexical(path))
        .filter(|path| !checkouts.contains(path) && is_under_any(path, &checkouts))
        .collect();
    let mut occupants: Vec<&AgentState> = crate::address::launch_occupants(agents)
        .into_iter()
        .filter(|agent| {
            occupies(agent, &checkouts, &nested) && liveness(agent) != AgentLiveness::Dead
        })
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

fn is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn occupies(agent: &AgentState, checkouts: &[PathBuf], nested: &[PathBuf]) -> bool {
    if agent.ended_at.is_some() || agent.pane.is_none() || agent.is_provider_subagent() {
        return false;
    }
    let Some(path) = agent.lane_path() else {
        return false;
    };
    let path = normalize_path_lexical(Path::new(path));
    is_under_any(&path, checkouts) && !is_under_any(&path, nested)
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
        decide_in(
            agents,
            &["/repo/wt"],
            &["/repo/wt/.worktrees/lane", "/repo/wt"],
        )
    }

    fn decide_in(
        agents: &[AgentState],
        checkouts: &[&str],
        owned: &[&str],
    ) -> Result<Vec<String>, Vec<(String, OpenTurn)>> {
        decide_in_with_liveness(agents, checkouts, owned, |_| AgentLiveness::Unknown)
    }

    fn decide_in_with_liveness(
        agents: &[AgentState],
        checkouts: &[&str],
        owned: &[&str],
        liveness: impl Fn(&AgentState) -> AgentLiveness,
    ) -> Result<Vec<String>, Vec<(String, OpenTurn)>> {
        decide_at(agents, checkouts, owned, now(), liveness)
    }

    fn decide_at(
        agents: &[AgentState],
        checkouts: &[&str],
        owned: &[&str],
        now: Timestamp,
        liveness: impl Fn(&AgentState) -> AgentLiveness,
    ) -> Result<Vec<String>, Vec<(String, OpenTurn)>> {
        let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        match plan(agents, &paths(checkouts), &paths(owned), now, liveness) {
            Takeover::Stop(occupants) => Ok(ids(&occupants)),
            Takeover::Blocked(blockers) => Err(blockers
                .into_iter()
                .map(|(agent, turn)| (agent.agent_id.as_str().to_owned(), turn))
                .collect()),
        }
    }

    #[test]
    fn a_closed_codex_turns_trailing_tool_does_not_block_takeover() {
        assert_closed_codex_turn_takeover(false);
    }

    #[test]
    fn a_closed_codex_turns_trailing_tool_does_not_end_compaction() {
        assert_closed_codex_turn_takeover(true);
    }

    #[track_caller]
    fn assert_closed_codex_turn_takeover(compacting: bool) {
        use crate::disk::paths::{RuntimePaths, StatePaths};
        use crate::ids::{EventId, WorkspaceId};
        use crate::store::{Store, writer::AgentLifecycleIntent};

        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let store = Store::open(
            StatePaths::under(workspace.clone(), dir.path()).unwrap(),
            RuntimePaths::under(workspace, dir.path()).unwrap(),
        )
        .unwrap();
        let turn = "01a17ed0-62df-77b2-8382-0db6b99a20b4";
        let at = Timestamp::now();
        let append = |index, event, payload| {
            let decoded = crate::agents::definition_by_kind("codex")
                .unwrap()
                .decode_hook(event, &payload)
                .unwrap();
            store
                .for_hook_ingress(EventId::new(), at + jiff::SignedDuration::from_secs(index))
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "takeover-test",
                    agent_kind: AgentKind::new_unchecked("codex"),
                    event_name: event,
                    observation: decoded.lifecycle().unwrap(),
                    spawned_subagents: &[],
                })
                .unwrap()
        };
        let tool = |id| {
            serde_json::json!({
                "session_id": "child", "turn_id": id, "tool_name": "Bash"
            })
        };
        append(
            0,
            "UserPromptSubmit",
            serde_json::json!({"session_id":"child","prompt":"work"}),
        );
        append(1, "PostToolUse", tool(turn));
        append(2, "PostToolUse", tool(turn));
        append(
            3,
            "Stop",
            serde_json::json!({"session_id":"child","turn_id":turn}),
        );
        let late_index = if compacting {
            append(4, "PreCompact", serde_json::json!({"session_id":"child"}));
            5
        } else {
            4
        };
        let late = append(late_index, "PostToolUse", tool(turn));
        let parent = agent("parent", AgentStatus::Idle, "/elsewhere");
        let decide_row = |mut row: AgentState| {
            let placement = child_of(&parent, "child", row.status, "/repo/wt");
            row.parent_agent_id = placement.parent_agent_id;
            row.launch_depth = placement.launch_depth;
            row.worktree_path = placement.worktree_path;
            row.pane = placement.pane;
            decide_at(
                &[row],
                &["/repo/wt"],
                &[],
                at + jiff::SignedDuration::from_secs(late_index + 2),
                |_| AgentLiveness::Live { pid: 42 },
            )
        };
        let expected = if compacting {
            Err(vec![("child".into(), OpenTurn::Compacting)])
        } else {
            Ok(vec!["child".into()])
        };
        let row = store.snapshot().unwrap().agents[0].clone();
        assert_eq!(decide_row(row.clone()), expected);
        let transition = late.transition.unwrap();
        assert!(matches!(
            transition.kind,
            crate::agents::lifecycle::TransitionKind::Ignored { .. }
        ));
        assert!(!transition.compaction_closed);
        assert_eq!(row.compacting_since.is_some(), compacting);
        assert_eq!(row.compaction_count, 0);
        assert!(!row.compaction_retired);
        assert_eq!(
            (row.status, row.phase),
            (AgentStatus::Success, TurnPhase::Idle)
        );
        assert_eq!(
            (row.turn_started_at, row.user_turn_started_at),
            (Some(at), Some(at))
        );
        assert_eq!(
            row.last_activity,
            at + jiff::SignedDuration::from_secs(late_index)
        );
        assert_eq!(row.tool_calls.get("Bash"), Some(&3));
        let cache = store.paths().rollup_cache().unwrap();
        if cache.exists() {
            std::fs::remove_file(cache).unwrap();
        }
        let (_, replayed, _) = crate::store::snapshot::RollupCursor::new()
            .fold(store.paths())
            .unwrap();
        let replayed = replayed.iter().next().unwrap();
        assert_eq!(decide_row(replayed.clone()), expected);
        assert_eq!(replayed.compacting_since.is_some(), compacting);
        assert_eq!(replayed.compaction_count, 0);
        let fresh = append(late_index + 1, "PostToolUse", tool("fresh-turn"));
        assert!(fresh.transition.unwrap().opened_turn);
        let row = store.snapshot().unwrap().agents[0].clone();
        assert!(row.compacting_since.is_none());
        assert_eq!(row.compaction_count, u32::from(compacting));
        assert_eq!(
            decide_row(row),
            Err(vec![("child".into(), OpenTurn::Working)])
        );
    }

    #[test]
    fn a_stamped_row_with_a_dead_owner_does_not_occupy() {
        let stamped = agent("stamped", AgentStatus::Running, "/repo/wt");
        assert_eq!(
            decide_in_with_liveness(&[stamped], &["/repo/wt"], &[], |_| AgentLiveness::Dead),
            Ok(vec![])
        );
    }

    #[test]
    fn a_stamped_row_with_a_live_owner_occupies() {
        let stamped = agent("stamped", AgentStatus::Running, "/repo/wt");
        assert_eq!(
            decide_in_with_liveness(&[stamped], &["/repo/wt"], &[], |_| AgentLiveness::Live {
                pid: 42
            }),
            Err(vec![("stamped".to_owned(), OpenTurn::Working)])
        );
    }

    #[test]
    fn a_stamped_row_with_an_unknown_owner_occupies() {
        let stamped = agent("stamped", AgentStatus::Running, "/repo/wt");
        assert_eq!(
            decide_in_with_liveness(&[stamped], &["/repo/wt"], &[], |_| AgentLiveness::Unknown),
            Err(vec![("stamped".to_owned(), OpenTurn::Working)])
        );
    }

    #[test]
    fn an_open_turn_in_a_child_without_a_pane_blocks_its_parent() {
        let parent = agent("parent", AgentStatus::Idle, "/repo/wt");
        let mut child = child_of(&parent, "child", AgentStatus::Running, "/elsewhere");
        child.pane = None;
        assert_eq!(
            decide_in_with_liveness(&[parent, child], &["/repo/wt"], &[], |agent| {
                if agent.agent_id.as_str() == "child" {
                    AgentLiveness::Dead
                } else {
                    AgentLiveness::Unknown
                }
            }),
            Err(vec![("child".to_owned(), OpenTurn::Working)])
        );
    }

    #[test]
    fn each_status_either_settles_or_blocks() {
        let mut sleeping = agent("a", AgentStatus::Idle, "/repo/wt");
        sleeping.pending_waits.push(PendingWait {
            class: crate::agents::PendingWaitClass::Working,
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
            reset_window: None,
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
    fn a_symlinked_checkout_counts_both_path_forms_and_keeps_nested_worktrees_out() {
        let decide = |agents: &[AgentState]| {
            decide_in(
                agents,
                &["/alias/wt", "/real/wt"],
                &[
                    "/alias/wt",
                    "/real/wt",
                    "/alias/wt/.worktrees/lane",
                    "/real/wt/.worktrees/lane",
                ],
            )
        };
        let lexical = agent("lexical", AgentStatus::Idle, "/alias/wt");
        let physical = agent("physical", AgentStatus::Idle, "/real/wt/docs");
        let nested_lexical = agent(
            "nested-lexical",
            AgentStatus::Running,
            "/alias/wt/.worktrees/lane",
        );
        let nested_physical = agent(
            "nested-physical",
            AgentStatus::Running,
            "/real/wt/.worktrees/lane/src",
        );
        let settled = [lexical, physical, nested_lexical, nested_physical];
        assert_eq!(
            decide(&settled),
            Ok(vec!["lexical".to_owned(), "physical".to_owned()])
        );

        let [lexical, mut physical, ..] = settled;
        physical.status = AgentStatus::Running;
        assert_eq!(
            decide(&[lexical, physical]),
            Err(vec![("physical".to_owned(), OpenTurn::Working)])
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
            "@coder is working, @fox is waiting on you, @owl is paused on a limit, @elk is compacting"
        );
    }
}
