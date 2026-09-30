//! Posture replay: the profile-declared settings a resumed session comes back
//! with, and how a broken profile degrades instead of stranding the session.

use super::*;

#[test]
fn resume_preserves_a_stamped_model_after_a_tier_rebind() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("definitions");
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::write(
        home.join("agents/claude.md"),
        "---\ndescription: Base.\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/planner.md"),
        "---\ndescription: Planner.\nagent: claude\ntier: senior\neffort: medium\ntools: [Bash]\n---\nPlan.",
    )
    .unwrap();
    let path = root.path().join("config.toml");
    let load = |tiers: &crate::config::tiers::TierConfig| {
        crate::config::definitions::load(
            &home,
            crate::config::definitions::SkillCheck::Skip,
            &crate::config::CommandsConfig::default(),
            tiers,
        )
    };
    let initial = load(&crate::config::tiers::TierConfig::default());
    assert!(initial.errors.is_empty(), "{:?}", initial.errors);
    assert_eq!(
        initial.agent_profiles.0["planner"].model.as_deref(),
        Some("opus")
    );
    std::fs::write(&path, "[tiers]\nsenior = ['fable']\nprincipal = []").unwrap();
    let config: crate::config::MachineConfig =
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let rebound = load(&config.tiers);
    let model = "opus";
    let stamp = tier_stamp(model);
    let posture = resolve_posture(
        PostureRequest {
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("claude"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &rebound.agent_profiles,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert_eq!(posture.launch.model.as_deref(), Some(model));
    assert_eq!(posture.launch.effort.as_deref(), Some("medium"));
}

#[test]
fn fallen_back_resume_uses_stamped_family_render_and_model_defaults() {
    for effort in [None, Some("medium")] {
        let profiles = profiles("planner", routed_profile(effort));
        let stamp = tier_stamp("astra");
        let plan = plan_profiled(
            AgentState {
                profile: Some("planner".to_owned()),
                model: Some("hook-observed-model".to_owned()),
                tier: Some(stamp.clone()),
                ..agent("codex", "a1", "/code/qe", 1)
            },
            &profiles,
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.kind.as_str(), "codex");
        assert_eq!(
            request.identity.params.model.as_deref(),
            Some(stamp.model.as_str())
        );
        assert_eq!(request.identity.params.tier, Some(stamp));
        let expected = effort.or(crate::agents::definition_defaults("codex", Some("astra")).effort);
        assert_eq!(request.identity.params.effort.as_deref(), expected);
        assert!(
            matches!(request.action, crate::harness::launch::ExecAction::Resume { extra_args, .. } if extra_args.iter().any(|arg| arg == "--search"))
        );
    }
}

#[test]
fn stamped_alias_resume_replays_the_recorded_session_model() {
    let profiles = profiles("planner", routed_profile(None));
    let stamp = tier_stamp("astra");
    let posture = resolve_posture(
        PostureRequest {
            profile: Some("planner"),
            kind: &AgentKind::new_unchecked("codex"),
            stamped_mode: None,
            stamped_tier: Some(&stamp),
        },
        &profiles,
    );
    assert!(posture.degraded.is_none());
    assert_eq!(posture.launch.model.as_deref(), Some("astra"));
    let plan = plan_profiled(
        AgentState {
            profile: Some("planner".into()),
            model: Some("gpt-6.1-astra".into()),
            tier: Some(stamp.clone()),
            ..agent("codex", "a1", "/code/qe", 1)
        },
        &profiles,
    );
    let mut request = decode_exec_request(&single_pane_argv(&plan));
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    let machine = toml::from_str("[models.codex]\nastra = 'gpt-6.2-astra'").unwrap();
    let (warnings, movement) = crate::harness::launch_plan::resolve_model(
        &mut request,
        &machine,
        &runtime,
        &crate::agents::ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some("gpt-6.1-astra"),
        None,
        &Default::default(),
    )
    .unwrap();
    assert!(warnings.is_empty());
    assert!(movement.is_none());
    assert_eq!(
        request.identity.params.model.as_deref(),
        Some("gpt-6.1-astra")
    );
    assert_eq!(request.identity.params.tier, Some(stamp));
    assert!(
        request
            .action
            .extra_args()
            .windows(2)
            .any(|args| args == ["--model", "gpt-6.1-astra"])
    );
    request.identity.params.model = Some("sol".into());
    let machine = toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    crate::harness::launch_plan::resolve_model(
        &mut request,
        &machine,
        &runtime,
        &crate::agents::ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some("gpt-6.1-astra"),
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(request.identity.params.model.as_deref(), Some("gpt-6-sol"));
}

#[test]
fn resume_replays_the_profile_declared_posture() {
    // A session that launched as `@planner` comes back as a planner: the
    // profile's model, effort, and system prompt ride the resume request,
    // not just the `@planner` handle.
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            auto_compact: Some("200k".to_owned()),
            ..profile("claude")
        },
    );
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles);

    assert!(plan.warnings.is_empty());
    let request = decode_exec_request(&single_pane_argv(&plan));
    let expected = crate::harness::spec::profile_cell("planner", &profiles)
        .expect("planner profile resolves")
        .args;
    assert!(
        expected.iter().any(|arg| arg == "opus"),
        "profile argv should carry the model: {expected:?}"
    );
    assert_eq!(
        request.action,
        crate::harness::launch::ExecAction::Resume {
            session_id: "a1".to_owned(),
            extra_args: expected,
        }
    );
    assert!(
        matches!(&request.action, crate::harness::launch::ExecAction::Resume { extra_args, .. }
        if extra_args.windows(2).any(|args| args == ["--autocompact", "200000"]))
    );
    assert_eq!(request.identity.params.model.as_deref(), Some("opus"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
    assert_eq!(
        request
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(prompt.path())
    );
}

#[test]
fn relaunch_rereads_profile_isolation_without_replaying_the_effective_stamp() {
    use crate::config::Isolation::{Host, Sandbox};
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        effective_isolation: Some(Host),
        ..agent("claude", "a1", "/code/qe", 1)
    };
    for isolation in [Host, Sandbox] {
        let profiles = profiles(
            "planner",
            Profile {
                isolation: Some(isolation),
                ..profile("claude")
            },
        );
        let plan = plan_profiled(agent.clone(), &profiles);
        let request = decode_exec_request(&single_pane_argv(&plan));
        assert_eq!(request.isolation_default, Some(isolation));
        assert_eq!(request.identity.params.isolation, None);
    }
}

#[test]
fn resume_leaves_one_off_launch_values_out_of_the_posture() {
    // `model` on the rollup is observed, not declared — the user may have
    // switched it mid-session with `/model`. Only the profile speaks here.
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        model: Some("some-one-off-model".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles("planner", profile("claude")));

    let argv = single_pane_argv(&plan);
    assert!(
        !argv.iter().any(|arg| arg == "some-one-off-model"),
        "one-off model leaked into the resume argv: {argv:?}"
    );
    assert_eq!(decode_exec_request(&argv).identity.params.model, None);
}

#[test]
fn resume_replays_the_stamped_mode_when_the_profile_declares_none() {
    // The launch event records the permission posture the user granted, so a
    // profile-less agent still comes back with it.
    let agent = AgentState {
        mode: Some(PermissionMode::Yolo),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.action.extra_args(), yolo_argv("claude"));
    assert_eq!(request.identity.params.mode, Some(PermissionMode::Yolo));
}

#[test]
fn resume_degrades_to_bare_when_the_profile_is_gone() {
    // Rebirth runs unattended, so a profile dropped from config warns and
    // recovers rather than refusing to bring the session back.
    let agent = AgentState {
        profile: Some("retired".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    assert_eq!(plan.tabs.len(), 1, "the session still comes back");
    assert_eq!(plan.warnings.len(), 1);
    assert!(
        plan.warnings[0].contains("retired"),
        "warning should name the profile: {}",
        plan.warnings[0]
    );
    assert_eq!(
        decode_exec_request(&single_pane_argv(&plan))
            .action
            .extra_args(),
        &[] as &[String]
    );
}

#[test]
fn profile_mode_wins_over_the_stamped_mode() {
    // The profile is the standing decision; the stamp only fills a gap.
    let profiles = profiles(
        "planner",
        Profile {
            mode: Some(PermissionMode::Auto),
            ..profile("claude")
        },
    );

    let posture = posture_for(
        "claude",
        Some("planner"),
        Some(PermissionMode::Yolo),
        &profiles,
    );

    assert_eq!(posture.launch.mode, Some(PermissionMode::Auto));
    assert!(
        !yolo_argv("claude")
            .iter()
            .any(|arg| posture.launch.args.contains(arg)),
        "stamped yolo argv leaked past the profile's mode: {:?}",
        posture.launch.args
    );
}

#[test]
fn a_profile_prompt_file_that_vanished_degrades_instead_of_refusing() {
    // Rebirth is unattended: a deleted prompt file must not strand the session.
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(dir.path().join("missing.md").into()),
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert!(posture.launch.args.is_empty());
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::PromptFileMissing { .. })
    ));
}

#[test]
fn a_profile_prompt_fragment_that_vanished_degrades_instead_of_refusing() {
    let base = tempfile::NamedTempFile::new().expect("base prompt");
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(base.path().to_path_buf().into()),
            append_system_prompt_files: vec![dir.path().join("missing.md").into()],
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::PromptFileMissing { .. })
    ));
}

#[test]
fn unsupported_prompt_replacement_is_reported_as_a_resume_skip() {
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            ..profile("droid")
        },
    );
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        ..agent("droid", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles);

    assert!(plan.tabs.is_empty());
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(
        plan.skipped,
        [ResumeSkip {
            label: "droid:qe".to_owned(),
            reason: ResumeSkipReason::PromptUnsupported,
        }]
    );
}

#[test]
fn posture_reports_a_provider_switch_rather_than_refusing() {
    // Restart and fork escalate this; unattended resume degrades on it. Either
    // way the resolver reports rather than fails.
    let profiles = profiles("planner", profile("codex"));

    let posture = posture_for("claude", Some("planner"), None, &profiles);

    assert!(posture.launch.args.is_empty());
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::KindChanged { .. })
    ));
}

#[test]
fn stamped_resume_degrades_when_the_profile_loses_its_family_render() {
    let profiles = profiles("planner", profile("codex"));
    let plan = plan_profiled(
        AgentState {
            profile: Some("planner".to_owned()),
            tier: Some(tier_stamp("astra")),
            ..agent("codex", "a1", "/code/qe", 1)
        },
        &profiles,
    );
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.warnings[0].contains("this session is codex"));
    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.identity.params.model, None);
}
