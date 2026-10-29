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
            fast: false,
            agent: None,
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

fn request(overrides: ResumeOverrides<'_>) -> ExecRequest {
    let mut saved = saved();
    saved.tier = Some(tier_stamp("opus"));
    let mut planner = routed_profile(None);
    planner
        .definition_renders
        .as_mut()
        .unwrap()
        .renders
        .insert("claude".into(), profile("claude"));
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
        saved.record.as_mut().unwrap().fast = !fast;
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
        assert_eq!(request.identity.params.record.unwrap().fast, fast);
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
    assert!(!request.identity.params.record.unwrap().fast);
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
    let mut profiles = profiles("planner", profile("claude"));
    profiles.0.insert(
        "reviewer".into(),
        Profile {
            model: Some("sonnet".into()),
            ..profile("claude")
        },
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
fn recorded_tier_override_selects_same_provider_and_records_model() {
    let tiers = crate::config::tiers::TierConfig::default();
    let request = request(ResumeOverrides {
        tier: Some((crate::config::tiers::ModelTier::Junior, &tiers)),
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
        &profiles("planner", routed_profile(None)),
        SingleResumeOptions {
            overrides: ResumeOverrides {
                tier: Some((crate::config::tiers::ModelTier::Principal, &tiers)),
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
    let tiers = crate::config::tiers::TierConfig::default();
    for overrides in [
        ResumeOverrides {
            agent: Some("missing"),
            ..Default::default()
        },
        ResumeOverrides {
            tier: Some((crate::config::tiers::ModelTier::Senior, &tiers)),
            ..Default::default()
        },
    ] {
        let (plan, facts) = single(
            &saved(),
            &profiles("planner", profile("claude")),
            SingleResumeOptions {
                overrides,
                ..Default::default()
            },
        );
        assert!(plan.tabs.is_empty());
        assert!(!plan.skipped[0].reason.label().is_empty());
        assert!(plan.warnings.is_empty());
        assert!(facts.is_none());
    }
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
            let mut profiles = profiles("worker", profile("opencode"));
            profiles.0.insert(
                "replacement".into(),
                Profile {
                    model: Some("provider/new".into()),
                    effort: Some("max".into()),
                    ..profile("opencode")
                },
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
                Some(if explicit_effort {
                    "low"
                } else if agent_override {
                    "max"
                } else {
                    "high"
                })
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
