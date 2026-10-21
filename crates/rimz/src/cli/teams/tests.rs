use std::collections::BTreeMap;

use super::*;
use clap::Parser;

#[derive(Debug, Parser)]
struct TeamsHarness {
    #[command(flatten)]
    args: TeamsArgs,
}

#[derive(Debug, Parser)]
struct AgentsHarness {
    #[command(flatten)]
    args: agents_cmd::AgentsArgs,
}

fn parse_teams(argv: &[&str]) -> TeamsArgs {
    TeamsHarness::try_parse_from(argv)
        .expect("parse teams command")
        .args
}

#[test]
fn nearby_launch_words_suggest_teams_and_verbs() {
    let teams = rimz::config::TeamsConfig(BTreeMap::from([(
        "peer".into(),
        rimz::config::Team {
            layout: Some("claude,codex".into()),
            ..Default::default()
        },
    )]));
    let error = guard_launch("pee", None, &teams, &Default::default()).unwrap_err();
    assert!(error.downcast_ref::<UsageError>().is_some());
    assert_eq!(
        error.to_string(),
        "`pee` is neither a teams command nor a configured team\ndid you mean: rimz teams peer"
    );
    let error = guard_launch("lsit", None, &teams, &Default::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`lsit` is neither a teams command nor a configured team\ndid you mean: rimz teams list"
    );
    let error = validate_team_name("pee", &teams, &Default::default()).unwrap_err();
    assert!(error.downcast_ref::<UsageError>().is_none());
    assert_eq!(
        error.to_string(),
        "unknown team `pee`\ndid you mean: rimz teams peer"
    );
}

#[test]
fn positional_launch_guards_commands_and_unknown_teams() {
    let teams = rimz::config::TeamsConfig(BTreeMap::from([(
        "peer".to_owned(),
        rimz::config::Team::default(),
    )]));
    let error = guard_launch("status", None, &teams, &Default::default()).unwrap_err();
    assert!(error.downcast_ref::<UsageError>().is_some());
    let text = error.to_string();
    assert!(
        text.contains("neither a teams command nor a configured team"),
        "{text}"
    );
    assert!(
        text.contains("commands:") && text.contains("configured teams:"),
        "{text}"
    );
    for name in ["peer.x", "peer.x#topic"] {
        let text = guard_launch(name, None, &teams, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            text.contains("launch it with `rimz agents peer.x`"),
            "{text}"
        );
    }
    for verb in ["stop", "inspect", "ls"] {
        let error = guard_launch("peer", Some(verb), &teams, &Default::default()).unwrap_err();
        assert!(error.downcast_ref::<UsageError>().is_some());
        let text = error.to_string();
        assert!(text.contains(&format!("rimz teams {verb} peer")), "{text}");
        assert!(
            text.contains(&format!("rimz teams launch peer {verb}")),
            "{text}"
        );
    }
    let Some(TeamsSubcmd::Launch(args)) = parse_teams(&["rimz", "launch", "peer", "stop"]).command
    else {
        panic!("launch")
    };
    assert_eq!(args.prompt.as_deref(), Some("stop"));
}

#[test]
fn launch_rejects_attached_short_prompt() {
    for prefix in [vec!["rimz", "forge"], vec!["rimz", "launch", "forge"]] {
        for prompt in ["-wfeat", "-w=feat"] {
            for separator in [false, true] {
                let mut argv = prefix.clone();
                if separator {
                    argv.push("--");
                }
                argv.push(prompt);
                let error = TeamsHarness::try_parse_from(argv).expect_err("reject attached short");
                assert!(
                    error.to_string().contains("write -w NAME with a space"),
                    "{error}"
                );
            }
        }
        let mut argv = prefix;
        argv.extend(["-w", "feat"]);
        let args = parse_teams(&argv);
        let launch = match args.command {
            Some(TeamsSubcmd::Launch(args)) => args.launch,
            None => args.launch,
            _ => panic!("launch"),
        };
        assert_eq!(launch.worktree.as_deref(), Some("feat"));
    }
}

#[test]
fn bare_launch_accepts_hyphen_prompt_and_flags() {
    for prompt in ["--dry-run first", "-x", "-"] {
        for argv in [
            ["rimz", "forge", "--isolation", "host", prompt],
            ["rimz", "forge", prompt, "--isolation", "host"],
        ] {
            let args = parse_teams(&argv);
            assert!(args.command.is_none());
            assert_eq!(args.name.as_deref(), Some("forge"));
            assert_eq!(args.prompt.as_deref(), Some(prompt));
            assert_eq!(args.isolation, Some(rimz::config::Isolation::Host));
        }
    }
    for prompt in ["--isolation", "-h", "--help"] {
        assert_eq!(
            parse_teams(&["rimz", "forge", "--", prompt])
                .prompt
                .as_deref(),
            Some(prompt)
        );
    }
}

#[test]
fn launch_verb_accepts_hyphen_prompt_and_flags() {
    for prompt in ["--dry-run first", "-x", "-"] {
        for argv in [
            ["rimz", "launch", "forge", "--isolation", "host", prompt],
            ["rimz", "launch", "forge", prompt, "--isolation", "host"],
        ] {
            let Some(TeamsSubcmd::Launch(args)) = parse_teams(&argv).command else {
                panic!("launch verb");
            };
            assert_eq!(args.name, "forge");
            assert_eq!(args.prompt.as_deref(), Some(prompt));
            assert_eq!(args.isolation, Some(rimz::config::Isolation::Host));
        }
    }
    for prompt in ["--isolation", "-h", "--help"] {
        let Some(TeamsSubcmd::Launch(args)) =
            parse_teams(&["rimz", "launch", "forge", "--", prompt]).command
        else {
            panic!("launch verb");
        };
        assert_eq!(args.prompt.as_deref(), Some(prompt));
    }
}

#[test]
fn record_accepts_exactly_one_text_source_and_lists_sections() {
    for args in [
        vec!["rimz", "record", "Decisions", "text"],
        vec!["rimz", "record", "Goal", "--stdin"],
        vec!["rimz", "record", "Result", "--file", "entry.md"],
    ] {
        assert!(TeamsHarness::try_parse_from(args).is_ok());
    }
    for args in [
        vec!["rimz", "record"],
        vec!["rimz", "record", "Decisions"],
        vec!["rimz", "record", "Goal", "text", "--file", "entry.md"],
        vec!["rimz", "record", "Goal", "text", "--stdin"],
        vec!["rimz", "record", "Goal", "--stdin", "--file", "entry.md"],
        vec!["rimz", "record", "Goal", "text", "--team", "forge"],
    ] {
        assert!(
            TeamsHarness::try_parse_from(args)
                .ok()
                .and_then(|parsed| match parsed.args.command {
                    Some(TeamsSubcmd::Record(args)) => args.normalize().ok(),
                    _ => None,
                })
                .is_none()
        );
    }
    for text in ["--no-tests landed", "-x", "-"] {
        let Some(TeamsSubcmd::Record(args)) =
            parse_teams(&["rimz", "record", "Decisions", text]).command
        else {
            panic!("record verb");
        };
        assert_eq!(args.text.as_deref(), Some(text));
    }
    let Some(TeamsSubcmd::Record(args)) =
        parse_teams(&["rimz", "record", "Result", "--file", "entry.md"]).command
    else {
        panic!("record verb");
    };
    assert_eq!(args.text, None);
    assert_eq!(args.file.as_deref(), Some(std::path::Path::new("entry.md")));
    let Some(TeamsSubcmd::Record(args)) =
        parse_teams(&["rimz", "record", "Goal", "--stdin"]).command
    else {
        panic!("record verb");
    };
    assert!(args.stdin && args.text.is_none());
    let Some(TeamsSubcmd::Record(args)) =
        parse_teams(&["rimz", "record", "Goal", "--", "--stdin"]).command
    else {
        panic!("record verb");
    };
    assert_eq!(args.text.as_deref(), Some("--stdin"));
    let help = TeamsHarness::try_parse_from(["rimz", "record", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("rimz teams record [TEAM#LANE] <SECTION> <TEXT|--file <PATH>|--stdin>"));
    for section in ["Goal", "Decisions", "Result"] {
        assert!(help.contains(section), "{help}");
    }
}

#[test]
fn launch_guard_accepts_only_defined_teams_and_lists_choices() {
    let teams = rimz::config::TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        rimz::config::Team::default(),
    )]));

    validate_team_name("forge", &teams, &Default::default()).unwrap();
    let error = validate_team_name("missing", &teams, &Default::default()).unwrap_err();
    assert!(error.to_string().contains("configured teams: forge"));

    let error = validate_team_name("forge.reviewer", &teams, &Default::default()).unwrap_err();
    assert!(error.to_string().contains("rimz agents forge.reviewer"));
}

#[test]
fn profiles_parse_as_team_profile_listing() {
    let args = parse_teams(&["rimz", "profiles", "--json", "--path"]);
    assert!(matches!(
        args.command,
        Some(TeamsSubcmd::Profiles {
            json: true,
            path: true
        })
    ));
}

#[test]
fn flip_requires_progress_note_and_accepts_only_team_selection() {
    let args = parse_teams(&["rimz", "flip", "Implement", "note", "--team", "forge"]);
    let Some(TeamsSubcmd::Flip(args)) = args.command else {
        panic!("flip verb");
    };
    assert_eq!(args.stage, "Implement");
    assert_eq!(args.note, "note");
    assert_eq!(args.team.as_deref(), Some("forge"));
    assert!(TeamsHarness::try_parse_from(["rimz", "flip"]).is_err());
    for stage in ["Implement", "Done"] {
        assert!(TeamsHarness::try_parse_from(["rimz", "flip", stage]).is_err());
        assert!(TeamsHarness::try_parse_from(["rimz", "flip", stage, "note"]).is_ok());
    }
    for note in ["", " ", "\t\r\n", "\u{2003}"] {
        let error = TeamsHarness::try_parse_from(["rimz", "flip", "Plan", note]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(error.to_string().contains("nonblank progress note"));
    }
    let note = "  Ready.\r\nKeep the evidence.  ";
    let args = parse_teams(&["rimz", "flip", "Plan", note]);
    let Some(TeamsSubcmd::Flip(args)) = args.command else {
        panic!("flip verb");
    };
    assert_eq!(args.note, note);
    for argv in [
        ["rimz", "flip", "Plan", "--no-tests ran", "--team", "forge"],
        ["rimz", "flip", "--team", "forge", "Plan", "--no-tests ran"],
    ] {
        let Some(TeamsSubcmd::Flip(args)) = parse_teams(&argv).command else {
            panic!("flip verb");
        };
        assert_eq!(args.note, "--no-tests ran");
        assert_eq!(args.team.as_deref(), Some("forge"));
    }
    let Some(TeamsSubcmd::Flip(args)) =
        parse_teams(&["rimz", "flip", "Plan", "--", "--team"]).command
    else {
        panic!("flip verb");
    };
    assert_eq!(args.note, "--team");
    let help = TeamsHarness::try_parse_from(["rimz", "flip", "--help"]).unwrap_err();
    assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    assert!(
        help.to_string()
            .contains("Destination stage owned by the team, or Done")
    );
    let Some(TeamsSubcmd::Flip(args)) =
        parse_teams(&["rimz", "flip", "Plan", "note", "--steer"]).command
    else {
        panic!("flip")
    };
    assert!(args.normalize().is_err());
    for flag in ["-m", "--note", "-w", "--worktree"] {
        assert!(
            TeamsHarness::try_parse_from(["rimz", "flip", "Plan", "note", flag, "value"]).is_err()
        );
    }
}

#[test]
fn wait_parses_references_and_join_flags() {
    let args = parse_teams(&[
        "rimz",
        "wait",
        "forge#a",
        "forge#b",
        "--any",
        "--timeout",
        "5m",
        "--json",
    ]);
    let Some(TeamsSubcmd::Wait(args)) = args.command else {
        panic!("wait verb");
    };
    assert_eq!(args.references, ["forge#a", "forge#b"]);
    assert!(args.any && args.json);
    assert_eq!(args.timeout, Some(std::time::Duration::from_secs(300)));
    let args = parse_teams(&["rimz", "wait", "forge", "-w", "feat-x"]);
    let Some(TeamsSubcmd::Wait(args)) = args.command else {
        panic!("wait verb");
    };
    assert_eq!(args.worktree.as_deref(), Some("feat-x"));
    assert!(TeamsHarness::try_parse_from(["rimz", "wait"]).is_err());
}

#[test]
fn all_team_launch_doorways_parse_the_same_cohort_payload() {
    assert!(TeamsHarness::try_parse_from(["rimz", "forge", "--resume"]).is_err());
    let agents = AgentsHarness::try_parse_from([
        "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
    ])
    .expect("parse agents launch")
    .args;
    let bare = parse_teams(&[
        "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
    ]);
    let verb = parse_teams(&[
        "rimz", "launch", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
    ]);

    assert_eq!(agents.launch.spec.as_deref(), Some("forge"));
    assert_eq!(agents.launch.prompt.as_deref(), Some("ship"));
    assert_eq!(bare.name.as_deref(), Some("forge"));
    assert_eq!(bare.prompt.as_deref(), Some("ship"));
    assert_eq!(bare.launch, agents.launch.cohort);
    let Some(TeamsSubcmd::Launch(verb)) = verb.command else {
        panic!("launch verb");
    };
    assert_eq!(verb.name, "forge");
    assert_eq!(verb.prompt.as_deref(), Some("ship"));
    assert_eq!(verb.launch, bare.launch);
    for argv in [
        &["rimz", "forge", "--continue", "--isolation", "host"][..],
        &[
            "rimz",
            "launch",
            "forge",
            "--continue",
            "--isolation",
            "host",
        ],
    ] {
        let args = parse_teams(argv);
        let (launch, isolation) = match args.command {
            Some(TeamsSubcmd::Launch(args)) => (args.launch, args.isolation),
            _ => (args.launch, args.isolation),
        };
        assert!(launch.continue_cohort);
        assert_eq!(isolation, Some(rimz::config::Isolation::Host));
    }
    let resume = parse_teams(&[
        "rimz",
        "resume",
        "forge",
        "ship",
        "--isolation",
        "host",
        "--agent",
        "reviewer",
        "--model",
        "opus[1m]",
        "--effort",
        "high",
        "--fast",
        "--ask",
    ]);
    let Some(TeamsSubcmd::Resume(args)) = resume.command else {
        panic!("resume verb");
    };
    let args = args.into_agents_args();
    assert_eq!(args.launch.spec.as_deref(), Some("forge"));
    assert!(args.launch.cohort.continue_cohort);
    assert_eq!(args.launch.prompt.as_deref(), Some("ship"));
    assert_eq!(args.launch.overrides.agent.as_deref(), Some("reviewer"));
    assert_eq!(args.launch.overrides.model.as_deref(), Some("opus[1m]"));
    assert_eq!(args.launch.overrides.effort.as_deref(), Some("high"));
    assert!(args.launch.overrides.fast && !args.launch.overrides.no_fast);
    assert!(args.launch.overrides.ask);
    let resume = parse_teams(&["rimz", "resume", "forge", "--tier", "senior", "--yolo"]);
    let Some(TeamsSubcmd::Resume(tier_args)) = resume.command else {
        panic!("resume verb")
    };
    let tier_args = tier_args.into_agents_args();
    assert_eq!(
        tier_args.launch.overrides.tier,
        Some(rimz::config::tiers::ModelTier::Senior)
    );
    assert!(tier_args.launch.overrides.yolo);
    assert!(!tier_args.launch.overrides.fast && !tier_args.launch.overrides.no_fast);
    let resume = parse_teams(&["rimz", "resume", "forge", "--no-fast"]);
    let Some(TeamsSubcmd::Resume(off_args)) = resume.command else {
        panic!("resume verb")
    };
    let off = off_args.into_agents_args().launch.overrides;
    assert!(!off.fast && off.no_fast);
    assert_eq!(
        args.launch.overrides.isolation,
        Some(rimz::config::Isolation::Host)
    );
}

#[test]
fn fused_team_lane_feeds_the_canonical_worktree_argument() {
    let show = parse_teams(&["rimz", "show", "forge#feat-x"]);
    let stop = parse_teams(&["rimz", "stop", "forge#feat-x"]);
    let resume = parse_teams(&["rimz", "resume", "forge#feat-x"]);
    let bare = parse_teams(&["rimz", "forge#feat-x", "ship", "--fresh"]);
    assert!(bare.launch.fresh);

    let Some(TeamsSubcmd::Show { name, worktree, .. }) = show.command else {
        panic!("show verb");
    };
    assert_eq!(
        show_target(name, worktree).unwrap(),
        (Some("forge".to_owned()), Some("feat-x".to_owned()))
    );
    let Some(TeamsSubcmd::Stop(args)) = stop.command else {
        panic!("stop verb");
    };
    assert_eq!(
        team_lane(args.name, args.worktree).unwrap().1.as_deref(),
        Some("feat-x")
    );
    let Some(TeamsSubcmd::Resume(args)) = resume.command else {
        panic!("resume verb");
    };
    assert_eq!(
        team_lane(args.name, args.worktree).unwrap().1.as_deref(),
        Some("feat-x")
    );
    assert_eq!(
        team_lane(bare.name.unwrap(), bare.launch.worktree)
            .unwrap()
            .1
            .as_deref(),
        Some("feat-x")
    );
}

#[test]
fn fused_team_lane_rejects_missing_and_duplicate_lanes() {
    let missing = team_lane("forge#".to_owned(), None).unwrap_err();
    assert!(missing.to_string().contains("after `#`"));

    let duplicate = team_lane("forge#feat-x".to_owned(), Some("other".to_owned())).unwrap_err();
    assert!(duplicate.to_string().contains("given twice"));
}

#[test]
fn show_accepts_lane_only_targets() {
    for argv in [
        vec!["rimz", "show", "#feat-x"],
        vec!["rimz", "show", "-w", "feat-x"],
    ] {
        let Some(TeamsSubcmd::Show { name, worktree, .. }) = parse_teams(&argv).command else {
            panic!("show verb");
        };
        assert_eq!(
            show_target(name, worktree).unwrap(),
            (None, Some("feat-x".to_owned()))
        );
    }
    let missing = show_target(None, None).unwrap_err();
    assert!(missing.to_string().contains("'#lane'"));
    assert!(show_target(Some("#".into()), None).is_err());
    assert!(show_target(Some("#feat-x".into()), Some("other".into())).is_err());
}

#[cfg(unix)]
#[test]
fn board_worktree_takes_the_nearest_team_link_below_the_git_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("checkout");
    let sub = root.join("app");
    let deep = sub.join("src");
    let run = temp.path().join("run");
    std::fs::create_dir_all(&deep).expect("create checkout dirs");
    std::fs::create_dir_all(&run).expect("create run dir");
    std::fs::create_dir_all(sub.join(".rimz")).expect("create link parent");

    assert_eq!(board_context::nearest_team_checkout(&deep, &root), root);

    std::os::unix::fs::symlink(&run, sub.join(".rimz/team")).expect("link run");
    assert_eq!(board_context::nearest_team_checkout(&deep, &root), sub);
    assert_eq!(board_context::nearest_team_checkout(&root, &root), root);

    std::fs::remove_dir(&run).expect("drop run dir");
    assert_eq!(
        board_context::nearest_team_checkout(&deep, &root),
        root,
        "a dangling link is no team board"
    );
}
