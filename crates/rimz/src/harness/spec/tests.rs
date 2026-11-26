use super::*;
use crate::config::{RoleBinding, Team};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[test]
fn team_roles_accept_bare_mini_runtime() {
    let profiles = ProfilesConfig::default();
    let teams = toml::from_str(
        "[forge]\nleader = 'coder'\nroles = [{role = 'coder', profile = 'opencode-mini'}]",
    )
    .unwrap();
    let actual = resolve_spec_with_agent_override(
        Some("forge"),
        &profiles,
        &profiles,
        &CommandsConfig::default(),
        &teams,
        None,
    )
    .map(|layout| {
        let cell = layout.agent_cells().next().unwrap();
        (
            cell.kind.to_string(),
            cell.args.first().map(String::as_str) == Some("mini"),
            cell.launch.role.clone(),
        )
    })
    .map_err(|error| error.to_string());
    assert_eq!(actual, Ok(("opencode".into(), true, Some("coder".into()))));
}

#[test]
fn mini_runtime_is_explicit_and_rebases_with_the_engine() {
    let profiles = toml_defs(
        "[profiles.mini]\ndescription = 'Mini'\nagent = 'opencode-mini'\nmode = 'yolo'\nargs = '--keep'\n\
         [profiles.child]\ndescription = 'Child'\nagent = 'mini'\n\
         [profiles.plain]\ndescription = 'Plain'\nagent = 'opencode'\n\
         [profiles.writer]\ndescription = 'Writer'\nagent = 'claude'\nsystem-prompt-file = '/prompts/writer.md'\n",
    );
    let actual: Vec<_> = [
        ("mini", None),
        ("child", None),
        ("mini", Some("claude")),
        ("mini", Some("opencode")),
        ("writer", Some("opencode-mini")),
        ("writer", Some("opencode")),
        ("plain", Some("opencode-mini")),
        ("opencode-mini", None),
        ("opencode", None),
    ]
    .map(|(name, base)| {
        resolve_spec_with_agent_override(
            Some(name),
            &profiles,
            &profiles,
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            base,
        )
        .map(|layout| {
            let cell = layout.agent_cells().next().unwrap();
            (
                cell.kind.to_string(),
                cell.args.first().is_some_and(|arg| arg == "mini"),
                cell.args.iter().filter(|arg| *arg == "mini").count(),
                cell.launch.profile.clone(),
                cell.notices.is_empty(),
                name != "writer"
                    || cell.system_prompt_file == profiles.0["writer"].system_prompt_file,
            )
        })
        .map_err(|error| error.to_string())
    })
    .into();
    let expected = [
        ("opencode", true, Some("mini")),
        ("opencode", true, Some("child")),
        ("claude", false, Some("mini")),
        ("opencode", false, Some("mini")),
        ("opencode", true, Some("writer")),
        ("opencode", false, Some("writer")),
        ("opencode", true, Some("plain")),
        ("opencode", true, Some("opencode-mini")),
        ("opencode", false, Some("opencode")),
    ]
    .map(|(kind, mini, profile)| {
        Ok((
            kind.into(),
            mini,
            usize::from(mini),
            profile.map(str::to_owned),
            true,
            true,
        ))
    });
    assert_eq!(actual, expected);
}

#[test]
fn mini_runtime_is_listed_without_permission_or_fast_suffixes() {
    let profiles = ProfilesConfig::default();
    let commands = CommandsConfig::default();
    let actual = ["opencode-mini-yolo", "opencode-mini-fast"].map(|name| {
        let result = parse_cell(name, &profiles, &commands, CellBase::none());
        let Err(LayoutErr::UnknownCell(error)) = result else {
            return false;
        };
        error
            .candidates
            .iter()
            .any(|candidate| candidate.name == "opencode-mini")
            && !error
                .candidates
                .iter()
                .any(|candidate| candidate.name == name)
    });
    assert_eq!(actual, [true, true]);
    assert!(
        valid_agent_overrides_in(&profiles, None)
            .split(", ")
            .any(|name| name == "opencode-mini")
    );
}

#[test]
fn mini_uses_the_canonical_base_and_project_cannot_shadow_its_name() {
    let profiles = toml_defs("[profiles.mini]\ndescription = 'Mini'\nagent = 'opencode-mini'\n");
    for name in ["mini", "opencode-mini"] {
        let resolved = resolve_profile(name, &profiles).unwrap();
        assert_eq!(resolved.kind.as_str(), "opencode");
        assert!(
            resolved
                .system_prompt_file
                .as_ref()
                .is_some_and(|s| s.origin().ends_with("agents/opencode.md")),
            "{name}: {:?}",
            resolved.system_prompt_file
        );
    }
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::write(
        home.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude.\n",
    )
    .unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.opencode-mini]\ndescription = 'Shadow'\nagent = 'claude'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&home.path().join("config.toml"), "", home.path())
            .unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), home.path()).unwrap();
    let err = launch
        .definition_failure_for("opencode-mini")
        .expect("project row named opencode-mini must fail");
    assert!(
        err.contains("[profiles.opencode-mini]")
            && err.contains("takes the name of an agent runtime"),
        "{err}"
    );
}

#[test]
fn mini_allowlist_is_a_literal_runtime_not_its_whole_kind() {
    let profiles: ProfilesConfig =
        toml::from_str("[caller]\nagent = 'claude'\nsubagents = ['opencode-mini']").unwrap();
    assert!(
        validate_subagent_allowlists(
            &profiles,
            &ProfilesConfig::default(),
            &CommandsConfig::default()
        )
        .is_ok()
    );
}

#[test]
fn a_toml_plus_subagent_entry_is_refused_as_markdown_only() {
    let profiles: ProfilesConfig =
        toml::from_str("[caller]\nagent = 'claude'\nsubagents = ['+general']").unwrap();
    let error = validate_subagent_allowlists(
        &profiles,
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        error,
        "profile `caller` allows subagent `+general`: `+` entries add to an inherited list in \
         Markdown definitions only; list the bare name"
    );
}

#[test]
fn unknown_spec_catalog_keeps_kinds_and_only_resolvable_names() {
    let mut profiles = md_defs_raw(&[
        (
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\neffort: low\nfast: on\n---\nClaude.\n",
        ),
        ("perf", "---\ndescription: Perf\nagent: claude\n---\n"),
        (
            "assistant",
            "---\ndescription: Assistant\nagent: perf\n---\n",
        ),
    ]);
    profiles.0.insert("forge.coder".into(), profile("codex"));
    let commands = commands([("build", "printf ready")]);
    let teams = TeamsConfig(BTreeMap::from([
        ("forge".into(), team(vec![role("coder", "assistant")])),
        // The built-in `peer` reaches the candidates from the machine team table, which holds it.
        (
            "peer".into(),
            crate::config::MachineConfig::default().agents.teams.0["peer"].clone(),
        ),
        (
            "review".into(),
            Team {
                leader: Some("claude".into()),
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
    // The suggestion lists the tuning the launch takes, here from the kind base.
    let SpecKind::Profile(perf) = &closest[0].kind else {
        panic!("perf is a profile")
    };
    assert_eq!(
        (perf.effort.as_deref(), perf.fast),
        (Some("low"), Some(crate::agents::Toggle::On))
    );
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
    let profiles = md_defs(&[
        ("worker", "---\ndescription: Worker\nagent: claude\n---\n"),
        (
            "reviewer",
            "---\ndescription: Reviewer\nagent: codex\n---\n",
        ),
        ("alias", "---\ndescription: Alias\nagent: worker\n---\n"),
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
        "team.stage=unknown",
        "team.stage!=",
        "team.stage[feat]=Plan",
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
            let result = prepare_team(
                "mill",
                &team,
                &ProfilesConfig::default(),
                &no_commands(),
                CellBase::none(),
                None,
            );
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
        let result = prepare_team(
            "mill",
            &team,
            &ProfilesConfig::default(),
            &no_commands(),
            CellBase::none(),
            None,
        );
        assert!(result.is_err(), "an unstaged team has no board");
        assert!(result.err().unwrap().to_string().contains("no board"));
    }
    for clause in ["team.stage=Plan", "team.stage!=Done"] {
        let team: Team = toml::from_str(&format!("leader = 'lead'\nstages = ['Plan']\nkeep-warm = {{while = '{clause}'}}\nroles = [{{role = 'lead', profile = 'claude', owns = ['Plan']}}]")).unwrap();
        assert!(
            prepare_team(
                "mill",
                &team,
                &ProfilesConfig::default(),
                &no_commands(),
                CellBase::none(),
                None
            )
            .is_ok()
        );
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

fn profile(agent: &str) -> Profile {
    Profile {
        empty_dir: None,
        allowed_tools: None,
        variant: None,
        definition: None,
        kind_defaults: false,
        kind_base: false,
        built_in_base: false,
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
        smart_compact: None,
        mode: None,
        model: None,
        effort: None,
        fast: None,
        code_mode: None,
        budget: None,
        timeout: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        skills: None,
        seat_consensus_skill: false,
        mcps: None,
        args: None,
    }
}

fn md_defs_raw(files: &[(&str, &str)]) -> ProfilesConfig {
    let home = tempfile::tempdir().expect("md_defs_raw home");
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for (name, content) in files {
        std::fs::write(home.path().join(format!("agents/{name}.md")), content).unwrap();
    }
    let loaded = crate::config::definitions::load(
        home.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &home.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        None,
    );
    assert!(
        loaded.errors.is_empty(),
        "md_defs_raw errors: {:?}",
        loaded.errors
    );
    loaded.agent_profiles
}

fn md_defs(files: &[(&str, &str)]) -> ProfilesConfig {
    let home = tempfile::tempdir().expect("md_defs home");
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for kind in [
        "claude",
        "codex",
        "pi",
        "opencode",
        "antigravity",
        "omp",
        "qwen",
        "minimax",
    ] {
        let tools = if matches!(kind, "claude" | "codex" | "antigravity") {
            "tools: [Read, Skill]\n"
        } else {
            ""
        };
        std::fs::write(
            home.path().join(format!("agents/{kind}.md")),
            format!("---\ndescription: {kind} base\n{tools}---\n{kind} house.\n"),
        )
        .unwrap();
    }
    for (name, content) in files {
        std::fs::write(home.path().join(format!("agents/{name}.md")), content).unwrap();
    }
    let loaded = crate::config::definitions::load(
        home.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &home.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        None,
    );
    assert!(
        loaded.errors.is_empty(),
        "md_defs errors: {:?}",
        loaded.errors
    );
    loaded.agent_profiles
}

fn toml_defs_with_bases(bases: &[(&str, &str)], project_toml: &str) -> ProfilesConfig {
    let home = tempfile::tempdir().expect("toml_defs home");
    let project = tempfile::tempdir().expect("toml_defs project");
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    for kind in [
        "claude",
        "codex",
        "pi",
        "opencode",
        "antigravity",
        "omp",
        "qwen",
        "minimax",
    ] {
        let tools = if matches!(kind, "claude" | "codex" | "antigravity") {
            "tools: [Read, Skill]\n"
        } else {
            ""
        };
        std::fs::write(
            home.path().join(format!("agents/{kind}.md")),
            format!("---\ndescription: {kind} base\n{tools}---\n{kind} house.\n"),
        )
        .unwrap();
    }
    for (name, content) in bases {
        std::fs::write(home.path().join(format!("agents/{name}.md")), content).unwrap();
    }
    let cfg_path = home.path().join("config.toml");
    let machine_toml = "[harness]\nauto_compact = 'off'\n";
    std::fs::write(&cfg_path, machine_toml).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&cfg_path, machine_toml, home.path()).unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(project.path().join(".rimz/config.toml"), project_toml).unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), home.path()).unwrap();
    assert!(
        launch.project_definition_errors().is_empty(),
        "toml_defs errors: {:?}",
        launch.project_definition_errors()
    );
    launch.profiles
}

fn toml_defs(project_toml: &str) -> ProfilesConfig {
    toml_defs_with_bases(&[], project_toml)
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
        ("pi", None, None, false),
        ("claude", Some(vec!["Bash"]), Some("opencode"), false),
        ("omp", Some(vec!["Read"]), None, false),
    ] {
        let tools_line = tools.map_or(String::new(), |t| format!("tools: [{}]\n", t.join(", ")));
        let base_file = format!("---\ndescription: {kind} base\n---\n{kind} house.\n");
        let target_base =
            target.map(|t| (t, format!("---\ndescription: {t} base\n---\n{t} house.\n")));
        let parent_file = format!("---\ndescription: Parent\nagent: {kind}\n{tools_line}---\n");
        let child_file = "---\ndescription: Child\nagent: parent\n---\n";
        let mut files: Vec<(&str, String)> = vec![
            (kind, base_file),
            ("parent", parent_file),
            ("child", child_file.to_owned()),
        ];
        if let Some((t, body)) = target_base {
            files.push((t, body));
        }
        let file_refs: Vec<(&str, &str)> = files.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let profiles = md_defs_raw(&file_refs);
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
            let case = format!("{kind} {spec} on {target:?}");
            actual.push((case.clone(), cell.no_skill_tool, request.no_skill_tool));
            expected.push((case, no_skill_tool, no_skill_tool));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn pi_native_tools_render_portably_after_an_engine_switch() {
    let profiles = md_defs_raw(&[
        ("claude", "---\ndescription: Claude base\n---\nClaude.\n"),
        ("pi", "---\ndescription: Pi base\n---\nPi.\n"),
        (
            "worker",
            "---\ndescription: Worker\nagent: pi\ntools: [read, bash]\n---\n",
        ),
    ]);
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
    assert!(
        cell.args.windows(2).any(|w| w == ["--tools", "Read,Bash"]),
        "{:?}",
        cell.args
    );
}

#[test]
fn tools_follow_nearest_role_value_with_explicit_empty_and_base_fallback() {
    let profiles = md_defs_raw(&[
        ("claude", "---\ndescription: Claude base\n---\nClaude.\n"),
        ("pi", "---\ndescription: Pi base\n---\nPi.\n"),
        (
            "parent",
            "---\ndescription: Parent\nagent: claude\ntools: [Read]\n---\n",
        ),
        ("child", "---\ndescription: Child\nagent: parent\n---\n"),
        (
            "empty",
            "---\ndescription: Empty\nagent: parent\ntools: []\n---\n",
        ),
        (
            "replacement",
            "---\ndescription: Replacement\nagent: pi\ntools: [Bash]\n---\n",
        ),
        ("bare", "---\ndescription: Bare\nagent: pi\n---\n"),
    ]);
    for (spec, target, expected) in [
        (
            "child",
            "replacement",
            vec!["--thinking", "xhigh", "--tools", "read", "--no-skills"],
        ),
        (
            "empty",
            "replacement",
            vec!["--thinking", "xhigh", "--no-tools", "--no-skills"],
        ),
        (
            "bare",
            "replacement",
            vec!["--thinking", "xhigh", "--tools", "bash", "--no-skills"],
        ),
        (
            "amp",
            "replacement",
            vec!["--thinking", "xhigh", "--tools", "bash", "--no-skills"],
        ),
        ("bare", "pi", vec!["--thinking", "xhigh"]),
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
    let parent_rules = vec!["Bash(git *)".parse().unwrap()];
    for (child_yaml, child_rules) in [
        ("", None),
        ("allowed-tools: []\n", Some(Vec::new())),
        (
            "allowed-tools: [Read]\n",
            Some(vec!["Read".parse().unwrap()]),
        ),
    ] {
        let expected = child_rules.clone().or_else(|| Some(parent_rules.clone()));
        let child_md = format!("---\ndescription: Child\nagent: parent\n{child_yaml}---\n");
        let profiles = md_defs(&[
            (
                "parent",
                "---\ndescription: Parent\nagent: claude\nallowed-tools: ['Bash(git *)']\n---\n",
            ),
            ("child", &child_md),
        ]);
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
fn tools_survive_profile_cells_fresh_requests_and_the_exec_wire() {
    for (entries, expected) in [
        (None, None),
        (Some(vec![]), Some(vec![])),
        (
            Some(vec!["read", "Bash(git *)", "Bash"]),
            Some(vec!["Read", "Bash"]),
        ),
    ] {
        let tools = entries.map(|entries: Vec<&str>| {
            crate::agents::ToolSet::parse(
                &entries.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
            .unwrap()
        });
        let expected =
            expected.map(|names: Vec<&str>| names.iter().map(|s| (*s).to_owned()).collect());
        let profiles = ProfilesConfig::from(BTreeMap::from([(
            "worker".to_owned(),
            Profile {
                tools,
                ..profile("claude")
            },
        )]));
        let cell = agent_cell("worker", &profiles, &CommandsConfig::default());
        assert_eq!(cell.tools, expected);
        let posture = crate::harness::plan::ResumeLaunchPosture::from(&cell);
        assert_eq!(posture.tools, expected);
        let request =
            crate::harness::launch::ExecRequest::fresh(&cell, Default::default(), None, false);
        assert_eq!(request.tools, expected);
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire.get("tools").is_some(), expected.is_some());
        let decoded: crate::harness::launch::ExecRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.tools, expected);
    }
}

#[test]
fn allowed_tools_override_preserves_explicit_empty() {
    let base_rules = Some(vec!["Read".parse().unwrap()]);
    for (yaml, rules) in [
        ("", None),
        ("allowed-tools: []\n", Some(Vec::new())),
        (
            "allowed-tools: [Write]\n",
            Some(vec!["Write".parse().unwrap()]),
        ),
    ] {
        let worker_md = format!("---\ndescription: Worker\nagent: claude\n{yaml}---\n");
        let profiles = md_defs(&[
            (
                "target",
                "---\ndescription: Target\nagent: codex\nallowed-tools: [Read]\n---\n",
            ),
            ("worker", &worker_md),
        ]);
        assert_eq!(
            resolve_profile_under("worker", Some("target"), &profiles, None)
                .unwrap()
                .allowed_tools,
            rules.or_else(|| base_rules.clone())
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
    let parent_skills = Some(vec!["merge".parse().unwrap()]);
    for (yaml, child_skills) in [
        ("", None),
        ("skills: []\n", Some(Vec::new())),
        ("skills: [rebase]\n", Some(vec!["rebase".parse().unwrap()])),
    ] {
        let expected = child_skills.clone().or_else(|| parent_skills.clone());
        let child_md = format!("---\ndescription: Child\nagent: parent\n{yaml}---\n");
        let profiles = md_defs(&[
            (
                "parent",
                "---\ndescription: Parent\nagent: claude\nskills: [merge]\n---\n",
            ),
            ("child", &child_md),
        ]);
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
    for (child, expected) in [("", "host"), ("isolation: scoped\n", "scoped")] {
        let child_md = format!("---\ndescription: Child\nagent: parent\n{child}---\n");
        let profiles = md_defs(&[
            (
                "parent",
                "---\ndescription: Parent\nagent: claude\nisolation: host\n---\n",
            ),
            ("child", &child_md),
        ]);
        let cell = agent_cell("child", &profiles, &CommandsConfig::default());
        let request =
            crate::harness::launch::ExecRequest::fresh(&cell, Default::default(), None, false);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["isolation_default"], expected);
        assert!(request.identity.params.isolation.is_none());
    }
}

#[test]
fn overridden_profile_skills_preserve_explicit_empty() {
    let base_skills = Some(vec!["merge".parse().unwrap()]);
    let profiles = md_defs(&[
        (
            "target",
            "---\ndescription: Target\nagent: codex\nskills: [merge]\n---\n",
        ),
        ("worker", "---\ndescription: Worker\nagent: claude\n---\n"),
        (
            "cleared",
            "---\ndescription: Cleared\nagent: claude\nskills: []\n---\n",
        ),
    ]);
    assert_eq!(
        resolve_profile_under("worker", Some("target"), &profiles, None)
            .unwrap()
            .skills,
        base_skills
    );
    assert_eq!(
        resolve_profile_under("cleared", Some("target"), &profiles, None)
            .unwrap()
            .skills,
        Some(Vec::new())
    );
}

#[test]
fn profile_mcps_inherit_replace_clear_and_survive_override() {
    let parent_mcps = Some(vec!["rimz-lsp".parse().unwrap()]);
    for (yaml, child_mcps) in [
        ("", None),
        ("mcps: []\n", Some(Vec::new())),
        ("mcps: [github]\n", Some(vec!["github".parse().unwrap()])),
    ] {
        let expected = child_mcps.clone().or_else(|| parent_mcps.clone());
        let child_md = format!("---\ndescription: Child\nagent: parent\n{yaml}---\n");
        let leaf_md = format!("---\ndescription: Leaf\nagent: claude\n{yaml}---\n");
        let profiles = md_defs(&[
            (
                "parent",
                "---\ndescription: Parent\nagent: claude\nmcps: [rimz-lsp]\n---\n",
            ),
            ("child", &child_md),
            (
                "target",
                "---\ndescription: Target\nagent: codex\nmcps: [rimz-lsp]\n---\n",
            ),
            ("leaf", &leaf_md),
        ]);
        assert_eq!(resolve_profile("child", &profiles).unwrap().mcps, expected);
        assert_eq!(
            agent_cell("child", &profiles, &CommandsConfig::default()).mcps,
            expected
        );
        assert_eq!(
            resolve_profile_under("leaf", Some("target"), &profiles, None)
                .unwrap()
                .mcps,
            expected
        );
    }
}

#[test]
fn overridden_profile_isolation_keeps_the_job_default() {
    use crate::config::Isolation::{Host, Scoped};
    let profiles = md_defs(&[
        (
            "target",
            "---\ndescription: Target\nagent: codex\nisolation: scoped\n---\n",
        ),
        ("unset", "---\ndescription: Unset\nagent: claude\n---\n"),
        (
            "host_job",
            "---\ndescription: Host job\nagent: claude\nisolation: host\n---\n",
        ),
    ]);
    assert_eq!(
        resolve_profile_under("unset", Some("target"), &profiles, None)
            .unwrap()
            .isolation_default,
        Some(Scoped)
    );
    assert_eq!(
        resolve_profile_under("host_job", Some("target"), &profiles, None)
            .unwrap()
            .isolation_default,
        Some(Host)
    );
}

fn profiles(entries: impl IntoIterator<Item = (&'static str, Profile)>) -> ProfilesConfig {
    ProfilesConfig::from(
        entries
            .into_iter()
            .map(|(name, profile)| (name.to_owned(), profile))
            .collect::<BTreeMap<_, _>>(),
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
        smart_compact: None,
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
        leader: roles.first().map(|binding| binding.role.clone()),
        roles,
        layout: None,
        scratch_files: None,
        consensus_file: None,
        consensus: Vec::new(),
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    }
}

fn team_with_layout(roles: Vec<RoleBinding>, layout: &str) -> Team {
    let first_cell = layout.split([',', '+', '/']).next();
    Team {
        keep_warm: None,
        leader: roles
            .first()
            .map(|binding| binding.role.as_str())
            .or(first_cell)
            .map(str::to_owned),
        roles,
        layout: Some(layout.to_owned()),
        scratch_files: None,
        consensus_file: None,
        consensus: Vec::new(),
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
    use crate::harness::signal::SelectorErr;

    let mut declared = team(vec![role("reviewer", "claude"), role("coder", "codex")]);
    declared.roles[1].signals.push(TeamSignalBinding {
        signal: "ci.failed".to_owned(),
        matches: BTreeMap::new(),
        prompt: None,
    });
    let validate = |team: &Team| {
        prepare_team(
            "forge",
            team,
            &no_profiles(),
            &no_commands(),
            CellBase::none(),
            None,
        )
        .map(|_| ())
    };
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
        }) if matches!(*source, SelectorErr::ObsoleteCiSignal { .. })
    ));
    declared.roles[1].signals[1].matches.clear();
    declared.roles[1].signals[1].signal = "agent.idle".to_owned();
    declared.roles[1].signals[1].matches = BTreeMap::from([("handle".to_owned(), " ".to_owned())]);
    assert!(matches!(
        validate(&declared),
        Err(LayoutErr::InvalidTeamSignal { source, .. })
            if matches!(*source, SelectorErr::BlankMatch { .. })
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
    // The built-in `peer` resolves from the machine team table, which holds it.
    teams.0.insert(
        "peer".to_owned(),
        crate::config::MachineConfig::default().agents.teams.0["peer"].clone(),
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
        &TeamsConfig(BTreeMap::from([(
            "peer".into(),
            team_with_layout(Vec::new(), "claude,codex"),
        )])),
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
    let teams = TeamsConfig(BTreeMap::from([
        ("forge".into(), team(vec![role("coder", "codex")])),
        // The built-in `peer` reaches the candidates from the machine team table, which holds it.
        ("peer".into(), team_with_layout(Vec::new(), "claude,codex")),
    ]));
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
    let profiles = toml_defs_with_bases(
        &[(
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\neffort: high\n---\nClaude.\n",
        )],
        "[profiles.base]\ndescription = 'Base'\nagent = 'codex'\nmodel = 'base-model'\n\
         auto-compact = '1m'\neffort = 'medium'\nargs = '--base'\n\
         [profiles.child]\ndescription = 'Child'\nagent = 'base'\neffort = 'high'\n\
         args = '--child'\nauto-compact = '200k'\n\
         [profiles.inherits-args]\ndescription = 'Inherits args'\nagent = 'base'\n\
         model = 'child-model'\n\
         [profiles.claude-child]\ndescription = 'Claude child'\nagent = 'claude'\n",
    );

    let bare = resolve_profile("claude", &no_profiles()).expect("built-in");
    assert_eq!(bare.kind.as_str(), "claude");
    assert!(bare.layers.is_empty());
    assert_eq!(
        resolve_profile("claude", &profiles).unwrap().layers,
        ["claude"]
    );
    let child = resolve_profile("child", &profiles).expect("child");
    assert_eq!(child.kind.as_str(), "codex");
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
    let profiles = toml_defs(
        "[profiles.codex-deep]\ndescription = 'Codex deep'\nagent = 'codex'\nmode = 'auto'\n\
         model = 'gpt-5-codex'\nauto-compact = '200k'\neffort = 'high'\n\
         args = '--profile reviewer'\n\
         [profiles.prompt-base]\ndescription = 'Prompt base'\nagent = 'claude'\n\
         system-prompt-file = '/prompts/base.md'\nauto-compact = '200000'\n\
         append-system-prompt-files = ['/prompts/base-fragment.md']\n\
         [profiles.prompt-child]\ndescription = 'Prompt child'\nagent = 'prompt-base'\n\
         append-system-prompt-files = ['/prompts/child-fragment.md']\n",
    );
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
    let base_tools =
        crate::agents::ToolSet::parse(&["Read".into(), "Skill".into()]).expect("tools");
    let (tool_args, _) = crate::agents::render_launch_tool_args(
        &AgentKind::new_unchecked("codex"),
        Some(&base_tools),
        None,
    )
    .expect("codex tools");
    expected.extend(tool_args);
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
    assert!(
        prompt
            .args
            .windows(2)
            .any(|w| w == ["--autocompact", "200000"]),
        "{:?}",
        prompt.args
    );
}

#[test]
fn cross_kind_override_replaces_provider_fields_and_carries_portable_fields() {
    let mut profiles = toml_defs(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\nmode = 'auto'\n\
         model = 'claude-model'\neffort = 'high'\nbudget = '$4'\nauto-compact = '200k'\n\
         system-prompt-file = '/prompts/system.md'\n\
         append-system-prompt-files = ['/prompts/fragment.md']\nargs = '--strict-mcp-config'\n",
    );
    profiles.0.get_mut("planner").unwrap().tier_stamp = Some(Box::new(crate::agents::TierStamp {
        tier: crate::config::tiers::ModelTier::Senior,
        model: "claude-model".to_owned(),
        used_tier: None,
        unbuildable: Vec::new(),
        skipped: Vec::new(),
    }));
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
    let profiles = toml_defs(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\neffort = 'high'\n\
         append-system-prompt-files = ['/prompts/planner.md']\n\
         [profiles.codex-x]\ndescription = 'Codex X'\nagent = 'codex'\nmodel = 'codex-model'\n\
         append-system-prompt-files = ['/prompts/codex.md']\nargs = '--base-only'\n",
    );
    let spec = resolve_spec_with_agent_override(
        Some("planner"),
        &profiles,
        &profiles,
        &no_commands(),
        &TeamsConfig::default(),
        Some("codex-x"),
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
    let self_named = md_defs_raw(&[(
        "codex",
        "---\ndescription: Codex base\ntools: [Read]\n---\nCodex.\n",
    )]);
    let resolved = resolve_profile("codex", &self_named).unwrap();
    assert_eq!(resolved.layers, ["codex"]);
    assert_eq!(resolved.kind, "codex");
    assert_eq!(resolved.into_chain(), ["codex"]);
    let rebased = resolve_profile_under("codex", Some("claude"), &self_named, None).unwrap();
    assert_eq!(rebased.kind, "claude");
    assert_eq!(rebased.into_chain(), ["codex", "claude"]);

    let writer_profiles = md_defs(&[
        ("writer", "---\ndescription: Writer\nagent: claude\n---\n"),
        ("fast", "---\ndescription: Fast\nagent: codex\n---\n"),
    ]);
    for (name, agent_override, kind, chain) in [
        ("writer", Some("writer"), "claude", vec!["writer", "claude"]),
        (
            "writer",
            Some("fast"),
            "codex",
            vec!["writer", "fast", "codex"],
        ),
        ("claude", None, "claude", vec!["claude"]),
    ] {
        let resolved = resolve_profile_under(name, agent_override, &writer_profiles, None).unwrap();
        assert_eq!(resolved.kind, kind);
        assert_eq!(resolved.into_chain(), chain);
    }
    let original = resolve_profile("writer", &writer_profiles).unwrap();
    let rebased = resolve_profile_under("writer", Some("writer"), &writer_profiles, None).unwrap();
    assert_eq!(rebased.kind, original.kind);
    assert_eq!(rebased.into_chain(), original.into_chain());

    let profiles = toml_defs(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
         model = 'claude-model'\nargs = '--profile planner'\n\
         [profiles.switch-base]\ndescription = 'Switch base'\nagent = 'codex'\n\
         [profiles.switch]\ndescription = 'Switch'\nagent = 'switch-base'\n",
    );
    for (agent_override, kind, expected_chain) in [
        (None, "claude", vec!["planner", "claude"]),
        (Some("claude"), "claude", vec!["planner", "claude"]),
        (Some(" codex "), "codex", vec!["planner", "codex"]),
        (
            Some("switch"),
            "codex",
            vec!["planner", "switch", "switch-base", "codex"],
        ),
    ] {
        let resolved = resolve_profile_under("planner", agent_override, &profiles, None).unwrap();
        assert_eq!(resolved.kind, kind);
        assert_eq!(resolved.into_chain(), expected_chain);
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
    use PermissionMode::Plan;

    let profiles = md_defs(&[
        (
            "markdown",
            "---\ndescription: Markdown\nagent: claude\n---\n",
        ),
        (
            "inherited",
            "---\ndescription: Inherited\nagent: markdown\n---\n",
        ),
        ("qwen-job", "---\ndescription: Qwen\nagent: qwen\n---\n"),
        ("toml", "---\ndescription: Toml\nagent: claude\n---\n"),
        ("codex-base", "---\ndescription: Codex\nagent: codex\n---\n"),
        (
            "fable-base",
            "---\ndescription: Fable\nagent: claude\nmodel: fable\n---\n",
        ),
        (
            "written",
            "---\ndescription: Written\nagent: claude\nmode: plan\neffort: high\n---\n",
        ),
        (
            "written-base",
            "---\ndescription: Written base\nagent: codex\nmode: yolo\neffort: low\n---\n",
        ),
    ]);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (source, target, kind, model, mode, effort) in [
        ("markdown", None, "claude", None, None, Some("xhigh")),
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
            None,
            Some("xhigh"),
        ),
        ("toml", Some("codex"), "codex", None, None, Some("xhigh")),
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
            Some("high"),
        ),
        (
            "written",
            Some("qwen"),
            "qwen",
            None,
            Some(Plan),
            Some("high"),
        ),
        (
            "markdown",
            Some("fable-base"),
            "claude",
            Some("fable"),
            None,
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
    assert_eq!(actual, expected);
}

#[test]
fn role_values_survive_defaults_in_both_rebase_orders() {
    use crate::agents::Toggle;
    use PermissionMode::{Auto, Plan};

    let profiles = md_defs(&[
        ("worker", "---\ndescription: Worker\nagent: claude\n---\n"),
        (
            "fable",
            "---\ndescription: Fable\nagent: claude\nmodel: fable\n---\n",
        ),
        (
            "fast-codex",
            "---\ndescription: Fast codex\nagent: codex\nfast: on\n---\n",
        ),
    ]);
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    // A role's own fast, off and auto included, survives `--agent` onto another kind.
    for (target, model, mode, effort, fast, final_mode, final_effort) in [
        ("codex", None, None, None, None, None, Some("xhigh")),
        ("opencode", None, None, None, None, None, None),
        (
            "codex",
            None,
            Some(Plan),
            Some("high"),
            None,
            Some(Plan),
            Some("high"),
        ),
        ("qwen", None, Some(Auto), None, None, Some(Auto), None),
        (
            "fable",
            None,
            None,
            Some("xhigh"),
            None,
            None,
            Some("xhigh"),
        ),
        (
            "claude",
            Some("fable"),
            None,
            None,
            None,
            None,
            Some("high"),
        ),
        (
            "codex",
            None,
            None,
            None,
            Some(Toggle::Off),
            None,
            Some("xhigh"),
        ),
        (
            "fast-codex",
            None,
            None,
            None,
            Some(Toggle::Auto),
            None,
            Some("xhigh"),
        ),
        (
            "fast-codex",
            None,
            None,
            None,
            Some(Toggle::Off),
            None,
            Some("xhigh"),
        ),
    ] {
        let binding = RoleBinding {
            model: model.map(str::to_owned),
            mode,
            effort: effort.map(str::to_owned),
            fast,
            ..role("coder", "worker")
        };
        let base = resolve_agent_override(Some(target), &profiles, None).unwrap();
        let configured = team(vec![binding.clone()]);
        let prepared = prepare_team(
            "forge",
            &configured,
            &profiles,
            &no_commands(),
            CellBase::under(base.as_ref()),
            None,
        )
        .map(|team| {
            let resolved = &team.roles[0].resolved;
            (
                resolved.launch.mode,
                resolved.launch.effort.clone(),
                resolved.fast.and_then(Toggle::enabled),
            )
        })
        .map_err(|error| error.to_string());
        let replayed = profile_cell_with_base_override(
            "worker",
            &profiles,
            CellBase::under(base.as_ref()),
            Some(&binding),
        )
        .map(|cell| (cell.launch.mode, cell.launch.effort, cell.launch.fast))
        .map_err(|error| error.to_string());
        actual.push((prepared, replayed));
        let launch = Ok((
            final_mode,
            final_effort.map(str::to_owned),
            fast.and_then(Toggle::enabled),
        ));
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
        let Cell::Agent(cell) =
            parse_cell(word, &profiles, &no_commands(), CellBase::none()).unwrap()
        else {
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
    let profiles = toml_defs(
        "[profiles.fixer]\ndescription = 'Fixer'\nagent = 'claude'\nmodel = 'fable'\n\
         effort = 'medium'\nsystem-prompt-file = '/prompts/fixer.md'\n\
         [profiles.opus]\ndescription = 'Opus'\nagent = 'claude'\nmodel = 'opus[1m]'\neffort = 'high'\n\
         [profiles.plain]\ndescription = 'Plain'\nagent = 'claude'\n\
         [profiles.luna]\ndescription = 'Luna'\nagent = 'codex'\neffort = 'xhigh'\n\
         [profiles.pi-model]\ndescription = 'Pi model'\nagent = 'pi'\nmodel = 'replacement'\n",
    );
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
            "medium",
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
            "medium",
            vec!["fixer", "luna", "codex"],
        ),
    ] {
        let resolved =
            resolve_profile_under("fixer", Some(agent_override), &profiles, None).unwrap();
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
    let profiles = toml_defs(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'claude'\n\
         model = 'profile-model'\nauto-compact = '200k'\n\
         [profiles.codex-x]\ndescription = 'Codex X'\nagent = 'codex'\n\
         model = 'codex-model'\neffort = 'high'\nargs = '--base-only'\n",
    );
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
        Some("codex-x"),
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
        Some("codex-x"),
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
        Some("codex-x"),
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
            .any(|args| { args == ["-c".to_owned(), "model_reasoning_effort=high".to_owned()] })
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
        resolve_profile("ghost", &no_profiles()),
        Err(LayoutErr::UnknownProfileBase {
            profile: "ghost".to_owned(),
            base: "ghost".to_owned(),
        })
    );
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::write(
        home.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude.\n",
    )
    .unwrap();
    let mut deep = String::new();
    deep.push_str("[profiles.d0]\ndescription = 'd0'\nagent = 'claude'\n");
    for i in 1..=16 {
        deep.push_str(&format!(
            "[profiles.d{i}]\ndescription = 'd{i}'\nagent = 'd{}'\n",
            i - 1
        ));
    }
    let toml_text = format!(
        "[profiles.planner]\ndescription = 'Planner'\nagent = 'ghost'\n\
         [profiles.a]\ndescription = 'A'\nagent = 'b'\n\
         [profiles.b]\ndescription = 'B'\nagent = 'c'\n\
         [profiles.c]\ndescription = 'C'\nagent = 'a'\n\
         [profiles.bad-args]\ndescription = 'Bad args'\nagent = 'claude'\nargs = \"'unterminated\"\n\
         {deep}"
    );
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(project.path().join(".rimz/config.toml"), toml_text).unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&home.path().join("config.toml"), "", home.path())
            .unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), home.path()).unwrap();
    assert!(
        launch
            .definition_failure_for("planner")
            .unwrap()
            .contains("follows unknown profile 'ghost'")
    );
    assert!(
        launch
            .definition_failure_for("a")
            .unwrap()
            .contains("follows itself")
    );
    assert!(
        launch
            .definition_failure_for("d16")
            .unwrap()
            .contains("deeper than 16")
    );
    assert!(
        launch
            .definition_failure_for("bad-args")
            .unwrap()
            .contains("shell quoting")
    );

    let unsupported = toml_defs(
        "[profiles.pi-deep]\ndescription = 'Pi deep'\nagent = 'pi'\n\
         system-prompt-file = '/abs/prompt.md'\n",
    );
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
    use crate::agents::Toggle;
    let profiles = toml_defs(
        "[profiles.swift]\ndescription = 'Swift'\nagent = 'codex'\nfast = 'on'\n\
         [profiles.steady]\ndescription = 'Steady'\nagent = 'swift'\nfast = 'off'\n\
         [profiles.heir]\ndescription = 'Heir'\nagent = 'swift'\n\
         [profiles.cleared]\ndescription = 'Cleared'\nagent = 'swift'\nfast = 'auto'\n\
         [profiles.plain]\ndescription = 'Plain'\nagent = 'codex'\n\
         [profiles.quick]\ndescription = 'Quick'\nagent = 'cursor'\nfast = 'on'\n\
         [profiles.calm]\ndescription = 'Calm'\nagent = 'cursor'\nfast = 'off'\n",
    );
    let on = ["-c", "service_tier=fast", "-c", "features.fast_mode=true"];
    let off = ["-c", "service_tier=default"];
    let has =
        |args: &[String], switch: &[&str]| args.windows(switch.len()).any(|part| part == switch);
    for (name, want) in [
        ("swift", Some(true)),
        ("steady", Some(false)),
        ("heir", Some(true)),
        ("cleared", None),
        ("plain", None),
    ] {
        let cell = profile_cell(name, &profiles).unwrap();
        assert_eq!(cell.launch.fast, want, "{name}");
        assert_eq!(
            has(&cell.args, &on),
            want == Some(true),
            "{name}: {:?}",
            cell.args
        );
        assert_eq!(
            has(&cell.args, &off),
            want == Some(false),
            "{name}: {:?}",
            cell.args
        );
    }
    assert_eq!(
        profile_cell("cleared", &profiles).unwrap().args,
        profile_cell("plain", &profiles).unwrap().args,
        "a written auto renders what an unwritten fast does"
    );
    let quick = profile_cell("quick", &profiles).expect("unsupported fast renders no switch");
    assert_eq!(
        quick.launch.fast,
        Some(true),
        "the finalizer clears it and warns"
    );
    assert!(quick.args.is_empty());
    let calm = profile_cell("calm", &profiles).expect("unsupported off renders no switch");
    assert!(calm.args.is_empty(), "{:?}", calm.args);
    let off = RoleBinding {
        fast: Some(Toggle::Off),
        ..role("coder", "swift")
    };
    assert_eq!(
        resolve_role(&off, &profiles).unwrap().fast,
        Some(Toggle::Off)
    );
    let auto = RoleBinding {
        fast: Some(Toggle::Auto),
        ..role("coder", "swift")
    };
    assert_eq!(
        resolve_role(&auto, &profiles).unwrap().fast,
        Some(Toggle::Auto)
    );
}

#[test]
fn profile_code_mode_follows_the_chain_and_renders_before_raw_args() {
    let profiles = toml_defs(
        "[profiles.coder]\ndescription = 'Coder'\nagent = 'pi'\ncode-mode = 'on'\nargs = '--raw'\n\
         [profiles.plain]\ndescription = 'Plain'\nagent = 'coder'\ncode-mode = 'off'\n\
         [profiles.heir]\ndescription = 'Heir'\nagent = 'coder'\n\
         [profiles.cleared]\ndescription = 'Cleared'\nagent = 'coder'\ncode-mode = 'auto'\n\
         [profiles.excluding]\ndescription = 'Excluding'\nagent = 'pi'\ncode-mode = 'off'\n\
         args = '--exclude-tools read,grep'\n\
         [profiles.tooled]\ndescription = 'Tooled'\nagent = 'claude'\ncode-mode = 'on'\n\
         [profiles.muted]\ndescription = 'Muted'\nagent = 'claude'\ncode-mode = 'off'\n\
         [profiles.pilot]\ndescription = 'Pilot'\nagent = 'copilot'\ncode-mode = 'on'\n\
         [profiles.base]\ndescription = 'Base'\nagent = 'pi'\n\
         [profiles.cdx]\ndescription = 'Cdx'\nagent = 'codex'\ncode-mode = 'on'\nargs = '--raw'\n\
         [profiles.cdx-off]\ndescription = 'Cdx off'\nagent = 'cdx'\ncode-mode = 'off'\n\
         [profiles.cdx-heir]\ndescription = 'Cdx heir'\nagent = 'cdx'\n\
         [profiles.cdx-auto]\ndescription = 'Cdx auto'\nagent = 'cdx'\ncode-mode = 'auto'\n\
         [profiles.cdx-base]\ndescription = 'Cdx base'\nagent = 'codex'\ncode-mode = 'off'\n\
         [profiles.cdx-bare]\ndescription = 'Cdx bare'\nagent = 'codex'\n",
    );
    // Codex renders exactly one of on and off, each `-c` setting before the raw args,
    // wherever the setting came from.
    let codex_switch = |args: &[String]| -> Vec<(usize, String)> {
        args.iter()
            .enumerate()
            .filter(|(_, arg)| arg.starts_with("features.code_mode"))
            .map(|(at, arg)| (at, arg.clone()))
            .collect()
    };
    let named = |settings: &[(usize, String)]| -> Vec<String> {
        settings.iter().map(|(_, arg)| arg.clone()).collect()
    };
    let on = ["features.code_mode_only=true"];
    let off = ["features.code_mode=false", "features.code_mode_only=false"];
    for (name, want) in [
        ("cdx", &on[..]),
        ("cdx-off", &off[..]),
        ("cdx-heir", &on[..]),
        ("cdx-auto", &[][..]),
    ] {
        let cell = profile_cell(name, &profiles).unwrap();
        let settings = codex_switch(&cell.args);
        assert_eq!(named(&settings), want, "{name}");
        for (at, _) in settings {
            assert_eq!(cell.args[at - 1], "-c", "{name}: {:?}", cell.args);
            let raw = cell.args.iter().position(|arg| arg == "--raw").unwrap();
            assert!(at < raw, "{name}: {:?}", cell.args);
        }
    }
    for name in ["cdx-base", "cdx-bare"] {
        let base = resolve_agent_override(Some(name), &profiles, None).unwrap();
        let rebased = profile_cell_with_base_override(
            "tooled",
            &profiles,
            CellBase::under(base.as_ref()),
            None,
        )
        .unwrap();
        assert_eq!(rebased.kind.as_str(), "codex");
        assert_eq!(named(&codex_switch(&rebased.args)), on, "{name}");
    }
    let switch = ["--tools", "+codemode"];
    for (name, want) in [
        ("coder", Some(true)),
        ("plain", Some(false)),
        ("heir", Some(true)),
        ("cleared", None),
    ] {
        let cell = profile_cell(name, &profiles).unwrap();
        assert_eq!(cell.launch.code_mode, want, "{name}");
        let at = cell.args.windows(2).position(|part| part == switch);
        assert_eq!(at.is_some(), want == Some(true), "{name}: {:?}", cell.args);
        let raw = cell.args.iter().position(|arg| arg == "--raw");
        assert!(at < raw, "{name}: {:?}", cell.args);
    }
    let cleared = profile_cell("cleared", &profiles).unwrap();
    assert!(
        !cleared.args.iter().any(|arg| arg.contains("codemode")),
        "{:?}",
        cleared.args
    );

    // Off excludes on the final argv, past any passthrough, so the cell keeps its lists as
    // written; `harness::launch::tests` pins the join.
    let excluding = profile_cell("excluding", &profiles).unwrap();
    assert_eq!(excluding.launch.code_mode, Some(false));
    assert!(
        excluding
            .args
            .ends_with(&["--exclude-tools".to_owned(), "read,grep".to_owned()]),
        "{:?}",
        excluding.args
    );

    // Claude project profiles inherit the kind base's tools list, so code-mode appends REPL,Poll
    // while off renders the kind base's tools alone and carries off on the cell for the env.
    let claude = profile_cell("tooled", &profiles).unwrap();
    assert_eq!(claude.launch.code_mode, Some(true));
    assert!(
        claude
            .args
            .windows(2)
            .any(|part| part == ["--tools", "Read,Skill,REPL,Poll"]),
        "{:?}",
        claude.args
    );
    let muted = profile_cell("muted", &profiles).unwrap();
    assert_eq!(muted.launch.code_mode, Some(false));
    assert_eq!(
        muted.args,
        ["--effort", "xhigh", "--tools", "Read,Skill"],
        "Claude's off leaves REPL/Poll out and rides the env alone"
    );

    let pilot = profile_cell("pilot", &profiles).expect("unsupported code mode renders nothing");
    assert_eq!(
        pilot.launch.code_mode,
        Some(true),
        "the finalizer clears it and warns"
    );
    assert!(pilot.args.is_empty(), "{:?}", pilot.args);

    // L's written code-mode wins over a replacement base's value on another kind too.
    let base = resolve_agent_override(Some("base"), &profiles, None).unwrap();
    let rebased =
        profile_cell_with_base_override("tooled", &profiles, CellBase::under(base.as_ref()), None)
            .unwrap();
    assert_eq!(rebased.kind.as_str(), "pi");
    assert_eq!(rebased.launch.code_mode, Some(true));
    assert!(
        rebased.args.iter().any(|arg| arg.contains("codemode")),
        "{:?}",
        rebased.args
    );
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

    let yolo = agent_cell("pi-yolo", &profiles, &no_commands());
    assert_eq!(yolo.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(yolo.args, vec!["--profile-arg".to_owned()]);
}

#[test]
fn mapped_permission_cells_accept_empty_vectors_without_widening_other_kinds() {
    for spec in ["opencode-auto", "pi-auto", "pi-yolo"] {
        for suffix in ["", "-fast"] {
            let layout =
                parse_layout_spec(&format!("{spec}{suffix}"), &no_profiles(), &no_commands());
            assert!(layout.is_ok(), "{spec}{suffix}: {layout:?}");
        }
    }
    for spec in [
        "amp-auto",
        "kiro-auto",
        "hermes-auto",
        "droid-yolo",
        "amp-yolo",
    ] {
        for suffix in ["", "-fast"] {
            assert!(
                parse_layout_spec(&format!("{spec}{suffix}"), &no_profiles(), &no_commands())
                    .is_err(),
                "{spec}{suffix}"
            );
        }
    }
}

#[test]
fn alias_words_read_their_kinds_base_as_the_kind_spelling_does() {
    let pairs = [("agy", "antigravity"), ("mcode", "minimax")];
    let profiles = toml_defs_with_bases(
        &[
            (
                "antigravity",
                "---\ndescription: Antigravity base\ntools: [Read]\n---\nAntigravity.\n",
            ),
            ("minimax", "---\ndescription: MiniMax base\n---\nMiniMax.\n"),
        ],
        "[profiles.worker-agy]\ndescription = 'Agy worker'\nagent = 'agy'\n\
         [profiles.worker-mcode]\ndescription = 'Mcode worker'\nagent = 'mcode'\n",
    );
    let cell = |raw: &str| {
        parse_layout_spec(raw, &profiles, &no_commands())
            .ok()
            .and_then(|spec| spec.agent_cells().next().cloned())
            .map(|cell| {
                (
                    cell.kind.to_string(),
                    cell.launch.profile,
                    cell.launch.mode,
                    cell.args,
                )
            })
    };
    for (alias, kind) in pairs {
        for suffix in ["", "-yolo", "-fast", "-yolo-fast"] {
            let launched = cell(&format!("{alias}{suffix}"));
            assert_eq!(
                launched,
                cell(&format!("{kind}{suffix}")),
                "{alias}{suffix}"
            );
        }
        assert_eq!(cell(alias).and_then(|cell| cell.1).as_deref(), Some(kind));
        let chained = resolve_profile(&format!("worker-{alias}"), &profiles).unwrap();
        assert_eq!(chained.layers, [format!("worker-{alias}"), kind.to_owned()]);
    }
}

#[test]
fn fast_suffix_cells_stamp_fast_on_their_base() {
    let fast_args = |kind: &str| {
        crate::agents::find_definition(kind)
            .expect("kind")
            .spec()
            .launch
            .fast_args(true)
            .expect("fast switch")
    };
    let claude = agent_cell("claude-fast", &no_profiles(), &no_commands());
    assert_eq!(claude.kind.as_str(), "claude");
    assert_eq!(claude.launch.fast, Some(true));
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
        (
            "calm",
            Profile {
                fast: Some(crate::agents::Toggle::Off),
                ..profile("codex")
            },
        ),
    ]);
    let reviewer = agent_cell("reviewer-fast", &profiles, &no_commands());
    assert_eq!(reviewer.kind.as_str(), "codex");
    assert_eq!(reviewer.launch.profile.as_deref(), Some("reviewer"));
    assert_eq!(reviewer.launch.effort.as_deref(), Some("high"));
    assert_eq!(reviewer.launch.fast, Some(true));
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
    assert_eq!(yolo.launch.fast, Some(true));
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
    assert_eq!(exact.launch.fast, None);

    // The suffix forces on over a profile that writes off.
    let calm = agent_cell("calm-fast", &profiles, &no_commands());
    assert_eq!(calm.launch.fast, Some(true));
    assert!(
        calm.args
            .windows(4)
            .any(|window| window == fast_args("codex").as_slice()),
        "{:?}",
        calm.args
    );
    assert!(
        !calm.args.iter().any(|arg| arg == "service_tier=default"),
        "{:?}",
        calm.args
    );

    let cursor = agent_cell("cursor-fast", &no_profiles(), &no_commands());
    assert_eq!(cursor.kind, "cursor");
    assert_eq!(
        cursor.launch.fast,
        Some(true),
        "the finalizer clears it and warns"
    );
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
                smart_compact: None,
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
                smart_compact: None,
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

    let empty_profiles = no_profiles();
    let preflight = |candidate: &Team, base: Option<&str>, default| {
        let base_override = resolve_agent_override(base, &empty_profiles, None)?;
        let prepared = prepare_team(
            "forge",
            candidate,
            &empty_profiles,
            &no_commands(),
            CellBase::under(base_override.as_ref()),
            None,
        )?;
        validate_flip_compact("forge", &prepared, default)
    };
    let mut candidate = team(vec![role("coder", "claude")]);
    candidate.roles[0].flip_compact = Some(FlipCompact::Threshold(AutoCompact::Tokens(180_000)));
    preflight(&candidate, None, None).expect("Claude supports compact");
    assert!(matches!(
        preflight(&candidate, Some("amp"), None),
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
    preflight(&candidate, Some("claude"), None).expect("override supports compact");
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
    let profiles = md_defs_raw(&[
        (
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude.\n",
        ),
        (
            "planner",
            "---\ndescription: Planner\nagent: claude\n---\nPlanner craft.\n",
        ),
    ]);
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
fn team_prompt_without_stages_is_rejected_at_load() {
    let mut candidate = team(vec![role("worker", "claude")]);
    candidate.consensus_file = Some("/prompts/consensus.md".into());
    assert!(matches!(
        validate_config(
            &no_profiles(),
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("crew".into(), candidate)])),
        ),
        Err(LayoutErr::TeamPromptWithoutStages { team }) if team == "crew"
    ));
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
                "team `crew` sets consensus-file, consensus, or \
                 append-system-prompt-files but role `worker` \
                 (claude) has no base system prompt to compose them onto; add system-prompt-file \
                 to role worker or its profile, or write {} with a nonempty body as the claude \
                 kind base (run `rimz agents validate` if it did not load)",
                crate::disk::paths::agents_home()
                    .join("agents/claude.md")
                    .display()
            ),
            "team `crew` sets consensus-file, consensus, or \
             append-system-prompt-files but role `worker` \
             (amp) has no base system prompt to compose them onto; add system-prompt-file to role \
             worker or its profile"
                .into(),
        ]
    );
}

#[test]
fn staged_team_roles_with_a_base_prompt_carry_the_team_layer() {
    let profiles = md_defs_raw(&[
        (
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude.\n",
        ),
        (
            "planner",
            "---\ndescription: Planner\nagent: claude\n---\nPlanner craft.\n",
        ),
    ]);
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
    let profiles = toml_defs(
        "[profiles.searcher]\ndescription = 'Searcher'\nagent = 'claude'\nempty-dir = true\n\
         [profiles.local]\ndescription = 'Local'\nagent = 'searcher'\nempty-dir = false\n",
    );
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
        consensus: Vec::new(),
        append_system_prompt_files: Vec::new(),
        stages: Vec::new(),
    };
    validate(layout_only("claude", "claude,codex")).expect("unique layout leader");
    // A cell counts by its profile, else its kind, so `claude-fast` is a `claude` cell.
    validate(layout_only("claude", "claude-fast,codex")).expect("variant cell leads");
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

    let mut leaderless = team(vec![role("planner", "planner"), role("coder", "coder")]);
    leaderless.leader = None;
    let roleless = |layout: &str| Team {
        leader: None,
        ..layout_only("claude", layout)
    };
    let offered = |cells: &str| {
        format!(
            "team `forge` must declare a `leader`; add `leader = \"<profile>\"` naming the \
             profile or kind that only one of its layout cells carries: {cells}"
        )
    };
    assert_eq!(
        [
            leaderless,
            roleless("claude,codex"),
            roleless("claude,claude-fast,codex"),
            roleless("claude,claude"),
        ]
        .map(|team| validate(team).map_err(|error| error.to_string())),
        [
            Err(
                "team `forge` must declare a `leader`; add `leader = \"<role>\"` naming one of \
                 its roles: planner, coder"
                    .to_owned()
            ),
            Err(offered("claude, codex")),
            Err(offered("codex")),
            Err(
                "team `forge` must declare a `leader`; no cell of its layout claude,claude has \
                 a profile or kind of its own, so give one cell a profile no other cell carries \
                 and add `leader = \"<profile>\"` naming it"
                    .to_owned()
            ),
        ]
    );
}

/// `peer`'s leader is the cell written `claude`, whichever kind `--agent` or `--tier` routes
/// each cell to.
#[test]
fn the_built_in_peer_team_is_led_by_its_claude_cell_under_any_selection() {
    let machine = crate::config::MachineConfig::default();
    let selection = || Selection::new(&machine.tiers);
    let principal = Some(crate::config::tiers::ModelTier::Principal);
    for (label, selection) in [
        ("bare", selection()),
        ("--agent codex", selection().agent(Some("codex"))),
        ("--agent claude", selection().agent(Some("claude"))),
        ("--tier principal", selection().tier(principal)),
    ] {
        let layout = resolve_spec_in_scope(
            Some("peer"),
            &no_profiles(),
            &no_profiles(),
            None,
            &no_commands(),
            &machine.agents.teams,
            &selection,
        );
        assert!(layout.is_ok(), "{label}: {layout:?}");
        let layout = layout.unwrap();
        assert_eq!(prompt_leader(&layout), Ok(0), "{label}");
    }
}

#[test]
fn prompt_leader_selects_or_rejects_targets() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let commands = commands([("vim", "vim")]);
    let mut configured = team_with_layout(
        vec![role("planner", "planner"), role("coder", "coder")],
        "coder,planner",
    );
    let leader = |configured: &Team| {
        let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), configured.clone())]));
        let layout =
            resolve_spec(Some("forge"), &profiles, &commands, &teams).expect("reordered team");
        prompt_leader(&layout)
    };
    assert_eq!(leader(&configured), Ok(1));
    configured.leader = Some("coder".to_owned());
    assert_eq!(leader(&configured), Ok(0));

    let command_first =
        parse_layout_spec("vim,codex+term", &profiles, &commands).expect("one agent");
    assert_eq!(prompt_leader(&command_first), Ok(0));
    let unique =
        parse_layout_spec("claude,codex", &no_profiles(), &no_commands()).expect("unique first");
    assert_eq!(prompt_leader(&unique), Ok(0));
    let inline = parse_layout_spec("claude:lead,claude", &no_profiles(), &no_commands())
        .expect("inline leader");
    assert_eq!(prompt_leader(&inline), Ok(0));

    let ambiguous =
        parse_layout_spec("claude,claude", &no_profiles(), &no_commands()).expect("duplicate");
    assert_eq!(
        prompt_leader(&ambiguous),
        Err(LayoutErr::AmbiguousPromptLeader {
            token: "claude".to_owned(),
        })
    );
    assert_eq!(
        prompt_leader(
            &parse_layout_spec("term", &no_profiles(), &no_commands()).expect("terminal")
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

#[test]
fn antigravity_layouts_render_catalog_effort_and_the_max_fallback() {
    let args = |model: &str, effort: Option<&str>| {
        let profiles = profiles([(
            "worker",
            Profile {
                kind_defaults: true,
                model: Some(model.to_owned()),
                effort: effort.map(str::to_owned),
                tools: Some(crate::agents::ToolSet::parse(&["Read".into()]).unwrap()),
                ..profile("antigravity")
            },
        )]);
        agent_cell("worker", &profiles, &CommandsConfig::default()).args
    };
    assert_eq!(
        [
            args("hydrogen", None),
            args("gemini-4-carbon", None),
            args("gemini-x", None),
            args("gemini-4-hydrogen-high", None),
            args("hydrogen", Some("low")),
            args("hydrogen", Some("max")),
            args("carbon", Some("max")),
        ],
        [
            vec!["--model", "hydrogen", "--effort", "high"],
            vec!["--model", "gemini-4-carbon", "--effort", "high"],
            vec!["--model", "gemini-x"],
            vec!["--model", "gemini-4-hydrogen-high"],
            vec!["--model", "hydrogen", "--effort", "low"],
            vec!["--model", "hydrogen", "--effort", "xhigh"],
            vec!["--model", "carbon", "--effort", "high"],
        ]
    );
    for id in [
        "gemini-pro-orchestrator-neon",
        "gemini-pro-orchestrator",
        "gemini-pro-orchestrator-argon",
    ] {
        // The final model settles the effort, so the profile render keeps it.
        assert_eq!(args(id, None), ["--model", id], "{id}");
        assert_eq!(
            args(id, Some("high")),
            ["--model", id, "--effort", "high"],
            "{id}"
        );
        assert_eq!(
            args(id, Some("max")),
            ["--model", id, "--effort", "xhigh"],
            "{id}"
        );
        let bogus = profiles([(
            "worker",
            Profile {
                model: Some(id.to_owned()),
                effort: Some("bogus".to_owned()),
                ..profile("antigravity")
            },
        )]);
        assert!(profile_cell("worker", &bogus).is_err(), "{id}");
    }
}

#[test]
fn kind_base_tuning_fills_below_written_values_and_follows_the_final_kind() {
    use crate::agents::Toggle::{Auto, Off, On};

    let profiles = toml_defs_with_bases(
        &[
            (
                "claude",
                "---\ndescription: Claude base\ntools: [Read]\nfast: on\ncode-mode: on\n\
                 effort: low\nauto-compact: 300k\n---\nClaude.\n",
            ),
            (
                "codex",
                "---\ndescription: Codex base\ntools: [Read]\nfast: off\neffort: medium\n---\nCodex.\n",
            ),
        ],
        "[profiles.markdown]\ndescription = 'Markdown'\nagent = 'claude'\n\
         [profiles.toml]\ndescription = 'Toml'\nagent = 'claude'\n\
         [profiles.cleared]\ndescription = 'Cleared'\nagent = 'claude'\nfast = 'auto'\ncode-mode = 'auto'\n\
         [profiles.heir]\ndescription = 'Heir'\nagent = 'cleared'\n\
         [profiles.eager]\ndescription = 'Eager'\nagent = 'claude'\nfast = 'on'\n\
         [profiles.eager-heir]\ndescription = 'Eager heir'\nagent = 'eager'\nfast = 'auto'\n\
         [profiles.written]\ndescription = 'Written'\nagent = 'claude'\nfast = 'off'\ncode-mode = 'off'\n\
         effort = 'high'\nauto-compact = '200k'\n",
    );
    let tuning = |resolved: ResolvedProfile| {
        (
            resolved.kind.to_string(),
            resolved.fast,
            resolved.code_mode,
            resolved.launch.effort.clone(),
            resolved.effective_auto_compact(),
        )
    };
    let based = (
        "claude".to_owned(),
        Some(On),
        Some(On),
        Some("low".to_owned()),
        Some("300000".to_owned()),
    );
    let codex = (
        "codex".to_owned(),
        Some(Off),
        None,
        Some("medium".to_owned()),
        None,
    );
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (name, agent, want) in [
        ("claude", None, based.clone()),
        ("markdown", None, based.clone()),
        ("toml", None, based.clone()),
        (
            "cleared",
            None,
            (
                "claude".to_owned(),
                Some(Auto),
                Some(Auto),
                Some("low".to_owned()),
                Some("300000".to_owned()),
            ),
        ),
        (
            "heir",
            None,
            (
                "claude".to_owned(),
                Some(Auto),
                Some(Auto),
                Some("low".to_owned()),
                Some("300000".to_owned()),
            ),
        ),
        (
            "eager-heir",
            None,
            (
                "claude".to_owned(),
                Some(Auto),
                Some(On),
                Some("low".to_owned()),
                Some("300000".to_owned()),
            ),
        ),
        (
            "written",
            None,
            (
                "claude".to_owned(),
                Some(Off),
                Some(Off),
                Some("high".to_owned()),
                Some("200000".to_owned()),
            ),
        ),
        ("markdown", Some("codex"), codex.clone()),
        ("toml", Some("codex"), codex.clone()),
        (
            "written",
            Some("codex"),
            (
                "codex".to_owned(),
                Some(Off),
                Some(Off),
                Some("high".to_owned()),
                Some("200000".to_owned()),
            ),
        ),
        (
            "eager",
            Some("codex"),
            (
                "codex".to_owned(),
                Some(On),
                None,
                Some("medium".to_owned()),
                None,
            ),
        ),
        ("markdown", Some("claude"), based.clone()),
    ] {
        actual.push((
            name,
            agent,
            resolve_profile_under(name, agent, &profiles, None)
                .map(tuning)
                .map_err(|error| error.to_string()),
        ));
        expected.push((name, agent, Ok(want)));
    }
    assert_eq!(actual, expected);

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (fast, effort, want_fast, want_effort) in [
        (None, None, Some(true), "low"),
        (Some(Off), Some("high"), Some(false), "high"),
        (Some(Auto), None, None, "low"),
    ] {
        let binding = RoleBinding {
            fast,
            effort: effort.map(str::to_owned),
            ..role("coder", "toml")
        };
        let configured = team(vec![binding.clone()]);
        let seat = prepare_team(
            "forge",
            &configured,
            &profiles,
            &no_commands(),
            CellBase::none(),
            None,
        )
        .map(|team| {
            let resolved = &team.roles[0].resolved;
            (
                resolved.fast.and_then(crate::agents::Toggle::enabled),
                resolved.code_mode.and_then(crate::agents::Toggle::enabled),
                resolved.launch.effort.clone(),
            )
        })
        .map_err(|error| error.to_string());
        let replayed =
            profile_cell_with_base_override("toml", &profiles, CellBase::none(), Some(&binding))
                .map(|cell| (cell.launch.fast, cell.launch.code_mode, cell.launch.effort))
                .map_err(|error| error.to_string());
        actual.push((seat, replayed));
        let launch = Ok((want_fast, Some(true), Some(want_effort.to_owned())));
        expected.push((launch.clone(), launch));
    }
    assert_eq!(actual, expected);

    let cell = agent_cell("toml", &profiles, &no_commands());
    assert_eq!(
        (
            cell.launch.fast,
            cell.launch.code_mode,
            cell.auto_compact.as_deref()
        ),
        (Some(true), Some(true), Some("300000"))
    );
    let suffixed = agent_cell("codex-fast", &profiles, &no_commands());
    assert_eq!(
        (suffixed.launch.fast, suffixed.launch.effort.as_deref()),
        (Some(true), Some("medium"))
    );
}

#[test]
fn markdown_kind_base_fields_reach_bare_kind_launches() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("agents")).unwrap();
    std::fs::write(
        home.path().join("agents/claude.md"),
        "---\ndescription: Base\ntools: [Bash, Skill]\nallowed-tools: [Read]\nskills: [work]\n\
         mcps: [github]\n---\nBase.",
    )
    .unwrap();
    let loaded = crate::config::definitions::load(
        home.path(),
        crate::config::definitions::SkillCheck::Skip,
        &no_commands(),
        &home.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        None,
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    for word in ["claude", "claude-yolo", "claude-fast"] {
        let cell = agent_cell(word, &loaded.agent_profiles, &no_commands());
        assert_eq!(
            (
                cell.skills
                    .iter()
                    .flatten()
                    .map(crate::config::SkillName::as_str)
                    .collect::<Vec<_>>(),
                cell.mcps
                    .iter()
                    .flatten()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                cell.allowed_tools.as_ref().map(Vec::len),
            ),
            (vec!["work"], vec!["github".to_owned()], Some(1)),
            "{word}"
        );
    }
}

#[test]
fn a_grant_follows_tools_through_a_chain_and_an_agent_switch_and_refuses_rimz_user_lists() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::create_dir_all(home.path().join("skills/rimz")).unwrap();
    std::fs::create_dir_all(home.path().join("skills/work")).unwrap();
    std::fs::create_dir_all(home.path().join("skills/rimz-reflect")).unwrap();
    for skill in ["rimz", "work", "rimz-reflect"] {
        std::fs::write(
            home.path().join(format!("skills/{skill}/SKILL.md")),
            format!("---\nname: {skill}\ndescription: {skill}\n---\nBody.\n"),
        )
        .unwrap();
    }
    for (name, body) in [
        (
            "claude",
            "---\ndescription: Claude base\ncapabilities: [bash, rimz-lsp]\n---\nClaude.\n",
        ),
        (
            "codex",
            "---\ndescription: Codex base\ncapabilities: [bash, rimz-watch]\n---\nCodex.\n",
        ),
        (
            "switched",
            "---\ndescription: Switched\nagent: codex\ncapabilities: [bash, rimz-watch]\n---\n",
        ),
    ] {
        std::fs::write(home.path().join(format!("agents/{name}.md")), body).unwrap();
    }
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.child]\ndescription = 'Child'\nagent = 'claude'\n\
         [profiles.own]\ndescription = 'Own'\nagent = 'claude'\nskills = ['rimz']\n\
         [profiles.served]\ndescription = 'Served'\nagent = 'claude'\nmcps = ['rimz']\n\
         [profiles.listed]\ndescription = 'Listed'\nagent = 'claude'\nskills = ['work', 'rimz-watch']\n\
         [profiles.reflects]\ndescription = 'Reflects'\nagent = 'claude'\nskills = ['work', 'rimz-reflect']\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), home.path()).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&home.path().join("config.toml"), "", home.path())
            .unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), home.path()).unwrap();
    let grant = |resolved: ResolvedProfile| {
        resolved
            .tools
            .as_ref()
            .and_then(crate::agents::ToolSet::rimz)
            .map(|grant| grant.iter().map(ToString::to_string).collect::<Vec<_>>())
    };
    let lsp = Some(vec!["rimz-lsp".to_owned()]);
    assert_eq!(
        grant(resolve_profile("child", &launch.profiles).unwrap()),
        lsp
    );
    assert_eq!(
        grant(resolve_profile_under("switched", Some("claude"), &launch.profiles, None).unwrap()),
        Some(vec!["rimz-watch".to_owned()])
    );
    // A skill named exactly `rimz` is the user's own, as is a `rimz-*` skill alone; in `mcps`,
    // `rimz` is RimZ's server.
    assert_eq!(
        grant(resolve_profile("own", &launch.profiles).unwrap()),
        lsp
    );
    assert_eq!(
        grant(resolve_profile("reflects", &launch.profiles).unwrap()),
        lsp
    );
    let served_err = launch
        .definition_failure_for("served")
        .expect("served refuses at build time");
    assert!(
        served_err.contains("[profiles.served]") && served_err.contains("capabilities"),
        "{served_err}"
    );
    let listed_err = launch
        .definition_failure_for("listed")
        .expect("listed refuses at build time");
    assert!(
        listed_err.contains("[profiles.listed]") && listed_err.contains("rimz-watch"),
        "{listed_err}"
    );
}

#[test]
fn team_seat_and_toml_profiles_follow_one_chain_and_agree_between_fresh_and_replay() {
    let root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("teams")).unwrap();
    for (rel, content) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\ntools: [Read]\n---\nCodex house.",
        ),
        (
            "agents/parent.md",
            "---\ndescription: Parent\nagent: claude\nmodel: opus\neffort: medium\n\
             tools: [Bash, Read]\n---\nParent craft.",
        ),
        (
            "agents/child.md",
            "---\ndescription: Child\nagent: parent\neffort: low\n---\nChild craft.",
        ),
        (
            "agents/astra.md",
            "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: high\n\
             fast: on\ntools: [Bash, Read]\n---\nAstra craft.",
        ),
        (
            "teams/forge.md",
            "---\ndescription: Forge\nleader: lead\nstages: [Plan, Implement, Review]\nroles:\n\
             - agent: parent\n  role: lead\n  owns: [Plan]\n  model: sonnet\n  effort: low\n\
             - agent: parent\n  role: builder\n  owns: [Implement]\n\
             - agent: parent\n  role: judge\n  owns: [Review]\n---\nPipeline.",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    assert!(machine.notices.definition_errors.is_empty());
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.toml_l]\ndescription = 'TOML L'\nagent = 'claude'\nmodel = 'opus'\n\
         effort = 'low'\nargs = '--claude-only'\n\
         [profiles.toml_x_base]\ndescription = 'TOML X base'\nagent = 'codex'\n\
         model = 'sol'\neffort = 'high'\n\
         [profiles.toml_x]\ndescription = 'TOML X'\nagent = 'toml_x_base'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, project.path(), root.path()).unwrap();
    assert!(
        launch.project_definition_errors().is_empty(),
        "{:?}",
        launch.project_definition_errors()
    );
    let profiles = launch.profiles;
    let forge = &launch.teams.0["forge"];
    let lead_binding = &forge.roles[0];
    let fresh_layout = resolve_spec_with_agent_override(
        Some("forge"),
        &profiles,
        &profiles,
        &CommandsConfig::default(),
        &launch.teams,
        Some("astra"),
    )
    .expect("fresh forge --agent astra");
    let fresh_lead = fresh_layout
        .agent_cells()
        .find(|cell| cell.launch.role.as_deref() == Some("lead"))
        .unwrap();
    let base_astra = resolve_agent_override(Some("astra"), &profiles, None).unwrap();
    let replay_lead = profile_cell_with_base_override(
        &lead_binding.profile,
        &profiles,
        CellBase::under(base_astra.as_ref()),
        Some(lead_binding),
    )
    .expect("replay forge.lead --agent astra");
    assert_eq!(fresh_lead.kind.as_str(), "codex");
    assert_eq!(fresh_lead.launch.model.as_deref(), Some("astra"));
    assert_eq!(fresh_lead.launch.effort.as_deref(), Some("low"));
    assert_eq!(fresh_lead.kind, replay_lead.kind);
    assert_eq!(fresh_lead.launch.model, replay_lead.launch.model);
    assert_eq!(fresh_lead.launch.effort, replay_lead.launch.effort);
    assert_eq!(fresh_lead.launch.fast, replay_lead.launch.fast);
    assert!(
        fresh_lead
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/codex.md")
    );

    // TOML RoleBinding writing model + effort + fast + args under X (astra): X replaces model,
    // role keeps effort + fast, raw args drop across kind switch, and fresh equals replay.
    let toml_binding = RoleBinding {
        model: Some("opus".into()),
        effort: Some("low".into()),
        fast: Some(crate::agents::Toggle::Off),
        args: Some("--claude-only".into()),
        ..role("coder", "toml_l")
    };
    let toml_team = team(vec![toml_binding.clone()]);
    let prepared = prepare_team(
        "toml_t",
        &toml_team,
        &profiles,
        &no_commands(),
        CellBase::under(base_astra.as_ref()),
        None,
    )
    .expect("fresh toml team");
    let fresh_toml = &prepared.roles[0].resolved;
    let replay_toml = profile_cell_with_base_override(
        "toml_l",
        &profiles,
        CellBase::under(base_astra.as_ref()),
        Some(&toml_binding),
    )
    .expect("replay toml team");
    assert_eq!(fresh_toml.kind.as_str(), "codex");
    assert_eq!(fresh_toml.launch.model.as_deref(), Some("astra"));
    assert_eq!(fresh_toml.launch.effort.as_deref(), Some("low"));
    assert_eq!(fresh_toml.fast, Some(crate::agents::Toggle::Off));
    assert_eq!(fresh_toml.args, None);
    assert_eq!(replay_toml.kind.as_str(), "codex");
    assert_eq!(replay_toml.launch.model.as_deref(), Some("astra"));
    assert_eq!(replay_toml.launch.effort.as_deref(), Some("low"));
    assert_eq!(replay_toml.launch.fast, Some(false));
    assert!(!replay_toml.args.iter().any(|arg| arg == "--claude-only"));

    // TOML L under Markdown X: takes X's model, keeps L's effort, drops raw args across kind switch.
    let toml_l_cell = profile_cell_with_base_override(
        "toml_l",
        &profiles,
        CellBase::under(base_astra.as_ref()),
        None,
    )
    .expect("toml_l --agent astra");
    assert_eq!(toml_l_cell.kind.as_str(), "codex");
    assert_eq!(toml_l_cell.launch.model.as_deref(), Some("astra"));
    assert_eq!(toml_l_cell.launch.effort.as_deref(), Some("low"));
    assert!(!toml_l_cell.args.iter().any(|arg| arg == "--claude-only"));
    assert!(
        toml_l_cell
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/codex.md")
    );

    // Multi-hop TOML X under Markdown L: X's resolved runtime and written model replace L's.
    let base_toml_x = resolve_agent_override(Some("toml_x"), &profiles, None).unwrap();
    let multi_toml = profile_cell_with_base_override(
        "child",
        &profiles,
        CellBase::under(base_toml_x.as_ref()),
        None,
    )
    .expect("child --agent toml_x");
    assert_eq!(multi_toml.kind.as_str(), "codex");
    assert_eq!(multi_toml.launch.model.as_deref(), Some("sol"));
    assert_eq!(multi_toml.launch.effort.as_deref(), Some("low"));

    // Idempotence: X equal to L or an ancestor of L changes nothing and duplicates no crafts.
    let plain_child = profile_cell("child", &profiles).unwrap();
    for ancestor in ["child", "parent"] {
        let base = resolve_agent_override(Some(ancestor), &profiles, None).unwrap();
        let same = profile_cell_with_base_override(
            "child",
            &profiles,
            CellBase::under(base.as_ref()),
            None,
        )
        .unwrap();
        assert_eq!(same.kind, plain_child.kind);
        assert_eq!(same.launch.model, plain_child.launch.model);
        assert_eq!(same.launch.effort, plain_child.launch.effort);
        assert_eq!(
            same.append_system_prompt_files, plain_child.append_system_prompt_files,
            "ancestor {ancestor} must not duplicate crafts"
        );
    }
}

#[test]
fn auto_compact_resolves_per_enforcer_and_remaps_across_agent_switches() {
    use crate::config::AutoCompactDefault;

    let root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("agents")).unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    for (rel, content) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\ntools: [Bash]\n---\nClaude.",
        ),
        (
            "agents/antigravity.md",
            "---\ndescription: Agy base\ntools: [Bash]\n---\nAgy.",
        ),
        ("agents/pi.md", "---\ndescription: Pi base\n---\nPi."),
        (
            "agents/md_claude.md",
            "---\ndescription: Claude worker\nagent: claude\ntools: [Bash]\n---\nWork.",
        ),
        (
            "agents/md_agy.md",
            "---\ndescription: Agy worker\nagent: antigravity\ntools: [Bash]\n---\nWork.",
        ),
        (
            "agents/md_pi.md",
            "---\ndescription: Pi worker\nagent: pi\ntools: [Bash]\n---\nWork.",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[profiles.toml_claude]\ndescription = 'TOML Claude'\nagent = 'claude'\n\
         [profiles.toml_agy]\ndescription = 'TOML Agy'\nagent = 'antigravity'\n\
         [profiles.toml_amp]\ndescription = 'TOML Amp'\nagent = 'amp'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), root.path()).unwrap();

    let cases = [
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_claude",
            None,
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_agy",
            None,
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_pi",
            None,
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_claude",
            Some("antigravity"),
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_agy",
            Some("claude"),
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_pi",
            Some("antigravity"),
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "md_agy",
            Some("pi"),
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "toml_claude",
            Some("antigravity"),
            Some("272000"),
        ),
        (
            Some(AutoCompactDefault::BuiltIn),
            "toml_agy",
            Some("claude"),
            Some("272000"),
        ),
        // A switch onto a kind with no enforcer drops the injected default.
        (
            Some(AutoCompactDefault::BuiltIn),
            "toml_agy",
            Some("amp"),
            None,
        ),
        (Some(AutoCompactDefault::BuiltIn), "toml_amp", None, None),
        (
            Some(AutoCompactDefault::Tokens(300_000)),
            "md_claude",
            None,
            Some("300000"),
        ),
        (
            Some(AutoCompactDefault::Tokens(300_000)),
            "md_agy",
            None,
            Some("300000"),
        ),
        (
            Some(AutoCompactDefault::Tokens(300_000)),
            "md_pi",
            None,
            Some("300000"),
        ),
        (None, "md_claude", None, None),
        (None, "md_agy", None, None),
        // Kind bases and bare launches get no machine default on any kind.
        (Some(AutoCompactDefault::BuiltIn), "claude", None, None),
        (Some(AutoCompactDefault::BuiltIn), "antigravity", None, None),
        (Some(AutoCompactDefault::BuiltIn), "pi", None, None),
    ];

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (default, target, agent_override, want) in cases {
        let machine_toml = match default {
            Some(AutoCompactDefault::BuiltIn) => String::new(),
            Some(AutoCompactDefault::Tokens(n)) => format!("[harness]\nauto_compact = '{n}'\n"),
            None => "[harness]\nauto_compact = 'off'\n".to_owned(),
        };
        let cfg_path = root.path().join("config.toml");
        std::fs::write(&cfg_path, &machine_toml).unwrap();
        let machine =
            crate::config::MachineConfig::parse_text(&cfg_path, &machine_toml, root.path())
                .unwrap();
        assert!(
            machine.notices.definition_errors.is_empty(),
            "{:?}",
            machine.notices.definition_errors
        );
        let launch =
            crate::config::effective::load_with_roots(&machine, project.path(), root.path())
                .unwrap();
        assert!(
            launch.project_definition_errors().is_empty(),
            "{:?}",
            launch.project_definition_errors()
        );
        let profiles = launch.profiles;
        let base = resolve_agent_override(agent_override, &profiles, None).unwrap();
        let cell = profile_cell_with_base_override(
            target,
            &profiles,
            CellBase::under(base.as_ref()),
            None,
        )
        .unwrap();
        actual.push((
            default,
            target,
            agent_override,
            cell.auto_compact,
            cell.tuning.auto_compact,
        ));
        expected.push((
            default,
            target,
            agent_override,
            want.map(str::to_owned),
            want.map(|_| TuningSource::MachineDefault),
        ));
    }
    assert_eq!(actual, expected);

    // Bare launch gets no machine default; a kind base's own auto-compact still applies.
    assert_eq!(
        resolve_spec_with_agent_override(
            Some("antigravity"),
            &ProfilesConfig::default(),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            None,
        )
        .unwrap()
        .agent_cells()
        .next()
        .unwrap()
        .auto_compact,
        None
    );
    std::fs::write(
        root.path().join("agents/antigravity.md"),
        "---\ndescription: Agy base\nauto-compact: 250k\n---\nAgy.",
    )
    .unwrap();
    let loaded = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &root.path().join("config.toml"),
        &crate::config::tiers::TierConfig::default(),
        Some(AutoCompactDefault::BuiltIn),
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let base_cell = profile_cell("antigravity", &loaded.agent_profiles).unwrap();
    assert_eq!(base_cell.auto_compact.as_deref(), Some("250000"));
    assert_eq!(
        base_cell.tuning.auto_compact,
        Some(TuningSource::KindBase(AgentKind::new_unchecked(
            "antigravity"
        )))
    );
    let worker_cell = profile_cell("md_agy", &loaded.agent_profiles).unwrap();
    assert_eq!(worker_cell.auto_compact.as_deref(), Some("250000"));
    assert_eq!(
        worker_cell.tuning.auto_compact,
        Some(TuningSource::KindBase(AgentKind::new_unchecked(
            "antigravity"
        )))
    );
}

/// A kind-base name that runs on another kind ends its explained chain in that kind's base, as
/// its launch layers do.
#[test]
fn explain_chain_of_a_kind_base_name_ends_in_the_kind_it_runs_on() {
    let profiles = md_defs_raw(&[
        (
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\n---\nClaude.\n",
        ),
        (
            "codex",
            "---\ndescription: Codex base\ntools: [Read]\n---\nCodex.\n",
        ),
    ]);
    assert_eq!(
        [
            explain_profile_chain("claude", None, "codex", false, &profiles, None),
            explain_profile_chain("claude", None, "claude", false, &profiles, None),
        ],
        [vec!["claude", "codex"], vec!["claude"]]
    );
}

#[test]
fn explain_names_the_built_in_kind_base_at_the_chain_root_and_marks_its_tuning() {
    let worker = (
        "worker",
        "---\ndescription: Worker\nagent: claude\n---\nCraft.\n",
    );
    let built_in = md_defs_raw(&[worker]);
    let home = md_defs_raw(&[
        (
            "claude",
            "---\ndescription: Claude base\ntools: [Read]\ncode-mode: on\n---\nClaude.\n",
        ),
        worker,
    ]);
    let explain = |profiles: &ProfilesConfig| {
        (
            explain_profile_chain("worker", None, "claude", false, profiles, None),
            resolve_profile("worker", profiles)
                .unwrap()
                .sources
                .code_mode,
        )
    };
    let claude = || AgentKind::new_unchecked("claude");
    assert_eq!(
        [explain(&built_in), explain(&home)],
        [
            (
                vec!["worker".to_owned(), "built-in kind base claude".to_owned()],
                Some(TuningSource::BuiltInKindBase(claude())),
            ),
            (
                vec!["worker".to_owned(), "claude".to_owned()],
                Some(TuningSource::KindBase(claude())),
            ),
        ]
    );
    assert_eq!(
        TuningSource::BuiltInKindBase(claude()).to_string(),
        "built-in kind base claude"
    );
}
