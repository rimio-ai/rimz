//! `rimz agents list` roster grouping: the same worktree pods and attention-first
//! order the sidebar paints, but uncapped over the whole roster.

use super::*;

use crate::ledger::snapshot::group_live_agents_by_worktree;

#[test]
fn groups_by_worktree_attention_first_external_last_uncapped() {
    let mut agents = vec![
        agent("claude", "main-idle", AgentStatus::Idle, 10).worktree("/repo/main"),
        agent("claude", "main-wait", AgentStatus::Waiting, 20).worktree("/repo/main"),
        agent("claude", "feat-run", AgentStatus::Running, 30).worktree("/repo/feat"),
        // No worktree at all → the `external` catch-all.
        agent("claude", "ext-idle", AgentStatus::Idle, 40),
    ];
    // Pad the main pod past the sidebar's 6-row cap to prove the roster is uncapped.
    for i in 0..8 {
        agents.push(
            agent(
                "claude",
                &format!("main-extra-{i}"),
                AgentStatus::Idle,
                1 + i,
            )
            .worktree("/repo/main"),
        );
    }
    let snapshot = room(Vec::new(), agents);
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    // The `external` catch-all tails every project pod.
    assert_eq!(
        groups.last().expect("a group").kind,
        SidebarWorktreeKind::External,
        "external sorts last"
    );
    // A `waiting`-topped pod leads a `running`-topped one.
    assert_eq!(groups[0].label, "main", "attention pod leads: {groups:?}");

    let main = groups
        .iter()
        .find(|group| group.label == "main")
        .expect("main pod");
    assert_eq!(
        main.agents.first().expect("a row").agent_id.as_str(),
        "main-wait",
        "the waiting agent leads its pod regardless of input order"
    );
    assert_eq!(main.agents.len(), 10, "uncapped: the whole pod shows");
}

#[test]
fn one_path_two_branches_splits_into_two_listing_pods() {
    // Parity with the sidebar's row fold (see `grouping::branches`): two
    // checkouts of one path must split by branch, not collapse under a single
    // mislabeled header.
    let feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    let main = agent("claude", "sess-b", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");
    let snapshot = room(Vec::new(), vec![feature, main]);
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    assert_eq!(
        groups.len(),
        2,
        "two branches under one path split into two pods: {groups:?}"
    );
    for group in &groups {
        assert_eq!(group.agents.len(), 1);
        assert_eq!(
            group.agents[0].worktree_branch.as_deref(),
            Some(group.label.as_str()),
            "each pod's label matches its branch",
        );
    }
}

#[test]
fn out_of_project_agent_tails_into_external_when_root_is_known() {
    // With the room's project root known — which the `--all` audit view now
    // copies from the live snapshot — a stale agent whose cwd sits outside the
    // repo lands in the `external` catch-all instead of a per-path pod that
    // would outrank project work on its `failed` attention status.
    let inside = agent("claude", "in", AgentStatus::Running, 10).worktree("/repo/main");
    let outside = agent("claude", "out", AgentStatus::Failed, 20).worktree("/tmp/scratch");
    let snapshot = room(Vec::new(), vec![inside, outside])
        .with_project_root(Some(std::path::PathBuf::from("/repo/main")));
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    let external = groups
        .iter()
        .find(|group| group.kind == SidebarWorktreeKind::External)
        .expect("the out-of-project agent forms an external pod");
    assert_eq!(external.agents.len(), 1);
    assert_eq!(external.agents[0].agent_id.as_str(), "out");
    assert_eq!(
        groups.last().expect("a group").kind,
        SidebarWorktreeKind::External,
        "external tails project work even though its agent is `failed`: {groups:?}",
    );
}

#[test]
fn git_backed_out_of_project_agent_gets_own_worktree_pod() {
    let with_branch = agent("claude", "out", AgentStatus::Failed, 20)
        .worktree("/home/user/.agents/teams")
        .branch("main");
    let without_branch =
        agent("claude", "out", AgentStatus::Failed, 20).worktree("/home/user/.agents/teams");
    let project_root = Some(std::path::PathBuf::from("/repo/main"));

    let grouped = |agent: AgentState| {
        let snapshot = room(Vec::new(), vec![agent]).with_project_root(project_root.clone());
        let refs: Vec<&AgentState> = snapshot.agents.iter().collect();
        let groups = group_live_agents_by_worktree(&refs, &snapshot);
        let group = groups.first().expect("a group");
        assert_eq!(groups.len(), 1);
        (group.kind, group.label.clone())
    };

    let external = (SidebarWorktreeKind::External, "external".to_owned());
    assert_eq!(
        grouped(with_branch),
        (SidebarWorktreeKind::Worktree, "main".to_owned())
    );
    assert_eq!(grouped(without_branch), external);
}

#[test]
fn sidebar_external_group_label_is_stable_for_non_git_cwd() {
    let outside =
        agent("claude", "out", AgentStatus::Failed, 20).worktree("/home/user/.agents/teams");
    let snapshot = room(Vec::new(), vec![outside])
        .with_project_root(Some(std::path::PathBuf::from("/repo/main")))
        .with_live_panes(vec![pane("%1", "claude", "/home/user/.agents/teams")], None);

    let group = snapshot.worktree_groups.first().expect("a group");
    assert_eq!(group.kind, SidebarWorktreeKind::External);
    assert_eq!(group.key, "external");
    assert_eq!(group.label, "external");
}

#[test]
fn branch_named_unmatched_path_keeps_pre_enumeration_worktree_group() {
    let feature = agent("claude", "feat", AgentStatus::Running, 20)
        .worktree("/work/query-engine-feature-x/src")
        .branch("feature-x");
    let snapshot = room(Vec::new(), vec![feature])
        .with_project_root(Some(std::path::PathBuf::from("/repo/main")))
        .with_live_panes(
            vec![pane("%1", "claude", "/work/query-engine-feature-x/src")],
            None,
        );

    let group = snapshot.worktree_groups.first().expect("a group");
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, "/work/query-engine-feature-x/src");
    assert_eq!(group.label, "feature-x");
}

#[test]
fn named_channel_groups_a_live_agent_ahead_of_worktree_identity() {
    let mut design = agent("claude", "design", AgentStatus::Running, 20)
        .worktree("/repo/main")
        .branch("main");
    design.channel = Some("design".to_owned());
    let snapshot = room(Vec::new(), vec![design]);
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].kind, SidebarWorktreeKind::Channel);
    assert_eq!(groups[0].label, "design");
}

#[test]
fn listing_roster_folds_unstamped_rimz_worktree_agents_into_channel_pod() {
    let worktree = "/repo/rimz-worktrees/message-channel";
    let mut stamped = agent("claude", "stamped", AgentStatus::Running, 20).worktree(worktree);
    stamped.channel = Some("message-channel".to_owned());
    let unstamped = agent("codex", "bare", AgentStatus::Idle, 10).worktree(worktree);
    let snapshot = room(Vec::new(), vec![stamped, unstamped])
        .with_project_root(Some(std::path::PathBuf::from("/repo/rimz")))
        .with_worktree_home(Some(std::path::PathBuf::from("/repo/rimz-worktrees")));
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    assert_eq!(
        groups.len(),
        1,
        "roster matches sidebar grouping: {groups:?}"
    );
    assert_eq!(groups[0].kind, SidebarWorktreeKind::Channel);
    assert_eq!(groups[0].label, "message-channel");
    assert_eq!(groups[0].agents.len(), 2);
}

#[test]
fn listing_roster_mirrors_cohort_block_order_and_group_attention_rank() {
    let mut first = agent("claude", "cohort-first", AgentStatus::Success, 30).worktree("/repo/a");
    first.launch_group = Some("launch_group_1".to_owned());
    first.launch_ordinal = Some(0);
    first.pane = Some(pane("%3", "claude", "/repo/a"));
    let mut second = agent("claude", "cohort-second", AgentStatus::Waiting, 10).worktree("/repo/a");
    second.launch_group = Some("launch_group_1".to_owned());
    second.launch_ordinal = Some(1);
    second.pane = Some(pane("%1", "claude", "/repo/a"));
    let other = agent("claude", "other-running", AgentStatus::Running, 40).worktree("/repo/b");
    let snapshot = room(Vec::new(), vec![second, other, first]);
    let refs: Vec<&AgentState> = snapshot.agents.iter().collect();

    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    assert_eq!(
        groups[0].label, "a",
        "the waiting cohort member still lifts its group even when block order keeps it below a calm teammate"
    );
    let order = groups[0]
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["cohort-first", "cohort-second"]);
}
