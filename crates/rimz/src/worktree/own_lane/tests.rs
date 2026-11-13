use super::*;
use crate::harness::lanes::tests::{Room, init_repo, room};

fn text(path: &Path) -> String {
    path.display().to_string()
}

fn trunk(room: &Room) -> Lane {
    Lane::own(LaneHome::Trunk, text(&room.root))
}

fn worktree(room: &Room) -> Lane {
    Lane::own(LaneHome::Worktree, text(&room.auth))
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
