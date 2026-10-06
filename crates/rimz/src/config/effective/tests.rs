use super::*;
use crate::agents::PermissionMode;
use crate::config::agents::AgentsConfig;
use crate::config::{CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig};
use std::collections::BTreeMap;
use tempfile::tempdir;

#[test]
fn definition_routing_overlay_keeps_load_choice_and_provider_fields() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("agents/worker.md"),
        "---\ndescription: Worker\ntier: senior\ntools: [Bash]\n---\nCraft.",
    )
    .unwrap();
    for (name, runtime) in [
        ("principal", "tier: principal"),
        ("exact", "agent: claude\nmodel: claude-future"),
        ("other", "tier: principal"),
        ("codex-worker", "agent: codex\ntier: junior"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Worker\n{runtime}\ntools: [Bash]\n---\nCraft."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &CommandsConfig::default(),
        &machine.tiers,
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    let project = tempdir().unwrap();
    let mut launch = load_with_roots(&machine, project.path(), root.path()).unwrap();
    launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("codex-worker"),
            Some(super::super::tiers::ModelTier::Senior),
            None,
            None,
            |_, _| None,
        )
        .unwrap();
    assert_eq!(
        launch.profiles.0["codex-worker"].model.as_deref(),
        Some("astra")
    );
    launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("worker"),
            None,
            None,
            Some("codex"),
            |_, _| None,
        )
        .unwrap();
    let selected = &launch.profiles.0["worker"];
    assert_eq!(selected.agent, "codex");
    assert_eq!(selected.model.as_deref(), Some("astra"));
    let resolved = crate::harness::spec::resolve_profile("worker", &launch.profiles).unwrap();
    assert!(
        resolved
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/codex.md")
    );
    assert_eq!(machine.agents.profiles.0["worker"].agent, "claude");
    let wire = serde_json::to_value(selected).unwrap();
    assert!(wire.get("model_tier").is_none());
    assert!(wire.get("renders").is_none());
    assert!(wire.get("tier_stamp").is_none());
    assert_eq!(
        launch.profiles.0["other"].model,
        machine.agents.profiles.0["other"].model
    );
    for name in ["principal", "exact", "claude"] {
        assert!(
            launch
                .route(
                    &machine.tiers,
                    ProfileScope::Agents,
                    Some(name),
                    Some(super::super::tiers::ModelTier::Intern),
                    None,
                    Some("codex"),
                    |_, _| None
                )
                .unwrap()
        );
        assert_eq!(launch.profiles.0[name].agent, "codex");
        assert_eq!(launch.profiles.0[name].model.as_deref(), Some("luna"));
        assert_eq!(
            launch.profiles.0["other"].model,
            machine.agents.profiles.0["other"].model
        );
    }
    launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("exact:author"),
            Some(super::super::tiers::ModelTier::Senior),
            None,
            Some("codex"),
            |_, _| None,
        )
        .unwrap();
    assert_eq!(launch.profiles.0["exact"].model.as_deref(), Some("astra"));
    launch.profiles.0.insert(
        "project".into(),
        toml::from_str("agent = 'claude'\nmodel = 'opus'").unwrap(),
    );
    let error = launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("project"),
            Some(super::super::tiers::ModelTier::Senior),
            None,
            None,
            |_, _| None,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("model tiers resolve in Markdown definitions")
    );
    for reason in [
        crate::agents::TierSkipReason::LoggedOut,
        crate::agents::TierSkipReason::Exhausted { until: None },
        crate::agents::TierSkipReason::DailyCap {
            spend_usd: 10.into(),
            cap_usd: 5.into(),
        },
    ] {
        let mut launch = load_with_roots(&machine, project.path(), root.path()).unwrap();
        launch
            .route(
                &machine.tiers,
                ProfileScope::Agents,
                Some("worker"),
                None,
                None,
                None,
                |kind, _| (kind == "claude").then(|| reason.clone()),
            )
            .unwrap();
        let cell = crate::harness::spec::resolve_profile("worker", &launch.profiles).unwrap();
        let wire = serde_json::to_value(&cell.launch).unwrap();
        assert_eq!(cell.kind.as_str(), "codex");
        assert_eq!(wire["tier"]["model"], "astra");
        assert_eq!(wire["tier"]["skipped"][0]["model"], "opus");
        assert_eq!(
            wire["tier"]["skipped"][0]["reason"],
            serde_json::to_value(reason).unwrap()["reason"]
        );
    }
    for routing in [
        super::super::tiers::Routing::Fallback,
        super::super::tiers::Routing::Off,
    ] {
        let mut table = machine.tiers.clone();
        table.routing = routing;
        let mut launch = load_with_roots(&machine, project.path(), root.path()).unwrap();
        launch.profiles.0.insert(
            "project".into(),
            toml::from_str("agent = 'claude'\nmodel = 'opus'").unwrap(),
        );
        launch
            .route(
                &table,
                ProfileScope::Agents,
                Some("worker"),
                None,
                None,
                None,
                |_, _| Some(crate::agents::TierSkipReason::LoggedOut),
            )
            .unwrap();
        let resolved = crate::harness::spec::resolve_profile("worker", &launch.profiles).unwrap();
        let wire = serde_json::to_value(&resolved.launch).unwrap();
        assert_eq!(wire["tier"]["model"], "opus");
        assert!(wire["tier"].get("skipped").is_none());
        let project = crate::harness::spec::resolve_profile("project", &launch.profiles).unwrap();
        assert_eq!(project.launch.model.as_deref(), Some("opus"));
        assert!(project.launch.tier.is_none());
    }
}

#[test]
fn team_cli_models_route_each_markdown_role() {
    let root = tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", ""),
        ("writer", "agent: claude\ntier: junior\ntools: [Bash]"),
        ("coder", "agent: codex\ntier: junior\ntools: [Bash]"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Role\n{fields}\n---\nWork."),
        )
        .unwrap();
    }
    let mut machine = MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &machine.tiers,
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    machine.agents.teams = toml::from_str("[forge]\nroles = [{role = 'writer', profile = 'writer'}, {role = 'coder', profile = 'coder'}]").unwrap();
    for (tier, model, spent) in [
        (None, Some("opus"), true),
        (Some(super::super::tiers::ModelTier::Senior), None, false),
    ] {
        let mut launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
        launch
            .route(
                &machine.tiers,
                ProfileScope::Agents,
                Some("forge"),
                tier,
                model,
                None,
                |kind, _| {
                    (spent && kind == "claude").then_some(crate::agents::TierSkipReason::LoggedOut)
                },
            )
            .unwrap();
        let mut layout = crate::harness::spec::resolve_team(
            "forge",
            &launch.teams,
            &launch.profiles,
            &machine.agents.commands,
        )
        .unwrap();
        crate::harness::plan::finalize_launch_layout(
            &mut layout,
            crate::harness::plan::LaunchFinalizeOptions {
                agent_base: None,
                permission_mode: None,
                isolation: None,
                preset: &crate::agents::LaunchPreset {
                    model: model.map(str::to_owned),
                    ..Default::default()
                },
                passthrough: &[],
                budget: None,
                max_turns: None,
            },
        )
        .unwrap();
        let cells: Vec<_> = layout.agent_cells().collect();
        for (index, cell) in cells.iter().enumerate() {
            let codex = spent || index == 1;
            assert_eq!(cell.kind.as_str(), if codex { "codex" } else { "claude" });
            assert_eq!(
                cell.launch.model.as_deref(),
                Some(if codex { "astra" } else { "opus" })
            );
            assert_eq!(
                cell.launch.tier.as_ref().unwrap().tier,
                super::super::tiers::ModelTier::Senior
            );
        }
    }
    machine
        .agents
        .profiles
        .0
        .insert("coder".into(), toml::from_str("agent = 'codex'").unwrap());
    let mut launch = load_with_roots(&machine, root.path(), root.path()).unwrap();
    let error = launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("forge"),
            Some(super::super::tiers::ModelTier::Senior),
            None,
            None,
            |_, _| None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("coder"), "{error}");
    assert!(
        error
            .to_string()
            .contains("--tier needs Markdown definitions"),
        "{error}"
    );
    launch
        .route(
            &machine.tiers,
            ProfileScope::Agents,
            Some("forge"),
            None,
            Some("opus"),
            None,
            |kind, _| (kind == "claude").then_some(crate::agents::TierSkipReason::LoggedOut),
        )
        .unwrap();
    let mut layout = crate::harness::spec::resolve_team(
        "forge",
        &launch.teams,
        &launch.profiles,
        &machine.agents.commands,
    )
    .unwrap();
    crate::harness::plan::finalize_launch_layout(
        &mut layout,
        crate::harness::plan::LaunchFinalizeOptions {
            agent_base: None,
            permission_mode: None,
            isolation: None,
            preset: &crate::agents::LaunchPreset {
                model: Some("opus".into()),
                ..Default::default()
            },
            passthrough: &[],
            budget: None,
            max_turns: None,
        },
    )
    .unwrap();
    let cells: Vec<_> = layout.agent_cells().collect();
    assert_eq!(cells[0].launch.model.as_deref(), Some("astra"));
    assert_eq!(cells[1].launch.model.as_deref(), Some("opus"));
    assert!(cells[1].launch.tier.is_none());
}

#[test]
fn trusted_repo_cannot_set_allowed_tools() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(
        &project,
        "[profiles.worker]\nagent = 'claude'\nallowed_tools = ['Bash(*)']\nallowed-tools = ['Bash(*)']",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
    let effective = load(&AgentsConfig::default(), project.path(), config.path()).unwrap();
    assert!(effective.profiles.0["worker"].allowed_tools.is_none());
}

#[test]
fn model_aliases_are_machine_only() {
    for trusted in [false, true] {
        let project = tempdir().unwrap();
        let config = tempdir().unwrap();
        write_project_config(&project, "[models.codex]\nsol = 'gpt-6.1-sol'");
        if trusted {
            crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
        }
        let result = load(&AgentsConfig::default(), project.path(), config.path());
        assert!(result.is_err(), "project model aliases must be refused");
        let error = result.err().unwrap().to_string();
        assert!(
            error.contains("project config cannot set [models]"),
            "{error}"
        );
        assert!(error.contains("move it to ~/.rimz/config.toml"), "{error}");
    }
}

#[test]
fn model_tiers_are_machine_only_and_project_models_are_concrete() {
    let profile: Profile = toml::from_str("agent = 'claude'\ndefinition_renders = {}\nmodel_tier = {tier = 'principal', fell_back = true}").unwrap();
    assert!(profile.definition_renders.is_none());
    assert!(profile.model_tier.is_none());
    for text in [
        "[profiles.worker]\nagent = 'claude'\ntier = 'senior'",
        "[subagents.profiles.worker]\nagent = 'codex'\ntier = 'intern'",
        "[agents.teams.work]\nroles = [{role = 'worker', profile = 'claude', tier = 'junior'}]",
        "[profiles.worker]\nagent = 'claude'\nmodel = 'senior'",
        "[subagents.profiles.worker]\nagent = 'codex'\nmodel = 'intern'",
        "[agents.teams.work]\nroles = [{role = 'worker', profile = 'claude', model = 'junior'}]",
    ] {
        let value: toml::Value = toml::from_str(text).unwrap();
        let result = repo_config_from_value(&value);
        assert!(result.is_err(), "{text}");
        let error = result.err().unwrap().to_string();
        assert!(error.contains("Markdown definitions"), "{error}");
    }
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(&project, "[tiers]");
    assert!(matches!(
        load(&AgentsConfig::default(), project.path(), config.path()),
        Err(EffectiveConfigErr::ProjectTiers)
    ));
}

#[test]
fn env_reminder_overlay_requires_trust_and_wins_in_both_directions() {
    for machine_value in [false, true] {
        let project = tempdir().unwrap();
        let config = tempdir().unwrap();
        let machine = AgentsConfig {
            env_reminder: machine_value,
            ..Default::default()
        };
        write_project_config(
            &project,
            &format!("[agents]\nenv-reminder = {}\n", !machine_value),
        );
        assert_eq!(
            load(&machine, project.path(), config.path())
                .unwrap()
                .env_reminder,
            machine_value
        );
        crate::trust::grant_with_roots(project.path(), config.path()).unwrap();
        assert_eq!(
            load(&machine, project.path(), config.path())
                .unwrap()
                .env_reminder,
            !machine_value
        );
    }
}

#[test]
fn retired_git_reminder_rejects_project_trust() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(&project, "[agents]\ngit-reminder = false\n");
    match crate::trust::grant_with_roots(project.path(), config.path()) {
        Err(crate::trust::TrustErr::RemovedProjectKey { detail, .. }) => {
            assert!(detail.contains("env-reminder"));
        }
        other => panic!("expected RemovedProjectKey, got {other:?}"),
    }
}

#[test]
fn project_lsp_policy_is_machine_only() {
    for field in [
        "reserve-percent = 20",
        "reserve-min = '2G'",
        "kill-floor-percent = 5",
        "idle-timeout = '1m'",
    ] {
        let value = toml::from_str(&format!("[lsp]\n{field}")).unwrap();
        let error = repo_config_from_value(&value)
            .err()
            .expect("reject project policy");
        assert!(error.to_string().contains("project config cannot set lsp."));
        assert!(error.to_string().contains("move it to ~/.rimz/config.toml"));
    }
}

#[test]
fn project_prompt_fields_reject_inline_text() {
    for declaration in [
        "[profiles.planner]\nagent = 'claude'\nsystem-prompt-file = { origin = 'x', text = 'y' }",
        "[profiles.planner]\nagent = 'claude'\nappend-system-prompt-files = [{ origin = 'x', text = 'y' }]",
        "[agents.teams.review]\nappend-system-prompt-files = [{ origin = 'x', text = 'y' }]",
        "[[agents.teams.review.roles]]\nrole = 'planner'\nprofile = 'claude'\nsystem-prompt-file = { origin = 'x', text = 'y' }",
    ] {
        assert!(
            toml::from_str::<crate::trust::ProjectConfig>(declaration).is_err(),
            "{declaration}"
        );
    }
    for field in [
        "system-prompt-file = { origin = 'x', text = 'y' }",
        "append-system-prompt-files = [{ origin = 'x', text = 'y' }]",
    ] {
        assert!(toml::from_str::<Profile>(&format!("agent = 'claude'\n{field}")).is_err());
    }
}

fn load(machine: &AgentsConfig, project_root: &Path, config_root: &Path) -> Result<LaunchAgents> {
    let config = MachineConfig {
        agents: machine.clone(),
        ..Default::default()
    };
    super::load_with_roots(&config, project_root, config_root)
}

fn profile(agent: &str, args: Option<&str>) -> Profile {
    Profile {
        allowed_tools: None,
        definition_renders: None,
        model_tier: None,
        tier_stamp: None,
        agent: agent.to_owned(),
        isolation: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
        mode: None,
        model: None,
        effort: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        skills: None,
        args: args.map(ToOwned::to_owned),
    }
}

fn profiles(entries: impl IntoIterator<Item = (&'static str, Profile)>) -> ProfilesConfig {
    ProfilesConfig(
        entries
            .into_iter()
            .map(|(name, profile)| (name.to_owned(), profile))
            .collect(),
    )
}

fn write_project_config(dir: &tempfile::TempDir, text: &str) {
    let config_dir = dir.path().join(".rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir .rimz");
    std::fs::write(config_dir.join("config.toml"), text).expect("write config");
}

#[test]
fn diagnosis_reaches_through_a_project_trust_parse_error() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\nagent = \"codex\"\n",
    );

    let Err(error) = load(&AgentsConfig::default(), project.path(), config.path()) else {
        panic!("duplicate project key must fail");
    };
    let diagnosis = error.diagnosis().expect("nested trust diagnosis");

    assert_eq!(diagnosis.line(), Some(3));
    assert_eq!(
        diagnosis.problem(),
        "`agent` is defined more than once in the same table"
    );
}

fn role(role: &str, profile: &str) -> RoleBinding {
    RoleBinding {
        signals: Vec::new(),
        owns: Vec::new(),
        flip_compact: None,
        idle_compact: None,
        keep_warm: None,
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

fn machine_agents(profiles: ProfilesConfig, teams: TeamsConfig) -> AgentsConfig {
    AgentsConfig {
        profiles,
        teams,
        ..AgentsConfig::default()
    }
}

fn effective_profiles(
    machine: &ProfilesConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProfilesConfig> {
    load(
        &machine_agents(machine.clone(), TeamsConfig::default()),
        project_root,
        config_root,
    )
    .map(|launch| launch.profiles)
}

fn effective_subagent_profiles(
    machine: &ProfilesConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProfilesConfig> {
    let mut config = MachineConfig::default();
    config.subagents.profiles = machine.clone();
    super::load_with_roots(&config, project_root, config_root)
        .map(|launch| launch.subagent_profiles)
}

fn effective_teams(
    machine: &TeamsConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<TeamsConfig> {
    load(
        &machine_agents(ProfilesConfig::default(), machine.clone()),
        project_root,
        config_root,
    )
    .map(|launch| launch.teams)
}

fn block_untrusted_profile_reference(
    spec: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<()> {
    let agents = machine_agents(profiles.clone(), teams.clone());
    let launch = load(&agents, project_root, config_root)?;
    launch.block_untrusted_reference(ProfileScope::Agents, spec, commands)
}

fn load_project_tasks(
    project_root: &std::path::Path,
    config_root: &std::path::Path,
) -> Result<ProjectTasks> {
    project_tasks(project_root, config_root).map(|tasks| tasks.expect("project tasks"))
}

#[test]
fn trusted_repo_profile_overlays_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\nargs = \"--repo\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("planner", profile("claude", Some("--machine")))]);

    let effective = effective_profiles(&machine, project.path(), config.path()).expect("effective");

    assert_eq!(
        effective.0.get("planner").and_then(|p| p.args.as_deref()),
        Some("--repo")
    );
}

#[test]
fn trusted_project_tasks_load_with_project_root_and_prompt_paths() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\naccount = \"work\"\nprompt-file = \"prompts/wait.md\"\nsystem-prompt-file = \"prompts/system.md\"\nevery = \"day\"\nat = \"08:00\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");
    let wait = loaded.tasks.0.get("wait").expect("wait task");

    assert_eq!(loaded.state, TrustState::Trusted);
    assert_eq!(loaded.config_path, project.path().join(".rimz/config.toml"));
    assert_eq!(wait.root, project.path());
    assert_eq!(wait.account, Some("work".parse().expect("login name")));
    assert_eq!(
        wait.prompt_file.as_ref(),
        Some(&project.path().join(".rimz/prompts/wait.md"))
    );
    assert_eq!(
        wait.system_prompt_file.as_ref(),
        Some(&project.path().join(".rimz/prompts/system.md"))
    );
}

#[test]
fn untrusted_project_tasks_stay_visible_with_state() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");

    assert_eq!(loaded.state, TrustState::Untrusted);
    assert!(loaded.tasks.0.contains_key("wait"));
}

#[test]
fn project_tasks_reject_machine_local_fields() {
    let cases = [
        (
            "[tasks.wait]\nagent = \"codex\"\nwhen = [\"team.stage=Done\"]\n",
            "when",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\nevery = \"1h\"\nfor = \"30m\"\n",
            "for",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\ndir = \"/tmp/linked\"\nevery = \"day\"\nat = \"08:00\"\n",
            "dir",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\nroot = \"/tmp/other\"\nevery = \"day\"\nat = \"08:00\"\n",
            "root",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\nwait = { kind = \"codex\", session = \"sess\", handle = \"@codex\" }\nevery = \"day\"\nat = \"08:00\"\n",
            "wait",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\ndeadline = \"2026-07-01T12:00:00Z\"\nevery = \"day\"\nat = \"08:00\"\n",
            "deadline",
        ),
        (
            "[tasks.wait]\nagent = \"codex\"\nsignal = \"ci.failed\"\nwait-meta = { armed_by = { kind = \"human\" }, armed_at = \"2026-07-01T12:00:00Z\" }\n",
            "wait-meta",
        ),
    ];
    for (text, field) in cases {
        let project = tempdir().expect("project");
        let config = tempdir().expect("config");
        write_project_config(&project, text);

        let err = project_tasks(project.path(), config.path()).expect_err("invalid field");

        assert!(matches!(
            err,
            EffectiveConfigErr::Tasks {
                source: ProjectTasksErr::UnsupportedField { field: found, .. },
                ..
            } if found == field
        ));
    }
}

#[test]
fn project_tasks_require_prompt_for_spawn_tasks() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("missing prompt");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::MissingPrompt { ref task },
            ..
        } if task == "wait"
    ));
    assert!(
        err.to_string()
            .contains("task `wait` has no prompt; set `prompt` or `prompt-file`"),
        "unexpected error: {err}"
    );

    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"triage\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("prompted project task");

    assert_eq!(
        loaded
            .tasks
            .0
            .get("wait")
            .and_then(|entry| entry.prompt.as_deref()),
        Some("triage")
    );
}

#[test]
fn project_tasks_validate_schedule_shape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nevery = \"weekday\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("invalid schedule");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::Schedule(crate::harness::schedule::ScheduleErr::EveryNeedsAt { name }),
            ..
        } if name == "wait"
    ));
}

#[test]
fn project_tasks_validate_budget_fields() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nevery = \"day\"\nbudget-per-day = \"$20.00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("invalid budget");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::Budget(crate::config::loop_::TaskBudgetError::MissingRunBudget { ref task }),
            ..
        } if task == "wait"
    ));
}

#[test]
fn project_tasks_must_repeat() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nat = \"08:00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("one-shot project task");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::MustRepeat { ref task },
            ..
        } if task == "wait"
    ));
}

#[test]
fn project_tasks_accept_signals_and_reject_machine_only_trigger_fields() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nsignal = \"ci.failed\"\nmatch = { branch = \"feature\" }\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("signal project task");
    let task = &loaded.tasks.0["wait"];
    assert_eq!(task.signal.as_deref(), Some("ci.failed"));
    assert_eq!(
        task.matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );

    for (field, value) in [
        ("watch", "\"cargo test\""),
        ("once", "true"),
        ("stay", "true"),
        ("each-worktree", "true"),
        ("takeover", "true"),
        ("subscribe", "[{ signal = \"ci.failed\" }]"),
    ] {
        write_project_config(
            &project,
            &format!(
                "[tasks.wait]\nagent = \"codex\"\nprompt = \"wait\"\nsignal = \"ci.failed\"\n{field} = {value}\n"
            ),
        );
        let err = project_tasks(project.path(), config.path()).expect_err(field);
        assert!(matches!(
            err,
            EffectiveConfigErr::Tasks {
                source: ProjectTasksErr::UnsupportedField { field: actual, .. },
                ..
            } if actual == field
        ));
    }
}

#[test]
fn untrusted_repo_profiles_are_inert_until_referenced() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    let machine = profiles([("local", profile("claude", Some("--local")))]);

    write_project_config(&project, "display_name = \"Query Engine\"\n");
    let effective = effective_profiles(&machine, project.path(), config.path()).expect("effective");
    assert_eq!(effective, machine);

    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");
    let effective = effective_profiles(&machine, project.path(), config.path()).expect("effective");
    assert_eq!(effective, machine);
    block_untrusted_profile_reference(
        Some("local"),
        &effective,
        &CommandsConfig::default(),
        &TeamsConfig::default(),
        project.path(),
        config.path(),
    )
    .expect("machine profile stays launchable");

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("planner"),
            &effective,
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_repo_profile_reference_is_detected_inside_requested_shape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\n\n[profiles.claude]\nagent = \"claude\"\n",
    );
    let profiles = ProfilesConfig::default();
    let commands = CommandsConfig::default();
    let teams = TeamsConfig::default();

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("planner,codex"),
            &profiles,
            &commands,
            &teams,
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
    for spec in ["planner:lead", "codex/planner:lead"] {
        assert!(matches!(
            block_untrusted_profile_reference(
                Some(spec),
                &profiles,
                &commands,
                &teams,
                project.path(),
                config.path(),
            ),
            Err(EffectiveConfigErr::Blocked {
                state: "untrusted",
                ..
            })
        ));
    }
    block_untrusted_profile_reference(
        Some("claude"),
        &profiles,
        &commands,
        &teams,
        project.path(),
        config.path(),
    )
    .expect("repo profile named like a built-in kind stays inert for the built-in launch");

    let commands = CommandsConfig(BTreeMap::from([(
        "planner:lead".to_owned(),
        "true".to_owned(),
    )]));
    block_untrusted_profile_reference(
        Some("planner:lead"),
        &profiles,
        &commands,
        &teams,
        project.path(),
        config.path(),
    )
    .expect("exact machine command with a colon stays launchable");
}

#[test]
fn repo_profile_cannot_inherit_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.child]\nagent = \"machine-base\"\n");
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("machine-base", profile("claude", Some("--machine")))]);

    let err = effective_profiles(&machine, project.path(), config.path()).expect_err("closed");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::RepoProfileEscapesTrust { profile, base },
            ..
        } if profile == "child" && base == "machine-base"
    ));
}

#[test]
fn repo_profiles_cannot_set_isolation_in_either_scope() {
    for namespace in ["profiles", "subagents.profiles"] {
        let project = tempdir().expect("project");
        let config = tempdir().expect("config");
        write_project_config(
            &project,
            &format!("[{namespace}.child]\nagent = \"claude\"\nisolation = \"host\"\n"),
        );
        crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
        let err = effective_profiles(&ProfilesConfig::default(), project.path(), config.path())
            .expect_err("machine-only isolation");
        assert!(err.to_string().contains("--isolation"), "{err}");
        assert!(
            matches!(err, EffectiveConfigErr::Agents { source: LayoutErr::RepoProfileSetsIsolation { profile }, .. } if profile == "child")
        );
    }
}

#[test]
fn repo_profile_typo_reports_unknown_base_not_machine_escape() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.child]\nagent = \"typoo\"\n");
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("machine-base", profile("claude", Some("--machine")))]);

    let err = effective_profiles(&machine, project.path(), config.path()).expect_err("typo");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::UnknownProfileBase { profile, base },
            ..
        } if profile == "child" && base == "typoo"
    ));
}

#[test]
fn trusted_repo_subagent_profile_overlays_machine_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[subagents.profiles.reviewer]\nagent = \"codex\"\nargs = \"--repo\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("reviewer", profile("claude", Some("--machine")))]);

    let effective =
        effective_subagent_profiles(&machine, project.path(), config.path()).expect("effective");

    let reviewer = effective.0.get("reviewer").expect("reviewer profile");
    assert_eq!(reviewer.agent, "codex");
    assert_eq!(reviewer.args.as_deref(), Some("--repo"));
}

#[test]
fn trusted_repo_allowlist_can_reference_repo_subagent_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\nsubagents = [\"repo-child\"]\n\
         [subagents.profiles.repo-child]\nagent = \"codex\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let effective =
        super::load_with_roots(&MachineConfig::default(), project.path(), config.path())
            .expect("merged catalogs validate together");

    assert_eq!(
        effective
            .profiles
            .0
            .get("planner")
            .and_then(|profile| profile.subagents.as_deref()),
        Some(["repo-child".to_owned()].as_slice())
    );
    assert!(effective.subagent_profiles.0.contains_key("repo-child"));
    let mut sources = crate::config::AgentSpecSources::default();
    effective.overlay_profile_sources(&mut sources);
    assert_eq!(
        sources.profile(ProfileScope::Agents, "planner"),
        Some(project.path().join(".rimz/config.toml").as_path())
    );
    assert_eq!(
        sources.profile(ProfileScope::Subagents, "repo-child"),
        Some(project.path().join(".rimz/config.toml").as_path())
    );
}

#[test]
fn untrusted_repo_subagent_profiles_are_inert_and_block_on_reference() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[subagents.profiles.reviewer]\nagent = \"codex\"\n",
    );
    let machine = profiles([("local", profile("claude", Some("--local")))]);
    let mut machine_config = MachineConfig::default();
    machine_config.subagents.profiles = machine.clone();
    let launch =
        super::load_with_roots(&machine_config, project.path(), config.path()).expect("effective");

    assert_eq!(launch.subagent_profiles, machine);
    assert!(matches!(
        launch.block_untrusted_reference(
            ProfileScope::Subagents,
            Some("reviewer"),
            &CommandsConfig::default(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn repo_subagent_profile_cannot_inherit_machine_subagent_profile() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[subagents.profiles.child]\nagent = \"machine-base\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = profiles([("machine-base", profile("claude", Some("--machine")))]);

    let err = effective_subagent_profiles(&machine, project.path(), config.path())
        .expect_err("subagent namespace stays trust-closed");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::RepoProfileEscapesTrust { profile, base },
            ..
        } if profile == "child" && base == "machine-base"
    ));
}

#[test]
fn repo_profiles_resolve_repo_and_builtin_bases() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.base]\nagent = \"codex\"\nmode = \"ask\"\n\n[profiles.child]\nagent = \"base\"\nargs = \"--child\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let effective = effective_profiles(&ProfilesConfig::default(), project.path(), config.path())
        .expect("effective");

    let child = crate::harness::spec::resolve_profile("child", &effective).expect("resolve child");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(child.args.as_deref(), Some("--child"));
}

#[test]
fn repo_prompt_file_paths_resolve_against_rimz_dir() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let effective = effective_profiles(
        &ProfilesConfig(BTreeMap::new()),
        project.path(),
        config.path(),
    )
    .expect("effective");

    assert_eq!(
        effective
            .0
            .get("planner")
            .and_then(|profile| profile.system_prompt_file.as_ref()),
        Some(&crate::config::PromptSource::File(
            project.path().join(".rimz/prompts/planner.md")
        ))
    );
}

#[test]
fn teams_fall_back_to_machine_without_project_or_on_load_error() {
    let project = tempdir().unwrap();
    let config = tempdir().unwrap();
    let mut machine = MachineConfig::default();
    machine
        .agents
        .teams
        .0
        .insert("machine".to_owned(), Team::default());
    assert_eq!(super::teams(&machine, None), machine.agents.teams);

    write_project_config(&project, "[broken");
    assert!(super::load_with_roots(&machine, project.path(), config.path()).is_err());
    assert_eq!(
        teams_with_roots(&machine, Some(project.path()), config.path()),
        machine.agents.teams
    );
}

#[test]
fn trusted_repo_team_overlays_machine_team_and_resolves_prompt_paths() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");
    let machine = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        Team {
            roles: vec![role("local", "local-profile")],
            leader: None,
            layout: None,
            scratch_files: None,
            consensus_file: None,
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    )]));

    let effective = effective_teams(&machine, project.path(), config.path()).expect("effective");

    let mut machine_config = MachineConfig::default();
    machine_config.agents.teams = machine;
    assert_eq!(
        teams_with_roots(&machine_config, Some(project.path()), config.path()),
        effective
    );

    let role = &effective.0.get("review").expect("repo team").roles[0];
    assert_eq!(role.role, "planner");
    assert_eq!(role.profile, "planner");
    assert_eq!(
        role.system_prompt_file.as_ref(),
        Some(&crate::config::PromptSource::File(
            project.path().join(".rimz/prompts/planner.md")
        ))
    );
}

#[test]
fn repo_team_roles_require_repo_profiles_even_for_builtin_kinds() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"claude\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let err = effective_teams(&TeamsConfig::default(), project.path(), config.path())
        .expect_err("repo team stays closed over repo profiles");

    assert!(matches!(
        err,
        EffectiveConfigErr::Agents {
            source: crate::harness::spec::LayoutErr::UnknownRoleProfile {
                team,
                role,
                profile,
            },
            ..
        } if team == "review" && role == "planner" && profile == "claude"
    ));
}

#[test]
fn untrusted_repo_team_reference_is_blocked() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[profiles.planner]\nagent = \"claude\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\n",
    );

    assert_eq!(
        effective_teams(&TeamsConfig::default(), project.path(), config.path())
            .expect("untrusted effective teams"),
        TeamsConfig::default()
    );
    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review"),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review.planner"),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &TeamsConfig::default(),
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_repo_profile_inside_machine_team_layout_is_blocked() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");
    let machine_profiles = profiles([("local", profile("codex", None))]);
    let machine_teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        Team {
            roles: vec![role("coder", "local")],
            leader: None,
            layout: Some("coder,planner".to_owned()),
            scratch_files: None,
            consensus_file: None,
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    )]));

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review"),
            &machine_profiles,
            &CommandsConfig::default(),
            &machine_teams,
            project.path(),
            config.path(),
        ),
        Err(EffectiveConfigErr::Blocked {
            state: "untrusted",
            ..
        })
    ));
}

#[test]
fn untrusted_layout_trust_uses_shared_structural_cells_and_inline_precedence() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(&project, "[profiles.planner]\nagent = \"claude\"\n");

    for spec in [
        "planner:lead+claude,codex/term",
        "claude+planner:lead,codex/term",
        "claude+term,planner:lead/codex",
        "claude+term,codex/planner:lead",
    ] {
        assert!(
            matches!(
                block_untrusted_profile_reference(
                    Some(spec),
                    &ProfilesConfig::default(),
                    &CommandsConfig::default(),
                    &TeamsConfig::default(),
                    project.path(),
                    config.path(),
                ),
                Err(EffectiveConfigErr::Blocked { .. })
            ),
            "{spec}"
        );
    }

    let exact_machine_command = CommandsConfig(BTreeMap::from([(
        "planner:lead".to_owned(),
        "true".to_owned(),
    )]));
    block_untrusted_profile_reference(
        Some("planner:lead"),
        &ProfilesConfig::default(),
        &exact_machine_command,
        &TeamsConfig::default(),
        project.path(),
        config.path(),
    )
    .expect("exact machine cell containing a colon stays inert");
}

fn write_home_machine_config(home: &tempfile::TempDir, text: &str) -> MachineConfig {
    write_project_config(home, text);
    let rimz_home = home.path().join(".rimz");
    MachineConfig::load_from(&rimz_home.join("config.toml"), &rimz_home).expect("machine config")
}

#[test]
fn home_machine_lsp_servers_are_machine_entries_not_untrusted_declarations() {
    let home = tempdir().unwrap();
    let machine = write_home_machine_config(
        &home,
        "[lsp.servers.rust]\ncommand = [\"rust-analyzer\"]\nextensions = [\"rs\"]\nroot-markers = [\"Cargo.toml\"]\n",
    );
    let loaded = load_with_roots(&machine, home.path(), &home.path().join(".rimz")).unwrap();
    assert!(loaded.untrusted_lsp_servers.is_empty());
    assert!(loaded.lsp_servers.contains_key("rust"));
}

#[test]
fn home_machine_lsp_policy_keys_are_not_project_policy() {
    let home = tempdir().unwrap();
    let machine = write_home_machine_config(&home, "[lsp]\nreserve-percent = 10\n");
    load_with_roots(&machine, home.path(), &home.path().join(".rimz")).unwrap();
}

#[test]
fn home_project_config_stays_a_project_layer_when_rimz_home_is_elsewhere() {
    let home = tempdir().unwrap();
    let config = tempdir().unwrap();
    write_project_config(
        &home,
        "[lsp.servers.rust]\ncommand = [\"rust-analyzer\"]\nextensions = [\"rs\"]\nroot-markers = [\"Cargo.toml\"]\n",
    );
    let loaded = load(&AgentsConfig::default(), home.path(), config.path()).unwrap();
    assert_eq!(loaded.untrusted_lsp_servers, vec!["rust".to_owned()]);
}
