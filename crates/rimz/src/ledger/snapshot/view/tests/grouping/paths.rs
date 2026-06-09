use super::*;

#[test]
fn is_within_compares_path_components() {
    let root = Path::new("/home/marvin");
    assert!(is_within(root, root));
    assert!(is_within(root, Path::new("/home/marvin/")));
    assert!(is_within(root, Path::new("/home/marvin/sub/dir")));
    // A shared string prefix that is not a component boundary is outside.
    assert!(!is_within(root, Path::new("/home/marvinX")));
    assert!(!is_within(root, Path::new("/home/other")));
    assert!(!is_within(root, Path::new("/")));
}

#[test]
fn out_of_project_process_folds_into_external_catch_all() {
    let root = "/home/marvin/workspace/project-rimz/rimz";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.key, "external");
    assert_eq!(group.label, "external");
    assert_eq!(group.rows[0].name, "zsh");
}

#[test]
fn in_project_worktree_pane_keeps_its_own_group() {
    let root = "/repo/rimz";
    let worktree = "/repo/rimz/.claude/worktrees/featureX";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", worktree)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, worktree);
    assert_eq!(group.label, "featureX");
}

#[test]
fn main_checkout_pane_is_in_project() {
    let root = "/repo/rimz";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root)], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.label, "rimz");
}

#[test]
fn component_boundary_pane_is_external() {
    // cwd shares a string prefix with the root but not a component boundary.
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from("/home/marvin")))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvinX/repo")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.label, "external");
}

#[test]
fn external_worktree_pane_gets_its_own_pod() {
    // A worktree parked outside the project root — captured by `git worktree
    // list` — is project-related and earns its own pod, not the `external`
    // catch-all the `project_root` prefix test alone would give it.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", external)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, external);
    assert_eq!(group.label, "feature-wt");
}

#[test]
fn external_worktree_subdir_stays_with_its_worktree() {
    // A cwd nested under an external worktree root is still that worktree's,
    // never `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/elsewhere/feature-wt/src")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
}

#[test]
fn non_worktree_path_is_the_only_external() {
    // With the worktree set known, a cwd that is neither under the project
    // root nor inside any worktree (a home shell) is all that's left as
    // `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.label, "external");
}

#[test]
fn no_project_root_preserves_per_path_grouping() {
    // With no known root, an outside cwd still gets its own worktree group —
    // the prior behavior, preserved as the safe default.
    let snapshot =
        room(Vec::new(), Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, "/home/marvin");
    assert_eq!(group.label, "marvin");
}

#[test]
fn worktree_subdir_panes_share_the_worktree_pod() {
    // Root-keying: every pane under one enumerated checkout folds into that
    // checkout's pod, so a shell in `feature-wt/src` sits with its worktree
    // instead of minting a `src` pod of its own.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(
            vec![
                pane("%1", "claude", external),
                pane("%2", "zsh", "/elsewhere/feature-wt/src"),
            ],
            None,
        );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, external);
    assert_eq!(group.rows.len(), 2);
}

// ── Fleet rooms: directory/marker roots, child-repo pods, the root pod ───────
