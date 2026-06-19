use super::*;
use crate::config::{CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig};
use crate::run::PermissionMode;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn profile(agent: &str, args: Option<&str>) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode: None,
        model: None,
        effort: None,
        system_prompt_file: None,
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

fn role(role: &str, profile: &str) -> RoleBinding {
    RoleBinding {
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        system_prompt_file: None,
        args: None,
    }
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
    block_untrusted_profile_reference(
        Some("claude"),
        &profiles,
        &commands,
        &teams,
        project.path(),
        config.path(),
    )
    .expect("repo profile named like a built-in kind stays inert for the built-in launch");
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
            source: crate::agents_spec::LayoutErr::RepoProfileEscapesTrust { profile, base },
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
            source: crate::agents_spec::LayoutErr::UnknownProfileBase { profile, base },
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

    let child = crate::agents_spec::resolve_profile("child", &effective).expect("resolve child");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.mode, Some(PermissionMode::Ask));
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
            layout: None,
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
            layout: Some("coder,planner".to_owned()),
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
