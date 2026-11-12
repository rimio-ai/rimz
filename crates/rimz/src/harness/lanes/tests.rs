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
fn a_directory_classifies_into_the_deepest_checkout_containing_it() {
    let room = room();
    let sub = room.root.join("crates/sub");
    std::fs::create_dir_all(&sub).unwrap();
    let nested = init_repo(&room.root.join("vendor/nested"));
    let auth_sub = room.auth.join("src");
    std::fs::create_dir_all(&auth_sub).unwrap();
    let foreign = init_repo(&room.parent.join("other"));
    let foreign_sub = foreign.join("deep");
    std::fs::create_dir_all(&foreign_sub).unwrap();
    let plain = room.parent.join("plain/dir");
    std::fs::create_dir_all(&plain).unwrap();
    let dotted_auth = room
        .root
        .join("..")
        .join(room.auth.strip_prefix(&room.parent).unwrap());

    for (dir, expect) in [
        (room.root.clone(), trunk(&room)),
        (sub, trunk(&room)),
        (nested, trunk(&room)),
        (room.auth.clone(), worktree(&room)),
        (auth_sub, worktree(&room)),
        (dotted_auth, worktree(&room)),
        (foreign_sub, Lane::own(LaneHome::Foreign, text(&foreign))),
        (plain.clone(), Lane::own(LaneHome::Foreign, text(&plain))),
    ] {
        assert_eq!(own_lane(&room.root, &dir), expect, "{}", dir.display());
    }

    let bare = tempfile::tempdir().unwrap();
    assert_eq!(
        own_lane(bare.path(), bare.path()),
        Lane::own(LaneHome::Trunk, text(bare.path())),
        "a non-repository room's root is its trunk"
    );
}

#[test]
fn a_foreign_directory_is_homed_only_at_a_real_repository_root_off_the_home() {
    let room = room();
    let marked = room.parent.join("marked");
    std::fs::create_dir_all(marked.join(".git")).unwrap();
    let under_marker = marked.join("work");
    let home = init_repo(&room.parent.join("home"));
    let under_home = home.join("scratch");
    let other = init_repo(&room.parent.join("other"));
    let other_sub = other.join("deep");
    let linked = room.parent.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        "gitdir: /elsewhere/.git/worktrees/linked\n",
    )
    .unwrap();
    let linked_sub = linked.join("src");
    for dir in [&under_marker, &under_home, &other_sub, &linked_sub] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let home_link = room.parent.join("home-link");
    std::os::unix::fs::symlink(&home, &home_link).unwrap();
    let through_link = home_link.join("scratch");

    for (dir, home, expect, why) in [
        (
            &under_marker,
            None,
            &under_marker,
            "a bare `.git` marker is no repository",
        ),
        (
            &under_home,
            Some(&home),
            &under_home,
            "a repository at the home homes nothing",
        ),
        (
            &other_sub,
            Some(&home),
            &other,
            "a repository elsewhere homes its subdirectory",
        ),
        (
            &linked_sub,
            Some(&home),
            &linked,
            "a `gitdir:` file is a repository root",
        ),
        (
            &other_sub,
            Some(&other_sub.join("me")),
            &other_sub,
            "nor one containing the home",
        ),
        (
            &under_home,
            Some(&home_link),
            &under_home,
            "a home named through a link still homes nothing",
        ),
        (
            &through_link,
            Some(&home),
            &through_link,
            "nor a directory reached through a link to the home",
        ),
    ] {
        assert_eq!(
            own_lane_at_home(&room.root, dir, home.map(PathBuf::as_path)),
            Lane::own(LaneHome::Foreign, text(expect)),
            "{why}"
        );
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
        own_lane(&room.root, &main),
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
        own_lane(&room.root, &nested.join("src")),
        Lane::own(LaneHome::Foreign, text(&nested)),
        "a managed `main` under the project root does not walk on into trunk"
    );
}

#[test]
fn a_typed_name_the_target_owns_is_its_own_lane() {
    let room = room();
    for (target, typed, expect) in [
        (&room.root, "main", trunk(&room)),
        (&room.root, "repo", trunk(&room)),
        (&room.auth, "auth", worktree(&room)),
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
    assert!(
        matches!(refuse(&room.auth, "repo"), LaneErr::TrunkCollision { name } if name == "repo")
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
    assert!(
        matches!(at_trunk(&old_stamp), Err(LaneErr::HeldElsewhere { .. })),
        "a lane-less row is read through its old stamp"
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
fn an_in_place_team_at_trunk_names_its_lane_and_a_joining_role_takes_the_callers() {
    let room = room();
    let team = |target: &Path| {
        decide(&LaneRequest {
            team: Some("forge"),
            ..request(&room, target)
        })
        .unwrap()
    };
    assert_eq!(team(&room.root), trunk(&room).named(name("repo/forge")));
    assert_eq!(team(&room.auth), worktree(&room));

    let joining = decide(&LaneRequest {
        team: Some("forge"),
        joins_cohort: true,
        caller: Some(CallerLane::Agent(trunk(&room).named(name("repo/forge")))),
        ..request(&room, &room.root)
    });
    assert_eq!(joining.unwrap(), trunk(&room).named(name("repo/forge")));
    let caller_over_team = decide(&LaneRequest {
        team: Some("forge"),
        caller: Some(CallerLane::Agent(trunk(&room).named(name("design")))),
        ..request(&room, &room.root)
    });
    assert_eq!(
        caller_over_team.unwrap(),
        trunk(&room).named(name("repo/forge")),
        "a fresh team outranks the caller's lane"
    );

    assert_eq!(
        in_place_team_name(Path::new("/code/project"), "forge"),
        name("project/forge")
    );
    assert_eq!(in_place_team_name(Path::new("/"), "forge"), name("forge"));
    assert_eq!(
        in_place_team_name(Path::new("/code/my #project"), "forge"),
        name("my--project/forge")
    );

    let spaced = tempfile::tempdir().unwrap();
    let root = spaced.path().join("my project");
    std::fs::create_dir_all(&root).unwrap();
    let at_spaced_trunk = decide(&LaneRequest {
        project_root: &root,
        team: Some("forge"),
        ..request(&room, &root)
    });
    assert_eq!(
        at_spaced_trunk.unwrap(),
        Lane::own(LaneHome::Trunk, text(&root)).named(name("my-project/forge")),
        "a project directory name with whitespace still names the team's lane"
    );
}

#[test]
fn a_directory_channel_is_its_own_lanes_routing_name_and_none_for_trunk() {
    let room = room();
    assert_eq!(directory_channel(&room.root, &room.root), None);
    assert_eq!(directory_channel(&room.root, &room.root.join("sub")), None);
    assert_eq!(
        directory_channel(&room.root, &room.auth).as_deref(),
        Some("auth")
    );
    assert_eq!(
        directory_channel(&room.root, &room.auth.join("src")).as_deref(),
        Some("auth"),
        "a worktree subdirectory routes to its worktree"
    );
}

#[test]
fn a_row_lane_is_the_stored_lane_else_the_adopted_stamp() {
    let room = room();
    let stored = holder(worktree(&room).named(name("design")));
    assert_eq!(
        row_lane(&stored, &room.root),
        Some(worktree(&room).named(name("design")))
    );
    assert_eq!(
        row_lane(&stamped("auth", &text(&room.auth)), &room.root),
        Some(worktree(&room))
    );
    assert_eq!(
        row_lane(&stamped("main", &text(&room.root)), &room.root),
        Some(trunk(&room))
    );
    assert_eq!(
        row_lane(&stamped("design", &text(&room.root)), &room.root),
        Some(trunk(&room).named(name("design")))
    );
    let pathless = crate::testkit::agent_state("claude", "sess", Timestamp::UNIX_EPOCH);
    assert_eq!(row_lane(&pathless, &room.root), None);
}

#[test]
fn worktree_name_admission_refuses_a_lane_live_agents_hold() {
    let room = room();
    let held = [stamped("design", &text(&room.root))];

    let err = admit_worktree_name(&held, &room.root, "design").expect_err("occupied lane");

    assert!(matches!(err, LaneErr::LaneOccupied { name } if name == "design"));
    admit_worktree_name(&held, &room.root, "docs").expect("another name");
}

#[test]
fn worktree_name_admission_ignores_ended_dead_and_worktree_agents() {
    let room = room();
    let design = room.parent.join("worktrees/design");
    std::fs::create_dir_all(&design).unwrap();
    let in_worktree = stamped("design", &text(&design));

    admit_worktree_name(
        &[
            ended(stamped("design", &text(&room.root))),
            dead(stamped("design", &text(&room.root))),
            in_worktree,
            // A worktree home inside the project, before the tree exists.
            stamped("design", &text(&room.root.join(".worktrees/design"))),
        ],
        &room.root,
        "design",
    )
    .expect("free lane");
}
