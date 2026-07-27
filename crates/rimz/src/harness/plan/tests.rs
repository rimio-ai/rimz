use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::*;
use crate::agents::{AgentStatus, LaunchParams};
use crate::config::{MachineConfig, Profile, Team, TeamsConfig};
use crate::harness::run::PermissionMode;
use crate::harness::spec::Column;
use crate::ids::AgentKind;

fn role_binding(role: &str) -> RoleBinding {
    RoleBinding {
        role: role.to_owned(),
        profile: format!("{role}-profile"),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_files: None,
        args: None,
    }
}

fn agent_cell_with_role(role: Option<&str>) -> Cell {
    Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams {
            profile: role.map(|role| format!("{role}-profile")),
            role: role.map(ToOwned::to_owned),
            ..Default::default()
        },
    })
}

#[derive(Clone, Copy, Debug)]
enum RequestField {
    Resume,
    Prompt,
    Name,
    LaunchId,
    Profile,
    Role,
    Team,
    LaunchGroup,
    LaunchOrdinal,
    Channel,
    Model,
    Effort,
}

fn assert_request_field(argv: &[String], field: RequestField, value: &str) {
    let request = exec_request(argv);
    let actual = match field {
        RequestField::Resume => match &request.action {
            crate::harness::launch::ExecAction::Resume { session_id, .. } => {
                Some(session_id.as_str())
            }
            _ => None,
        },
        RequestField::Prompt => match &request.action {
            crate::harness::launch::ExecAction::Launch { prompt, .. } => prompt.as_deref(),
            _ => None,
        },
        RequestField::Name => request.identity.name.as_deref(),
        RequestField::LaunchId => request.identity.launch_id.as_deref(),
        RequestField::Profile => request.identity.params.profile.as_deref(),
        RequestField::Role => request.identity.params.role.as_deref(),
        RequestField::Team => request.identity.params.team.as_deref(),
        RequestField::LaunchGroup => request.identity.params.launch_group.as_deref(),
        RequestField::Channel => request.identity.params.channel.as_deref(),
        RequestField::Model => request.identity.params.model.as_deref(),
        RequestField::Effort => request.identity.params.effort.as_deref(),
        RequestField::LaunchOrdinal => {
            assert_eq!(request.identity.params.launch_ordinal, value.parse().ok());
            return;
        }
    };
    assert!(
        actual == Some(value),
        "missing `{field:?}={value}` in {argv:?}"
    );
}

fn exec_request(argv: &[String]) -> crate::harness::launch::ExecRequest {
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("exec request payload");
    let worktree = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--worktree-path").then(|| Path::new(pair[1].as_str())));
    crate::harness::launch::decode_exec_request(&argv[3], worktree, payload)
        .expect("decode exec request")
}

fn preset_cell(kind: &str, args: &[&str], model: Option<&str>, effort: Option<&str>) -> Cell {
    Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked(kind),
        args: args.iter().map(|value| (*value).to_owned()).collect(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams {
            profile: Some(format!("{kind}-coder")),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            ..Default::default()
        },
    })
}

fn finalize<'a>(
    layout: &mut LayoutSpec,
    preset: &'a crate::agents::LaunchPreset,
    passthrough: &'a [String],
) -> std::result::Result<Vec<LaunchFinalizeWarning>, LaunchFinalizeError> {
    finalize_launch_layout(
        layout,
        LaunchFinalizeOptions {
            permission_mode: None,
            preset,
            passthrough,
            budget: None,
            max_turns: None,
        },
    )
}

fn configured_profile(
    agent: &str,
    mode: Option<PermissionMode>,
    model: Option<&str>,
    effort: Option<&str>,
    system_prompt_file: Option<PathBuf>,
    append_system_prompt_files: Option<PathBuf>,
    args: Option<&str>,
) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        budget: None,
        system_prompt_file,
        append_system_prompt_files: append_system_prompt_files.map(|path| vec![path]),
        args: args.map(str::to_owned),
    }
}

fn effective_launch(
    machine: &MachineConfig,
    project_root: &Path,
) -> crate::config::effective::LaunchAgents {
    crate::config::effective::load(
        &machine.agents,
        project_root,
        &project_root.join("config-home"),
    )
    .expect("effective launch config")
}

#[test]
fn profile_prompt_validation_requires_declared_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let present = dir.path().join("planner.md");
    std::fs::write(&present, "be terse").expect("write prompt");
    let present_append = dir.path().join("append.md");
    std::fs::write(&present_append, "follow house style").expect("write append prompt");
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "planner".to_owned(),
        configured_profile(
            "claude",
            None,
            None,
            None,
            Some(present),
            Some(present_append),
            None,
        ),
    );
    let launch = effective_launch(&machine, dir.path());
    let resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"), None)
        .expect("resolve profile");
    validate_profile_prompt_files(&resolved.layout).expect("present prompt files pass");

    for (system, append, fragment) in [
        (
            Some(dir.path().join("absent.md")),
            None,
            "system-prompt-file",
        ),
        (
            None,
            Some(dir.path().join("absent-append.md")),
            "append-system-prompt-files",
        ),
    ] {
        machine.agents.profiles.0.insert(
            "planner".to_owned(),
            configured_profile("claude", None, None, None, system, append, None),
        );
        let launch = effective_launch(&machine, dir.path());
        let resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"), None)
            .expect("resolve missing prompt profile");
        let err =
            validate_profile_prompt_files(&resolved.layout).expect_err("missing prompt fails");
        assert!(err.to_string().contains(fragment), "{err}");
    }

    let invalid = dir.path().join("invalid.md");
    std::fs::write(&invalid, [0xff]).expect("write invalid prompt");
    machine.agents.profiles.0.insert(
        "planner".to_owned(),
        configured_profile("claude", None, None, None, None, Some(invalid), None),
    );
    let launch = effective_launch(&machine, dir.path());
    let resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"), None)
        .expect("resolve invalid prompt profile");
    let err =
        validate_profile_prompt_files(&resolved.layout).expect_err("non-text prompt fails early");
    assert!(err.to_string().contains("cannot be read as text"), "{err}");
}

#[test]
fn cli_prompt_fragments_replace_profile_list_and_require_replacement_support() {
    let dir = tempfile::tempdir().expect("temp dir");
    let profile_fragment = dir.path().join("profile.md");
    let first = dir.path().join("first.md");
    let second = dir.path().join("second.md");
    for path in [&profile_fragment, &first, &second] {
        std::fs::write(path, path.display().to_string()).expect("write prompt");
    }
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "planner".to_owned(),
        configured_profile(
            "claude",
            None,
            None,
            None,
            None,
            Some(profile_fragment),
            None,
        ),
    );
    let launch = effective_launch(&machine, dir.path());
    let mut resolved =
        resolve_launch(&launch, &machine.agents.commands, Some("planner"), None).expect("resolve");
    finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: None,
            preset: &crate::agents::LaunchPreset {
                append_system_prompt_files: vec![first.clone(), second.clone()],
                ..Default::default()
            },
            passthrough: &[],
            budget: None,
            max_turns: None,
        },
    )
    .expect("finalize");
    assert_eq!(
        resolved
            .layout
            .agent_cells()
            .next()
            .unwrap()
            .append_system_prompt_files,
        [first, second]
    );

    machine
        .agents
        .profiles
        .0
        .get_mut("planner")
        .expect("profile")
        .agent = "droid".to_owned();
    let launch = effective_launch(&machine, dir.path());
    let mut resolved =
        resolve_launch(&launch, &machine.agents.commands, Some("planner"), None).expect("resolve");
    let err = finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: None,
            preset: &crate::agents::LaunchPreset::default(),
            passthrough: &[],
            budget: None,
            max_turns: None,
        },
    )
    .expect_err("droid cannot replace prompts");
    assert_eq!(
        err.to_string(),
        "droid does not support config key `append-system-prompt-files` / flag `--append-system-prompt-file`; remove it or put provider-specific flags in `args`"
    );
}

#[test]
fn provider_override_carries_profile_fields_and_renders_with_new_adapter() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "coder".to_owned(),
        configured_profile(
            "codex",
            Some(PermissionMode::Auto),
            Some("profile-model"),
            Some("high"),
            None,
            None,
            Some("--raw profile"),
        ),
    );
    let launch = effective_launch(&machine, dir.path());
    let claude = AgentKind::new_unchecked("claude");
    let resolved = resolve_launch(
        &launch,
        &machine.agents.commands,
        Some("coder"),
        Some(&claude),
    )
    .expect("override");
    let cell = resolved.layout.agent_cells().next().expect("cell");
    assert_eq!(cell.kind, claude);
    assert_eq!(cell.launch.profile.as_deref(), Some("coder"));
    assert_eq!(cell.launch.model.as_deref(), Some("profile-model"));
    assert_eq!(cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        cell.args,
        [
            "--model",
            "profile-model",
            "--effort",
            "high",
            "--permission-mode",
            "auto",
            "--raw",
            "profile",
        ]
    );

    let kimi = AgentKind::new_unchecked("kimi");
    let err = resolve_launch(
        &launch,
        &machine.agents.commands,
        Some("coder"),
        Some(&kimi),
    )
    .expect_err("kimi cannot express effort");
    assert!(
        err.to_string()
            .contains("does not support profile field `effort`")
    );
}

#[test]
fn resolved_launch_finalizes_profile_cli_and_passthrough_precedence() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "planner".to_owned(),
        configured_profile(
            "codex",
            None,
            Some("profile"),
            Some("high"),
            None,
            None,
            Some("--model raw-profile --config=model_reasoning_effort=low"),
        ),
    );
    let launch = effective_launch(&machine, dir.path());
    let preset = crate::agents::LaunchPreset {
        model: Some("override".to_owned()),
        effort: Some("xhigh".to_owned()),
        ..Default::default()
    };
    let passthrough = vec![
        "--model".to_owned(),
        "raw-cli".to_owned(),
        "-c".to_owned(),
        "model_reasoning_effort=medium".to_owned(),
    ];

    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"), None)
        .expect("resolve launch");
    finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Yolo),
            preset: &preset,
            passthrough: &passthrough,
            budget: Some("2/day".parse().expect("budget")),
            max_turns: None,
        },
    )
    .expect("finalize launch");
    let [
        Cell::Agent(AgentCell {
            args,
            launch:
                LaunchParams {
                    mode,
                    model,
                    effort,
                    budget,
                    ..
                },
            ..
        }),
    ] = resolved.layout.columns[0].rows.as_slice()
    else {
        panic!("one agent")
    };
    assert_eq!(*mode, Some(PermissionMode::Yolo));
    assert_eq!(model.as_deref(), Some("override"));
    assert_eq!(effort.as_deref(), Some("xhigh"));
    assert_eq!(budget.as_deref(), Some("$2.00/day"));
    assert_eq!(
        args,
        &[
            "--dangerously-bypass-approvals-and-sandbox",
            "--model",
            "override",
            "-c",
            "model_reasoning_effort=xhigh",
        ]
    );
}

#[test]
fn resolved_launch_retains_profile_mode_and_wires_turn_limits() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "asked".to_owned(),
        configured_profile(
            "codex",
            Some(PermissionMode::Ask),
            None,
            None,
            None,
            None,
            None,
        ),
    );
    let launch = effective_launch(&machine, dir.path());
    let preset = crate::agents::LaunchPreset::default();
    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("asked,codex"), None)
        .expect("resolve launch");
    finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Yolo),
            preset: &preset,
            passthrough: &[],
            budget: None,
            max_turns: None,
        },
    )
    .expect("finalize launch");
    let modes = resolved
        .layout
        .agent_cells()
        .map(|cell| cell.launch.mode)
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        [Some(PermissionMode::Ask), Some(PermissionMode::Yolo)]
    );

    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("claude"), None)
        .expect("resolve supervised launch");
    finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Auto),
            preset: &preset,
            passthrough: &[],
            budget: None,
            max_turns: Some(3),
        },
    )
    .expect("finalize supervised launch");
    let [
        Cell::Agent(AgentCell {
            args,
            launch: LaunchParams { model, .. },
            ..
        }),
    ] = resolved.layout.columns[0].rows.as_slice()
    else {
        panic!("one agent")
    };
    assert_eq!(args, &["--permission-mode", "auto", "--max-turns", "3"]);
    assert_eq!(model, &None);

    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("codex"), None)
        .expect("resolve codex launch");
    let err = finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Auto),
            preset: &preset,
            passthrough: &[],
            budget: None,
            max_turns: Some(3),
        },
    )
    .expect_err("codex should reject max turns");
    assert_eq!(err.to_string(), "codex does not support --max-turns");
}

#[test]
fn launch_options_apply_without_overwriting_spec_identity() {
    let auto_args = crate::agents::find_definition("codex")
        .expect("codex")
        .spec()
        .launch
        .permission_args(PermissionMode::Auto);
    let cell = |args, mode| {
        Cell::Agent(AgentCell {
            kind: AgentKind::new_unchecked("codex"),
            args,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            launch: LaunchParams {
                profile: Some("codex-coder".to_owned()),
                mode,
                role: Some("coder".to_owned()),
                model: Some("profile-model".to_owned()),
                effort: Some("medium".to_owned()),
                ..Default::default()
            },
        })
    };
    let mut layout = LayoutSpec::single(cell(vec!["--model".into(), "profile-model".into()], None));
    layout.columns[0]
        .rows
        .push(cell(auto_args.clone(), Some(PermissionMode::Auto)));
    finalize_launch_layout(
        &mut layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Yolo),
            preset: &crate::agents::LaunchPreset {
                model: Some("override-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                ..Default::default()
            },
            passthrough: &["--debug".to_owned()],
            budget: None,
            max_turns: None,
        },
    )
    .expect("finalize launch");
    let [
        Cell::Agent(AgentCell {
            args: unset_args,
            launch:
                LaunchParams {
                    mode: unset_mode,
                    model: unset_model,
                    effort: unset_effort,
                    ..
                },
            ..
        }),
        Cell::Agent(AgentCell {
            args: preset_args,
            launch:
                LaunchParams {
                    mode: preset_mode,
                    model: preset_model,
                    effort: preset_effort,
                    ..
                },
            ..
        }),
    ] = layout.columns[0].rows.as_slice()
    else {
        panic!("two agents")
    };
    assert_eq!(
        (*unset_mode, *preset_mode),
        (Some(PermissionMode::Yolo), Some(PermissionMode::Auto))
    );
    assert!(unset_args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()));
    assert!(preset_args.starts_with(&auto_args));
    for args in [unset_args, preset_args] {
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "override-model"])
        );
        assert!(
            args.iter().any(|arg| arg.contains("xhigh")) && args.contains(&"--debug".to_owned())
        );
    }
    assert_eq!(
        (unset_model.as_deref(), unset_effort.as_deref()),
        (Some("override-model"), Some("xhigh"))
    );
    assert_eq!(
        (preset_model.as_deref(), preset_effort.as_deref()),
        (Some("override-model"), Some("xhigh"))
    );
}

#[test]
fn default_launch_models_stamp_only_cells_without_models() {
    let codex_default = crate::agents::find_definition("codex")
        .expect("codex")
        .default_launch_model()
        .expect("codex default model");
    let explicit = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "o3".to_owned()],
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams {
            model: Some("o3".to_owned()),
            ..Default::default()
        },
    });
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    layout.columns[0]
        .rows
        .extend([explicit, Cell::agent(AgentKind::new_unchecked("claude"))]);
    finalize(&mut layout, &Default::default(), &[]).expect("finalize launch");
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == &codex_default && args == &["--model", codex_default.as_str()]));
    assert!(matches!(&layout.columns[0].rows[1],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == "o3" && args == &["--model", "o3"]));
    assert_eq!(
        layout.columns[0].rows[2],
        Cell::agent(AgentKind::new_unchecked("claude"))
    );
}

#[test]
fn args_only_model_becomes_identity_and_suppresses_default() {
    let mut layout = LayoutSpec::single(preset_cell(
        "codex",
        &["--debug", "--model", "gpt-5.6-sol"],
        None,
        None,
    ));
    assert!(
        finalize(&mut layout, &Default::default(), &[])
            .expect("finalize launch")
            .is_empty()
    );
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == "gpt-5.6-sol"
                && args == &["--debug", "--model", "gpt-5.6-sol"]));
}

#[test]
fn declared_model_replaces_different_args_and_dedupes_equal_args_silently() {
    let mut different = LayoutSpec::single(preset_cell(
        "codex",
        &["--model", "gpt-5.6-max", "--model=gpt-5.6-sol"],
        Some("gpt-5.6-max"),
        None,
    ));
    assert_eq!(
        finalize(&mut different, &Default::default(), &[])
            .expect("finalize launch")
            .into_iter()
            .map(|warning| warning.to_string())
            .collect::<Vec<_>>(),
        [
            "warning: profile `codex-coder` args set --model gpt-5.6-sol; declared model gpt-5.6-max wins"
        ]
    );
    assert!(matches!(&different.columns[0].rows[0],
        Cell::Agent(AgentCell { args, .. }) if args == &["--model", "gpt-5.6-max"]));

    let mut equal = LayoutSpec::single(preset_cell(
        "codex",
        &["--model", "gpt-5.6-max", "-m", "gpt-5.6-max"],
        Some("gpt-5.6-max"),
        None,
    ));
    assert!(
        finalize(&mut equal, &Default::default(), &[])
            .expect("finalize launch")
            .is_empty()
    );
    assert!(matches!(&equal.columns[0].rows[0],
        Cell::Agent(AgentCell { args, .. }) if args == &["--model", "gpt-5.6-max"]));
}

#[test]
fn declared_prompt_removes_raw_replacement_args_before_exec_materializes_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let typed = dir.path().join("typed.md");
    std::fs::write(&typed, "typed").expect("typed prompt");
    let mut cell = preset_cell(
        "claude",
        &["--system-prompt-file", "/raw.md", "--debug"],
        None,
        None,
    );
    let Cell::Agent(agent_cell) = &mut cell else {
        unreachable!("preset_cell always returns an agent");
    };
    agent_cell.system_prompt_file = Some(typed);
    let mut layout = LayoutSpec::single(cell);

    assert_eq!(
        finalize(&mut layout, &Default::default(), &[])
            .expect("finalize launch")
            .into_iter()
            .map(|warning| warning.to_string())
            .collect::<Vec<_>>(),
        [
            "warning: profile `claude-coder` args set --system-prompt-file /raw.md; declared system prompt wins"
        ]
    );
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, .. }) if args == &["--debug"]));
}

#[test]
fn args_only_model_uses_last_short_or_joined_occurrence() {
    let mut layout = LayoutSpec::single(preset_cell(
        "codex",
        &["--model=first", "--debug", "-m", "second"],
        None,
        None,
    ));
    assert_eq!(
        finalize(&mut layout, &Default::default(), &[])
            .expect("finalize launch")
            .len(),
        1
    );
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == "second" && args == &["--debug", "-m", "second"]));
}

#[test]
fn launch_model_override_wins_over_profile_and_args_models_silently() {
    let mut layout = LayoutSpec::single(preset_cell(
        "codex",
        &["--model", "profile", "--model", "raw"],
        Some("profile"),
        None,
    ));
    let warnings = finalize(
        &mut layout,
        &crate::agents::LaunchPreset {
            model: Some("override".into()),
            ..Default::default()
        },
        &[],
    )
    .expect("finalize launch");
    assert!(warnings.is_empty());
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == "override" && args == &["--model", "override"]));

    let mut args_only = LayoutSpec::single(preset_cell(
        "codex",
        &["--model", "profile-args"],
        None,
        None,
    ));
    let warnings = finalize(
        &mut args_only,
        &crate::agents::LaunchPreset {
            model: Some("override".into()),
            ..Default::default()
        },
        &[],
    )
    .expect("finalize launch");
    assert!(warnings.is_empty());
    assert!(matches!(&args_only.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { model: Some(model), .. }, .. })
            if model == "override" && args == &["--model", "override"]));
}

#[test]
fn config_key_effort_reconciles_without_touching_unrelated_or_undeclared_flags() {
    let mut codex = LayoutSpec::single(preset_cell(
        "codex",
        &[
            "-c",
            "model_reasoning_effort=high",
            "-c",
            "web_search=cached",
            "--config=model_reasoning_effort=low",
        ],
        None,
        Some("high"),
    ));
    assert_eq!(
        finalize(&mut codex, &Default::default(), &[])
            .expect("finalize launch")
            .len(),
        1
    );
    let codex_default = crate::agents::find_definition("codex")
        .expect("codex")
        .default_launch_model()
        .expect("codex default model");
    assert!(matches!(&codex.columns[0].rows[0],
    Cell::Agent(AgentCell { args, .. })
        if args == &[
            "-c",
            "web_search=cached",
            "-c",
            "model_reasoning_effort=high",
            "--model",
            codex_default.as_str(),
        ]));

    let mut claude = LayoutSpec::single(preset_cell("claude", &["--effort", "high"], None, None));
    assert!(
        finalize(&mut claude, &Default::default(), &[])
            .expect("finalize launch")
            .is_empty()
    );
    assert!(matches!(&claude.columns[0].rows[0],
        Cell::Agent(AgentCell { args, launch: LaunchParams { effort: None, .. }, .. }) if args == &["--effort", "high"]));
}

#[test]
fn supervised_turn_limit_renders_supported_adapter_and_fails_fast() {
    let preset = crate::agents::LaunchPreset::default();
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
    finalize_launch_layout(
        &mut layout,
        LaunchFinalizeOptions {
            permission_mode: None,
            preset: &preset,
            passthrough: &[],
            budget: None,
            max_turns: Some(3),
        },
    )
    .expect("claude supports max turns");
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, .. }) if args == &["--max-turns", "3"]));

    let mut layout = LayoutSpec::single(preset_cell(
        "codex",
        &["--model", "first", "--model", "second"],
        None,
        None,
    ));
    layout.columns[0].rows.push(preset_cell(
        "codex",
        &["--model", "third", "--model", "fourth"],
        None,
        None,
    ));
    let err = finalize_launch_layout(
        &mut layout,
        LaunchFinalizeOptions {
            permission_mode: None,
            preset: &preset,
            passthrough: &[],
            budget: None,
            max_turns: Some(3),
        },
    )
    .expect_err("codex rejects max turns");
    assert_eq!(err.to_string(), "codex does not support --max-turns");
    assert_eq!(
        err.warnings()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "warning: profile `codex-coder` args set --model first; later model second wins",
            "warning: profile `codex-coder` args set --model third; later model fourth wins",
        ]
    );
}

#[test]
fn finalization_handles_mixed_cells_without_leaking_state() {
    let codex_default = crate::agents::find_definition("codex")
        .expect("codex")
        .default_launch_model()
        .expect("codex default model");
    let command = Cell::Command {
        argv: vec!["printf".to_owned(), "untouched".to_owned()],
    };
    let mut layout = LayoutSpec {
        columns: vec![Column {
            rows: vec![
                preset_cell(
                    "codex",
                    &["--model", "profile", "--model", "raw"],
                    Some("profile"),
                    None,
                ),
                Cell::agent(AgentKind::new_unchecked("codex")),
                Cell::agent(AgentKind::new_unchecked("claude")),
                command.clone(),
            ],
            stacked: false,
        }],
    };
    finalize_launch_layout(
        &mut layout,
        LaunchFinalizeOptions {
            permission_mode: Some(PermissionMode::Yolo),
            preset: &Default::default(),
            passthrough: &["--debug".to_owned()],
            budget: Some("2/day".parse().expect("budget")),
            max_turns: None,
        },
    )
    .expect("finalize mixed layout");

    let [profile, bare, no_default, actual_command] = layout.columns[0].rows.as_slice() else {
        panic!("mixed cells")
    };
    assert!(matches!(profile,
        Cell::Agent(AgentCell { launch: LaunchParams { model: Some(model), budget: Some(budget), .. }, .. })
            if model == "profile" && budget == "$2.00/day"));
    assert!(matches!(bare,
        Cell::Agent(AgentCell { launch: LaunchParams { model: Some(model), budget: Some(budget), .. }, .. })
            if model == &codex_default && budget == "$2.00/day"));
    assert!(matches!(no_default,
        Cell::Agent(AgentCell { launch: LaunchParams { model: None, budget: Some(budget), .. }, .. })
            if budget == "$2.00/day"));
    assert_eq!(actual_command, &command);
}

#[test]
fn launch_placement_matrix() {
    use Placement::{NewPane, NewTab, SamePane};

    for (name, new_tab, new_pane, policy, is_worktree, single_cell, has_pane, expected) in [
        (
            "auto single same-pane",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            true,
            SamePane,
        ),
        (
            "auto worktree tab",
            false,
            false,
            LaunchPlacement::Auto,
            true,
            true,
            true,
            NewTab,
        ),
        (
            "auto multi tab",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            false,
            true,
            NewTab,
        ),
        (
            "auto no ambient pane tab",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            false,
            NewTab,
        ),
        (
            "explicit tab",
            true,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            true,
            NewTab,
        ),
        (
            "explicit pane",
            false,
            true,
            LaunchPlacement::Auto,
            true,
            true,
            true,
            NewPane,
        ),
        (
            "pane policy split",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            true,
            true,
            NewPane,
        ),
        (
            "pane policy worktree tab",
            false,
            false,
            LaunchPlacement::Pane,
            true,
            true,
            true,
            NewTab,
        ),
        (
            "pane policy multi tab",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            false,
            true,
            NewTab,
        ),
        (
            "pane policy no ambient pane tab",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            true,
            false,
            NewTab,
        ),
        (
            "tab policy",
            false,
            false,
            LaunchPlacement::Tab,
            false,
            true,
            true,
            NewTab,
        ),
    ] {
        assert_eq!(
            resolve_placement(
                new_tab,
                new_pane,
                policy,
                is_worktree,
                single_cell,
                has_pane
            )
            .unwrap(),
            expected,
            "{name}"
        );
    }

    for (placement, bg, allow_in_place, expected) in [
        (SamePane, true, true, NewPane),
        (SamePane, false, false, NewPane),
        (NewTab, false, false, NewTab),
        (NewPane, true, false, NewPane),
    ] {
        assert_eq!(
            apply_in_place_downgrade(placement, bg, allow_in_place),
            expected
        );
    }

    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, false, true)
        .expect_err("multi-cell new-pane");
    assert!(err.to_string().contains("single agent cell"), "{err:#}");

    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, true, false)
        .expect_err("paneless new-pane");
    assert!(err.to_string().contains("inside the room"), "{err:#}");
}

#[test]
fn fork_placement_defaults_to_the_launching_pane() {
    use Placement::{NewPane, NewTab, SamePane};

    for (name, new_tab, new_pane, bg, has_pane, expected) in [
        ("plain fork", false, false, false, true, SamePane),
        ("explicit pane", false, true, false, true, NewPane),
        ("explicit tab", true, false, false, true, NewTab),
        ("background fork", false, false, true, true, NewPane),
        ("outside a room", false, false, false, false, NewTab),
    ] {
        assert_eq!(
            resolve_fork_placement(new_tab, new_pane, bg, has_pane).unwrap(),
            expected,
            "{name}"
        );
    }
}

#[test]
fn single_role_team_launch_takes_over_caller_pane() {
    let profiles = crate::config::ProfilesConfig(BTreeMap::from([(
        "planner-profile".to_owned(),
        Profile {
            agent: "codex".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: None,
            args: None,
        },
    )]));
    let teams = TeamsConfig(BTreeMap::from([(
        "solo".to_owned(),
        Team {
            roles: vec![role_binding("planner")],
            leader: None,
            layout: None,
            scratch_files: Vec::new(),
        },
    )]));

    for spec in ["solo", "solo.planner"] {
        let layout = crate::harness::spec::resolve_spec(
            Some(spec),
            &profiles,
            &crate::config::CommandsConfig::default(),
            &teams,
        )
        .expect("single-role team launch");
        let placement = apply_in_place_downgrade(
            resolve_placement(
                false,
                false,
                LaunchPlacement::Auto,
                false,
                layout.agent_cells().count() == 1,
                true,
            )
            .unwrap(),
            false,
            true,
        );

        assert_eq!(crate::harness::spec::spec_team(spec, &teams), Some("solo"));
        assert_eq!(placement, Placement::SamePane);
    }
}

#[test]
fn launch_request_names_and_metadata() {
    let layout = LayoutSpec::single(Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("codex"),
        args: Vec::new(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams {
            profile: Some("codex-coder".to_owned()),
            mode: Some(PermissionMode::Yolo),
            role: Some("coder".to_owned()),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        },
    }));

    let requests = launch_identity_requests(
        &layout,
        Some("docs"),
        None,
        Some("forge"),
        None,
        Some("design"),
        Some(("draft it", 0)),
        None,
    )
    .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Explicit("docs".to_owned())
    );
    assert_eq!(requests[0].kind.as_str(), "codex");
    assert_eq!(requests[0].launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(requests[0].launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(requests[0].launch.role.as_deref(), Some("coder"));
    assert_eq!(requests[0].launch.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(requests[0].launch.effort.as_deref(), Some("high"));
    assert_eq!(requests[0].launch.team.as_deref(), Some("forge"));
    assert_eq!(requests[0].launch.channel.as_deref(), Some("design"));
    assert_eq!(requests[0].prompt.as_deref(), Some("draft it"));

    let requests = launch_identity_requests(
        &layout,
        None,
        Some("my_feature"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Soft("my_feature".to_owned())
    );
    assert_eq!(
        launch_identity_requests(&layout, None, None, None, None, None, None, None).unwrap()[0]
            .name,
        AgentLaunchName::Mint
    );
    assert!(
        launch_identity_requests(
            &layout,
            Some("my_feature"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("invalid agent name")
    );
}

#[test]
fn launch_identity_requests_stamp_team_and_inline_cohort_order() {
    let team_roles = vec![
        role_binding("planner"),
        role_binding("coder"),
        role_binding("reviewer"),
    ];
    let team_layout = LayoutSpec {
        columns: vec![Column {
            rows: vec![
                agent_cell_with_role(Some("coder")),
                agent_cell_with_role(Some("planner")),
                agent_cell_with_role(None),
            ],
            stacked: false,
        }],
    };

    let requests = launch_identity_requests(
        &team_layout,
        None,
        None,
        Some("forge"),
        Some(&team_roles),
        None,
        Some(("implement", 1)),
        None,
    )
    .unwrap();
    assert_eq!(requests[0].launch.launch_ordinal, Some(1));
    assert_eq!(requests[1].launch.launch_ordinal, Some(0));
    assert_eq!(requests[2].launch.launch_ordinal, None);
    assert_eq!(requests[0].prompt, None);
    assert_eq!(requests[1].prompt.as_deref(), Some("implement"));
    assert!(
        requests
            .iter()
            .all(|request| request.launch.launch_group.is_none())
    );

    let inline = crate::harness::spec::parse_layout_spec(
        "claude:planner,codex:coder",
        &crate::config::ProfilesConfig::default(),
        &crate::config::CommandsConfig::default(),
    )
    .unwrap();
    let requests =
        launch_identity_requests(&inline, None, None, None, None, None, None, None).unwrap();
    let group = requests[0].launch.launch_group.as_deref().unwrap();
    assert!(group.starts_with("launch_"));
    assert_eq!(requests[1].launch.launch_group.as_deref(), Some(group));
    assert_eq!(requests[0].launch.launch_ordinal, Some(0));
    assert_eq!(requests[1].launch.launch_ordinal, Some(1));

    let single = LayoutSpec::single(agent_cell_with_role(None));
    let requests =
        launch_identity_requests(&single, None, None, None, None, None, None, None).unwrap();
    assert_eq!(requests[0].launch.launch_group, None);
    assert_eq!(requests[0].launch.launch_ordinal, None);
}

#[test]
fn layout_panes_put_the_prompt_only_on_the_leader_agent() {
    let layout = LayoutSpec {
        columns: vec![
            Column {
                rows: vec![Cell::agent(AgentKind::new_unchecked("claude"))],
                stacked: false,
            },
            Column {
                rows: vec![
                    Cell::shell(),
                    Cell::agent(AgentKind::new_unchecked("codex")),
                ],
                stacked: false,
            },
        ],
    };
    let identity = |kind: &str, name: &str| AgentLaunchIdentity {
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(format!("launch_{name}")),
        name: name.to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams::default(),
        run_id: None,
        prompt: None,
    };
    let mut identities = [identity("claude", "first"), identity("codex", "leader")];
    identities[1].prompt = Some("lead this".to_owned());
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/tmp/project"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: None,
            launch_identities: &identities,
            fallback_channel: None,
        },
    )
    .unwrap();

    assert!(matches!(
        &exec_request(&panes.columns[0].panes[0].argv).action,
        crate::harness::launch::ExecAction::Launch { prompt: None, .. }
    ));
    assert_request_field(
        &panes.columns[1].panes[1].argv,
        RequestField::Prompt,
        "lead this",
    );
    assert_eq!(
        panes.columns[1].panes[0].argv,
        [crate::harness::launch::user_shell_program()]
    );
}

#[test]
fn mixed_resume_and_fresh_panes_stay_aligned_in_layout_order() {
    let layout = LayoutSpec {
        columns: vec![Column {
            rows: vec![
                Cell::agent(AgentKind::new_unchecked("claude")),
                Cell::Command {
                    argv: vec!["watch".to_owned()],
                },
                Cell::agent(AgentKind::new_unchecked("codex")),
            ],
            stacked: false,
        }],
    };
    let mut resumed = crate::testkit::agent_state("claude", "sess-resume", Timestamp::now());
    resumed.name = Some("steady-beacon".to_owned());
    resumed.channel = None;
    let seeds = vec![CohortSeed::Resume(Box::new(resumed)), CohortSeed::Fresh];
    let fresh = AgentLaunchIdentity {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("launch_fresh"),
        name: "bright-river".to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams {
            channel: Some("fallback".to_owned()),
            ..Default::default()
        },
        run_id: None,
        prompt: Some("fresh prompt".to_owned()),
    };

    let fresh = [fresh];
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&seeds),
            launch_identities: &fresh,
            fallback_channel: Some("fallback"),
        },
    )
    .expect("mixed panes");

    let panes = &panes.columns[0].panes;
    assert_request_field(&panes[0].argv, RequestField::Resume, "sess-resume");
    assert_request_field(&panes[0].argv, RequestField::Channel, "fallback");
    assert_eq!(panes[1].argv, vec!["watch"]);
    assert_request_field(&panes[2].argv, RequestField::Name, "bright-river");
    assert_request_field(&panes[2].argv, RequestField::Prompt, "fresh prompt");

    let err = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&[CohortSeed::Fresh, CohortSeed::Fresh]),
            launch_identities: &[],
            fallback_channel: None,
        },
    )
    .expect_err("identity count mismatch");
    assert_eq!(
        err.to_string(),
        "launch plan missing identity for agent cell 0"
    );

    let err = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&[CohortSeed::Fresh]),
            launch_identities: &fresh,
            fallback_channel: None,
        },
    )
    .expect_err("resume seed count mismatch");
    assert_eq!(err.to_string(), "resume plan has 1 seeds for 2 agent cells");

    let surplus = [fresh[0].clone(), fresh[0].clone()];
    let err = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&seeds),
            launch_identities: &surplus,
            fallback_channel: None,
        },
    )
    .expect_err("surplus launch identity");
    assert_eq!(
        err.to_string(),
        "launch plan has more identities than fresh agent cells"
    );
}

#[test]
fn pane_command_stamps_cli_identity_and_close_policy() {
    let cell = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams::default(),
    });
    let launch = AgentLaunchIdentity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_0123456789abcdef0123456789abcdef"),
        name: "swift-otter".to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams {
            profile: Some("claude-planner".to_owned()),
            role: Some("planner".to_owned()),
            team: Some("forge".to_owned()),
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(2),
            channel: Some("design".to_owned()),
            model: Some("claude-sonnet".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        },
        run_id: None,
        prompt: None,
    };
    let layout = LayoutSpec::single(cell);
    let launches = [launch];
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/tmp/project"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: None,
            launch_identities: &launches,
            fallback_channel: None,
        },
    )
    .unwrap();
    let pane = &panes.columns[0].panes[0];

    for (field, value) in [
        (RequestField::Name, "swift-otter"),
        (
            RequestField::LaunchId,
            "launch_0123456789abcdef0123456789abcdef",
        ),
        (RequestField::Profile, "claude-planner"),
        (RequestField::Role, "planner"),
        (RequestField::Team, "forge"),
        (RequestField::LaunchGroup, "launch_group_1"),
        (RequestField::LaunchOrdinal, "2"),
        (RequestField::Channel, "design"),
        (RequestField::Model, "claude-sonnet"),
        (RequestField::Effort, "high"),
    ] {
        assert_request_field(&pane.argv, field, value);
    }
    assert!(exec_request(&pane.argv).close_pane_on_exit);

    for (cleanup_worktree, in_place) in [(false, true), (true, false)] {
        let panes = compile_layout_panes(
            &layout,
            LayoutPaneParams {
                cwd: Path::new("/tmp/project"),
                cleanup_worktree,
                in_place,
                resume_seeds: None,
                launch_identities: &launches,
                fallback_channel: None,
            },
        )
        .unwrap();
        let pane = &panes.columns[0].panes[0];
        assert!(!exec_request(&pane.argv).close_pane_on_exit);
        assert_eq!(
            exec_request(&pane.argv).worktree_path.as_deref(),
            cleanup_worktree.then_some(Path::new("/tmp/project"))
        );
    }
}

/// A resumed pane takes its *identity* from the durable record — it is the same
/// agent coming back — and its *posture* from the resolved cell, so a team
/// member returns with the model, effort, and argv its role binding declares.
#[test]
fn pane_command_resume_keeps_prior_identity_and_replays_cell_posture() {
    let cell = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: vec!["--profile-declared".to_owned()],
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: LaunchParams {
            profile: Some("new-profile".to_owned()),
            role: Some("new-role".to_owned()),
            model: Some("new-model".to_owned()),
            effort: Some("new-effort".to_owned()),
            ..Default::default()
        },
    });
    let mut agent = crate::testkit::agent_state("claude", "sess-1", Timestamp::now());
    agent.status = AgentStatus::Idle;
    agent.name = Some("swift-otter".to_owned());
    agent.profile = Some("prior-profile".to_owned());
    agent.role = Some("prior-role".to_owned());
    agent.team = Some("forge".to_owned());
    agent.launch_group = Some("launch_group_1".to_owned());
    agent.launch_ordinal = Some(1);
    agent.channel = Some("design".to_owned());
    agent.model = Some("old-model".to_owned());
    agent.effort = Some("old-effort".to_owned());
    let seeds = [CohortSeed::Resume(Box::new(agent))];
    let layout = LayoutSpec::single(cell);
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/tmp/project"),
            cleanup_worktree: false,
            in_place: false,
            resume_seeds: Some(&seeds),
            launch_identities: &[],
            fallback_channel: Some("new-channel"),
        },
    )
    .unwrap();
    let pane = &panes.columns[0].panes[0];

    for (field, value) in [
        (RequestField::Resume, "sess-1"),
        (RequestField::Name, "swift-otter"),
        (RequestField::Profile, "prior-profile"),
        (RequestField::Role, "prior-role"),
        (RequestField::Team, "forge"),
        (RequestField::LaunchGroup, "launch_group_1"),
        (RequestField::LaunchOrdinal, "1"),
        (RequestField::Channel, "design"),
    ] {
        assert_request_field(&pane.argv, field, value);
    }
    let request = exec_request(&pane.argv);
    assert!(request.close_pane_on_exit);
    // Posture is the cell's, not the session's observed values: `old-model` was
    // whatever the user last switched to mid-session and stays out.
    assert_eq!(request.identity.params.model.as_deref(), Some("new-model"));
    assert_eq!(
        request.identity.params.effort.as_deref(),
        Some("new-effort")
    );
    assert!(matches!(
        request.action,
        crate::harness::launch::ExecAction::Resume { ref extra_args, .. }
            if extra_args == &vec!["--profile-declared".to_owned()]
    ));
}

#[test]
fn preset_values_trim_and_drop_blanks() {
    assert_eq!(
        normalized_preset_value(Some(" gpt-5 ")),
        Some("gpt-5".to_owned())
    );
    assert_eq!(normalized_preset_value(Some("  ")), None);
    assert_eq!(normalized_preset_value(None), None);
}

#[test]
fn prompt_that_looks_like_spec_reports_the_fan_out_fix() {
    let err = reject_prompt_that_looks_like_spec(
        Some("claude"),
        Some("codex"),
        &crate::config::ProfilesConfig::default(),
        &crate::config::CommandsConfig::default(),
        &crate::config::TeamsConfig::default(),
    )
    .expect_err("reject fan-out typo");

    assert!(
        err.to_string().contains("rimz agents claude,codex"),
        "{err:#}"
    );
}
