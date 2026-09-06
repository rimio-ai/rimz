use std::path::Path;

use super::super::project_trust::trust_gap_at;

fn write_trust(config: &Path, entries: &[(&Path, &str)], markers: Option<&[&str]>) {
    let mut root = toml::Table::new();
    let mut projects = toml::Table::new();
    for (path, level) in entries {
        let mut project = toml::Table::new();
        project.insert("trust_level".to_owned(), (*level).into());
        projects.insert(path.to_string_lossy().into_owned(), project.into());
    }
    root.insert("projects".to_owned(), projects.into());
    if let Some(markers) = markers {
        root.insert(
            "project_root_markers".to_owned(),
            toml::Value::Array(markers.iter().map(|marker| (*marker).into()).collect()),
        );
    }
    std::fs::write(config, toml::to_string(&root).unwrap()).unwrap();
}

#[test]
fn exact_cwd_accepts_either_recorded_trust_level() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    for level in ["trusted", "untrusted"] {
        write_trust(&config, &[(dir.path(), level)], None);
        assert_eq!(trust_gap_at(&config, dir.path(), None), None);
    }
}

#[test]
fn project_trust_checks_nearest_root_and_configured_markers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    let nested = root.join("nested");
    let cwd = nested.join("src");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let config = dir.path().join("config.toml");
    write_trust(&config, &[(&root, "trusted")], None);
    assert_eq!(trust_gap_at(&config, &cwd, None), None);

    std::fs::write(nested.join("project.marker"), "").unwrap();
    std::fs::write(root.join("project.marker"), "").unwrap();
    write_trust(&config, &[(&root, "trusted")], Some(&["project.marker"]));
    assert!(trust_gap_at(&config, &cwd, None).is_some());
    write_trust(
        &config,
        &[(&nested, "untrusted")],
        Some(&["project.marker"]),
    );
    assert_eq!(trust_gap_at(&config, &cwd, None), None);
    write_trust(&config, &[(&nested, "trusted")], Some(&[]));
    assert!(trust_gap_at(&config, &cwd, None).is_some());
}

#[test]
fn project_trust_skips_incomplete_git_markers() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("src");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let config = dir.path().join("config.toml");
    write_trust(&config, &[(dir.path(), "trusted")], None);
    assert!(trust_gap_at(&config, &cwd, None).is_some());
}

#[test]
fn linked_worktree_trust_checks_checkout_then_supplied_main_root() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    let checkout = dir.path().join("linked");
    let cwd = checkout.join("src");
    std::fs::create_dir(&main).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        checkout.join(".git"),
        "gitdir: ../main/.git/worktrees/linked\n",
    )
    .unwrap();
    let config = dir.path().join("config.toml");
    write_trust(&config, &[(&checkout, "untrusted")], None);
    assert_eq!(trust_gap_at(&config, &cwd, Some(&main)), None);
    write_trust(&config, &[(&main, "trusted")], None);
    assert_eq!(trust_gap_at(&config, &cwd, Some(&main)), None);
    assert!(trust_gap_at(&config, &cwd, None).is_some());
    write_trust(&config, &[(&cwd, "untrusted"), (&main, "trusted")], None);
    assert_eq!(trust_gap_at(&config, &cwd, Some(&main)), None);
}

#[test]
fn missing_project_trust_names_manual_fixes_without_writing_config() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("src");
    std::fs::create_dir(&cwd).unwrap();
    let config = dir.path().join("config.toml");
    for repo in [None, Some(dir.path())] {
        let fix = trust_gap_at(&config, &cwd, repo).unwrap();
        assert!(fix.contains(config.to_str().unwrap()));
        assert!(fix.contains(cwd.to_str().unwrap()));
        let key = toml::Value::String(
            repo.unwrap_or(&cwd)
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        assert!(fix.contains(&format!("[projects.{key}]")));
    }
    assert!(!config.exists());
    write_trust(&config, &[], None);
    assert!(trust_gap_at(&config, &cwd, None).is_some());
}

#[test]
fn malformed_project_trust_requires_repair_even_with_a_valid_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("src");
    std::fs::create_dir(&cwd).unwrap();
    let config = dir.path().join("config.toml");
    write_trust(&config, &[(&cwd, "invalid"), (dir.path(), "trusted")], None);
    let fix = trust_gap_at(&config, &cwd, Some(dir.path())).unwrap();
    assert!(fix.contains("repair"));
    assert!(fix.contains("invalid"));
    for text in [
        "[broken",
        "project_root_markers = [42]",
        "[projects.somewhere]\ntrust_level = true",
    ] {
        std::fs::write(&config, text).unwrap();
        let fix = trust_gap_at(&config, &cwd, None).unwrap();
        assert!(fix.contains("repair"), "{fix}");
        assert!(fix.contains(config.to_str().unwrap()));
    }
}

#[test]
fn project_trust_accepts_canonical_and_symlink_keys() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("actual");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&cwd).unwrap();
    std::os::unix::fs::symlink(&cwd, &alias).unwrap();
    let config = dir.path().join("config.toml");
    for key in [&cwd.canonicalize().unwrap(), &alias] {
        write_trust(&config, &[(key, "trusted")], None);
        assert_eq!(trust_gap_at(&config, &alias, None), None);
    }
}

#[test]
fn codex_config_path_honors_codex_home_and_override() {
    const PROBE: &str = "RIMZ_TEST_CODEX_CONFIG_PATH";
    if let Some(expected) = std::env::var_os(PROBE) {
        let path = super::super::install::codex_config_path().unwrap();
        assert_eq!(path, std::path::PathBuf::from(expected));
        let adapter = crate::agents::definition_by_kind("codex").unwrap();
        let cwd = path.parent().unwrap();
        let error = crate::agents::preflight_launch_dir(adapter, cwd, None).unwrap_err();
        assert_eq!(error.kind, "codex");
        assert_eq!(error.dir, cwd);
        assert!(error.fix.contains(path.to_str().unwrap()));
        write_trust(&path, &[(cwd, "untrusted")], None);
        assert!(crate::agents::preflight_launch_dir(adapter, cwd, None).is_ok());
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    for override_name in [None, Some("override.toml")] {
        let expected = dir.path().join(override_name.unwrap_or("config.toml"));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "agents::adapters::codex::tests::project_trust::codex_config_path_honors_codex_home_and_override",
                "--nocapture",
            ])
            .env("CODEX_HOME", dir.path())
            .env_remove("RIMZ_CODEX_CONFIG")
            .env(PROBE, &expected);
        if override_name.is_some() {
            command.env("RIMZ_CODEX_CONFIG", &expected);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
