use super::*;

#[test]
fn pr_branch_terminal_defaults_and_eof() {
    use rimz::worktree::{PrBranchChoice, PrBranchDivergence, WorktreeErr};
    for (divergence, default, choice) in [
        (
            PrBranchDivergence::Behind { behind: 2 },
            0,
            PrBranchChoice::Remote,
        ),
        (
            PrBranchDivergence::Rebased {
                ahead: 1,
                behind: 2,
            },
            0,
            PrBranchChoice::Remote,
        ),
        (
            PrBranchDivergence::Diverged {
                ahead: 1,
                behind: 2,
                conflicts: true,
            },
            1,
            PrBranchChoice::Local,
        ),
    ] {
        let facts = WorktreeErr::PrBranchDiverged {
            branch: "feature".into(),
            holder: Some("/repo/tree".into()),
            divergence,
        };
        let mut warning = String::new();
        assert_eq!(
            resolve_pr_branch_choice_with(
                &facts,
                true,
                |_, choices, selected| {
                    assert_eq!(choices, &["remote", "local"]);
                    assert_eq!(selected, default);
                    Ok(Some(selected))
                },
                |line| {
                    warning.push_str(line);
                    Ok(())
                }
            )
            .unwrap(),
            Some(choice)
        );
        assert!(warning.contains("rimz: "));
        assert!(warning.contains("warning:"));
        assert!(warning.contains("/repo/tree"));
        assert!(warning.contains("reflog"));
        let mut prompted = false;
        assert_eq!(
            resolve_pr_branch_choice_with(
                &facts,
                true,
                |_, _, _| {
                    prompted = true;
                    Ok(None)
                },
                |_| Ok(())
            )
            .unwrap(),
            None
        );
        assert!(prompted);
    }
}

#[test]
fn pr_branch_nonterminal_fast_forwards_only_behind() {
    use rimz::worktree::{PrBranchChoice, PrBranchDivergence, WorktreeErr};
    let mut facts = WorktreeErr::PrBranchDiverged {
        branch: "feature".into(),
        holder: None,
        divergence: PrBranchDivergence::Behind { behind: 2 },
    };
    let mut warning = String::new();
    assert_eq!(
        resolve_pr_branch_choice_with(
            &facts,
            false,
            |_, _, _| panic!("no prompt"),
            |line| {
                warning.push_str(line);
                Ok(())
            }
        )
        .unwrap(),
        Some(PrBranchChoice::Remote)
    );
    assert!(warning.contains("fast-forwarding"));
    for divergence in [
        PrBranchDivergence::Rebased {
            ahead: 1,
            behind: 2,
        },
        PrBranchDivergence::Diverged {
            ahead: 1,
            behind: 0,
            conflicts: false,
        },
    ] {
        if let WorktreeErr::PrBranchDiverged {
            divergence: value, ..
        } = &mut facts
        {
            *value = divergence;
        }
        let err =
            resolve_pr_branch_choice_with(&facts, false, |_, _, _| panic!("no prompt"), |_| Ok(()))
                .unwrap_err();
        assert!(err.to_string().contains("git branch -f"));
    }
}

#[test]
fn parse_choice_blank_uses_default() {
    for answer in ["", " \t\n"] {
        assert_eq!(
            parse_choice(answer, &["resume", "fresh", "cancel"], 2),
            Some(2)
        );
    }
}

#[test]
fn parse_choice_matches_full_words_and_unique_prefixes() {
    let choices = ["Resume", "fresh", "cancel"];
    for (answer, expected) in [
        ("resume", 0),
        (" FRESH\n", 1),
        ("Cancel", 2),
        ("R", 0),
        ("f", 1),
        ("C", 2),
        ("frE", 1),
    ] {
        assert_eq!(parse_choice(answer, &choices, 0), Some(expected));
    }
}

#[test]
fn parse_choice_rejects_ambiguous_prefixes_and_invalid_answers() {
    for answer in ["r", "RE", "garbage", "fresh extra"] {
        assert_eq!(
            parse_choice(answer, &["resume", "remove", "fresh"], 0),
            None
        );
    }
}

#[test]
fn parse_choice_full_match_wins_over_longer_prefix_match() {
    assert_eq!(parse_choice(" RE ", &["resume", "re"], 0), Some(1));
}

fn parsed_scope(args: &[&str]) -> (String, String, Option<String>, Option<String>) {
    let mut matches = help::customize(<Cli as CommandFactory>::command())
        .try_get_matches_from(args)
        .unwrap();
    let canonical = matches.subcommand_name().unwrap_or("start").to_owned();
    let cli = Cli::from_arg_matches_mut(&mut matches).unwrap();
    let facts = scope_facts(&canonical, cli.subcommand.as_ref());
    let command = facts.command.to_owned();
    let session = facts.session.map(ToOwned::to_owned);
    let agent = facts.agent.map(ToOwned::to_owned);
    (canonical, command, session, agent)
}

fn workspace(
    project_root: &str,
    worktree_root: &str,
    worktree_branch: Option<&str>,
) -> rimz::ResolvedWorkspace {
    let project_root = PathBuf::from(project_root);
    rimz::ResolvedWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
        cwd_project_root: Some(project_root.clone()),
        project_root,
        root_class: rimz::workspace::RootClass::Repo,
        worktree_root: PathBuf::from(worktree_root),
        worktree_branch: worktree_branch.map(ToOwned::to_owned),
        session_name: "rimz-test".to_owned(),
        mux_hint: None,
    }
}

#[test]
fn current_channel_uses_callers_card_before_worktree() {
    let root = workspace("/repo", "/repo", None);
    let worktree = workspace("/repo", "/repo-worktrees/other", Some("other"));
    let mut agent = rimz::testkit::agent_state("claude", "caller", jiff::Timestamp::UNIX_EPOCH);
    agent.channel = Some("card".to_owned());
    let caller = rimz::harness::ancestry::CallerIdentity {
        kind: agent.kind.clone(),
        launch_id: Some(agent.agent_id.clone()),
        pane_id: None,
        name: None,
        profile: None,
        role: None,
    };
    for workspace in [&root, &worktree] {
        for ended in [false, true] {
            agent.ended_at = ended.then_some(jiff::Timestamp::UNIX_EPOCH);
            assert_eq!(
                current_channel_with(workspace, None, || Some((
                    caller.clone(),
                    vec![agent.clone()]
                )))
                .into_name(),
                Some("card".to_owned()),
            );
        }
    }
    assert_eq!(
        current_channel_with(&root, Some("explicit".to_owned()), || panic!(
            "explicit channel needs no lookup"
        ))
        .into_name(),
        Some("explicit".to_owned())
    );
    assert_eq!(current_channel_with(&root, None, || None).into_name(), None);
    assert_eq!(
        current_channel_with(&worktree, None, || Some((caller.clone(), vec![]))).into_name(),
        Some("other".to_owned())
    );
    agent.channel = None;
    agent.worktree_path = Some("/elsewhere/own-card".to_owned());
    assert_eq!(
        current_channel_with(&root, Some(String::new()), || Some((caller, vec![agent])))
            .into_name(),
        Some("own-card".to_owned())
    );
}

#[test]
fn address_context_preserves_current_channel_provenance() {
    use rimz::address::ChannelOrigin;
    for (current, channel, origin) in [
        (
            CurrentChannel::Named("card".into()),
            Some("card"),
            ChannelOrigin::Stamped,
        ),
        (
            CurrentChannel::Derived("checkout".into()),
            Some("checkout"),
            ChannelOrigin::Directory,
        ),
        (CurrentChannel::Unscoped, None, ChannelOrigin::Stamped),
    ] {
        let context = current.address_context();
        assert_eq!(context.channel.as_deref(), channel);
        assert_eq!(context.origin, origin);
    }
}

fn workspace_with_roots(room: &str, cwd: Option<&str>) -> rimz::ResolvedWorkspace {
    let project_root = PathBuf::from(room);
    rimz::ResolvedWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd_project_root: cwd.map(PathBuf::from),
        root_class: rimz::workspace::RootClass::Repo,
        worktree_root: project_root,
        worktree_branch: Some("main".to_owned()),
        session_name: "rimz-test".to_owned(),
        mux_hint: None,
    }
}

#[test]
fn cross_repo_worktree_requires_explicit_terminal_confirmation() {
    let workspace = workspace_with_roots("/repos/room", Some("/repos/current"));
    let mut output = Vec::new();

    let proceed = confirm_cross_repo_worktree_with(
        &workspace,
        true,
        |question| {
            assert_eq!(
                question,
                "Create the worktree at the current Git root and manage its cleanup from there?"
            );
            Ok(false)
        },
        |message| {
            output.push(message.to_owned());
            Ok(())
        },
    )
    .expect("confirmation");

    assert!(!proceed);
    let output = output.join("\n");
    assert!(output.contains("/repos/room"));
    assert!(output.contains("/repos/current"));
    assert!(output.contains("Launch aborted; nothing changed."));
}

#[test]
fn cross_repo_worktree_refuses_non_terminal_input_with_root_hint() {
    let workspace = workspace_with_roots("/repos/room", Some("/repos/current"));
    let mut output = Vec::new();

    let err = confirm_cross_repo_worktree_with(
        &workspace,
        false,
        |_| -> Result<bool> { panic!("non-terminal input must not prompt") },
        |message| {
            output.push(message.to_owned());
            Ok(())
        },
    )
    .expect_err("non-terminal mismatch must refuse");

    let error = err.to_string();
    assert!(error.contains("/repos/room"));
    assert!(error.contains("/repos/current"));
    assert!(error.contains("--root /repos/current"));
    assert!(output.join("\n").contains("current Git root"));
}

#[test]
fn explicit_root_avoids_cross_repo_confirmation() {
    let workspace = workspace_with_roots("/repos/current", Some("/repos/current"));

    let proceed = confirm_cross_repo_worktree_with(
        &workspace,
        false,
        |_| -> Result<bool> { panic!("equal roots must not prompt") },
        |_| panic!("equal roots must not warn"),
    )
    .expect("equal roots");

    assert!(proceed);
}

#[test]
fn room_channel_stamps_in_place_team_members() {
    let workspace = workspace("/code/team-channel", "/code/team-channel", None);

    assert_eq!(
        rimz::harness::spec::resolve_room_channel(
            &workspace.project_root,
            &workspace.worktree_root,
            Some("forge"),
            None,
        )
        .as_deref(),
        Some("team-channel/forge")
    );
    assert_eq!(
        rimz::harness::spec::resolve_room_channel(
            &workspace.project_root,
            &workspace.worktree_root,
            None,
            None,
        ),
        None
    );
}

#[test]
fn room_channel_ignores_branch_for_lane_identity() {
    let branch = workspace("/code/project", "/code/project", Some("feat/auth"));
    assert_eq!(
        rimz::harness::spec::resolve_room_channel(
            &branch.project_root,
            &branch.worktree_root,
            None,
            None,
        ),
        None
    );

    let child_worktree = workspace("/code/project", "/code/project-wt/auth", None);
    assert_eq!(
        rimz::harness::spec::resolve_room_channel(
            &child_worktree.project_root,
            &child_worktree.worktree_root,
            None,
            None,
        )
        .as_deref(),
        Some("auth")
    );
}

#[test]
fn mux_aliases_normalize_to_mux() {
    let mut cli = Cli::try_parse_from(["rimz", "--zellij"]).unwrap();
    cli.global.normalize().unwrap();
    assert_eq!(cli.global.mux, Some(MuxName::Zellij));

    let mut cli = Cli::try_parse_from(["rimz", "--tmux"]).unwrap();
    cli.global.normalize().unwrap();
    assert_eq!(cli.global.mux, Some(MuxName::Tmux));
}

#[test]
fn message_alias_parses_send_and_subcommands() {
    let cli = Cli::try_parse_from(["rimz", "msg", "@codex", "hi"]).unwrap();
    assert!(matches!(cli.subcommand, Some(Subcmd::Message(_))));

    let cli = Cli::try_parse_from(["rimz", "msg", "list"]).unwrap();
    assert!(matches!(cli.subcommand, Some(Subcmd::Message(_))));
}

#[test]
fn command_scope_uses_canonical_clap_labels() {
    assert_eq!(
        parsed_scope(&["rimz"]),
        ("start".to_owned(), "start".to_owned(), None, None)
    );
    assert_eq!(
        parsed_scope(&["rimz", "list"]),
        ("list".to_owned(), "list".to_owned(), None, None)
    );
    assert_eq!(
        parsed_scope(&["rimz", "msg", "@codex", "hi"]),
        ("message".to_owned(), "message".to_owned(), None, None)
    );
}

#[test]
fn command_scope_keeps_nested_labels_and_agent_identity() {
    for (args, expected) in [
        (vec!["rimz", "remote", "list"], ("remote list", None, None)),
        (vec!["rimz", "hooks", "apply"], ("hooks apply", None, None)),
        (
            vec!["rimz", "sidebar", "snapshot"],
            ("sidebar snapshot", None, None),
        ),
        (
            vec!["rimz", "hooks", "feed", "--source", "codex"],
            ("hooks feed", None, Some("codex")),
        ),
        (
            vec![
                "rimz",
                "hooks",
                "drain",
                "--project-root",
                "/workspace",
                "--once",
            ],
            ("hooks drain", None, None),
        ),
    ] {
        let (_, command, session, agent) = parsed_scope(&args);
        assert_eq!(
            (command.as_str(), session.as_deref(), agent.as_deref()),
            expected,
            "{args:?}"
        );
    }

    let workspace_id = rimz::WorkspaceId::from_project_root(std::path::Path::new("/workspace"));
    let request = rimz::agents::LifecycleRefreshRequest {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        session_id: "sess-codex".to_owned(),
        workspace_id,
        model: None,
        server_url: None,
    };
    let mut argv = vec!["rimz".to_owned()];
    argv.extend(rimz::child_process::agent_helper_argv(
        "refresh-context",
        &request,
    ));
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let (_, command, session, agent) = parsed_scope(&argv);
    assert_eq!(
        (command.as_str(), session.as_deref(), agent.as_deref()),
        ("agents refresh-context", Some("sess-codex"), Some("codex")),
    );
}

#[test]
fn mux_aliases_are_global_flags() {
    let mut cli = Cli::try_parse_from(["rimz", "list", "--tmux"]).unwrap();
    cli.global.normalize().unwrap();
    assert_eq!(cli.global.mux, Some(MuxName::Tmux));
}

#[test]
fn mux_aliases_conflict_with_each_other_and_mux() {
    let mut cli = Cli::try_parse_from(["rimz", "--zellij", "--tmux"]).unwrap();
    let err = cli.global.normalize().unwrap_err();
    assert_eq!(err.to_string(), "choose one of --mux, --zellij, --tmux");

    let mut cli = Cli::try_parse_from(["rimz", "--mux", "zellij", "--zellij"]).unwrap();
    let err = cli.global.normalize().unwrap_err();
    assert_eq!(err.to_string(), "choose one of --mux, --zellij, --tmux");
}

#[test]
fn mux_option_still_normalizes_unchanged() {
    let mut cli = Cli::try_parse_from(["rimz", "--mux", "tmux"]).unwrap();
    cli.global.normalize().unwrap();
    assert_eq!(cli.global.mux, Some(MuxName::Tmux));
}

#[test]
fn loop_verify_requires_a_spawned_agent() {
    let err = Cli::try_parse_from([
        "rimz",
        "loop",
        "add",
        "check-only",
        "--check",
        "false",
        "--verify",
        "true",
        "--every",
        "1h",
    ])
    .expect_err("verify needs an agent task");

    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}

#[test]
fn current_channel_is_stable_across_branch_changes() {
    let before = workspace("/code/project", "/code/project-wt/auth", Some("feat/auth"));
    let after = workspace("/code/project", "/code/project-wt/auth", Some("scratch"));

    let before_channel = rimz::harness::spec::resolve_room_channel(
        &before.project_root,
        &before.worktree_root,
        None,
        None,
    );
    let after_channel = rimz::harness::spec::resolve_room_channel(
        &after.project_root,
        &after.worktree_root,
        None,
        None,
    );
    assert_eq!(before_channel, after_channel);
    assert_eq!(before_channel.as_deref(), Some("auth"));
}

#[test]
fn removed_top_level_command_rejects_before_global_help() {
    assert!(
        reject_removed_top_level_tokens_from([
            OsString::from("autoping"),
            OsString::from("--help")
        ])
        .is_err()
    );
    assert!(
        reject_removed_top_level_tokens_from([
            OsString::from("--root"),
            OsString::from("."),
            OsString::from("autoping"),
            OsString::from("--help"),
        ])
        .is_err()
    );
    assert!(
        reject_removed_top_level_tokens_from([
            OsString::from("--refresh-ms"),
            OsString::from("100"),
            OsString::from("autoping"),
        ])
        .is_err()
    );
    assert!(
        reject_removed_top_level_tokens_from([
            OsString::from("--tmux"),
            OsString::from("autoping"),
        ])
        .is_err()
    );
    assert!(reject_removed_top_level_tokens_from([OsString::from("run")]).is_err());
    assert!(reject_removed_top_level_tokens_from([OsString::from("tab")]).is_err());
    for args in [
        vec!["channel", "new", "x"],
        vec!["--mux", "tmux", "channel", "list"],
    ] {
        let err = reject_removed_top_level_tokens_from(args.into_iter().map(OsString::from))
            .expect_err("removed channel noun")
            .to_string();
        for replacement in [
            "`rimz channel` has been removed",
            "rimz agents <spec> --channel <name>",
            "rimz agents list --all",
            "rimz worktree list",
        ] {
            assert!(err.contains(replacement), "{err}");
        }
    }
    assert!(reject_removed_top_level_tokens_from([OsString::from("agents")]).is_ok());
    assert!(
        reject_removed_top_level_tokens_from([OsString::from("docs"), OsString::from("autoping"),])
            .is_ok()
    );
}
