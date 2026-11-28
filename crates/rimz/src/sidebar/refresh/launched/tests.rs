use std::path::{Path, PathBuf};

use jiff::SignedDuration;

use super::*;
use crate::ids::{AgentKind, FireId, Lane, LaneHome, LaneName, TaskId, WorkspaceId};

fn now() -> Timestamp {
    "2026-11-26T12:00:00Z".parse().unwrap()
}

fn ago(secs: i64) -> Timestamp {
    now() - SignedDuration::from_secs(secs)
}

fn link(id: &str) -> LaunchedBy {
    LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: id.into(),
    }
}

/// A live row `id` that `launcher` launched at `registered` seconds ago, its launch id
/// `<id>-launch`.
fn peer(id: &str, launcher: &str, registered: i64) -> AgentState {
    AgentState {
        launch_id: Some(format!("{id}-launch").as_str().into()),
        launched_by: Some(link(launcher)),
        registered_at: Some(ago(registered)),
        profile: Some("coder".to_owned()),
        ..crate::sidebar::test_support::root_agent("claude", id, None)
    }
}

fn ended(agent: AgentState, secs_ago: i64) -> AgentState {
    AgentState {
        ended_at: Some(ago(secs_ago)),
        ..agent
    }
}

fn lane(name: &str) -> Option<Lane> {
    Some(Lane::own(LaneHome::Trunk, "/w").named(LaneName::dashed(name)))
}

fn seat(id: &str, launcher: &str, registered: i64) -> AgentState {
    AgentState {
        team: Some("forge".to_owned()),
        lane: lane("feat"),
        ..peer(id, launcher, registered)
    }
}

fn room_dirs(dir: &Path) -> (PathBuf, RuntimePaths) {
    let root = dir.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&root), dir).unwrap();
    runtime.ensure_dirs().unwrap();
    (root, runtime)
}

/// A linked checkout whose `.git` file leads to a RimZ marker naming `branch`.
fn marked_tree(dir: &Path, name: &str, branch: &str, root: &Path) -> PathBuf {
    let tree = dir.join(name);
    let admin = dir.join("admin").join(name);
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    let marker = crate::worktree::WorktreeMarker {
        loop_fire: None,
        external: None,
        version: 4,
        name: name.to_owned(),
        branch: branch.to_owned(),
        base_branch: None,
        from_pr: None,
        base_ref: "main".to_owned(),
        repo_root: root.to_path_buf(),
        worktree_path: tree.clone(),
        created_at: "2026-11-26T10:00:00Z".parse().unwrap(),
    };
    std::fs::write(
        admin.join("rimz-worktree.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    tree
}

/// One pass over `rows` with no prior publication.
fn pass(rows: &[AgentState], root: &Path, runtime: &RuntimePaths) -> Vec<LaunchedFacts> {
    pass_after(
        rows,
        root,
        runtime,
        &LaunchedCache::default(),
        &mut BTreeSet::new(),
    )
}

fn pass_after(
    rows: &[AgentState],
    root: &Path,
    runtime: &RuntimePaths,
    prior: &LaunchedCache,
    expired: &mut BTreeSet<Vec<String>>,
) -> Vec<LaunchedFacts> {
    let room = Room {
        rows,
        source: &CiSource::read(runtime),
        root,
        now: now(),
        window_secs: 900,
    };
    collect(&room, prior, expired)
}

fn names(facts: &[LaunchedFacts]) -> Vec<(String, Option<&str>, Vec<&str>)> {
    facts
        .iter()
        .map(|facts| {
            let starts = match &facts.starts {
                BookingStarts::Team(name) => format!("team {name}"),
                BookingStarts::Profile(name) => name.clone(),
            };
            (
                starts,
                facts.lane.as_deref(),
                facts.launches.iter().map(String::as_str).collect(),
            )
        })
        .collect()
}

/// A solo peer lists once across its molt (two rows on one launch id), a team once for all
/// its seats, each under its launcher in launch order; a `rimz subagents` child and a
/// booked fire's launch never list, whatever link they carry.
#[test]
fn a_peer_lists_once_across_a_molt_and_a_team_once_for_its_seats() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let molted = AgentState {
        launch_id: Some("solo-launch".into()),
        description: Some("the replaced row's text".to_owned()),
        ..ended(peer("solo-old", "lead", 600), 300)
    };
    let successor = AgentState {
        launch_id: Some("solo-launch".into()),
        epoch: 1,
        description: Some("store refactor".to_owned()),
        ..peer("solo-new", "lead", 300)
    };
    let child = AgentState {
        parent_agent_id: Some("lead".into()),
        launch_depth: Some(1),
        ..peer("child", "lead", 500)
    };
    let fired = AgentState {
        loop_fire: Some(FireId::new(&TaskId::new())),
        ..peer("fired", "lead", 500)
    };
    let plain = AgentState {
        launched_by: None,
        ..peer("plain", "lead", 500)
    };
    // In Audit order (by kind, then agent id): the successor sorts before the row it
    // replaced.
    let rows = [
        child,
        fired,
        peer("other", "someone", 100),
        plain,
        seat("seat-a", "lead", 450),
        seat("seat-b", "lead", 400),
        successor,
        molted,
    ];
    let facts = pass(&rows, &root, &runtime);
    assert_eq!(
        names(&facts),
        [
            ("coder".to_owned(), None, vec!["solo-launch"]),
            (
                "team forge".to_owned(),
                Some("feat"),
                vec!["seat-a-launch", "seat-b-launch"]
            ),
            ("coder".to_owned(), None, vec!["other-launch"]),
        ]
    );
    assert_eq!(facts[0].launched_by, link("lead"));
    assert_eq!(facts[2].launched_by, link("someone"));
    assert_eq!(facts[0].at, ago(600), "the first registration");
    assert_eq!(facts[0].ended_at, None, "its successor is alive");
    assert_eq!(
        facts[0].description.as_deref(),
        Some("store refactor"),
        "the newest row's text"
    );
    assert_eq!(facts[1].at, ago(450));
    assert_eq!(facts[1].team, Some("forge#feat".parse().unwrap()));
    assert_eq!(facts[0].team, None);
}

/// A peer swapped to another kind reads as its newest row: the Audit rows list the
/// replaced `claude` row before its `codex` holder.
#[test]
fn a_swapped_peer_reads_its_newest_row() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let replaced = AgentState {
        profile: None,
        ..ended(peer("swap", "lead", 600), 300)
    };
    let holder = AgentState {
        kind: AgentKind::new_unchecked("codex"),
        launch_id: Some("swap-launch".into()),
        epoch: 1,
        profile: None,
        ..peer("swap-new", "lead", 300)
    };
    let facts = pass(&[replaced, holder], &root, &runtime);
    assert_eq!(
        names(&facts),
        [("codex".to_owned(), None, vec!["swap-launch"])]
    );
}

/// A launch found past its lifetime lists again once one of its rows is alive (a resumed
/// peer keeps its launch id), and keeps its window after it ends again.
#[test]
fn an_expired_launch_lists_again_while_a_row_of_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let launches = vec!["resumed-launch".to_owned()];
    let mut expired = BTreeSet::from([launches.clone()]);
    let resumed = [peer("resumed", "lead", 7_200)];
    let facts = pass_after(
        &resumed,
        &root,
        &runtime,
        &LaunchedCache::default(),
        &mut expired,
    );
    assert_eq!(
        names(&facts),
        [("coder".to_owned(), None, vec!["resumed-launch"])]
    );
    assert!(expired.is_empty(), "{expired:?}");
    let ended_again = [ended(peer("resumed", "lead", 7_200), 60)];
    let facts = pass_after(
        &ended_again,
        &root,
        &runtime,
        &LaunchedCache::default(),
        &mut expired,
    );
    assert_eq!(facts.len(), 1, "its window after the new end holds");
}

/// Once a worktree peer's checkout is removed, the entry keeps the branch and tree the last
/// publication read, so it ends with its checkout instead of lingering as treeless.
#[test]
fn a_removed_checkout_ends_the_entry_with_the_branch_the_last_publication_read() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let tree = marked_tree(dir.path(), "feat", "feat-branch", &root);
    let rows = [AgentState {
        worktree_path: Some(tree.display().to_string()),
        ..ended(peer("feat", "lead", 7_200), 600)
    }];
    let prior = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched: pass(&rows, &root, &runtime),
    };
    assert_eq!(prior.launched[0].branch.as_deref(), Some("feat-branch"));
    std::fs::remove_dir_all(&tree).unwrap();
    let mut expired = BTreeSet::new();
    let facts = pass_after(&rows, &root, &runtime, &prior, &mut expired);
    assert!(facts.is_empty(), "{facts:?}");
    assert!(expired.contains(&vec!["feat-launch".to_owned()]));
}

/// A launched entry keeps a booked fire's lifetime: one with no worktree for its window after
/// its last seat's end, one in a RimZ worktree until its pull request merges or its
/// checkout is gone. Each found past it is remembered as expired and never read again.
#[test]
fn a_launched_entry_outlives_its_end_for_the_booking_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let tree = |name: &str| {
        marked_tree(dir.path(), name, &format!("{name}-branch"), &root)
            .display()
            .to_string()
    };
    let in_tree = |id: &str, path: String| AgentState {
        worktree_path: Some(path),
        ..ended(peer(id, "lead", 7_200), 3_600)
    };
    let gone_tree = tree("gone");
    let rows = [
        ended(peer("recent", "lead", 7_200), 600),
        ended(peer("old", "lead", 7_200), 1_200),
        in_tree("kept", tree("kept")),
        in_tree("gone", gone_tree.clone()),
        AgentState {
            team: Some("forge".to_owned()),
            lane: lane("team"),
            ..ended(peer("team-a", "lead", 7_200), 5_000)
        },
        AgentState {
            team: Some("forge".to_owned()),
            lane: lane("team"),
            ..ended(peer("team-b", "lead", 7_200), 300)
        },
    ];
    std::fs::remove_dir_all(&gone_tree).unwrap();
    let mut expired = BTreeSet::new();
    let facts = pass_after(
        &rows,
        &root,
        &runtime,
        &LaunchedCache::default(),
        &mut expired,
    );
    let kept: Vec<_> = facts
        .iter()
        .map(|facts| {
            (
                facts.launches[0].as_str(),
                facts.ended_at,
                facts.branch.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("recent-launch", Some(ago(600)), None),
            ("kept-launch", Some(ago(3_600)), Some("kept-branch")),
            ("team-a-launch", Some(ago(300)), None),
        ],
        "a team ends with its last seat"
    );
    assert_eq!(
        expired,
        BTreeSet::from([
            vec!["gone-launch".to_owned()],
            vec!["old-launch".to_owned()]
        ])
    );

    let merged = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched: facts
            .iter()
            .map(|facts| LaunchedFacts {
                pr: BookingPr::Merged { number: Some(7) },
                ..facts.clone()
            })
            .collect(),
    };
    let after = pass_after(&rows, &root, &runtime, &merged, &mut expired);
    assert_eq!(
        after
            .iter()
            .map(|facts| facts.launches[0].as_str())
            .collect::<Vec<_>>(),
        ["recent-launch", "team-a-launch"],
        "a merged pull request ends the worktree entry, carried while the lane reads none"
    );
    assert!(expired.contains(&vec!["kept-launch".to_owned()]));
}

/// An entry whose rows left the Audit rows stays from the last publication while its
/// lifetime holds, and is gone after.
#[test]
fn an_entry_whose_rows_are_gone_keeps_its_last_publication_for_its_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let rows = [
        ended(peer("recent", "lead", 7_200), 600),
        ended(peer("old", "lead", 7_200), 600),
    ];
    let mut prior = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched: pass(&rows, &root, &runtime),
    };
    prior.launched[1].ended_at = Some(ago(1_200));
    let facts = pass_after(&rows[..0], &root, &runtime, &prior, &mut BTreeSet::new());
    assert_eq!(facts, prior.launched[..1]);
}

/// A peer that launched and ended before any pass saw it has no row in the fold, so any
/// agent in the room opens the read.
#[test]
fn only_a_room_with_an_agent_or_a_published_entry_reads_the_audit_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let fold = |agents: Vec<AgentState>| {
        SidebarSnapshot::build_with_agents(WorkspaceId::from_project_root(&root), agents, now())
    };
    let plain = AgentState {
        launched_by: None,
        ..peer("plain", "lead", 60)
    };
    let child = AgentState {
        parent_agent_id: Some("lead".into()),
        launch_depth: Some(1),
        ..peer("child", "lead", 60)
    };
    let empty = LaunchedCache::default();
    assert!(!wanted(&fold(Vec::new()), &empty));
    assert!(wanted(&fold(vec![plain.clone(), child]), &empty));
    assert!(wanted(
        &fold(vec![plain.clone(), peer("p", "lead", 60)]),
        &empty
    ));
    let published = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched: pass(&[peer("p", "lead", 60)], &root, &runtime),
    };
    assert!(wanted(&fold(vec![plain]), &published));
    assert!(wanted(&fold(Vec::new()), &published));
}

/// A team entry carries its team's configured leader role; a solo peer and a team the
/// config no longer names carry none.
#[test]
fn a_team_entry_carries_its_configured_leader_role() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room_dirs(dir.path());
    let mut facts = pass(
        &[
            seat("seat", "lead", 60),
            peer("solo", "lead", 30),
            AgentState {
                team: Some("gone".to_owned()),
                ..peer("orphan", "lead", 10)
            },
        ],
        &root,
        &runtime,
    );
    let teams = crate::config::TeamsConfig(std::collections::BTreeMap::from([(
        "forge".to_owned(),
        crate::config::Team {
            leader: Some("reviewer".to_owned()),
            ..Default::default()
        },
    )]));
    name_leaders(&mut facts, &teams);
    let leaders: Vec<_> = facts.iter().map(|facts| facts.leader.as_deref()).collect();
    assert_eq!(leaders, [Some("reviewer"), None, None]);
}

#[test]
fn the_pull_request_lane_reads_each_unmerged_branch() {
    let facts = |branch: Option<&str>, pr: BookingPr| LaunchedFacts {
        launched_by: link("lead"),
        starts: BookingStarts::Profile("coder".to_owned()),
        lane: None,
        description: None,
        leader: None,
        team: None,
        launches: vec!["x".to_owned()],
        at: now(),
        ended_at: None,
        branch: branch.map(ToOwned::to_owned),
        checkout: None,
        pr,
    };
    let cache = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched: vec![
            facts(Some("open"), BookingPr::Open { number: Some(1) }),
            facts(Some("unread"), BookingPr::Unread),
            facts(Some("merged"), BookingPr::Merged { number: Some(2) }),
            facts(None, BookingPr::Unread),
        ],
    };
    assert_eq!(
        branch_demand(&cache),
        BTreeSet::from(["open".to_owned(), "unread".to_owned()])
    );
}
