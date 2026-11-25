use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::agents::{
    AgentState, AgentStatus, PendingWait, PendingWaitClass, PendingWaitTarget, PendingWaitTrigger,
    TurnPhase,
};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::store::snapshot::{SidebarRow, SidebarSubAgent};

fn now() -> Timestamp {
    "2026-11-16T12:00:00Z".parse().unwrap()
}

fn row(agent: &AgentState, pane_at: usize) -> SidebarRow {
    let mut row = activity_row(true, Some(agent.status), now(), Path::new("/w"));
    row.id = agent.agent_id.to_string();
    row.name = agent.kind.to_string();
    row.pane = Some(pane(&format!("terminal_{pane_at}"), "claude", "/w"));
    if let Some(card) = row.as_agent_mut() {
        card.handle = agent.role.clone();
        card.phase = agent.phase;
        card.description = agent.description.clone();
    }
    row
}

fn lane(name: Option<&str>) -> Option<crate::ids::Lane> {
    let own = crate::ids::Lane::own(crate::ids::LaneHome::Trunk, "/w");
    Some(match name {
        Some(name) => own.named(crate::ids::LaneName::dashed(name)),
        None => own,
    })
}

fn child_entry(agent: &AgentState, prior_turn: bool) -> SidebarSubAgent {
    SidebarSubAgent {
        pane: None,
        first_prompt: None,
        finished_at: None,
        turn_error_label: None,
        id: agent.agent_id.to_string(),
        kind: agent.kind.clone(),
        prior_turn,
        reopenable: false,
        name: "coder".to_owned(),
        petname: None,
        provider_native: false,
        fast: false,
        stalled: false,
        compacting: false,
        status: agent.status,
        phase: agent.phase,
        task: None,
        profile: None,
        lane: None,
        model: None,
        effort: None,
        description: None,
        tokens: None,
        context_window: None,
        compaction_count: 0,
        cost_usd: None,
        elapsed_secs: None,
        started_at: None,
        rested_at: None,
        last_activity: now(),
        registered_at: None,
    }
}

fn wait(name: &str, target: Option<&AgentState>, settled: Option<&str>) -> PendingWait {
    PendingWait {
        name: name.to_owned(),
        trigger: PendingWaitTrigger::Subagent {
            active_at: now(),
            target: target.map(|agent| PendingWaitTarget {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
            }),
            deadline_at: None,
            settled: settled.map(ToOwned::to_owned),
        },
        armed_at: Some(now() - SignedDuration::from_secs(600)),
        class: PendingWaitClass::Working,
    }
}

/// A launcher waiting on a nested child it lists, a child of an earlier turn, a peer
/// with its own card, an agent with no row, and a wait recorded without its target. An
/// entry with a row carries the record's profile, its lane only when named, and its card's
/// activity text; one without a row carries none of them.
#[test]
fn awaited_agents_are_the_waits_without_a_child_entry() {
    let launcher = root_agent("claude", "launcher", None);
    let child = root_agent("claude", "child", None);
    let earlier = AgentState {
        status: AgentStatus::Success,
        role: Some("earlier".to_owned()),
        profile: Some("reviewer".to_owned()),
        lane: lane(None),
        ..root_agent("claude", "earlier", None)
    };
    let peer = AgentState {
        role: Some("tidy-iris".to_owned()),
        phase: TurnPhase::Acting,
        profile: Some("coder".to_owned()),
        lane: lane(Some("feat-x")),
        description: Some("store refactor".to_owned()),
        ..root_agent("codex", "peer", None)
    };
    let ghost = AgentState {
        profile: Some("coder".to_owned()),
        ..root_agent("claude", "ghost", None)
    };
    let mut launcher_row = row(&launcher, 0);
    if let Some(card) = launcher_row.as_agent_mut() {
        card.sub_agents = vec![child_entry(&child, false), child_entry(&earlier, true)];
        card.pending_waits = vec![
            wait("@child", Some(&child), None),
            wait("@earlier", Some(&earlier), Some("completed")),
            wait("@tidy-iris", Some(&peer), None),
            wait("@ghost", Some(&ghost), None),
            wait("@old", None, None),
        ];
    }
    let mut peer_row = row(&peer, 2);
    if let Some(card) = peer_row.as_agent_mut() {
        card.compacting = true;
    }
    let rows = vec![launcher_row, row(&earlier, 1), peer_row];
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/w")),
        vec![launcher, child, earlier, peer, ghost],
        now(),
    );
    snapshot.worktree_groups = vec![worktree_group(Path::new("/w"), rows)].into();
    attach(&mut snapshot);
    let since = Some(now() - SignedDuration::from_secs(600));
    let card = snapshot.worktree_groups[0].rows[0].as_agent().unwrap();
    assert_eq!(
        card.awaited,
        [
            SidebarAwaitedAgent {
                status: Some(AgentStatus::Success),
                phase: TurnPhase::Idle,
                compacting: false,
                pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
                since,
                settled: Some("completed".to_owned()),
                profile: Some("reviewer".to_owned()),
                lane: None,
                description: None,
            },
            SidebarAwaitedAgent {
                status: Some(AgentStatus::Running),
                phase: TurnPhase::Acting,
                compacting: true,
                pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_2")),
                since,
                settled: None,
                profile: Some("coder".to_owned()),
                lane: Some("feat-x".to_owned()),
                description: Some("store refactor".to_owned()),
            },
            SidebarAwaitedAgent {
                status: None,
                phase: TurnPhase::Idle,
                compacting: false,
                pane: None,
                since,
                settled: None,
                profile: None,
                lane: None,
                description: None,
            },
        ]
    );
    let peer_card = snapshot.worktree_groups[0].rows[2].as_agent().unwrap();
    assert!(
        peer_card.awaited.is_empty(),
        "a card waiting on nothing awaits nothing"
    );
    let before = snapshot.worktree_groups.clone();
    attach(&mut snapshot);
    assert!(
        Arc::ptr_eq(&before, &snapshot.worktree_groups),
        "an unchanged list copies nothing"
    );
}

/// A peer whose settled turn still owes its report while the launcher's next turn is open
/// carries one wait per run, both naming the same row: it is one awaited agent, drawn live.
#[test]
fn a_peer_awaited_through_two_runs_is_one_entry_in_its_live_state() {
    let launcher = root_agent("claude", "launcher", None);
    let peer = AgentState {
        role: Some("tidy-iris".to_owned()),
        phase: TurnPhase::Acting,
        ..root_agent("codex", "peer", None)
    };
    let mut launcher_row = row(&launcher, 0);
    if let Some(card) = launcher_row.as_agent_mut() {
        card.pending_waits = vec![
            wait("@tidy-iris", Some(&peer), Some("completed")),
            wait("@tidy-iris", Some(&peer), None),
        ];
    }
    let rows = vec![launcher_row, row(&peer, 2)];
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/w")),
        vec![launcher, peer],
        now(),
    );
    snapshot.worktree_groups = vec![worktree_group(Path::new("/w"), rows)].into();
    attach(&mut snapshot);
    let card = snapshot.worktree_groups[0].rows[0].as_agent().unwrap();
    assert_eq!(
        card.awaited,
        [SidebarAwaitedAgent {
            status: Some(AgentStatus::Running),
            phase: TurnPhase::Acting,
            compacting: false,
            pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_2")),
            since: Some(now() - SignedDuration::from_secs(600)),
            settled: None,
            profile: None,
            lane: None,
            description: None,
        }]
    );
}
