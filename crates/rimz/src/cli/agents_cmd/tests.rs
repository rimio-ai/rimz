use super::exec::*;
use super::launch::*;
use super::*;
use clap::Parser;
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::run_wake::{ExpectedRunFrame, RunWakeOutcome};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use std::collections::BTreeSet;
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
            budget: None,
            system_prompt_file: system_prompt_file.map(Path::to_path_buf),
            append_system_prompt_file: append_system_prompt_file.map(Path::to_path_buf),
            args: None,
        },
    );
    profiles
}

fn parse_exec(argv: &[&str]) -> ExecArgs {
    let parsed = ExecHarness::try_parse_from(argv).expect("parse exec");
    let AgentsSubcmd::Exec(args) = parsed.command else {
        panic!("expected exec subcommand");
    };
    *args
}

fn assert_clap_error(argv: &[&str], kind: clap::error::ErrorKind) {
    let err = AgentsHarness::try_parse_from(argv).expect_err("invalid form");
    assert_eq!(err.kind(), kind, "{argv:?}");
}

mod parse {
    use super::*;

    #[test]
    fn launch_forms_parse_public_contract() {
        let parsed = AgentsHarness::try_parse_from(
            "rimz claude,codex+term fix-tests --worktree=docs --bg".split_ascii_whitespace(),
        )
        .expect("parse agents launch");
        assert_eq!(
            (
                parsed.args.spec.as_deref(),
                parsed.args.prompt.as_deref(),
                parsed.args.worktree.as_deref(),
                parsed.args.bg
            ),
            (
                Some("claude,codex+term"),
                Some("fix-tests"),
                Some("docs"),
                true
            )
        );

        let parsed = AgentsHarness::try_parse_from(
            "rimz claude fix-auth --from-pr https://gitlab.com/org/repo/-/merge_requests/12 --worktree review-12 --model opus --description port-auth --effort high --system-prompt-file /abs/prompt.md --append-system-prompt-file /abs/append.md -p --max-turns 3 --retries 2 --verify true --max-attempts 4 -n swift-otter"
                .split_ascii_whitespace(),
        )
        .expect("parse maximal supervised launch");
        assert_eq!(
            (
                parsed.args.spec.as_deref(),
                parsed.args.prompt.as_deref(),
                parsed.args.worktree.as_deref()
            ),
            (Some("claude"), Some("fix-auth"), Some("review-12"))
        );
        assert_eq!(
            (
                parsed.args.model.as_deref(),
                parsed.args.description.as_deref(),
                parsed.args.effort.as_deref()
            ),
            (Some("opus"), Some("port-auth"), Some("high"))
        );
        assert_eq!(
            (
                parsed.args.system_prompt_file.as_deref(),
                parsed.args.append_system_prompt_file.as_deref()
            ),
            (
                Some(Path::new("/abs/prompt.md")),
                Some(Path::new("/abs/append.md"))
            )
        );
        assert_eq!(
            (parsed.args.max_turns, parsed.args.retries),
            (Some(3), Some(2))
        );
        assert_eq!(
            (
                parsed.args.verify.as_deref(),
                parsed.args.max_attempts,
                parsed.args.name.as_deref()
            ),
            (Some("true"), Some(4), Some("swift-otter"))
        );
        assert_eq!(
            parsed.args.from_pr.unwrap().forge,
            Some(rimz::forge::Forge::GitLab)
        );

        let resume =
            AgentsHarness::try_parse_from("rimz forge --resume".split_ascii_whitespace()).unwrap();
        let alias = AgentsHarness::try_parse_from("rimz forge --continue".split_ascii_whitespace())
            .unwrap();
        let scoped = AgentsHarness::try_parse_from(
            "rimz forge --worktree=restore-living-team --resume".split_ascii_whitespace(),
        )
        .unwrap();
        assert!(resume.args.resume && alias.args.resume && scoped.args.resume);
        assert_eq!(scoped.args.worktree.as_deref(), Some("restore-living-team"));
    }

    #[test]
    fn lane_resume_forms_parse_public_contract() {
        let scoped = AgentsHarness::try_parse_from(["rimz", "resume", "#docs"])
            .expect("parse scoped resume");
        let pr = AgentsHarness::try_parse_from([
            "rimz",
            "resume",
            "--from-pr",
            "https://github.com/rimz/rimz/pull/69",
            "--bg",
        ])
        .expect("parse PR resume");
        assert!(matches!(
            scoped.args.command,
            Some(AgentsSubcmd::Resume { scope: Some(scope), from_pr: None, bg: false })
                if scope == "#docs"
        ));
        assert!(matches!(
            pr.args.command,
            Some(AgentsSubcmd::Resume {
                scope: None,
                from_pr: Some(rimz::forge::PrTarget { number: 69, .. }),
                bg: true,
            })
        ));
        assert_clap_error(
            &["rimz", "resume", "#docs", "--from-pr", "69"],
            clap::error::ErrorKind::ArgumentConflict,
        );
    }

    #[test]
    fn invalid_launch_forms_report_clap_contracts() {
        use clap::error::ErrorKind::{ArgumentConflict, MissingRequiredArgument};

        for (argv, kind) in [
            (&["rimz", "list", "#docs", "--all"][..], ArgumentConflict),
            (
                &["rimz", "show", "swift-otter", "--ansi"],
                MissingRequiredArgument,
            ),
            (&["rimz", "refresh", "@codex", "--all"], ArgumentConflict),
            (
                &["rimz", "claude", "hi", "--output-format", "json"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "--max-turns", "3"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "--retries", "1"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--retries", "1", "--bg"],
                ArgumentConflict,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--verify", "true", "--bg"],
                ArgumentConflict,
            ),
            (
                &["rimz", "claude", "hi", "-p", "--max-attempts", "2"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "wait", "codex", "--from-start"],
                MissingRequiredArgument,
            ),
            (
                &["rimz", "wait", "otter", "--any", "--stream"],
                ArgumentConflict,
            ),
            (&["rimz", "wait"], MissingRequiredArgument),
            (
                &["rimz", "claude", "--new-pane", "--new-tab"],
                ArgumentConflict,
            ),
        ] {
            assert_clap_error(argv, kind);
        }
        for override_args in [
            &["hi"][..],
            &["--channel=design"],
            &["--from-pr", "1"],
            &["--name", "swift-otter"],
            &["--description", "work"],
            &["--model", "opus"],
            &["--effort", "high"],
            &["--budget", "5"],
            &["--ask"],
            &["--yolo"],
            &["--system-prompt-file", "/x"],
            &["--append-system-prompt-file", "/x"],
            &["-p"],
            &["--", "--debug"],
        ] {
            let argv = [vec!["rimz", "claude", "--resume"], override_args.to_vec()].concat();
            assert_clap_error(&argv, ArgumentConflict);
        }
    }

    #[test]
    fn invalid_supervised_output_combinations_fail_fast() {
        for (argv, output, fragment) in [
            (
                &["rimz", "claude", "hi", "-p", "--bg"][..],
                OutputFormat::StreamJson,
                "cannot be combined with --bg",
            ),
            (
                &["rimz", "claude", "hi", "-p", "--retries", "1"],
                OutputFormat::StreamJson,
                "choose text or json",
            ),
            (
                &["rimz", "claude", "hi", "-p", "--verify", "true"],
                OutputFormat::StreamJson,
                "choose text or json",
            ),
            (
                &[
                    "rimz",
                    "claude",
                    "hi",
                    "-p",
                    "--verify",
                    "true",
                    "--max-attempts",
                    "0",
                ],
                OutputFormat::Text,
                "at least 1",
            ),
        ] {
            let parsed = AgentsHarness::try_parse_from(argv).expect("parse runtime-invalid form");
            let err = validate_supervised_output(&parsed.args, output).expect_err("reject output");
            assert!(err.to_string().contains(fragment), "{argv:?}: {err:#}");
        }

        let parsed = AgentsHarness::try_parse_from(["rimz", "claude", "hi", "-p"])
            .expect("parse attempts without verify");
        let mut args = parsed.args;
        args.max_attempts = Some(2);
        let err =
            validate_supervised_output(&args, OutputFormat::Text).expect_err("require verify");
        assert!(err.to_string().contains("requires --verify"), "{err:#}");
    }

    #[test]
    fn launch_preconditions_reject_missing_or_ambiguous_specs() {
        for (argv, fragment) in [
            (&["rimz", "--worktree=docs"][..], "--worktree requires"),
            (&["rimz", "--from-pr", "1"], "--from-pr requires"),
            (&["rimz", "--", "term"], "missing agent spec"),
            (&["rimz", "--model", "opus"], "require an agent spec"),
            (&["rimz", "-p", "--max-turns", "3"], "require an agent spec"),
        ] {
            let parsed = AgentsHarness::try_parse_from(argv.iter().copied()).expect("parse flag");
            let err = reject_launch_flags_without_spec(&parsed.args).expect_err("reject flag");
            assert!(err.to_string().contains(fragment), "{err:#}");
        }

        let err = reject_prompt_that_looks_like_spec(
            Some("claude"),
            Some("codex"),
            &rimz::config::ProfilesConfig::default(),
            &rimz::config::CommandsConfig::default(),
            &rimz::config::TeamsConfig::default(),
        )
        .expect_err("reject fan-out typo");
        assert!(
            err.to_string().contains("rimz agents claude,codex"),
            "{err:#}"
        );
    }

    #[test]
    fn exec_actions_parse_identity_and_conflicts() {
        let args = parse_exec(&[
            "rimz",
            "exec",
            "codex",
            "--run-id",
            "run_0123456789abcdef0123456789abcdef",
            "--agent-name",
            "lucid-atlas",
            "--agent-mode",
            "yolo",
            "--agent-role",
            "coder",
            "--agent-team",
            "forge",
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
        ]);
        assert_eq!(args.kind, "codex");
        assert_eq!(
            args.run_id.as_ref().map(rimz::RunId::as_str),
            Some("run_0123456789abcdef0123456789abcdef")
        );
        assert_eq!(args.agent_name.as_deref(), Some("lucid-atlas"));
        assert_eq!(args.agent_mode, Some(PermissionMode::Yolo));
        assert_eq!(
            (args.agent_role.as_deref(), args.agent_team.as_deref()),
            (Some("coder"), Some("forge"))
        );
        assert_eq!(
            (args.launch_group.as_deref(), args.launch_ordinal),
            (Some("launch_group_1"), Some(2))
        );
        assert_eq!(args.agent_channel.as_deref(), Some("design"));
        assert_eq!(
            (args.agent_model.as_deref(), args.agent_effort.as_deref()),
            (Some("gpt-5.5"), Some("xhigh"))
        );
        assert_eq!(
            args.launch_id.as_deref(),
            Some("launch_0123456789abcdef0123456789abcdef")
        );
        assert!(args.exit_on_run_completion && args.close_pane_on_exit);
        assert_eq!(
            (args.worktree_path.as_deref(), args.prompt.as_deref()),
            (Some(Path::new("/x")), Some("hi"))
        );
        assert_eq!(args.extra_args, ["--model", "gpt-5-codex"]);

        let fork = parse_exec(&["rimz", "exec", "codex", "--fork", "sess-2"]);
        let resume = parse_exec(&["rimz", "exec", "claude", "--resume", "sess-1"]);
        assert_eq!(fork.fork.as_deref(), Some("sess-2"));
        assert_eq!(resume.resume.as_deref(), Some("sess-1"));
        for argv in [
            &[
                "rimz", "exec", "claude", "--resume", "sess-1", "--prompt", "hi",
            ][..],
            &[
                "rimz", "exec", "codex", "--fork", "sess-2", "--resume", "sess-1",
            ],
            &[
                "rimz", "exec", "codex", "--fork", "sess-2", "--prompt", "hi",
            ],
        ] {
            assert!(ExecHarness::try_parse_from(argv).is_err(), "{argv:?}");
        }
    }
}

#[test]
fn refresh_targets_honor_channel_filter() {
    let now = jiff::Timestamp::from_second(2_000).unwrap();
    let mut auth = rimz::testkit::agent_state("claude", "auth", now);
    auth.channel = Some("auth-refresh".to_owned());
    auth.worktree_path = Some("/repo/worktrees/auth-refresh".to_owned());
    let mut docs = rimz::testkit::agent_state("codex", "docs", now);
    docs.channel = Some("docs".to_owned());
    docs.worktree_path = Some("/repo/main".to_owned());
    let mut child = rimz::testkit::agent_state("claude", "child", now);
    child.channel = auth.channel.clone();
    child.parent_agent_id = Some(AgentSessionId::from("auth"));
    let mut unknown = rimz::testkit::agent_state("ghost", "unknown", now);
    unknown.channel = auth.channel.clone();
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/repo/main")),
        vec![auth, docs, child, unknown],
        now,
    );

    let scoped: Vec<&str> = super::refresh::refresh_targets(&snapshot, Some("auth-refresh"))
        .into_iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(scoped, vec!["auth"]);

    let workspace: Vec<&str> = super::refresh::refresh_targets(&snapshot, None)
        .into_iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(workspace, vec!["auth", "docs"]);
}

mod placement {
    use super::*;

    #[test]
    fn supervised_run_placement_matrix() {
        for (force_new_tab, has_ambient_pane, loop_zone, expected) in [
            (false, true, false, RunPlacement::Split),
            (true, true, false, RunPlacement::Tab),
            (false, false, false, RunPlacement::Tab),
            (false, true, true, RunPlacement::LoopZone),
            (false, false, true, RunPlacement::LoopZone),
            (true, true, true, RunPlacement::Tab),
        ] {
            assert_eq!(
                run_placement(force_new_tab, has_ambient_pane, loop_zone),
                expected,
                "force_new_tab={force_new_tab}, has_ambient_pane={has_ambient_pane}, loop_zone={loop_zone}"
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
        let append = dir.path().join("append.md");
        std::fs::write(&prompt, "be concise").expect("write prompt");
        std::fs::write(&append, "follow project rules").expect("write append");
        let parsed = AgentsHarness::try_parse_from([
            "rimz",
            "claude",
            "hi",
            "--system-prompt-file",
            prompt.to_str().unwrap(),
            "--append-system-prompt-file",
            append.to_str().unwrap(),
        ])
        .expect("parse prompt files");
        let preset = launch_override_preset(&parsed.args).expect("resolve prompt files");
        assert_eq!(
            (preset.system_prompt_file, preset.append_system_prompt_file),
            (
                Some(prompt.canonicalize().unwrap()),
                Some(append.canonicalize().unwrap())
            )
        );

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
    fn launch_options_apply_without_overwriting_spec_identity() {
        let auto_args = rimz::agents::find_adapter("codex")
            .expect("codex")
            .permission_args(PermissionMode::Auto);
        let cell = |args, mode| Cell::Agent {
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
        };
        let mut layout =
            LayoutSpec::single(cell(vec!["--model".into(), "profile-model".into()], None));
        layout.columns[0]
            .rows
            .push(cell(auto_args.clone(), Some(PermissionMode::Auto)));
        apply_launch_mode_and_passthrough(
            &mut layout,
            Some(PermissionMode::Yolo),
            &rimz::agents::LaunchPreset {
                model: Some("override-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                ..rimz::agents::LaunchPreset::default()
            },
            &["--debug".to_owned()],
        )
        .expect("apply launch options");
        let [
            Cell::Agent {
                args: unset_args,
                mode: unset_mode,
                model: unset_model,
                effort: unset_effort,
                ..
            },
            Cell::Agent {
                args: preset_args,
                mode: preset_mode,
                model: preset_model,
                effort: preset_effort,
                ..
            },
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
                args.iter().any(|arg| arg.contains("xhigh"))
                    && args.contains(&"--debug".to_owned())
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
        let codex_default = rimz::agents::find_adapter("codex")
            .expect("codex")
            .default_launch_model()
            .expect("codex default model");
        let explicit = Cell::Agent {
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
        };
        let mut layout = LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("codex")));
        layout.columns[0]
            .rows
            .extend([explicit, Cell::agent(AgentKind::new_unchecked("claude"))]);
        apply_default_launch_models(&mut layout).expect("apply defaults");
        assert!(matches!(&layout.columns[0].rows[0],
            Cell::Agent { args, model: Some(model), .. }
                if model == &codex_default && args == &["--model", codex_default.as_str()]));
        assert!(matches!(&layout.columns[0].rows[1],
            Cell::Agent { args, model: Some(model), .. }
                if model == "o3" && args == &["--model", "o3"]));
        assert_eq!(
            layout.columns[0].rows[2],
            Cell::agent(AgentKind::new_unchecked("claude"))
        );
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

mod pane_exec {
    use super::*;

    #[test]
    fn agent_argv_preserves_launch_resume_and_fork_actions() {
        let adapter = rimz::agents::find_adapter("codex").expect("codex adapter");
        for (fork, resume, prompt, action, session) in [
            (None, None, Some("inspect"), "inspect", None),
            (Some("fork-id"), None, None, "fork", Some("fork-id")),
            (None, Some("resume-id"), None, "resume", Some("resume-id")),
        ] {
            let mut args = bare_exec_args();
            args.fork = fork.map(str::to_owned);
            args.resume = resume.map(str::to_owned);
            args.prompt = prompt.map(str::to_owned);
            args.extra_args = vec!["--model".to_owned(), "o3".to_owned()];
            let argv = agent_argv(adapter, &args.kind, &exec_action(&args)).expect("agent argv");
            assert!(argv.windows(2).any(|pair| pair == ["--model", "o3"]));
            assert!(argv.iter().any(|arg| arg == action), "{argv:?}");
            assert!(session.is_none_or(|id| argv.iter().any(|arg| arg == id)));
        }
    }

    #[test]
    fn wrapper_lifetime_policy_preserves_recoverable_sessions() {
        let mut run_owned = bare_exec_args();
        run_owned.run_id = Some(rimz::RunId::new());
        let mut worktree_owned = bare_exec_args();
        worktree_owned.worktree_path = Some(PathBuf::from("/tmp/rimz-worktree"));
        let mut completion_owned = bare_exec_args();
        completion_owned.run_id = Some(rimz::RunId::new());
        completion_owned.exit_on_run_completion = true;
        completion_owned.close_pane_on_exit = true;
        let mut close_owned = bare_exec_args();
        close_owned.close_pane_on_exit = true;

        for (name, args, direct, record_end, drop_to_shell) in [
            ("bare", bare_exec_args(), cfg!(unix), true, false),
            ("supervised run", run_owned, false, true, false),
            ("worktree", worktree_owned, false, true, true),
            ("completion", completion_owned, false, false, false),
            ("close", close_owned, false, true, true),
        ] {
            assert_eq!(should_exec_agent_directly(&args), direct, "{name}");
            assert_eq!(should_record_end_trace(&args), record_end, "{name}");
            assert_eq!(should_drop_to_shell(&args, false), drop_to_shell, "{name}");
            assert!(!should_drop_to_shell(&args, true), "{name}");
        }

        for (abrupt, accepts_close, expected) in [
            (false, false, true),
            (false, true, true),
            (true, true, true),
            (true, false, false),
        ] {
            assert_eq!(close_is_deliberate(abrupt, accepts_close), expected);
        }
    }

    #[test]
    fn exit_hints_use_best_relaunch_identity() {
        let mut team = bare_exec_args();
        team.agent_team = Some("trim".to_owned());
        team.agent_role = Some("pruner".to_owned());
        team.agent_profile = Some("codex-plan".to_owned());
        let mut profile = bare_exec_args();
        profile.agent_profile = Some("codex-plan".to_owned());
        assert_eq!(relaunch_command(&team), "rimz agents trim.pruner");
        assert_eq!(relaunch_command(&profile), "rimz agents codex-plan");
        assert_eq!(relaunch_command(&bare_exec_args()), "rimz agents codex");

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
        let (sock, _sock_path) =
            rimz::harness::run_wake::bind_run(&runtime, &run_id).expect("bind run");
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
        let outcome = rimz::harness::run_wake::wait_for_run_completion_owning(
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
    fn cached_daemon_reap_forwards_published_live_panes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = rimz::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%1");
        let mut codex = agent_with_status(
            "live-pane",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_000,
        );
        codex.kind = AgentKind::new_unchecked("codex");
        codex.pane = Some(rimz::pane::PaneRef::from_id(pane_id.clone()));
        codex.runtime_owner = Some(rimz::RuntimeOwner::new(
            rimz::RuntimeOwnerKind::Daemon,
            "live-pane",
            77,
            None,
        ));
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            workspace_id,
            vec![codex],
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
        let frame = rimz::sidebar::frame::assemble_frame(
            vec![rimz::pane::PaneRef::from_id(pane_id)],
            1,
            "rimz-test",
        );
        rimz::store::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame)
            .unwrap();

        crate::cli::apply_cached_daemon_reap(&mut snapshot, &runtime, "rimz-test");
        assert_eq!(snapshot.agents[0].agent_id.as_str(), "live-pane");
    }

    #[test]
    fn agents_table_projects_public_row_contract() {
        let now = jiff::Timestamp::from_second(2_000).unwrap();
        let mut failed = agent_with_status(
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
        failed.name = Some("writer".to_owned());
        failed.name_explicit = true;
        failed.description = Some("fix failing auth flow".to_owned());
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
        let running = agent_with_status(
            "running-sess",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1_000,
        );
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![failed, paused, running],
            now,
        );
        let text = render_agents_text(&snapshot, now, 120);

        assert!(
            text.contains("@writer") && !text.contains("@claude"),
            "{text}"
        );
        assert!(text.contains("DESC"), "{text}");
        assert!(text.contains("fix failing auth flow"), "{text}");
        assert!(text.contains("failed"), "{text}");
        assert!(text.contains("paused"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(
            !text.contains(":reasoning"),
            "agent rows drop phase suffixes:\n{text}"
        );
    }

    #[test]
    fn agents_table_clips_description_to_width() {
        let now = jiff::Timestamp::from_second(2_000).unwrap();
        let mut agent = agent_with_status(
            "long-desc",
            rimz::agents::AgentStatus::Idle,
            rimz::agents::TurnPhase::Idle,
            1_000,
        );
        agent.description = Some("this description is far too long for the terminal".to_owned());
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![agent],
            now,
        );
        let agents: Vec<&rimz::agents::AgentState> = snapshot.agents.iter().collect();

        let mut out = anstream::StripStream::new(Vec::new());
        render_agents_table(
            &mut out,
            &snapshot,
            &agents,
            now,
            72,
            &rimz::config::ThemeConfig::default(),
        )
        .expect("render agents table");
        let text = String::from_utf8(out.into_inner()).expect("utf8");

        assert!(text.contains('…'), "{text}");
        assert!(
            text.lines()
                .all(|line| unicode_width::UnicodeWidthStr::width(line) <= 72),
            "{text}"
        );
    }

    #[test]
    fn agents_table_groups_lanes_with_theme_and_team_context() {
        let now = jiff::Timestamp::from_second(2_000).unwrap();
        let auth_path = Some("/repo/worktrees/auth-refresh");
        let mut external = agent_in_lane("external", None, None, None);
        external.status = rimz::agents::AgentStatus::Failed;
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/repo/main")),
            vec![
                agent_in_lane("planner", Some("auth-refresh"), auth_path, Some("forge")),
                agent_in_lane("coder", Some("auth-refresh"), auth_path, Some("forge")),
                agent_in_lane("docs", Some("docs"), Some("/repo/main"), None),
                external,
                agent_in_lane(
                    "repeated",
                    Some("rimz/forge"),
                    Some("/repo/main"),
                    Some("forge"),
                ),
                agent_in_lane(
                    "mixed-one",
                    Some("mixed"),
                    Some("/repo/main"),
                    Some("forge"),
                ),
                agent_in_lane(
                    "mixed-two",
                    Some("mixed"),
                    Some("/repo/main"),
                    Some("review"),
                ),
            ],
            now,
        )
        .with_project_root(Some(PathBuf::from("/repo/main")));

        let text = render_agents_text(&snapshot, now, 120);
        assert!(text.contains("⑂ auth-refresh · forge team"), "{text}");
        assert!(text.contains("# docs"), "{text}");
        assert!(text.contains("external"), "{text}");
        assert!(
            !text.lines().next().unwrap_or_default().contains("CHANNEL"),
            "{text}"
        );
        for lane in ["rimz/forge", "# mixed"] {
            let header = text.lines().find(|line| line.contains(lane)).unwrap();
            assert!(!header.contains("team"), "{text}");
        }

        let theme = rimz::config::ThemeConfig {
            glyphs: rimz::config::ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..rimz::config::ThemeGlyphsConfig::default()
            },
            ..rimz::config::ThemeConfig::default()
        };
        let text = render_agents_text_with_theme(&snapshot, now, 120, &theme);
        assert!(text.contains("\u{f126} auth-refresh"), "{text}");
        assert!(text.contains("\u{f292} docs"), "{text}");
    }

    #[test]
    fn show_activity_projects_phase_only_for_active_turns() {
        let now = jiff::Timestamp::from_second(2_000).unwrap();
        let active = agent_with_status(
            "active",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Acting,
            1_000,
        );
        let idle = agent_with_status(
            "idle",
            rimz::agents::AgentStatus::Idle,
            rimz::agents::TurnPhase::Idle,
            1_000,
        );

        let mut active_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut active_out, &active, None, false, now)
            .expect("render active activity");
        let active_text = String::from_utf8(active_out.into_inner()).expect("utf8");
        assert!(
            active_text
                .lines()
                .any(|line| line.contains("status:") && line.contains("running")),
            "{active_text}"
        );
        assert!(
            active_text
                .lines()
                .any(|line| line.contains("phase:") && line.contains("acting")),
            "{active_text}"
        );

        let mut idle_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut idle_out, &idle, None, false, now)
            .expect("render idle activity");
        let idle_text = String::from_utf8(idle_out.into_inner()).expect("utf8");
        assert!(idle_text.contains("status:"), "{idle_text}");
        assert!(!idle_text.contains("phase:"), "{idle_text}");
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
            spec: "planner".to_owned(),
            prompt: Some("fix auth".to_owned()),
            worktree: Some("auth".to_owned()),
            mode: Some(PermissionMode::Ask),
            effort: Some("high".to_owned()),
            budget: Some("5/day".parse().expect("budget")),
            system_prompt_file: Some(PathBuf::from("/prompts/system.md")),
            timeout: Some(Duration::from_secs(90)),
            keep: true,
            stream: true,
            verify: Some("cargo xtask test auth".to_owned()),
            max_attempts: Some(4),
            loop_zone: false,
        });
        assert_eq!(
            (
                args.spec.as_deref(),
                args.prompt.as_deref(),
                args.worktree.as_deref(),
                args.ask,
                args.effort.as_deref(),
                args.budget.as_ref().map(ToString::to_string)
            ),
            (
                Some("planner"),
                Some("fix auth"),
                Some("auth"),
                true,
                Some("high"),
                Some("$5.00/day".to_owned())
            )
        );
        assert_eq!(
            (
                args.system_prompt_file.as_deref(),
                args.timeout,
                args.keep,
                args.stream_text,
                args.verify.as_deref(),
                args.max_attempts,
                args.print,
                args.bg,
                args.passthrough.as_slice()
            ),
            (
                Some(Path::new("/prompts/system.md")),
                Some(Duration::from_secs(90)),
                true,
                true,
                Some("cargo xtask test auth"),
                Some(4),
                true,
                false,
                &[] as &[String]
            )
        );

        let unscoped = AgentsArgs::for_task(TaskRunArgs {
            spec: "codex".to_owned(),
            prompt: Some("check status".to_owned()),
            worktree: None,
            mode: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            timeout: None,
            keep: true,
            stream: false,
            verify: None,
            max_attempts: None,
            loop_zone: false,
        });
        assert_eq!(unscoped.worktree, None);
        assert!(
            unscoped.keep,
            "manual loop fire can keep the transient pane"
        );
    }
}

fn bare_exec_args() -> ExecArgs {
    ExecArgs {
        kind: "codex".to_owned(),
        fork: None,
        resume: None,
        run_id: None,
        agent_name: Some("lucid-atlas".to_owned()),
        agent_name_explicit: false,
        agent_profile: None,
        agent_mode: None,
        agent_role: None,
        agent_team: None,
        launch_group: None,
        launch_ordinal: None,
        agent_channel: None,
        agent_model: None,
        agent_effort: None,
        agent_budget: None,
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

fn render_agents_text(
    snapshot: &rimz::SidebarSnapshot,
    now: jiff::Timestamp,
    max_width: usize,
) -> String {
    render_agents_text_with_theme(
        snapshot,
        now,
        max_width,
        &rimz::config::ThemeConfig::default(),
    )
}

fn render_agents_text_with_theme(
    snapshot: &rimz::SidebarSnapshot,
    now: jiff::Timestamp,
    max_width: usize,
    theme: &rimz::config::ThemeConfig,
) -> String {
    let agents: Vec<&rimz::agents::AgentState> = snapshot.agents.iter().collect();
    let mut out = anstream::StripStream::new(Vec::new());
    render_agents_table(&mut out, snapshot, &agents, now, max_width, theme)
        .expect("render agents table");
    String::from_utf8(out.into_inner()).expect("utf8")
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
            turn_opened_by: Vec::new(),
            turn_error: Some(rimz::agents::AgentTurnError {
                class,
                at: jiff::Timestamp::from_second(at).unwrap(),
                label: Some(label.to_owned()),
            }),
            turn_complete: None,
            turn_interrupted: None,
            observed_at: jiff::Timestamp::from_second(at).unwrap(),
        });
        self
    }
}

fn agent_in_lane(
    id: &str,
    channel: Option<&str>,
    worktree: Option<&str>,
    team: Option<&str>,
) -> rimz::agents::AgentState {
    let mut agent = agent_with_status(
        id,
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        1_000,
    );
    agent.channel = channel.map(ToOwned::to_owned);
    agent.worktree_path = worktree.map(ToOwned::to_owned);
    agent.worktree_branch = worktree.map(|_| "main".to_owned());
    agent.team = team.map(ToOwned::to_owned);
    agent
}

fn agent_with_status(
    id: &str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    activity: i64,
) -> rimz::agents::AgentState {
    let at = jiff::Timestamp::from_second(activity).unwrap();
    rimz::agents::AgentState {
        status,
        phase,
        worktree_path: Some("/tmp/rimz-agents-table".to_owned()),
        worktree_branch: Some("main".to_owned()),
        ..rimz::testkit::agent_state("claude", id, at)
    }
}
