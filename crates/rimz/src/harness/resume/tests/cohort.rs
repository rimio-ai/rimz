//! Cohort matching and relaunch: which prior members a launch spec claims,
//! how an unresumable match relaunches, and what a reborn team tab seeds.

use super::*;

#[test]
fn live_team_seats_are_refilled_without_planning_a_team_tab() {
    let (teams, profiles, commands) = team_configs();
    let parked = vec![
        team_agent("claude", "old-planner", "planner", "/repo/forge", 10),
        team_agent("codex", "old-coder", "coder", "/repo/forge", 10),
    ];
    for fully_live in [false, true] {
        let mut roster = parked.clone();
        roster.push(team_agent("codex", "live-coder", "coder", "/repo/forge", 1));
        if fully_live {
            roster.push(team_agent(
                "claude",
                "live-planner",
                "planner",
                "/repo/forge",
                1,
            ));
        }
        let (tabs, flat, refilled) = split_team_and_flat(
            &parked,
            &NO_LOGINS,
            &NO_ACCOUNTS,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            &WORKSPACE,
            |_| true,
            |_| true,
            false,
            &roster,
            |agent| {
                if agent.agent_id.as_str().starts_with("live-") {
                    live(agent)
                } else {
                    dead(agent)
                }
            },
        );
        assert!(tabs.is_empty(), "a live cohort already owns its tab");
        assert!(refilled.contains(&(parked[1].kind.clone(), parked[1].agent_id.clone())));
        assert_eq!(refilled.len(), if fully_live { 2 } else { 1 });
        assert_eq!(
            flat,
            if fully_live {
                Vec::new()
            } else {
                vec![parked[0].clone()]
            }
        );
    }
}

#[test]
fn an_ended_parked_row_never_claims_the_seat_a_live_member_refills() {
    let (teams, profiles, commands) = team_configs();
    let parked = vec![
        AgentState {
            ended_at: Some(jiff::Timestamp::now()),
            ..team_agent("codex", "ended-coder", "coder", "/repo/forge", 20)
        },
        team_agent("codex", "old-coder", "coder", "/repo/forge", 10),
    ];
    let mut roster = parked.clone();
    roster.push(team_agent("codex", "live-coder", "coder", "/repo/forge", 1));
    let (_, _, refilled) = split_team_and_flat(
        &parked,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        &WORKSPACE,
        |_| true,
        |_| true,
        false,
        &roster,
        |agent| {
            if agent.agent_id.as_str().starts_with("live-") {
                live(agent)
            } else {
                dead(agent)
            }
        },
    );
    assert_eq!(
        refilled,
        BTreeSet::from([(parked[1].kind.clone(), parked[1].agent_id.clone())])
    );
}

#[test]
fn fresh_cohort_seeds_retain_the_parked_session_key() {
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 1);
    let plan = cohort_with(
        std::slice::from_ref(&coder),
        &[cohort_cell("codex", Some("coder"))],
        Some("forge"),
        dead,
        |_| true,
        |_| false,
    )
    .unwrap();
    assert_eq!(plan.seeds, [CohortSeed::Fresh]);
    assert_eq!(
        plan.refilled,
        BTreeSet::from([(coder.kind, coder.agent_id)])
    );
}

#[test]
fn recorded_team_restore_keeps_role_layers() {
    let (mut teams, profiles, commands) = team_configs();
    let binding = &mut teams.0.get_mut("forge").unwrap().roles[0];
    binding.args = Some("--verbose".into());
    binding.system_prompt_file = Some(crate::config::PromptSource::Text {
        origin: "/role.md".into(),
        text: "Role".into(),
    });
    binding
        .append_system_prompt_files
        .push(crate::config::PromptSource::Text {
            origin: "/append.md".into(),
            text: "Append".into(),
        });
    let expected = binding.clone();
    for recorded in [false, true] {
        let mut planner = team_agent("claude", "planner", "planner", "/repo/forge", 1);
        planner.profile = Some("claude-plan".into());
        planner.record = recorded.then(|| {
            Box::new(crate::agents::LaunchRecord {
                model: Some("opus".into()),
                effort: Some("high".into()),
                agent: None,
            })
        });
        let tabs = plan_team_restore_tabs(
            &[planner],
            &NO_LOGINS,
            &NO_ACCOUNTS,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            &WORKSPACE,
            |_| true,
            |_| true,
            false,
            &[],
            dead,
        )
        .0;
        assert_eq!(tabs.len(), 1);
        let cell = tabs[0].layout.agent_cells().next().unwrap();
        assert!(cell.args.contains(&"--verbose".into()), "{:?}", cell.args);
        assert_eq!(cell.system_prompt_file, expected.system_prompt_file);
        assert_eq!(
            cell.append_system_prompt_files,
            expected.append_system_prompt_files
        );
        if recorded {
            assert_eq!(cell.launch.model.as_deref(), Some("opus"));
            assert_eq!(cell.launch.effort.as_deref(), Some("high"));
        }
    }
}

#[test]
fn member_posture_keeps_role_layers_for_a_restart() {
    let (mut teams, profiles, _) = team_configs();
    let binding = &mut teams.0.get_mut("forge").unwrap().roles[0];
    binding.args = Some("--verbose".into());
    binding.system_prompt_file = Some(crate::config::PromptSource::Text {
        origin: "/role.md".into(),
        text: "Role".into(),
    });
    let expected = binding.system_prompt_file.clone();
    let mut planner = team_agent("claude", "planner", "planner", "/repo/forge", 1);
    planner.profile = Some("claude-plan".into());
    planner.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("opus".into()),
        ..Default::default()
    }));
    let request = PostureRequest {
        record: planner.record.as_deref(),
        profile: planner.profile.as_deref(),
        kind: &planner.kind,
        stamped_mode: planner.mode,
        stamped_tier: None,
    };

    let posture = resolve_member_posture(
        request,
        &profiles,
        &teams,
        planner.team.as_deref(),
        planner.role.as_deref(),
    );
    assert_eq!(posture.degraded, None);
    assert!(
        posture.launch.args.contains(&"--verbose".into()),
        "{:?}",
        posture.launch.args
    );
    assert_eq!(posture.launch.system_prompt_file, expected);
    assert_eq!(posture.launch.model.as_deref(), Some("opus"));

    let roleless = resolve_member_posture(request, &profiles, &teams, None, None);
    assert!(!roleless.launch.args.contains(&"--verbose".into()));
    assert_eq!(roleless.launch.system_prompt_file, None);
}

#[test]
fn recorded_cross_provider_cohorts_match_without_a_tier() {
    for base in [Some("codex"), None] {
        let mut agent = agent("codex", "a1", "/repo", 1);
        agent.profile = Some("planner".into());
        agent.record = Some(Box::new(crate::agents::LaunchRecord {
            model: Some("gpt-6-sol".into()),
            agent: base.map(str::to_owned),
            ..Default::default()
        }));
        agent.team = Some("forge".into());
        agent.launch_group = Some("group".into());
        let cell = profile_cell("claude", "planner");
        let pool = [&agent];
        assert!(
            match_single_cohort(&pool, &cell)[0].is_some(),
            "base {base:?}"
        );
        assert!(match_team_cohort(&pool, std::slice::from_ref(&cell), "forge")[0].is_some());
        assert!(match_inline_cohort(&pool, &[cell])[0].is_some());
    }
}

#[test]
fn replacement_base_keeps_saved_mode() {
    check_replacement_base(None);
}

#[test]
fn replacement_base_rescues_a_deleted_base() {
    check_replacement_base(Some("deleted"));
}

fn check_replacement_base(saved_base: Option<&str>) {
    let mut profiles = profiles("planner", profile("claude"));
    profiles.0.insert("reviewer".into(), profile("claude"));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    agent.mode = Some(PermissionMode::Yolo);
    agent.record = Some(Box::new(crate::agents::LaunchRecord {
        agent: saved_base.map(str::to_owned),
        ..Default::default()
    }));
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(agent))],
        &profiles,
        &ResumeOverrides {
            agent: Some("reviewer"),
            ..Default::default()
        },
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.launch.mode, Some(PermissionMode::Yolo));
    assert!(cell.args.contains(&"--dangerously-skip-permissions".into()));
}

#[test]
fn legacy_cohort_resume_preserves_the_resolved_team_prompts() {
    use crate::harness::team_prompt::{Consensus, TeamPrompt};

    let mut worker = profile("claude");
    worker.system_prompt_file = Some(crate::config::PromptSource::Text {
        origin: "/definitions/worker.md".into(),
        text: "Worker".into(),
    });
    let profiles = profiles("worker", worker);
    for (fresh, recorded_profile, recorded) in [
        (false, false, false),
        (false, false, true),
        (false, true, false),
        (true, false, false),
    ] {
        let mut cell = crate::harness::spec::profile_cell("worker", &profiles).unwrap();
        cell.team_prompt = Some(TeamPrompt {
            consensus: Consensus::BuiltIn,
            files: vec![crate::config::PromptSource::Text {
                origin: "/definitions/team.md".into(),
                text: "Team instructions".into(),
            }],
        });
        let base = cell.system_prompt_file.clone();
        let team_prompt = cell.team_prompt.clone();
        let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(cell));
        let mut agent = agent("claude", "a1", "/repo", 1);
        agent.profile = recorded_profile.then(|| "worker".into());
        agent.record = recorded.then(|| Box::new(crate::agents::LaunchRecord::default()));
        let seeds = [if fresh {
            CohortSeed::Fresh
        } else {
            CohortSeed::Resume(Box::new(agent))
        }];
        let result = restore_routed_cells(&mut layout, &seeds, &profiles, &Default::default());
        result.unwrap();
        let cell = layout.agent_cells().next().unwrap();
        assert_eq!(cell.system_prompt_file, base);
        assert_eq!(cell.team_prompt, team_prompt);
        assert!(cell.launch.record.is_some());
    }
}

#[test]
fn cohort_alias_replay_uses_the_durable_record_not_the_finalized_cell() {
    check_cohort_alias_replay(None);
}

#[test]
fn legacy_model_override_bypasses_observed_alias_replay() {
    check_cohort_alias_replay(Some("sol"));
}

#[test]
fn legacy_tier_override_bypasses_observed_alias_replay() {
    check_cohort_alias_replay(Some("tier"));
}

fn check_cohort_alias_replay(override_model: Option<&str>) {
    let mut profile = profile("codex");
    profile.model = Some("sol".into());
    profile.definition_renders = routed_profile(None).definition_renders;
    let profiles = profiles("planner", profile);
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    let store = crate::Store::open(
        crate::StatePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap(),
        runtime.clone(),
    )
    .unwrap();
    let machine = toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    let tiers = crate::config::tiers::TierConfig::default();
    for record in [None, Some("sol")] {
        let (expected_model, expected_record) = if record.is_none() && override_model.is_none() {
            ("observed-model", "observed-model")
        } else {
            ("gpt-6-sol", "sol")
        };
        let mut agent = agent("codex", "a1", "/repo", 1);
        agent.profile = Some("planner".into());
        agent.model = Some("observed-model".into());
        agent.record = record.map(|model| {
            Box::new(crate::agents::LaunchRecord {
                model: Some(model.into()),
                ..Default::default()
            })
        });
        let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
            crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
        ));
        let seeds = vec![CohortSeed::Resume(Box::new(agent.clone()))];
        restore_routed_cells(
            &mut layout,
            &seeds,
            &profiles,
            &ResumeOverrides {
                preset: crate::agents::LaunchPreset {
                    model: override_model
                        .filter(|value| *value != "tier")
                        .map(str::to_owned),
                    ..Default::default()
                },
                tier: (override_model == Some("tier"))
                    .then_some((crate::config::tiers::ModelTier::Junior, &tiers)),
                ..Default::default()
            },
        )
        .unwrap();
        let panes = compile_layout_panes(
            &layout,
            LayoutPaneParams {
                runtime: &runtime,
                cwd: root.path(),
                cleanup_worktree: false,
                in_place: false,
                resume_seeds: Some(&seeds),
                launch_identities: &[],
                fallback_channel: None,
                loop_reminder: None,
            },
        )
        .unwrap();
        let mut request = decode_exec_request(&panes.columns[0].panes[0].argv);
        crate::harness::launch_plan::resolve_model(
            &mut request,
            &machine,
            &runtime,
            &crate::agents::ProviderLogin::default_for(agent.kind.clone()),
            Some(&agent),
            None,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            request.identity.params.model.as_deref(),
            Some(expected_model)
        );
        assert!(
            request
                .action
                .extra_args()
                .windows(2)
                .any(|args| args == ["--model", expected_model])
        );
        assert_eq!(
            request
                .identity
                .params
                .record
                .as_ref()
                .unwrap()
                .model
                .as_deref(),
            Some(expected_record)
        );
        store
            .attach_agent_pane(
                &agent.kind,
                &agent.agent_id,
                None,
                &crate::ids::LoginName::default(),
                "resume-test",
                &pane_id("terminal_a1"),
                crate::pane::RuntimeOwner::new(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "a1",
                    42,
                    None,
                ),
                None,
                None,
                Some(&request.identity.params),
            )
            .unwrap();
        let events = store.read_events().unwrap();
        let crate::store::event::EventKind::AgentAttach(attach) = events.last().unwrap().kind()
        else {
            panic!("resume attach");
        };
        assert_eq!(
            attach.record.unwrap().model.as_deref(),
            Some(expected_record)
        );
    }
}

#[test]
fn untiered_resume_rebuilds_the_record_and_saves_overrides() {
    let mut profiles = profiles("planner", profile("claude"));
    profiles.0.insert("coder".into(), profile("codex"));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    agent.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("opus[1m]".into()),
        effort: Some("high".into()),
        agent: None,
    }));
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    let mut seeds = vec![CohortSeed::Resume(Box::new(agent))];
    restore_routed_cells(&mut layout, &seeds, &profiles, &Default::default()).unwrap();
    assert_eq!(
        layout.agent_cells().next().unwrap().launch.model.as_deref(),
        Some("opus[1m]")
    );
    let overrides = ResumeOverrides {
        preset: crate::agents::LaunchPreset {
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    restore_routed_cells(&mut layout, &seeds, &profiles, &overrides).unwrap();
    let cell = layout.agent_cells().next().unwrap();
    let record = cell.launch.record.as_ref().unwrap();
    assert_eq!(record.model.as_deref(), Some("sonnet"));
    assert_eq!(record.effort.as_deref(), Some("low"));
    assert!(cell.launch.tier.is_none());
    let mut coder = super::agent("codex", "a2", "/repo", 1);
    coder.profile = Some("coder".into());
    seeds.push(CohortSeed::Resume(Box::new(coder)));
    seeds.push(CohortSeed::Fresh);
    for name in ["coder", "planner"] {
        layout.columns[0]
            .rows
            .push(crate::harness::spec::Cell::Agent(
                crate::harness::spec::profile_cell(name, &profiles).unwrap(),
            ));
    }
    restore_routed_cells(&mut layout, &seeds, &profiles, &overrides).unwrap();
    for cell in layout.agent_cells() {
        let record = cell.launch.record.as_ref().unwrap();
        assert_eq!(record.model.as_deref(), Some("sonnet"));
        assert_eq!(record.effort.as_deref(), Some("low"));
    }
}

#[test]
fn resume_overrides_keep_provider_args_and_record_in_sync() {
    use crate::config::tiers::ModelTier;
    let mut profiles = profiles("planner", routed_profile(None));
    let mut reviewer = profile("claude");
    reviewer.model = Some("sonnet".into());
    reviewer.effort = Some("low".into());
    reviewer.args = Some("--strict-mcp-config".into());
    profiles.0.insert("reviewer".into(), reviewer);
    let tiers = crate::config::tiers::TierConfig::default();
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    for base in [None, Some("reviewer")] {
        for tier in [None, Some((ModelTier::Principal, &tiers))] {
            for (model, effort) in [
                (None, None),
                (Some("sonnet[1m]"), None),
                (None, Some("medium")),
                (Some("sonnet[1m]"), Some("medium")),
            ] {
                let mut agent = agent("claude", "a1", "/repo", 1);
                agent.profile = Some("planner".into());
                agent.record = Some(Box::new(crate::agents::LaunchRecord {
                    model: Some("opus[1m]".into()),
                    effort: Some("xhigh".into()),
                    agent: Some("reviewer".into()),
                }));
                let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
                    crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
                ));
                let seeds = [CohortSeed::Resume(Box::new(agent))];
                restore_routed_cells(
                    &mut layout,
                    &seeds,
                    &profiles,
                    &ResumeOverrides {
                        agent: base,
                        tier,
                        preset: crate::agents::LaunchPreset {
                            model: model.map(str::to_owned),
                            effort: effort.map(str::to_owned),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .unwrap();
                let cell = layout.agent_cells().next().unwrap();
                let record = cell.launch.record.as_ref().unwrap();
                let panes = compile_layout_panes(
                    &layout,
                    LayoutPaneParams {
                        runtime: &runtime,
                        cwd: root.path(),
                        cleanup_worktree: false,
                        in_place: false,
                        resume_seeds: Some(&seeds),
                        launch_identities: &[],
                        fallback_channel: None,
                        loop_reminder: None,
                    },
                )
                .unwrap();
                let request = decode_exec_request(&panes.columns[0].panes[0].argv);
                for (flag, expected) in [("--model", &record.model), ("--effort", &record.effort)] {
                    let values = request
                        .action
                        .extra_args()
                        .windows(2)
                        .filter(|pair| pair[0] == flag)
                        .map(|pair| pair[1].as_str())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        values,
                        vec![expected.as_deref().unwrap()],
                        "{base:?} {tier:?} {model:?} {effort:?}: {flag}"
                    );
                }
                assert_eq!(record.model, request.identity.params.model);
                assert_eq!(record.effort, request.identity.params.effort);
                assert_eq!(
                    record.model.as_deref(),
                    model.or(Some(if tier.is_some() {
                        "fable"
                    } else if base.is_some() {
                        "sonnet"
                    } else {
                        "opus[1m]"
                    }))
                );
                assert_eq!(record.agent.as_deref(), Some("reviewer"));
                assert!(cell.args.iter().any(|arg| arg == "--strict-mcp-config"));
                if let Some(stamp) = &cell.launch.tier {
                    assert_eq!(record.model.as_deref(), Some(stamp.model.as_str()));
                }
                assert_eq!(
                    cell.launch.tier.is_some(),
                    tier.is_some() && model.is_none()
                );
                let reopened = resolve_posture(
                    PostureRequest {
                        profile: Some("planner"),
                        kind: &cell.kind,
                        stamped_mode: cell.launch.mode,
                        stamped_tier: cell.launch.tier.as_deref(),
                        record: Some(record),
                    },
                    &profiles,
                );
                assert!(reopened.degraded.is_none());
                assert_eq!(reopened.launch.record.as_ref().unwrap().agent, record.agent);
                assert!(
                    reopened
                        .launch
                        .args
                        .iter()
                        .any(|arg| arg == "--strict-mcp-config")
                );
            }
        }
    }
}

#[test]
fn resume_agent_override_requires_same_kind_and_records_base() {
    let mut profiles = profiles("planner", profile("claude"));
    let mut reviewer = profile("claude");
    reviewer.model = Some("sonnet".into());
    profiles.0.insert("reviewer".into(), reviewer);
    profiles.0.insert("coder".into(), profile("codex"));
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    let mut seeds = vec![CohortSeed::Resume(Box::new(agent))];
    restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            agent: Some("reviewer"),
            ..Default::default()
        },
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.launch.profile.as_deref(), Some("planner"));
    assert_eq!(
        cell.launch.record.as_ref().unwrap().agent.as_deref(),
        Some("reviewer")
    );
    assert_eq!(cell.launch.model.as_deref(), Some("sonnet"));
    let err = restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            agent: Some("coder"),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(!err.contains("no longer resolves"), "{err}");
    assert!(
        err.contains("claude") && err.contains("codex") && err.contains("fresh"),
        "{err}"
    );
    let mut coder = super::agent("codex", "a2", "/repo", 1);
    coder.profile = Some("coder".into());
    seeds.push(CohortSeed::Resume(Box::new(coder)));
    layout.columns[0]
        .rows
        .push(crate::harness::spec::Cell::Agent(
            crate::harness::spec::profile_cell("coder", &profiles).unwrap(),
        ));
    let err = restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            agent: Some("reviewer"),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("claude") && err.contains("codex") && err.contains("fresh"),
        "{err}"
    );
}

#[test]
fn resume_tier_is_same_kind_and_never_climbs() {
    use crate::config::tiers::ModelTier;
    let mut profiles = profiles("planner", routed_profile(None));
    let mut coder_profile = routed_profile(None);
    coder_profile.agent = "codex".into();
    coder_profile.model = Some("astra".into());
    profiles.0.insert("coder".into(), coder_profile);
    let tiers: crate::config::tiers::TierConfig =
        toml::from_str("principal = ['gpt-6-sol']").unwrap();
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    let mut seeds = vec![CohortSeed::Resume(Box::new(agent))];
    restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            tier: Some((ModelTier::Junior, &tiers)),
            ..Default::default()
        },
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.launch.tier.as_ref().unwrap().tier, ModelTier::Junior);
    assert_eq!(
        cell.launch.record.as_ref().unwrap().model.as_deref(),
        Some("sonnet")
    );
    let err = restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            tier: Some((ModelTier::Principal, &tiers)),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(!err.contains("no longer resolves"), "{err}");
    assert!(
        err.contains("principal")
            && err.contains("claude")
            && err.contains("junior")
            && err.contains("senior"),
        "{err}"
    );
    let mut coder = super::agent("codex", "a2", "/repo", 1);
    coder.profile = Some("coder".into());
    seeds.insert(0, CohortSeed::Resume(Box::new(coder)));
    layout.columns[0].rows.insert(
        0,
        crate::harness::spec::Cell::Agent(
            crate::harness::spec::profile_cell("coder", &profiles).unwrap(),
        ),
    );
    let err = restore_routed_cells(
        &mut layout,
        &seeds,
        &profiles,
        &ResumeOverrides {
            tier: Some((ModelTier::Principal, &tiers)),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("principal") && err.contains("claude"), "{err}");
}

#[test]
fn unavailable_resume_tier_keeps_the_requested_row() {
    use crate::config::tiers::ModelTier;
    let profiles = profiles("planner", routed_profile(None));
    let tiers = crate::config::tiers::TierConfig::default();
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(agent))],
        &profiles,
        &ResumeOverrides {
            tier: Some((ModelTier::Junior, &tiers)),
            unavailable: Some(&|_, _| Some(crate::agents::TierSkipReason::LoggedOut)),
            ..Default::default()
        },
    )
    .expect("availability is best-effort, as in the fresh-launch tier walk");
    let cell = layout.agent_cells().next().unwrap();
    let stamp = cell.launch.tier.as_ref().unwrap();
    assert_eq!(stamp.tier, ModelTier::Junior);
    assert_eq!(stamp.model, "sonnet");
    assert!(stamp.skipped.is_empty());
}

#[test]
fn team_hold_table() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_str().unwrap();
    let present = team_agent("claude", "present", "lead", path, 10);
    let ended = AgentState {
        ended_at: Some(Timestamp::now()),
        ..present.clone()
    };
    let other = AgentState {
        team: Some("other".into()),
        ..team_agent("claude", "other", "lead", path, 0)
    };
    let ended_other = AgentState {
        ended_at: Some(Timestamp::now()),
        ..other.clone()
    };
    let live_hold = Some(TeamHold {
        team: "forge".into(),
        checkout: root.path().into(),
        reason: TeamHoldReason::LiveMember,
    });
    let board_hold = Some(TeamHold {
        team: "forge".into(),
        checkout: root.path().into(),
        reason: TeamHoldReason::BoardStage("Plan".into()),
    });
    let cases = [
        (vec![present.clone()], None, live_hold.clone()),
        (
            vec![ended.clone()],
            Some("Stage: Plan (@planner)"),
            board_hold,
        ),
        (vec![ended.clone()], Some("Stage: Done"), None),
        (vec![ended.clone()], None, None),
        (vec![ended.clone()], Some("No stage"), None),
        (vec![], Some("Stage: Plan"), None),
        (
            vec![team_agent("claude", "elsewhere", "lead", "/other", 0)],
            None,
            None,
        ),
        (
            vec![present.clone(), ended_other.clone()],
            Some("Stage: Plan"),
            live_hold.clone(),
        ),
        (
            vec![AgentState {
                worktree_path: Some(root.path().join("child/..").display().to_string()),
                ..present.clone()
            }],
            None,
            Some(TeamHold {
                team: "forge".into(),
                checkout: root.path().join("child/.."),
                reason: TeamHoldReason::LiveMember,
            }),
        ),
        (
            vec![present, other],
            None,
            Some(TeamHold {
                team: "other".into(),
                checkout: root.path().into(),
                reason: TeamHoldReason::LiveMember,
            }),
        ),
        (
            vec![ended, ended_other],
            Some("Stage: Plan"),
            Some(TeamHold {
                team: "other".into(),
                checkout: root.path().into(),
                reason: TeamHoldReason::BoardStage("Plan".into()),
            }),
        ),
    ];
    for (index, (agents, board, expected)) in cases.into_iter().enumerate() {
        let board_path = root.path().join("blackboard.md");
        if let Some(board) = board {
            std::fs::write(&board_path, board).unwrap();
        } else if board_path.exists() {
            std::fs::remove_file(&board_path).unwrap();
        }
        assert_eq!(
            inspect_team_hold(&agents, root.path()),
            expected,
            "case {index}"
        );
    }
}

#[test]
fn channel_hold_table() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("X");
    std::fs::create_dir(&checkout).unwrap();
    let present = AgentState {
        channel: Some("X".into()),
        ..team_agent("claude", "present", "lead", checkout.to_str().unwrap(), 10)
    };
    let ended = AgentState {
        ended_at: Some(Timestamp::now()),
        ..present.clone()
    };
    let live_hold = TeamHold {
        team: "forge".into(),
        checkout: checkout.clone(),
        reason: TeamHoldReason::LiveMember,
    };
    let other = AgentState {
        channel: Some("X".into()),
        team: Some("other".into()),
        ..team_agent("claude", "other", "lead", checkout.to_str().unwrap(), 0)
    };
    let ended_on_y = |secs_ago| AgentState {
        channel: Some("Y".into()),
        team: Some("other".into()),
        ended_at: Some(Timestamp::now()),
        ..team_agent(
            "claude",
            "on-y",
            "lead",
            checkout.to_str().unwrap(),
            secs_ago,
        )
    };
    let cases = [
        (vec![present.clone()], None, Some(live_hold.clone())),
        (
            vec![ended.clone()],
            Some("Stage: Build"),
            Some(TeamHold {
                reason: TeamHoldReason::BoardStage("Build".into()),
                ..live_hold.clone()
            }),
        ),
        (vec![ended.clone()], Some("Stage: Done"), None),
        (
            vec![ended.clone(), ended_on_y(0)],
            Some("Stage: Build"),
            None,
        ),
        (
            vec![ended.clone(), ended_on_y(20)],
            Some("Stage: Build"),
            Some(TeamHold {
                reason: TeamHoldReason::BoardStage("Build".into()),
                ..live_hold.clone()
            }),
        ),
        (vec![ended], None, None),
        (
            vec![AgentState {
                channel: None,
                ..present.clone()
            }],
            None,
            Some(live_hold.clone()),
        ),
        (
            vec![AgentState {
                channel: Some("Y".into()),
                ..present.clone()
            }],
            None,
            None,
        ),
        (
            vec![AgentState {
                worktree_path: None,
                ..present.clone()
            }],
            None,
            None,
        ),
        (
            vec![present, other],
            None,
            Some(TeamHold {
                team: "other".into(),
                ..live_hold
            }),
        ),
        (vec![], Some("Stage: Build"), None),
    ];
    for (index, (agents, board, expected)) in cases.into_iter().enumerate() {
        let board_path = checkout.join("blackboard.md");
        if let Some(board) = board {
            std::fs::write(&board_path, board).unwrap();
        } else if board_path.exists() {
            std::fs::remove_file(&board_path).unwrap();
        }
        assert_eq!(inspect_channel_hold(&agents, "X"), expected, "case {index}");
    }
}

#[test]
fn team_restore_routes_fresh_seats_before_planning() {
    let root = tempfile::tempdir().unwrap();
    let (teams, mut profiles, commands) = team_configs();
    let mut routed = routed_profile(None);
    routed.model_tier = Some(crate::config::tiers::TierProvenance {
        tier: crate::config::tiers::ModelTier::Senior,
        fell_back: false,
    });
    routed
        .definition_renders
        .as_mut()
        .unwrap()
        .renders
        .insert("claude".into(), profile("claude"));
    profiles.0.insert("claude-plan".into(), routed);
    let mut machine = crate::config::MachineConfig::default();
    machine.agents.teams = teams;
    machine.agents.profiles = profiles;
    machine.agents.commands = commands;
    let config = LaneRestoreConfig::load(&machine, root.path(), |kind, _| {
        (kind == "claude").then_some(crate::agents::TierSkipReason::LoggedOut)
    })
    .unwrap();
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 5);
    for fresh in [false, true] {
        let tabs = plan_team_restore_tabs(
            std::slice::from_ref(&coder),
            &NO_LOGINS,
            &NO_ACCOUNTS,
            &config.teams,
            &config.profiles,
            &config.commands,
            Some(Path::new("/repo")),
            &WORKSPACE,
            |_| true,
            |_| true,
            fresh,
            &[],
            dead,
        )
        .0;
        assert_eq!(tabs.len(), 1);
        let cell = tabs[0].layout.agent_cells().next().unwrap();
        assert_eq!(cell.kind.as_str(), "codex");
        assert_eq!(cell.launch.model.as_deref(), Some("astra"));
        assert_eq!(cell.launch.tier.as_ref().unwrap().skipped.len(), 1);
        assert!(matches!(tabs[0].cohort.seeds[0], CohortSeed::Fresh));
    }
}

#[test]
fn stamped_cohort_resume_reapplies_explicit_model_and_effort() {
    let profiles = profiles("planner", routed_profile(Some("medium")));
    let agent = AgentState {
        profile: Some("planner".into()),
        tier: Some(tier_stamp("astra")),
        ..agent("codex", "fallback", "/repo/forge", 1)
    };
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    let preset = crate::agents::LaunchPreset {
        model: Some("sol".into()),
        effort: Some("low".into()),
        ..Default::default()
    };
    restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(agent))],
        &profiles,
        &ResumeOverrides {
            preset,
            ..Default::default()
        },
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.launch.model.as_deref(), Some("sol"));
    assert_eq!(cell.launch.effort.as_deref(), Some("low"));
    assert!(cell.args.iter().any(|arg| arg == "sol"));
    assert!(cell.args.iter().any(|arg| arg.contains("low")));
}

#[test]
fn cohort_resume_ignores_launched_children() {
    let parent = AgentState {
        profile: Some("astra".to_owned()),
        launch_depth: Some(1),
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..agent("codex", "parent", "/code/feature", 30)
    };
    let child = AgentState {
        profile: Some("general".to_owned()),
        parent_agent_id: Some(parent.agent_id.clone()),
        launch_depth: Some(2),
        runtime_owner: Some(crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            "child".to_owned(),
        )),
        ..agent("codex", "child", "/code/feature", 1)
    };
    let agents = [parent, child];
    for child_live in [true, false] {
        for cell in [profile_cell("codex", "astra"), cohort_cell("codex", None)] {
            let plan = cohort_with(
                &agents,
                std::slice::from_ref(&cell),
                None,
                |row| {
                    if row.is_launched_child() && child_live {
                        live(row)
                    } else {
                        dead(row)
                    }
                },
                |_| true,
                |_| true,
            )
            .expect("child does not compete with root");
            assert_eq!(resume_id(&plan.seeds[0]), Some("parent"));
            assert_eq!(
                inspect_cohort_relaunch(&agents, Path::new("/code/feature"), &[cell], None,),
                CohortRelaunchState::Closed,
            );
        }
    }
    assert_eq!(closed_cohort_specs(&agents, dead), ["astra"]);
}

#[test]
fn single_cell_resume_matches_requested_profile() {
    let astra = AgentState {
        profile: Some("astra".to_owned()),
        ..agent("codex", "astra-session", "/code/feature", 30)
    };
    let debugger = AgentState {
        profile: Some("debugger".to_owned()),
        ..agent("codex", "debugger-session", "/code/feature", 10)
    };
    let agents = [astra, debugger, agent("codex", "bare", "/code/feature", 1)];
    for (cell, expected) in [
        (profile_cell("codex", "astra"), "astra-session"),
        (profile_cell("codex", "debugger"), "debugger-session"),
        (cohort_cell("codex", None), "bare"),
    ] {
        let plan = cohort(&agents, &[cell], None).expect("matching root");
        assert_eq!(resume_id(&plan.seeds[0]), Some(expected));
    }
    assert_eq!(
        cohort(&agents, &[profile_cell("codex", "nova")], None),
        Err(CohortResumeErr::NothingToResume {
            spec: "nova".to_owned(),
        }),
    );
    assert!(matches!(
        cohort_with(
            &agents,
            &[profile_cell("codex", "astra")],
            None,
            live,
            |_| true,
            |_| true,
        ),
        Err(CohortResumeErr::MembersStillLive { labels })
            if labels == [cohort_agent_label(&agents[0])],
    ));
    let plan = cohort(&agents[..2], &[cohort_cell("codex", None)], None)
        .expect("bare kind also matches profiled roots");
    assert_eq!(resume_id(&plan.seeds[0]), Some("debugger-session"));
}

#[test]
fn single_cell_resume_matches_a_stamped_cross_family_profile() {
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        tier: Some(tier_stamp("astra")),
        ..agent("codex", "fallback", "/code/feature", 1)
    };
    let plan = cohort(&[agent], &[profile_cell("claude", "planner")], None)
        .expect("the stamped profile matches across families");
    assert_eq!(resume_id(&plan.seeds[0]), Some("fallback"));
}

#[test]
fn roleless_team_resume_matches_a_stamped_cross_family_profile() {
    let planner = AgentState {
        profile: Some("planner".to_owned()),
        tier: Some(tier_stamp("astra")),
        team: Some("forge".to_owned()),
        ..agent("codex", "fallback", "/code/feature", 1)
    };
    for tier in [Some(tier_stamp("opus")), None] {
        let worker = AgentState {
            profile: Some("worker".to_owned()),
            tier,
            team: Some("forge".to_owned()),
            ..agent("claude", "worker", "/code/feature", 0)
        };
        let plan = cohort(
            &[planner.clone(), worker],
            &[
                profile_cell("claude", "planner"),
                profile_cell("claude", "worker"),
            ],
            Some("forge"),
        )
        .expect("a roleless team member still matches its stamped profile");
        assert_eq!(resume_id(&plan.seeds[0]), Some("fallback"));
        assert_eq!(resume_id(&plan.seeds[1]), Some("worker"));
    }
}

#[test]
fn team_restore_rebuilds_a_fallen_back_seat_on_its_stamped_render() {
    let (teams, mut profiles, commands) = team_configs();
    profiles
        .0
        .insert("claude-plan".to_owned(), routed_profile(Some("medium")));
    let stamp = tier_stamp("astra");
    let planner = AgentState {
        profile: Some("claude-plan".to_owned()),
        tier: Some(stamp.clone()),
        ..team_agent("codex", "planner", "planner", "/repo/forge", 3)
    };
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 5);
    let tabs = plan_team_restore_tabs(
        &[planner, coder],
        &NO_LOGINS,
        &NO_ACCOUNTS,
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        &WORKSPACE,
        |_| true,
        |_| true,
        false,
        &[],
        dead,
    )
    .0;
    assert_eq!(tabs.len(), 1);
    let tab = &tabs[0];
    let panes = compile_layout_panes(
        &tab.layout,
        LayoutPaneParams {
            runtime: &RUNTIME,
            cwd: &tab.cwd,
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&tab.cohort.seeds),
            launch_identities: &[],
            fallback_channel: None,
            loop_reminder: None,
        },
    )
    .unwrap();
    let request = decode_exec_request(&panes.columns[0].panes[0].argv);
    assert_eq!(request.kind.as_str(), "codex");
    assert_eq!(
        request.identity.params.model.as_deref(),
        Some(stamp.model.as_str())
    );
    assert_eq!(request.identity.params.tier, Some(stamp));
    assert!(
        matches!(request.action, crate::harness::launch::ExecAction::Resume { extra_args, .. } if extra_args.iter().any(|arg| arg == "--search"))
    );
}

#[test]
fn dead_placeholder_does_not_shadow_the_conversation() {
    let session = AgentState {
        profile: Some("astra".to_owned()),
        ..agent("codex", "session", "/code/feature", 30)
    };
    let placeholder = AgentState {
        profile: Some("astra".to_owned()),
        ..agent(
            "codex",
            "launch_019f2cecea067320b667c5946d266e64",
            "/code/feature",
            1,
        )
    };
    let agents = [session, placeholder];
    let cells = [profile_cell("codex", "astra")];
    for liveness in [AgentLiveness::Dead, AgentLiveness::Unknown] {
        let plan = cohort_with(&agents, &cells, None, |_| liveness, |_| true, |_| true)
            .expect("placeholder has no conversation to shadow the root");
        assert_eq!(resume_id(&plan.seeds[0]), Some("session"));
        assert_eq!(
            cohort_with(&agents[1..], &cells, None, |_| liveness, |_| true, |_| true),
            Err(CohortResumeErr::NothingToResume {
                spec: "astra".to_owned(),
            }),
        );
    }
    assert_eq!(
        cohort_with(
            &agents,
            &cells,
            None,
            |row| if row.agent_id.is_provisional() {
                live(row)
            } else {
                dead(row)
            },
            |_| true,
            |_| true,
        ),
        Err(CohortResumeErr::MembersStillLive {
            labels: vec![cohort_agent_label(&agents[1])],
        }),
    );
}

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
    assert_eq!(
        relaunch_spec(Some(""), Some(""), Some(""), "codex"),
        "codex"
    );
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
            "a session the provider never persisted",
            vec![AgentState {
                root_lane: true,
                ..agent("claude", "a1", "/code/query-engine", 1)
            }],
            vec![cohort_cell("claude", None)],
            None,
            false,
            "claude:main",
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
fn cohort_matching_uses_one_occupant_per_launch_instance() {
    let owner = |session: &str| {
        crate::store::runtime::current_process_owner(crate::pane::RuntimeOwnerKind::Agent, session)
    };
    let member = |id: &str, secs_ago: i64| AgentState {
        launch_id: Some(AgentSessionId::from("launch_coder")),
        runtime_owner: Some(owner(id)),
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        ..agent_on_pane("codex", id, "/code/feature", secs_ago, "terminal_coder")
    };
    let old = member("old", 10);
    let current = member("current", 2);
    let ended_old = AgentState {
        ended_at: Some(old.last_seen),
        ..old
    };
    let cell = cohort_cell("codex", Some("coder"));

    let live_pool = [ended_old.clone(), current.clone()];
    let matched = match_cohort(
        &live_pool.iter().collect::<Vec<_>>(),
        std::slice::from_ref(&cell),
        Some("forge"),
    );
    assert_eq!(
        matched[0].map(|agent| agent.agent_id.as_str()),
        Some("current")
    );
    assert_eq!(
        inspect_cohort_relaunch(
            &live_pool,
            Path::new("/code/feature"),
            std::slice::from_ref(&cell),
            Some("forge")
        ),
        CohortRelaunchState::Present {
            focus_pane: Some(pane_id("terminal_coder")),
        }
    );

    let ended_current = AgentState {
        ended_at: Some(current.last_seen),
        ..current
    };
    let closed_pool = [ended_old, ended_current];
    let matched = match_cohort(
        &closed_pool.iter().collect::<Vec<_>>(),
        std::slice::from_ref(&cell),
        Some("forge"),
    );
    assert_eq!(
        matched[0].map(|agent| agent.agent_id.as_str()),
        Some("current")
    );
    assert_eq!(
        inspect_cohort_relaunch(
            &closed_pool,
            Path::new("/code/feature"),
            &[cell],
            Some("forge")
        ),
        CohortRelaunchState::Closed
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

    let placeholder = paneless(team_agent(
        "codex",
        "launch_019f2cecea067320b667c5946d266e64",
        "coder",
        "/code/feature",
        1,
    ));
    let live_placeholder = AgentState {
        runtime_owner: Some(crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            placeholder.agent_id.to_string(),
        )),
        ..placeholder.clone()
    };

    for (label, agents, cells, team, expected) in [
        (
            "absent",
            Vec::new(),
            &one_cell,
            None,
            CohortRelaunchState::Absent,
        ),
        (
            "ended",
            vec![closed(agent("codex", "ended", "/code/feature", 4))],
            &one_cell,
            None,
            CohortRelaunchState::Closed,
        ),
        (
            "unknown with pane",
            vec![with_pane],
            &one_cell,
            None,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_pane")),
            },
        ),
        (
            "unknown without pane",
            vec![paneless(agent("codex", "paneless", "/code/feature", 2))],
            &one_cell,
            None,
            CohortRelaunchState::Closed,
        ),
        (
            "live without pane",
            vec![live_without_pane],
            &one_cell,
            None,
            CohortRelaunchState::Present { focus_pane: None },
        ),
        (
            "newest inline group decides, and it is closed",
            closed_newest_group,
            &two_cells,
            None,
            CohortRelaunchState::Closed,
        ),
        (
            "focus lands on the freshest present pane",
            present_group,
            &two_cells,
            None,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_fresher")),
            },
        ),
        (
            "an unadopted team has no cohort to resume",
            vec![placeholder.clone()],
            &one_cell,
            Some("forge"),
            CohortRelaunchState::Absent,
        ),
        (
            "a live placeholder still protects the starting team",
            vec![live_placeholder],
            &one_cell,
            Some("forge"),
            CohortRelaunchState::Present { focus_pane: None },
        ),
        (
            "a partially adopted team still offers resume",
            vec![
                placeholder,
                closed(team_agent(
                    "claude",
                    "planner",
                    "planner",
                    "/code/feature",
                    3,
                )),
            ],
            &two_cells,
            Some("forge"),
            CohortRelaunchState::Closed,
        ),
    ] {
        assert_eq!(
            inspect_cohort_relaunch(&agents, Path::new("/code/feature"), cells, team),
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
            "a team with only an unadopted placeholder has nothing to restore",
            vec![team_agent(
                "codex",
                "launch_019f2cecea067320b667c5946d266e64",
                "coder",
                "/repo/forge",
                1,
            )],
            teams.clone(),
            None,
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
            &NO_LOGINS,
            &NO_ACCOUNTS,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            &WORKSPACE,
            |_| true,
            |_| true,
            false,
            &[],
            dead,
        )
        .0;

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

    let (tabs, flat_agents, _) = split_team_and_flat(
        &[planner, flat],
        &NO_LOGINS,
        &NO_ACCOUNTS,
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        &WORKSPACE,
        |_| true,
        |_| true,
        false,
        &[],
        dead,
    );

    assert_eq!(tabs.len(), 1);
    assert_eq!(flat_agents.len(), 1);
    assert_eq!(flat_agents[0].agent_id.as_str(), "flat");
}

#[test]
fn cohort_resume_refuses_a_member_from_another_account() {
    let planner = AgentState {
        login: Some("personal".parse().expect("login name")),
        ..team_agent("claude", "planner", "planner", "/code/forge", 1)
    };
    let room = claude_room("work");

    let plan = plan_cohort_resume(
        std::slice::from_ref(&planner),
        &room,
        &claude_accounts("shared"),
        dead,
        &[cohort_cell("claude", Some("planner"))],
        Some("forge"),
        |_| true,
        |_| true,
    )
    .expect("a pooled member resumes");
    let [CohortSeed::Resume(seed)] = plan.seeds.as_slice() else {
        panic!("expected one resume seed, got {:?}", plan.seeds);
    };
    assert_eq!(seed.login, Some("work".parse().expect("login name")));

    let err = plan_cohort_resume(
        &[planner],
        &room,
        &claude_accounts("standalone"),
        dead,
        &[cohort_cell("claude", Some("planner"))],
        Some("forge"),
        |_| true,
        |_| true,
    )
    .unwrap_err();

    assert!(
        matches!(&err, CohortResumeErr::LoginMismatch(RelaunchLoginErr::Mismatch(mismatch)) if mismatch.session_login.as_str() == "personal"),
        "{err:?}"
    );
}

#[test]
fn explicit_resume_mode_beats_recorded_and_profile_modes() {
    let mut planner = profile("claude");
    planner.mode = Some(PermissionMode::Auto);
    planner.args = Some("--verbose".into());
    let profiles = profiles("planner", planner);
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    agent.mode = Some(PermissionMode::Yolo);
    agent.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("opus".into()),
        effort: Some("high".into()),
        agent: None,
    }));
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
    ));
    restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(agent))],
        &profiles,
        &ResumeOverrides {
            permission_mode: Some(PermissionMode::Plan),
            preset: crate::agents::LaunchPreset {
                model: Some("sonnet".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.launch.mode, Some(PermissionMode::Plan));
    assert_eq!(
        cell.args,
        [
            "--verbose",
            "--effort",
            "high",
            "--permission-mode",
            "plan",
            "--model",
            "sonnet"
        ]
    );
}

#[test]
fn agent_override_mode_precedence_and_refusals() {
    let mut planner = profile("claude");
    planner.mode = Some(PermissionMode::Plan);
    let mut profiles = profiles("planner", planner);
    let mut reviewer = profile("claude");
    reviewer.mode = Some(PermissionMode::Auto);
    profiles.0.insert("reviewer".into(), reviewer);
    profiles.0.insert("helper".into(), profile("claude"));
    let restore = |seed: CohortSeed, base: &str| {
        let mut cell = crate::harness::spec::profile_cell("planner", &profiles).unwrap();
        cell.launch.mode = Some(PermissionMode::Yolo);
        let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(cell));
        let before = layout.clone();
        let result = restore_routed_cells(
            &mut layout,
            &[seed],
            &profiles,
            &ResumeOverrides {
                agent: Some(base),
                ..Default::default()
            },
        );
        (before, layout, result)
    };
    let resumed = |mode: Option<PermissionMode>, profile: &str| {
        let mut agent = agent("claude", "a1", "/repo", 1);
        agent.profile = Some(profile.into());
        agent.mode = mode;
        CohortSeed::Resume(Box::new(agent))
    };
    for (label, seed, base, mode, args) in [
        (
            "a base-declared mode beats the saved mode",
            resumed(Some(PermissionMode::Yolo), "planner"),
            "reviewer",
            PermissionMode::Auto,
            vec!["--permission-mode", "auto"],
        ),
        (
            "a fresh seat replays its layout cell's mode",
            CohortSeed::Fresh,
            "helper",
            PermissionMode::Yolo,
            vec!["--dangerously-skip-permissions"],
        ),
        (
            "an unstamped seat falls back to the rebased profile's mode",
            resumed(None, "planner"),
            "helper",
            PermissionMode::Plan,
            vec!["--permission-mode", "plan"],
        ),
        (
            "a blank base keeps the seat's own profile",
            resumed(Some(PermissionMode::Yolo), "planner"),
            "",
            PermissionMode::Yolo,
            vec!["--dangerously-skip-permissions"],
        ),
    ] {
        let (_, layout, result) = restore(seed, base);
        result.unwrap();
        let cell = layout.agent_cells().next().unwrap();
        assert_eq!(cell.launch.mode, Some(mode), "{label}");
        assert_eq!(cell.args, args, "{label}");
        assert_eq!(
            cell.launch.record.as_ref().unwrap().agent.as_deref(),
            Some(base),
            "{label}"
        );
        assert!(cell.resume_model_override, "{label}");
    }
    let unknown = crate::harness::spec::resolve_agent_override(Some("ghost"), &profiles)
        .unwrap_err()
        .to_string();
    let retired = crate::harness::spec::profile_cell("retired", &profiles)
        .unwrap_err()
        .to_string();
    for (seed, base, reason) in [
        (resumed(None, "planner"), "ghost", unknown),
        (resumed(None, "retired"), "reviewer", retired),
    ] {
        let (before, after, result) = restore(seed, base);
        assert_eq!(result, Err(PostureDegrade::OverrideRejected { reason }));
        assert_eq!(after, before);
    }
}

#[test]
fn tier_stamped_legacy_seat_keeps_the_layout_mode() {
    let mut planner = routed_profile(None);
    planner
        .definition_renders
        .as_mut()
        .unwrap()
        .renders
        .insert("claude".into(), profile("claude"));
    let profiles = profiles("planner", planner);
    for (label, tier, record, mode, args) in [
        (
            "a stamped seat without a record keeps the layout mode",
            Some(tier_stamp("opus")),
            None,
            PermissionMode::Auto,
            vec!["--permission-mode", "auto"],
        ),
        (
            "a recorded seat replays its saved mode",
            Some(tier_stamp("opus")),
            Some(crate::agents::LaunchRecord::default()),
            PermissionMode::Yolo,
            vec!["--dangerously-skip-permissions"],
        ),
        (
            "an unstamped seat keeps the layout mode",
            None,
            None,
            PermissionMode::Auto,
            vec!["--permission-mode", "auto"],
        ),
    ] {
        let mut cell = crate::harness::spec::profile_cell("planner", &profiles).unwrap();
        cell.launch.mode = Some(PermissionMode::Auto);
        let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(cell));
        let mut agent = agent("claude", "a1", "/repo", 1);
        agent.profile = Some("planner".into());
        agent.mode = Some(PermissionMode::Yolo);
        agent.tier = tier;
        agent.record = record.map(Box::new);
        restore_routed_cells(
            &mut layout,
            &[CohortSeed::Resume(Box::new(agent))],
            &profiles,
            &Default::default(),
        )
        .unwrap();
        let cell = layout.agent_cells().next().unwrap();
        assert_eq!(cell.launch.mode, Some(mode), "{label}");
        let permission = [
            "--permission-mode",
            "auto",
            "plan",
            "--dangerously-skip-permissions",
        ];
        assert_eq!(
            cell.args
                .iter()
                .filter(|arg| permission.contains(&arg.as_str()))
                .collect::<Vec<_>>(),
            args,
            "{label}"
        );
    }
}

#[test]
fn degraded_stamped_seat_refuses_its_cohort_restore() {
    let mut profiles = profiles("planner", profile("codex"));
    profiles.0.insert("helper".into(), profile("claude"));
    for (label, profile, tier, record) in [
        (
            "a tier-stamped seat whose family render is gone",
            "planner",
            Some(tier_stamp("astra")),
            None,
        ),
        (
            "a recorded seat whose profile is gone",
            "retired",
            None,
            Some(Box::new(crate::agents::LaunchRecord::default())),
        ),
    ] {
        let mut seat = agent("codex", "a1", "/repo", 1);
        seat.profile = Some(profile.into());
        seat.mode = Some(PermissionMode::Yolo);
        seat.tier = tier;
        seat.record = record;
        let reason = resolve_posture(
            PostureRequest {
                record: seat.record.as_deref(),
                profile: seat.profile.as_deref(),
                kind: &seat.kind,
                stamped_mode: seat.mode,
                stamped_tier: seat.tier.as_deref(),
            },
            &profiles,
        )
        .degraded
        .expect(label);
        let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
            crate::harness::spec::profile_cell("helper", &profiles).unwrap(),
        ));
        layout.columns[0]
            .rows
            .push(crate::harness::spec::Cell::Agent(
                crate::harness::spec::profile_cell("planner", &profiles).unwrap(),
            ));
        let failing = layout.agent_cells().nth(1).unwrap().clone();
        let result = restore_routed_cells(
            &mut layout,
            &[CohortSeed::Fresh, CohortSeed::Resume(Box::new(seat))],
            &profiles,
            &Default::default(),
        );
        assert_eq!(result, Err(reason), "{label}");
        let mut cells = layout.agent_cells();
        assert!(cells.next().unwrap().launch.record.is_some(), "{label}");
        assert_eq!(cells.next().unwrap(), &failing, "{label}");
    }

    let (teams, profiles, commands) = team_configs();
    let planner = AgentState {
        profile: Some("claude-plan".to_owned()),
        tier: Some(tier_stamp("opus")),
        ..team_agent("claude", "planner", "planner", "/repo/forge", 3)
    };
    let (tabs, flat, _) = split_team_and_flat(
        std::slice::from_ref(&planner),
        &NO_LOGINS,
        &NO_ACCOUNTS,
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        &WORKSPACE,
        |_| true,
        |_| true,
        false,
        &[],
        dead,
    );
    assert!(tabs.is_empty());
    assert_eq!(flat, [planner]);
}

#[test]
fn unrenderable_record_effort_refuses_its_cohort_restore() {
    let profiles = profiles("worker", profile("kimi"));
    let mut seat = agent("kimi", "a1", "/repo", 1);
    seat.profile = Some("worker".into());
    seat.record = Some(Box::new(crate::agents::LaunchRecord {
        effort: Some("high".into()),
        ..Default::default()
    }));
    let reason = resolve_posture(
        PostureRequest {
            record: seat.record.as_deref(),
            profile: seat.profile.as_deref(),
            kind: &seat.kind,
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles,
    )
    .degraded
    .unwrap();
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(
        crate::harness::spec::profile_cell("worker", &profiles).unwrap(),
    ));
    let result = restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(seat))],
        &profiles,
        &Default::default(),
    );
    assert_eq!(result, Err(reason));
}

#[test]
fn resumed_seats_clear_auto_compact_and_fresh_seats_keep_it() {
    let profiles = profiles(
        "planner",
        Profile {
            auto_compact: Some("200k".into()),
            ..profile("claude")
        },
    );
    let cell = crate::harness::spec::profile_cell("planner", &profiles).unwrap();
    let declared = cell.auto_compact.clone();
    assert!(declared.is_some());
    let mut layout = LayoutSpec::single(crate::harness::spec::Cell::Agent(cell.clone()));
    layout.columns[0]
        .rows
        .push(crate::harness::spec::Cell::Agent(cell));
    let mut agent = agent("claude", "a1", "/repo", 1);
    agent.profile = Some("planner".into());
    restore_routed_cells(
        &mut layout,
        &[CohortSeed::Resume(Box::new(agent)), CohortSeed::Fresh],
        &profiles,
        &Default::default(),
    )
    .unwrap();
    let mut cells = layout.agent_cells();
    assert_eq!(cells.next().unwrap().auto_compact, None);
    assert_eq!(cells.next().unwrap().auto_compact, declared);
}
