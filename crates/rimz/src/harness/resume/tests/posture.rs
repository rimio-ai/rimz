//! Posture replay: the profile-declared settings a resumed session comes back
//! with, and how a broken profile degrades instead of stranding the session.

use super::*;
use crate::agents::LaunchParams;
use crate::harness::spec::AgentCell;

#[test]
fn cell_posture_projection_covers_every_agent_cell_field() {
    let cell = AgentCell {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "o3".to_owned()],
        system_prompt_file: Some(PathBuf::from("system.md")),
        append_system_prompt_files: vec![PathBuf::from("append.md")],
        launch: LaunchParams {
            mode: Some(PermissionMode::Yolo),
            model: Some("o3".to_owned()),
            effort: Some("high".to_owned()),
            budget: Some("$10".to_owned()),
            ..Default::default()
        },
    };
    let AgentCell {
        kind: _,
        args,
        system_prompt_file,
        append_system_prompt_files,
        launch,
    } = cell.clone();

    assert_eq!(
        ResumePosture::from_cell(&cell),
        ResumePosture {
            args,
            system_prompt_file,
            append_system_prompt_files,
            mode: launch.mode,
            model: launch.model,
            effort: launch.effort,
            budget: launch.budget,
            degraded: None,
        }
    );
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
            system_prompt_file: Some(prompt.path().to_path_buf()),
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
    assert_eq!(request.identity.params.model.as_deref(), Some("opus"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
    assert_eq!(request.system_prompt_file.as_deref(), Some(prompt.path()));
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

    assert_eq!(posture.mode, Some(PermissionMode::Auto));
    assert!(
        !yolo_argv("claude")
            .iter()
            .any(|arg| posture.args.contains(arg)),
        "stamped yolo argv leaked past the profile's mode: {:?}",
        posture.args
    );
}

#[test]
fn a_profile_prompt_file_that_vanished_degrades_instead_of_refusing() {
    // Rebirth is unattended: a deleted prompt file must not strand the session.
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(dir.path().join("missing.md")),
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert!(posture.args.is_empty());
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
            system_prompt_file: Some(base.path().to_path_buf()),
            append_system_prompt_files: vec![dir.path().join("missing.md")],
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
            system_prompt_file: Some(prompt.path().to_path_buf()),
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

    assert!(posture.args.is_empty());
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::KindChanged { .. })
    ));
}
