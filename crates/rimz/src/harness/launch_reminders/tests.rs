use super::*;
use crate::config::Team;

#[test]
fn empty_directory_target_explains_the_recorded_reason_for_each_kind() {
    let cwd = crate::disk::paths::empty_dir_root().join("room/child");
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (kind, files) in [
        ("claude", "hooks, settings, and CLAUDE.md/AGENTS.md"),
        ("codex", ".codex config, hooks, and AGENTS.md"),
    ] {
        for untrusted in [true, false] {
            let mut request =
                ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
            request.identity.params = serde_json::from_value(serde_json::json!({
                "checkout": "/target", "add_dirs": ["/target", "/other"],
                "untrusted_checkout": untrusted
            }))
            .unwrap();
            let text = render(&request, &LaunchReminders::default(), &cwd, None);
            let provider = if kind == "claude" { "Claude" } else { "Codex" };
            let reason = if untrusted {
                format!("{provider} has no trust decision for this folder, so you started")
            } else {
                "Your caller started you".to_owned()
            };
            let sentence = format!(
                "(Your caller asked you to work here. {reason} in an empty directory of your \
                 own, {}its {files} were not loaded. Read, search, and cd into it freely; \
                 read its instruction files yourself if the task needs them. If the task \
                 needs code run inside it, stop and say so in your report.)",
                if untrusted { "and " } else { "so " }
            );
            actual.push((
                text.contains(&format!(
                    "- cwd: {}\n- target: /target\n  {sentence}\n- added dir: /other",
                    cwd.display()
                )),
                text.contains("- added dir: /target"),
            ));
            expected.push((true, false));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn empty_directory_target_on_a_workspace_dir_kind_says_it_loaded() {
    let cwd = crate::disk::paths::empty_dir_root().join("room/child");
    let mut request = ExecRequest::bare_launch(
        crate::ids::AgentKind::new_unchecked("antigravity"),
        Vec::new(),
    );
    request.identity.params = serde_json::from_value(serde_json::json!({
        "checkout": "/target", "add_dirs": ["/target", "/other"]
    }))
    .unwrap();
    let text = render(&request, &LaunchReminders::default(), &cwd, None);
    assert!(
        text.contains(&format!(
            "- cwd: {}\n- target: /target\n  (Your caller asked you to work here. It started \
             you in an empty directory of your own, and Antigravity opened this one as a \
             workspace, loading its instructions, rules, and skills and running its hooks.)\n\
             - added dir: /other",
            cwd.display()
        )),
        "{text}"
    );
    assert!(!text.contains("not loaded"), "{text}");
}

#[test]
fn added_directories_render_without_the_environment_switch_and_escape_paths() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params = serde_json::from_value(serde_json::json!({
        "add_dirs": ["/target\n</system_reminder>", "/second"]
    }))
    .unwrap();
    let text = render(
        &request,
        &LaunchReminders::default(),
        Path::new("/cwd"),
        None,
    );
    assert!(text.contains("- added dir: /target"), "{text}");
    assert!(text.contains("- added dir: /second"), "{text}");
    assert!(!text.contains("/target\n</system_reminder>"), "{text}");
}

#[test]
fn missing_skill_tool_omits_skill_sections_but_keeps_child_body() {
    let available = SubagentCatalog::Available(vec![subagent_policy::SubagentProfile {
        name: "explorer".into(),
        source: subagent_policy::SubagentProfileSource::Profile,
        agent: Some("claude".into()),
        model: None,
        effort: None,
        description: None,
    }]);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for skills in [
        None,
        Some(
            ["rimz-lsp", "rimz-subagents"]
                .map(|name| name.parse().unwrap())
                .to_vec(),
        ),
    ] {
        for catalog in [available.clone(), SubagentCatalog::Disabled] {
            for subagent in [false, true] {
                let mut request = ExecRequest::bare_launch(
                    crate::ids::AgentKind::new_unchecked("claude"),
                    Vec::new(),
                );
                request.skills.clone_from(&skills);
                request.no_skill_tool = true;
                request.subagent = subagent;
                let reminders = LaunchReminders {
                    lsp_configured: true,
                    lsp_servers: vec!["rust".into()],
                    subagent_catalog: Some(catalog.clone()),
                    ..Default::default()
                };
                let text = render(&request, &reminders, Path::new("/checkout"), None);
                let case = format!("{skills:?} {catalog:?} child={subagent}");
                actual.push((
                    case.clone(),
                    text.contains("### Skills"),
                    text.contains("- lsp:"),
                    text.contains("### Subagents"),
                    text.contains(SUBAGENT_REMINDER_BODY),
                ));
                expected.push((case, false, false, subagent, subagent));
            }
        }
    }
    assert_eq!(actual, expected);
}

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
        assert!(text.contains("### Environment\n\n- lsp: rust, python, via skill(rimz-lsp)"));
        assert!(!text.contains("### RimZ capabilities"));
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

#[test]
fn lsp_bullet_names_the_mcp_tool_when_live_and_the_skill_when_only_it_is_callable() {
    let mut actual = Vec::new();
    for (kind, tool) in [
        ("claude", false),
        ("claude", true),
        ("antigravity", false),
        ("antigravity", true),
    ] {
        for skills in [None, Some(Vec::new())] {
            let mut request =
                ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
            request.skills = skills;
            let reminders = LaunchReminders {
                lsp_configured: true,
                lsp_servers: vec!["rust".to_owned()],
                mcp_server: tool.then(|| ("/bin/rimz".into(), Vec::new())),
                ..LaunchReminders::default()
            };
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            actual.push((
                kind,
                tool,
                request.skills.is_none(),
                text.lines()
                    .find(|line| line.starts_with("- lsp:"))
                    .map(str::to_owned),
            ));
        }
    }
    let tool = Some("- lsp: rust, via mcp(lsp)".to_owned());
    let skill = Some("- lsp: rust, via skill(rimz-lsp)".to_owned());
    assert_eq!(
        actual,
        [
            ("claude", false, true, skill.clone()),
            ("claude", false, false, None),
            ("claude", true, true, tool.clone()),
            ("claude", true, false, tool.clone()),
            ("antigravity", false, true, skill),
            ("antigravity", false, false, None),
            ("antigravity", true, true, tool.clone()),
            ("antigravity", true, false, tool),
        ]
    );
}

#[test]
fn a_served_subagent_tool_keeps_the_subagents_policy_and_a_child_is_not_served_it() {
    let catalog = SubagentCatalog::Available(vec![subagent_policy::SubagentProfile {
        name: "explorer".into(),
        source: subagent_policy::SubagentProfileSource::Profile,
        agent: None,
        model: None,
        effort: None,
        description: None,
    }]);
    let mut actual = Vec::new();
    for (kind, skills) in [
        ("claude", None),
        ("claude", Some(Vec::new())),
        ("antigravity", Some(Vec::new())),
    ] {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
        request.skills = skills;
        let reminders = LaunchReminders {
            mcp_server: Some(("/bin/rimz".into(), Vec::new())),
            subagent_catalog: Some(catalog.clone()),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        actual.push((
            kind,
            reminders.disable_native_subagents(&request),
            text.lines()
                .find(|line| line.starts_with("Launch them through"))
                .map(str::to_owned),
        ));
        // A launch that cannot delegate is served neither tool that hands a launch on: a
        // booking's fire would launch a top-level agent past its refusals.
        let cannot_delegate = |request: &ExecRequest| {
            let served = reminders.mcp_capabilities(request);
            assert!(!served.contains(&Capability::Subagents), "{served:?}");
            assert!(!served.contains(&Capability::Loop), "{served:?}");
            assert!(served.contains(&Capability::Wait), "{served:?}");
            assert!(served.contains(&Capability::Workflow), "{served:?}");
        };
        request.subagent = true;
        cannot_delegate(&request);
        request.subagent = false;
        request.identity.params.workflow = Some(crate::agents::WorkflowAttemptRef {
            run: crate::ids::WorkflowRunId::new(),
            seq: 1,
            role: crate::agents::WorkflowAttemptRole::Judge,
        });
        cannot_delegate(&request);
    }
    let via = Some(
        "Launch them through mcp(subagent). `subagent(x)` in any text means the profile `x` in \
         this list:"
            .to_owned(),
    );
    assert_eq!(
        actual,
        [
            ("claude", true, via.clone()),
            ("claude", true, via.clone()),
            ("antigravity", true, via),
        ]
    );
}

#[test]
fn a_profile_that_allows_no_subagents_gets_no_subagents_paragraph() {
    let mut actual = Vec::new();
    let grant = || {
        Some(vec![
            "rimz-message".parse().unwrap(),
            "rimz-subagents".parse().unwrap(),
        ])
    };
    for (skills, server, rimz_capabilities) in [
        (
            Some(vec![
                "rimz-message".parse().unwrap(),
                "rimz-subagents".parse().unwrap(),
            ]),
            false,
            None,
        ),
        (None, false, None),
        (None, true, None),
        (None, false, grant()),
        (None, true, grant()),
    ] {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.skills = skills;
        request.rimz_capabilities = rimz_capabilities;
        let reminders = LaunchReminders {
            mcp_server: server.then(|| ("/bin/rimz".into(), Vec::new())),
            subagent_catalog: Some(SubagentCatalog::NoneAllowed),
            rimz_skills: rimz_skills(&["rimz-message", "rimz-subagents"]),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(!text.contains("### RimZ capabilities"), "{text}");
        let capabilities: Vec<_> = reminders
            .capability_lines(&request)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        actual.push((
            text.contains("### Subagents"),
            capabilities.iter().any(|name| name == "rimz-message")
                && !capabilities.iter().any(|name| name == "rimz-subagents"),
            reminders
                .mcp_capabilities(&request)
                .contains(&Capability::Subagents),
            crate::agents::skills::launch_callable(
                request.skills.as_deref(),
                &reminders.withheld_skills(&request),
                request.rimz_capabilities.as_deref(),
                "rimz-subagents",
            ),
        ));
    }
    assert_eq!(actual, [(false, true, false, false); 5]);
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
            wrap(&format!(
                "### Environment\n\n{line}\n{shared}\n\n{SKILLS_REMINDER_BODY}"
            ))
        );
    }
}

#[test]
fn sandbox_full_catalog_rendering() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.role = Some("coder".to_owned());
    request.identity.params.model = Some("opus".to_owned());
    let mut reminders = LaunchReminders {
        sandbox: true,
        env: true,
        lsp_servers: vec!["rust".to_owned()],
        files: Some(TempFiles {
            tmp: "/tmp".into(),
            shared: "/home/you/.rimz/ws/room/shared".into(),
            caller: false,
        }),
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
    let full = r#"<system_reminder>
You are @coder, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- lsp: rust, via skill(rimz-lsp)
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/you/.rimz/ws/room/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

### Subagents

Whether you could do a piece of work yourself settles nothing; you could do all of it. What decides is what the work leaves in your window: when you need its result but not the output behind it (a gate run, a log, a sweep across files, a long command), a profile below takes it. The launch costs you one turn, and the output you read yourself costs you every turn after.

Launch them through skill(rimz-subagents). `subagent(x)` in any text means the profile `x` in this list:

- `explorer`: Finds files and traces code paths

### Skills

When a skill's description matches the work in hand, invoke it, even when you know the commands by heart: each skill is built for its one task and does it better than you would by hand.
</system_reminder>"#;
    let skills_only = r#"<system_reminder>
You are @coder, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/you/.rimz/ws/room/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

### Skills

When a skill's description matches the work in hand, invoke it, even when you know the commands by heart: each skill is built for its one task and does it better than you would by hand.
</system_reminder>"#;
    let environment_only = r#"<system_reminder>
You are @coder, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/you/.rimz/ws/room/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.
</system_reminder>"#;
    let disabled = r#"<system_reminder>
You are @coder, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/you/.rimz/ws/room/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

### Subagents

No subagent profile is configured for you, so none is listed here. Do the work yourself with your direct tools.

### Skills

When a skill's description matches the work in hand, invoke it, even when you know the commands by heart: each skill is built for its one task and does it better than you would by hand.
</system_reminder>"#;
    for (skills, disable, expected) in [
        (Some(vec!["rimz-subagents", "rimz-lsp"]), false, full),
        (None, false, full),
        (Some(vec!["rimz-message", "commit"]), false, skills_only),
        (Some(vec!["commit"]), false, environment_only),
        (Some(vec!["rimz-subagents"]), true, disabled),
    ] {
        request.skills = skills
            .as_ref()
            .map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
        if disable {
            reminders.subagent_catalog = Some(SubagentCatalog::Disabled);
        }
        assert_eq!(
            render(
                &request,
                &reminders,
                Path::new("/checkout"),
                Some(Path::new("/usr/bin/zsh"))
            ),
            expected,
            "skills: {skills:?}"
        );
    }
}

#[test]
fn a_result_file_child_is_told_its_file_and_a_capture_child_its_final_reply() {
    let w1 = "### Subagents\n\nYou are a subagent: a supervised child launched by another agent to complete the task you were given. The task is scoped to this one run, so do the work yourself. The agent that launched you reads only your result file: write your whole result to `/tmp/child.output` before you end your turn, then end the turn with one line saying it is written. A turn that ends without a new result in that file gives your launcher nothing. Send your launcher no progress notes: each one stops its wait and costs it a turn. You cannot ask the user. When you need an answer to go on, write the question to the file as your result and end your turn: your launcher answers. When your launcher sends you another task, write its result to the same file. When the user types in this pane, answer in this pane.";
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.skills = Some(Vec::new());
    let told = |request: &ExecRequest, result_file: Option<&str>| {
        let reminders = LaunchReminders {
            result_file: result_file.map(PathBuf::from),
            ..Default::default()
        };
        render(request, &reminders, Path::new("/checkout"), None)
    };
    request.subagent = true;
    assert_eq!(told(&request, Some("/tmp/child.output")), wrap(w1));
    assert_eq!(told(&request, None), wrap(SUBAGENT_REMINDER_BODY));
    request.subagent = false;
    assert!(!told(&request, Some("/tmp/child.output")).contains("### Subagents"));
}

#[test]
fn child_policy_is_fixed_regardless_of_skills_or_catalog() {
    let body = "### Subagents\n\nYou are a subagent: a supervised child launched by another agent to complete the task you were given. The task is scoped to this one run, so do the work yourself. Report the result in your final response: it is the answer your caller reads once this run settles. Send your caller no progress notes: each one stops its wait and costs it a turn. When you need its answer to go on, end your turn with the question as your final response.";
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    request.subagent = true;
    let reminders = LaunchReminders {
        subagent_catalog: Some(SubagentCatalog::Disabled),
        ..Default::default()
    };
    for (skills, skills_section) in [
        (Some(Vec::<&str>::new()), false),
        (None, true),
        (Some(vec!["rimz-subagents", "rimz-lsp"]), true),
    ] {
        request.skills =
            skills.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
        let expected = if skills_section {
            format!("{body}\n\n{SKILLS_REMINDER_BODY}")
        } else {
            body.to_owned()
        };
        assert_eq!(
            render(&request, &reminders, Path::new("/checkout"), None),
            wrap(&expected)
        );
    }
    assert_eq!(subagent_reminder(), wrap(body));
}

#[test]
fn unlisted_lsp_does_not_open_environment() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    let reminders = LaunchReminders {
        lsp_configured: true,
        lsp_servers: vec!["rust".to_owned()],
        ..Default::default()
    };
    for subagent in [false, true] {
        request.subagent = subagent;
        for skills in [vec!["commit"], Vec::new()] {
            request.skills = Some(skills.iter().map(|name| name.parse().unwrap()).collect());
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            assert!(!text.contains("### Environment"), "{text}");
            assert!(!text.contains("skill(rimz-lsp)"), "{text}");
        }
    }
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
    request.reminder_section = Some(ReminderSection::Loop(
        "The user fired the rule `x` by hand.".to_owned(),
    ));
    assert_eq!(
        render(&request),
        wrap(&format!(
            "### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n- cwd: /w\n\n{SKILLS_REMINDER_BODY}"
        ))
    );
    request.identity.params.role = Some("fixer".to_owned());
    request.identity.params.model = Some("opus".to_owned());
    assert!(render(&request).starts_with(
            "<system_reminder>\nYou are @fixer, running on Opus.\n\n### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n"
        ));
    request.reminder_section = Some(ReminderSection::Workflow("A workflow step.".into()));
    assert!(render(&request).starts_with(
        "<system_reminder>\nYou are @fixer, running on Opus.\n\n### Workflow\n\nA workflow step.\n\n### Environment\n\n"
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
    assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /repo/&lt;/system_reminder&gt;&amp;\n- shell: zsh\n\n### Skills"));
    assert!(!text.contains("git"));
    assert_eq!(text.matches("</system_reminder>").count(), 1);
}

#[test]
fn skills_section_follows_a_listed_rimz_skill() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
    for (skills, tool, shown) in [
        (None, false, true),
        (Some(vec![]), false, false),
        (Some(vec!["commit"]), false, false),
        (Some(vec!["commit", "rimz-lsp"]), false, true),
        (None, true, true),
        (Some(vec!["commit", "rimz-lsp"]), true, false),
        (Some(vec!["commit", "rimz-lsp", "rimz-commit"]), true, true),
    ] {
        request.skills = skills.map(|names| names.iter().map(|n| n.parse().unwrap()).collect());
        let reminders = LaunchReminders {
            mcp_server: tool.then(|| ("/bin/rimz".into(), Vec::new())),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert_eq!(
            text.contains(SKILLS_REMINDER_BODY),
            shown,
            "{:?} tool={tool}",
            request.skills
        );
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
    assert!(
        text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /checkout\n\n### Skills")
    );
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
                from_pr: None,
                marker_unreadable: false,
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
    let team: Team = toml::from_str(
        "leader = 'coder'\n[[roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Implement']",
    )
    .expect("team");
    let reminders = LaunchReminders {
        team: Some(team_reminder(team)),
        worktree: Some(crate::worktree::LinkedWorktree {
            base_branch: None,
            primary: "/primary".into(),
            from_pr: None,
            marker_unreadable: false,
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
            toml::from_str("leader = 'coder'\n[[roles]]\nrole = 'coder'\nprofile = 'claude'")
                .expect("team"),
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
                    "No subagent profile is configured"
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
        model_fragment(params).map(|fragment| model_line(params, None, &fragment))
    };
    for (params, expected) in [
        (
            params.clone(),
            Some("You are @planner, running on Claude Fable 5.1."),
        ),
        (
            LaunchParams {
                role: None,
                ..params.clone()
            },
            Some("You are @writer, running on Claude Fable 5.1."),
        ),
        (
            LaunchParams {
                role: None,
                profile: None,
                ..params.clone()
            },
            Some("You are running on Claude Fable 5.1."),
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
    // The agent's own name joins its handle when the two differ.
    let params = LaunchParams {
        role: Some("planner".to_owned()),
        ..LaunchParams::default()
    };
    assert_eq!(
        model_line(&params, Some("round-binder"), "on Fable 5.1"),
        "You are @planner (@round-binder), running on Fable 5.1."
    );
    assert_eq!(
        model_line(&params, Some("planner"), "on Fable 5.1"),
        "You are @planner, running on Fable 5.1."
    );
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
            std::fs::create_dir_all(worktree.path().join(".rimz/team")).unwrap();
            std::fs::write(
                worktree.path().join(".rimz/team/blackboard.md"),
                "Stage: Done\n",
            )
            .unwrap();
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
                "- shell: zsh\n- lsp: rust, via skill(rimz-lsp)\n- tmp: /tmp (`$TMPDIR`)"
            )
        );
        assert!(text.contains(&format!(
            "you name.\n\n```\n$ ls .rimz/team/*.md\n{listing}\n```\n\n{SKILLS_REMINDER_BODY}\n</system_reminder>"
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
    reminders
        .team
        .as_mut()
        .unwrap()
        .team
        .roles
        .iter_mut()
        .for_each(|role| role.owns.clear());
    let text = render(
        &request,
        &reminders,
        worktree.path(),
        Some(Path::new("/bin/zsh")),
    );
    assert!(!text.contains("$ ls"));
    assert!(!text.contains("no memory files"));
}

fn rimz_skills(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn capability_lines_use_short_form_and_reminder_omits_rimz_capabilities_section() {
    let mut actual = Vec::new();
    for kind in ["claude", "antigravity"] {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
        let reminders = LaunchReminders {
            mcp_server: Some(("/bin/rimz".into(), Vec::new())),
            rimz_skills: rimz_skills(&["rimz-lsp", "rimz-message", "rimz-wait"]),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert_eq!(text.matches("<system_reminder>").count(), 1, "{text}");
        assert!(!text.contains("### RimZ capabilities"), "{text}");
        actual.push(reminders.capability_lines(&request));
    }
    // Claude withholds the skills its served tools stand for; Antigravity on the host cannot,
    // so it has both, and the tool wins. Both kinds render the same short form with no server
    // name.
    let control = crate::mcp::catalog::TOOLS
        .iter()
        .filter(|tool| tool.capability == Some(Capability::Control))
        .map(|tool| format!("mcp({})", tool.name))
        .collect::<Vec<_>>()
        .join(", ");
    let served = vec![
        ("rimz-lsp".to_owned(), "mcp(lsp)".to_owned()),
        ("rimz-wait".to_owned(), "mcp(wait)".to_owned()),
        ("rimz-subagents".to_owned(), "mcp(subagent)".to_owned()),
        ("rimz-agents".to_owned(), "mcp(agent)".to_owned()),
        ("rimz-teams".to_owned(), "mcp(team)".to_owned()),
        ("rimz-workflow".to_owned(), "mcp(workflow)".to_owned()),
        ("rimz-loop".to_owned(), "mcp(schedule)".to_owned()),
        ("rimz-message".to_owned(), "mcp(message)".to_owned()),
        ("rimz-ask".to_owned(), "mcp(ask)".to_owned()),
        ("rimz-control".to_owned(), control),
        ("rimz-notes".to_owned(), "mcp(notes)".to_owned()),
        (
            "rimz-blackboard".to_owned(),
            "mcp(blackboard_flip), mcp(blackboard_record)".to_owned(),
        ),
    ];
    assert_eq!(actual, [served.clone(), served]);
}

#[test]
fn capability_lines_on_a_skills_only_launch_follow_the_skills_list() {
    let mut actual = Vec::new();
    for skills in [
        None,
        Some(vec!["rimz-message", "rimz-wait", "commit"]),
        Some(Vec::new()),
    ] {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("opencode"), Vec::new());
        request.skills =
            skills.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
        let reminders = LaunchReminders {
            rimz_skills: rimz_skills(&["rimz-lsp", "rimz-message", "rimz-wait"]),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(!text.contains("### RimZ capabilities"), "{text}");
        actual.push(reminders.capability_lines(&request));
    }
    assert_eq!(
        actual,
        [
            vec![
                ("rimz-lsp".to_owned(), "skill(rimz-lsp)".to_owned()),
                ("rimz-message".to_owned(), "skill(rimz-message)".to_owned()),
                ("rimz-wait".to_owned(), "skill(rimz-wait)".to_owned()),
            ],
            vec![
                ("rimz-message".to_owned(), "skill(rimz-message)".to_owned()),
                ("rimz-wait".to_owned(), "skill(rimz-wait)".to_owned()),
            ],
            Vec::new(),
        ]
    );
}

/// Under a RimZ grant the capability lines name only the granted capabilities, a served one as its
/// tools and the rest as skills on disk, whatever capability names a `skills` list carries, and
/// then each `rimz-*` skill alone that the `skills` list admits: `skills: []` withholds it and
/// still leaves a granted capability its skill. The launch plan's unreachable-reference warning
/// reads this same list.
#[test]
fn capability_lines_under_a_grant_name_only_the_granted_capabilities() {
    let mut actual = Vec::new();
    for (kind, server) in [("claude", true), ("opencode", false)] {
        for skills in [
            None,
            Some(vec!["rimz-wait", "rimz-reflect", "commit"]),
            Some(Vec::new()),
        ] {
            let quiet = skills.as_ref().is_some_and(Vec::is_empty);
            let mut request =
                ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
            request.skills =
                skills.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
            request.rimz_capabilities = Some(vec!["rimz-lsp:deferred".parse().unwrap()]);
            let reminders = LaunchReminders {
                mcp_server: server.then(|| ("/bin/rimz".into(), Vec::new())),
                rimz_skills: rimz_skills(&["rimz-lsp", "rimz-reflect", "rimz-wait"]),
                ..LaunchReminders::default()
            };
            let names: Vec<_> = reminders
                .rimz_capabilities(&request)
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            let expected: &[&str] = if quiet {
                &["rimz-lsp"]
            } else {
                &["rimz-lsp", "rimz-reflect"]
            };
            assert_eq!(names, expected, "{kind}");
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            assert!(!text.contains("### RimZ capabilities"), "{text}");
            actual.push(reminders.capability_lines(&request));
        }
    }
    let served = vec![
        ("rimz-lsp".to_owned(), "mcp(lsp)".to_owned()),
        ("rimz-reflect".to_owned(), "skill(rimz-reflect)".to_owned()),
    ];
    let served_quiet = vec![("rimz-lsp".to_owned(), "mcp(lsp)".to_owned())];
    let skills = vec![
        ("rimz-lsp".to_owned(), "skill(rimz-lsp)".to_owned()),
        ("rimz-reflect".to_owned(), "skill(rimz-reflect)".to_owned()),
    ];
    let skills_quiet = vec![("rimz-lsp".to_owned(), "skill(rimz-lsp)".to_owned())];
    assert_eq!(
        actual,
        [
            served.clone(),
            served,
            served_quiet,
            skills.clone(),
            skills,
            skills_quiet
        ]
    );
}

/// A granted capability the launch does not serve as tools is reachable by its skill only when
/// that skill is on disk, as a skills list is checked on disk.
#[test]
fn a_granted_capability_with_neither_tools_nor_a_skill_on_disk_is_unreachable() {
    let mut request =
        ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("pi"), Vec::new());
    request.rimz_capabilities = Some(vec![
        "rimz-lsp".parse().unwrap(),
        "rimz-subagents".parse().unwrap(),
    ]);
    let reminders = LaunchReminders {
        rimz_skills: rimz_skills(&["rimz-lsp"]),
        ..LaunchReminders::default()
    };
    assert!(reminders.lsp_callable(&request));
    assert!(!reminders.callable(&request, Capability::Subagents));
}

#[test]
fn a_child_has_no_launching_capability_as_tool_or_skill() {
    for (kind, served) in [("claude", true), ("opencode", false)] {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
        request.subagent = true;
        let reminders = LaunchReminders {
            mcp_server: served.then(|| ("/bin/rimz".into(), Vec::new())),
            rimz_skills: rimz_skills(&[
                "rimz-agents",
                "rimz-ask",
                "rimz-message",
                "rimz-subagents",
                "rimz-teams",
            ]),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(!text.contains("### RimZ capabilities"), "{text}");
        let lines = reminders.capability_lines(&request);
        let message = if served {
            ("rimz-message".to_owned(), "mcp(message)".to_owned())
        } else {
            ("rimz-message".to_owned(), "skill(rimz-message)".to_owned())
        };
        assert!(lines.contains(&message), "{lines:?}");
        for denied in ["rimz-subagents", "rimz-agents", "rimz-teams", "rimz-ask"] {
            assert!(lines.iter().all(|(name, _)| name != denied), "{lines:?}");
        }
        assert_eq!(
            lines.iter().any(|(name, _)| name == "rimz-wait"),
            served,
            "{lines:?}"
        );
    }
}

#[test]
fn a_judge_has_no_launching_capability_as_tool_or_skill() {
    for (kind, served) in [("claude", true), ("opencode", false)] {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked(kind), Vec::new());
        request.identity.params.workflow = Some(crate::agents::WorkflowAttemptRef {
            run: crate::ids::WorkflowRunId::new(),
            seq: 1,
            role: crate::agents::WorkflowAttemptRole::Judge,
        });
        let reminders = LaunchReminders {
            mcp_server: served.then(|| ("/bin/rimz".into(), Vec::new())),
            rimz_skills: rimz_skills(&[
                "rimz-agents",
                "rimz-loop",
                "rimz-message",
                "rimz-subagents",
                "rimz-teams",
            ]),
            ..LaunchReminders::default()
        };
        let capabilities = reminders.rimz_capabilities(&request);
        assert!(
            capabilities.contains(&("rimz-loop".to_owned(), Reach::Skill)),
            "a judge not served `schedule` keeps the rimz-loop skill: {capabilities:?}"
        );
        for launching in [Capability::Subagents, Capability::Agents, Capability::Teams] {
            assert!(
                capabilities
                    .iter()
                    .all(|(name, _)| name != launching.name()),
                "{capabilities:?}"
            );
        }
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(!text.contains("### RimZ capabilities"), "{text}");
        let lines = reminders.capability_lines(&request);
        let message = if served {
            ("rimz-message".to_owned(), "mcp(message)".to_owned())
        } else {
            ("rimz-message".to_owned(), "skill(rimz-message)".to_owned())
        };
        assert!(lines.contains(&message), "{lines:?}");
        for launching in ["rimz-subagents", "rimz-agents", "rimz-teams"] {
            assert!(lines.iter().all(|(name, _)| name != launching), "{lines:?}");
        }
        assert_eq!(
            lines.iter().any(|(name, _)| name == "rimz-wait"),
            served,
            "{lines:?}"
        );
    }
}

#[test]
fn resolve_reach_maps_tools_and_skills_to_short_form() {
    let actual = [
        resolve_reach("rimz-lsp", Reach::Tools(Capability::Lsp)),
        resolve_reach("rimz-wait", Reach::Tools(Capability::Wait)),
        resolve_reach("rimz-subagents", Reach::Tools(Capability::Subagents)),
        resolve_reach("rimz-agents", Reach::Tools(Capability::Agents)),
        resolve_reach("rimz-teams", Reach::Tools(Capability::Teams)),
        resolve_reach("rimz-workflow", Reach::Tools(Capability::Workflow)),
        resolve_reach("rimz-loop", Reach::Tools(Capability::Loop)),
        resolve_reach("rimz-message", Reach::Tools(Capability::Message)),
        resolve_reach("rimz-ask", Reach::Tools(Capability::Ask)),
        resolve_reach("rimz-control", Reach::Tools(Capability::Control)),
        resolve_reach("rimz-notes", Reach::Tools(Capability::Notes)),
        resolve_reach("rimz-lsp", Reach::Skill),
        resolve_reach("rimz-reflect", Reach::Skill),
    ];
    assert_eq!(
        actual,
        [
            "mcp(lsp)",
            "mcp(wait)",
            "mcp(subagent)",
            "mcp(agent)",
            "mcp(team)",
            "mcp(workflow)",
            "mcp(schedule)",
            "mcp(message)",
            "mcp(ask)",
            "mcp(self_stop), mcp(self_compact), mcp(self_tune), mcp(self_molt)",
            "mcp(notes)",
            "skill(rimz-lsp)",
            "skill(rimz-reflect)",
        ]
    );
}
