//! Cohort matching and relaunch: which prior members a launch spec claims,
//! how an unresumable match relaunches, and what a reborn team tab seeds.

use super::*;

#[test]
fn cohort_resume_selects_newest_team_member_per_role() {
    let old_planner = team_agent("claude", "old-planner", "planner", "/code/forge", 30);
    let planner = AgentState {
        channel: Some("design".to_owned()),
        ..team_agent("claude", "planner", "planner", "/code/forge", 2)
    };
    let coder = team_agent("codex", "coder", "coder", "/code/forge", 4);
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = cohort(&[old_planner, planner, coder], &cells, Some("forge")).expect("cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/forge")));
    assert_eq!(plan.channel.as_deref(), Some("design"));
    assert!(plan.fresh.is_empty());
}

#[test]
fn cohort_resume_uses_filtered_worktree_even_when_older_than_same_team_elsewhere() {
    let agents = vec![
        team_agent("claude", "newest-planner", "planner", "/code/newer", 1),
        team_agent("codex", "newest-coder", "coder", "/code/newer", 2),
        team_agent("claude", "target-planner", "planner", "/code/restore", 50),
        team_agent("codex", "target-coder", "coder", "/code/restore", 60),
    ];
    let scoped = agents
        .into_iter()
        .filter(|agent| agent.worktree_path.as_deref() == Some("/code/restore"))
        .collect::<Vec<_>>();
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = cohort(&scoped, &cells, Some("forge")).expect("filtered cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("target-planner"), Some("target-coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/restore")));
}

#[test]
fn cohort_resume_includes_every_ended_session_backed_member() {
    let planner = team_agent("claude", "planner", "planner", "/code/forge", 1);
    let planner = AgentState {
        ended_at: Some(planner.last_seen),
        ..planner
    };
    let coder = team_agent("codex", "coder", "coder", "/code/forge", 2);
    let coder = AgentState {
        ended_at: Some(coder.last_seen),
        ..coder
    };

    let plan = cohort(
        &[planner, coder],
        &[
            cohort_cell("claude", Some("planner")),
            cohort_cell("codex", Some("coder")),
        ],
        Some("forge"),
    )
    .expect("closed team member remains a cohort candidate");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
}

#[test]
fn relaunch_spec_prefers_team_role_then_profile_then_kind() {
    assert_eq!(
        relaunch_spec(Some("forge"), Some("coder"), Some("codex-plan"), "codex"),
        "forge.coder"
    );
    assert_eq!(
        relaunch_spec(None, Some("coder"), Some("codex-plan"), "codex"),
        "codex-plan"
    );
    assert_eq!(relaunch_spec(Some("forge"), None, None, "codex"), "codex");
    assert_eq!(relaunch_spec(Some(""), Some(""), Some(""), "codex"), "codex");
}

#[test]
fn closed_cohort_specs_name_resumable_members_newest_first() {
    let coder = team_agent("codex", "coder", "coder", "/code/forge", 2);
    let coder = AgentState {
        ended_at: Some(coder.last_seen),
        ..coder
    };
    let reviewer = team_agent("claude", "reviewer", "reviewer", "/code/forge", 8);
    let live_member = team_agent("claude", "live", "planner", "/code/forge", 1);
    let provisional = team_agent(
        "codex",
        "launch_019f2cecea067320b667c5946d266e64",
        "scout",
        "/code/forge",
        3,
    );
    let subagent = AgentState {
        parent_agent_id: Some(AgentSessionId::from("coder")),
        ..agent("claude", "sub", "/code/forge", 1)
    };

    let live_id = live_member.agent_id.clone();
    let specs = closed_cohort_specs(
        &[coder, reviewer, live_member, provisional, subagent],
        move |candidate| {
            if candidate.agent_id == live_id {
                AgentLiveness::Live { pid: 42 }
            } else {
                AgentLiveness::Dead
            }
        },
    );

    assert_eq!(specs, ["forge.coder", "forge.reviewer"]);
}

#[test]
fn closed_cohort_specs_dedupe_repeat_identities() {
    let old = team_agent("codex", "old", "coder", "/code/forge", 30);
    let new = team_agent("codex", "new", "coder", "/code/forge", 2);

    let specs = closed_cohort_specs(&[old, new], dead);

    assert_eq!(specs, ["forge.coder"]);
}

#[test]
fn cohort_refuses_live_and_unmatched_specs() {
    // The pet name is what proves the live-member label format.
    let live_planner = AgentState {
        name: Some("swift-otter".to_owned()),
        ..team_agent("claude", "planner", "planner", "/code/forge", 1)
    };

    for (label, agents, cells, team, liveness, on_disk, expected) in [
        (
            "a still-live member blocks the relaunch",
            vec![live_planner],
            vec![cohort_cell("claude", Some("planner"))],
            Some("forge"),
            live as fn(&AgentState) -> AgentLiveness,
            true,
            CohortResumeErr::MembersStillLive {
                labels: vec!["claude:swift-otter (planner)".to_owned()],
            },
        ),
        (
            "no prior session of the requested kind",
            vec![agent("codex", "c1", "/code/query-engine", 1)],
            vec![cohort_cell("claude", None)],
            None,
            dead,
            true,
            CohortResumeErr::NothingToResume {
                spec: "claude".to_owned(),
            },
        ),
        (
            "a vanished worktree drops its members",
            vec![agent("claude", "a1", "/code/gone", 1)],
            vec![cohort_cell("claude", None)],
            None,
            dead,
            false,
            CohortResumeErr::NothingToResume {
                spec: "claude".to_owned(),
            },
        ),
    ] {
        let err =
            cohort_with(&agents, &cells, team, liveness, |_| on_disk, |_| true).expect_err(label);
        assert_eq!(err, expected, "{label}");
    }
}

/// A cell that matched a prior member but cannot resume it keeps the match and
/// relaunches fresh, rather than asking an adapter to reopen a session that is
/// not there.
#[test]
fn cohort_relaunches_an_unresumable_match_fresh() {
    let provisional = "launch_019f2cecea067320b667c5946d266e64";

    for (label, agents, cells, team, redeemable, fresh, cwd) in [
        (
            "a kind with no resume CLI",
            vec![agent("ghost", "g1", "/code/query-engine", 1)],
            vec![cohort_cell("ghost", None)],
            None,
            true,
            "ghost:query-engine",
            "/code/query-engine",
        ),
        (
            "a provisional launch placeholder",
            vec![team_agent("codex", provisional, "coder", "/code/pets-l", 4)],
            vec![cohort_cell("codex", Some("coder"))],
            Some("forge"),
            true,
            "codex:pets-l",
            "/code/pets-l",
        ),
        (
            "a session the provider never persisted",
            vec![agent("claude", "a1", "/code/query-engine", 1)],
            vec![cohort_cell("claude", None)],
            None,
            false,
            "claude:query-engine",
            "/code/query-engine",
        ),
    ] {
        let plan = cohort_with(&agents, &cells, team, dead, |_| true, |_| redeemable).expect(label);

        assert_eq!(plan.seeds, vec![CohortSeed::Fresh], "{label}");
        assert_eq!(plan.fresh, vec![fresh.to_owned()], "{label}");
        assert_eq!(plan.cwd.as_deref(), Some(Path::new(cwd)), "{label}");
    }
}

#[test]
fn cohort_resume_matches_inline_group_by_launch_ordinal() {
    let old = inline_agent("claude", "old", "launch_old", 0, "/code/old", 50);
    let first = inline_agent("codex", "first", "launch_new", 0, "/code/new", 2);
    let second = inline_agent("claude", "second", "launch_new", 1, "/code/new", 3);
    let cells = vec![cohort_cell("claude", None), cohort_cell("codex", None)];

    let plan = cohort(&[old, first, second], &cells, None).expect("inline cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("first"), Some("second")]
    );
    assert_eq!(plan.launch_group.as_deref(), Some("launch_new"));
}

/// `match_cohort` walks a ladder: a named team claims by role, an inline group
/// claims by launch ordinal, then by role, then by bare kind. Legacy members
/// carrying roles but no ordinals must still land on their own cell.
#[test]
fn match_cohort_resolves_cells_by_team_then_ordinal_then_role_then_kind() {
    let roled = |kind, id, role: &str| AgentState {
        launch_group: Some("launch_new".to_owned()),
        role: Some(role.to_owned()),
        ..agent(kind, id, "/code/new", 2)
    };
    let grouped = |kind, id| AgentState {
        launch_group: Some("launch_new".to_owned()),
        ..agent(kind, id, "/code/new", 2)
    };

    let by_role = [
        roled("claude", "planner", "planner"),
        roled("claude", "coder", "coder"),
    ];
    let by_kind = [grouped("claude", "claude"), grouped("codex", "codex")];
    let mixed = [
        team_agent("claude", "team", "planner", "/code/forge", 3),
        team_agent("codex", "team-coder", "coder", "/code/forge", 4),
        inline_agent("claude", "inline", "launch_inline", 0, "/code/forge", 2),
        inline_agent(
            "codex",
            "inline-coder",
            "launch_inline",
            1,
            "/code/forge",
            1,
        ),
    ];

    for (label, candidates, cells, team, expected) in [
        (
            "same-kind inline members without ordinals claim by role",
            by_role.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("coder")),
                cohort_cell("claude", Some("planner")),
            ],
            None,
            vec![Some("coder"), Some("planner")],
        ),
        (
            "inline members without roles fall back to bare kind",
            by_kind.iter().collect::<Vec<_>>(),
            vec![cohort_cell("codex", None), cohort_cell("claude", None)],
            None,
            vec![Some("codex"), Some("claude")],
        ),
        (
            "one pool, team membership claims the cell",
            mixed.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("planner")),
                cohort_cell("codex", Some("coder")),
            ],
            Some("forge"),
            vec![Some("team"), Some("team-coder")],
        ),
        (
            "the same pool without a team falls to the inline group",
            mixed.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("planner")),
                cohort_cell("codex", Some("coder")),
            ],
            None,
            vec![Some("inline"), Some("inline-coder")],
        ),
    ] {
        let matched = match_cohort(&candidates, &cells, team)
            .into_iter()
            .map(|agent| agent.map(|agent| agent.agent_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(matched, expected, "{label}");
    }
}

/// A `..` segment in the requested worktree normalizes before matching, and a
/// named team keeps every sibling in the cohort — so a closed planner still
/// focuses its live reviewer.
#[test]
fn cohort_relaunch_normalizes_worktrees_and_keeps_named_team_siblings() {
    let planner = AgentState {
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..team_agent("claude", "planner", "planner", "/code/feature", 3)
    };
    let reviewer = team_agent("codex", "reviewer", "reviewer", "/code/feature", 1);

    assert_eq!(
        inspect_cohort_relaunch(
            &[planner, reviewer],
            Path::new("/code/topic/../feature"),
            &[cohort_cell("claude", Some("planner"))],
            Some("forge"),
        ),
        CohortRelaunchState::Present {
            focus_pane: Some(pane_id("terminal_reviewer")),
        }
    );
}

#[test]
fn cohort_relaunch_presence_table() {
    let one_cell = vec![cohort_cell("codex", None)];
    let two_cells = vec![cohort_cell("claude", None), cohort_cell("codex", None)];
    let closed = |agent: AgentState| AgentState {
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..agent
    };
    let paneless = |agent: AgentState| AgentState {
        pane: None,
        ..agent
    };
    let member = |kind, id, group, ordinal, secs| {
        inline_agent(kind, id, group, ordinal, "/code/feature", secs)
    };

    let with_pane = agent("codex", "pane", "/code/feature", 3);
    let live_without_pane = {
        let base = agent("codex", "live", "/code/feature", 1);
        AgentState {
            runtime_owner: Some(crate::store::runtime::current_process_owner(
                crate::pane::RuntimeOwnerKind::Agent,
                base.agent_id.to_string(),
            )),
            ..paneless(base)
        }
    };
    let closed_newest_group = vec![
        member("claude", "old-planner", "launch_old", 0, 10),
        member("codex", "old-coder", "launch_old", 1, 9),
        closed(member("claude", "new-planner", "launch_new", 0, 2)),
        closed(member("codex", "new-coder", "launch_new", 1, 1)),
    ];
    let present_group = vec![
        member("claude", "older", "launch_group", 0, 5),
        member("codex", "fresher", "launch_group", 1, 1),
    ];

    for (label, agents, cells, expected) in [
        ("absent", Vec::new(), &one_cell, CohortRelaunchState::Absent),
        (
            "ended",
            vec![closed(agent("codex", "ended", "/code/feature", 4))],
            &one_cell,
            CohortRelaunchState::Closed,
        ),
        (
            "unknown with pane",
            vec![with_pane],
            &one_cell,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_pane")),
            },
        ),
        (
            "unknown without pane",
            vec![paneless(agent("codex", "paneless", "/code/feature", 2))],
            &one_cell,
            CohortRelaunchState::Closed,
        ),
        (
            "live without pane",
            vec![live_without_pane],
            &one_cell,
            CohortRelaunchState::Present { focus_pane: None },
        ),
        (
            "newest inline group decides, and it is closed",
            closed_newest_group,
            &two_cells,
            CohortRelaunchState::Closed,
        ),
        (
            "focus lands on the freshest present pane",
            present_group,
            &two_cells,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_fresher")),
            },
        ),
    ] {
        assert_eq!(
            inspect_cohort_relaunch(&agents, Path::new("/code/feature"), cells, None),
            expected,
            "{label}"
        );
    }
}

/// A restorable team tab carries one seed per declared role, in the team's own
/// layout order: a prior member resumes, a missing one launches fresh, and a
/// group whose team no longer resolves is left alone entirely.
#[test]
fn team_restore_tabs_seed_every_declared_role() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 5);

    for (label, agents, teams, expected) in [
        (
            "declared layout order, not arrival order",
            vec![coder, planner.clone()],
            teams.clone(),
            Some(vec![Some("planner"), Some("coder")]),
        ),
        (
            "a missing member launches fresh beside the resumed one",
            vec![planner.clone()],
            teams,
            Some(vec![Some("planner"), None]),
        ),
        (
            "a team that no longer resolves plans no tab",
            vec![planner],
            TeamsConfig::default(),
            None,
        ),
    ] {
        let tabs = plan_team_restore_tabs(
            &agents,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
            |_| true,
        );

        let Some(expected) = expected else {
            assert!(tabs.is_empty(), "{label}");
            continue;
        };
        assert_eq!(tabs.len(), 1, "{label}");
        assert_eq!(tabs[0].label, "#forge", "{label}");
        let seeds = tabs[0]
            .cohort
            .seeds
            .iter()
            .map(resume_id)
            .collect::<Vec<_>>();
        assert_eq!(seeds, expected, "{label}");
    }
}

#[test]
fn split_team_and_flat_keeps_unmatched_agents_for_flat_resume() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);
    let flat = agent("codex", "flat", "/repo/other", 5);

    let (tabs, flat_agents) = split_team_and_flat(
        &[planner, flat],
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        |_| true,
        |_| true,
    );

    assert_eq!(tabs.len(), 1);
    assert_eq!(flat_agents.len(), 1);
    assert_eq!(flat_agents[0].agent_id.as_str(), "flat");
}
