use super::*;
use crate::config::{
    AgentsConfig, CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig,
};
use crate::harness::run::PermissionMode;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn profile(agent: &str, args: Option<&str>) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
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
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        budget: None,
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
    launch.block_untrusted_reference(spec, commands)
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
        "[tasks.wake]\nagent = \"codex\"\nprompt-file = \"prompts/wake.md\"\nsystem-prompt-file = \"prompts/system.md\"\nevery = \"day\"\nat = \"08:00\"\n",
    );
    crate::trust::grant_with_roots(project.path(), config.path()).expect("grant");

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");
    let wake = loaded.tasks.0.get("wake").expect("wake task");

    assert_eq!(loaded.state, TrustState::Trusted);
    assert_eq!(loaded.config_path, project.path().join(".rimz/config.toml"));
    assert_eq!(wake.root, project.path());
    assert_eq!(
        wake.prompt_file.as_ref(),
        Some(&project.path().join(".rimz/prompts/wake.md"))
    );
    assert_eq!(
        wake.system_prompt_file.as_ref(),
        Some(&project.path().join(".rimz/prompts/system.md"))
    );
}

#[test]
fn untrusted_project_tasks_stay_visible_with_state() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wake]\nagent = \"codex\"\nprompt = \"wake\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("project tasks");

    assert_eq!(loaded.state, TrustState::Untrusted);
    assert!(loaded.tasks.0.contains_key("wake"));
}

#[test]
fn project_tasks_reject_machine_local_fields() {
    let cases = [
        (
            "[tasks.wake]\nagent = \"codex\"\nroot = \"/tmp/other\"\nevery = \"day\"\nat = \"08:00\"\n",
            "root",
        ),
        (
            "[tasks.wake]\nagent = \"codex\"\nwake = { kind = \"codex\", session = \"sess\", handle = \"@codex\" }\nevery = \"day\"\nat = \"08:00\"\n",
            "wake",
        ),
        (
            "[tasks.wake]\nagent = \"codex\"\ndeadline = \"2026-07-01T12:00:00Z\"\nevery = \"day\"\nat = \"08:00\"\n",
            "deadline",
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
        "[tasks.wake]\nagent = \"codex\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("missing prompt");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::MissingPrompt { ref task },
            ..
        } if task == "wake"
    ));
    assert!(
        err.to_string()
            .contains("task `wake` has no prompt; set `prompt` or `prompt-file`"),
        "unexpected error: {err}"
    );

    write_project_config(
        &project,
        "[tasks.wake]\nagent = \"codex\"\nprompt = \"triage\"\nevery = \"day\"\nat = \"08:00\"\n",
    );

    let loaded = load_project_tasks(project.path(), config.path()).expect("prompted project task");

    assert_eq!(
        loaded
            .tasks
            .0
            .get("wake")
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
        "[tasks.wake]\nagent = \"codex\"\nprompt = \"wake\"\nevery = \"weekday\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("invalid schedule");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::Schedule(crate::harness::schedule::ScheduleErr::EveryNeedsAt { name }),
            ..
        } if name == "wake"
    ));
}

#[test]
fn project_tasks_validate_budget_fields() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wake]\nagent = \"codex\"\nprompt = \"wake\"\nevery = \"day\"\nbudget-per-day = \"$20.00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("invalid budget");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::Budget(crate::config::TaskBudgetError::MissingRunBudget { ref task }),
            ..
        } if task == "wake"
    ));
}

#[test]
fn project_tasks_must_repeat() {
    let project = tempdir().expect("project");
    let config = tempdir().expect("config");
    write_project_config(
        &project,
        "[tasks.wake]\nagent = \"codex\"\nprompt = \"wake\"\nat = \"08:00\"\n",
    );

    let err = project_tasks(project.path(), config.path()).expect_err("one-shot project task");

    assert!(matches!(
        err,
        EffectiveConfigErr::Tasks {
            source: ProjectTasksErr::MustRepeat { ref task },
            ..
        } if task == "wake"
    ));
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
        Some(&project.path().join(".rimz/prompts/planner.md"))
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
            scratch_files: Vec::new(),
        },
    )]));

    let effective = effective_teams(&machine, project.path(), config.path()).expect("effective");

    let role = &effective.0.get("review").expect("repo team").roles[0];
    assert_eq!(role.role, "planner");
    assert_eq!(role.profile, "planner");
    assert_eq!(
        role.system_prompt_file.as_ref(),
        Some(&project.path().join(".rimz/prompts/planner.md"))
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
            scratch_files: Vec::new(),
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
