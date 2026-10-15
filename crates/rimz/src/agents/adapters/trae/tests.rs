use super::*;

use crate::agents::{AgentDefinition, LaunchPreset, PermissionMode};
use crate::harness::launch::{AgentProcessCompileErr, ExecAction, compile_provider_argv};
use std::path::Path;

#[test]
fn documented_launch_argv_and_picker() {
    assert_eq!(
        TraeAdapter.launch_command(&[], Some("review")),
        Some(vec!["traecli".to_owned(), "review".to_owned()])
    );
    assert_eq!(
        TraeAdapter.spec().launch.picker,
        Some(["traecli", "resume"].as_slice())
    );
    assert!(
        TraeAdapter
            .resume_command("id", Path::new("/repo"))
            .is_none()
    );
    assert!(TraeAdapter.spec().launch.fork_command("id").is_none());
}

#[test]
fn positional_prompt_rejects_option_prefix() {
    assert!(matches!(
        compile_provider_argv(
            &AgentDefinition::new(&TraeAdapter),
            "trae",
            &ExecAction::Launch {
                prompt: Some("- fix it".to_owned()),
                extra_args: Vec::new(),
            },
            Path::new("/repo"),
        ),
        Err(AgentProcessCompileErr::PositionalPromptOption { .. })
    ));
}

#[test]
fn yolo_only_and_model_only_presets() {
    let spec = TraeAdapter.spec();
    let yolo = spec.launch.permission_args(PermissionMode::Yolo);
    assert_eq!(yolo, ["--yolo"]);
    assert_eq!(
        TraeAdapter.launch_command(&yolo, Some("review")),
        Some(vec!["traecli".into(), "--yolo".into(), "review".into()])
    );
    assert!(spec.launch.permission_args(PermissionMode::Auto).is_empty());
    assert_eq!(
        spec.render_preset(&LaunchPreset {
            model: Some("chosen".into()),
            ..LaunchPreset::default()
        })
        .unwrap(),
        ["--model", "chosen"]
    );
    assert!(
        spec.render_preset(&LaunchPreset {
            effort: Some("high".into()),
            ..LaunchPreset::default()
        })
        .is_err()
    );
    assert!(
        spec.render_preset(&LaunchPreset {
            auto_compact: Some("off".into()),
            ..LaunchPreset::default()
        })
        .is_err()
    );
}

#[test]
fn config_home_uses_only_launch_env_and_no_skills_root() {
    let mut env = BTreeMap::from([("HOME".into(), "/fixture/home".into())]);
    assert_eq!(
        TraeAdapter.config_home(&env),
        Some("/fixture/home/.trae".into())
    );
    env.insert("TRAE_HOME".into(), "/selected".into());
    assert_eq!(TraeAdapter.config_home(&env), Some("/selected".into()));
    assert!(TraeAdapter.skills_home(&env).is_none());
    assert!(TraeAdapter.config_home(&BTreeMap::new()).is_none());
    assert!(matches!(
        crate::sandbox::preflight_launch(
            crate::config::Isolation::Sandbox,
            &crate::ids::AgentKind::new_unchecked("trae"),
            true,
            TraeAdapter.manual_skill(),
        ),
        Err(crate::sandbox::SandboxErr::ManualSkillsUnsupported { .. })
    ));
}
