//! Collision-only worktree label qualification.

use super::*;

fn group(path: &str, label: &str) -> crate::SidebarWorktreeGroup {
    let mut group = worktree_group(Path::new(path), Vec::new());
    group.label = label.to_owned();
    group
}

#[test]
fn colliding_labels_use_the_shortest_distinguishing_path_suffix() {
    let (_, _, mut snapshot) = runtime();
    snapshot.worktree_groups = vec![
        group("/a/rimz", "main"),
        group("/b/.agents", "main"),
        group("/x/work/app", "release"),
        group("/y/oss/app", "release"),
    ];

    disambiguate_group_labels(&mut snapshot);

    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label_qualifier.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("rimz"),
            Some(".agents"),
            Some("work/app"),
            Some("oss/app")
        ]
    );
}

#[test]
fn unambiguous_channels_and_redundant_qualifiers_stay_quiet() {
    let (_, _, mut snapshot) = runtime();
    let mut channel = group("/channels/main", "main");
    channel.kind = SidebarWorktreeKind::Channel;
    channel.key = "channel:main".to_owned();
    snapshot.worktree_groups = vec![
        group("/repo/rimz", "main"),
        group("/repo/.agents", "other"),
        group("/repo/feature", "feature"),
        group("/repo/other", "feature"),
        channel,
    ];

    disambiguate_group_labels(&mut snapshot);

    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label_qualifier.as_deref())
            .collect::<Vec<_>>(),
        vec![None, None, None, Some("other"), None]
    );
}

#[test]
fn pass_preserves_identity_and_clears_stale_qualifiers() {
    let (_, _, mut snapshot) = runtime();
    snapshot.worktree_groups = vec![group("/a/rimz", "main"), group("/b/.agents", "main")];
    let keys = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.key.clone())
        .collect::<Vec<_>>();

    disambiguate_group_labels(&mut snapshot);
    snapshot.worktree_groups[1].label = "develop".to_owned();
    disambiguate_group_labels(&mut snapshot);

    assert_eq!(snapshot.worktree_groups.len(), 2);
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| &group.key)
            .collect::<Vec<_>>(),
        keys.iter().collect::<Vec<_>>()
    );
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|group| group.label_qualifier.is_none())
    );
}

#[test]
fn identical_paths_under_split_keys_get_no_qualifier() {
    let (_, _, mut snapshot) = runtime();
    let mut first = group("/repo/app", "main");
    first.key.push_str("\nold-main");
    let mut second = group("/repo/app", "main");
    second.key.push_str("\nnew-main");
    snapshot.worktree_groups = vec![first, second];

    disambiguate_group_labels(&mut snapshot);

    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|group| group.label_qualifier.is_none())
    );
}
