use super::exec::*;
use super::launch::*;
use super::*;
use clap::{CommandFactory, Parser};
use jiff::Timestamp;
use rimz::agents::{
    AgentState, AgentStatus, AgentTurnError, LaunchParams, LaunchPreset, TurnErrorClass, TurnPhase,
};
use rimz::config::{MachineConfig, Profile, ProfilesConfig, ThemeConfig, ThemeGlyphsConfig};
use rimz::forge::Forge;
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecInvocation};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::run_wake::{ExpectedRunFrame, RunWakeOutcome};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use rimz::sidebar::refresh::CodexDaemonReap;
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

fn planner_profiles() -> ProfilesConfig {
    let mut profiles = ProfilesConfig::default();
    profiles.0.insert(
        "planner".to_owned(),
        Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        },
    );
    profiles
}

fn parse_agents(argv: &[&str]) -> AgentsArgs {
    AgentsHarness::try_parse_from(argv)
        .expect("parse agents command")
        .args
}

fn parse_exec(argv: &[&str]) -> ExecArgs {
    let parsed = ExecHarness::try_parse_from(argv).expect("parse exec");
    let AgentsSubcmd::Exec(args) = parsed.command else {
        panic!("expected exec subcommand");
    };
    *args
}

fn parse_exec_invocation(input: &ExecInvocation<'_>) -> ExecArgs {
    let argv = rimz::harness::launch::exec_argv(Path::new("/bin/rimz"), input);
    let parsed = crate::cli::Cli::try_parse_from(argv).expect("parse rendered exec argv");
    let Some(crate::cli::Subcmd::Agents(args)) = parsed.subcommand else {
        panic!("expected agents subcommand");
    };
    let Some(AgentsSubcmd::Exec(args)) = args.command else {
        panic!("expected exec subcommand");
    };
    *args
}

fn minimal_exec_invocation<'a>(kind: &'a str, action: ExecAction<'a>) -> ExecInvocation<'a> {
    ExecInvocation {
        kind,
        action,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: ExecIdentity::default(),
    }
}

fn resume_or_fork_contract(action: ExecAction<'_>) -> (&str, &str, &[String]) {
    use ExecAction::{Fork, Launch, Resume};

    match action {
        Resume {
            session_id,
            extra_args,
        } => ("resume", session_id, extra_args),
        Fork {
            session_id,
            extra_args,
        } => ("fork", session_id, extra_args),
        Launch { .. } => panic!("expected resume or fork"),
    }
}

fn assert_clap_error(argv: &[&str], kind: clap::error::ErrorKind) {
    let err = AgentsHarness::try_parse_from(argv).expect_err("invalid form");
    assert_eq!(err.kind(), kind, "{argv:?}");
}

#[test]
fn reserved_agent_words_name_current_verbs() {
    let command = AgentsHarness::command();
    let mut verbs = BTreeSet::new();
    for subcommand in command.get_subcommands() {
        verbs.insert(subcommand.get_name().to_owned());
        verbs.extend(subcommand.get_all_aliases().map(str::to_owned));
    }
    for word in rimz::harness::petname::RESERVED_AGENT_WORDS {
        if *word == "term" {
            continue;
        }
        assert!(
            verbs.contains(*word),
            "reserved word `{word}` is not an agents verb"
        );
    }
}

mod parse {
    use super::*;

    #[test]
    fn launch_forms_parse_public_contract() {
        let argv: Vec<_> = "rimz claude,codex+term fix-tests --worktree=docs --bg"
            .split_ascii_whitespace()
            .collect();
        let args = parse_agents(&argv);
        assert_eq!(
            [
                args.spec.as_deref(),
                args.prompt.as_deref(),
                args.worktree.as_deref()
            ],
            [Some("claude,codex+term"), Some("fix-tests"), Some("docs")]
        );
        assert!(args.bg);

        let argv: Vec<_> = "rimz claude fix-auth --from-pr https://gitlab.com/org/repo/-/merge_requests/12 --worktree review-12 --model opus --description port-auth --effort high --system-prompt-file /abs/prompt.md --append-system-prompt-file /abs/append.md -p --max-turns 3 --retries 2 --verify true --max-attempts 4 -n swift-otter"
            .split_ascii_whitespace()
            .collect();
        let args = parse_agents(&argv);
        assert_eq!(
            (
                args.spec.as_deref(),
                args.prompt.as_deref(),
                args.worktree.as_deref(),
                args.model.as_deref(),
                args.description.as_deref(),
                args.effort.as_deref(),
                args.system_prompt_file.as_deref(),
                args.append_system_prompt_file.as_deref(),
                args.max_turns,
            ),
            (
                Some("claude"),
                Some("fix-auth"),
                Some("review-12"),
                Some("opus"),
                Some("port-auth"),
                Some("high"),
                Some(Path::new("/abs/prompt.md")),
                Some(Path::new("/abs/append.md")),
                Some(3),
            )
        );
        assert_eq!(
            (
                args.retries,
                args.verify.as_deref(),
                args.max_attempts,
                args.name.as_deref(),
            ),
            (Some(2), Some("true"), Some(4), Some("swift-otter"),)
        );
        assert_eq!(args.from_pr.unwrap().forge, Some(Forge::GitLab));

        let resume = parse_agents(&["rimz", "forge", "--resume"]);
        let alias = parse_agents(&["rimz", "forge", "--continue"]);
        let scoped = parse_agents(&[
            "rimz",
            "forge",
            "--worktree=restore-living-team",
            "--resume",
        ]);
        assert!(resume.resume && alias.resume && scoped.resume);
        assert_eq!(scoped.worktree.as_deref(), Some("restore-living-team"));
    }

    #[test]
    fn lane_resume_forms_parse_public_contract() {
        let scoped = parse_agents(&["rimz", "resume", "#docs"]);
        let pr = parse_agents(&[
            "rimz",
            "resume",
            "--from-pr",
            "https://github.com/rimz/rimz/pull/69",
            "--bg",
        ]);
        assert!(matches!(
            scoped.command,
            Some(AgentsSubcmd::Resume { scope: Some(scope), from_pr: None, bg: false })
                if scope == "#docs"
        ));
        assert!(matches!(
            pr.command,
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
            let args = parse_agents(argv);
            let err = validate_supervised_output(&args, output).expect_err("reject output");
            assert!(err.to_string().contains(fragment), "{argv:?}: {err:#}");
        }
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
            let args = parse_agents(argv);
            let err = reject_launch_flags_without_spec(&args).expect_err("reject flag");
            assert!(err.to_string().contains(fragment), "{err:#}");
        }
    }

    #[test]
    fn exec_argv_round_trips_identity_actions_and_conflicts() {
        let launch_extra = vec!["--dangerously-skip-permissions".to_owned()];
        let input = ExecInvocation {
            kind: "claude",
            action: ExecAction::Launch {
                prompt: Some("fix it"),
                extra_args: &launch_extra,
            },
            run_id: Some("run_0123456789abcdef0123456789abcdef"),
            worktree_path: Some(Path::new("/repo/worktree")),
            close_pane_on_exit: true,
            exit_on_run_completion: true,
            identity: ExecIdentity {
                name: Some("swift-otter"),
                name_explicit: true,
                launch_id: Some("launch_0123456789abcdef0123456789abcdef"),
                profile: Some("planner"),
                mode: Some(PermissionMode::Yolo),
                role: Some("coder"),
                team: Some("forge"),
                launch_group: Some("launch_group_1"),
                launch_ordinal: Some(2),
                channel: Some("design"),
                model: Some("opus"),
                effort: Some("high"),
                budget: Some("$12.50/day"),
            },
        };

        let args = parse_exec_invocation(&input);
        let action = exec_action(&args);
        let actual = exec_invocation(&args, action);
        assert_eq!(
            (
                actual.kind,
                actual.run_id,
                actual.worktree_path,
                actual.close_pane_on_exit,
                actual.exit_on_run_completion,
            ),
            (
                "claude",
                Some("run_0123456789abcdef0123456789abcdef"),
                Some(Path::new("/repo/worktree")),
                true,
                true,
            )
        );
        let ExecAction::Launch { prompt, extra_args } = actual.action else {
            panic!("expected launch actions");
        };
        assert_eq!(
            (prompt, extra_args),
            (Some("fix it"), launch_extra.as_slice())
        );
        assert_eq!(
            (
                actual.identity.name,
                actual.identity.name_explicit,
                args.launch_id.as_deref(),
            ),
            (
                Some("swift-otter"),
                true,
                Some("launch_0123456789abcdef0123456789abcdef"),
            )
        );
        assert_eq!(
            launch_params(&args),
            LaunchParams {
                profile: Some("planner".to_owned()),
                mode: Some(PermissionMode::Yolo),
                role: Some("coder".to_owned()),
                model: Some("opus".to_owned()),
                effort: Some("high".to_owned()),
                budget: Some("$12.50/day".to_owned()),
                team: Some("forge".to_owned()),
                launch_group: Some("launch_group_1".to_owned()),
                launch_ordinal: Some(2),
                channel: Some("design".to_owned()),
                kind_ordinal: None,
            }
        );

        let resume_extra = vec!["--verbose".to_owned()];
        let fork_extra = vec!["--branch".to_owned()];
        for input in [
            minimal_exec_invocation(
                "claude",
                ExecAction::Resume {
                    session_id: "sess-1",
                    extra_args: &resume_extra,
                },
            ),
            minimal_exec_invocation(
                "codex",
                ExecAction::Fork {
                    session_id: "sess-2",
                    extra_args: &fork_extra,
                },
            ),
        ] {
            let args = parse_exec_invocation(&input);
            let actual = exec_invocation(&args, exec_action(&args));
            assert_eq!(actual.kind, input.kind);
            assert_eq!(
                resume_or_fork_contract(actual.action),
                resume_or_fork_contract(input.action)
            );
        }

        for command in [
            "rimz exec claude --resume sess-1 --prompt hi",
            "rimz exec codex --fork sess-2 --resume sess-1",
            "rimz exec codex --fork sess-2 --prompt hi",
        ] {
            assert!(
                ExecHarness::try_parse_from(command.split_ascii_whitespace()).is_err(),
                "{command}"
            );
        }

        let args = parse_exec(&["rimz", "exec", "claude", "--launch-id", "launch_orphan"]);
        assert_eq!(
            super::exec::exec_launch_identity(&args)
                .expect_err("launch id requires a name")
                .to_string(),
            "--launch-id requires --agent-name"
        );
    }
}

#[test]
fn refresh_targets_honor_channel_filter() {
    let now = Timestamp::from_second(2_000).unwrap();
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

    fn resolve_and_validate(
        args: &AgentsArgs,
        machine: &MachineConfig,
        root: &Path,
    ) -> Result<(ResolvedLaunch, LaunchPreset)> {
        let effective =
            rimz::config::effective::load(&machine.agents, root, &root.join("config-home"))?;
        let resolved = rimz::harness::plan::resolve_launch(
            &effective,
            &machine.agents.commands,
            args.spec.as_deref(),
        )?;
        let preset = validate_resolved_launch_inputs(
            args,
            &effective,
            &machine.agents.commands,
            &resolved.layout,
            true,
        )?;
        Ok((resolved, preset))
    }

    #[test]
    fn supervised_launch_normalizes_model_and_effort_overrides() {
        let args = parse_agents(&[
            "rimz", "codex", "fix-it", "--model", " gpt-5 ", "--effort", " low ", "-p",
        ]);
        let (request, _) = into_supervised_request(args).expect("build supervised request");
        let dir = tempfile::tempdir().expect("temp dir");
        let workspace = rimz::workspace::WorkspaceResolver::resolve(dir.path(), None)
            .expect("resolve workspace");

        let prepared = crate::cli::supervised::run::prepare_supervised_launch_layout(
            &request,
            &workspace,
            &MachineConfig::default(),
        )
        .expect("prepare supervised launch");
        let [Cell::Agent(rimz::harness::spec::AgentCell { model, effort, .. })] =
            prepared.columns[0].rows.as_slice()
        else {
            panic!("one agent")
        };
        assert_eq!(model.as_deref(), Some("gpt-5"));
        assert_eq!(effort.as_deref(), Some("low"));
    }

    #[test]
    fn prompt_file_flags_resolve_and_reject_bad_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        let append = dir.path().join("append.md");
        std::fs::write(&prompt, "be concise").expect("write prompt");
        std::fs::write(&append, "follow project rules").expect("write append");
        let system_flag = format!("--system-prompt-file={}", prompt.display());
        let append_flag = format!("--append-system-prompt-file={}", append.display());
        let args = parse_agents(&["rimz", "claude", "hi", &system_flag, &append_flag]);
        let preset = launch_override_preset(&args).expect("resolve prompt files");
        assert_eq!(
            (preset.system_prompt_file, preset.append_system_prompt_file),
            (
                Some(prompt.canonicalize().unwrap()),
                Some(append.canonicalize().unwrap())
            )
        );

        for flag in ["--system-prompt-file", "--append-system-prompt-file"] {
            let dir_path = dir.path().to_str().expect("utf8 dir path");
            let args = parse_agents(&["rimz", "claude", "hi", flag, dir_path]);
            let err = launch_override_preset(&args).expect_err("reject a directory");
            assert!(err.to_string().contains("is not a regular file"), "{err:#}");
        }

        let missing = dir.path().join("missing-append.md");
        let missing_path = missing.to_str().expect("utf8 missing path");
        let missing_flag = format!("--append-system-prompt-file={missing_path}");
        let args = parse_agents(&["rimz", "claude", "hi", &missing_flag]);
        let err = launch_override_preset(&args).expect_err("reject missing append path");
        assert!(
            err.to_string()
                .contains("reading --append-system-prompt-file"),
            "{err:#}"
        );
    }

    #[test]
    fn unknown_spec_errors_precede_secondary_launch_validation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing.md");
        let missing_path = missing.to_str().expect("utf8 path");
        let ambiguous = ["rimz", "missing-agent", "claude"];
        let missing_flag = format!("--system-prompt-file={missing_path}");
        let missing_file = ["rimz", "missing-agent", &missing_flag];
        for (argv, secondary_error) in [
            (&ambiguous[..], "looks like another spec"),
            (&missing_file[..], "system-prompt-file"),
        ] {
            let args = parse_agents(argv);
            let err = resolve_and_validate(&args, &MachineConfig::default(), dir.path())
                .expect_err("unknown spec wins");
            let message = err.to_string();
            assert!(message.contains("missing-agent"), "{err:#}");
            assert!(!message.contains(secondary_error), "{err:#}");
        }
    }

    #[test]
    fn multi_cell_name_fails_before_finalize_warnings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut machine = MachineConfig::default();
        machine.agents.profiles.0.insert(
            "warn".to_owned(),
            rimz::config::Profile {
                agent: "codex".to_owned(),
                mode: None,
                model: Some("declared".to_owned()),
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_file: None,
                args: Some("--model raw".to_owned()),
            },
        );
        let args = parse_agents(&["rimz", "warn,codex", "--name", "one"]);
        let effective = rimz::config::effective::load(
            &machine.agents,
            dir.path(),
            &dir.path().join("config-home"),
        )
        .expect("effective config");
        let resolved = rimz::harness::plan::resolve_launch(
            &effective,
            &machine.agents.commands,
            args.spec.as_deref(),
        )
        .expect("resolve warning-capable layout");

        let err = validate_resolved_launch_inputs(
            &args,
            &effective,
            &machine.agents.commands,
            &resolved.layout,
            true,
        )
        .expect_err("name cardinality wins");
        assert_eq!(
            err.to_string(),
            "--name requires a layout with exactly one agent cell"
        );

        let mut warning_layout = resolved.layout;
        let warnings = rimz::harness::plan::finalize_launch_layout(
            &mut warning_layout,
            LaunchFinalizeOptions {
                permission_mode: None,
                preset: &LaunchPreset::default(),
                passthrough: &[],
                budget: None,
                max_turns: None,
            },
        )
        .expect("layout can finalize");
        assert!(!warnings.is_empty(), "fixture must be warning-capable");
    }
}

mod pane_exec {
    use super::*;

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
            AgentStatus::Running,
            TurnPhase::Reasoning,
            1_000,
        );
        codex.kind = AgentKind::new_unchecked("codex");
        codex.pane = Some(PaneRef::from_id(pane_id.clone()));
        codex.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Daemon,
            "live-pane",
            77,
            None,
        ));
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            workspace_id,
            vec![codex],
            Timestamp::from_second(1_020).unwrap(),
        );
        rimz::sidebar::refresh::write_codex_daemon_reap(
            &runtime,
            &CodexDaemonReap {
                produced_at_ms: 1,
                daemon_pids: BTreeSet::from([77]),
                loaded: Some(BTreeSet::new()),
            },
        )
        .unwrap();
        let frame =
            rimz::sidebar::frame::assemble_frame(vec![PaneRef::from_id(pane_id)], 1, "rimz-test");
        rimz::store::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame)
            .unwrap();

        crate::cli::apply_cached_daemon_reap(&mut snapshot, &runtime, "rimz-test");
        assert_eq!(snapshot.agents[0].agent_id.as_str(), "live-pane");
    }

    #[test]
    fn agents_table_projects_public_row_contract() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut failed = agent_with_turn_error(
            agent_with_status(
                "failed-sess",
                AgentStatus::Running,
                TurnPhase::Reasoning,
                1_000,
            ),
            TurnErrorClass::Failed,
            1_010,
            "API Error: Bad Request",
        );
        failed.name = Some("writer".to_owned());
        failed.name_explicit = true;
        failed.description = Some("fix failing auth flow".to_owned());
        let paused = agent_with_turn_error(
            agent_with_status(
                "paused-sess",
                AgentStatus::Running,
                TurnPhase::Reasoning,
                1_000,
            ),
            TurnErrorClass::PausedOverloaded,
            1_010,
            "API Error: Overloaded",
        );
        let running = agent_with_status(
            "running-sess",
            AgentStatus::Running,
            TurnPhase::Reasoning,
            1_000,
        );
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![failed, paused, running],
            now,
        );
        let text = render_agents_text(&snapshot, now, 120);

        let header = text.lines().next().unwrap_or_default();
        assert!(
            text.contains("@writer")
                && !text.contains("@claude")
                && !header.contains("DESC")
                && text.lines().any(|line| line == "  fix failing auth flow")
                && ["failed", "paused", "running"]
                    .into_iter()
                    .all(|status| text.contains(status))
                && !text.contains(":reasoning"),
            "{text}"
        );
    }

    #[test]
    fn agents_table_wraps_collapsed_description_to_width() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut agent = agent_with_status("long-desc", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        agent.description = Some(
            "this description starts\nwith pasted\tcontent and keeps going across enough words to fill the first line, then the second line, then the third line, and finally more preview text that must be truncated because agent cards only show a bounded activity summary instead of the entire prompt or attached reference content"
                .to_owned(),
        );
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![agent],
            now,
        );
        let text = render_agents_text(&snapshot, now, 72);

        let description_lines: Vec<_> =
            text.lines().filter(|line| line.starts_with("  ")).collect();
        assert!(
            text.lines()
                .all(|line| unicode_width::UnicodeWidthStr::width(line) <= 72)
                && !description_lines.is_empty()
                && description_lines.len() <= 3
                && description_lines
                    .join(" ")
                    .contains("this description starts with pasted content")
                && description_lines
                    .last()
                    .is_some_and(|line| line.ends_with('…')),
            "{text}"
        );
    }

    #[test]
    fn agents_table_separates_descriptionless_cards() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut first = agent_with_status("first", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        first.name = Some("alpha".to_owned());
        first.name_explicit = true;
        let mut second = agent_with_status("second", AgentStatus::Idle, TurnPhase::Idle, 1_000);
        second.name = Some("beta".to_owned());
        second.name_explicit = true;
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-agents-table")),
            vec![first, second],
            now,
        );

        let text = render_agents_text(&snapshot, now, 120);
        let lines: Vec<_> = text.lines().collect();
        let first = lines
            .iter()
            .position(|line| line.starts_with("@alpha"))
            .expect("alpha row");
        let second = lines
            .iter()
            .position(|line| line.starts_with("@beta"))
            .expect("beta row");
        let earlier = first.min(second);

        assert_eq!(first.abs_diff(second), 2, "{text}");
        assert!(lines[earlier + 1].is_empty(), "{text}");
        assert!(!text.ends_with("\n\n"), "{text}");
    }

    #[test]
    fn agents_table_groups_lanes_with_theme_and_team_context() {
        let now = Timestamp::from_second(2_000).unwrap();
        let auth_path = Some("/repo/worktrees/auth-refresh");
        let mut external = agent_in_lane("external", None, None, None);
        external.status = AgentStatus::Failed;
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

        let theme = ThemeConfig {
            glyphs: ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..ThemeGlyphsConfig::default()
            },
            ..ThemeConfig::default()
        };
        let text = render_agents_text_with_theme(&snapshot, now, 120, &theme);
        assert!(text.contains("\u{f126} auth-refresh"), "{text}");
        assert!(text.contains("\u{f292} docs"), "{text}");
    }

    #[test]
    fn show_activity_projects_phase_only_for_active_turns() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut active =
            agent_with_status("active", AgentStatus::Running, TurnPhase::Acting, 1_000);
        active.description = Some("ship\nwide\tfix".to_owned());
        let idle = agent_with_status("idle", AgentStatus::Idle, TurnPhase::Idle, 1_000);

        let mut active_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut active_out, &active, None, false, now)
            .expect("render active activity");
        let active_text = String::from_utf8(active_out.into_inner()).expect("utf8");
        assert!(
            active_text
                .lines()
                .any(|line| line.contains("status:") && line.contains("running"))
                && active_text
                    .lines()
                    .any(|line| line.contains("phase:") && line.contains("acting"))
                && active_text.contains("description:   ship wide fix"),
            "{active_text}"
        );

        let mut idle_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut idle_out, &idle, None, false, now)
            .expect("render idle activity");
        let idle_text = String::from_utf8(idle_out.into_inner()).expect("utf8");
        assert!(idle_text.contains("status:"), "{idle_text}");
        assert!(!idle_text.contains("phase:"), "{idle_text}");

        let mut native_wait = agent_with_status(
            "droid-wait",
            AgentStatus::Running,
            TurnPhase::Reasoning,
            1_000,
        );
        let mut context = rimz::store::agent_context::empty_context("droid", now);
        context.native_permission_wait = Some(Timestamp::from_second(1_010).unwrap());
        native_wait.context = Some(context);
        let mut native_out = anstream::StripStream::new(Vec::new());
        super::show::render_activity_section(&mut native_out, &native_wait, None, false, now)
            .expect("render native wait activity");
        let native_text = String::from_utf8(native_out.into_inner()).expect("utf8");
        assert!(native_text.contains("waiting"), "{native_text}");
        assert!(!native_text.contains("phase:"), "{native_text}");
    }
}

mod automation {
    use super::*;

    #[test]
    fn create_on_miss_launches_kinds_and_agent_profiles_but_not_commands() {
        let profiles = planner_profiles();

        assert!(is_launchable_type("codex", &profiles));
        assert!(is_launchable_type("planner", &profiles));
        assert!(!is_launchable_type("vim", &profiles));
        assert!(!is_launchable_type("swift-otter", &profiles));
    }
}

fn bare_exec_args() -> ExecArgs {
    let argv: Vec<_> = "rimz exec codex --agent-name lucid-atlas --launch-id launch_0123456789abcdef0123456789abcdef"
        .split_ascii_whitespace()
        .collect();
    parse_exec(&argv)
}

fn render_agents_text(
    snapshot: &rimz::SidebarSnapshot,
    now: Timestamp,
    max_width: usize,
) -> String {
    render_agents_text_with_theme(snapshot, now, max_width, &ThemeConfig::default())
}

fn render_agents_text_with_theme(
    snapshot: &rimz::SidebarSnapshot,
    now: Timestamp,
    max_width: usize,
    theme: &ThemeConfig,
) -> String {
    let agents: Vec<&AgentState> = snapshot.agents.iter().collect();
    let mut out = anstream::StripStream::new(Vec::new());
    render_agents_table(&mut out, snapshot, &agents, now, max_width, theme)
        .expect("render agents table");
    String::from_utf8(out.into_inner()).expect("utf8")
}

fn agent_with_turn_error(
    mut agent: AgentState,
    class: TurnErrorClass,
    at: i64,
    label: &str,
) -> AgentState {
    let at = Timestamp::from_second(at).unwrap();
    let mut context = rimz::store::agent_context::empty_context(&agent.kind.to_string(), at);
    context.turn_error = Some(AgentTurnError {
        class,
        at,
        label: Some(label.to_owned()),
    });
    agent.context = Some(context);
    agent
}

fn agent_in_lane(
    id: &str,
    channel: Option<&str>,
    worktree: Option<&str>,
    team: Option<&str>,
) -> AgentState {
    let mut agent = agent_with_status(id, AgentStatus::Idle, TurnPhase::Idle, 1_000);
    agent.channel = channel.map(ToOwned::to_owned);
    agent.worktree_path = worktree.map(ToOwned::to_owned);
    agent.worktree_branch = worktree.map(|_| "main".to_owned());
    agent.team = team.map(ToOwned::to_owned);
    agent
}

fn agent_with_status(id: &str, status: AgentStatus, phase: TurnPhase, activity: i64) -> AgentState {
    let at = Timestamp::from_second(activity).unwrap();
    AgentState {
        status,
        phase,
        worktree_path: Some("/tmp/rimz-agents-table".to_owned()),
        worktree_branch: Some("main".to_owned()),
        ..rimz::testkit::agent_state("claude", id, at)
    }
}
