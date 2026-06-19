use super::*;
use crate::config::{CommandsConfig, LayoutsConfig, Profile, ProfilesConfig};
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
        &LayoutsConfig::default(),
        project.path(),
        config.path(),
    )
    .expect("machine profile stays launchable");

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("planner"),
            &effective,
            &CommandsConfig::default(),
            &LayoutsConfig::default(),
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
    let layouts = LayoutsConfig(BTreeMap::from([(
        "review".to_owned(),
        "planner,codex".to_owned(),
    )]));

    assert!(matches!(
        block_untrusted_profile_reference(
            Some("review"),
            &profiles,
            &commands,
            &layouts,
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
        &layouts,
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
        EffectiveConfigErr::Profiles {
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
        EffectiveConfigErr::Profiles {
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
