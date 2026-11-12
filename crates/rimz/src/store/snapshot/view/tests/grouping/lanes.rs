//! Launch lanes as the sidebar groups them: each row's lane is decided by
//! `harness::lanes::decide` over a real project with one managed worktree.

use super::*;
use crate::harness::lanes::tests::{Room, init_repo, request, room};
use crate::harness::lanes::{CallerLane, LaneRequest, decide};
use crate::ids::Lane;
use std::path::Path;

/// A row seated in `lane`, with a live pane in the lane's directory.
fn seat(id: &str, lane: Lane, cwd: &Path) -> (AgentState, PaneRef) {
    let raw = format!("%{id}");
    let mut agent = agent_in(id, &cwd.display().to_string(), AgentStatus::Idle, 0).in_pane(&raw);
    agent.lane = Some(lane);
    agent.project_lane();
    (agent, pane(&raw, "claude", &cwd.display().to_string()))
}

/// A seat whose registration reported a branch, as a hook in a Git checkout does.
fn branched((agent, pane): (AgentState, PaneRef)) -> (AgentState, PaneRef) {
    (agent.branch("main"), pane)
}

fn decided(request: &LaneRequest<'_>) -> Lane {
    decide(request).expect("admitted lane")
}

/// Each group's key beside the ids of its rows, sorted.
fn groups(room: &Room, seats: Vec<(AgentState, PaneRef)>) -> BTreeSet<(String, Vec<String>)> {
    let (agents, panes): (Vec<_>, Vec<_>) = seats.into_iter().unzip();
    let snapshot = super::room(agents)
        .with_project_root(Some(room.root.clone()))
        .with_worktree_roots(vec![room.root.clone(), room.auth.clone()], BTreeMap::new())
        .with_live_panes(panes, None);
    snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            let mut ids = group
                .rows
                .iter()
                .map(|row| {
                    row.id
                        .as_str()
                        .rsplit(':')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            ids.sort();
            (group.key.clone(), ids)
        })
        .collect()
}

fn key(path: &Path) -> String {
    path.display().to_string()
}

fn group(key: impl Into<String>, ids: &[&str]) -> (String, Vec<String>) {
    (key.into(), ids.iter().map(|id| (*id).to_owned()).collect())
}

#[test]
fn a_typed_main_at_trunk_shares_trunks_one_group() {
    let room = room();
    let main = decided(&LaneRequest {
        typed: Some("main"),
        ..request(&room, &room.root)
    });
    let plain = decided(&request(&room, &room.root));

    assert_eq!(
        groups(
            &room,
            vec![
                seat("stamped", main, &room.root),
                seat("desk", plain, &room.root)
            ]
        ),
        BTreeSet::from([group(key(&room.root), &["desk", "stamped"])])
    );
}

#[test]
fn a_child_lands_at_its_target_checkout_and_keeps_its_callers_named_lane_only_there() {
    let room = room();
    let worktree_caller = decided(&request(&room, &room.auth));
    let to_root = decided(&LaneRequest {
        caller: Some(CallerLane::Agent(worktree_caller.clone())),
        ..request(&room, &room.root)
    });
    let design = decided(&LaneRequest {
        typed: Some("design"),
        ..request(&room, &room.root)
    });
    let design_child = decided(&LaneRequest {
        caller: Some(CallerLane::Agent(design.clone())),
        ..request(&room, &room.root)
    });
    let design_child_to_worktree = decided(&LaneRequest {
        caller: Some(CallerLane::Agent(design.clone())),
        ..request(&room, &room.auth)
    });

    assert_eq!(
        groups(
            &room,
            vec![
                seat("wtcaller", worktree_caller, &room.auth),
                seat("rootchild", to_root, &room.root),
                seat("designer", design, &room.root),
                seat("designchild", design_child, &room.root),
                seat("wtchild", design_child_to_worktree, &room.auth),
            ]
        ),
        BTreeSet::from([
            group(key(&room.root), &["rootchild"]),
            group("channel:design", &["designchild", "designer"]),
            group("channel:auth", &["wtcaller", "wtchild"]),
        ])
    );
}

#[test]
fn a_pinned_human_launch_carries_its_pin_only_at_its_checkout_while_admissible() {
    let room = room();
    let pin = || Some(CallerLane::Pin(trunk_design(&room)));
    let at_trunk = decided(&LaneRequest {
        caller: pin(),
        ..request(&room, &room.root)
    });
    let at_worktree = decided(&LaneRequest {
        caller: pin(),
        ..request(&room, &room.auth)
    });
    let (holder, holder_pane) = seat(
        "holder",
        Lane::own(crate::ids::LaneHome::Worktree, key(&room.auth))
            .named("design".parse().expect("name")),
        &room.auth,
    );
    let held_elsewhere = decided(&LaneRequest {
        caller: pin(),
        agents: std::slice::from_ref(&holder),
        ..request(&room, &room.root)
    });

    assert_eq!(
        groups(
            &room,
            vec![
                seat("pinned", at_trunk, &room.root),
                seat("away", at_worktree, &room.auth),
            ]
        ),
        BTreeSet::from([
            group("channel:design", &["pinned"]),
            group("channel:auth", &["away"]),
        ])
    );
    assert_eq!(
        groups(
            &room,
            vec![
                (holder, holder_pane),
                seat("refused", held_elsewhere, &room.root)
            ]
        ),
        BTreeSet::from([
            group(key(&room.root), &["refused"]),
            group("channel:design", &["holder"]),
        ])
    );
}

fn trunk_design(room: &Room) -> Lane {
    Lane::own(crate::ids::LaneHome::Trunk, key(&room.root)).named("design".parse().expect("name"))
}

#[test]
fn a_subdirectory_launch_groups_with_its_checkout_and_an_outside_one_by_its_git_root() {
    let room = room();
    let root_sub = room.root.join("sub");
    let worktree_sub = room.auth.join("sub");
    let outside = init_repo(&room.parent.join("elsewhere"));
    let bare = room.parent.join("bare");
    for dir in [&root_sub, &worktree_sub, &bare] {
        std::fs::create_dir_all(dir).expect("dir");
    }

    assert_eq!(
        groups(
            &room,
            vec![
                seat("rootsub", decided(&request(&room, &root_sub)), &root_sub),
                seat(
                    "wtsub",
                    decided(&request(&room, &worktree_sub)),
                    &worktree_sub
                ),
                branched(seat(
                    "outside",
                    decided(&request(&room, &outside)),
                    &outside
                )),
                seat("bare", decided(&request(&room, &bare)), &bare),
            ]
        ),
        BTreeSet::from([
            group(key(&room.root), &["rootsub"]),
            group("channel:auth", &["wtsub"]),
            group(key(&outside), &["outside"]),
            group("external", &["bare"]),
        ])
    );
}

#[test]
fn an_in_place_team_names_its_lane_at_trunk_and_takes_a_worktrees_own() {
    let room = room();
    let leader = decided(&LaneRequest {
        team: Some("forge"),
        ..request(&room, &room.root)
    });
    let joiner = decided(&LaneRequest {
        joins_cohort: true,
        caller: Some(CallerLane::Agent(leader.clone())),
        ..request(&room, &room.root)
    });
    let in_worktree = decided(&LaneRequest {
        team: Some("forge"),
        ..request(&room, &room.auth)
    });

    assert_eq!(
        groups(
            &room,
            vec![
                seat("leader", leader, &room.root),
                seat("joiner", joiner, &room.root),
                seat("wtteam", in_worktree, &room.auth),
            ]
        ),
        BTreeSet::from([
            group("channel:repo/forge", &["joiner", "leader"]),
            group("channel:auth", &["wtteam"]),
        ])
    );
}
