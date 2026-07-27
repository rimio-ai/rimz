//! Lane resume: resolving a selector to a place, then focusing, splitting, or
//! restoring it — with native session discovery and recovery materialization.

use super::*;

#[test]
fn concurrent_session_set_selects_the_newest_overlap_cluster() {
    // `second` overlaps neither `first` nor `third` on its own, but both
    // bracket it, so the three merge transitively into one working set.
    let overlapping = vec![
        local_session("claude", "first", 9, 17),
        local_session("codex", "second", 13, 16),
        local_session("claude", "third", 10, 18),
    ];
    // A day apart: two clusters that never touch.
    let disjoint = vec![
        local_session("claude", "yesterday", 9, 10),
        local_session("codex", "today", 33, 34),
    ];

    for (label, observations, expect_resume, expect_skipped) in [
        (
            "transitive overlap merges, newest first",
            overlapping,
            vec!["third", "first", "second"],
            vec![],
        ),
        (
            "only the newest disjoint run resumes",
            disjoint,
            vec!["today"],
            vec!["yesterday"],
        ),
        ("nothing observed", Vec::new(), vec![], vec![]),
    ] {
        let (resume, skipped) = concurrent_session_set(observations);
        assert_eq!(session_ids(&resume), expect_resume, "{label}");
        assert_eq!(session_ids(&skipped), expect_skipped, "{label}");
    }
}

#[test]
fn discovered_candidate_requires_session_and_workspace() {
    let mut observation = local_session("claude", "only", 9, 10);
    observation.session_id = AgentSessionId::from("");
    assert!(ResumeCandidate::from_observation(&observation).is_none());

    observation.session_id = AgentSessionId::from("only");
    observation.workspace = PathBuf::new();
    assert!(ResumeCandidate::from_observation(&observation).is_none());
}

/// Lane selectors resolve to a place before anything is planned. A durable
/// agent outranks a same-named worktree, a scope matches a worktree by name,
/// branch, full path, or directory name, and a PR marker outranks the legacy
/// `pr-<n>` naming. Each case reports the resolved worktree through `Removed`.
#[test]
fn lane_selectors_resolve_to_a_place() {
    let durable = AgentState {
        channel: Some("docs".to_owned()),
        ..agent("codex", "durable", "/other/agent-lane", 1)
    };
    let agent_lane = [durable];
    let review = [LaneWorktree {
        name: "review".to_owned(),
        path: PathBuf::from("/repo-worktrees/docs"),
        branch: Some("feat/docs".to_owned()),
        from_pr: None,
    }];
    let pr_lanes = [
        lane_worktree("pr-42", "legacy", None),
        lane_worktree("review", "pull/42", Some(42)),
    ];
    let scope = |name: &str| LaneResumeSelector::Scope(name.to_owned());

    for (label, selector, agents, worktrees, resolved) in [
        (
            "a durable agent outranks a colliding worktree name",
            scope("docs"),
            agent_lane.as_slice(),
            [lane_worktree("docs", "feat/docs", None)].as_slice(),
            "agent-lane",
        ),
        (
            "scope by worktree name",
            scope("review"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by branch",
            scope("feat/docs"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by full path",
            scope("/repo-worktrees/docs"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by directory name",
            scope("docs"),
            &[],
            &review,
            "review",
        ),
        (
            "a PR marker outranks the legacy pr-<n> name",
            LaneResumeSelector::PullRequest(42),
            &[],
            &pr_lanes,
            "review",
        ),
    ] {
        let error = LaneCase::new(selector, agents)
            .worktrees(worktrees)
            .path_exists(|_| false)
            .liveness(live)
            .run()
            .unwrap_err();
        assert!(
            matches!(error, LaneResumeError::Removed { ref worktree, .. } if worktree == resolved),
            "{label}: got {error:?}"
        );
    }
}

#[test]
fn lane_focus_uses_freshest_live_member_after_pane_dedupe() {
    let agents = [
        agent_on_pane("codex", "older", "/lane", 20, "shared"),
        agent_on_pane("claude", "other", "/lane", 5, "other"),
        agent_on_pane("codex", "newer", "/lane", 2, "shared"),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root("/lane")
        .liveness(live)
        .restore(|| panic!("focus must not load restore config"))
        .run()
        .unwrap();

    assert!(matches!(
        action,
        LaneResumeAction::Focus { pane_id: id, .. } if id == pane_id("shared")
    ));
}

#[test]
fn lane_partial_resume_targets_live_pane_and_only_seeds_closed_members() {
    let agents = [
        agent_on_pane("claude", "live", "/lane", 1, "live-pane"),
        agent_on_pane("codex", "closed", "/lane", 2, "closed-pane"),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root("/lane")
        .liveness(|agent| {
            if agent.agent_id.as_str() == "live" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        })
        .run()
        .unwrap();

    let LaneResumeAction::SplitClosed {
        target_pane_id,
        commands,
        live_labels,
        ..
    } = action
    else {
        panic!("expected partial split");
    };
    assert_eq!(target_pane_id, pane_id("live-pane"));
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        decode_exec_request(&commands[0]).action,
        crate::harness::launch::ExecAction::Resume { ref session_id, .. }
            if session_id == "closed"
    ));
    assert_eq!(live_labels.len(), 1);
}

#[test]
fn lane_rejects_provisional_or_unbacked_conversations() {
    let provisional = [agent("codex", "launch_pending", "/lane", 1)];
    let provisional_error = LaneCase::new(LaneResumeSelector::Current, &provisional)
        .current_root("/lane")
        .run()
        .unwrap_err();
    assert_eq!(
        provisional_error,
        LaneResumeError::Nothing {
            scope: "#lane".to_owned()
        }
    );

    let durable = [agent("codex", "durable", "/lane", 1)];
    let no_conversation = LaneCase::new(LaneResumeSelector::Current, &durable)
        .current_root("/lane")
        .session_backed(|_| false)
        .run()
        .unwrap_err();
    assert_eq!(
        no_conversation,
        LaneResumeError::Nothing {
            scope: "#lane".to_owned()
        }
    );
}

#[test]
fn lane_listing_groups_deduped_members_and_sorts_freshest_first() {
    let agents = [
        agent_on_pane("codex", "old", "/docs", 30, "docs"),
        agent_on_pane("codex", "new", "/docs", 10, "docs"),
        agent_on_pane("claude", "api", "/api", 2, "api"),
    ];
    let action = LaneCase::new(LaneResumeSelector::List, &agents)
        .liveness(|agent| {
            if agent.agent_id.as_str() == "api" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        })
        .restore(|| panic!("listing must not load restore config"))
        .run()
        .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].label, "#api");
    assert_eq!(lanes[0].live, 1);
    assert_eq!(lanes[1].members, 1);
}

#[test]
fn lane_listing_discovers_only_worktrees_without_durable_members() {
    let durable = [agent("codex", "docs", "/repo-worktrees/docs", 5)];
    let worktrees = [
        lane_worktree("docs", "feat/docs", None),
        lane_worktree("native", "feat/native", None),
    ];
    let action = LaneCase::new(LaneResumeSelector::List, &durable)
        .worktrees(&worktrees)
        .discover(|path| {
            assert_eq!(path, Path::new("/repo-worktrees/native"));
            vec![local_session("claude", "native", 9, 10)]
        })
        .restore(|| panic!("listing must not load restore config"))
        .run()
        .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert!(lanes.iter().any(|lane| lane.label == "#native"));
}

/// The team tab is planned first and its panes count against the cap, so a
/// flat member only rides along when the cap leaves room for it.
#[test]
fn lane_all_closed_restores_team_first_within_the_cap() {
    for (label, max, entries, panes, over_cap) in [
        (
            "the two team panes consume a cap of two",
            2,
            ["team"].as_slice(),
            [2].as_slice(),
            true,
        ),
        (
            "a cap of three leaves room for the flat member",
            3,
            ["team", "flat"].as_slice(),
            [2, 1].as_slice(),
            false,
        ),
    ] {
        let (teams, profiles, commands) = team_configs();
        let agents = [
            team_agent("claude", "planner", "planner", "/lane", 1),
            team_agent("codex", "coder", "coder", "/lane", 2),
            agent("codex", "flat", "/lane", 3),
        ];
        let action = LaneCase::new(LaneResumeSelector::Current, &agents)
            .current_root("/lane")
            .max(max)
            .restore(|| {
                Ok(LaneRestoreConfig {
                    teams,
                    profiles,
                    commands,
                })
            })
            .run()
            .unwrap();

        let LaneResumeAction::RestoreClosed { plan, .. } = action else {
            panic!("expected closed restore: {label}");
        };
        let kinds = plan
            .recovery
            .entries
            .iter()
            .map(|entry| match entry {
                RecoveryEntry::Team(_) => "team",
                RecoveryEntry::Flat(_) => "flat",
            })
            .collect::<Vec<_>>();
        let pane_counts = plan
            .recovery
            .entries
            .iter()
            .map(RecoveryEntry::pane_count)
            .collect::<Vec<_>>();
        assert_eq!(kinds, entries, "{label}");
        assert_eq!(pane_counts, panes, "{label}");
        assert_eq!(
            plan.recovery
                .skipped
                .iter()
                .any(|skip| skip.label == "codex:lane" && skip.reason == ResumeSkipReason::OverCap),
            over_cap,
            "{label}"
        );
    }
}

#[test]
fn lane_recovery_materializes_team_first_and_fails_strictly() {
    let dir = tempfile::tempdir().expect("test root");
    let lane = dir.path().join("lane");
    std::fs::create_dir(&lane).expect("lane");
    let Ok(init) = std::process::Command::new("git")
        .arg("-C")
        .arg(&lane)
        .args(["init", "-q", "-b", "main"])
        .status()
    else {
        return;
    };
    if !init.success() {
        return;
    }
    std::fs::write(lane.join("plan.md"), "scratch\n").expect("scratch file");
    let lane = lane.to_string_lossy().into_owned();

    let (mut teams, profiles, commands) = team_configs();
    teams.0.get_mut("forge").expect("forge team").scratch_files = vec!["/plan.md".to_owned()];
    let agents = [
        team_agent("claude", "planner", "planner", &lane, 1),
        team_agent("codex", "coder", "coder", &lane, 2),
        agent("codex", "flat", &lane, 3),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root(&lane)
        .max(3)
        .restore(|| {
            Ok(LaneRestoreConfig {
                teams,
                profiles,
                commands,
            })
        })
        .run()
        .unwrap();
    let LaneResumeAction::RestoreClosed { plan, .. } = action else {
        panic!("expected closed restore");
    };
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let paths =
        crate::store::paths::StatePaths::under(workspace.clone(), &dir.path().join("state"))
            .expect("state paths");
    let runtime = crate::store::paths::RuntimePaths::under(workspace, &dir.path().join("runtime"))
        .expect("runtime paths");
    let store = Store::open(paths, runtime).expect("store");

    let tabs = plan
        .clone()
        .materialize(&store, "rimz-test")
        .expect("strict lane materialization");
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0].pane_count(), 2);
    assert_eq!(tabs[1].pane_count(), 1);
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&lane)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .expect("git status");
    assert!(status.status.success());
    assert_eq!(status.stdout, b"");

    let mut broken = plan;
    let team = broken
        .recovery
        .entries
        .iter_mut()
        .find_map(|entry| match entry {
            RecoveryEntry::Team(team) => Some(team),
            RecoveryEntry::Flat(_) => None,
        })
        .expect("team entry");
    team.cohort.seeds.truncate(1);
    assert!(broken.materialize(&store, "rimz-test").is_err());
}

#[test]
fn recovery_plan_sorts_equal_freshness_by_label() {
    let when: Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    let zed = AgentState {
        last_activity: when,
        ..agent("codex", "zed", "/work/zed", 1)
    };
    let alpha = AgentState {
        last_activity: when,
        ..agent("codex", "alpha", "/work/alpha", 1)
    };
    let flat = plan_resume_detailed(
        &[zed, alpha],
        &BTreeSet::new(),
        ctx(2, None, &no_profiles()),
        |_| true,
        |_| true,
    );
    let mut recovery = RecoveryPlan::new(TeamsConfig::default(), Vec::new(), flat);
    recovery.sort_by_freshness();

    assert_eq!(recovery.labels(), ["#alpha", "#zed"]);
}
