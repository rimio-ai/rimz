use super::*;
use crate::agents::{LaunchPreset, LaunchRecord};
use crate::config::Isolation;
use crate::harness::launch::ExecRequest;

fn saved() -> AgentState {
    AgentState {
        name: Some("proper-notch".into()),
        profile: Some("planner".into()),
        mode: Some(PermissionMode::Ask),
        record: Some(Box::new(LaunchRecord {
            model: Some("opus".into()),
            effort: Some("high".into()),
            fast: None,
            agent: None,
            auto_compact: None,
            ..Default::default()
        })),
        ..agent("claude", "saved", "/repo", 0)
    }
}

fn single(
    saved: &AgentState,
    profiles: &ProfilesConfig,
    options: SingleResumeOptions<'_>,
) -> (ResumePlan, Option<ResumeLaunchFacts>) {
    plan_recorded_resume(saved, ctx(1, None, profiles), &options, |_| true, |_| true)
}

#[test]
fn recorded_resume_keeps_the_original_loop_section_through_overrides() {
    let mut saved = saved();
    let mut record = serde_json::to_value(&saved.record).unwrap();
    record["loop_section"] = "The original loop fire".into();
    saved.record = serde_json::from_value(record).unwrap();
    let mut actual = Vec::new();
    for model in [None, Some("sonnet")] {
        let (plan, _) = single(
            &saved,
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    preset: LaunchPreset {
                        model: model.map(str::to_owned),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        actual.push(decode_exec_request(&single_pane_argv(&plan)).reminder_section);
    }
    assert_eq!(
        actual,
        vec![
            Some(crate::harness::launch::ReminderSection::Loop(
                "The original loop fire".into()
            ));
            2
        ]
    );
}

fn request(overrides: ResumeOverrides<'_>) -> ExecRequest {
    let mut saved = saved();
    saved.tier = Some(tier_stamp("opus"));
    let planner = routed_profile();
    let (plan, _) = single(
        &saved,
        &profiles("planner", planner),
        SingleResumeOptions {
            overrides,
            ..Default::default()
        },
    );
    decode_exec_request(&single_pane_argv(&plan))
}

#[test]
fn recorded_model_override_rebuilds_record_and_clears_tier() {
    let request = request(ResumeOverrides {
        preset: LaunchPreset {
            model: Some("sonnet".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(
        request.identity.params.record.unwrap().model.as_deref(),
        Some("sonnet")
    );
    assert!(request.identity.resume_model_override);
    assert!(request.identity.params.tier.is_none());
}

/// `--model` alone goes through the resume engine, as `--tier` does: a tier name or a model the
/// session's kind cannot run refuses before any pane opens.
#[test]
fn recorded_model_override_refuses_a_tier_name_and_another_provider_model() {
    for (model, expected) in [
        ("senior", "use `--tier senior`"),
        (
            "astra",
            "this session is claude, but --model astra runs on codex; resume with a claude model \
             or launch fresh",
        ),
    ] {
        let (plan, facts) = single(
            &saved(),
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    preset: LaunchPreset {
                        model: Some(model.into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(plan.tabs.is_empty(), "{model}");
        assert!(
            plan.skipped[0].reason.label().contains(expected),
            "{:?}",
            plan.skipped
        );
        assert!(facts.is_none());
    }
}

/// `--model` on a resume rebuilds under the `--agent` the session recorded, so the effort is the
/// one the build under that agent and model gives, not the record's.
#[test]
fn recorded_model_override_takes_the_effort_of_the_build_under_the_recorded_agent() {
    for (agent, expected) in [(None, "medium"), (Some("low"), "low")] {
        let mut profiles = project_profiles(
            "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
             [profiles.low]\ndescription = 'Low'\nagent = 'claude'\neffort = 'low'\n",
        );
        let mut base = profile("claude");
        base.kind_base = true;
        base.effort = Some("medium".into());
        profiles.0.insert("claude".into(), base);
        let mut saved = saved();
        saved.record.as_mut().unwrap().agent = agent.map(str::to_owned);
        let (plan, _) = single(
            &saved,
            &profiles,
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    preset: LaunchPreset {
                        model: Some("sonnet".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.identity.params.model.as_deref(), Some("sonnet"));
        assert_eq!(
            request.identity.params.effort.as_deref(),
            Some(expected),
            "{agent:?}"
        );
        let record = request.identity.params.record.unwrap();
        assert_eq!(record.agent.as_deref(), agent);
    }
}

#[test]
fn recorded_effort_override_rebuilds_record_without_overriding_model() {
    let request = request(ResumeOverrides {
        preset: LaunchPreset {
            effort: Some("low".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    let record = request.identity.params.record.unwrap();
    assert_eq!(record.effort.as_deref(), Some("low"));
    assert_eq!(record.model.as_deref(), Some("opus"));
    assert!(!request.identity.resume_model_override);
}

#[test]
fn recorded_fast_override_saves_both_values() {
    for fast in [true, false] {
        let mut saved = saved();
        saved.record.as_mut().unwrap().fast = Some(!fast);
        let (plan, _) = single(
            &saved,
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    preset: LaunchPreset {
                        fast: Some(fast),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.identity.params.record.unwrap().fast, Some(fast));
        assert!(!request.identity.resume_model_override);
    }
}

#[test]
fn unsupported_fast_warns_and_resumes_without_stamping_it() {
    // Resume requires a registered adapter; Copilot has no fast alternative.
    let mut saved = agent("copilot", "saved", "/repo", 0);
    saved.record = Some(Box::new(LaunchRecord::default()));
    let (plan, _) = single(
        &saved,
        &ProfilesConfig::default(),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                preset: LaunchPreset {
                    fast: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
    assert!(plan.warnings[0].contains("fast"));
    assert!(plan.warnings[0].contains("copilot"));
    assert!(plan.warnings[0].contains("launching without"));
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.record.unwrap().fast, None);
}

#[test]
fn recorded_permission_override_replaces_saved_mode_and_provider_args() {
    for (previous, mode) in [
        (PermissionMode::Ask, PermissionMode::Yolo),
        (PermissionMode::Yolo, PermissionMode::Ask),
    ] {
        let mut saved = saved();
        saved.mode = Some(previous);
        let (plan, _) = single(
            &saved,
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    permission_mode: Some(mode),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.identity.params.mode, Some(mode));
        let adapter = find_definition("claude").unwrap();
        let expected = adapter.spec().launch.permission_args(mode);
        let args = request.action.extra_args();
        assert!(args.windows(expected.len()).any(|args| args == expected));
        let previous = adapter.spec().launch.permission_args(previous);
        assert!(!args.windows(previous.len()).any(|args| args == previous));
    }
}

#[test]
fn recorded_agent_override_rebases_model_and_records_base() {
    let profiles = project_profiles(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
         [profiles.reviewer]\ndescription = 'Reviewer'\nagent = 'claude'\nmodel = 'sonnet'\n",
    );
    let (plan, _) = single(
        &saved(),
        &profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("reviewer"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = decode_exec_request(&single_pane_argv(&plan));
    let record = request.identity.params.record.unwrap();
    assert_eq!(record.agent.as_deref(), Some("reviewer"));
    assert_eq!(record.model.as_deref(), Some("sonnet"));
    assert!(request.identity.resume_model_override);
}

#[test]
fn resume_tier_on_model_clients_uses_the_exact_row_on_the_engine() {
    let tiers = crate::config::tiers::TierConfig::default();
    let actual: Vec<_> = ["pi", "opencode"]
        .map(|kind| {
            let planner = profile(kind);
            let saved = AgentState {
                profile: Some("planner".into()),
                ..agent(kind, "saved", "/repo", 0)
            };
            let (plan, _) = single(
                &saved,
                &profiles("planner", planner),
                SingleResumeOptions {
                    overrides: ResumeOverrides {
                        selection: crate::config::Selection::new(&tiers)
                            .tier(Some(crate::config::tiers::ModelTier::Junior))
                            .availability(move |family, _| {
                                assert_eq!(family, kind);
                                None
                            }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            if !plan.skipped.is_empty() {
                return Err(format!("{:?}", plan.skipped));
            }
            let request = decode_exec_request(&single_pane_argv(&plan));
            Ok((request.kind.to_string(), request.identity.params.model))
        })
        .into();
    assert_eq!(
        actual,
        [
            Ok(("pi".into(), Some("sol".into()))),
            Ok(("opencode".into(), Some("sol".into())))
        ],
    );
}

#[test]
fn recorded_tier_override_selects_same_provider_and_records_model() {
    let tiers = crate::config::tiers::TierConfig::default();
    let request = request(ResumeOverrides {
        selection: crate::config::Selection::new(&tiers)
            .tier(Some(crate::config::tiers::ModelTier::Junior)),
        ..Default::default()
    });
    let tier = request.identity.params.tier.unwrap();
    assert_eq!(tier.tier, crate::config::tiers::ModelTier::Junior);
    assert_eq!(tier.model, "sonnet");
    assert_eq!(
        request.identity.params.record.unwrap().model.as_deref(),
        Some("sonnet")
    );
    assert!(request.identity.resume_model_override);
}

#[test]
fn recorded_tier_override_takes_the_kind_base_effort_a_fresh_tier_launch_takes() {
    let tiers = crate::config::tiers::TierConfig::default();
    let mut saved = saved();
    saved.tier = Some(tier_stamp("opus"));
    let planner = routed_profile();
    let mut profiles = profiles("planner", planner);
    let mut base = profile("claude");
    base.kind_base = true;
    base.effort = Some("low".into());
    profiles.0.insert("claude".into(), base);
    let (plan, _) = single(
        &saved,
        &profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                selection: crate::config::Selection::new(&tiers)
                    .tier(Some(crate::config::tiers::ModelTier::Junior)),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.effort.as_deref(), Some("low"));
}

#[test]
fn recorded_tier_override_keeps_machine_kind_base_effort_despite_kind_named_project_row() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, text) in [
        (
            "claude",
            "---\ndescription: Base\neffort: high\n---\nHouse.\n",
        ),
        (
            "planner",
            "---\ndescription: Planner\nagent: claude\ntier: senior\ntools: [Bash]\n---\nPlan.\n",
        ),
    ] {
        std::fs::write(root.path().join(format!("agents/{name}.md")), text).unwrap();
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.claude]\ndescription = \"Shadow\"\nagent = \"claude\"\neffort = \"low\"\n\
         system-prompt-file = \"base.md\"\n",
    )
    .unwrap();
    std::fs::write(project.path().join("base.md"), "House.\n").unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), root.path()).unwrap();
    assert!(launch.definition_failure_for("claude").is_some());
    let fresh = crate::harness::spec::resolve_profile("planner", &launch.profiles).unwrap();
    assert_eq!(fresh.launch.effort.as_deref(), Some("high"));

    let mut saved = saved();
    saved.tier = Some(tier_stamp("opus"));
    let tiers = crate::config::tiers::TierConfig::default();
    let (plan, _) = single(
        &saved,
        &launch.profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                selection: crate::config::Selection::new(&tiers)
                    .tier(Some(crate::config::tiers::ModelTier::Junior)),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
}

/// `--tier` on a resume rebuilds under the `--agent` the session recorded, so the tier changes
/// only the model, tier and effort, and the effort is the one the build under that agent gives.
#[test]
fn recorded_tier_override_rebuilds_under_the_recorded_agent() {
    let profiles = project_profiles(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
         [profiles.low]\ndescription = 'Low'\nagent = 'claude'\neffort = 'low'\n",
    );
    let mut saved = saved();
    saved.record.as_mut().unwrap().agent = Some("low".into());
    let tiers = crate::config::tiers::TierConfig::default();
    let (plan, _) = single(
        &saved,
        &profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                selection: crate::config::Selection::new(&tiers)
                    .tier(Some(crate::config::tiers::ModelTier::Junior)),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.model.as_deref(), Some("sonnet"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("low"));
}

#[test]
fn tier_override_on_a_bare_provider_session_takes_its_own_row_and_never_climbs() {
    let saved = AgentState {
        profile: None,
        record: None,
        ..saved()
    };
    let resume = |tiers: &crate::config::tiers::TierConfig, tier| {
        single(
            &saved,
            &ProfilesConfig::default(),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    selection: crate::config::Selection::new(tiers).tier(Some(tier)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .0
    };
    let plan = resume(
        &crate::config::tiers::TierConfig::default(),
        crate::config::tiers::ModelTier::Junior,
    );
    assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.kind.as_str(), "claude");
    assert_eq!(request.identity.params.model.as_deref(), Some("sonnet"));
    let tier = request.identity.params.tier.unwrap();
    assert_eq!(tier.tier, crate::config::tiers::ModelTier::Junior);
    assert_eq!(tier.model, "sonnet");
    let plan = resume(
        &toml::from_str("principal = ['gpt-6-sol']").unwrap(),
        crate::config::tiers::ModelTier::Principal,
    );
    assert!(plan.tabs.is_empty());
    assert!(
        plan.skipped[0]
            .reason
            .label()
            .contains("tier principal has no entry for claude"),
        "{:?}",
        plan.skipped
    );
}

#[test]
fn isolation_only_changes_the_isolation_even_without_a_record() {
    for recorded in [true, false] {
        let mut saved = saved();
        if !recorded {
            saved.record = None;
            saved.tier = None;
        }
        let profiles = profiles("planner", profile("claude"));
        let original = plan_resume(
            std::slice::from_ref(&saved),
            &BTreeSet::new(),
            ctx(1, None, &profiles),
            |_| true,
            |_| true,
        );
        let (plan, _) = single(
            &saved,
            &profiles,
            SingleResumeOptions {
                isolation: Some(Isolation::Sandbox),
                ..Default::default()
            },
        );
        let mut expected = decode_exec_request(&single_pane_argv(&original));
        expected.identity.params.isolation = Some(Isolation::Sandbox);
        assert_eq!(decode_exec_request(&single_pane_argv(&plan)), expected);
    }
}

#[test]
fn omitted_overrides_keep_the_existing_flat_commands() {
    let saved = saved();
    let profiles = profiles("planner", profile("claude"));
    let original = plan_resume(
        std::slice::from_ref(&saved),
        &BTreeSet::new(),
        ctx(1, None, &profiles),
        |_| true,
        |_| true,
    );
    let (plan, _) = single(&saved, &profiles, SingleResumeOptions::default());
    assert_eq!(single_pane_argv(&plan), single_pane_argv(&original));
    assert_eq!(plan.tabs, original.tabs);
}

#[test]
fn incompatible_agent_override_refuses_without_warning_or_launch_facts() {
    let mut profiles = profiles("planner", profile("claude"));
    profiles.0.insert("coder".into(), profile("codex"));
    let (plan, facts) = single(
        &saved(),
        &profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("coder"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(plan.tabs.is_empty());
    assert!(plan.skipped[0].reason.label().contains(concat!(
        "this session is claude, but --agent coder is codex; resume with a ",
        "same-kind profile or launch fresh"
    )));
    assert!(plan.warnings.is_empty());
    assert!(facts.is_none());
}

#[test]
fn unavailable_tier_refuses_without_warning_or_launch_facts() {
    let tiers = toml::from_str("principal = ['gpt-6-sol']").unwrap();
    let (plan, facts) = single(
        &saved(),
        &profiles("planner", routed_profile()),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                selection: crate::config::Selection::new(&tiers)
                    .tier(Some(crate::config::tiers::ModelTier::Principal)),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(plan.tabs.is_empty());
    assert!(
        plan.skipped[0]
            .reason
            .label()
            .contains("tier principal has no entry for claude")
    );
    assert!(plan.warnings.is_empty());
    assert!(facts.is_none());
}

#[test]
fn invalid_explicit_overrides_refuse_instead_of_resuming_bare() {
    let (plan, facts) = single(
        &saved(),
        &profiles("planner", profile("claude")),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("missing"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(plan.tabs.is_empty());
    assert!(!plan.skipped[0].reason.label().is_empty());
    assert!(plan.warnings.is_empty());
    assert!(facts.is_none());
}

#[test]
fn unsupported_explicit_preset_refuses_instead_of_resuming_bare() {
    let mut saved = agent("droid", "saved", "/repo", 0);
    saved.profile = Some("worker".into());
    let (plan, facts) = single(
        &saved,
        &profiles("worker", profile("droid")),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                preset: LaunchPreset {
                    model: Some("custom".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(plan.tabs.is_empty());
    assert_eq!(
        plan.skipped[0].reason.label(),
        "droid does not support --model; remove it or put provider-specific flags in `args`"
    );
    assert!(plan.warnings.is_empty());
    assert!(facts.is_none());
}

#[test]
fn opencode_effort_only_resume_warns_and_keeps_the_saved_level() {
    for explicit in [false, true] {
        let mut saved = agent("opencode", "saved", "/repo", 0);
        saved.profile = Some("worker".into());
        saved.effort = Some("high".into());
        saved.record = Some(Box::new(LaunchRecord {
            model: Some("provider/model".into()),
            effort: Some("high".into()),
            ..Default::default()
        }));
        let (plan, _) = single(
            &saved,
            &profiles("worker", profile("opencode")),
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    preset: LaunchPreset {
                        effort: explicit.then(|| "low".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let expected = if explicit {
            vec!["warning: `worker` effort low reaches opencode only through a model switch; resuming at the session's saved level; pass --model with --effort".to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(plan.warnings, expected);
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
        assert_eq!(
            request
                .identity
                .params
                .record
                .as_ref()
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );
        assert!(!request.identity.resume_model_override);
    }
}

#[test]
fn opencode_model_and_agent_resume_overrides_keep_effort_for_the_session_step() {
    for agent_override in [false, true] {
        for explicit_effort in [false, true] {
            let mut saved = agent("opencode", "saved", "/repo", 0);
            saved.profile = Some("worker".into());
            saved.record = Some(Box::new(LaunchRecord {
                model: Some("provider/old".into()),
                effort: Some("high".into()),
                ..Default::default()
            }));
            let profiles = project_profiles(
                "[profiles.worker]\ndescription = 'Worker'\nagent = 'opencode'\n\
                 [profiles.replacement]\ndescription = 'Replacement'\nagent = 'opencode'\n\
                 model = 'provider/new'\neffort = 'max'\n",
            );
            let (plan, _) = single(
                &saved,
                &profiles,
                SingleResumeOptions {
                    overrides: ResumeOverrides {
                        agent: agent_override.then_some("replacement"),
                        preset: LaunchPreset {
                            model: (!agent_override).then(|| "provider/new".into()),
                            effort: explicit_effort.then(|| "low".into()),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
            let request = decode_exec_request(&single_pane_argv(&plan));
            assert!(request.identity.resume_model_override);
            assert_eq!(
                request.identity.params.model.as_deref(),
                Some("provider/new")
            );
            assert_eq!(
                request.identity.params.effort.as_deref(),
                if explicit_effort {
                    Some("low")
                } else if agent_override {
                    Some("max")
                } else {
                    None
                }
            );
            assert_eq!(
                request.identity.params.record.as_ref().unwrap().effort,
                request.identity.params.effort
            );
        }
    }
}

#[test]
fn unsupported_explicit_effort_warns_and_resumes() {
    let mut saved = agent("kimi", "saved", "/repo", 0);
    saved.profile = Some("worker".into());
    let (plan, facts) = single(
        &saved,
        &profiles("worker", profile("kimi")),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                preset: LaunchPreset {
                    effort: Some("high".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(plan.tabs.len(), 1);
    assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    assert_eq!(
        plan.warnings,
        ["warning: `worker` effort high is unsupported on kimi; launching without it"]
    );
    assert!(facts.is_some());
}

/// A resume whose recorded `--agent` or profile no longer resolves degrades to bare with a
/// warning, and `--model` or `--tier` applies on that bare posture rather than refusing.
#[test]
fn model_or_tier_override_applies_on_a_degraded_replay() {
    let tiers = crate::config::tiers::TierConfig::default();
    for gone in ["agent", "profile"] {
        for tier in [false, true] {
            let mut saved = saved();
            if gone == "agent" {
                saved.record.as_mut().unwrap().agent = Some("gone".into());
            } else {
                saved.profile = Some("gone".into());
            }
            let (plan, _) = single(
                &saved,
                &profiles("planner", profile("claude")),
                SingleResumeOptions {
                    overrides: ResumeOverrides {
                        preset: LaunchPreset {
                            model: (!tier).then(|| "haiku".into()),
                            ..Default::default()
                        },
                        selection: crate::config::Selection::new(&tiers)
                            .tier(tier.then_some(crate::config::tiers::ModelTier::Junior)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            assert!(plan.skipped.is_empty(), "{gone} {tier}: {:?}", plan.skipped);
            assert!(
                plan.warnings.iter().any(|w| w.contains("resuming bare")),
                "{gone} {tier}: {:?}",
                plan.warnings
            );
            let request = decode_exec_request(&single_pane_argv(&plan));
            assert_eq!(
                request.identity.params.model.as_deref(),
                Some(if tier { "sonnet" } else { "haiku" }),
                "{gone} {tier}"
            );
            assert_eq!(
                request.identity.params.tier.map(|stamp| stamp.tier),
                tier.then_some(crate::config::tiers::ModelTier::Junior),
                "{gone} {tier}"
            );
        }
    }
}

#[test]
fn agent_override_replaces_a_degraded_replay_without_warning() {
    let mut profiles = profiles("planner", profile("claude"));
    profiles.0.insert("reviewer".into(), profile("claude"));
    let mut saved = saved();
    saved.record.as_mut().unwrap().agent = Some("gone".into());
    let (plan, _) = single(
        &saved,
        &profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("reviewer"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(plan.tabs.len(), 1);
}

#[test]
fn isolation_warning_compares_the_stamp_with_the_resolved_policy() {
    for (stamp, flag, machine, expected) in [
        (
            Some(Isolation::Scoped),
            Some(Isolation::Host),
            Isolation::Scoped,
            Some("@proper-notch ran scoped; resuming under host"),
        ),
        (
            Some(Isolation::Host),
            None,
            Isolation::Scoped,
            Some("@proper-notch ran host; resuming under scoped"),
        ),
        (
            Some(Isolation::Host),
            Some(Isolation::Host),
            Isolation::Scoped,
            None,
        ),
        (None, Some(Isolation::Host), Isolation::Scoped, None),
    ] {
        let mut saved = saved();
        saved.effective_isolation = stamp;
        let (plan, _) = single(
            &saved,
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                isolation: flag,
                machine_isolation: machine,
                ..Default::default()
            },
        );
        assert_eq!(
            plan.warnings,
            expected.into_iter().map(str::to_owned).collect::<Vec<_>>()
        );
    }
}

#[test]
fn returned_facts_follow_flag_record_profile_machine_and_skills() {
    for (flag, stored, default, machine, expected) in [
        (
            Some(Isolation::Host),
            Some(Isolation::Scoped),
            Some(Isolation::Sandbox),
            Isolation::Scoped,
            Isolation::Host,
        ),
        (
            None,
            Some(Isolation::Scoped),
            Some(Isolation::Sandbox),
            Isolation::Host,
            Isolation::Scoped,
        ),
        (
            None,
            None,
            Some(Isolation::Sandbox),
            Isolation::Scoped,
            Isolation::Sandbox,
        ),
        (None, None, None, Isolation::Scoped, Isolation::Scoped),
    ] {
        for skills in [None, Some(Vec::new())] {
            let mut saved = saved();
            saved.isolation = stored;
            let profiles = profiles(
                "planner",
                Profile {
                    isolation: default,
                    skills: skills.clone(),
                    ..profile("claude")
                },
            );
            let (_, facts) = single(
                &saved,
                &profiles,
                SingleResumeOptions {
                    isolation: flag,
                    machine_isolation: machine,
                    ..Default::default()
                },
            );
            assert_eq!(
                facts,
                Some(ResumeLaunchFacts {
                    isolation: expected,
                    skills: skills.is_some(),
                    tier_fallback: None,
                })
            );
        }
    }
}

#[test]
fn adopted_session_applies_model_mode_isolation_without_history_warning() {
    let mut observation = local_session("claude", "native", 0, 1);
    observation.workspace = PathBuf::from("/tmp");
    let (_, plan, facts) = plan_provider_resume(
        &observation,
        None,
        &[],
        ctx(1, None, &no_profiles()),
        &SingleResumeOptions {
            isolation: Some(Isolation::Host),
            overrides: ResumeOverrides {
                permission_mode: Some(PermissionMode::Yolo),
                preset: LaunchPreset {
                    model: Some("sonnet".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(
        request
            .identity
            .params
            .record
            .as_ref()
            .and_then(|record| record.model.as_deref()),
        Some("sonnet")
    );
    assert_eq!(request.identity.params.mode, Some(PermissionMode::Yolo));
    assert_eq!(request.identity.params.isolation, Some(Isolation::Host));
    assert_eq!(
        facts,
        Some(ResumeLaunchFacts {
            isolation: Isolation::Host,
            skills: false,
            tier_fallback: None,
        })
    );
    assert!(plan.warnings.is_empty());
}

#[test]
fn single_resume_resolves_agents_override_for_subagent_scope_and_refuses_cross_provider_model() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("subagents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\n---\nCodex house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/astra.md"),
        "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: low\ntools: [Bash, Read]\n---\nAstra craft.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("subagents/advisor.md"),
        "---\ndescription: Advisor\nagent: claude\nmodel: opus\neffort: high\ntools: [Read]\n---\nAdvise.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    let mut subagent_saved = AgentState {
        name: Some("wise-owl".into()),
        profile: Some("advisor".into()),
        profile_scope: Some(ProfileScope::Subagents),
        mode: Some(PermissionMode::Auto),
        record: Some(Box::new(LaunchRecord {
            model: Some("astra".into()),
            effort: Some("high".into()),
            agent: Some("astra".into()),
            ..Default::default()
        })),
        ..agent("codex", "sub-saved", "/repo", 0)
    };
    let mut both_ctx = ctx(1, None, &machine.agents.profiles);
    both_ctx.subagent_profiles = &machine.subagents.profiles;

    let (plan, _) = plan_recorded_resume(
        &subagent_saved,
        both_ctx,
        &SingleResumeOptions::default(),
        |_| true,
        |_| true,
    );
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(
        request
            .identity
            .params
            .record
            .as_ref()
            .and_then(|r| r.agent.as_deref()),
        Some("astra")
    );
    assert_eq!(request.identity.params.model.as_deref(), Some("astra"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
    let base_prompt = std::fs::read_to_string(
        request
            .system_prompt_file
            .as_ref()
            .expect("base prompt")
            .origin(),
    )
    .unwrap();
    assert!(base_prompt.contains("Codex house."), "{base_prompt}");
    assert!(!base_prompt.contains("Claude house."), "{base_prompt}");
    let appended: String = request
        .append_system_prompt_files
        .iter()
        .map(|src| std::fs::read_to_string(src.origin()).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(appended.contains("Advise."), "{appended}");

    // Explicit resume with `--agent astra` (from agents table, subagent scope) and `--model opus`
    // across providers must refuse with the exact cross-provider line.
    subagent_saved.record = Some(Box::new(LaunchRecord {
        model: Some("astra".into()),
        ..Default::default()
    }));
    let (rejected, _) = plan_recorded_resume(
        &subagent_saved,
        both_ctx,
        &SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("astra"),
                preset: LaunchPreset {
                    model: Some("opus".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
        |_| true,
        |_| true,
    );
    assert!(rejected.tabs.is_empty());
    assert_eq!(rejected.skipped.len(), 1, "{:?}", rejected.skipped);
    assert_eq!(
        rejected.skipped[0].reason,
        ResumeSkipReason::OverrideRejected(
            "--agent astra runs on codex but --model opus runs on claude; choose the same family"
                .into()
        )
    );

    // Same cross-provider refusal on agents scope too (`--agent claude --model astra`).
    let (rejected_agents, _) = single(
        &saved(),
        &machine.agents.profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("claude"),
                preset: LaunchPreset {
                    model: Some("astra".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(rejected_agents.tabs.is_empty());
    assert_eq!(
        rejected_agents.skipped[0].reason,
        ResumeSkipReason::OverrideRejected(
            "--agent claude runs on claude but --model astra runs on codex; choose the same family"
                .into()
        )
    );
}

#[test]
fn single_resume_same_kind_agent_override_on_tiered_profile_succeeds() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\n---\nCodex house.\n",
    )
    .unwrap();
    // Default `principal` tier carries only claude models, so an unpinned walk lands on claude.
    std::fs::write(
        root.path().join("agents/principal_worker.md"),
        "---\ndescription: Principal worker\nagent: claude\ntier: principal\ntools: [Read]\n---\nPrincipal.\n",
    )
    .unwrap();
    for (name, engine) in [
        ("senior_worker", "agent: claude\ntier: senior"),
        ("senior_codex", "agent: codex\ntier: senior"),
        ("plain_worker", "agent: claude"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: W\n{engine}\ntools: [Read]\n---\nW.\n"),
        )
        .unwrap();
    }
    let machine = crate::config::MachineConfig::parse_text(
        &root.path().join("config.toml"),
        "[tiers]\nsenior = [\"opus\", \"astra\"]\n",
        root.path(),
    )
    .unwrap();
    let codex_session = AgentState {
        name: Some("codex-worker".into()),
        profile: Some("principal_worker".into()),
        mode: Some(PermissionMode::Auto),
        record: Some(Box::new(LaunchRecord {
            model: Some("sol".into()),
            ..Default::default()
        })),
        ..agent("codex", "codex-principal", "/repo", 0)
    };
    for (label, model_flag, expected_model) in [
        ("--agent codex without --model", None, None),
        ("--agent codex --model sol", Some("sol"), Some("sol")),
    ] {
        let (plan, _) = single(
            &codex_session,
            &machine.agents.profiles,
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    agent: Some("codex"),
                    preset: LaunchPreset {
                        model: model_flag.map(str::to_owned),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(
            plan.skipped.is_empty(),
            "{label}: unexpected skip: {:?}",
            plan.skipped
        );
        assert_eq!(plan.tabs.len(), 1, "{label}");
        let req = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(req.kind.as_str(), "codex", "{label}");
        assert_eq!(
            req.identity.params.model.as_deref(),
            expected_model,
            "{label}"
        );
    }

    // A tier with a model of the session's kind is still walked to it.
    for (profile, agent) in [("senior_worker", "codex"), ("plain_worker", "senior_codex")] {
        let session = AgentState {
            profile: Some(profile.into()),
            ..codex_session.clone()
        };
        let (plan, _) = single(
            &session,
            &machine.agents.profiles,
            SingleResumeOptions {
                overrides: ResumeOverrides {
                    agent: Some(agent),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(plan.skipped.is_empty(), "{profile}: {:?}", plan.skipped);
        let req = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(
            req.identity.params.model.as_deref(),
            Some("astra"),
            "{profile} --agent {agent}"
        );
    }

    // Another kind stays refused with the existing message.
    let (rejected, _) = single(
        &codex_session,
        &machine.agents.profiles,
        SingleResumeOptions {
            overrides: ResumeOverrides {
                agent: Some("claude"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(rejected.tabs.is_empty());
    assert_eq!(
        rejected.skipped[0].reason,
        ResumeSkipReason::OverrideRejected(
            "this session is codex, but --agent claude is claude; resume with a same-kind profile or launch fresh"
                .into()
        )
    );
}

#[test]
fn project_row_replay_passes_when_valid_and_degrades_like_failed_markdown_when_broken() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::create_dir_all(home.path().join("subagents")).unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    for (rel, body) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base.\ntools: [Read]\n---\nClaude house.\n",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base.\ntools: [Read]\n---\nCodex house.\n",
        ),
        (
            "agents/sub_worker.md",
            "---\ndescription: Machine sub worker.\nagent: claude\n---\nMachine sub worker.\n",
        ),
        (
            "agents/ov.md",
            "---\ndescription: Machine ov.\nagent: claude\n---\nMachine ov.\n",
        ),
        (
            "subagents/ok_sub.md",
            "---\ndescription: Healthy subagent.\nagent: claude\n---\nOk sub.\n",
        ),
    ] {
        std::fs::write(home.path().join(rel), body).unwrap();
    }
    let cfg_path = home.path().join("config.toml");
    std::fs::write(&cfg_path, "").unwrap();
    let machine = crate::config::MachineConfig::parse_text(&cfg_path, "", home.path()).unwrap();

    // Valid project row replays without degradation.
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.worker]\ndescription = 'Worker.'\nagent = 'claude'\neffort = 'high'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let restore_ok =
        LaneRestoreConfig::load_with_roots(&machine, project.path(), home.path(), |_, _| None)
            .unwrap();
    let record = crate::agents::LaunchRecord {
        effort: Some("high".to_owned()),
        ..Default::default()
    };
    let ok_posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("worker"),
            kind: &crate::ids::AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &restore_ok.profiles,
        None,
    );
    assert!(ok_posture.degraded.is_none(), "{:?}", ok_posture.degraded);

    // Broken project rows (agent row, subagent row shadowing machine, kind-named row, and
    // failing subagent override) degrade through `LaneRestoreConfig::load_with_roots`.
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.worker]\nagent = 'claude'\n\
         [profiles.codex]\ndescription = 'Kind named'\nagent = 'codex'\n\
         [subagents.profiles.sub_worker]\nagent = 'claude'\n\
         [subagents.profiles.ov]\nagent = 'claude'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let restore =
        LaneRestoreConfig::load_with_roots(&machine, project.path(), home.path(), |_, _| None)
            .unwrap();
    assert!(!restore.profiles.0.contains_key("worker"));
    let bad_posture = resolve_posture(
        PostureRequest {
            record: Some(&record),
            profile: Some("worker"),
            kind: &crate::ids::AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: None,
        },
        &restore.profiles,
        None,
    );
    let reason = bad_posture
        .degraded
        .as_ref()
        .expect("broken project row must degrade with reason")
        .to_string();
    assert!(
        reason.contains("[profiles.worker]") && reason.contains("description"),
        "{reason}"
    );

    let mut sub_worker =
        crate::testkit::agent_state("claude", "sub-worker", jiff::Timestamp::UNIX_EPOCH);
    sub_worker.profile = Some("sub_worker".into());
    sub_worker.profile_scope = Some(crate::config::effective::ProfileScope::Subagents);
    sub_worker.worktree_path = Some(project.path().display().to_string());
    sub_worker.record = Some(Box::default());

    let mut kind_named =
        crate::testkit::agent_state("codex", "kind-named", jiff::Timestamp::UNIX_EPOCH);
    kind_named.profile = Some("codex".into());
    kind_named.worktree_path = Some(project.path().display().to_string());
    kind_named.record = Some(Box::default());

    let mut failed_ov =
        crate::testkit::agent_state("claude", "failed-ov", jiff::Timestamp::UNIX_EPOCH);
    failed_ov.profile = Some("ok_sub".into());
    failed_ov.profile_scope = Some(crate::config::effective::ProfileScope::Subagents);
    failed_ov.worktree_path = Some(project.path().display().to_string());
    failed_ov.record = Some(Box::new(crate::agents::LaunchRecord {
        agent: Some("ov".into()),
        ..Default::default()
    }));

    let workspace = crate::WorkspaceResolver::resolve_under(
        project.path(),
        None,
        &home.path().join("rimz-state"),
    )
    .unwrap();
    let state = crate::StatePaths::for_project_root(&workspace.project_root).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let logins = crate::agents::room_accounts(&state.workspace_record, None, &machine).unwrap();
    let catalog = crate::agents::LoginCatalog::default();
    let options = SingleResumeOptions::default();
    for (label, agent) in [
        ("sub_worker", &sub_worker),
        ("kind_named", &kind_named),
        ("failed_ov", &failed_ov),
    ] {
        let (plan, _) = plan_recorded_resume(
            agent,
            ResumeContext {
                in_place: false,
                project_root: Some(project.path()),
                workspace_id: &state.workspace_id,
                rimz_bin: std::path::Path::new("rimz"),
                runtime: &runtime,
                profiles: &restore.profiles,
                subagent_profiles: &restore.subagent_profiles,
                selection: &crate::config::Selection::default(),
                max: 10,
                logins: &logins,
                catalog: &catalog,
            },
            &options,
            |_| true,
            |_| true,
        );
        assert_eq!(plan.tabs.len(), 1, "{label}");
        assert!(
            plan.warnings.iter().any(|w| w.contains("resuming bare")),
            "{label}: expected degrade warning, got {:?}",
            plan.warnings
        );
    }
}
