use super::*;
use crate::config::WorktreeConfig;
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::worktree::LaunchBase;
use jiff::Timestamp;

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

pub(crate) fn init_repo(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).expect("repo dir");
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.test"]);
    git(path, &["config", "user.name", "RimZ Test"]);
    git(path, &["commit", "--allow-empty", "-m", "base"]);
    std::fs::canonicalize(path).expect("canonical repo")
}

/// A project `repo` with a managed worktree `auth` beside it.
pub(crate) struct Room {
    _dir: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) auth: PathBuf,
    pub(crate) parent: PathBuf,
}

pub(crate) fn room() -> Room {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let root = init_repo(&parent.join("repo"));
    let config = WorktreeConfig {
        dir: parent.join("worktrees").display().to_string(),
        ..WorktreeConfig::default()
    };
    crate::worktree::create(
        &root,
        &config,
        Some("auth"),
        None,
        None,
        false,
        LaunchBase::Configured,
    )
    .expect("managed worktree");
    let auth = crate::worktree::worktree_path(&root, &config, "auth").expect("worktree path");
    Room {
        _dir: dir,
        root,
        auth,
        parent,
    }
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

fn trunk(room: &Room) -> Lane {
    Lane::own(LaneHome::Trunk, text(&room.root))
}

fn worktree(room: &Room) -> Lane {
    Lane::own(LaneHome::Worktree, text(&room.auth))
}

fn name(raw: &str) -> LaneName {
    raw.parse().expect("lane name")
}

pub(crate) fn request<'a>(room: &'a Room, target: &'a Path) -> LaneRequest<'a> {
    LaneRequest {
        project_root: &room.root,
        target,
        typed: None,
        team: None,
        joins_cohort: false,
        caller: None,
        agents: &[],
    }
}

fn holder(lane: Lane) -> AgentState {
    AgentState {
        lane: Some(lane),
        ..crate::testkit::agent_state("claude", "holder", Timestamp::UNIX_EPOCH)
    }
}

fn stamped(channel: &str, worktree_path: &str) -> AgentState {
    AgentState {
        channel: Some(channel.to_owned()),
        worktree_path: Some(worktree_path.to_owned()),
        ..crate::testkit::agent_state("claude", "sess", Timestamp::UNIX_EPOCH)
    }
}

fn ended(agent: AgentState) -> AgentState {
    AgentState {
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..agent
    }
}

fn dead(agent: AgentState) -> AgentState {
    AgentState {
        runtime_owner: Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess",
            u32::MAX,
            None,
        )),
        ..agent
    }
}

#[test]
fn a_managed_worktree_whose_directory_is_main_is_foreign() {
    let room = room();
    let main = room.auth.with_file_name("main");
    git(
        &room.root,
        &["worktree", "move", &text(&room.auth), &text(&main)],
    );

    assert_eq!(
        crate::worktree::own_lane(&room.root, &main),
        Lane::own(LaneHome::Foreign, text(&main))
    );
    let typed = LaneRequest {
        typed: Some("main"),
        ..request(&room, &room.root)
    };
    assert_eq!(decide(&typed).unwrap(), trunk(&room));

    let nested = room.root.join("trees/main");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    git(
        &room.root,
        &["worktree", "move", &text(&main), &text(&nested)],
    );
    assert_eq!(
        crate::worktree::own_lane(&room.root, &nested.join("src")),
        Lane::own(LaneHome::Foreign, text(&nested)),
        "a managed `main` under the project root does not walk on into trunk"
    );
}

#[test]
fn a_typed_name_the_target_owns_is_its_own_lane() {
    let room = room();
    for (target, typed, expect) in [
        (&room.root, "main", trunk(&room)),
        (&room.auth, "auth", worktree(&room)),
        (&room.root, "repo", trunk(&room).named(name("repo"))),
        (&room.auth, "repo", worktree(&room).named(name("repo"))),
    ] {
        let request = LaneRequest {
            typed: Some(typed),
            ..request(&room, target)
        };
        assert_eq!(decide(&request).unwrap(), expect, "{typed}");
    }
}

#[test]
fn a_typed_name_is_a_named_lane_at_the_target() {
    let room = room();
    let sub = room.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    for (target, expect) in [
        (&room.root, trunk(&room)),
        (&sub, trunk(&room)),
        (&room.auth, worktree(&room)),
    ] {
        let request = LaneRequest {
            typed: Some("design"),
            team: Some("forge"),
            caller: Some(CallerLane::Agent(trunk(&room).named(name("docs")))),
            ..request(&room, target)
        };
        assert_eq!(decide(&request).unwrap(), expect.named(name("design")));
    }
    let empty = LaneRequest {
        typed: Some(""),
        ..request(&room, &room.root)
    };
    assert_eq!(
        decide(&empty).unwrap(),
        trunk(&room),
        "an empty name is none"
    );
}

#[test]
fn a_typed_name_another_checkout_owns_or_that_is_not_flat_is_refused() {
    let room = room();
    let refuse = |target: &Path, typed: &str| {
        decide(&LaneRequest {
            typed: Some(typed),
            ..request(&room, target)
        })
        .expect_err(typed)
    };

    assert!(
        matches!(refuse(&room.auth, "main"), LaneErr::TrunkCollision { name } if name == "main")
    );
    let err = refuse(&room.root, "auth");
    assert_eq!(
        err.to_string(),
        "channel `auth` is backed by a worktree; use `--worktree auth`"
    );
    assert!(
        matches!(refuse(&room.parent, "auth"), LaneErr::WorktreeCollision { name } if name == "auth")
    );
    for typed in ["bad/name", "a.b", "#ops", "two words"] {
        assert!(
            matches!(refuse(&room.root, typed), LaneErr::InvalidName { ref name } if name == typed),
            "{typed}"
        );
    }
    assert_eq!(
        refuse(&room.auth, "main").to_string(),
        "channel `main` is the project checkout's own lane; launch at the project root, or pick \
         another name"
    );
}

#[test]
fn a_typed_name_held_live_at_another_checkout_is_refused_until_its_holders_end() {
    let room = room();
    let held = [holder(worktree(&room).named(name("design")))];
    let at_trunk = |agents| {
        decide(&LaneRequest {
            typed: Some("design"),
            agents,
            ..request(&room, &room.root)
        })
    };

    let err = at_trunk(&held).expect_err("held at the worktree");
    assert!(matches!(&err, LaneErr::HeldElsewhere { name, checkout }
        if name == "design" && checkout == &room.auth));
    assert_eq!(
        err.to_string(),
        format!(
            "channel `design` is held by live agents at `{}`; launch there, or pick another name",
            room.auth.display()
        )
    );
    let gone = [ended(held[0].clone()), dead(held[0].clone())];
    assert_eq!(at_trunk(&gone).unwrap(), trunk(&room).named(name("design")));
    let same_checkout = decide(&LaneRequest {
        typed: Some("design"),
        agents: &held,
        ..request(&room, &room.auth)
    });
    assert_eq!(
        same_checkout.unwrap(),
        worktree(&room).named(name("design"))
    );
    let old_stamp = [stamped("design", &text(&room.auth))];
    assert_eq!(
        at_trunk(&old_stamp).unwrap(),
        trunk(&room).named(name("design")),
        "a lane-less row holds no lane"
    );
}

#[test]
fn a_child_takes_its_callers_named_lane_only_at_the_callers_checkout() {
    let room = room();
    let design = trunk(&room).named(name("design"));
    let child = |target: &Path, caller: Lane| {
        decide(&LaneRequest {
            caller: Some(CallerLane::Agent(caller)),
            ..request(&room, target)
        })
        .unwrap()
    };
    let sub = room.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    assert_eq!(child(&room.root, design.clone()), design);
    assert_eq!(child(&sub, design.clone()), design);
    assert_eq!(child(&room.auth, design.clone()), worktree(&room));
    assert_eq!(
        child(&room.root, worktree(&room)),
        trunk(&room),
        "a worktree caller's child at the root is in trunk's lane"
    );
    assert_eq!(child(&room.auth, trunk(&room)), worktree(&room));
}

#[test]
fn a_pin_carries_only_at_its_checkout_and_only_while_admissible() {
    let room = room();
    let pin = || Some(CallerLane::Pin(trunk(&room).named(name("design"))));
    let human = |target: &Path, agents: &[AgentState]| {
        decide(&LaneRequest {
            caller: pin(),
            agents,
            ..request(&room, target)
        })
        .unwrap()
    };

    assert_eq!(human(&room.root, &[]), trunk(&room).named(name("design")));
    assert_eq!(human(&room.auth, &[]), worktree(&room));
    assert_eq!(
        human(&room.root, &[holder(worktree(&room).named(name("design")))]),
        trunk(&room),
        "a pin held elsewhere is ignored, not refused"
    );
    let joining = decide(&LaneRequest {
        caller: pin(),
        joins_cohort: true,
        agents: &[holder(worktree(&room).named(name("design")))],
        ..request(&room, &room.root)
    });
    assert_eq!(
        joining.unwrap(),
        trunk(&room),
        "a bare role joining a cohort ignores a pin held elsewhere"
    );
    let colliding = decide(&LaneRequest {
        caller: Some(CallerLane::Pin(trunk(&room).named(name("auth")))),
        ..request(&room, &room.root)
    });
    assert_eq!(colliding.unwrap(), trunk(&room));
    let harness_owned = decide(&request(&room, &room.root));
    assert_eq!(harness_owned.unwrap(), trunk(&room));
}

#[test]
fn an_in_place_team_at_trunk_is_the_named_lane_of_the_team() {
    let room = room();
    let team = |target: &Path, team: &'static str, agents: &[AgentState]| {
        decide(&LaneRequest {
            team: Some(team),
            agents,
            ..request(&room, target)
        })
    };
    let forge = trunk(&room).named(name("forge"));
    assert_eq!(team(&room.root, "forge", &[]).unwrap(), forge);
    assert_eq!(team(&room.auth, "forge", &[]).unwrap(), worktree(&room));
    assert_eq!(
        team(&room.root, "repo", &[]).unwrap(),
        trunk(&room).named(name("repo")),
        "a team named after the project directory is a named lane"
    );
    assert_eq!(
        team(&room.root, "ops v2.1", &[]).unwrap(),
        trunk(&room).named(LaneName::dashed("ops v2.1")),
        "a team name outside the flat set is dashed, not refused as a typed name"
    );
    assert_eq!(
        team(&room.root, "forge", &[holder(forge.clone())]).unwrap(),
        forge,
        "the running cohort's own lane does not refuse it"
    );

    let design = trunk(&room).named(name("design"));
    let joining = decide(&LaneRequest {
        team: Some("forge"),
        joins_cohort: true,
        caller: Some(CallerLane::Agent(design.clone())),
        ..request(&room, &room.root)
    });
    assert_eq!(
        joining.unwrap(),
        design,
        "a role joining the caller's cohort takes the caller's lane"
    );
    let caller_over_team = decide(&LaneRequest {
        team: Some("forge"),
        caller: Some(CallerLane::Agent(design)),
        ..request(&room, &room.root)
    });
    assert_eq!(
        caller_over_team.unwrap(),
        forge,
        "a fresh team outranks the caller's lane"
    );

    let err = team(&room.root, "auth", &[]).expect_err("a managed worktree holds the name");
    assert_eq!(
        err.to_string(),
        "channel `auth` is backed by a worktree; use `--worktree auth`"
    );
    let err = team(&room.root, "main", &[]).expect_err("trunk owns `main`");
    assert!(matches!(&err, LaneErr::TeamOwnLane { team } if team == "main"));
    assert_eq!(
        err.to_string(),
        "team `main` cannot run in place: `main` is the project checkout's own lane; launch it \
         in a worktree with `--worktree <name>`"
    );
    assert_eq!(
        team(&room.auth, "main", &[]).unwrap(),
        worktree(&room),
        "the refusal's `--worktree <name>` runs team `main` in that worktree's own lane"
    );
    let held = [holder(worktree(&room).named(name("forge")))];
    assert!(
        matches!(
            team(&room.root, "forge", &held),
            Err(LaneErr::HeldElsewhere { name, .. }) if name == "forge"
        ),
        "a lane of the team's name held live at another checkout refuses it"
    );
}

#[test]
fn a_directory_channel_is_its_own_lanes_routing_name_and_none_for_trunk() {
    let room = room();
    assert_eq!(directory_channel(&room.root, &room.root), None);
    assert_eq!(directory_channel(&room.root, &room.root.join("sub")), None);
    assert_eq!(
        directory_channel(&room.root, &room.auth)
            .as_ref()
            .map(|lane| lane.as_str()),
        Some("auth")
    );
    assert_eq!(
        directory_channel(&room.root, &room.auth.join("src"))
            .as_ref()
            .map(|lane| lane.as_str()),
        Some("auth"),
        "a worktree subdirectory routes to its worktree"
    );
}

#[test]
fn worktree_name_admission_refuses_a_lane_live_agents_hold() {
    let room = room();
    let dir = |name: &str| room.parent.join("worktrees").join(name);
    let held = [
        holder(trunk(&room).named(name("design"))),
        holder(trunk(&room).named(name("forge"))),
        holder(trunk(&room).named(name("repo"))),
    ];

    for taken in ["design", "forge", "repo"] {
        let err = admit_worktree_name(&held, taken, &dir(taken)).expect_err("occupied lane");
        assert!(
            matches!(&err, LaneErr::LaneOccupied { name } if name == taken),
            "{taken}: {err}"
        );
    }
    admit_worktree_name(&held, "docs", &dir("docs")).expect("another name");
    admit_worktree_name(&held[..2], "repo", &dir("repo")).expect("the project directory's name");
    admit_worktree_name(
        &[stamped("design", &text(&room.root))],
        "design",
        &dir("design"),
    )
    .expect("a lane-less row holds no lane");
}

#[test]
fn worktree_name_admission_ignores_ended_dead_and_worktree_agents() {
    let room = room();
    let held =
        |path: &Path| holder(Lane::own(LaneHome::Worktree, text(path)).named(name("design")));
    let ended_and_dead = [
        ended(holder(trunk(&room).named(name("design")))),
        dead(holder(trunk(&room).named(name("design")))),
    ];
    // The worktree's own directory, made or not yet made, outside or inside the project.
    for design in [
        room.parent.join("worktrees/design"),
        room.root.join(".worktrees/design"),
    ] {
        std::fs::create_dir_all(design.parent().unwrap()).unwrap();
        let mut agents = ended_and_dead.to_vec();
        agents.push(held(&design));
        admit_worktree_name(&agents, "design", &design).expect("free lane");
    }
}
