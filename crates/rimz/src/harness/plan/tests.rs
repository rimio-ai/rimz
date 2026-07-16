use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::*;
use crate::agents::AgentStatus;
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
        append_system_prompt_file: None,
        args: None,
    }
}

fn agent_cell_with_role(role: Option<&str>) -> Cell {
    Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: role.map(|role| format!("{role}-profile")),
        role: role.map(ToOwned::to_owned),
        model: None,
        effort: None,
        budget: None,
    })
}

fn assert_arg_pair(argv: &[String], flag: &str, value: &str) {
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value),
        "missing `{flag} {value}` in {argv:?}"
    );
}

fn preset_cell(kind: &str, args: &[&str], model: Option<&str>, effort: Option<&str>) -> Cell {
    Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked(kind),
        args: args.iter().map(|value| (*value).to_owned()).collect(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some(format!("{kind}-coder")),
        role: None,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        budget: None,
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
    append_system_prompt_file: Option<PathBuf>,
    args: Option<&str>,
) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode,
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        budget: None,
        system_prompt_file,
        append_system_prompt_file,
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
    let resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"))
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
            "append-system-prompt-file",
        ),
    ] {
        machine.agents.profiles.0.insert(
            "planner".to_owned(),
            configured_profile("claude", None, None, None, system, append, None),
        );
        let launch = effective_launch(&machine, dir.path());
        let resolved = resolve_launch(&launch, &machine.agents.commands, Some("planner"))
            .expect("resolve missing prompt profile");
        let err =
            validate_profile_prompt_files(&resolved.layout).expect_err("missing prompt fails");
        assert!(err.to_string().contains(fragment), "{err}");
    }
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

    let mut resolved =
        resolve_launch(&launch, &machine.agents.commands, Some("planner")).expect("resolve launch");
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
            mode,
            model,
            effort,
            budget,
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
    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("asked,codex"))
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
        .map(|cell| cell.mode)
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        [Some(PermissionMode::Ask), Some(PermissionMode::Yolo)]
    );

    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("claude"))
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
    let [Cell::Agent(AgentCell { args, model, .. })] = resolved.layout.columns[0].rows.as_slice()
    else {
        panic!("one agent")
    };
    assert_eq!(args, &["--permission-mode", "auto", "--max-turns", "3"]);
    assert_eq!(model, &None);

    let mut resolved = resolve_launch(&launch, &machine.agents.commands, Some("codex"))
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
    let auto_args = crate::agents::find_adapter("codex")
        .expect("codex")
        .permission_args(PermissionMode::Auto);
    let cell = |args, mode| {
        Cell::Agent(AgentCell {
            kind: AgentKind::new_unchecked("codex"),
            args,
            mode,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: Some("codex-coder".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("profile-model".to_owned()),
            effort: Some("medium".to_owned()),
            budget: None,
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
            mode: unset_mode,
            model: unset_model,
            effort: unset_effort,
            ..
        }),
        Cell::Agent(AgentCell {
            args: preset_args,
            mode: preset_mode,
            model: preset_model,
            effort: preset_effort,
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
    let codex_default = crate::agents::find_adapter("codex")
        .expect("codex")
        .default_launch_model()
        .expect("codex default model");
    let explicit = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("codex"),
        args: vec!["--model".to_owned(), "o3".to_owned()],
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: None,
        role: None,
        model: Some("o3".to_owned()),
        effort: None,
        budget: None,
    });
    let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
    layout.columns[0]
        .rows
        .extend([explicit, Cell::agent(AgentKind::new_unchecked("claude"))]);
    finalize(&mut layout, &Default::default(), &[]).expect("finalize launch");
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, model: Some(model), .. })
            if model == &codex_default && args == &["--model", codex_default.as_str()]));
    assert!(matches!(&layout.columns[0].rows[1],
        Cell::Agent(AgentCell { args, model: Some(model), .. })
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
        Cell::Agent(AgentCell { args, model: Some(model), .. })
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
        Cell::Agent(AgentCell { args, model: Some(model), .. })
            if model == "second" && args == &["--debug", "-m", "second"]));
}

#[test]
fn launch_model_override_wins_over_profile_and_args_models() {
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
    assert_eq!(warnings.len(), 2);
    assert!(matches!(&layout.columns[0].rows[0],
        Cell::Agent(AgentCell { args, model: Some(model), .. })
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
    let codex_default = crate::agents::find_adapter("codex")
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
        Cell::Agent(AgentCell { args, effort: None, .. }) if args == &["--effort", "high"]));
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
    let codex_default = crate::agents::find_adapter("codex")
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
        Cell::Agent(AgentCell { model: Some(model), budget: Some(budget), .. })
            if model == "profile" && budget == "$2.00/day"));
    assert!(matches!(bare,
        Cell::Agent(AgentCell { model: Some(model), budget: Some(budget), .. })
            if model == &codex_default && budget == "$2.00/day"));
    assert!(matches!(no_default,
        Cell::Agent(AgentCell { model: None, budget: Some(budget), .. })
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
            append_system_prompt_file: None,
            args: None,
        },
    )]));
    let teams = TeamsConfig(BTreeMap::from([(
        "solo".to_owned(),
        Team {
            roles: vec![role_binding("planner")],
            leader: None,
            layout: None,
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
        mode: Some(PermissionMode::Yolo),
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("codex-coder".to_owned()),
        role: Some("coder".to_owned()),
        model: Some("gpt-5-codex".to_owned()),
        effort: Some("high".to_owned()),
        budget: None,
    }));

    let requests = launch_identity_requests(
        &layout,
        Some("docs"),
        None,
        Some("forge"),
        None,
        Some("design"),
        Some(("draft it", 0)),
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

    let requests =
        launch_identity_requests(&layout, None, Some("my_feature"), None, None, None, None)
            .unwrap();
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Soft("my_feature".to_owned())
    );
    assert_eq!(
        launch_identity_requests(&layout, None, None, None, None, None, None).unwrap()[0].name,
        AgentLaunchName::Mint
    );
    assert!(
        launch_identity_requests(&layout, Some("my_feature"), None, None, None, None, None)
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
    let requests = launch_identity_requests(&inline, None, None, None, None, None, None).unwrap();
    let group = requests[0].launch.launch_group.as_deref().unwrap();
    assert!(group.starts_with("launch_"));
    assert_eq!(requests[1].launch.launch_group.as_deref(), Some(group));
    assert_eq!(requests[0].launch.launch_ordinal, Some(0));
    assert_eq!(requests[1].launch.launch_ordinal, Some(1));

    let single = LayoutSpec::single(agent_cell_with_role(None));
    let requests = launch_identity_requests(&single, None, None, None, None, None, None).unwrap();
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
    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/tmp/project"),
            prompt: Some("lead this"),
            prompt_agent_index: Some(1),
            cleanup_worktree: false,
            in_place: false,
            team: None,
            channel: None,
            resume_seeds: None,
        },
        &[identity("claude", "first"), identity("codex", "leader")],
    )
    .unwrap();

    assert!(
        !panes.columns[0].panes[0]
            .argv
            .iter()
            .any(|arg| arg == "--prompt")
    );
    assert_arg_pair(&panes.columns[1].panes[1].argv, "--prompt", "lead this");
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
        prompt: None,
    };

    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            prompt: Some("fresh prompt"),
            prompt_agent_index: Some(1),
            cleanup_worktree: false,
            in_place: false,
            team: None,
            channel: Some("fallback"),
            resume_seeds: Some(&seeds),
        },
        &[fresh],
    )
    .expect("mixed panes");

    let panes = &panes.columns[0].panes;
    assert_arg_pair(&panes[0].argv, "--resume", "sess-resume");
    assert_arg_pair(&panes[0].argv, "--agent-channel", "fallback");
    assert_eq!(panes[1].argv, vec!["watch"]);
    assert_arg_pair(&panes[2].argv, "--agent-name", "bright-river");
    assert_arg_pair(&panes[2].argv, "--prompt", "fresh prompt");

    let err = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/repo"),
            prompt: None,
            prompt_agent_index: None,
            cleanup_worktree: false,
            in_place: false,
            team: None,
            channel: None,
            resume_seeds: Some(&[CohortSeed::Fresh, CohortSeed::Fresh]),
        },
        &[],
    )
    .expect_err("identity count mismatch");
    assert_eq!(
        err.to_string(),
        "launch plan missing identity for agent cell 0"
    );
}

#[test]
fn pane_command_stamps_cli_identity_and_close_policy() {
    let cell = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("claude-planner".to_owned()),
        role: Some("planner".to_owned()),
        model: Some("claude-sonnet".to_owned()),
        effort: Some("high".to_owned()),
        budget: None,
    });
    let launch = AgentLaunchIdentity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_0123456789abcdef0123456789abcdef"),
        name: "swift-otter".to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams {
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(2),
            ..Default::default()
        },
        run_id: None,
        prompt: None,
    };

    let pane = pane_cmd_with_name(
        &cell,
        PaneCmdOptions {
            rimz_bin: Path::new("/usr/bin/rimz"),
            cwd: Path::new("/tmp/project"),
            prompt: None,
            cleanup_worktree: false,
            in_place: false,
            team: Some("forge"),
            channel: Some("design"),
            launch: Some(&launch),
            resume_seed: None,
        },
    )
    .unwrap();

    for (flag, value) in [
        ("--agent-name", "swift-otter"),
        ("--launch-id", "launch_0123456789abcdef0123456789abcdef"),
        ("--agent-profile", "claude-planner"),
        ("--agent-role", "planner"),
        ("--agent-team", "forge"),
        ("--launch-group", "launch_group_1"),
        ("--launch-ordinal", "2"),
        ("--agent-channel", "design"),
        ("--agent-model", "claude-sonnet"),
        ("--agent-effort", "high"),
    ] {
        assert_arg_pair(&pane.argv, flag, value);
    }
    assert!(pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));

    for (cleanup_worktree, in_place) in [(false, true), (true, false)] {
        let pane = pane_cmd_with_name(
            &cell,
            PaneCmdOptions {
                rimz_bin: Path::new("/usr/bin/rimz"),
                cwd: Path::new("/tmp/project"),
                prompt: None,
                cleanup_worktree,
                in_place,
                team: Some("forge"),
                channel: None,
                launch: Some(&launch),
                resume_seed: None,
            },
        )
        .unwrap();
        assert!(!pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));
    }
}

#[test]
fn pane_command_resume_replays_prior_identity_without_launch_preset() {
    let cell = Cell::Agent(AgentCell {
        kind: AgentKind::new_unchecked("claude"),
        args: vec!["--ignored".to_owned()],
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("new-profile".to_owned()),
        role: Some("new-role".to_owned()),
        model: Some("new-model".to_owned()),
        effort: Some("new-effort".to_owned()),
        budget: None,
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
    let seed = CohortSeed::Resume(Box::new(agent));

    let pane = pane_cmd_with_name(
        &cell,
        PaneCmdOptions {
            rimz_bin: Path::new("/usr/bin/rimz"),
            cwd: Path::new("/tmp/project"),
            prompt: Some("ignored prompt"),
            cleanup_worktree: false,
            in_place: false,
            team: Some("new-team"),
            channel: Some("new-channel"),
            launch: None,
            resume_seed: Some(&seed),
        },
    )
    .unwrap();

    for (flag, value) in [
        ("--resume", "sess-1"),
        ("--agent-name", "swift-otter"),
        ("--agent-profile", "prior-profile"),
        ("--agent-role", "prior-role"),
        ("--agent-team", "forge"),
        ("--launch-group", "launch_group_1"),
        ("--launch-ordinal", "1"),
        ("--agent-channel", "design"),
    ] {
        assert_arg_pair(&pane.argv, flag, value);
    }
    assert!(pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));
    assert!(!pane.argv.iter().any(|arg| matches!(
        arg.as_str(),
        "--agent-model" | "--agent-effort" | "--prompt"
    )));
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
