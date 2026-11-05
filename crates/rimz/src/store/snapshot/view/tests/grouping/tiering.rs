use super::*;
use crate::agents::ATTENTION_AGE_CEILING_SECS;
use crate::store::snapshot::{RowCard, WorktreeTrunkSync};

fn ranked_snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    ranked_snapshot_in(agents, None)
}

fn ranked_snapshot_in(mut agents: Vec<AgentState>, project_root: Option<&str>) -> SidebarSnapshot {
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

    list_agent_checkouts(room(agents))
        .with_project_root(project_root.map(PathBuf::from))
        .with_live_panes(panes, None)
}

fn labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}

#[test]
fn inactive_membership_uses_strict_quiet_windows_and_blockers() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (name, landed, age, status, unread, root, external, process, dirty, folded) in [
        (
            "landed boundary",
            true,
            3600,
            AgentStatus::Success,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "landed past",
            true,
            3601,
            AgentStatus::Success,
            false,
            false,
            false,
            false,
            false,
            true,
        ),
        (
            "unlanded 23h",
            false,
            82800,
            AgentStatus::Idle,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "unlanded boundary",
            false,
            86400,
            AgentStatus::Idle,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "unlanded past",
            false,
            86401,
            AgentStatus::Idle,
            false,
            false,
            false,
            false,
            false,
            true,
        ),
        (
            "root agent",
            true,
            90000,
            AgentStatus::Idle,
            false,
            true,
            false,
            false,
            false,
            false,
        ),
        (
            "root process",
            true,
            90000,
            AgentStatus::Idle,
            false,
            true,
            false,
            true,
            false,
            false,
        ),
        (
            "external",
            true,
            90000,
            AgentStatus::Idle,
            false,
            false,
            true,
            false,
            false,
            false,
        ),
        (
            "running",
            true,
            90000,
            AgentStatus::Running,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "waiting",
            true,
            90000,
            AgentStatus::Waiting,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "failed",
            true,
            90000,
            AgentStatus::Failed,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "paused",
            true,
            90000,
            AgentStatus::Paused,
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "unread unlanded",
            false,
            90000,
            AgentStatus::Success,
            true,
            false,
            false,
            false,
            false,
            false,
        ),
        (
            "unread landed",
            true,
            90000,
            AgentStatus::Success,
            true,
            false,
            false,
            false,
            false,
            true,
        ),
        (
            "process landed",
            true,
            3601,
            AgentStatus::Idle,
            false,
            false,
            false,
            true,
            false,
            true,
        ),
        (
            "dirty merged",
            true,
            3601,
            AgentStatus::Success,
            false,
            false,
            false,
            false,
            true,
            false,
        ),
    ] {
        let mut member = agent_in("member", "/repo/work", status, 0);
        member.registered_at = Some(ago(age));
        let mut snapshot = ranked_snapshot(vec![member]);
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
        group.clean = Some(!dirty);
        group.pr_state = landed.then_some(WorktreePrState::Merged);
        group.root_lane = root;
        if external {
            group.kind = SidebarWorktreeKind::External;
        }
        let row = &mut std::sync::Arc::make_mut(&mut group.rows)[0];
        row.unread = unread;
        row.last_activity = ago(age);
        if process {
            row.card = RowCard::Process(crate::store::snapshot::ProcessCard::default());
        }
        snapshot.sort_groups_for_presentation();
        actual.push((name, snapshot.worktree_groups[0].inactive.is_some()));
        expected.push((name, folded));
    }
    assert_eq!(actual, expected);
}

#[test]
fn inactive_membership_changes_with_time_alone_and_a_new_agent_revives_it() {
    let mut member = agent_in("member", "/repo/work", AgentStatus::Idle, 0);
    member.registered_at = Some(ago(3600));
    let mut snapshot = ranked_snapshot(vec![member]);
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    group.clean = Some(true);
    group.pr_state = Some(WorktreePrState::Closed);
    snapshot.sort_groups_for_presentation();
    let before = snapshot.worktree_groups[0].inactive.is_some();
    snapshot.now += std::time::Duration::from_secs(1);
    snapshot.sort_groups_for_presentation();
    let after = snapshot.worktree_groups[0].inactive.is_some();
    let mut arrival = snapshot.worktree_groups[0].rows[0].clone();
    arrival.id = "arrival".to_owned();
    arrival.as_agent_mut().unwrap().registered_at = Some(snapshot.now);
    // The card's engagement instant is derived at projection, not row activity.
    let mut value = serde_json::to_value(&arrival).unwrap();
    value["engaged_at"] = serde_json::json!(snapshot.now);
    arrival = serde_json::from_value(value).unwrap();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(arrival);
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        (
            before,
            after,
            snapshot.worktree_groups[0].inactive.is_some()
        ),
        (false, true, false)
    );
}

#[test]
fn root_group_pins_calm_and_archived_agents_above_working_groups() {
    let orders = [
        (AgentStatus::Success, 0),
        (AgentStatus::Idle, 0),
        (AgentStatus::Sleeping, 0),
        (AgentStatus::Paused, 0),
        (AgentStatus::Idle, 25 * 60 * 60),
        (AgentStatus::Success, 25 * 60 * 60),
    ]
    .map(|(status, age)| {
        let snapshot = ranked_snapshot_in(
            vec![
                agent_in("desk", "/repo", status, 0).active_ago(age),
                agent_in("working", "/repo/working", AgentStatus::Running, 0),
            ],
            Some("/repo"),
        );
        labels(&snapshot)
    });

    assert_eq!(orders, std::array::from_fn(|_| vec!["repo", "working"]));
}

#[test]
fn actionable_groups_lead_the_pin_and_hot_parks_follow_it() {
    let snapshot = ranked_snapshot_in(
        vec![
            agent_in("desk", "/repo", AgentStatus::Success, 0),
            agent_in("parked", "/repo/paused", AgentStatus::Paused, 0).active_ago(59 * 60),
            agent_in("working", "/repo/working", AgentStatus::Running, 0),
            agent_in("ask", "/repo/waiting", AgentStatus::Waiting, 0).active_ago(0),
            agent_in("failure", "/repo/failed", AgentStatus::Failed, 0).active_ago(0),
        ],
        Some("/repo"),
    );

    assert_eq!(
        labels(&snapshot),
        ["waiting", "failed", "repo", "paused", "working"]
    );
}

#[test]
fn fresh_ask_group_outranks_hot_park_without_a_root_group() {
    let snapshot = ranked_snapshot(vec![
        agent_in("parked", "/repo/paused", AgentStatus::Paused, 0).active_ago(59 * 60),
        agent_in("ask", "/repo/waiting", AgentStatus::Waiting, 0).active_ago(0),
    ]);

    assert_eq!(labels(&snapshot), ["waiting", "paused"]);
}

#[test]
fn native_child_attention_outranks_the_pinned_root_group() {
    let snapshot = list_agent_checkouts(room(vec![
        agent_in("desk", "/repo", AgentStatus::Success, 0).in_pane("%1"),
        agent_in("parent", "/repo/child", AgentStatus::Success, 0).in_pane("%2"),
        child_state("parent", "ask", AgentStatus::Waiting, 0),
        agent_in("working", "/repo/working", AgentStatus::Running, 0).in_pane("%3"),
    ]))
    .with_project_root(Some(PathBuf::from("/repo")))
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo"),
            pane("%2", "claude", "/repo/child"),
            pane("%3", "claude", "/repo/working"),
        ],
        None,
    );

    assert_eq!(labels(&snapshot), ["child", "repo", "working"]);
}

#[test]
fn process_only_root_group_is_pinned_and_never_folds() {
    let mut snapshot = list_agent_checkouts(room(vec![
        agent_in("working", "/repo/working", AgentStatus::Running, 0).in_pane("%1"),
    ]))
    .with_project_root(Some(PathBuf::from("/repo")))
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/working"),
            pane("%2", "zsh", "/repo"),
        ],
        None,
    );
    let before = labels(&snapshot);
    let root = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.key == "/repo")
        .expect("root group");
    root.clean = Some(true);
    root.trunk_sync = Some(WorktreeTrunkSync::Merged);
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        (
            before,
            labels(&snapshot),
            snapshot.worktree_groups[0].root_lane,
            snapshot.worktree_groups[0].inactive.is_some(),
        ),
        (
            vec!["repo".to_owned(), "working".to_owned()],
            vec!["repo".to_owned(), "working".to_owned()],
            true,
            false,
        )
    );
}

#[test]
fn root_group_with_an_agent_never_folds_on_a_landed_branch() {
    let results = [false, true].map(|merged_pr| {
        let mut snapshot = ranked_snapshot_in(
            vec![
                agent_in("desk", "/repo", AgentStatus::Success, 0),
                agent_in("working", "/repo/working", AgentStatus::Running, 0),
            ],
            Some("/repo"),
        );
        let root = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
            .iter_mut()
            .find(|group| group.key == "/repo")
            .expect("root group");
        root.clean = Some(true);
        if merged_pr {
            root.pr_state = Some(WorktreePrState::Merged);
        } else {
            root.trunk_sync = Some(WorktreeTrunkSync::Merged);
        }
        snapshot.sort_groups_for_presentation();
        let root = snapshot
            .worktree_groups
            .iter()
            .find(|group| group.key == "/repo")
            .expect("root group");
        (labels(&snapshot), root.inactive.is_some(), root.collapses())
    });

    assert_eq!(
        results,
        std::array::from_fn(|_| (vec!["repo".to_owned(), "working".to_owned()], false, false))
    );
}

#[test]
fn root_group_with_own_attention_keeps_urgency_order() {
    let snapshot = ranked_snapshot_in(
        vec![
            agent_in("desk", "/repo", AgentStatus::Waiting, 0).active_ago(5 * 60),
            agent_in("failure", "/repo/failed", AgentStatus::Failed, 0).active_ago(50 * 60),
            agent_in("ask", "/repo/waiting", AgentStatus::Waiting, 0).active_ago(0),
        ],
        Some("/repo"),
    );

    assert_eq!(
        (labels(&snapshot), snapshot.worktree_groups[1].root_lane),
        (
            vec!["failed".to_owned(), "repo".to_owned(), "waiting".to_owned()],
            true
        )
    );
}

#[test]
fn root_pin_survives_snapshot_json_and_presentation_sort() {
    let snapshot = ranked_snapshot_in(
        vec![
            agent_in("desk", "/repo", AgentStatus::Idle, 0).active_ago(25 * 60 * 60),
            agent_in("working", "/repo/working", AgentStatus::Running, 0),
        ],
        Some("/repo"),
    );
    let json = serde_json::to_value(&snapshot).unwrap();
    let markers = json["worktree_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| (group["key"].as_str().unwrap(), group.get("root_lane")))
        .collect::<BTreeMap<_, _>>();
    let mut restored: SidebarSnapshot = serde_json::from_value(json.clone()).unwrap();
    restored.sort_groups_for_presentation();

    assert_eq!(
        (labels(&snapshot), labels(&restored), markers),
        (
            vec!["repo".to_owned(), "working".to_owned()],
            vec!["repo".to_owned(), "working".to_owned()],
            BTreeMap::from([
                ("/repo", Some(&serde_json::Value::Bool(true))),
                ("/repo/working", None),
            ]),
        )
    );
    assert!(restored.worktree_groups[0].root_lane);
    assert!(!restored.worktree_groups[1].root_lane);
}

#[test]
fn active_group_tiering_floats_attention_and_tails_external() {
    let labels_for = |agents: Vec<AgentState>| labels(&ranked_snapshot(agents));
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

    // The external catch-all always tails, even when it holds an attention
    // agent: out-of-project work never displaces a project worktree.
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["beta", "external"]
    );

    // A cooled member still outranks external work in the active partition.
    // Group inactivity is applied by the later presentation sort.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Idle, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["alpha", "external"]
    );
}

#[test]
fn git_rung_does_not_order_active_worktree_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("unknown", "/repo/unknown", AgentStatus::Idle, 1_000),
        agent_in("merged", "/repo/merged", AgentStatus::Idle, 1_000),
        agent_in("dirty", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("clean", "/repo/clean", AgentStatus::Idle, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "clean" => {
                group.clean = Some(true);
                group.landed = Some(false);
            }
            "merged" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            "unknown" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["clean", "dirty", "merged", "unknown"],
        "equal launch times use labels, not git rungs"
    );
}

#[test]
fn newest_registration_precedes_git_for_working_and_success_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("success", "/repo/success", AgentStatus::Success, 1_000),
        agent_in("running", "/repo/working", AgentStatus::Running, 2_000),
        agent_in("idle", "/repo/working", AgentStatus::Idle, 3_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        match group.label.as_str() {
            "success" => group.clean = Some(false),
            "working" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["working", "success"]);
    assert!(
        snapshot.worktree_groups[0].inactive.is_none(),
        "a running member revives a git-finished line of work"
    );
}

#[test]
fn equal_registration_times_use_labels_not_git_or_calm_state() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("sleeping", "/repo/sleeping", AgentStatus::Sleeping, 1_000),
        agent_in("idle", "/repo/idle", AgentStatus::Idle, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(group.label == "sleeping");
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["idle", "sleeping"]);
}

#[test]
fn newer_success_group_leads_without_folding_a_fresh_merge() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged", "/repo/merged", AgentStatus::Success, 1_000),
        agent_in("clean", "/repo/clean", AgentStatus::Success, 2_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(true);
        if group.label == "merged" {
            group.trunk_sync = Some(WorktreeTrunkSync::Merged);
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["clean", "merged"]);
    assert!(snapshot.worktree_groups[0].inactive.is_none());
    assert!(snapshot.worktree_groups[1].inactive.is_none());
}

#[test]
fn inactive_partition_follows_active_work_and_external() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done", "/repo/done", AgentStatus::Success, 1_000),
        agent_in("old", "/repo/old", AgentStatus::Idle, 2_000).active_ago(25 * 60 * 60),
        agent("claude", "outside", AgentStatus::Waiting, 3_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        if group.label == "done" {
            group.clean = Some(true);
            group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            let card = std::sync::Arc::make_mut(&mut group.rows)[0]
                .as_agent_mut()
                .unwrap();
            card.registered_at = Some(ago(7200));
            card.engaged_at = Some(ago(3601));
        }
    }
    snapshot.sort_groups_for_presentation();
    assert_eq!(labels(&snapshot), ["old", "external", "done"]);
    assert!(snapshot.worktree_groups[0].rows[0].archived);
    assert!(snapshot.worktree_groups[2].inactive.is_some());
}

#[test]
fn native_child_attention_keeps_merged_success_active_and_expanded() {
    for status in [AgentStatus::Failed, AgentStatus::Waiting] {
        let mut snapshot = room_with_agent_panes(vec![
            agent_in("parent", "/repo/merged", AgentStatus::Success, 1_000),
            child_state("parent", "child", status, 0),
            agent_in("idle", "/repo/idle", AgentStatus::Idle, 2_000),
        ]);
        let merged = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
            .iter_mut()
            .find(|group| group.label == "merged")
            .expect("merged group present");
        merged.clean = Some(true);
        merged.trunk_sync = Some(WorktreeTrunkSync::Merged);
        std::sync::Arc::make_mut(&mut merged.rows)[0]
            .as_agent_mut()
            .unwrap()
            .engaged_at = Some(ago(3601));
        assert_eq!(merged.rows.len(), 1);
        assert_eq!(merged.rows[0].status(), Some(AgentStatus::Success));
        assert_eq!(merged.rows[0].attention_status(), Some(status));

        snapshot.sort_groups_for_presentation();

        let merged = snapshot
            .worktree_groups
            .iter()
            .find(|group| group.label == "merged")
            .expect("merged group present");
        assert!(merged.inactive.is_none(), "native {status:?} blocks finish");
        assert!(
            !merged.collapses(),
            "an actionable singleton stays expanded"
        );
        assert_eq!(labels(&snapshot), ["merged", "idle"]);
    }
}

#[test]
fn inactive_membership_revives_on_prompts_but_not_resume_activity() {
    for prompt in [false, true] {
        let mut member = agent_in("member", "/repo/work", AgentStatus::Idle, 0);
        member.registered_at = Some(ago(7200));
        member.last_prompt_at = Some(ago(3601));
        member.last_activity = ago(3601);
        let mut snapshot = ranked_snapshot(vec![member.clone()]);
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
        group.clean = Some(true);
        group.pr_state = Some(WorktreePrState::Closed);
        snapshot.sort_groups_for_presentation();
        assert!(snapshot.worktree_groups[0].inactive.is_some());

        member.last_activity = ago(0);
        if prompt {
            member.last_prompt_at = Some(ago(0));
        }
        let mut snapshot = ranked_snapshot(vec![member]);
        let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
        group.clean = Some(true);
        group.pr_state = Some(WorktreePrState::Closed);
        snapshot.sort_groups_for_presentation();
        assert_eq!(
            snapshot.worktree_groups[0].inactive.is_some(),
            !prompt,
            "prompt={prompt}"
        );
    }
}

#[test]
fn landed_group_folds_by_engagement_and_attention_revives_it() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done", "/repo/done", AgentStatus::Success, 1_000).active_ago(5 * 60),
        agent_in("warm", "/repo/warm", AgentStatus::Idle, 2_000).active_ago(
            i64::from(
                crate::config::AttentionConfig::default()
                    .inactive_after_secs
                    .get(),
            ) + 1,
        ),
    ]);
    let done = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.label == "done")
        .expect("done group present");
    done.clean = Some(true);
    done.trunk_sync = Some(WorktreeTrunkSync::Merged);
    let card = std::sync::Arc::make_mut(&mut done.rows)[0]
        .as_agent_mut()
        .unwrap();
    card.registered_at = Some(ago(7200));
    card.engaged_at = Some(ago(3601));
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["warm", "done"]);
    let done = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "done")
        .expect("done group present");
    assert!(done.inactive.is_some());
    assert!(
        done.rows.iter().all(|row| !row.inactive && !row.archived),
        "the group folds from engagement, not its activity clock"
    );

    let done = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.label == "done")
        .expect("done group present");
    std::sync::Arc::make_mut(&mut done.rows)[0]
        .as_agent_mut()
        .expect("agent row")
        .status = AgentStatus::Running;
    snapshot.sort_groups_for_presentation();
    assert_eq!(labels(&snapshot), vec!["warm", "done"]);
    assert!(snapshot.worktree_groups[1].inactive.is_none());

    let done = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.label == "done")
        .unwrap();
    std::sync::Arc::make_mut(&mut done.rows)[0]
        .as_agent_mut()
        .expect("agent row")
        .status = AgentStatus::Waiting;
    snapshot.sort_groups_for_presentation();
    assert_eq!(labels(&snapshot), vec!["done", "warm"]);
    assert!(
        snapshot.worktree_groups[0].inactive.is_none(),
        "attention keeps a merged group hot"
    );
}

#[test]
fn pristine_and_freshly_merged_groups_follow_registration_order() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged", "/repo/merged", AgentStatus::Success, 1_000),
        agent_in("pristine", "/repo/pristine", AgentStatus::Success, 2_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(true);
        group.landed = Some(true);
        match group.label.as_str() {
            "merged" => group.trunk_sync = Some(WorktreeTrunkSync::Merged),
            "pristine" => group.trunk_sync = Some(WorktreeTrunkSync::Pristine),
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["pristine", "merged"]);
    assert!(snapshot.worktree_groups[0].inactive.is_none());
    assert!(snapshot.worktree_groups[1].inactive.is_none());
}

#[test]
fn process_only_group_tails_calm_agent_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("process", "/repo/process", AgentStatus::Idle, 1_000),
        agent_in("idle", "/repo/idle", AgentStatus::Idle, 2_000),
    ]);
    let process = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.label == "process")
        .expect("process group present");
    std::sync::Arc::make_mut(&mut process.rows)[0].card =
        RowCard::Process(crate::store::snapshot::ProcessCard::default());
    process.status_counts.clear();
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["idle", "process"]);
}

#[test]
fn fresh_merged_pr_does_not_sink_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done" => group.pr_state = Some(WorktreePrState::Merged),
            "working" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["done", "working"]);
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|g| g.inactive.is_none())
    );
}

#[test]
fn fresh_closed_pr_does_not_sink_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done" => group.pr_state = Some(WorktreePrState::Closed),
            "working" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["done", "working"]);
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|g| g.inactive.is_none())
    );
}

#[test]
fn open_pr_does_not_sink_calm_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("open-idle", "/repo/open", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "open" => group.pr_state = Some(WorktreePrState::Open),
            "working" => {}
            "done" => group.pr_state = Some(WorktreePrState::Merged),
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["done", "open", "working"]);
}

#[test]
fn fresh_merged_pr_does_not_sink_git_backed_channel_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done-channel", AgentStatus::Idle, 1_000),
        agent_in(
            "working-idle",
            "/repo/working-channel",
            AgentStatus::Idle,
            1_000,
        ),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        group.kind = SidebarWorktreeKind::Channel;
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done-channel" => group.pr_state = Some(WorktreePrState::Merged),
            "working-channel" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["done-channel", "working-channel"]);
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|g| g.inactive.is_none())
    );
}

#[test]
fn attention_member_overrides_git_rung() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("dirty-idle", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("merged-wait", "/repo/merged", AgentStatus::Waiting, 1_000),
    ]);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "merged" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "dirty"],
        "attention urgency beats the merged git rung"
    );
}

#[test]
fn external_group_tails_even_merged_project_work() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged-idle", "/repo/merged", AgentStatus::Idle, 1_000),
        agent("claude", "external-fail", AgentStatus::Failed, 1_000),
    ]);
    let merged = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|group| group.label == "merged")
        .expect("merged group present");
    merged.clean = Some(true);
    merged.trunk_sync = Some(WorktreeTrunkSync::Merged);
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "external"],
        "external stays the hard tail below even a calm merged project group"
    );
}
