use super::exec::*;
use super::launch::*;
use super::*;
use clap::Parser;
use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
use rimz::config::LaunchPlacement;
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::spec::Column;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, ViewId, WorkspaceId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Parser)]
struct ExecHarness {
    #[command(subcommand)]
    command: AgentsSubcmd,
}

#[derive(Debug, Parser)]
struct AgentsHarness {
    #[command(flatten)]
    args: AgentsArgs,
}

fn only_agent(layout: &LayoutSpec) -> (&[String], Option<PermissionMode>) {
    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, mode, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    (args, *mode)
}

fn only_agent_args_and_model(layout: &LayoutSpec) -> (&[String], Option<&str>) {
    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [Cell::Agent { args, model, .. }] = column.rows.as_slice() else {
        panic!("single agent cell");
    };
    (args, model.as_deref())
}

fn only_agent_args_model_effort(layout: &LayoutSpec) -> (&[String], Option<&str>, Option<&str>) {
    let [column] = layout.columns.as_slice() else {
        panic!("single column");
    };
    let [
        Cell::Agent {
            args,
            model,
            effort,
            ..
        },
    ] = column.rows.as_slice()
    else {
        panic!("single agent cell");
    };
    (args, model.as_deref(), effort.as_deref())
}

fn role_binding(role: &str) -> rimz::config::RoleBinding {
    rimz::config::RoleBinding {
        role: role.to_owned(),
        profile: format!("{role}-profile"),
        mode: None,
        model: None,
        effort: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        args: None,
    }
}

fn agent_cell_with_role(role: Option<&str>) -> Cell {
    Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: role.map(|role| format!("{role}-profile")),
        role: role.map(ToOwned::to_owned),
        model: None,
        effort: None,
    }
}

fn agent_profile(
    system_prompt_file: Option<&Path>,
    append_system_prompt_file: Option<&Path>,
) -> rimz::config::ProfilesConfig {
    let mut profiles = rimz::config::ProfilesConfig::default();
    profiles.0.insert(
        "planner".to_owned(),
        rimz::config::Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: system_prompt_file.map(Path::to_path_buf),
            append_system_prompt_file: append_system_prompt_file.map(Path::to_path_buf),
            args: None,
        },
    );
    profiles
}

fn assert_arg_pair(argv: &[String], flag: &str, value: &str) {
    assert!(
        argv.windows(2)
            .any(|args| args[0] == flag && args[1] == value),
        "{flag} {value} missing from {argv:?}"
    );
}

mod parse {
    use super::*;

    #[test]
    fn accepted_agent_forms() {
        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude,codex+term",
            "fix the tests",
            "--worktree=docs",
            "--bg",
        ])
        .expect("parse agents launch");
        assert!(parsed.args.command.is_none());
        assert_eq!(parsed.args.spec.as_deref(), Some("claude,codex+term"));
        assert_eq!(parsed.args.prompt.as_deref(), Some("fix the tests"));
        assert_eq!(parsed.args.worktree.as_deref(), Some("docs"));
        assert!(parsed.args.bg);

        let parsed = AgentsHarness::try_parse_from(["rimz", "peer", "--worktree", "docs"])
            .expect("parse space-separated worktree");
        assert_eq!(parsed.args.spec.as_deref(), Some("peer"));
        assert_eq!(parsed.args.worktree.as_deref(), Some("docs"));
        assert!(parsed.args.prompt.is_none());

        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "codex",
            "--from-pr",
            "https://gitlab.com/org/repo/-/merge_requests/12",
            "--worktree",
            "review-12",
        ])
        .expect("parse from-pr launch");
        assert_eq!(parsed.args.spec.as_deref(), Some("codex"));
        assert_eq!(parsed.args.worktree.as_deref(), Some("review-12"));
        assert_eq!(
            parsed.args.from_pr,
            Some(rimz::forge::PrTarget {
                number: 12,
                forge: Some(rimz::forge::Forge::GitLab)
            })
        );

        let parsed = AgentsHarness::try_parse_from(["rimz", "list", "--json"]).expect("parse list");
        assert!(matches!(
            parsed.args.command,
            Some(AgentsSubcmd::List { json: true, .. })
        ));

        let parsed = AgentsHarness::try_parse_from(["rimz", "--json"]).expect("parse bare json");
        assert!(parsed.args.command.is_none());
        assert!(parsed.args.spec.is_none());
        assert!(parsed.args.json);

        let parsed =
            AgentsHarness::try_parse_from(["rimz", "show", "swift-otter", "--capture", "--ansi"])
                .expect("parse show capture ansi");
        assert!(matches!(
            parsed.args.command,
            Some(AgentsSubcmd::Show {
                capture: true,
                ansi: true,
                ..
            })
        ));

        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "hi",
            "--model",
            "opus",
            "--description",
            "port auth",
            "--effort",
            "high",
            "--system-prompt-file",
            "/abs/prompt.md",
            "--append-system-prompt-file",
            "/abs/append.md",
            "-p",
            "--max-turns",
            "3",
            "-n",
            "swift-otter",
        ])
        .expect("parse shared launch params");
        assert_eq!(parsed.args.model.as_deref(), Some("opus"));
        assert_eq!(parsed.args.description.as_deref(), Some("port auth"));
        assert_eq!(parsed.args.effort.as_deref(), Some("high"));
        assert_eq!(
            parsed.args.system_prompt_file.as_deref(),
            Some(Path::new("/abs/prompt.md"))
        );
        assert_eq!(
            parsed.args.append_system_prompt_file.as_deref(),
            Some(Path::new("/abs/append.md"))
        );
        assert_eq!(parsed.args.max_turns, Some(3));
        assert_eq!(parsed.args.name.as_deref(), Some("swift-otter"));

        let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "hi", "-p", "--json"])
            .expect("parse print json");
        assert!(parsed.args.print);
        assert!(parsed.args.json);

        let parsed = AgentsHarness::try_parse_from(["rimz", "pcr", "--resume"])
            .expect("parse cohort resume");
        assert!(parsed.args.resume);

        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "pcr",
            "--worktree=restore-living-team",
            "--resume",
        ])
        .expect("parse worktree-scoped cohort resume");
        assert!(parsed.args.resume);
        assert_eq!(parsed.args.worktree.as_deref(), Some("restore-living-team"));
    }

    #[test]
    fn rejected_conflicting_forms() {
        let err = AgentsHarness::try_parse_from(["rimz", "list", "--all", "--worktree", "docs"])
            .expect_err("all worktrees and one worktree conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

        let err = AgentsHarness::try_parse_from(["rimz", "show", "swift-otter", "--ansi"])
            .expect_err("ansi requires capture");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "hi",
            "-p",
            "--output-format",
            "stream-json",
        ])
        .expect("parse output-format");
        assert_eq!(parsed.args.output_format, Some(OutputFormat::StreamJson));

        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "-p",
            "--input-format",
            "stream-json",
        ])
        .expect("parse input-format");
        assert_eq!(parsed.args.input_format, Some(InputFormat::StreamJson));

        for argv in [
            &["rimz", "claude", "hi", "-p", "--stream"][..],
            &["rimz", "claude", "hi", "--output-format", "json"],
            &["rimz", "claude", "hi", "--max-turns", "3"],
            &["rimz", "wait", "codex", "--from-start"],
            &["rimz", "wait", "codex", "--stream", "--json"],
            &["rimz", "claude", "--new-pane", "--new-tab"],
            &["rimz", "claude", "hi", "--resume"],
            &["rimz", "claude", "--resume", "--channel=design"],
            &["rimz", "claude", "--resume", "--from-pr", "1"],
            &["rimz", "claude", "--resume", "--name", "swift-otter"],
            &["rimz", "claude", "--resume", "--description", "work"],
            &["rimz", "claude", "--resume", "--model", "opus"],
            &["rimz", "claude", "--resume", "--effort", "high"],
            &["rimz", "claude", "--resume", "--ask"],
            &["rimz", "claude", "--resume", "--yolo"],
            &["rimz", "claude", "--resume", "--system-prompt-file", "/x"],
            &[
                "rimz",
                "claude",
                "--resume",
                "--append-system-prompt-file",
                "/x",
            ],
            &["rimz", "claude", "--resume", "-p"],
            &["rimz", "claude", "--resume", "--", "--debug"],
        ] {
            assert!(
                AgentsHarness::try_parse_from(argv.iter().copied()).is_err(),
                "{argv:?} should fail"
            );
        }
    }

    #[test]
    fn launch_options_require_spec() {
        for (argv, fragment) in [
            (&["rimz", "--worktree=docs"][..], "--worktree requires"),
            (&["rimz", "--from-pr", "1"], "--from-pr requires"),
            (&["rimz", "--", "term"], "missing agent spec"),
            (&["rimz", "--model", "opus"], "require an agent spec"),
            (&["rimz", "--new-pane"], "require an agent spec"),
            (&["rimz", "--resume"], "require an agent spec"),
            (&["rimz", "-p", "--max-turns", "3"], "require an agent spec"),
        ] {
            let parsed = AgentsHarness::try_parse_from(argv.iter().copied()).expect("parse flag");
            let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject flag");
            assert!(err.to_string().contains(fragment), "{err:#}");
        }
    }

    #[test]
    fn exec_subcommand_forms() {
        let parsed = ExecHarness::try_parse_from([
            "rimz",
            "exec",
            "codex",
            "--run-id",
            "run_0123456789abcdef0123456789abcdef",
            "--agent-name",
            "lucid-atlas",
            "--agent-role",
            "coder",
            "--agent-team",
            "pcr",
            "--launch-group",
            "launch_group_1",
            "--launch-ordinal",
            "2",
            "--agent-channel",
            "design",
            "--agent-model",
            "gpt-5.5",
            "--agent-effort",
            "xhigh",
            "--launch-id",
            "launch_0123456789abcdef0123456789abcdef",
            "--exit-on-run-completion",
            "--close-pane-on-exit",
            "--worktree-path",
            "/x",
            "--prompt",
            "hi",
            "--",
            "--model",
            "gpt-5-codex",
        ])
        .expect("parse exec");
        let AgentsSubcmd::Exec(args) = parsed.command else {
            panic!("expected exec subcommand");
        };
        assert_eq!(args.kind, "codex");
        assert_eq!(
            args.run_id.as_ref().map(rimz::RunId::as_str),
            Some("run_0123456789abcdef0123456789abcdef")
        );
        assert_eq!(args.agent_name.as_deref(), Some("lucid-atlas"));
        assert_eq!(args.agent_role.as_deref(), Some("coder"));
        assert_eq!(args.agent_team.as_deref(), Some("pcr"));
        assert_eq!(args.launch_group.as_deref(), Some("launch_group_1"));
        assert_eq!(args.launch_ordinal, Some(2));
        assert_eq!(args.agent_channel.as_deref(), Some("design"));
        assert_eq!(args.agent_model.as_deref(), Some("gpt-5.5"));
        assert_eq!(args.agent_effort.as_deref(), Some("xhigh"));
        assert_eq!(
            args.launch_id.as_deref(),
            Some("launch_0123456789abcdef0123456789abcdef")
        );
        assert!(args.exit_on_run_completion);
        assert!(args.close_pane_on_exit);
        assert_eq!(args.worktree_path, Some(PathBuf::from("/x")));
        assert_eq!(args.prompt.as_deref(), Some("hi"));
        assert_eq!(args.extra_args, ["--model", "gpt-5-codex"]);

        let parsed = ExecHarness::try_parse_from(["rimz", "exec", "claude", "--resume", "sess-1"])
            .expect("parse resume");
        let AgentsSubcmd::Exec(args) = parsed.command else {
            panic!("expected exec subcommand");
        };
        assert_eq!(args.kind, "claude");
        assert_eq!(args.resume.as_deref(), Some("sess-1"));

        assert!(
            ExecHarness::try_parse_from([
                "rimz", "exec", "claude", "--resume", "sess-1", "--prompt", "hi",
            ])
            .is_err(),
            "resume and launch prompt conflict"
        );
    }

    #[test]
    fn prompt_that_looks_like_another_spec_errors() {
        let profiles = rimz::config::ProfilesConfig::default();
        let commands = rimz::config::CommandsConfig::default();
        let layouts = rimz::config::TeamsConfig::default();
        let err = reject_prompt_that_looks_like_spec(
            Some("claude"),
            Some("codex"),
            &profiles,
            &commands,
            &layouts,
        )
        .expect_err("reject fan-out");
        assert!(
            err.to_string()
                .contains("did you mean `rimz agents claude,codex`"),
            "{err:#}"
        );
    }
}

mod placement {
    use super::*;

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
        let profiles = rimz::config::ProfilesConfig(BTreeMap::from([(
            "planner-profile".to_owned(),
            rimz::config::Profile {
                agent: "codex".to_owned(),
                mode: None,
                model: None,
                effort: None,
                system_prompt_file: None,
                append_system_prompt_file: None,
                args: None,
            },
        )]));
        let teams = rimz::config::TeamsConfig(BTreeMap::from([(
            "solo".to_owned(),
            rimz::config::Team {
                roles: vec![role_binding("planner")],
                layout: None,
            },
        )]));

        for spec in ["solo", "solo.planner"] {
            let layout = rimz::harness::spec::resolve_spec(
                Some(spec),
                &profiles,
                &rimz::config::CommandsConfig::default(),
                &teams,
            )
            .expect("single-role team launch");
            let single_cell = layout.agent_cells().count() == 1;
            let team_name = rimz::harness::spec::spec_team(spec, &teams);

            let placement = apply_in_place_downgrade(
                resolve_placement(
                    false,
                    false,
                    LaunchPlacement::Auto,
                    false,
                    single_cell,
                    true,
                )
                .unwrap(),
                false,
                true,
            );

            assert_eq!(team_name, Some("solo"));
            assert_eq!(placement, Placement::SamePane);
        }
    }

    #[test]
    fn supervised_run_placement_matrix() {
        for (force_new_tab, has_ambient_pane, expected) in [
            (false, true, RunPlacement::Split),
            (true, true, RunPlacement::Tab),
            (false, false, RunPlacement::Tab),
        ] {
            assert_eq!(
                run_placement(force_new_tab, has_ambient_pane),
                expected,
                "force_new_tab={force_new_tab}, has_ambient_pane={has_ambient_pane}"
            );
        }
    }
}

mod launch_options {
    use super::*;

    #[test]
    fn prompt_file_flags_resolve_and_reject_bad_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "be concise").expect("write prompt");
        let append = dir.path().join("append.md");
        std::fs::write(&append, "follow project rules").expect("write append");

        for (flag, file) in [
            ("--system-prompt-file", prompt.as_path()),
            ("--append-system-prompt-file", append.as_path()),
        ] {
            let parsed = AgentsHarness::try_parse_from([
                "rimz",
                "claude",
                "hi",
                "--model",
                "opus",
                flag,
                file.to_str().expect("utf8 file path"),
            ])
            .expect("parse prompt flag");
            let preset = launch_override_preset(&parsed.args).expect("resolve prompt file");
            let canonical = file.canonicalize().expect("canonical file");
            match flag {
                "--system-prompt-file" => {
                    assert_eq!(preset.model.as_deref(), Some("opus"));
                    assert_eq!(
                        preset.system_prompt_file.as_deref(),
                        Some(canonical.as_path())
                    );
                }
                "--append-system-prompt-file" => {
                    assert_eq!(
                        preset.append_system_prompt_file.as_deref(),
                        Some(canonical.as_path())
                    );
                }
                _ => unreachable!(),
            }
        }

        for flag in ["--system-prompt-file", "--append-system-prompt-file"] {
            let parsed = AgentsHarness::try_parse_from([
                "rimz",
                "claude",
                "hi",
                flag,
                dir.path().to_str().expect("utf8 dir path"),
            ])
            .expect("parse prompt directory");
            let err = launch_override_preset(&parsed.args).expect_err("reject a directory");
            assert!(err.to_string().contains("is not a regular file"), "{err:#}");
        }

        let missing = dir.path().join("missing-append.md");
        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "hi",
            "--append-system-prompt-file",
            missing.to_str().expect("utf8 missing path"),
        ])
        .expect("parse missing append prompt");
        let err = launch_override_preset(&parsed.args).expect_err("reject missing append path");
        assert!(
            err.to_string()
                .contains("reading --append-system-prompt-file"),
            "{err:#}"
        );
    }

    #[test]
    fn profile_launch_requires_its_system_prompt_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let present = dir.path().join("planner.md");
        std::fs::write(&present, "be terse").expect("write prompt");
        let present_append = dir.path().join("append.md");
        std::fs::write(&present_append, "follow house style").expect("write append prompt");

        let profiles = agent_profile(Some(&present), Some(&present_append));
        let layout = rimz::harness::spec::resolve_spec(
            Some("planner"),
            &profiles,
            &rimz::config::CommandsConfig::default(),
            &rimz::config::TeamsConfig::default(),
        )
        .expect("resolve planner profile");
        ensure_profile_prompt_files(&layout).expect("present prompt files pass");

        for (system_prompt_file, append_system_prompt_file, fragment) in [
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
            let layout = rimz::harness::spec::resolve_spec(
                Some("planner"),
                &agent_profile(
                    system_prompt_file.as_deref(),
                    append_system_prompt_file.as_deref(),
                ),
                &rimz::config::CommandsConfig::default(),
                &rimz::config::TeamsConfig::default(),
            )
            .expect("resolve missing prompt profile");
            let err = ensure_profile_prompt_files(&layout).expect_err("missing prompt fails");
            assert!(err.to_string().contains(fragment), "{err:#}");
        }
    }

    #[test]
    fn mode_application_matrix() {
        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
        apply_launch_mode_and_passthrough(
            &mut layout,
            interactive_permission_mode_from_flags(false, false).unwrap(),
            &rimz::agents::LaunchPreset::default(),
            &[],
        )
        .expect("apply native interactive mode");
        let (args, mode) = only_agent(&layout);
        assert!(args.is_empty());
        assert_eq!(mode, None);

        let mut layout = LayoutSpec::single(Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: vec!["--model".to_owned(), "gpt-5-codex".to_owned()],
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: None,
            role: None,
            model: None,
            effort: None,
        });
        apply_launch_mode_and_passthrough(
            &mut layout,
            interactive_permission_mode_from_flags(false, true).unwrap(),
            &rimz::agents::LaunchPreset::default(),
            &[],
        )
        .expect("apply explicit interactive mode");
        let (args, mode) = only_agent(&layout);
        assert_eq!(mode, Some(PermissionMode::Yolo));
        assert!(
            args.iter()
                .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
        );

        let yolo_args = rimz::agents::find_adapter("codex")
            .expect("codex")
            .permission_args(PermissionMode::Yolo);
        let mut layout = LayoutSpec::single(Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: yolo_args.clone(),
            mode: Some(PermissionMode::Yolo),
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: None,
            role: None,
            model: None,
            effort: None,
        });
        apply_launch_mode_and_passthrough(
            &mut layout,
            Some(PermissionMode::Auto),
            &rimz::agents::LaunchPreset::default(),
            &[],
        )
        .expect("preserve virtual/profile mode");
        let (args, mode) = only_agent(&layout);
        assert_eq!(args, &yolo_args);
        assert_eq!(mode, Some(PermissionMode::Yolo));

        let auto_args = rimz::agents::find_adapter("claude")
            .expect("claude")
            .permission_args(PermissionMode::Auto);
        let mut layout = LayoutSpec::single(Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: auto_args.clone(),
            mode: Some(PermissionMode::Auto),
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: None,
            role: None,
            model: None,
            effort: None,
        });
        apply_launch_mode_and_passthrough(
            &mut layout,
            Some(PermissionMode::Yolo),
            &rimz::agents::LaunchPreset::default(),
            &[],
        )
        .expect("explicit mode does not overwrite existing mode");
        let (args, mode) = only_agent(&layout);
        assert_eq!(args, &auto_args);
        assert_eq!(mode, Some(PermissionMode::Auto));
    }

    #[test]
    fn launch_override_preset_replaces_cell_model_and_effort_identity() {
        let mut layout = LayoutSpec::single(Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: vec!["--model".to_owned(), "profile-model".to_owned()],
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: Some("codex-coder".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("profile-model".to_owned()),
            effort: Some("medium".to_owned()),
        });

        apply_launch_mode_and_passthrough(
            &mut layout,
            None,
            &rimz::agents::LaunchPreset {
                model: Some("override-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                ..rimz::agents::LaunchPreset::default()
            },
            &[],
        )
        .expect("apply launch options");

        let (args, model, effort) = only_agent_args_model_effort(&layout);
        assert!(args.contains(&"override-model".to_owned()), "{args:?}");
        assert!(args.iter().any(|arg| arg.contains("xhigh")), "{args:?}");
        assert_eq!(model, Some("override-model"));
        assert_eq!(effort, Some("xhigh"));
    }

    #[test]
    fn default_launch_models_stamp_only_cells_without_models() {
        let codex_default = rimz::agents::find_adapter("codex")
            .expect("codex")
            .default_launch_model()
            .expect("codex default model");

        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
        apply_default_launch_models(&mut layout).expect("codex default model");
        let (args, model) = only_agent_args_and_model(&layout);
        assert_eq!(model, Some(codex_default.as_str()));
        assert_eq!(args, &["--model", codex_default.as_str()]);

        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
        apply_default_launch_models(&mut layout).expect("claude has no default model");
        let (args, model) = only_agent_args_and_model(&layout);
        assert_eq!(model, None);
        assert!(args.is_empty());

        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
        apply_launch_mode_and_passthrough(
            &mut layout,
            None,
            &rimz::agents::LaunchPreset {
                model: Some("o3".to_owned()),
                ..Default::default()
            },
            &[],
        )
        .expect("explicit model preset");
        apply_default_launch_models(&mut layout).expect("skip explicit model");
        let (args, model) = only_agent_args_and_model(&layout);
        assert_eq!(model, Some("o3"));
        assert_eq!(args, &["--model", "o3"]);
    }

    #[test]
    fn supervised_turn_limit_renders_supported_adapter_and_fails_fast() {
        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")));
        apply_supervised_turn_limit(&mut layout, 3).expect("claude supports max turns");
        let (args, _) = only_agent(&layout);
        assert_eq!(args, &["--max-turns", "3"]);

        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
        let err = apply_supervised_turn_limit(&mut layout, 3).expect_err("codex rejects max turns");
        assert!(
            err.to_string()
                .contains("codex does not support --max-turns"),
            "{err:#}"
        );
    }
}

mod identity {
    use super::*;

    #[test]
    fn launch_request_names_and_metadata() {
        let layout = LayoutSpec::single(Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: Vec::new(),
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: Some("codex-coder".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
        });

        let requests = launch_identity_requests(
            &layout,
            Some("docs"),
            None,
            Some("pcr"),
            None,
            Some("design"),
        )
        .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].name,
            AgentLaunchName::Explicit("docs".to_owned())
        );
        assert_eq!(requests[0].kind.as_str(), "codex");
        assert_eq!(requests[0].profile.as_deref(), Some("codex-coder"));
        assert_eq!(requests[0].role.as_deref(), Some("coder"));
        assert_eq!(requests[0].model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(requests[0].effort.as_deref(), Some("high"));
        assert_eq!(requests[0].team.as_deref(), Some("pcr"));
        assert_eq!(requests[0].channel.as_deref(), Some("design"));

        let requests =
            launch_identity_requests(&layout, None, Some("my_feature"), None, None, None).unwrap();
        assert_eq!(
            requests[0].name,
            AgentLaunchName::Soft("my_feature".to_owned())
        );

        let requests = launch_identity_requests(&layout, None, None, None, None, None).unwrap();
        assert_eq!(requests[0].name, AgentLaunchName::Mint);

        assert!(
            launch_identity_requests(&layout, Some("my_feature"), None, None, None, None)
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
            Some("pcr"),
            Some(&team_roles),
            None,
        )
        .unwrap();
        assert_eq!(requests[0].role.as_deref(), Some("coder"));
        assert_eq!(requests[0].launch_ordinal, Some(1));
        assert_eq!(requests[1].role.as_deref(), Some("planner"));
        assert_eq!(requests[1].launch_ordinal, Some(0));
        assert_eq!(requests[2].role, None);
        assert_eq!(requests[2].launch_ordinal, None);
        assert!(
            requests
                .iter()
                .all(|request| request.launch_group.is_none())
        );

        let single_role = LayoutSpec::single(agent_cell_with_role(Some("coder")));
        let requests = launch_identity_requests(
            &single_role,
            None,
            None,
            Some("pcr"),
            Some(&team_roles),
            None,
        )
        .unwrap();
        assert_eq!(requests[0].launch_ordinal, Some(1));

        let inline = LayoutSpec {
            columns: vec![Column {
                rows: vec![agent_cell_with_role(None), agent_cell_with_role(None)],
                stacked: false,
            }],
        };
        let requests = launch_identity_requests(&inline, None, None, None, None, None).unwrap();
        let group = requests[0]
            .launch_group
            .as_deref()
            .expect("inline launch group");
        assert!(group.starts_with("launch_"));
        assert_eq!(requests[1].launch_group.as_deref(), Some(group));
        assert_eq!(requests[0].launch_ordinal, Some(0));
        assert_eq!(requests[1].launch_ordinal, Some(1));

        let single = LayoutSpec::single(agent_cell_with_role(None));
        let requests = launch_identity_requests(&single, None, None, None, None, None).unwrap();
        assert_eq!(requests[0].launch_group, None);
        assert_eq!(requests[0].launch_ordinal, None);
    }
}

mod pane_exec {
    use super::*;

    #[test]
    fn pane_command_stamps_cli_identity_and_close_policy() {
        let cell = Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: Vec::new(),
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: Some("claude-planner".to_owned()),
            role: Some("planner".to_owned()),
            model: Some("claude-sonnet".to_owned()),
            effort: Some("high".to_owned()),
        };
        let launch = LaunchIdentity {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_0123456789abcdef0123456789abcdef"),
            name: "swift-otter".to_owned(),
            profile: None,
            role: None,
            model: None,
            effort: None,
            team: None,
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(2),
            channel: None,
            run_id: None,
        };

        let pane = pane_cmd_with_name(
            &cell,
            PaneCmdOptions {
                rimz_bin: Path::new("/usr/bin/rimz"),
                cwd: Path::new("/tmp/project"),
                prompt: None,
                cleanup_worktree: false,
                in_place: false,
                team: Some("pcr"),
                channel: Some("design"),
                launch: Some(&launch),
                resume_seed: None,
            },
        )
        .expect("pane command");

        for (flag, value) in [
            ("--agent-name", "swift-otter"),
            ("--launch-id", "launch_0123456789abcdef0123456789abcdef"),
            ("--agent-profile", "claude-planner"),
            ("--agent-role", "planner"),
            ("--agent-team", "pcr"),
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
                    team: Some("pcr"),
                    channel: None,
                    launch: Some(&launch),
                    resume_seed: None,
                },
            )
            .expect("pane command without close");
            assert!(
                !pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"),
                "{pane:?}"
            );
        }
    }

    #[test]
    fn pane_command_resume_replays_prior_identity_without_launch_preset() {
        let cell = Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--ignored".to_owned()],
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: Some("new-profile".to_owned()),
            role: Some("new-role".to_owned()),
            model: Some("new-model".to_owned()),
            effort: Some("new-effort".to_owned()),
        };
        let mut agent = agent_with_status(
            "sess-1",
            rimz::agents::AgentStatus::Idle,
            rimz::agents::TurnPhase::Idle,
            0,
        );
        agent.name = Some("swift-otter".to_owned());
        agent.profile = Some("prior-profile".to_owned());
        agent.role = Some("prior-role".to_owned());
        agent.team = Some("pcr".to_owned());
        agent.launch_group = Some("launch_group_1".to_owned());
        agent.launch_ordinal = Some(1);
        agent.channel = Some("design".to_owned());
        agent.model = Some("old-model".to_owned());
        agent.effort = Some("old-effort".to_owned());
        let seed = rimz::harness::resume::CohortSeed::Resume(Box::new(agent));

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
        .expect("resume pane command");

        for (flag, value) in [
            ("--resume", "sess-1"),
            ("--agent-name", "swift-otter"),
            ("--agent-profile", "prior-profile"),
            ("--agent-role", "prior-role"),
            ("--agent-team", "pcr"),
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
    fn wrapper_lifetime_matrix() {
        let mut run_owned = bare_exec_args();
        run_owned.run_id = Some(rimz::RunId::new());
        let mut worktree_owned = bare_exec_args();
        worktree_owned.worktree_path = Some(PathBuf::from("/tmp/rimz-worktree"));
        let mut completion_owned = bare_exec_args();
        completion_owned.exit_on_run_completion = true;
        completion_owned.close_pane_on_exit = true;
        let mut close_owned = bare_exec_args();
        close_owned.close_pane_on_exit = true;

        for (name, args, direct, record_end_trace) in [
            ("bare", bare_exec_args(), cfg!(unix), true),
            ("run", run_owned, false, true),
            ("worktree", worktree_owned, false, true),
            ("completion", completion_owned, false, false),
            ("close", close_owned, false, true),
        ] {
            assert_eq!(should_exec_agent_directly(&args), direct, "{name}");
            assert_eq!(should_record_end_trace(&args), record_end_trace, "{name}");
        }
    }

    #[test]
    fn drop_to_shell_gate_matches_interactive_rimz_owned_panes() {
        let args = bare_exec_args();
        assert!(!should_drop_to_shell(&args, false));

        let mut args = bare_exec_args();
        args.close_pane_on_exit = true;
        assert!(should_drop_to_shell(&args, false));
        assert!(!should_drop_to_shell(&args, true));

        let mut args = bare_exec_args();
        args.worktree_path = Some(PathBuf::from("/tmp/rimz-worktree"));
        assert!(should_drop_to_shell(&args, false));

        let mut args = bare_exec_args();
        args.close_pane_on_exit = true;
        args.run_id = Some(rimz::RunId::new());
        assert!(!should_drop_to_shell(&args, false));
    }

    #[test]
    fn relaunch_command_prefers_team_role_then_profile_then_kind() {
        let mut args = bare_exec_args();
        args.agent_team = Some("trim".to_owned());
        args.agent_role = Some("pruner".to_owned());
        args.agent_profile = Some("codex-plan".to_owned());
        assert_eq!(relaunch_command(&args), "rimz agents trim.pruner");

        let mut args = bare_exec_args();
        args.agent_profile = Some("codex-plan".to_owned());
        assert_eq!(relaunch_command(&args), "rimz agents codex-plan");

        let args = bare_exec_args();
        assert_eq!(relaunch_command(&args), "rimz agents codex");
    }

    #[test]
    fn exit_hint_reports_startup_failure_and_relaunch_command() {
        let status = exit_status(7);
        let message = exit_hint("codex", &status, true, "rimz agents trim.pruner");

        assert_eq!(
            message,
            format!(
                "rimz: agent `codex` failed to start ({status}); relaunch with `rimz agents trim.pruner`\r\n"
            )
        );
    }

    #[test]
    fn exit_hint_reports_clean_exit_and_relaunch_command() {
        let status = exit_status(0);
        let message = exit_hint("codex", &status, false, "rimz agents codex-plan");

        assert_eq!(
            message,
            format!(
                "rimz: agent `codex` exited ({status}); relaunch with `rimz agents codex-plan`\r\n"
            )
        );
    }

    fn exit_status(code: i32) -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            std::process::ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", &format!("exit {code}")])
                .status()
                .expect("exit status")
        }
    }

    #[test]
    fn close_is_deliberate_keeps_only_unaccepted_abrupt_exits_recoverable() {
        for (abrupt, session_accepts_close, expected) in [
            (false, false, true),
            (false, true, true),
            (true, true, true),
            (true, false, false),
        ] {
            assert_eq!(
                close_is_deliberate(abrupt, session_accepts_close),
                expected,
                "abrupt={abrupt}, session_accepts_close={session_accepts_close}"
            );
        }
    }
}

mod runs {
    use super::*;

    #[test]
    fn run_stop_should_cancel_only_live_runs() {
        for status in [RunStatus::Pending, RunStatus::Running] {
            assert!(run_stop_should_cancel(&run_record_with_status(status)));
        }
        for status in [
            RunStatus::Completed,
            RunStatus::Canceled,
            RunStatus::Failed,
            RunStatus::TimedOut,
        ] {
            assert!(!run_stop_should_cancel(&run_record_with_status(status)));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_exit_marks_nonterminal_run_failed_and_wakes_waiter() {
        let state = tempfile::tempdir().expect("state dir");
        let runtime_root = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .expect("runtime dir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).expect("paths");
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
        paths.ensure_dirs().expect("state dirs");
        runtime.ensure_dirs().expect("runtime dirs");
        let record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "summarize".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let run_id = record.run_id.clone();
        rimz::harness::run::create(&paths, &record).expect("create run");
        let (sock, _sock_path) = rimz::bridge::bind_run(&runtime, &run_id).expect("bind run");
        let context = RunExecContext {
            run_id: run_id.clone(),
            paths: paths.clone(),
            runtime,
            session_name: "rimz-test".to_owned(),
        };

        let globals = GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Auto,
        };

        fail_run_if_child_exited_first(&context, &globals, Duration::ZERO);

        let failed = rimz::harness::run::load(&paths, &run_id).expect("load failed run");
        assert_eq!(failed.status, RunStatus::Failed);
        let outcome = rimz::bridge::wait_for_run_completion_owning(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id,
            },
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("run wait");
        assert_eq!(outcome, RunWakeOutcome::Completed(RunStatus::Failed));
    }
}

mod render {
    use super::*;

    #[test]
    fn cached_agents_reap_uses_published_live_panes_for_runtime_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = rimz::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%1");
        let dead_pane_id = PaneId::from_parts(MuxName::Tmux, "%dead");
        let host_pane_id = PaneId::from_parts(MuxName::Tmux, "%host");
        let mut old = agent_with_status(
            "old",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_000,
        );
        let mut new = agent_with_status(
            "new",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_010,
        );
        for agent in [&mut old, &mut new] {
            agent.kind = AgentKind::new_unchecked("codex");
            agent.worktree_path = Some("/repo/main".to_owned());
            agent.worktree_branch = Some("main".to_owned());
            agent.origin = Some(rimz::agents::SessionOrigin::Fresh);
            agent.pane = Some(rimz::pane::PaneRef::from_id(pane_id.clone()));
        }
        let mut dead_daemon = agent_with_status(
            "dead-daemon",
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            900,
        );
        dead_daemon.kind = AgentKind::new_unchecked("codex");
        dead_daemon.worktree_path = Some("/repo/main".to_owned());
        dead_daemon.pane = Some(rimz::pane::PaneRef::from_id(dead_pane_id));
        dead_daemon.runtime_owner = Some(rimz::RuntimeOwner::new(
            rimz::RuntimeOwnerKind::Agent,
            "dead-daemon",
            77,
            None,
        ));
        let mut host = agent_with_status(
            "host",
            rimz::agents::AgentStatus::Idle,
            rimz::agents::TurnPhase::Idle,
            800,
        );
        host.kind = AgentKind::new_unchecked("claude");
        host.worktree_path = Some("/repo/daemon".to_owned());
        host.pane = Some(rimz::pane::PaneRef::from_id(host_pane_id.clone()));
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            workspace_id,
            Vec::new(),
            vec![old, new, dead_daemon, host],
            jiff::Timestamp::from_second(1_020).unwrap(),
        );
        rimz::sidebar::refresh::write_codex_daemon_reap(
            &runtime,
            &rimz::sidebar::refresh::CodexDaemonReap {
                produced_at_ms: 1,
                daemon_pids: BTreeSet::from([77]),
                loaded: Some(BTreeSet::new()),
            },
        )
        .unwrap();
        let codex_pane = rimz::sidebar::frame::PaneState {
            pane_id: pane_id.clone(),
            first_seen_at_ms: None,
            hosted_carry_since_ms: None,
            is_floating: false,
            current: rimz::sidebar::frame::PaneProcess {
                pid: None,
                command: Some("codex".to_owned()),
                spawn_command: None,
                cwd: Some("/repo/main".to_owned()),
                started_at: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                resumed_session_id: None,
                elevated_agent: None,
            },
            previous: None,
            children: Vec::new(),
            metrics: rimz::sidebar::frame::PaneMetrics::default(),
        };
        let host_pane = rimz::sidebar::frame::PaneState {
            pane_id: host_pane_id,
            current: rimz::sidebar::frame::PaneProcess {
                command: Some("claude".to_owned()),
                spawn_command: Some("claude remote-control --spawn worktree".to_owned()),
                cwd: Some("/repo/daemon".to_owned()),
                ..codex_pane.current.clone()
            },
            ..codex_pane.clone()
        };
        let frame = rimz::sidebar::frame::PaneFrame {
            produced_at_ms: 1,
            observed_at_ms: 1,
            build: None,
            session_name: "rimz-test".to_owned(),
            tabs: vec![rimz::sidebar::frame::TabFrame {
                view_id: ViewId::new_unchecked("window-1"),
                kind: rimz::ViewKind::Window,
                name: None,
                panes: vec![codex_pane, host_pane],
            }],
            carried_panes: Vec::new(),
            viewed_panes: Vec::new(),
            focused_pane: None,
            presence: None,
        };
        rimz::ledger::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame)
            .unwrap();

        super::commands::apply_cached_daemon_reap(&mut snapshot, &runtime, "rimz-test");

        let ids = snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["new"]);
    }

    #[test]
    fn agents_table_projects_turn_error_statuses() {
        let now = jiff::Timestamp::from_second(2_000).unwrap();
        let failed = agent_with_status(
            "failed-sess",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_000,
        )
        .with_turn_error(
            rimz::agents::TurnErrorClass::Failed,
            1_010,
            "API Error: Bad Request",
        );
        let paused = agent_with_status(
            "paused-sess",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_000,
        )
        .with_turn_error(
            rimz::agents::TurnErrorClass::PausedOverloaded,
            1_010,
            "API Error: Overloaded",
        );
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            Vec::new(),
            vec![failed, paused],
            now,
        );
        let agents: Vec<&rimz::agents::AgentState> = snapshot.agents.iter().collect();

        let mut out = anstream::StripStream::new(Vec::new());
        render_agents_table(&mut out, &snapshot, &agents, now).expect("render agents table");
        let text = String::from_utf8(out.into_inner()).expect("utf8");

        assert!(text.contains("failed"), "{text}");
        assert!(text.contains("paused"), "{text}");
        assert!(
            !text.contains("running:reasoning"),
            "turn-error rows drop the stale phase suffix:\n{text}"
        );
    }
}

mod automation {
    use super::*;

    #[test]
    fn create_on_miss_launches_kinds_and_agent_profiles_but_not_commands() {
        let profiles = agent_profile(None, None);

        assert!(is_launchable_type("codex", &profiles));
        assert!(is_launchable_type("planner", &profiles));
        assert!(!is_launchable_type("vim", &profiles));
        assert!(!is_launchable_type("swift-otter", &profiles));
    }

    #[test]
    fn for_task_builds_a_blocking_supervised_turn() {
        let args = AgentsArgs::for_task(TaskRunArgs {
            spec: "claude-ping".to_owned(),
            prompt: Some("ping".to_owned()),
            worktree: Some("main".to_owned()),
            mode: None,
            effort: Some("low".to_owned()),
            system_prompt_file: None,
            timeout: None,
            keep: false,
        });
        assert_eq!(
            args.spec.as_deref(),
            Some("claude-ping"),
            "the spec is carried exactly"
        );
        assert_eq!(args.prompt.as_deref(), Some("ping"), "the prompt is `ping`");
        assert_eq!(
            args.effort.as_deref(),
            Some("low"),
            "lowest effort primes cheaply"
        );
        assert_eq!(
            args.worktree.as_deref(),
            Some("main"),
            "the worktree is carried"
        );
        assert!(args.print, "the ping is a supervised -p run");
        assert!(!args.bg, "a window-priming ping blocks until the turn ends");
        assert!(
            args.passthrough.is_empty(),
            "no passthrough flags are injected"
        );

        assert_eq!(
            AgentsArgs::for_task(TaskRunArgs {
                spec: "codex".to_owned(),
                prompt: Some("check status".to_owned()),
                worktree: None,
                mode: None,
                effort: None,
                system_prompt_file: None,
                timeout: None,
                keep: true,
            })
            .worktree,
            None
        );
        assert!(
            AgentsArgs::for_task(TaskRunArgs {
                spec: "codex".to_owned(),
                prompt: Some("check status".to_owned()),
                worktree: None,
                mode: None,
                effort: None,
                system_prompt_file: None,
                timeout: None,
                keep: true,
            })
            .keep,
            "manual loop fire can keep the transient pane"
        );
    }

    #[test]
    fn virtual_ping_defaults_prompt_unless_explicit_or_stream_json() {
        let mut args = AgentsHarness::try_parse_from(["rimz", "codex-ping"])
            .expect("parse ping")
            .args;
        default_virtual_ping_prompt(&mut args);
        assert_eq!(
            args.prompt.as_deref(),
            Some(rimz::harness::spec::PING_PROMPT)
        );

        let mut explicit = AgentsHarness::try_parse_from(["rimz", "codex-ping", "status"])
            .expect("parse explicit ping")
            .args;
        default_virtual_ping_prompt(&mut explicit);
        assert_eq!(explicit.prompt.as_deref(), Some("status"));

        let mut normal = AgentsHarness::try_parse_from(["rimz", "codex"])
            .expect("parse normal")
            .args;
        default_virtual_ping_prompt(&mut normal);
        assert!(normal.prompt.is_none());

        let mut stream_json = AgentsHarness::try_parse_from([
            "rimz",
            "codex-ping",
            "-p",
            "--input-format",
            "stream-json",
        ])
        .expect("parse stream-json ping")
        .args;
        default_virtual_ping_prompt(&mut stream_json);
        assert!(stream_json.prompt.is_none());

        let mut resume = AgentsHarness::try_parse_from(["rimz", "codex-ping", "--resume"])
            .expect("parse resume ping")
            .args;
        default_virtual_ping_prompt(&mut resume);
        assert!(resume.prompt.is_none());
    }
}

fn bare_exec_args() -> ExecArgs {
    ExecArgs {
        kind: "codex".to_owned(),
        resume: None,
        run_id: None,
        agent_name: Some("lucid-atlas".to_owned()),
        agent_profile: None,
        agent_role: None,
        agent_team: None,
        launch_group: None,
        launch_ordinal: None,
        agent_channel: None,
        agent_model: None,
        agent_effort: None,
        launch_id: Some("launch_0123456789abcdef0123456789abcdef".to_owned()),
        exit_on_run_completion: false,
        close_pane_on_exit: false,
        worktree_path: None,
        prompt: None,
        extra_args: Vec::new(),
    }
}

fn run_record_with_status(status: RunStatus) -> RunRecord {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = status;
    record
}

trait AgentTurnErrorFixture {
    fn with_turn_error(self, class: rimz::agents::TurnErrorClass, at: i64, label: &str) -> Self;
}

impl AgentTurnErrorFixture for rimz::agents::AgentState {
    fn with_turn_error(
        mut self,
        class: rimz::agents::TurnErrorClass,
        at: i64,
        label: &str,
    ) -> Self {
        self.context = Some(rimz::agents::AgentContext {
            source: self.kind.to_string(),
            session_name: None,
            session_preview: None,
            model_id: None,
            model_display_name: None,
            effort: None,
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: None,
            exceeds_200k_tokens: None,
            cost: None,
            tokens: None,
            rate_limits: None,
            pr: None,
            account: None,
            turn_error: Some(rimz::agents::AgentTurnError {
                class,
                at: jiff::Timestamp::from_second(at).unwrap(),
                label: Some(label.to_owned()),
            }),
            turn_complete: None,
            observed_at: jiff::Timestamp::from_second(at).unwrap(),
        });
        self
    }
}

fn agent_with_status(
    id: &str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    activity: i64,
) -> rimz::agents::AgentState {
    let at = jiff::Timestamp::from_second(activity).unwrap();
    rimz::agents::AgentState {
        agent_id: AgentSessionId::from(id),
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: None,
        status,
        phase,
        pane: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: Some("/tmp/rimz-agents-table".to_owned()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: at,
        last_activity: at,
        registered_at: Some(at),
    }
}
