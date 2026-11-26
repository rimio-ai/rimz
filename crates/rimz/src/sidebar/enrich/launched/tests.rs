use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::agents::{AgentState, AgentStatus, LaunchedBy};
use crate::ids::{AgentKind, WorkspaceId};
use crate::sidebar::refresh::launched::LaunchedFacts;
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::store::snapshot::{BookingPr, BookingStarts, SidebarLaunched, SidebarRow};

fn now() -> Timestamp {
    "2026-11-26T12:00:00Z".parse().unwrap()
}

fn ago(secs: i64) -> Timestamp {
    now() - SignedDuration::from_secs(secs)
}

fn agent(id: &str) -> AgentState {
    AgentState {
        launch_id: Some(id.into()),
        registered_at: Some(ago(3_600)),
        ..root_agent("claude", id, None)
    }
}

fn row(agent: &AgentState, pane_at: usize) -> SidebarRow {
    let mut row = activity_row(true, Some(agent.status), now(), Path::new("/w"));
    row.id = agent.agent_id.to_string();
    row.name = agent.kind.to_string();
    row.pane = Some(pane(&format!("terminal_{pane_at}"), "claude", "/w"));
    if let Some(card) = row.as_agent_mut() {
        card.description = agent.description.clone();
    }
    row
}

/// A fold holding `agents`, each with a row in one group unless it has ended.
fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    let rows = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| agent.ended_at.is_none())
        .map(|(index, agent)| row(agent, index))
        .collect();
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/w")),
        agents,
        now(),
    );
    snapshot.worktree_groups = vec![worktree_group(Path::new("/w"), rows)].into();
    snapshot
}

fn link(id: &str) -> LaunchedBy {
    LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: id.into(),
    }
}

fn facts(launcher: &str, starts: BookingStarts, launches: &[&str]) -> LaunchedFacts {
    LaunchedFacts {
        launched_by: link(launcher),
        starts,
        lane: None,
        description: None,
        leader: None,
        team: None,
        launches: launches.iter().map(|id| (*id).to_owned()).collect(),
        at: ago(600),
        ended_at: None,
        branch: None,
        checkout: None,
        pr: BookingPr::Unread,
    }
}

fn coder() -> BookingStarts {
    BookingStarts::Profile("coder".to_owned())
}

fn cache(launched: Vec<LaunchedFacts>) -> LaunchedCache {
    LaunchedCache {
        version: 1,
        launched,
    }
}

/// Each agent row's launched entries, by session id.
fn on_cards(
    snapshot: &SidebarSnapshot,
) -> std::collections::BTreeMap<String, Vec<SidebarLaunched>> {
    snapshot.worktree_groups[0]
        .rows
        .iter()
        .filter_map(|row| Some((row.id.clone(), row.as_agent()?.launched.clone())))
        .filter(|(_, launched)| !launched.is_empty())
        .collect()
}

/// A running peer, a team whose seats are alive, and a peer that ended land on their
/// launcher's card in cache order: the live ones with their status and the pane a click
/// focuses, the ended one with nothing alive and its end.
#[test]
fn launched_entries_land_on_the_launchers_card_with_what_they_have_alive() {
    let peer = AgentState {
        status: AgentStatus::Waiting,
        description: Some("store refactor".to_owned()),
        ..agent("peer")
    };
    let seat = |id: &str| AgentState {
        team: Some("forge".to_owned()),
        ..agent(id)
    };
    let mut fold = snapshot(vec![agent("lead"), peer, seat("seat-a"), seat("seat-b")]);
    let ended = LaunchedFacts {
        ended_at: Some(ago(60)),
        ..facts("lead", coder(), &["gone"])
    };
    attach(
        &mut fold,
        &cache(vec![
            facts("lead", coder(), &["peer"]),
            facts(
                "lead",
                BookingStarts::Team("forge".to_owned()),
                &["seat-a", "seat-b"],
            ),
            ended,
            facts("nobody", coder(), &["peer"]),
        ]),
    );
    let cards = on_cards(&fold);
    assert_eq!(cards.keys().collect::<Vec<_>>(), ["lead"]);
    let launched = &cards["lead"];
    assert_eq!(launched.len(), 3);
    let live = launched[0].live.as_ref().expect("the peer is alive");
    assert_eq!(live.status, AgentStatus::Waiting);
    assert_eq!(live.description.as_deref(), Some("store refactor"));
    assert_eq!(
        launched[0].live_pane().map(ToString::to_string),
        fold.worktree_groups[0].rows[1]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.to_string())
    );
    assert!(launched[1].live.is_some(), "the team's seats are alive");
    assert!(launched[1].live.as_ref().unwrap().description.is_none());
    assert!(launched[2].is_finished());
    assert_eq!(launched[2].ended_at, Some(ago(60)));
}

#[test]
fn a_launched_entry_follows_its_launcher_across_a_swap() {
    let replaced = agent("lead");
    let holder = AgentState {
        launch_id: Some("lead".into()),
        epoch: 1,
        ..agent("swapped")
    };
    let mut fold = snapshot(vec![replaced, holder, agent("peer")]);
    attach(&mut fold, &cache(vec![facts("lead", coder(), &["peer"])]));
    assert_eq!(
        on_cards(&fold).keys().collect::<Vec<_>>(),
        ["swapped"],
        "the holder at the next epoch carries the entry; the replaced row does not"
    );
}

/// A successor copies its predecessor's link: an entry naming the card's own launch never
/// lists on that card.
#[test]
fn a_card_never_lists_its_own_launch() {
    let mut fold = snapshot(vec![agent("lead"), agent("peer")]);
    attach(&mut fold, &cache(vec![facts("peer", coder(), &["peer"])]));
    assert!(on_cards(&fold).is_empty());
}

/// With no seat asking, a team entry focuses the seat holding its configured leader role,
/// as `rimz teams focus` does, not its first seat; it carries its team instance.
#[test]
fn a_team_entry_focuses_its_leader_seat() {
    let seat = |id: &str, role: &str| AgentState {
        team: Some("forge".to_owned()),
        role: Some(role.to_owned()),
        ..agent(id)
    };
    let mut fold = snapshot(vec![
        agent("lead"),
        seat("seat-a", "coder"),
        seat("seat-b", "reviewer"),
    ]);
    let instance: crate::ids::TeamInstanceId = "forge#external".parse().unwrap();
    attach(
        &mut fold,
        &cache(vec![LaunchedFacts {
            leader: Some("reviewer".to_owned()),
            team: Some(instance.clone()),
            ..facts(
                "lead",
                BookingStarts::Team("forge".to_owned()),
                &["seat-a", "seat-b"],
            )
        }]),
    );
    let launched = &on_cards(&fold)["lead"][0];
    assert_eq!(
        launched.live_pane().map(ToString::to_string),
        fold.worktree_groups[0].rows[2]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.to_string())
    );
    assert_eq!(launched.team, Some(instance));
}

/// A seat the lane read alive whose card has not landed yet reads starting, not finished;
/// one that left the fold's rows since the lane's pass reads finished.
#[test]
fn a_launch_whose_card_has_not_landed_reads_starting() {
    let mut fold = snapshot(vec![agent("lead"), agent("fresh")]);
    let groups = std::sync::Arc::make_mut(&mut fold.worktree_groups);
    std::sync::Arc::make_mut(&mut groups[0].rows).retain(|row| row.id != "fresh");
    attach(
        &mut fold,
        &cache(vec![
            facts("lead", coder(), &["fresh"]),
            facts("lead", coder(), &["left"]),
        ]),
    );
    let launched = &on_cards(&fold)["lead"];
    assert!(launched[0].live.is_none());
    assert!(launched[0].starting && !launched[0].is_finished());
    assert!(!launched[1].starting && launched[1].is_finished());
}

/// An entry the launcher no longer holds leaves its card.
#[test]
fn a_card_drops_an_entry_the_cache_no_longer_holds() {
    let mut fold = snapshot(vec![agent("lead"), agent("peer")]);
    attach(&mut fold, &cache(vec![facts("lead", coder(), &["peer"])]));
    assert_eq!(on_cards(&fold)["lead"].len(), 1);
    attach(&mut fold, &cache(Vec::new()));
    assert!(on_cards(&fold).is_empty());
}
