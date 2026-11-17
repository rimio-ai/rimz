//! The RimZ mod folder and its `env.CLAUDE_CODE_PLUGIN_DIRS` entry, installed
//! and removed beside the settings hooks.

use super::*;

use std::collections::BTreeMap;

use crate::agents::adapters::claude::install::MANAGED_INTEGRATION;
use crate::agents::{AgentErr, HookInstallOptions, ManagedIntegration};

struct Fixture {
    _temp: tempfile::TempDir,
    env: BTreeMap<String, String>,
    settings: PathBuf,
    mod_dir: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("claude/settings.json");
    let rimz_home = temp.path().join("rimz");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let env = BTreeMap::from([
        (
            "HOME".to_owned(),
            temp.path().join("home").display().to_string(),
        ),
        ("RIMZ_HOME".to_owned(), rimz_home.display().to_string()),
        (
            "RIMZ_CLAUDE_SETTINGS".to_owned(),
            settings.display().to_string(),
        ),
    ]);
    Fixture {
        mod_dir: rimz_home.join("mods/claude"),
        _temp: temp,
        env,
        settings,
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn install(fixture: &Fixture) -> crate::agents::HookInstallReport {
    MANAGED_INTEGRATION
        .install(&fixture.env, HookInstallOptions::default())
        .unwrap()
}

#[test]
fn install_writes_the_mod_and_lists_it_after_the_users_folders() {
    let fixture = fixture();
    std::fs::write(
        &fixture.settings,
        r#"{"env":{"CLAUDE_CODE_PLUGIN_DIRS":"/mine:~/also","FOO":"bar"}}"#,
    )
    .unwrap();
    let report = install(&fixture);
    let mod_dir = fixture.mod_dir.display().to_string();
    let settings = read(&fixture.settings);
    assert_eq!(
        settings["env"],
        serde_json::json!({
            "CLAUDE_CODE_PLUGIN_DIRS": format!("/mine:~/also:{mod_dir}"),
            "FOO": "bar",
        })
    );
    for file in [
        ".claude-plugin/plugin.json",
        "hooks/hooks.json",
        "hooks/rimz.ts",
    ] {
        assert!(fixture.mod_dir.join(file).is_file(), "{file}");
        assert!(
            report
                .files
                .iter()
                .any(|written| written.path == fixture.mod_dir.join(file)),
            "{file} is reported"
        );
    }
    assert!(MANAGED_INTEGRATION.installed(&fixture.env));
    assert!(
        MANAGED_INTEGRATION
            .outdated_files(&fixture.env, HookInstallOptions::default())
            .is_empty()
    );

    let settings_bytes = std::fs::read(&fixture.settings).unwrap();
    let module = fixture.mod_dir.join("hooks/rimz.ts");
    let module_inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&module).unwrap());
    let again = install(&fixture);
    assert_eq!(std::fs::read(&fixture.settings).unwrap(), settings_bytes);
    assert_eq!(again.files.len(), 1, "a reinstall writes no mod file");
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&module).unwrap()),
        module_inode,
        "an unchanged module is not rewritten, so open sessions do not reload"
    );
}

#[test]
fn install_then_uninstall_leaves_the_settings_as_they_were() {
    for before in [
        None,
        Some(r#"{"model":"opus"}"#),
        Some(r#"{"env":{"FOO":"bar"},"model":"opus"}"#),
        Some(r#"{"env":{"CLAUDE_CODE_PLUGIN_DIRS":"/mine:~/also","FOO":"bar"}}"#),
    ] {
        let fixture = fixture();
        if let Some(before) = before {
            std::fs::write(&fixture.settings, before).unwrap();
        }
        install(&fixture);
        assert!(MANAGED_INTEGRATION.managed_artifacts_present(&fixture.env));
        MANAGED_INTEGRATION.uninstall(&fixture.env).unwrap();
        let after = read(&fixture.settings);
        let expected = before.map_or_else(
            || serde_json::json!({}),
            |before| serde_json::from_str(before).unwrap(),
        );
        assert_eq!(after, expected, "{before:?}");
        assert!(!MANAGED_INTEGRATION.managed_artifacts_present(&fixture.env));
        assert!(
            fixture.mod_dir.join("hooks/rimz.ts").is_file(),
            "hook uninstall leaves the shared mod folder"
        );
    }
}

#[test]
fn a_foreign_env_shape_refuses_install_and_leaves_the_file() {
    for before in [
        r#"{"env":"FOO=bar"}"#,
        r#"{"env":{"CLAUDE_CODE_PLUGIN_DIRS":["/mine"]}}"#,
    ] {
        let fixture = fixture();
        std::fs::write(&fixture.settings, before).unwrap();
        let err = MANAGED_INTEGRATION
            .install(&fixture.env, HookInstallOptions::default())
            .unwrap_err();
        assert!(
            matches!(&err, AgentErr::Install { agent: "claude", reason } if reason.contains("refusing to install")),
            "{err}"
        );
        assert!(
            MANAGED_INTEGRATION
                .preview(&fixture.env, HookInstallOptions::default())
                .is_err(),
            "the consent gate sees the refusal first"
        );
        assert_eq!(std::fs::read_to_string(&fixture.settings).unwrap(), before);
    }
}

#[test]
fn an_unwritable_mod_folder_fails_the_install_before_the_settings_change() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.mod_dir.parent().unwrap()).unwrap();
    std::fs::write(&fixture.mod_dir, "a file where the folder goes").unwrap();
    assert!(
        MANAGED_INTEGRATION
            .install(&fixture.env, HookInstallOptions::default())
            .is_err()
    );
    assert!(!fixture.settings.exists());
}

#[test]
fn missing_entry_and_stale_or_missing_mod_files_are_drift() {
    let fixture = fixture();
    assert!(
        MANAGED_INTEGRATION
            .outdated_files(&fixture.env, HookInstallOptions::default())
            .is_empty(),
        "no install, no drift"
    );
    install(&fixture);
    let outdated =
        || MANAGED_INTEGRATION.outdated_files(&fixture.env, HookInstallOptions::default());

    let module = fixture.mod_dir.join("hooks/rimz.ts");
    std::fs::write(&module, "stale").unwrap();
    assert_eq!(outdated(), vec![module.clone()]);
    std::fs::remove_dir_all(&fixture.mod_dir).unwrap();
    assert_eq!(outdated().len(), 3, "a missing folder lists every file");
    install(&fixture);

    let mut settings = read(&fixture.settings);
    settings.as_object_mut().unwrap().remove("env");
    std::fs::write(&fixture.settings, settings.to_string()).unwrap();
    assert!(MANAGED_INTEGRATION.installed(&fixture.env));
    assert_eq!(outdated(), vec![fixture.settings.clone()]);
}

#[test]
fn preview_names_the_mod_folder_and_its_new_files() {
    let fixture = fixture();
    let preview = MANAGED_INTEGRATION
        .preview(&fixture.env, HookInstallOptions::default())
        .unwrap();
    assert_eq!(
        preview.plugin_dir.as_deref(),
        Some(fixture.mod_dir.as_path())
    );
    assert_eq!(preview.files[0].path, fixture.settings);
    assert!(preview.files[0].candidate.contains(&format!(
        r#""CLAUDE_CODE_PLUGIN_DIRS": "{}""#,
        fixture.mod_dir.display()
    )));
    let mod_files: Vec<_> = preview.files[1..]
        .iter()
        .map(|file| file.path.strip_prefix(&fixture.mod_dir).unwrap().to_owned())
        .collect();
    assert_eq!(
        mod_files,
        [
            ".claude-plugin/plugin.json",
            "hooks/hooks.json",
            "hooks/rimz.ts"
        ]
        .map(PathBuf::from)
    );
    assert!(
        preview.files[1..]
            .iter()
            .all(|file| file.original.is_none())
    );

    install(&fixture);
    let preview = MANAGED_INTEGRATION
        .preview(&fixture.env, HookInstallOptions::default())
        .unwrap();
    assert_eq!(
        preview.files.len(),
        1,
        "current mod files are not re-listed"
    );
    assert!(preview.plugin_dir.is_some(), "consent still names the mod");
}

#[test]
fn named_and_ambient_homes_list_the_same_mod_folder() {
    let fixture = fixture();
    let mut ambient = fixture.env.clone();
    ambient.remove("RIMZ_CLAUDE_SETTINGS");
    let home = PathBuf::from(&ambient["HOME"]);
    let named_home = home.join("named");
    std::fs::create_dir_all(&named_home).unwrap();
    let named = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("claude"),
        "work".parse().unwrap(),
        named_home.clone(),
    )
    .unwrap()
    .env(&ambient);
    for env in [&ambient, &named] {
        MANAGED_INTEGRATION
            .install(env, HookInstallOptions::default())
            .unwrap();
    }
    let mod_dir = fixture.mod_dir.display().to_string();
    for settings in [
        home.join(".claude/settings.json"),
        named_home.join("settings.json"),
    ] {
        assert_eq!(
            read(&settings)["env"]["CLAUDE_CODE_PLUGIN_DIRS"],
            mod_dir,
            "{settings:?}"
        );
    }
}
