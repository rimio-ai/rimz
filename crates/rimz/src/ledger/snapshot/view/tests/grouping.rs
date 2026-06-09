use super::*;

// ── Worktree grouping ────────────────────────────────────────────────────────

#[test]
fn agents_on_different_branches_in_one_path_form_two_groups() {
    // Root cause 5: stale rows put two branches under one path, collapsing
    // into a single mislabeled section. Keying on branch splits them into
    // two correctly-labeled groups.
    let feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    let main = agent("claude", "sess-b", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");

    let snapshot = room_with_agent_panes(Vec::new(), vec![feature, main]);

    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "two branches under one path split into two groups"
    );
    for group in &snapshot.worktree_groups {
        assert_eq!(group.rows.len(), 1);
        assert_eq!(
            group.rows[0].worktree_branch.as_deref(),
            Some(group.label.as_str()),
            "each group's label matches its branch"
        );
    }
}

#[test]
fn one_branch_path_keeps_agent_and_shell_in_one_group() {
    // The common case must not fragment: a process/shell row carries no
    // branch, so it stays with the single-branch agent in its worktree.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .branch("main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "agent and its shell share one worktree group: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "main");
    let rows = &snapshot.worktree_groups[0].rows;
    assert!(rows.iter().any(|row| row.is_agent()));
    assert!(rows.iter().any(|row| row.is_process() && row.name == "zsh"));
}

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

#[test]
fn directory_room_groups_panes_by_child_repo() {
    // A directory room (`/srv/agents` holding repos): each enumerated child
    // repo is a group root, so every pane under one child shares one pod keyed
    // on the child's root; panes at the room root take the name-only `Root`
    // pod; a cwd outside the room stays external.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(
            vec![
                pane("%1", "claude", "/srv/agents/query-engine"),
                pane("%2", "zsh", "/srv/agents/query-engine/src"),
                pane("%3", "codex", "/srv/agents/billing"),
                pane("%4", "zsh", "/srv/agents"),
                pane("%5", "zsh", "/tmp/elsewhere"),
            ],
            None,
        );

    let summary: Vec<(SidebarWorktreeKind, &str, &str, usize)> = snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            (
                group.kind,
                group.key.as_str(),
                group.label.as_str(),
                group.rows.len(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (SidebarWorktreeKind::Root, "/srv/agents", "agents", 1),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/billing",
                "billing",
                1
            ),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/query-engine",
                "query-engine",
                2
            ),
            (SidebarWorktreeKind::External, "external", "external", 1),
        ],
    );
}

#[test]
fn depth_two_repo_folds_into_the_root_pod() {
    // The v1 depth rule: enumeration mints pods for depth-1 children only, so
    // a deeper repo's panes belong to the room's root pod.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![PathBuf::from("/srv/agents/billing")])
        .with_live_panes(vec![pane("%1", "zsh", "/srv/agents/org/repo")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.key, "/srv/agents");
}

#[test]
fn scratch_room_is_one_root_pod() {
    // The degenerate fleet room — a marker-less scratch dir: zero child
    // repos, one flat name-only pod.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/tmp/scratch")))
        .with_live_panes(
            vec![
                pane("%1", "claude", "/tmp/scratch"),
                pane("%2", "zsh", "/tmp/scratch/logs"),
            ],
            None,
        );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.label, "scratch");
    assert_eq!(group.rows.len(), 2);
}

#[test]
fn marker_room_root_pod_reads_like_a_directory_room() {
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Marker)
        .with_project_root(Some(PathBuf::from("/srv/app")))
        .with_live_panes(vec![pane("%1", "zsh", "/srv/app/src")], None);

    assert_eq!(snapshot.worktree_groups[0].kind, SidebarWorktreeKind::Root);
    assert_eq!(snapshot.worktree_groups[0].label, "app");
}

#[test]
fn stale_branch_row_never_relabels_the_root_pod() {
    // A row claiming a branch at a non-repo room root is stale by definition
    // (the root has no git story); the pod keeps its directory name.
    let live = pane("%scratch", "rimz-ask", "/tmp/scratch");
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/tmp/scratch".to_owned());
    item.worktree_branch = Some("main".to_owned());
    item.pane = Some(live.clone());

    let snapshot = room(vec![item], Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/tmp/scratch")))
        .with_live_panes(vec![live], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.label, "scratch");
}

#[test]
fn fleet_room_tiering_floats_the_attention_child_repo() {
    // Directory-room ordering rides the same tier ladder: the waiting child
    // repo leads, calm pods follow by label, external tails.
    let q_pane = pane("%q1", "claude", "/srv/agents/query-engine");
    let r_pane = pane("%r1", "claude", "/srv/agents");
    let b_pane = pane("%b1", "claude", "/srv/agents/billing");
    let e_pane = pane("%e1", "claude", "/tmp/outside");
    let mut q1 = agent_in(
        "q1",
        "/srv/agents/query-engine",
        AgentStatus::Waiting,
        1_000,
    );
    q1.pane = Some(q_pane.clone());
    let mut r1 = agent_in("r1", "/srv/agents", AgentStatus::Idle, 1_000);
    r1.pane = Some(r_pane.clone());
    let mut b1 = agent_in("b1", "/srv/agents/billing", AgentStatus::Idle, 1_000);
    b1.pane = Some(b_pane.clone());
    let mut e1 = agent("claude", "e1", AgentStatus::Idle, 1_000);
    e1.pane = Some(e_pane.clone());

    let snapshot = room(Vec::new(), vec![q1, r1, b1, e1])
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(vec![q_pane, r_pane, b_pane, e_pane], None);

    let labels: Vec<&str> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["query-engine", "agents", "billing", "external"]
    );
}

#[test]
fn group_tiering_floats_attention_and_tails_external() {
    let labels_for = |mut agents: Vec<AgentState>| {
        let mut panes = Vec::new();
        for (idx, agent) in agents.iter_mut().enumerate() {
            let raw = format!("%tier-{idx}");
            let mut live = pane(
                &raw,
                agent.kind.as_str(),
                agent.worktree_path.as_deref().unwrap_or("/repo/main"),
            );
            if agent.worktree_path.is_none() {
                live.cwd = None;
            }
            agent.pane = Some(live.clone());
            panes.push(live);
        }

        room(Vec::new(), agents)
            .with_live_panes(panes, None)
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };
    let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

    // A calm external sinks below calm project worktrees; an attention
    // worktree leads regardless of its name.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
            agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Idle),
        ]),
        vec!["alpha", "beta", "external"]
    );

    // The external catch-all rises out of the tail only when it holds an
    // attention agent (waiting or failed).
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["external", "beta"]
    );
}
