use super::*;
use crate::config::{RoleBinding, Team};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[test]
fn unknown_spec_catalog_keeps_kinds_and_only_resolvable_names() {
    let profiles = profiles([
        ("perf", profile("claude")),
        ("assistant", profile("perf")),
        ("forge.coder", profile("codex")),
        ("claude", profile("claude")),
    ]);
    let commands = commands([("build", "printf ready")]);
    let teams = TeamsConfig(BTreeMap::from([
        ("forge".into(), team(vec![role("coder", "assistant")])),
        (
            "review".into(),
            Team {
                layout: Some("claude,build".into()),
                ..Default::default()
            },
        ),
    ]));
    let LayoutErr::UnknownSpec(error) =
        resolve_spec(Some("per"), &profiles, &commands, &teams).unwrap_err()
    else {
        panic!("unknown spec")
    };
    let closest = error.closest();
    assert_eq!(
        closest
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect::<Vec<_>>(),
        ["perf", "peer"]
    );
    assert!(matches!(closest[0].kind, SpecKind::Profile(_)));
    assert!(matches!(&closest[1].kind, SpecKind::Team(Some(summary)) if summary == "claude,codex"));
    assert!(
        error
            .candidates
            .iter()
            .any(|candidate| candidate.name == "claude-fast")
    );
    for candidate in &error.candidates {
        assert!(
            resolve_spec(Some(&candidate.name), &profiles, &commands, &teams).is_ok(),
            "{} must resolve",
            candidate.name
        );
    }
    let text = error.choices();
    assert!(
        text.contains("profiles:     assistant, build, perf\n"),
        "{text}"
    );
    assert!(
        text.lines().all(|line| line.chars().count() + 2 <= 88),
        "{text}"
    );
}

#[test]
fn team_summaries_follow_layout_or_distinct_role_kinds() {
    let profiles = profiles([
        ("worker", profile("claude")),
        ("reviewer", profile("codex")),
        ("alias", profile("worker")),
    ]);
    let mut configured = team(vec![
        role("plan", "alias"),
        role("code", "codex"),
        role("review", "reviewer"),
    ]);
    assert_eq!(
        team_summary(&configured, &profiles).as_deref(),
        Some("claude,codex")
    );
    configured.layout = Some("claude+term,codex".into());
    assert_eq!(
        team_summary(&configured, &profiles).as_deref(),
        Some("claude+term,codex")
    );
    configured.layout = None;
    configured.roles.push(role("missing", "not-defined"));
    assert_eq!(team_summary(&configured, &profiles), None);
}
#[test]
fn keep_warm_while_is_admitted_only_for_staged_team_readings() {
    for value in [
        "ci=passed",
        "pr=open",
        "pr.queue=queued",
        "window.5h.left>10",
        "team.stage=Missing",
        "team.stage!=",
    ] {
        for at_role in [true, false] {
            let setting = format!("keep-warm = {{ while = '{value}' }}");
            let raw = if at_role {
                format!(
                    "leader = 'lead'\nstages = ['Plan']\n[[roles]]\nrole = 'lead'\nprofile = 'claude'\nowns = ['Plan']\n{setting}"
                )
            } else {
                format!(
                    "leader = 'lead'\nstages = ['Plan']\n{setting}\nroles = [{{role = 'lead', profile = 'claude', owns = ['Plan']}}]"
                )
            };
            let team: Team = toml::from_str(&raw).unwrap();
            let result = prepare_team("mill", &team, &ProfilesConfig::default(), None);
            assert!(result.is_err(), "invalid while {value} must be refused");
            let error = result.err().unwrap().to_string();
            assert!(error.contains("mill") && error.contains(value), "{error}");
            if at_role {
                assert!(error.contains("lead"), "{error}");
            }
        }
    }
    for raw in [
        "keep-warm = {while = 'team.stage!=Done'}\nroles = [{role = 'lead', profile = 'claude'}]",
        "keep-warm = {while = 'off'}\nroles = [{role = 'lead', profile = 'claude'}]",
    ] {
        let team: Team = toml::from_str(raw).unwrap();
        let result = prepare_team("mill", &team, &ProfilesConfig::default(), None);
        assert!(result.is_err(), "an unstaged team has no board");
        assert!(result.err().unwrap().to_string().contains("no board"));
    }
    for clause in ["team.stage=Plan", "team.stage!=Done", "team.stage=unknown"] {
        let team: Team = toml::from_str(&format!("leader = 'lead'\nstages = ['Plan']\nkeep-warm = {{while = '{clause}'}}\nroles = [{{role = 'lead', profile = 'claude', owns = ['Plan']}}]")).unwrap();
        assert!(prepare_team("mill", &team, &ProfilesConfig::default(), None).is_ok());
    }
    for value in ["team.stage!=Done", "ci=passed", "off"] {
        let profiles: ProfilesConfig = toml::from_str(&format!(
            "[solo]\nagent = 'claude'\nkeep-warm = {{while = '{value}'}}"
        ))
        .unwrap();
        let error = validate_config(
            &profiles,
            &CommandsConfig::default(),
            &TeamsConfig::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("solo") && error.contains(value) && error.contains("no board"),
            "{error}"
        );
        assert!(
            validate_subagent_profile_namespace(
                &profiles,
                &CommandsConfig::default(),
                &TeamsConfig::default()
            )
            .is_err()
        );
    }
}

#[test]
fn room_channel_resolver_prefers_explicit_worktree_then_in_place_team() {
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project-wt/auth"),
            Some("forge"),
            None,
        )
        .as_deref(),
        Some("auth")
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project"),
            Some("forge"),
            None,
        )
        .as_deref(),
        Some("project/forge")
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project"),
            None,
            None,
        ),
        None
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project-wt/auth"),
            Some("forge"),
            Some("design"),
        )
        .as_deref(),
        Some("design")
    );
}

fn profile(agent: &str) -> Profile {
    Profile {
        empty_dir: None,
        allowed_tools: None,
        definition_renders: None,
        kind_defaults: false,
        tools: None,
        auto_compact_default: None,
        model_tier: None,
        tier_stamp: None,
        agent: agent.to_owned(),
        isolation: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
        idle_compact: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        skills: None,
        args: None,
    }
}

#[test]
fn tools_determine_skill_callability_after_layering_and_rebase() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (kind, tools, target, no_skill_tool) in [
        ("claude", Some(vec!["Bash", "Read"]), None, true),
        ("codex", Some(vec!["Bash"]), None, true),
        ("pi", Some(vec!["Read", "Bash"]), None, true),
        ("claude", Some(vec!["Skill", "Read"]), None, false),
        ("codex", Some(vec!["Skill", "Read"]), None, false),
        ("pi", Some(vec!["Skill", "Read"]), None, false),
        ("claude", None, None, false),
        ("codex", None, None, false),
        ("pi", None, None, false),
        ("claude", Some(vec!["Bash"]), Some("opencode"), false),
        ("omp", Some(vec!["Read"]), None, false),
    ] {
        let mut parent = profile(kind);
        parent.tools = tools.map(|tools| {
            crate::agents::ToolSet::parse(&tools.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .unwrap()
        });
        let profiles = profiles([
            ("parent", parent),
            (
                "child",
                Profile {
                    skills: Some(
                        ["rimz-lsp", "rimz-subagents"]
                            .map(|name| name.parse().unwrap())
                            .to_vec(),
                    ),
                    ..profile("parent")
                },
            ),
        ]);
        for spec in ["parent", "child"] {
            let layout = resolve_spec_with_agent_override(
                Some(spec),
                &profiles,
                &profiles,
                &CommandsConfig::default(),
                &TeamsConfig::default(),
                target,
            )
            .unwrap();
            let cell = layout.agent_cells().next().unwrap();
            let request =
                crate::harness::launch::ExecRequest::fresh(cell, Default::default(), None, false);
            assert_eq!(cell.skills, profiles.0[spec].skills);
            let case = format!("{kind} {spec} on {target:?}");
            actual.push((case.clone(), cell.no_skill_tool, request.no_skill_tool));
            expected.push((case, no_skill_tool, no_skill_tool));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn pi_native_tools_render_portably_after_an_engine_switch() {
    let worker = Profile {
        tools: Some(crate::agents::ToolSet::parse(&["read".into(), "bash".into()]).unwrap()),
        ..profile("pi")
    };
    let profiles = profiles([("worker", worker)]);
    let layout = resolve_spec_with_agent_override(
        Some("worker"),
        &profiles,
        &profiles,
        &CommandsConfig::default(),
        &TeamsConfig::default(),
        Some("claude"),
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap();
    assert_eq!(cell.kind, "claude");
    assert_eq!(cell.args, ["--strict-mcp-config", "--tools", "Read,Bash"]);
}

#[test]
fn tools_follow_nearest_role_value_with_explicit_empty_and_base_fallback() {
    let mut parent = profile("claude");
    parent.tools = Some(crate::agents::ToolSet::parse(&["Read".into()]).unwrap());
    let child = profile("parent");
    let empty = Profile {
        tools: Some(crate::agents::ToolSet::parse(&[]).unwrap()),
        ..profile("parent")
    };
    let base = Profile {
        tools: Some(crate::agents::ToolSet::parse(&["Bash".into()]).unwrap()),
        ..profile("pi")
    };
    let profiles = profiles([
        ("parent", parent),
        ("child", child),
        ("empty", empty),
        ("replacement", base),
        ("bare", profile("claude")),
    ]);
    for (spec, target, expected) in [
        (
            "child",
            "replacement",
            vec!["--tools", "read", "--no-skills"],
        ),
        ("empty", "replacement", vec!["--no-tools", "--no-skills"]),
        (
            "bare",
            "replacement",
            vec!["--tools", "bash", "--no-skills"],
        ),
        ("amp", "replacement", vec!["--tools", "bash", "--no-skills"]),
        ("bare", "pi", vec![]),
    ] {
        let layout = resolve_spec_with_agent_override(
            Some(spec),
            &profiles,
            &profiles,
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            Some(target),
        )
        .unwrap();
        assert_eq!(
            layout.agent_cells().next().unwrap().args,
            expected,
            "{spec} on {target}"
        );
    }
}

#[test]
fn allowed_tools_survive_profile_cells_and_fresh_requests() {
    let parent = Profile {
        allowed_tools: Some(vec!["Bash(git *)".parse().unwrap()]),
        ..profile("claude")
    };
    for child_rules in [None, Some(Vec::new()), Some(vec!["Read".parse().unwrap()])] {
        let expected = child_rules.clone().or_else(|| parent.allowed_tools.clone());
        let profiles = ProfilesConfig(BTreeMap::from([
            ("parent".to_owned(), parent.clone()),
            (
                "child".to_owned(),
                Profile {
                    allowed_tools: child_rules,
                    ..profile("parent")
                },
            ),
        ]));
        let resolved = resolve_profile("child", &profiles).unwrap();
        assert_eq!(resolved.allowed_tools, expected);
        let cell = agent_cell("child", &profiles, &CommandsConfig::default());
        assert_eq!(cell.allowed_tools, expected);
        let request =
            crate::harness::launch::ExecRequest::fresh(&cell, Default::default(), None, false);
        assert_eq!(request.allowed_tools, expected);
    }
}

#[test]
fn allowed_tools_rebase_preserves_explicit_empty() {
    let mut base = ResolvedProfile::bare("codex");
    base.allowed_tools = Some(vec!["Read".parse().unwrap()]);
    for rules in [None, Some(Vec::new()), Some(vec!["Write".parse().unwrap()])] {
        let mut original = ResolvedProfile::bare("claude");
        original.allowed_tools = rules.clone();
        assert_eq!(
            rebase_onto(original, Some(&base)).allowed_tools,
            rules.or_else(|| base.allowed_tools.clone())
        );
    }
}

#[test]
fn profile_skills_inherit_replace_and_clear() {
    assert_eq!(
        agent_cell(
            "claude",
            &ProfilesConfig::default(),
            &CommandsConfig::default()
        )
        .skills,
        None
    );
    let parent = Profile {
        skills: Some(vec!["merge".parse().unwrap()]),
        ..profile("claude")
    };
    for child_skills in [
        None,
        Some(Vec::new()),
        Some(vec!["rebase".parse().unwrap()]),
    ] {
        let expected = child_skills.clone().or_else(|| parent.skills.clone());
        let profiles = ProfilesConfig(BTreeMap::from([
            ("parent".to_owned(), parent.clone()),
            (
                "child".to_owned(),
                Profile {
                    skills: child_skills,
                    ..profile("parent")
                },
            ),
        ]));
        let resolved = resolve_profile("child", &profiles).unwrap();
        assert_eq!(resolved.skills, expected);
        assert_eq!(
            agent_cell("child", &profiles, &CommandsConfig::default()).skills,
            expected
        );
    }
}

#[test]
fn profile_isolation_inherits_without_becoming_a_launch_override() {
    for (child, expected) in [("", "host"), ("isolation = 'scoped'", "scoped")] {
        let profiles = ProfilesConfig(BTreeMap::from([
            (
                "parent".to_owned(),
                toml::from_str("agent = 'claude'\nisolation = 'host'").unwrap(),
            ),
            (
                "child".to_owned(),
                toml::from_str(&format!("agent = 'parent'\n{child}")).unwrap(),
            ),
        ]));
        let cell = agent_cell("child", &profiles, &CommandsConfig::default());
        let request =
            crate::harness::launch::ExecRequest::fresh(&cell, Default::default(), None, false);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["isolation_default"], expected);
        assert!(request.identity.params.isolation.is_none());
    }
}

#[test]
fn rebased_profile_skills_preserve_explicit_empty() {
    let mut base = ResolvedProfile::bare("codex");
    base.skills = Some(vec!["merge".parse().unwrap()]);
    let original = ResolvedProfile::bare("claude");
    assert_eq!(
        rebase_onto(original.clone(), Some(&base)).skills,
        base.skills
    );
    let mut cleared = original;
    cleared.skills = Some(Vec::new());
    assert_eq!(rebase_onto(cleared, Some(&base)).skills, Some(Vec::new()));
}

#[test]
fn rebased_profile_isolation_keeps_the_job_default() {
    use crate::config::Isolation::{Host, Scoped};
    let mut base = ResolvedProfile::bare("codex");
    base.isolation_default = Some(Scoped);
    let mut original = ResolvedProfile::bare("claude");
    assert_eq!(
        rebase_onto(original.clone(), Some(&base)).isolation_default,
        Some(Scoped)
    );
    original.isolation_default = Some(Host);
    assert_eq!(
        rebase_onto(original, Some(&base)).isolation_default,
        Some(Host)
    );
}

fn profiles(entries: impl IntoIterator<Item = (&'static str, Profile)>) -> ProfilesConfig {
    ProfilesConfig(
        entries
            .into_iter()
            .map(|(name, profile)| (name.to_owned(), profile))
            .collect(),
    )
}

fn commands(entries: impl IntoIterator<Item = (&'static str, &'static str)>) -> CommandsConfig {
    CommandsConfig(
        entries
            .into_iter()
            .map(|(name, command)| (name.to_owned(), command.to_owned()))
            .collect(),
    )
}

fn role(role: &str, profile: &str) -> RoleBinding {
    RoleBinding {
        role: role.to_owned(),
        profile: profile.to_owned(),
        signals: Vec::new(),
        owns: Vec::new(),
        flip_compact: None,
        idle_compact: None,
        keep_warm: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

fn team(roles: Vec<RoleBinding>) -> Team {
    Team {
        keep_warm: None,
        roles,
        leader: None,
        layout: None,
        scratch_files: None,
        consensus_file: None,
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    }
}

fn team_with_layout(roles: Vec<RoleBinding>, layout: &str) -> Team {
    Team {
        keep_warm: None,
        roles,
        leader: None,
        layout: Some(layout.to_owned()),
        scratch_files: None,
        consensus_file: None,
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    }
}

fn no_profiles() -> ProfilesConfig {
    ProfilesConfig::default()
}

#[test]
fn team_signal_validation_checks_role_selectors_and_matches() {
    use crate::config::TeamSignalBinding;
    use crate::harness::schedule::ScheduleErr;

    let mut declared = team(vec![role("reviewer", "claude"), role("coder", "codex")]);
    declared.roles[1].signals.push(TeamSignalBinding {
        signal: "ci.failed".to_owned(),
        matches: BTreeMap::new(),
        prompt: None,
    });
    let validate = |team: &Team| prepare_team("forge", team, &no_profiles(), None).map(|_| ());
    assert_eq!(validate(&declared), Ok(()));
    let repeated = declared.roles[1].signals[0].clone();
    declared.roles[1].signals.push(repeated);
    for signal in ["invalid signal", "ci.finished"] {
        declared.roles[1].signals[1].signal = signal.to_owned();
        let error = validate(&declared).unwrap_err();
        assert!(matches!(
            error,
            LayoutErr::InvalidTeamSignal { index: 2, .. }
        ));
        assert!(
            error
                .to_string()
                .starts_with("team `forge` role `coder` signal binding 2: ")
        );
    }
    declared.roles[1].signals[1].signal = "ci.*".to_owned();
    declared.roles[1].signals[1]
        .matches
        .insert("conclusion".to_owned(), "failure".to_owned());
    assert!(matches!(
        validate(&declared),
        Err(LayoutErr::InvalidTeamSignal {
            source,
            ..
        }) if matches!(*source, ScheduleErr::ObsoleteCiSignal { .. })
    ));
    declared.roles[1].signals[1].matches.clear();
    declared.roles[1].signals[1].signal = "agent.idle".to_owned();
    declared.roles[1].signals[1].matches = BTreeMap::from([("handle".to_owned(), " ".to_owned())]);
    assert!(matches!(
        validate(&declared),
        Err(LayoutErr::InvalidTeamSignal { source, .. })
            if matches!(*source, ScheduleErr::BlankMatch { .. })
    ));
    for signal in ["agent.*", "agent.idle"] {
        declared.roles[1].signals[1].signal = signal.to_owned();
        for matches in [
            BTreeMap::new(),
            BTreeMap::from([("handle".to_owned(), "*".to_owned())]),
            BTreeMap::from([("session".to_owned(), "*".to_owned())]),
        ] {
            declared.roles[1].signals[1].matches = matches;
            assert!(matches!(
                validate(&declared),
                Err(LayoutErr::UnscopedTeamAgentSignal { index: 2, .. })
            ));
        }
        for key in ["handle", "session"] {
            declared.roles[1].signals[1].matches =
                BTreeMap::from([(key.to_owned(), "reviewer".to_owned())]);
            assert_eq!(validate(&declared), Ok(()));
        }
    }
}

fn no_commands() -> CommandsConfig {
    CommandsConfig::default()
}

fn agent_cell(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> AgentCell {
    let spec = parse_layout_spec(raw, profiles, commands).expect("parse agent");
    agent_at(&spec, 0, 0).clone()
}

fn agent_at(spec: &LayoutSpec, column: usize, row: usize) -> &AgentCell {
    match &spec.columns[column].rows[row] {
        Cell::Agent(agent) => agent,
        Cell::Command { .. } => panic!("agent cell"),
    }
}

#[test]
fn parses_layout_shape_and_command_precedence() {
    let commands = commands([
        ("vim", "nvim -p"),
        ("htop", "htop"),
        ("claude", "nvim"),
        ("logs:tail", "tail -f rimz.log"),
    ]);

    let spec = parse_layout_spec("term,vim+htop,claude/logs:tail", &no_profiles(), &commands)
        .expect("layout");
    assert_eq!(
        spec.columns
            .iter()
            .map(|column| (column.rows.len(), column.stacked))
            .collect::<Vec<_>>(),
        vec![(1, false), (2, false), (2, true)]
    );
    assert_eq!(spec.columns[0].rows[0], Cell::shell());
    for (column, row, expected) in [
        (1, 0, vec!["nvim", "-p"]),
        (1, 1, vec!["htop"]),
        (2, 0, vec!["nvim"]),
        (2, 1, vec!["tail", "-f", "rimz.log"]),
    ] {
        assert!(matches!(
            &spec.columns[column].rows[row],
            Cell::Command { argv }
                if argv.iter().map(String::as_str).eq(expected)
        ));
    }
    assert_eq!(
        parse_layout_spec("claude+codex/term", &no_profiles(), &no_commands()),
        Err(LayoutErr::MixedRowOperators {
            column: "claude+codex/term".to_owned(),
        })
    );
}

#[test]
fn path_executables_are_last_resort_layout_cells() {
    let spec = parse_layout_spec("sh,claude", &no_profiles(), &no_commands()).expect("layout");
    assert!(matches!(
        &spec.columns[0].rows[0],
        Cell::Command { argv } if argv == &["sh"]
    ));
    assert_eq!(agent_at(&spec, 1, 0).kind.as_str(), "claude");

    let configured_command =
        parse_layout_spec("sh", &no_profiles(), &commands([("sh", "echo shadowed")]))
            .expect("configured command");
    assert!(matches!(
        &configured_command.columns[0].rows[0],
        Cell::Command { argv } if argv == &["echo", "shadowed"]
    ));

    let configured_profile =
        parse_layout_spec("sh", &profiles([("sh", profile("claude"))]), &no_commands())
            .expect("configured profile");
    assert!(matches!(
        agent_at(&configured_profile, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, .. }, .. }
            if kind.as_str() == "claude" && profile.as_deref() == Some("sh")
    ));

    assert!(matches!(
        parse_layout_spec(
            "definitely-not-a-binary-xyz,claude",
            &no_profiles(),
            &no_commands()
        ),
        Err(LayoutErr::UnknownCell(unknown))
            if unknown.word == "definitely-not-a-binary-xyz"
    ));
}

#[test]
fn structural_layout_grammar_table() {
    let cases = [
        (
            "claude,codex",
            Ok(vec![(false, vec!["claude"]), (false, vec!["codex"])]),
        ),
        (
            " claude + codex ",
            Ok(vec![(false, vec!["claude", "codex"])]),
        ),
        (
            " claude / codex ",
            Ok(vec![(true, vec!["claude", "codex"])]),
        ),
        (" ", Err(LayoutErr::Empty)),
        ("claude, ", Err(LayoutErr::EmptyCell("claude,".to_owned()))),
        (
            "claude+codex/term",
            Err(LayoutErr::MixedRowOperators {
                column: "claude+codex/term".to_owned(),
            }),
        ),
    ];

    for (raw, expected) in cases {
        let actual = parse_layout_structure(raw).map(|layout| {
            layout
                .columns
                .iter()
                .map(|column| (column.stacked, column.cells.clone()))
                .collect::<Vec<_>>()
        });
        assert_eq!(actual, expected, "{raw:?}");
    }
}

#[test]
fn inline_roles_compose_and_validate() {
    let profiles = profiles([("planner", profile("claude"))]);
    let commands = commands([("logs", "tail -f rimz.log")]);
    let spec = parse_layout_spec(
        "claude:planner,planner:lead,claude-auto:coder",
        &profiles,
        &commands,
    )
    .expect("inline roles");

    assert!(matches!(
        agent_at(&spec, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { role, profile, .. }, .. }
            if kind.as_str() == "claude"
                && role.as_deref() == Some("planner")
                && profile.is_none()
    ));
    assert!(matches!(
        agent_at(&spec, 1, 0),
        AgentCell { launch: crate::agents::LaunchParams { role, profile, .. }, .. }
            if role.as_deref() == Some("lead") && profile.as_deref() == Some("planner")
    ));
    assert!(matches!(
        agent_at(&spec, 2, 0),
        AgentCell { launch: crate::agents::LaunchParams { mode, role, .. }, .. }
            if *mode == Some(PermissionMode::Auto) && role.as_deref() == Some("coder")
    ));

    for (raw, cell) in [("term:x", "term"), ("logs:x", "logs")] {
        assert_eq!(
            parse_layout_spec(raw, &profiles, &commands),
            Err(LayoutErr::RoleOnCommandCell {
                cell: cell.to_owned(),
                role: "x".to_owned(),
            })
        );
    }
    for role in ["codex", "all", "claude-2", "user", "rimz"] {
        assert!(matches!(
            parse_layout_spec(&format!("claude:{role}"), &profiles, &commands),
            Err(LayoutErr::InlineRoleShadowsAddress { name, .. }) if name == role
        ));
    }
    for role in ["", "bad role"] {
        assert_eq!(
            parse_layout_spec(&format!("claude:{role}"), &profiles, &commands),
            Err(LayoutErr::InvalidInlineRole {
                name: role.to_owned(),
            })
        );
    }
    assert_eq!(
        parse_layout_spec("claude:x,codex:x", &profiles, &commands),
        Err(LayoutErr::DuplicateInlineRole {
            role: "x".to_owned(),
        })
    );

    let exact_commands = CommandsConfig(BTreeMap::from([(
        "logs:lead".to_owned(),
        "tail -f exact.log".to_owned(),
    )]));
    let exact = parse_layout_spec("logs:lead", &profiles, &exact_commands)
        .expect("exact command name wins before inline role splitting");
    assert!(matches!(
        &exact.columns[0].rows[0],
        Cell::Command { argv } if argv == &["tail", "-f", "exact.log"]
    ));
}

#[test]
fn known_spec_tokens_require_parseable_layouts() {
    let profiles = no_profiles();
    let commands = no_commands();
    let teams = TeamsConfig::default();

    for raw in ["claude/codex", "claude:planner"] {
        assert!(is_known_spec_token(raw, &profiles, &commands, &teams));
    }
    for raw in ["sh", "https://example.invalid", "claude: fix the bug"] {
        assert!(!is_known_spec_token(raw, &profiles, &commands, &teams));
    }
}

#[test]
fn resolve_spec_dispatches_default_inline_peer_and_team() {
    let profiles = profiles([
        ("planner", profile("claude")),
        ("reviewer", profile("codex")),
    ]);
    let mut teams = TeamsConfig(BTreeMap::from([(
        "stacked".to_owned(),
        team(vec![
            role("planner", "planner"),
            role("reviewer", "reviewer"),
        ]),
    )]));

    for arg in [None, Some("  ")] {
        assert_eq!(
            resolve_spec(arg, &profiles, &no_commands(), &teams),
            Ok(LayoutSpec::single(Cell::shell()))
        );
    }
    assert_eq!(
        resolve_spec(Some("claude"), &profiles, &no_commands(), &teams),
        Ok(LayoutSpec::single(Cell::agent(AgentKind::new_unchecked(
            "claude"
        ))))
    );
    assert_eq!(
        resolve_spec(Some("peer"), &profiles, &no_commands(), &teams)
            .expect("peer")
            .agent_kinds()
            .collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert_eq!(
        resolve_spec(Some("sh"), &profiles, &no_commands(), &teams),
        Ok(LayoutSpec::single(Cell::Command {
            argv: vec!["sh".to_owned()]
        }))
    );
    assert_eq!(
        resolve_spec(Some("sh:role"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::RoleOnCommandCell {
            cell: "sh".to_owned(),
            role: "role".to_owned(),
        })
    );
    assert_eq!(
        resolve_spec(Some("stacked"), &profiles, &no_commands(), &teams)
            .expect("team")
            .columns
            .len(),
        2
    );

    teams
        .0
        .insert("claude".to_owned(), team(vec![role("lead", "planner")]));
    assert_eq!(
        resolve_spec(Some("claude"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::ReservedTeamName("claude".to_owned()))
    );
    teams
        .0
        .insert("rimz".to_owned(), team(vec![role("lead", "planner")]));
    assert_eq!(
        resolve_spec(Some("rimz"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::ReservedTeamName("rimz".to_owned()))
    );
    teams.0.remove("rimz");
    assert!(matches!(
        resolve_spec(Some("missing"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::UnknownSpec(error)) if error.word == "missing"
    ));
}

#[test]
fn unknown_spec_error_groups_and_wraps_valid_choices() {
    let error = resolve_spec(
        Some("missing"),
        &no_profiles(),
        &no_commands(),
        &TeamsConfig::default(),
    )
    .expect_err("missing team");
    let rendered = error.to_string();

    assert!(rendered.starts_with(
        "no team, profile, or command named `missing`\nteams:        peer\nkinds:        "
    ));
    assert!(!rendered.contains("profiles:"), "{rendered}");
    assert!(
        rendered.lines().all(|line| line.len() <= 88),
        "error contains an overlong line:\n{rendered}"
    );
}

#[test]
fn unknown_cell_suggests_nearby_cells_but_not_teams() {
    let profiles = profiles([("reviewer", profile("claude"))]);
    let commands = commands([("build", "printf ready")]);
    let teams = TeamsConfig(BTreeMap::from([
        ("review".into(), team(vec![role("coder", "codex")])),
        (
            "rimz-cell-test-team".into(),
            team(vec![role("coder", "codex")]),
        ),
    ]));
    for (word, suggestion) in [("cdx", "codex"), ("reviewe", "reviewer"), ("buid", "build")] {
        let error = resolve_spec(
            Some(&format!("claude,{word}")),
            &profiles,
            &commands,
            &teams,
        )
        .unwrap_err();
        assert!(matches!(error, LayoutErr::UnknownCell(_)));
        assert_eq!(
            error.to_string(),
            format!("unknown layout cell `{word}`\ndid you mean: {suggestion}")
        );
    }
    let error = resolve_spec(
        Some("claude,rimz-cell-test-tem"),
        &profiles,
        &commands,
        &teams,
    )
    .unwrap_err();
    let LayoutErr::UnknownCell(unknown) = error else {
        panic!("unknown cell")
    };
    assert!(
        !unknown
            .candidates
            .iter()
            .any(|candidate| matches!(candidate.kind, SpecKind::Team(_) | SpecKind::Role))
    );
    assert!(unknown.closest().is_empty());
}

#[test]
fn unknown_cell_fallback_groups_and_wraps_cell_choices() {
    let profiles = profiles([("reviewer", profile("claude"))]);
    let commands = commands([("build", "printf ready")]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".into(),
        team(vec![role("coder", "codex")]),
    )]));
    let error = resolve_spec(Some("claude,zzzzzz"), &profiles, &commands, &teams).unwrap_err();
    let text = error.to_string();
    assert!(
        text.starts_with("unknown layout cell `zzzzzz`\nprofiles:     build, reviewer\nkinds:"),
        "{text}"
    );
    assert!(
        text.contains("term") && text.contains("(each also as <kind>"),
        "{text}"
    );
    for excluded in [
        "teams:",
        "forge.coder",
        "claude-plan",
        "<agents_home>",
        "rimz agents",
    ] {
        assert!(!text.contains(excluded), "{text}");
    }
    assert!(
        text.lines().all(|line| line.chars().count() + 2 <= 88),
        "{text}"
    );
}

#[test]
fn unknown_cell_candidates_resolve_as_cells() {
    let profiles = profiles([
        ("reviewer", profile("claude")),
        ("forge.coder", profile("codex")),
        ("claude", profile("claude")),
    ]);
    let commands = commands([("build", "printf ready")]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".into(),
        team(vec![role("coder", "codex")]),
    )]));
    let LayoutErr::UnknownCell(unknown) =
        resolve_spec(Some("claude,zzzzzz"), &profiles, &commands, &teams).unwrap_err()
    else {
        panic!("unknown cell")
    };
    for name in [
        "reviewer",
        "build",
        "forge.coder",
        "claude",
        "claude-plan",
        "claude-fast",
    ] {
        assert!(
            unknown
                .candidates
                .iter()
                .any(|candidate| candidate.name == name),
            "missing {name}"
        );
    }
    for candidate in &unknown.candidates {
        assert!(
            parse_layout_spec(&candidate.name, &profiles, &commands).is_ok(),
            "{} must resolve as a cell",
            candidate.name
        );
    }
}

#[test]
fn unknown_spec_suggests_only_the_nearest_tier() {
    let profiles = profiles([
        ("perf", profile("claude")),
        ("forge.coder", profile("codex")),
        ("claude", profile("claude")),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".into(),
        team(vec![role("coder", "codex")]),
    )]));
    let error = resolve_spec(Some("per"), &profiles, &no_commands(), &teams).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no team, profile, or command named `per`\ndid you mean: perf, peer"
    );
    let error = resolve_spec(Some("forg.coder"), &profiles, &no_commands(), &teams).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no team, profile, or command named `forg.coder`\ndid you mean: forge.coder"
    );
    let error = resolve_spec(Some("xyz"), &profiles, &no_commands(), &teams).unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("teams:        forge, peer\nprofiles:     perf\nkinds:"),
        "{text}"
    );
    assert!(
        !text.contains("forge.coder") && !text.contains("claude-plan"),
        "{text}"
    );
    assert!(
        text.contains("term") && !text.contains("rimz agents") && !text.contains("<agents_home>"),
        "{text}"
    );
}

#[test]
fn unknown_role_suggests_a_declared_role() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".into(),
        team(vec![role("coder", "codex")]),
    )]));
    let error =
        resolve_spec(Some("forge.coderr"), &no_profiles(), &no_commands(), &teams).unwrap_err();
    assert!(matches!(error, LayoutErr::UnknownRoleInTeam { .. }));
    assert_eq!(
        error.to_string(),
        "team `forge` has no role `coderr`; declared roles: coder\ndid you mean: forge.coder"
    );
}

#[test]
fn profile_inheritance_and_builtin_overrides_resolve() {
    let profiles = profiles([
        (
            "base",
            Profile {
                agent: "codex".to_owned(),
                model: Some("base-model".to_owned()),
                auto_compact: Some("1m".to_owned()),
                effort: Some("medium".to_owned()),
                args: Some("--base".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "child",
            Profile {
                agent: "base".to_owned(),
                effort: Some("high".to_owned()),
                args: Some("--child".to_owned()),
                auto_compact: Some("200k".to_owned()),
                ..profile("base")
            },
        ),
        (
            "inherits-args",
            Profile {
                agent: "base".to_owned(),
                model: Some("child-model".to_owned()),
                ..profile("base")
            },
        ),
        (
            "claude",
            Profile {
                agent: "claude".to_owned(),
                effort: Some("high".to_owned()),
                ..profile("claude")
            },
        ),
        ("claude-child", profile("claude")),
    ]);

    let bare = resolve_profile("claude", &no_profiles()).expect("built-in");
    assert_eq!(bare.kind.as_str(), "claude");
    assert!(bare.layers.is_empty());
    assert_eq!(
        resolve_profile("claude", &profiles).unwrap().layers,
        ["claude"]
    );
    let child = resolve_profile("child", &profiles).expect("child");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.layers, ["child", "base"]);
    assert_eq!(child.launch.model.as_deref(), Some("base-model"));
    assert_eq!(child.launch.effort.as_deref(), Some("high"));
    assert_eq!(child.args.as_deref(), Some("--child"));
    assert_eq!(child.auto_compact.as_deref(), Some("200000"));

    let inherited = resolve_profile("inherits-args", &profiles).expect("inherited");
    assert_eq!(inherited.launch.model.as_deref(), Some("child-model"));
    assert_eq!(inherited.launch.effort.as_deref(), Some("medium"));
    assert_eq!(inherited.args.as_deref(), Some("--base"));
    assert_eq!(inherited.auto_compact.as_deref(), Some("1000000"));

    for name in ["claude", "claude-child"] {
        let resolved = resolve_profile(name, &profiles).expect("override");
        assert_eq!(resolved.kind.as_str(), "claude");
        assert_eq!(resolved.launch.effort.as_deref(), Some("high"));
    }
}

#[test]
fn profile_cells_render_preset_args_and_carry_prompt_sources() {
    let profiles = profiles([
        (
            "codex-deep",
            Profile {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                model: Some("gpt-5-codex".to_owned()),
                auto_compact: Some("200k".to_owned()),
                effort: Some("high".to_owned()),
                args: Some("--profile reviewer".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "prompt-base",
            Profile {
                agent: "claude".to_owned(),
                system_prompt_file: Some("/prompts/base.md".into()),
                auto_compact: Some("200000".to_owned()),
                append_system_prompt_files: vec!["/prompts/base-fragment.md".into()],
                ..profile("claude")
            },
        ),
        (
            "prompt-child",
            Profile {
                agent: "prompt-base".to_owned(),
                append_system_prompt_files: vec!["/prompts/child-fragment.md".into()],
                ..profile("prompt-base")
            },
        ),
    ]);
    let spec =
        parse_layout_spec("codex-deep,prompt-child", &profiles, &no_commands()).expect("profiles");

    let codex = agent_at(&spec, 0, 0);
    let mut expected = vec![
        "--model".to_owned(),
        "gpt-5-codex".to_owned(),
        "-c".to_owned(),
        "model_reasoning_effort=high".to_owned(),
        "-c".to_owned(),
        "model_auto_compact_token_limit=200000".to_owned(),
    ];
    expected.extend(
        crate::agents::find_definition("codex")
            .expect("codex")
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
    );
    expected.extend(["--profile".to_owned(), "reviewer".to_owned()]);
    assert_eq!(codex.args, expected);
    assert_eq!(codex.launch.mode, Some(PermissionMode::Auto));
    assert_eq!(codex.launch.profile.as_deref(), Some("codex-deep"));
    assert_eq!(codex.launch.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(codex.launch.effort.as_deref(), Some("high"));

    let prompt = agent_at(&spec, 1, 0);
    assert_eq!(prompt.launch.profile.as_deref(), Some("prompt-child"));
    assert_eq!(
        prompt
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(Path::new("/prompts/base.md"))
    );
    assert_eq!(
        prompt.append_system_prompt_files,
        [
            PathBuf::from("/prompts/base-fragment.md").into(),
            PathBuf::from("/prompts/child-fragment.md").into()
        ]
    );
    assert_eq!(prompt.args, ["--autocompact", "200000"]);
}

#[test]
fn cross_kind_override_replaces_provider_fields_and_carries_portable_fields() {
    let profiles = profiles([(
        "planner",
        Profile {
            empty_dir: None,
            allowed_tools: None,
            definition_renders: None,
            kind_defaults: false,
            tools: None,
            auto_compact_default: None,
            model_tier: None,
            tier_stamp: Some(Box::new(crate::agents::TierStamp {
                tier: crate::config::tiers::ModelTier::Senior,
                model: "claude-model".to_owned(),
                used_tier: None,
                unbuildable: Vec::new(),
                skipped: Vec::new(),
            })),
            agent: "claude".to_owned(),
            isolation: None,
            description: None,
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            idle_compact: None,
            mode: Some(PermissionMode::Auto),
            model: Some("claude-model".to_owned()),
            effort: Some("high".to_owned()),
            fast: None,
            budget: Some("$4".to_owned()),
            auto_compact: Some("200k".to_owned()),
            system_prompt_file: Some("/prompts/system.md".into()),
            append_system_prompt_files: vec!["/prompts/fragment.md".into()],
            skills: None,
            args: Some("--strict-mcp-config".to_owned()),
        },
    )]);
    let spec = resolve_spec_with_agent_override(
        Some("planner"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("codex"),
    )
    .expect("rebase");

    let cell = agent_at(&spec, 0, 0);
    assert_eq!(cell.kind, "codex");
    assert_eq!(cell.launch.model, None);
    assert!(
        cell.launch.tier.is_none(),
        "an unrouted rebase must not keep the old engine's stamp"
    );
    assert_eq!(cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(cell.launch.budget.as_deref(), Some("$4.00"));
    assert_eq!(cell.auto_compact.as_deref(), Some("200000"));
    assert!(
        cell.args
            .windows(2)
            .any(|args| args == ["-c", "model_auto_compact_token_limit=200000"])
    );
    assert_eq!(cell.launch.mode, Some(PermissionMode::Auto));
    assert_eq!(
        cell.system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(Path::new("/prompts/system.md"))
    );
    assert_eq!(
        cell.append_system_prompt_files,
        [PathBuf::from("/prompts/fragment.md").into()]
    );
    assert!(!cell.args.iter().any(|arg| arg == "--strict-mcp-config"));
}

#[test]
fn auto_compact_rejects_invalid_counts_but_accepts_unsupported_adapters() {
    for (kind, value) in [
        ("claude", "70%"),
        ("codex", "0"),
        ("codex", "lots"),
        ("codex", "18446744073709551615k"),
        ("pi", "70%"),
        ("pi", "99999"),
    ] {
        let profiles = profiles([(
            "compact",
            Profile {
                auto_compact: Some(value.to_owned()),
                ..profile(kind)
            },
        )]);
        assert!(
            matches!(resolve_profile("compact", &profiles),
            Err(LayoutErr::InvalidProfile { profile, reason })
                if profile == "compact" && reason.contains("auto-compact")),
            "{kind}: {value}"
        );
    }

    let profiles = profiles([(
        "compact",
        Profile {
            auto_compact: Some("200k".to_owned()),
            ..profile("pi")
        },
    )]);
    assert!(resolve_profile("compact", &profiles).is_ok());
    assert!(cell_from_profile("compact", &resolve_profile("compact", &profiles).unwrap()).is_ok());

    for value in ["70%", "0", "200", "99999", "1000001", "lots"] {
        let mut binding = role("coder", "codex");
        binding.auto_compact = Some(value.to_owned());
        let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), team(vec![binding]))]));
        assert!(matches!(resolve_spec_with_agent_override(
            Some("forge"), &no_profiles(), &no_profiles(), &no_commands(), &teams, None,
        ), Err(LayoutErr::InvalidProfile { reason, .. }) if reason.contains("auto-compact")));
    }
}

#[test]
fn auto_compact_validates_portable_token_range() {
    for kind in ["claude", "codex"] {
        for value in ["200", "1000", "50k", "99999", "1000001", "2M"] {
            let profiles = profiles([(
                "compact",
                Profile {
                    auto_compact: Some(value.to_owned()),
                    ..profile(kind)
                },
            )]);
            assert!(
                matches!(resolve_profile("compact", &profiles),
                Err(LayoutErr::InvalidProfile { reason, .. })
                    if reason.contains("100k and 1M tokens") && reason.contains("`200k`")),
                "{kind}: {value}"
            );
        }
        for (value, expected) in [
            ("100k", "100000"),
            ("100000", "100000"),
            ("1M", "1000000"),
            ("1000000", "1000000"),
        ] {
            let profiles = profiles([(
                "compact",
                Profile {
                    auto_compact: Some(value.to_owned()),
                    ..profile(kind)
                },
            )]);
            assert_eq!(
                resolve_profile("compact", &profiles)
                    .expect("boundary accepted")
                    .auto_compact
                    .as_deref(),
                Some(expected)
            );
        }
    }
}

#[test]
fn profile_override_supplies_provider_base_and_orders_prompt_fragments() {
    let profiles = profiles([
        (
            "planner",
            Profile {
                agent: "claude".to_owned(),
                effort: Some("high".to_owned()),
                append_system_prompt_files: vec!["/prompts/planner.md".into()],
                ..profile("claude")
            },
        ),
        (
            "codex",
            Profile {
                agent: "codex".to_owned(),
                model: Some("codex-model".to_owned()),
                append_system_prompt_files: vec!["/prompts/codex.md".into()],
                args: Some("--base-only".to_owned()),
                ..profile("codex")
            },
        ),
    ]);
    let spec = resolve_spec_with_agent_override(
        Some("planner"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("codex"),
    )
    .expect("profile base");

    let cell = agent_at(&spec, 0, 0);
    assert_eq!(cell.kind, "codex");
    assert_eq!(cell.launch.model.as_deref(), Some("codex-model"));
    assert_eq!(cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        cell.append_system_prompt_files,
        [
            PathBuf::from("/prompts/codex.md").into(),
            PathBuf::from("/prompts/planner.md").into()
        ]
    );
    assert!(cell.args.ends_with(&["--base-only".to_owned()]));
}

#[test]
fn same_kind_and_chained_overrides_resolve_without_drops() {
    let self_named = profiles([("codex", profile("codex"))]);
    let resolved = resolve_profile("codex", &self_named).unwrap();
    assert_eq!(resolved.layers, ["codex"]);
    assert_eq!(resolved.kind, "codex");
    assert_eq!(resolved.into_chain(), ["codex"]);
    let rebased = resolve_profile_rebased("codex", Some("claude"), &self_named).unwrap();
    assert_eq!(rebased.layers, ["codex"]);
    assert_eq!(rebased.kind, "claude");
    assert_eq!(rebased.into_chain(), ["codex", "claude"]);

    let mut writer_profiles = profiles([("writer", profile("claude")), ("fast", profile("codex"))]);
    for (name, agent_override, layers, kind, chain) in [
        (
            "writer",
            Some("writer"),
            vec!["writer"],
            "claude",
            vec!["writer", "claude"],
        ),
        (
            "writer",
            Some("fast"),
            vec!["writer", "fast"],
            "codex",
            vec!["writer", "fast", "codex"],
        ),
        ("claude", None, Vec::new(), "claude", vec!["claude"]),
    ] {
        let resolved = resolve_profile_rebased(name, agent_override, &writer_profiles).unwrap();
        assert_eq!(resolved.layers, layers);
        assert_eq!(resolved.kind, kind);
        assert_eq!(resolved.into_chain(), chain);
    }
    writer_profiles
        .0
        .insert("claude".to_owned(), profile("claude"));
    let original = resolve_profile("writer", &writer_profiles).unwrap();
    let rebased = resolve_profile_rebased("writer", Some("writer"), &writer_profiles).unwrap();
    assert_eq!(original.layers, ["writer", "claude"]);
    assert_eq!(rebased.layers, original.layers);
    assert_eq!(rebased.kind, original.kind);
    assert_eq!(rebased.into_chain(), original.into_chain());

    let profiles = profiles([
        (
            "planner",
            Profile {
                agent: "claude".to_owned(),
                model: Some("claude-model".to_owned()),
                args: Some("--profile planner".to_owned()),
                ..profile("claude")
            },
        ),
        ("switch", profile("switch-base")),
        ("switch-base", profile("codex")),
    ]);
    for (agent_override, layers, kind) in [
        (None, vec!["planner"], "claude"),
        (Some("claude"), vec!["planner"], "claude"),
        (Some(" codex "), vec!["planner"], "codex"),
        (
            Some("switch"),
            vec!["planner", "switch", "switch-base"],
            "codex",
        ),
    ] {
        let resolved = resolve_profile_rebased("planner", agent_override, &profiles).unwrap();
        assert_eq!(resolved.layers, layers);
        assert_eq!(resolved.kind, kind);
    }
    let same = resolve_spec_with_agent_override(
        Some("planner"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("claude"),
    )
    .expect("same kind");
    let cell = agent_at(&same, 0, 0);
    assert_eq!(cell.launch.model.as_deref(), Some("claude-model"));
    assert!(
        cell.args
            .ends_with(&["--profile".to_owned(), "planner".to_owned()])
    );

    let chained = resolve_spec_with_agent_override(
        Some("claude"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("switch"),
    )
    .expect("chained base");
    assert_eq!(agent_at(&chained, 0, 0).kind, "codex");

    assert!(matches!(
        resolve_spec_with_agent_override(
            Some("claude"),
            &profiles,
            &profiles,
            &no_commands(),
            &TeamsConfig::default(),
            Some("ghost"),
        ),
        Err(LayoutErr::UnknownAgentOverride { value, valid })
            if value == "ghost"
                && valid.contains("planner")
                && valid.contains("claude")
                && valid.contains("codex")
    ));
}

#[test]
fn kind_defaults_follow_the_final_kind_and_model() {
    use PermissionMode::{Auto, Plan, Yolo};

    let profiles = profiles([
        (
            "markdown",
            Profile {
                kind_defaults: true,
                ..profile("claude")
            },
        ),
        ("inherited", profile("markdown")),
        (
            "qwen-job",
            Profile {
                kind_defaults: true,
                ..profile("qwen")
            },
        ),
        ("toml", profile("claude")),
        (
            "codex-base",
            Profile {
                kind_defaults: true,
                ..profile("codex")
            },
        ),
        (
            "fable-base",
            Profile {
                model: Some("fable".into()),
                ..profile("claude")
            },
        ),
        (
            "written",
            Profile {
                kind_defaults: true,
                mode: Some(Plan),
                effort: Some("high".into()),
                ..profile("claude")
            },
        ),
        (
            "written-base",
            Profile {
                kind_defaults: true,
                mode: Some(Yolo),
                effort: Some("low".into()),
                ..profile("codex")
            },
        ),
    ]);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (source, target, kind, model, mode, effort) in [
        ("markdown", None, "claude", None, Some(Auto), Some("xhigh")),
        ("markdown", Some("opencode"), "opencode", None, None, None),
        ("markdown", Some("qwen"), "qwen", None, None, None),
        (
            "inherited",
            Some("codex"),
            "codex",
            None,
            None,
            Some("xhigh"),
        ),
        ("markdown", Some("pi"), "pi", None, None, Some("xhigh")),
        (
            "qwen-job",
            Some("claude"),
            "claude",
            None,
            Some(Auto),
            Some("xhigh"),
        ),
        ("toml", Some("codex"), "codex", None, None, None),
        (
            "toml",
            Some("codex-base"),
            "codex",
            None,
            None,
            Some("xhigh"),
        ),
        (
            "qwen",
            Some("codex-base"),
            "codex",
            None,
            None,
            Some("xhigh"),
        ),
        (
            "written",
            Some("codex-base"),
            "codex",
            None,
            Some(Plan),
            Some("high"),
        ),
        (
            "written",
            Some("written-base"),
            "codex",
            None,
            Some(Plan),
            Some("low"),
        ),
        (
            "markdown",
            Some("fable-base"),
            "claude",
            Some("fable"),
            Some(Auto),
            Some("high"),
        ),
    ] {
        let result = resolve_spec_with_agent_override(
            Some(source),
            &profiles,
            &profiles,
            &no_commands(),
            &TeamsConfig::default(),
            target,
        )
        .map(|layout| {
            let cell = agent_at(&layout, 0, 0);
            (
                cell.kind.to_string(),
                cell.launch.model.clone(),
                cell.launch.mode,
                cell.launch.effort.clone(),
            )
        })
        .map_err(|error| error.to_string());
        actual.push((source, target, result));
        expected.push((
            source,
            target,
            Ok((
                kind.to_owned(),
                model.map(str::to_owned),
                mode,
                effort.map(str::to_owned),
            )),
        ));
    }
    let refusal = resolve_spec_with_agent_override(
        Some("written"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("qwen"),
    )
    .map(|_| ("qwen".into(), None, None, None))
    .map_err(|error| error.to_string());
    actual.push(("written", Some("qwen"), refusal));
    let reason = concat!(
        "invalid profile `written`: agent `qwen` does not support profile field `effort`; ",
        "remove it or put provider-specific flags in `args`",
    );
    expected.push(("written", Some("qwen"), Err(reason.into())));
    assert_eq!(actual, expected);
}

#[test]
fn role_values_survive_defaults_in_both_rebase_orders() {
    use PermissionMode::{Auto, Plan};

    let profiles = profiles([
        (
            "worker",
            Profile {
                kind_defaults: true,
                ..profile("claude")
            },
        ),
        (
            "fable",
            Profile {
                model: Some("fable".into()),
                ..profile("claude")
            },
        ),
    ]);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (target, model, mode, effort, final_mode, final_effort) in [
        ("codex", None, None, None, None, Some("xhigh")),
        ("opencode", None, None, None, None, None),
        (
            "codex",
            None,
            Some(Plan),
            Some("high"),
            Some(Plan),
            Some("high"),
        ),
        ("qwen", None, Some(Auto), None, Some(Auto), None),
        (
            "fable",
            None,
            None,
            Some("xhigh"),
            Some(Auto),
            Some("xhigh"),
        ),
        (
            "claude",
            Some("fable"),
            None,
            None,
            Some(Auto),
            Some("high"),
        ),
    ] {
        let binding = RoleBinding {
            model: model.map(str::to_owned),
            mode,
            effort: effort.map(str::to_owned),
            ..role("coder", "worker")
        };
        let base = resolve_agent_override(Some(target), &profiles).unwrap();
        let configured = team(vec![binding.clone()]);
        let prepared = prepare_team("forge", &configured, &profiles, base.as_ref())
            .map(|team| {
                let launch = &team.roles[0].resolved.launch;
                (launch.mode, launch.effort.clone())
            })
            .map_err(|error| error.to_string());
        let replayed =
            profile_cell_with_base_override("worker", &profiles, base.as_ref(), Some(&binding))
                .map(|cell| (cell.launch.mode, cell.launch.effort))
                .map_err(|error| error.to_string());
        actual.push((prepared, replayed));
        let launch = Ok((final_mode, final_effort.map(str::to_owned)));
        expected.push((launch.clone(), launch));
    }
    assert_eq!(actual, expected);
}

#[test]
fn virtual_cells_render_only_their_own_permission_posture_with_kind_defaults() {
    let profiles = profiles([(
        "claude",
        Profile {
            kind_defaults: true,
            ..profile("claude")
        },
    )]);
    let mut actual = Vec::new();
    for word in [
        "claude-yolo",
        "claude-yolo-fast",
        "claude-ask",
        "claude-ask-fast",
    ] {
        let Cell::Agent(cell) = parse_cell(word, &profiles, &no_commands(), None).unwrap() else {
            panic!("expected an agent cell");
        };
        actual.push((
            cell.launch.effort,
            cell.args
                .iter()
                .filter(|arg| arg.as_str() == "--dangerously-skip-permissions")
                .count(),
            cell.args
                .windows(2)
                .filter(|args| args[0] == "--permission-mode" && args[1] == "auto")
                .count(),
        ));
    }
    assert_eq!(
        actual,
        [
            (Some("xhigh".into()), 1, 0),
            (Some("xhigh".into()), 1, 0),
            (Some("xhigh".into()), 0, 0),
            (Some("xhigh".into()), 0, 0)
        ]
    );
}

#[test]
fn rebase_takes_the_engine_from_the_base_and_keeps_the_role() {
    let engine = |agent: &str, model: Option<&str>, effort: Option<&str>| Profile {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        ..profile(agent)
    };
    let profiles = profiles([
        (
            "fixer",
            Profile {
                system_prompt_file: Some("/prompts/fixer.md".into()),
                ..engine("claude", Some("fable"), Some("medium"))
            },
        ),
        ("opus", engine("claude", Some("opus[1m]"), Some("high"))),
        ("plain", profile("claude")),
        ("luna", engine("codex", None, Some("xhigh"))),
        ("pi", engine("pi", None, None)),
        ("pi-model", engine("pi", Some("replacement"), None)),
    ]);
    for (agent_override, kind, model, effort, chain) in [
        ("pi", "pi", Some("fable"), "medium", vec!["fixer", "pi"]),
        (
            "pi-model",
            "pi",
            Some("replacement"),
            "medium",
            vec!["fixer", "pi-model", "pi"],
        ),
        (
            "opus",
            "claude",
            Some("opus[1m]"),
            "high",
            vec!["fixer", "opus", "claude"],
        ),
        (
            "plain",
            "claude",
            Some("fable"),
            "medium",
            vec!["fixer", "plain", "claude"],
        ),
        (
            "luna",
            "codex",
            None,
            "xhigh",
            vec!["fixer", "luna", "codex"],
        ),
    ] {
        let resolved = resolve_profile_rebased("fixer", Some(agent_override), &profiles).unwrap();
        assert_eq!(resolved.kind, kind, "{agent_override}");
        assert_eq!(resolved.launch.model.as_deref(), model, "{agent_override}");
        assert_eq!(
            resolved.launch.effort.as_deref(),
            Some(effort),
            "{agent_override}"
        );
        assert_eq!(
            resolved
                .system_prompt_file
                .as_ref()
                .and_then(crate::config::PromptSource::file),
            Some(std::path::Path::new("/prompts/fixer.md"))
        );
        assert_eq!(resolved.into_chain(), chain, "{agent_override}");
    }
}

#[test]
fn team_roles_and_virtual_cells_rebase_through_the_same_path() {
    let profiles = profiles([
        (
            "planner",
            Profile {
                agent: "claude".to_owned(),
                model: Some("profile-model".to_owned()),
                auto_compact: Some("200k".to_owned()),
                ..profile("claude")
            },
        ),
        (
            "codex",
            Profile {
                agent: "codex".to_owned(),
                model: Some("codex-model".to_owned()),
                effort: Some("high".to_owned()),
                args: Some("--base-only".to_owned()),
                ..profile("codex")
            },
        ),
    ]);
    let mut planner = role("planner", "planner");
    planner.args = Some("--role-arg".to_owned());
    planner.auto_compact = Some("300K".to_owned());
    let mut reviewer = role("reviewer", "planner");
    reviewer.args = Some("--reviewer-arg".to_owned());
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![planner, reviewer]),
    )]));
    let team_spec = resolve_spec_with_agent_override(
        Some("forge"),
        &profiles,
        &profiles,
        &no_commands(),
        &teams,
        Some("codex"),
    )
    .expect("team rebase");
    let cell = agent_at(&team_spec, 0, 0);
    assert_eq!(cell.kind, "codex");
    assert_eq!(cell.launch.role.as_deref(), Some("planner"));
    assert_eq!(cell.auto_compact.as_deref(), Some("300000"));
    assert!(
        cell.args
            .windows(2)
            .any(|args| args == ["-c", "model_auto_compact_token_limit=300000"])
    );
    assert_eq!(
        agent_at(&team_spec, 1, 0).auto_compact.as_deref(),
        Some("200000")
    );
    assert!(!cell.args.iter().any(|arg| arg == "--role-arg"));

    let role_spec = resolve_spec_with_agent_override(
        Some("forge.planner"),
        &profiles,
        &profiles,
        &no_commands(),
        &teams,
        Some("codex"),
    )
    .expect("single role rebase");
    assert_eq!(
        agent_at(&role_spec, 0, 0).launch.role.as_deref(),
        Some("planner")
    );

    let virtual_spec = resolve_spec_with_agent_override(
        Some("claude-yolo"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("codex"),
    )
    .expect("virtual rebase");
    let virtual_cell = agent_at(&virtual_spec, 0, 0);
    assert_eq!(virtual_cell.kind, "codex");
    assert_eq!(virtual_cell.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(virtual_cell.launch.model.as_deref(), Some("codex-model"));
    assert!(
        virtual_cell
            .args
            .windows(2)
            .any(|args| { args == ["--model".to_owned(), "codex-model".to_owned()] })
    );
    assert!(
        virtual_cell
            .args
            .windows(2)
            .any(|args| { args == ["-c".to_owned(), "model_reasoning_effort=high".to_owned(),] })
    );
    assert!(virtual_cell.args.iter().any(|arg| arg == "--base-only"));
    let posture = crate::agents::find_definition("codex")
        .expect("codex")
        .spec()
        .launch
        .permission_args(PermissionMode::Yolo);
    assert!(virtual_cell.args.ends_with(&posture));
}

#[test]
fn profile_resolution_rejects_invalid_chains_and_fields() {
    assert_eq!(
        resolve_profile("planner", &profiles([("planner", profile("ghost"))])),
        Err(LayoutErr::UnknownProfileBase {
            profile: "planner".to_owned(),
            base: "ghost".to_owned(),
        })
    );
    let cycle = profiles([
        ("a", profile("b")),
        ("b", profile("c")),
        ("c", profile("a")),
    ]);
    assert!(matches!(
        resolve_profile("a", &cycle),
        Err(LayoutErr::ProfileCycle { chain }) if chain == "a -> b -> c -> a"
    ));

    let mut chain = BTreeMap::new();
    for i in 0..=MAX_PROFILE_DEPTH {
        chain.insert(format!("p{i}"), profile(&format!("p{}", i + 1)));
    }
    assert_eq!(
        resolve_profile("p0", &ProfilesConfig(chain)),
        Err(LayoutErr::ProfileChainTooDeep {
            profile: "p0".to_owned(),
        })
    );

    let bad_args = profiles([(
        "bad-args",
        Profile {
            agent: "claude".to_owned(),
            args: Some("'unterminated".to_owned()),
            ..profile("claude")
        },
    )]);
    assert!(matches!(
        parse_layout_spec("bad-args", &bad_args, &no_commands()),
        Err(LayoutErr::InvalidProfile { profile, reason })
            if profile == "bad-args" && reason.contains("shell quoting")
    ));

    let unsupported = profiles([(
        "pi-deep",
        Profile {
            agent: "pi".to_owned(),
            system_prompt_file: Some(PathBuf::from("/abs/prompt.md").into()),
            ..profile("pi")
        },
    )]);
    let unsupported =
        parse_layout_spec("pi-deep", &unsupported, &no_commands()).expect("prompt stays typed");
    assert_eq!(
        agent_at(&unsupported, 0, 0)
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(Path::new("/abs/prompt.md"))
    );
}

#[test]
fn profile_fast_follows_the_chain_and_skips_where_unsupported() {
    let fast = |agent: &str, fast: Option<bool>| Profile {
        fast,
        ..profile(agent)
    };
    let profiles = ProfilesConfig(BTreeMap::from([
        ("swift".to_owned(), fast("codex", Some(true))),
        ("steady".to_owned(), fast("swift", Some(false))),
        ("heir".to_owned(), fast("swift", None)),
        ("quick".to_owned(), fast("cursor", Some(true))),
        ("calm".to_owned(), fast("cursor", Some(false))),
    ]));
    let switch = ["-c", "service_tier=fast", "-c", "features.fast_mode=true"];
    for (name, want) in [
        ("swift", true),
        ("steady", false),
        ("heir", true),
        ("calm", false),
    ] {
        let cell = profile_cell(name, &profiles).unwrap();
        assert_eq!(cell.launch.fast, want, "{name}");
        assert_eq!(
            cell.args.windows(switch.len()).any(|part| part == switch),
            want,
            "{name}: {:?}",
            cell.args
        );
    }
    let quick = profile_cell("quick", &profiles).expect("unsupported fast renders no switch");
    assert!(quick.launch.fast, "the finalizer clears it and warns");
    assert!(quick.args.is_empty());
    let role = RoleBinding {
        fast: Some(false),
        ..role("coder", "swift")
    };
    assert!(!resolve_role(&role, &profiles).unwrap().fast.unwrap());
}

#[test]
fn virtual_cells_obey_adapter_capabilities_and_profile_overrides() {
    for (raw, mode) in [
        ("pi-ask", PermissionMode::Ask),
        ("pi-plan", PermissionMode::Plan),
    ] {
        let cell = agent_cell(raw, &no_profiles(), &no_commands());
        assert_eq!(cell.kind.as_str(), "pi");
        assert_eq!(cell.launch.mode, Some(mode));
        assert!(cell.args.is_empty());
    }

    let profiles = profiles([
        (
            "claude",
            Profile {
                agent: "claude".to_owned(),
                mode: Some(PermissionMode::Plan),
                args: Some("--append".to_owned()),
                ..profile("claude")
            },
        ),
        (
            "pi",
            Profile {
                agent: "pi".to_owned(),
                mode: Some(PermissionMode::Auto),
                args: Some("--profile-arg".to_owned()),
                ..profile("pi")
            },
        ),
    ]);

    let auto = agent_cell("claude-auto", &profiles, &no_commands());
    let mut expected_auto = vec!["--append".to_owned()];
    expected_auto.extend(
        crate::agents::find_definition("claude")
            .expect("claude")
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
    );
    assert_eq!(auto.kind.as_str(), "claude");
    assert_eq!(auto.launch.profile.as_deref(), Some("claude"));
    assert_eq!(auto.launch.mode, Some(PermissionMode::Auto));
    assert_eq!(auto.args, expected_auto);

    let ask = agent_cell("pi-ask", &profiles, &no_commands());
    assert_eq!(ask.launch.profile.as_deref(), Some("pi"));
    assert_eq!(ask.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(ask.args, vec!["--profile-arg".to_owned()]);

    assert!(matches!(
        parse_layout_spec("pi-yolo", &profiles, &no_commands()),
        Err(LayoutErr::UnknownCell(unknown))
            if unknown.word == "pi-yolo"
                && !unknown.candidates.iter().any(|candidate| candidate.name == "pi-yolo")
    ));
}

#[test]
fn fast_suffix_cells_stamp_fast_on_their_base() {
    let fast_args = |kind: &str| {
        crate::agents::find_definition(kind)
            .expect("kind")
            .spec()
            .launch
            .fast_args()
            .expect("fast switch")
    };
    let claude = agent_cell("claude-fast", &no_profiles(), &no_commands());
    assert_eq!(claude.kind.as_str(), "claude");
    assert!(claude.launch.fast);
    assert_eq!(claude.launch.profile, None);
    assert!(
        claude
            .args
            .windows(2)
            .any(|pair| pair == fast_args("claude").as_slice()),
        "{:?}",
        claude.args
    );

    let profiles = profiles([
        (
            "reviewer",
            Profile {
                agent: "codex".to_owned(),
                effort: Some("high".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "x-fast",
            Profile {
                agent: "claude".to_owned(),
                ..profile("claude")
            },
        ),
    ]);
    let reviewer = agent_cell("reviewer-fast", &profiles, &no_commands());
    assert_eq!(reviewer.kind.as_str(), "codex");
    assert_eq!(reviewer.launch.profile.as_deref(), Some("reviewer"));
    assert_eq!(reviewer.launch.effort.as_deref(), Some("high"));
    assert!(reviewer.launch.fast);
    assert!(
        reviewer
            .args
            .windows(4)
            .any(|window| window == fast_args("codex").as_slice()),
        "{:?}",
        reviewer.args
    );

    let yolo = agent_cell("codex-yolo-fast", &no_profiles(), &no_commands());
    assert_eq!(yolo.kind.as_str(), "codex");
    assert_eq!(yolo.launch.mode, Some(PermissionMode::Yolo));
    assert!(yolo.launch.fast);
    for arg in crate::agents::find_definition("codex")
        .expect("codex")
        .spec()
        .launch
        .permission_args(PermissionMode::Yolo)
    {
        assert!(yolo.args.contains(&arg), "{:?}", yolo.args);
    }

    // An exact profile name wins over the suffix reading.
    let exact = agent_cell("x-fast", &profiles, &no_commands());
    assert_eq!(exact.launch.profile.as_deref(), Some("x-fast"));
    assert!(!exact.launch.fast);

    let cursor = agent_cell("cursor-fast", &no_profiles(), &no_commands());
    assert_eq!(cursor.kind, "cursor");
    assert!(cursor.launch.fast, "the finalizer clears it and warns");
    assert!(cursor.args.is_empty());
    assert!(matches!(
        parse_layout_spec("nosuch-fast", &no_profiles(), &no_commands()),
        Err(LayoutErr::UnknownCell(unknown)) if unknown.word == "nosuch-fast"
    ));

    for raw in [
        "claude-fast",
        "reviewer-fast",
        "codex-yolo-fast",
        "cursor-fast",
    ] {
        assert!(is_cell_word(raw, &profiles, &no_commands()), "{raw}");
    }
    assert_eq!(
        split_inline_role("reviewer-fast:critic", &profiles, &no_commands()),
        ("reviewer-fast", Some("critic"))
    );
}

#[test]
fn config_names_reject_grammar_clashes() {
    assert!(
        parse_layout_spec(
            "claude",
            &profiles([("claude", profile("claude"))]),
            &no_commands()
        )
        .is_ok()
    );

    assert_eq!(
        parse_layout_spec(
            "term",
            &profiles([("bad/name", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::InvalidProfileName {
            name: "bad/name".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec(
            "term",
            &profiles([("bad(name)", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::InvalidProfileName {
            name: "bad(name)".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec("term", &no_profiles(), &commands([("bad name", "nvim")])),
        Err(LayoutErr::InvalidCommandName {
            name: "bad name".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec(
            "claude",
            &profiles([("term", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::ReservedProfileName {
            name: "term".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec("claude", &no_profiles(), &commands([("term", "nvim")])),
        Err(LayoutErr::ReservedCommandName {
            name: "term".to_owned(),
        })
    );
    assert!(matches!(
        parse_layout_spec(
            "claude",
            &profiles([("rimz", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::ReservedProfileName { name }) if name == "rimz"
    ));
    assert!(matches!(
        parse_layout_spec("claude", &no_profiles(), &commands([("user", "nvim")])),
        Err(LayoutErr::ReservedCommandName { name }) if name == "user"
    ));

    for name in ["all", "claude-2", "a:b", "a#b"] {
        assert!(matches!(
            parse_layout_spec(
                "term",
                &profiles([(name, profile("claude"))]),
                &no_commands()
            ),
            Err(LayoutErr::ProfileShadowsAddress { name: actual, .. }) if actual == name
        ));
    }
    assert!(matches!(
        parse_layout_spec(
            "bad-command",
            &no_profiles(),
            &commands([("bad-command", "nvim 'unterminated")])
        ),
        Err(LayoutErr::InvalidCommand { command, reason })
            if command == "bad-command" && reason.contains("shell quoting")
    ));
}

#[test]
fn named_teams_compile_roles_and_apply_overrides() {
    let profiles = profiles([
        (
            "coder-base",
            Profile {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                model: Some("base-model".to_owned()),
                effort: Some("medium".to_owned()),
                args: Some("--base".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "planner-base",
            Profile {
                agent: "claude".to_owned(),
                system_prompt_file: Some("/prompts/base.md".into()),
                append_system_prompt_files: vec!["/prompts/profile.md".into()],
                ..profile("claude")
            },
        ),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        team(vec![
            RoleBinding {
                role: "coder".to_owned(),
                profile: "coder-base".to_owned(),
                signals: Vec::new(),
                owns: Vec::new(),
                flip_compact: None,
                idle_compact: None,
                keep_warm: None,
                mode: Some(PermissionMode::Ask),
                model: Some("role-model".to_owned()),
                effort: Some("high".to_owned()),
                fast: None,
                budget: None,
                system_prompt_file: Some("/prompts/coder.md".into()),
                auto_compact: None,
                append_system_prompt_files: Vec::new(),
                args: Some("--role".to_owned()),
            },
            RoleBinding {
                role: "planner".to_owned(),
                profile: "planner-base".to_owned(),
                signals: Vec::new(),
                owns: Vec::new(),
                flip_compact: None,
                idle_compact: None,
                keep_warm: None,
                mode: None,
                model: None,
                effort: None,
                fast: None,
                budget: None,
                system_prompt_file: Some("/prompts/role.md".into()),
                auto_compact: None,
                append_system_prompt_files: vec!["/prompts/role-fragment.md".into()],
                args: None,
            },
        ]),
    )]));

    let spec = resolve_spec(Some("review"), &profiles, &no_commands(), &teams).expect("team");
    assert_eq!(spec.columns.len(), 2);
    assert!(spec.columns.iter().all(|column| !column.stacked));

    let coder = agent_at(&spec, 0, 0);
    assert_eq!(coder.kind.as_str(), "codex");
    assert_eq!(coder.launch.profile.as_deref(), Some("coder-base"));
    assert_eq!(coder.launch.role.as_deref(), Some("coder"));
    assert_eq!(coder.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(coder.launch.model.as_deref(), Some("role-model"));
    assert_eq!(coder.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        coder
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(Path::new("/prompts/coder.md"))
    );
    assert!(coder.args.contains(&"--role".to_owned()));
    assert!(!coder.args.contains(&"--base".to_owned()));

    let planner = agent_at(&spec, 1, 0);
    assert_eq!(planner.kind.as_str(), "claude");
    assert_eq!(planner.launch.profile.as_deref(), Some("planner-base"));
    assert_eq!(planner.launch.role.as_deref(), Some("planner"));
    assert_eq!(
        planner
            .system_prompt_file
            .as_ref()
            .and_then(crate::config::PromptSource::file),
        Some(Path::new("/prompts/role.md"))
    );
    assert_eq!(
        planner.append_system_prompt_files,
        [
            PathBuf::from("/prompts/profile.md").into(),
            PathBuf::from("/prompts/role-fragment.md").into()
        ]
    );
    assert!(planner.args.is_empty());
}

#[test]
fn team_role_specs_preserve_identity_and_disambiguate_dots() {
    let profiles = profiles([
        ("planner-profile", profile("codex")),
        ("notateam.planner", profile("claude")),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![
            role("planner", "planner-profile"),
            role("sub.planner", "planner-profile"),
        ]),
    )]));

    let planner =
        resolve_spec(Some("forge.planner"), &profiles, &no_commands(), &teams).expect("role");
    assert_eq!(planner.columns.len(), 1);
    assert!(matches!(
        agent_at(&planner, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "codex"
                && profile.as_deref() == Some("planner-profile")
                && role.as_deref() == Some("planner")
    ));
    let dotted = resolve_spec(Some("forge.sub.planner"), &profiles, &no_commands(), &teams)
        .expect("dotted role");
    assert_eq!(
        agent_at(&dotted, 0, 0).launch.role.as_deref(),
        Some("sub.planner")
    );
    assert!(matches!(
        resolve_spec(Some("forge.bogus"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::UnknownRoleInTeam { team, role, valid_roles, .. })
            if team == "forge"
                && role == "bogus"
                && valid_roles == "planner, sub.planner"
    ));

    let profile_spec = resolve_spec(Some("notateam.planner"), &profiles, &no_commands(), &teams)
        .expect("dotted profile");
    assert_eq!(
        agent_at(&profile_spec, 0, 0).launch.profile.as_deref(),
        Some("notateam.planner")
    );
    assert_eq!(spec_team("forge", &teams), Some("forge"));
    assert_eq!(spec_team("forge.planner", &teams), Some("forge"));
    assert_eq!(spec_team("notateam.planner", &teams), None);

    let bad_teams = TeamsConfig(BTreeMap::from([(
        "pc.r".to_owned(),
        team(vec![role("planner", "planner-profile")]),
    )]));
    assert_eq!(
        validate_config(&profiles, &no_commands(), &bad_teams),
        Err(LayoutErr::InvalidTeamName {
            name: "pc.r".to_owned(),
        })
    );
}

#[test]
fn explicit_team_layouts_place_roles_and_roleless_cells() {
    let profiles = profiles([
        ("planner-profile", profile("claude")),
        ("coder-profile", profile("codex")),
        ("reviewer-profile", profile("claude")),
    ]);
    let commands = commands([("logs", "tail -f rimz.log")]);
    let teams = TeamsConfig(BTreeMap::from([
        (
            "review".to_owned(),
            team_with_layout(
                vec![
                    role("planner", "planner-profile"),
                    role("coder", "coder-profile"),
                    role("reviewer", "reviewer-profile"),
                ],
                "reviewer/planner,coder+term+logs",
            ),
        ),
        (
            "pair".to_owned(),
            team_with_layout(Vec::new(), "claude,codex"),
        ),
    ]));

    let spec = resolve_spec(Some("review"), &profiles, &commands, &teams).expect("team");
    assert_eq!(spec.columns.len(), 2);
    assert!(spec.columns[0].stacked);
    assert_eq!(
        agent_at(&spec, 0, 0).launch.role.as_deref(),
        Some("reviewer")
    );
    assert_eq!(
        agent_at(&spec, 0, 1).launch.role.as_deref(),
        Some("planner")
    );
    assert_eq!(agent_at(&spec, 1, 0).launch.role.as_deref(), Some("coder"));
    assert_eq!(spec.columns[1].rows[1], Cell::shell());
    assert_eq!(
        spec.columns[1].rows[2],
        Cell::Command {
            argv: vec!["tail".to_owned(), "-f".to_owned(), "rimz.log".to_owned()],
        }
    );

    let pair = resolve_spec(Some("pair"), &profiles, &commands, &teams).expect("roleless team");
    assert_eq!(
        pair.agent_kinds().collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert!(pair.columns.iter().all(|column| !column.stacked));
}

#[test]
fn team_validation_rejects_invalid_roles_and_layouts() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let config = |team| TeamsConfig(BTreeMap::from([("review".to_owned(), team)]));
    let error = |candidate| {
        validate_config(&profiles, &no_commands(), &config(candidate)).expect_err("invalid team")
    };

    assert!(matches!(
        error(team(Vec::new())),
        LayoutErr::EmptyTeam { .. }
    ));
    assert!(matches!(
        error(team(vec![role("bad role", "planner")])),
        LayoutErr::InvalidRoleName { name, .. } if name == "bad role"
    ));
    assert!(matches!(
        error(team(vec![role("planner", "planner"), role("planner", "planner")])),
        LayoutErr::DuplicateRole { role, .. } if role == "planner"
    ));
    assert!(matches!(
        error(team(vec![role("reviewer", "missing")])),
        LayoutErr::UnknownRoleProfile { profile, .. } if profile == "missing"
    ));
    assert!(matches!(
        error(team_with_layout(
            vec![role("planner", "planner")],
            "planner,claude:helper"
        )),
        LayoutErr::UnknownRoleInLayout { role, .. } if role == "claude:helper"
    ));
    let roles = || vec![role("planner", "planner"), role("coder", "coder")];
    assert!(matches!(
        error(team_with_layout(roles(), "planner+term")),
        LayoutErr::RoleNotPlaced { role, .. } if role == "coder"
    ));
    assert!(matches!(
        error(team_with_layout(roles(), "planner+planner,coder")),
        LayoutErr::DuplicateRoleInLayout { role, .. } if role == "planner"
    ));
    assert!(matches!(
        error(team_with_layout(roles(), "planner,coder+ghost")),
        LayoutErr::UnknownRoleInLayout { role, .. } if role == "ghost"
    ));
    for name in ["all", "claude", "user", "rimz"] {
        assert!(matches!(
            validate_config(
                &profiles,
                &no_commands(),
                &config(team(vec![role(name, "planner")]))
            ),
            Err(LayoutErr::RoleShadowsAddress { name: actual, .. }) if actual == name
        ));
    }
}

#[test]
fn team_validation_rejects_empty_and_duplicate_stages() {
    let validate = |stages: &[&str]| {
        let mut candidate = team_with_layout(Vec::new(), "claude,codex");
        candidate.stages = stages.iter().map(|stage| (*stage).to_owned()).collect();
        validate_config(
            &no_profiles(),
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("review".to_owned(), candidate)])),
        )
    };
    for name in [
        "",
        " \t ",
        "\u{2003}",
        " Plan",
        "Plan ",
        "Plan\nReview",
        "Plan\0",
    ] {
        assert_eq!(
            validate(&["Explore", name]),
            Err(LayoutErr::InvalidStageName {
                team: "review".to_owned(),
                name: name.to_owned(),
            })
        );
    }
    assert_eq!(
        validate(&["Plan", "Implement", "Plan"]),
        Err(LayoutErr::DuplicateStage {
            team: "review".to_owned(),
            name: "Plan".to_owned(),
        })
    );
    validate(&[]).expect("undeclared stages");
    assert_eq!(
        validate(&["Explore", "Done"]),
        Err(LayoutErr::ReservedStage {
            team: "review".to_owned(),
            name: "Done".to_owned(),
        })
    );
    validate(&["Explore", "Plan", "Implement (delta)"]).expect("ordered stages");
    validate(&["Plan", "plan"]).expect("stage names are case-sensitive");
}

#[test]
fn team_stage_ownership_validation() {
    let mut candidate = team(vec![role("planner", "claude"), role("coder", "codex")]);
    candidate.stages = ["Explore", "Plan", "Implement", "Review", "Reflect"]
        .map(str::to_owned)
        .to_vec();
    candidate.roles[0].owns = ["Explore", "Plan", "Reflect"].map(str::to_owned).to_vec();
    candidate.roles[1].owns = vec!["Implement".to_owned()];
    validate_team_stages("forge", &candidate).expect("forge with an unowned Review stage");
    let valid = candidate.clone();
    for name in [
        "",
        " ",
        " Plan",
        "Plan ",
        "Plan\nReview",
        "Plan\rReview",
        "Plan\0",
    ] {
        candidate.roles[0].owns = vec![name.to_owned()];
        assert!(matches!(
            validate_team_stages("forge", &candidate),
            Err(LayoutErr::InvalidStageName { .. })
        ));
    }
    candidate.roles[0].owns = vec!["Done".to_owned()];
    assert!(matches!(
        validate_team_stages("forge", &candidate),
        Err(LayoutErr::ReservedStage { .. })
    ));
    candidate.roles[0].owns = vec!["Submit".to_owned()];
    assert!(
        matches!(validate_team_stages("forge", &candidate), Err(LayoutErr::UnknownOwnedStage { role, stage, .. }) if role == "planner" && stage == "Submit")
    );
    candidate.stages.clear();
    validate_team_stages("forge", &candidate).expect("owns defines the vocabulary");
    for role in ["planner\nStage: Done", "planner)", "(planner", "planner\0"] {
        candidate.roles[0].role = role.to_owned();
        assert!(matches!(
            validate_team_stages("forge", &candidate),
            Err(LayoutErr::InvalidRoleName { .. })
        ));
    }
    for (index, previous) in [(0, "planner"), (1, "planner")] {
        candidate = valid.clone();
        candidate.roles[index].owns.push("Plan".to_owned());
        assert!(
            matches!(validate_team_stages("forge", &candidate), Err(LayoutErr::DuplicateStageOwner { roles, .. }) if roles == [previous, candidate.roles[index].role.as_str()])
        );
    }
}

#[test]
fn flip_compact_preflight_uses_effective_policy_and_resolved_launch_kind() {
    use crate::config::FlipCompact;
    use crate::store::message::AutoCompact;

    let preflight = |candidate: &Team, base: Option<&ResolvedProfile>, default| {
        let prepared = prepare_team("forge", candidate, &no_profiles(), base)?;
        validate_flip_compact("forge", &prepared, default)
    };
    let mut candidate = team(vec![role("coder", "claude")]);
    candidate.roles[0].flip_compact = Some(FlipCompact::Threshold(AutoCompact::Tokens(180_000)));
    preflight(&candidate, None, None).expect("Claude supports compact");
    assert!(matches!(
        preflight(&candidate, Some(&ResolvedProfile::bare("amp")), None),
        Err(LayoutErr::FlipCompactUnsupported { kind, .. }) if kind.as_str() == "amp"
    ));
    candidate.roles[0].profile = "amp".to_owned();
    validate_config(
        &no_profiles(),
        &no_commands(),
        &TeamsConfig(BTreeMap::from([("forge".to_owned(), candidate.clone())])),
    )
    .expect("capability checks wait for the final launch kind");
    assert!(matches!(
        preflight(&candidate, None, None),
        Err(LayoutErr::FlipCompactUnsupported { .. })
    ));
    preflight(&candidate, Some(&ResolvedProfile::bare("claude")), None)
        .expect("override supports compact");
    candidate.roles[0].flip_compact = Some(FlipCompact::Off);
    let machine: crate::config::HarnessConfig = toml::from_str("flip_compact = \"180k\"").unwrap();
    preflight(&candidate, None, machine.flip_compact).expect("disabled on unsupported adapter");
    candidate.roles[0].flip_compact = None;
    assert!(matches!(
        preflight(&candidate, None, None),
        Err(LayoutErr::FlipCompactUnsupported { .. })
    ));
    let disabled: crate::config::HarnessConfig =
        toml::from_str("flip_compact = \"off\"").expect("machine off");
    preflight(&candidate, None, disabled.flip_compact).expect("machine disables compaction");
    assert!(matches!(
        preflight(&candidate, None, machine.flip_compact),
        Err(LayoutErr::FlipCompactUnsupported { .. })
    ));
}

#[test]
fn team_prompt_base_checks_wait_for_resolution_after_agent_rebase() {
    let profiles = profiles([(
        "planner",
        Profile {
            system_prompt_file: Some("/prompts/planner.md".into()),
            ..profile("claude")
        },
    )]);
    let mut actual = Vec::new();
    for layout in [None, Some("worker".to_owned())] {
        let mut candidate = team(vec![role("worker", "codex")]);
        candidate.roles[0].owns = vec!["Plan".into()];
        candidate.layout = layout;
        candidate.consensus_file = Some("/prompts/consensus.md".into());
        let teams = TeamsConfig(BTreeMap::from([("crew".into(), candidate)]));
        actual.push(
            validate_config(&profiles, &no_commands(), &teams)
                .err()
                .map(|err| err.to_string()),
        );
        for spec in ["crew", "crew.worker"] {
            let refused = resolve_spec(Some(spec), &profiles, &no_commands(), &teams);
            assert!(
                matches!(
                    &refused,
                    Err(LayoutErr::TeamPromptWithoutBase { team, role, .. })
                        if team == "crew" && role == "worker"
                ),
                "{refused:?}"
            );
            let rebased = resolve_spec_with_agent_override(
                Some(spec),
                &profiles,
                &profiles,
                &no_commands(),
                &teams,
                Some("planner"),
            )
            .unwrap();
            assert_eq!(
                agent_at(&rebased, 0, 0)
                    .team_prompt
                    .as_ref()
                    .unwrap()
                    .consensus,
                crate::harness::team_prompt::Consensus::File("/prompts/consensus.md".into())
            );
        }
    }
    assert_eq!(actual, [None, None]);
}

#[test]
fn team_prompt_without_base_names_the_kind_and_supported_fixes() {
    let actual = ["claude", "amp"].map(|kind| {
        let mut candidate = team(vec![role("worker", kind)]);
        candidate.roles[0].owns = vec!["Plan".into()];
        candidate.consensus_file = Some("/prompts/consensus.md".into());
        resolve_team(
            "crew",
            &TeamsConfig(BTreeMap::from([("crew".into(), candidate)])),
            &no_profiles(),
            &no_commands(),
        )
        .unwrap_err()
        .to_string()
    });
    assert_eq!(
        actual,
        [
            format!(
                "team `crew` sets consensus-file or append-system-prompt-files but role `worker` \
                 (claude) has no base system prompt to compose them onto; add system-prompt-file \
                 to role worker or its profile, or create {} with a nonempty body as the claude \
                 kind base",
                crate::disk::paths::agents_home()
                    .join("agents/claude.md")
                    .display()
            ),
            "team `crew` sets consensus-file or append-system-prompt-files but role `worker` \
             (amp) has no base system prompt to compose them onto; add system-prompt-file to role \
             worker or its profile"
                .into(),
        ]
    );
}

#[test]
fn staged_team_roles_with_a_base_prompt_carry_the_team_layer() {
    let profiles = profiles([(
        "planner",
        Profile {
            system_prompt_file: Some("/prompts/planner.md".into()),
            ..profile("claude")
        },
    )]);
    let resolve = |team: &Team, spec: &str, agent: Option<&str>| {
        resolve_spec_with_agent_override(
            Some(spec),
            &profiles,
            &profiles,
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("review".to_owned(), team.clone())])),
            agent,
        )
    };
    let layer = |spec: &LayoutSpec, column| agent_at(spec, column, 0).team_prompt.clone();
    let built_in = Some(TeamPrompt {
        consensus: crate::harness::team_prompt::Consensus::BuiltIn,
        files: Vec::new(),
    });

    let mut staged = team(vec![role("planner", "planner"), role("coder", "codex")]);
    staged.roles[0].owns = vec!["Plan".to_owned()];
    let spec = resolve(&staged, "review", None).expect("staged team");
    assert_eq!(layer(&spec, 0), built_in);
    assert_eq!(layer(&spec, 1), None, "a role without a base gets no layer");
    let role_spec = resolve(&staged, "review.coder", Some("planner")).expect("rebased role");
    assert_eq!(
        layer(&role_spec, 0),
        built_in,
        "an --agent base that supplies the prompt brings the layer"
    );

    let mut unstaged = staged.clone();
    unstaged.roles[0].owns.clear();
    assert_eq!(
        layer(&resolve(&unstaged, "review", None).expect("unstaged"), 0),
        None
    );
    unstaged.append_system_prompt_files = vec!["/prompts/pipeline.md".into()];
    assert!(matches!(
        resolve(&unstaged, "review", None),
        Err(LayoutErr::TeamPromptWithoutStages { team }) if team == "review"
    ));

    let mut explicit = staged;
    explicit.consensus_file = Some("/prompts/consensus.md".into());
    assert!(matches!(
        resolve(&explicit, "review", None),
        Err(LayoutErr::TeamPromptWithoutBase { team, role, .. }) if team == "review" && role == "coder"
    ));
    explicit.roles.truncate(1);
    explicit.append_system_prompt_files = vec!["/prompts/pipeline.md".into()];
    assert_eq!(
        layer(
            &resolve(&explicit, "review", None).expect("explicit layer"),
            0
        ),
        Some(TeamPrompt {
            consensus: crate::harness::team_prompt::Consensus::File("/prompts/consensus.md".into()),
            files: vec!["/prompts/pipeline.md".into()],
        })
    );
}

#[test]
fn team_validation_rejects_unsafe_scratch_patterns() {
    let profiles = profiles([("planner", profile("claude"))]);
    let error = |pattern: &str| {
        let mut candidate = team(vec![role("planner", "planner")]);
        candidate.scratch_files = Some(vec![pattern.to_owned()]);
        validate_config(
            &profiles,
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("review".to_owned(), candidate)])),
        )
        .expect_err("invalid scratch pattern")
    };

    for (pattern, reason) in [
        ("", "patterns cannot be empty"),
        (" \t ", "patterns cannot be empty"),
        (" /plan.md", "leading or trailing whitespace is not allowed"),
        ("/plan.md ", "leading or trailing whitespace is not allowed"),
        ("../plan.md", "`..` path segments are not allowed"),
        ("notes/../plan.md", "`..` path segments are not allowed"),
        ("!plan.md", "negation patterns are not allowed"),
        ("# plan", "comment lines are not patterns"),
        ("plan\nreview", "ASCII control characters are not allowed"),
        ("plan\rreview", "ASCII control characters are not allowed"),
        ("plan\0review", "ASCII control characters are not allowed"),
    ] {
        assert!(matches!(
            error(pattern),
            LayoutErr::InvalidScratchPattern {
                team,
                pattern: actual,
                reason: actual_reason,
            } if team == "review" && actual == pattern && actual_reason == reason
        ));
    }

    let mut anchored = team(vec![role("planner", "planner")]);
    anchored.scratch_files = Some(vec!["/plan.md".to_owned(), "/*-notes.md".to_owned()]);
    validate_config(
        &profiles,
        &no_commands(),
        &TeamsConfig(BTreeMap::from([("review".to_owned(), anchored)])),
    )
    .expect("leading slash is a valid gitignore root anchor");
}

#[test]
fn default_team_defers_role_budget_validation() {
    let profiles = profiles([("planner", profile("claude"))]);
    let mut budget_role = role("planner", "planner");
    budget_role.budget = Some("not-a-budget".to_owned());
    let mut teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        team(vec![budget_role, role("bad role", "planner")]),
    )]));

    assert!(matches!(
        validate_config(&profiles, &no_commands(), &teams),
        Err(LayoutErr::InvalidRoleName { name, .. }) if name == "bad role"
    ));

    teams.0.get_mut("review").expect("team").roles[1].role = "coder".to_owned();
    validate_config(&profiles, &no_commands(), &teams)
        .expect("default team defers budget normalization");
    assert!(matches!(
        resolve_spec(Some("review"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::InvalidProfile { profile, .. }) if profile == "planner"
    ));
}

#[test]
fn team_roles_accept_implicit_builtin_profiles() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude"), role("coder", "codex")]),
    )]));

    validate_config(&no_profiles(), &no_commands(), &teams).expect("built-in profiles validate");
    let spec = resolve_spec(Some("forge"), &no_profiles(), &no_commands(), &teams)
        .expect("built-in profiles resolve");
    assert!(matches!(
        agent_at(&spec, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "claude"
                && profile.as_deref() == Some("claude")
                && role.as_deref() == Some("planner")
    ));
    assert!(matches!(
        agent_at(&spec, 1, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "codex"
                && profile.as_deref() == Some("codex")
                && role.as_deref() == Some("coder")
    ));
}

#[test]
fn team_roles_allow_clearing_inherited_empty_dir() {
    let profiles = profiles([
        (
            "searcher",
            Profile {
                empty_dir: Some(true),
                ..profile("claude")
            },
        ),
        (
            "local",
            Profile {
                empty_dir: Some(false),
                ..profile("searcher")
            },
        ),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "local")]),
    )]));
    let result = resolve_spec(Some("forge"), &profiles, &no_commands(), &teams);
    assert!(
        result.is_ok(),
        "false clears inherited empty-dir for teams: {result:?}"
    );
}

#[test]
fn team_leader_validation_accepts_one_target() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let validate = |team| {
        validate_config(
            &profiles,
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team)])),
        )
    };
    let mut declared = team(vec![role("planner", "planner"), role("coder", "coder")]);
    declared.leader = Some("coder".to_owned());
    validate(declared.clone()).expect("declared leader");

    declared.leader = Some("reviewer".to_owned());
    assert!(matches!(
        validate(declared),
        Err(LayoutErr::UnknownLeaderRole { leader, valid_roles, .. })
            if leader == "reviewer" && valid_roles == "planner, coder"
    ));

    let layout_only = |leader: &str, layout: &str| Team {
        keep_warm: None,
        roles: Vec::new(),
        leader: Some(leader.to_owned()),
        layout: Some(layout.to_owned()),
        scratch_files: None,
        consensus_file: None,
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    };
    validate(layout_only("claude", "claude,codex")).expect("unique layout leader");
    assert!(matches!(
        validate(layout_only("pi", "claude,codex")),
        Err(LayoutErr::UnknownLeaderRole { leader, .. }) if leader == "pi"
    ));
    assert_eq!(
        validate(layout_only("claude", "claude,claude")),
        Err(LayoutErr::AmbiguousPromptLeader {
            token: "claude".to_owned(),
        })
    );
}

#[test]
fn prompt_leader_selects_or_rejects_targets() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let commands = commands([("vim", "vim")]);
    let mut configured = team_with_layout(
        vec![role("planner", "planner"), role("coder", "coder")],
        "coder,planner",
    );
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), configured.clone())]));
    let layout = resolve_spec(Some("forge"), &profiles, &commands, &teams).expect("reordered team");
    assert_eq!(prompt_leader(&layout, Some(&configured)), Ok(1));

    configured.leader = Some("coder".to_owned());
    assert_eq!(prompt_leader(&layout, Some(&configured)), Ok(0));

    let command_first =
        parse_layout_spec("vim,codex+term", &profiles, &commands).expect("one agent");
    assert_eq!(prompt_leader(&command_first, None), Ok(0));
    let unique =
        parse_layout_spec("claude,codex", &no_profiles(), &no_commands()).expect("unique first");
    assert_eq!(prompt_leader(&unique, None), Ok(0));
    let inline = parse_layout_spec("claude:lead,claude", &no_profiles(), &no_commands())
        .expect("inline leader");
    assert_eq!(prompt_leader(&inline, None), Ok(0));

    let ambiguous =
        parse_layout_spec("claude,claude", &no_profiles(), &no_commands()).expect("duplicate");
    assert_eq!(
        prompt_leader(&ambiguous, None),
        Err(LayoutErr::AmbiguousPromptLeader {
            token: "claude".to_owned(),
        })
    );
    assert_eq!(
        prompt_leader(
            &parse_layout_spec("term", &no_profiles(), &no_commands()).expect("terminal"),
            None
        ),
        Err(LayoutErr::NoPromptTarget)
    );
}

#[test]
fn default_tab_title_uses_launch_identity_and_layout_order() {
    let layout = parse_layout_spec("term,codex", &no_profiles(), &no_commands()).expect("layout");
    let single = parse_layout_spec("claude", &no_profiles(), &no_commands()).expect("agent");
    let terminal = LayoutSpec::single(Cell::shell());
    let command = parse_layout_spec(
        "vim,claude",
        &no_profiles(),
        &commands([("vim", "/usr/bin/nvim -p")]),
    )
    .expect("command layout");
    let profile = parse_layout_spec(
        "opus",
        &profiles([("opus", profile("claude"))]),
        &no_commands(),
    )
    .expect("profile");
    let capped = parse_layout_spec("claude,codex,pi,claude", &no_profiles(), &no_commands())
        .expect("four-cell layout");
    let empty = LayoutSpec {
        columns: Vec::new(),
    };
    let shell_name = crate::proc::shell_pane_name();
    let mixed_title = format!("{shell_name}+codex");

    for (spec, worktree, team, expected) in [
        (&layout, Some("tab-name"), Some("forge"), "#tab-name"),
        (&layout, None, Some("forge"), "team:forge"),
        (&layout, None, None, mixed_title.as_str()),
        (&single, None, None, "claude"),
        (&terminal, None, None, shell_name.as_str()),
        (&command, None, None, "nvim+claude"),
        (&profile, None, None, "opus"),
        (&capped, None, None, "claude+codex+pi+…"),
        (&empty, None, None, "term"),
    ] {
        assert_eq!(default_tab_title(spec, worktree, team), expected);
    }
}

#[test]
fn bare_role_qualifies_against_the_lane_team() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![
            role("planner", "claude"),
            role("reviewer", "reviewer"),
        ]),
    )]));
    let profiles = profiles([("reviewer", profile("claude"))]);
    let commands = commands([]);

    // A role bound to an unrelated profile qualifies.
    assert_eq!(
        qualify_spec_in_channel("planner", "forge", "forge", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Owned::<str>("forge.planner".to_owned())
    );
    // A role bound to its same-named profile refines it, so it qualifies too.
    assert_eq!(
        qualify_spec_in_channel("reviewer", "forge", "forge", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Owned::<str>("forge.reviewer".to_owned())
    );
}

#[test]
fn role_colliding_with_an_unrelated_cell_word_refuses() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude"), role("codex", "claude")]),
    )]));
    let profiles = profiles([]);
    let commands = commands([]);

    // `codex` is a registered agent kind, and the role binds a different agent.
    let err = qualify_spec_in_channel("codex", "forge", "forge", &teams, &profiles, &commands)
        .expect_err("ambiguous");
    assert!(matches!(err, LayoutErr::AmbiguousInChannel { .. }));
    let message = err.to_string();
    assert!(message.contains("forge.codex"), "{message}");
}

#[test]
fn specs_that_need_no_help_pass_through_unchanged() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude")]),
    )]));
    let profiles = profiles([]);
    let commands = commands([]);

    for raw in [
        "forge.planner", // already qualified
        "forge",         // the whole team
        "claude+codex",  // an inline layout
        "claude:lead",   // an inline role
        "claude",        // a kind the team declares no role for
        "",              // no spec at all
    ] {
        assert_eq!(
            qualify_spec_in_channel(raw, "forge", "forge", &teams, &profiles, &commands)
                .expect("qualify"),
            Cow::Borrowed(raw),
            "{raw}"
        );
    }

    // A stale stamp naming a team the config no longer declares.
    assert_eq!(
        qualify_spec_in_channel("planner", "gone", "gone", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Borrowed("planner")
    );
}
