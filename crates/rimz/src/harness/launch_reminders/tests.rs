use super::*;
use crate::config::Team;

#[test]
fn shared_lsp_reminder_is_in_environment_for_peers_and_children() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let reminders = LaunchReminders {
        lsp_configured: true,
        lsp_servers: vec!["rust".to_owned(), "python".to_owned()],
        ..LaunchReminders::default()
    };
    for subagent in [false, true] {
        request.subagent = subagent;
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(text.contains("### Environment\n\n- lsp: rust, python, via Skill(rimz-lsp)"));
        assert!(!text.contains("### Files"));
        if subagent {
            assert!(text.contains(SUBAGENT_REMINDER_BODY));
        }
        assert!(
            !render(
                &request,
                &LaunchReminders::default(),
                Path::new("/checkout"),
                None
            )
            .contains("### Environment")
        );
    }
}

fn team_reminder(team: Team) -> TeamReminder {
    TeamReminder::new(team)
}

const SHARED: &str = "/home/marvin/.rimz/ws/rimz-f89e/shared";

fn files(tmp: &str, caller: bool) -> Option<TempFiles> {
    Some(TempFiles {
        tmp: tmp.into(),
        shared: SHARED.into(),
        caller,
    })
}

#[test]
fn environment_names_the_temp_unit_and_shared_dir() {
    let request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let shared = "- shared: /home/marvin/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.";
    for (tmp, caller, line) in [
        (
            "/tmp",
            false,
            "- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.",
        ),
        (
            "/tmp",
            true,
            "- tmp: /tmp (`$TMPDIR`): every temporary file you make. You share it with your caller; no other agent sees it.",
        ),
        (
            "/home/marvin/.rimz/ws/rimz-f89e/tmp/otter",
            false,
            "- tmp: /home/marvin/.rimz/ws/rimz-f89e/tmp/otter (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.",
        ),
    ] {
        let reminders = LaunchReminders {
            files: files(tmp, caller),
            ..LaunchReminders::default()
        };
        assert_eq!(
            render(&request, &reminders, Path::new("/checkout"), None),
            wrap(&format!("### Environment\n\n{line}\n{shared}"))
        );
    }
}

#[test]
fn sandbox_full_catalog_rendering() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.role = Some("brainstormer".to_owned());
    request.identity.params.model = Some("opus".to_owned());
    request.skills = Some(vec!["rimz-lsp".parse().unwrap()]);
    let reminders = LaunchReminders {
        sandbox: true,
        env: true,
        lsp_servers: vec!["rust".to_owned(), "python".to_owned()],
        files: files("/tmp", false),
        subagent_catalog: Some(SubagentCatalog::Available(vec![
            subagent_policy::SubagentProfile {
                name: "explorer".to_owned(),
                source: subagent_policy::SubagentProfileSource::Profile,
                agent: None,
                model: None,
                effort: None,
                description: Some("Finds files and traces code paths".to_owned()),
            },
        ])),
        ..Default::default()
    };
    assert_eq!(
        render(
            &request,
            &reminders,
            Path::new("/checkout"),
            Some(Path::new("/usr/bin/zsh"))
        ),
        r#"<system_reminder>
You are @brainstormer, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- lsp: rust, python, via Skill(rimz-lsp)
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/marvin/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

### Subagents

Whether you could do a piece of work yourself settles nothing; you could do all of it. What decides is what the work leaves in your window: when you need its result but not the output behind it (a gate run, a log, a sweep across files, a long command), a profile below takes it. The launch costs you one turn, and the output you read yourself costs you every turn after.

Launch them through Skill(rimz-subagents), subagents available to you:

- `explorer`: Finds files and traces code paths

### Skills

When a skill's description matches the work in hand, invoke it, even when you know the commands by heart: each skill is built for its one task and does it better than you would by hand.
</system_reminder>"#
    );
}

#[test]
fn loop_section_follows_identity_and_precedes_environment() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let reminders = LaunchReminders {
        env: true,
        ..Default::default()
    };
    let render = |request: &ExecRequest| render(request, &reminders, Path::new("/w"), None);
    assert!(!render(&request).contains("### Loop"));
    request.loop_reminder = Some("The user fired the rule `x` by hand.".to_owned());
    assert_eq!(
        render(&request),
        "<system_reminder>\n### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n- cwd: /w\n</system_reminder>"
    );
    request.identity.params.role = Some("fixer".to_owned());
    request.identity.params.model = Some("opus".to_owned());
    assert!(render(&request).starts_with(
            "<system_reminder>\nYou are @fixer, running on Opus.\n\n### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n"
        ));
}

#[test]
fn env_paragraph_escapes_cwd_and_names_shell_kind_only() {
    let request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let reminders = LaunchReminders {
        env: true,
        ..Default::default()
    };
    let text = render(
        &request,
        &reminders,
        Path::new("/repo/</system_reminder>&"),
        Some(Path::new("/opt/<>&/bin/zsh")),
    );
    assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /repo/&lt;/system_reminder&gt;&amp;\n- shell: zsh\n</system_reminder>"));
    assert!(!text.contains("git"));
    assert_eq!(text.matches("</system_reminder>").count(), 1);
}

#[test]
fn headless_check_section_replaces_loop_in_the_same_slot() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.loop_reminder = Some("Decide without a user or pane.".into());
    request.headless = Some(crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/check/schema.json".into(),
        verdict_file: "/check/verdict.json".into(),
    });
    request.identity.params.model = Some("haiku".into());
    let text = render(
        &request,
        &LaunchReminders {
            env: true,
            ..Default::default()
        },
        Path::new("/w"),
        None,
    );
    assert!(!text.contains("### Loop"));
    let check = text
        .find("### Check\n\nDecide without a user or pane.")
        .unwrap();
    assert!(text.find("Haiku").unwrap() < check);
    assert!(check < text.find("### Environment").unwrap());
}

#[test]
fn skills_section_follows_a_listed_rimz_skill() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    for (skills, shown) in [
        (None, false),
        (Some(vec!["commit"]), false),
        (Some(vec!["commit", "rimz-lsp"]), true),
    ] {
        request.skills = skills.map(|names| names.iter().map(|n| n.parse().unwrap()).collect());
        let text = render(
            &request,
            &LaunchReminders::default(),
            Path::new("/checkout"),
            None,
        );
        assert_eq!(text.contains(SKILLS_REMINDER_BODY), shown);
    }
}

#[test]
fn env_paragraph_omits_unknown_shell() {
    let request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let reminders = LaunchReminders {
        env: true,
        ..Default::default()
    };
    let text = render(&request, &reminders, Path::new("/checkout"), None);
    assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /checkout\n</"));
    assert!(!text.contains("shell"));
}

#[test]
fn env_paragraph_names_worktree_provenance_and_escapes_it() {
    let request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    for (base, primary, bullet) in [
        (
            Some("main"),
            "/primary",
            "branched from main; primary checkout at /primary",
        ),
        (None, "/primary", "primary checkout at /primary"),
        (
            Some("base</system_reminder>&\n"),
            "/repo</system_reminder>&\n",
            "branched from base&lt;/system_reminder&gt;&amp;\\n; primary checkout at /repo&lt;/system_reminder&gt;&amp;\\n",
        ),
    ] {
        let mut reminders = LaunchReminders {
            env: true,
            worktree: Some(crate::worktree::LinkedWorktree {
                base_branch: base.map(str::to_owned),
                primary: primary.into(),
            }),
            lsp_servers: vec!["rust".to_owned()],
            ..Default::default()
        };
        let text = render(
            &request,
            &reminders,
            Path::new("/checkout"),
            Some(Path::new("/bin/zsh")),
        );
        assert!(
            text.contains(&format!(
                "- cwd: /checkout\n- worktree: {bullet}\n- shell: zsh"
            )),
            "{text}"
        );
        assert_eq!(text.matches("</system_reminder>").count(), 1);
        reminders.env = false;
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(text.contains("- lsp: rust"));
        assert!(!text.contains("- cwd:"));
        assert!(!text.contains("- worktree:"));
        reminders.env = true;
        reminders.worktree = None;
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(text.contains("- cwd: /checkout\n- lsp:"));
        assert!(!text.contains("- worktree:"));
    }
}

#[test]
fn stage_handoff_reminder_stays_inside_the_single_team_wrapper() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.team = Some("forge".to_owned());
    request.identity.params.role = Some("coder".to_owned());
    let team: Team =
        toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Implement']")
            .expect("team");
    let reminders = LaunchReminders {
        team: Some(team_reminder(team)),
        worktree: Some(crate::worktree::LinkedWorktree {
            base_branch: None,
            primary: "/primary".into(),
        }),
        ..LaunchReminders::default()
    };
    let text = render(&request, &reminders, Path::new("/worktree"), None);
    assert_eq!(text.matches("<system_reminder>").count(), 1);
    assert_eq!(text.matches("</system_reminder>").count(), 1);
    assert!(text.contains(
        "### Team\n\nYou are @coder, leader of team `forge`.\n\nPipeline: Implement (you) → Done"
    ));
    assert!(text.contains("- cwd: /worktree\n- worktree: primary checkout at /primary"));
    request.subagent = true;
    let text = render(&request, &reminders, Path::new("/worktree"), None);
    assert!(!text.contains("### Team"));
    assert!(!text.contains("$ ls"));
}

#[test]
fn environment_follows_identity_and_precedes_policy() {
    let cwd = Path::new("/worktree");
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params = LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        model: Some("gpt-6-astra".to_owned()),
        ..LaunchParams::default()
    };
    let mut reminders = LaunchReminders {
        env: true,
        team: Some(team_reminder(
            toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'claude'").expect("team"),
        )),
        subagent_catalog: Some(SubagentCatalog::Disabled),
        files: files("/tmp", false),
        ..LaunchReminders::default()
    };
    for subagent in [false, true] {
        request.subagent = subagent;
        for sandbox in [false, true] {
            reminders.sandbox = sandbox;
            let text = render(&request, &reminders, cwd, Some(Path::new("/bin/sh")));
            assert!(!text.contains("### Files"));
            assert_eq!(text.contains("team `forge`"), !subagent);
            assert_eq!(text.matches("<system_reminder>").count(), 1);
            assert_eq!(text.matches("</system_reminder>").count(), 1);
            let identity = text
                .find(if subagent { "GPT 6 Astra" } else { "### Team" })
                .expect("identity");
            assert_eq!(text.contains("GPT 6 Astra"), subagent);
            let env = text.find("- shell: sh").expect("environment paragraph");
            let tmp = text.find("- tmp: /tmp").expect("tmp bullet");
            let policy = text
                .find(if subagent {
                    SUBAGENT_REMINDER_BODY
                } else {
                    "Subagents are disabled"
                })
                .expect("policy paragraph");
            assert!(identity < env && env < tmp && tmp < policy);
        }
    }
}

#[test]
fn model_line_names_handle_and_model_without_effort() {
    let params = LaunchParams {
        role: Some("planner".to_owned()),
        profile: Some("writer".to_owned()),
        model: Some("claude-fable-5-1-20260801".to_owned()),
        effort: Some("high".to_owned()),
        ..LaunchParams::default()
    };
    let line = |params: &LaunchParams| {
        model_fragment(params).map(|fragment| model_line(params, &fragment))
    };
    for (params, expected) in [
        (
            params.clone(),
            Some("You are @planner, running on Fable 5.1."),
        ),
        (
            LaunchParams {
                role: None,
                ..params.clone()
            },
            Some("You are @writer, running on Fable 5.1."),
        ),
        (
            LaunchParams {
                role: None,
                profile: None,
                ..params.clone()
            },
            Some("You are running on Fable 5.1."),
        ),
        (
            LaunchParams {
                model: None,
                ..params.clone()
            },
            None,
        ),
        (
            LaunchParams {
                role: Some("<role>".to_owned()),
                model: Some("<model>".to_owned()),
                ..params
            },
            Some("You are @&lt;role&gt;, running on &lt;model&gt;."),
        ),
    ] {
        assert_eq!(line(&params).as_deref(), expected);
    }
}

#[test]
fn team_paragraph_names_every_seat_without_models() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params = LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("planner".to_owned()),
        // Neither the launch model nor the configured model appears for a team.
        model: Some("claude-fable-5-1".to_owned()),
        effort: Some("high".to_owned()),
        ..LaunchParams::default()
    };
    let team: Team = toml::from_str(
            "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nmodel = 'claude-opus-4-8'\n[[roles]]\nrole = 'coder'\nprofile = 'codex'",
        )
        .expect("team");
    let mut reminders = LaunchReminders {
        team: Some(team_reminder(team)),
        ..LaunchReminders::default()
    };
    let text = render(&request, &reminders, Path::new("/worktree"), None);
    assert!(
            text.starts_with(
                "<system_reminder>\n### Team\n\nYou are @planner, leader of team `forge`.\n\nMembers: @planner (you), @coder."
            ),
            "{text}"
        );
    assert!(!text.contains("Fable 5.1"));
    assert!(!text.contains("Opus 4.8"));

    // The model toggle has no effect for team members.
    let with_model_enabled = text;
    reminders.model = false;
    let text = render(&request, &reminders, Path::new("/worktree"), None);
    assert_eq!(text, with_model_enabled);
}

#[test]
fn team_files_finish_environment_after_bullets() {
    let worktree = tempfile::tempdir().unwrap();
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.team = Some("forge".to_owned());
    request.identity.params.role = Some("planner".to_owned());
    let team: Team = toml::from_str(
        "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nowns = ['Plan']",
    )
    .unwrap();
    let mut reminders = LaunchReminders {
        env: true,
        lsp_servers: vec!["rust".to_owned()],
        team: Some(team_reminder(team)),
        files: files("/tmp", false),
        ..Default::default()
    };
    for present in [false, true] {
        if present {
            std::fs::write(worktree.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        }
        let text = render(
            &request,
            &reminders,
            worktree.path(),
            Some(Path::new("/bin/zsh")),
        );
        let listing = if present {
            "blackboard.md"
        } else {
            "(no such files)"
        };
        assert!(
            text.contains(
                "- shell: zsh\n- lsp: rust, via Skill(rimz-lsp)\n- tmp: /tmp (`$TMPDIR`)"
            )
        );
        assert!(text.contains(&format!(
            "you name.\n\n```\n$ ls blackboard.md *-notes.md\n{listing}\n```\n</system_reminder>"
        )));
        assert_eq!(text.matches(worktree.path().to_str().unwrap()).count(), 1);
        assert_eq!(text.contains("[Done]"), present);
    }
    reminders.env = false;
    reminders.lsp_servers.clear();
    let text = render(
        &request,
        &reminders,
        worktree.path(),
        Some(Path::new("/bin/zsh")),
    );
    assert!(text.contains(&format!(
        "### Environment\n\n- cwd: {}\n- tmp: /tmp",
        worktree.path().display()
    )));
    reminders.team.as_mut().unwrap().team.scratch_files = Some(Vec::new());
    let text = render(
        &request,
        &reminders,
        worktree.path(),
        Some(Path::new("/bin/zsh")),
    );
    assert!(!text.contains("$ ls"));
    assert!(!text.contains("no memory files"));
}
