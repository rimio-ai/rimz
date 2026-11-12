//! Collision-only worktree label qualification.

use super::*;

#[test]
fn foreign_places_get_a_qualifier_unless_it_repeats_the_label_and_collisions_deepen_it() {
    // A foreign checkout's own lane is labelled by its folder, so the folder
    // qualifier would only repeat it.
    let (_, _, mut snapshot) = runtime();
    let mut groups = vec![
        group("/home/me/dotfiles", "dotfiles"),
        group("/x/work/app", "main"),
        group("/y/oss/app", "main"),
    ];
    for group in &mut groups {
        group.place = Some(crate::store::snapshot::SidebarPlace {
            path: PathBuf::from(&group.key),
            foreign: true,
        });
    }
    snapshot.worktree_groups = groups.into();
    disambiguate_group_labels(&mut snapshot);
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label_qualifier.as_deref())
            .collect::<Vec<_>>(),
        [None, Some("work/app"), Some("oss/app")]
    );
}

fn group(path: &str, label: &str) -> crate::store::snapshot::SidebarWorktreeGroup {
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
    ]
    .into();

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
    ]
    .into();

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
    snapshot.worktree_groups = vec![group("/a/rimz", "main"), group("/b/.agents", "main")].into();
    let keys = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.key.clone())
        .collect::<Vec<_>>();

    disambiguate_group_labels(&mut snapshot);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].label = "develop".to_owned();
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
fn a_named_lane_sharing_its_checkouts_place_gets_no_qualifier() {
    // One checkout is one group; the only group that shares its place is a
    // named lane homed there, and a channel label never collides.
    let (_, _, mut snapshot) = runtime();
    let mut named = group("/repo/app", "app");
    named.kind = SidebarWorktreeKind::Channel;
    named.key = "channel:app".to_owned();
    snapshot.worktree_groups = vec![group("/repo/app", "app"), named].into();

    disambiguate_group_labels(&mut snapshot);

    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|group| group.label_qualifier.is_none())
    );
}
