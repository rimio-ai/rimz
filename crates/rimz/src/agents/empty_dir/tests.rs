use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn paths(root: &Path) -> StatePaths {
    StatePaths::for_project_root_under(&root.join("project"), root).unwrap()
}

#[test]
fn account_verdict_uses_the_empty_root_and_its_git_ancestry() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let root = temp.path().join("empty");
    let config = temp.path().join("claude.json");
    let env = BTreeMap::from([
        ("HOME".into(), temp.path().display().to_string()),
        (
            "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
            config.display().to_string(),
        ),
        (
            "RIMZ_CODEX_CONFIG".into(),
            temp.path().join("codex.toml").display().to_string(),
        ),
    ]);
    let claude = AgentKind::new_unchecked("claude");
    let codex = AgentKind::new_unchecked("codex");
    assert!(
        matches!(verdict_at(&root, &claude, &env), Verdict::Fallback(FolderTrust::Undecided(gap)) if gap.grant.is_ok() && gap.key == root)
    );
    fs::write(&config, serde_json::to_vec(&serde_json::json!({"projects": {root.display().to_string(): {"hasTrustDialogAccepted": true}}})).unwrap()).unwrap();
    assert_eq!(verdict_at(&root, &claude, &env), Verdict::Applies);
    assert_eq!(verdict_at(&root, &codex, &env), Verdict::Applies);
    assert_eq!(
        verdict_at(&root, &AgentKind::new_unchecked("pi"), &env),
        Verdict::Applies
    );
    fs::write(
        temp.path().join("codex.toml"),
        format!(
            "[projects.{}]\ntrust_level = 'untrusted'\n",
            toml::Value::String(root.display().to_string())
        ),
    )
    .unwrap();
    assert!(
        matches!(verdict_at(&root, &codex, &env), Verdict::Fallback(FolderTrust::Restricted { key }) if key == root)
    );
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(temp.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        matches!(verdict_at(&root, &claude, &env), Verdict::Fallback(FolderTrust::Undecided(gap)) if gap.grant.is_err() && gap.key == temp.path())
    );
    assert_eq!(paths.empty_dir_root(), root);
}

#[test]
fn root_under_symlinked_home_keeps_one_trust_key_before_and_after_creation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let root = link.join("missing/empty");
    let env = BTreeMap::from([
        ("HOME".into(), home.display().to_string()),
        (
            "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
            home.join("claude.json").display().to_string(),
        ),
    ]);
    let trust = trust_at(&root, &AgentKind::new_unchecked("claude"), &env);
    assert!(
        matches!(&trust, Some(FolderTrust::Undecided(gap)) if gap.key == home.join("missing/empty")),
        "missing roots must use the key that later reads resolve: {trust:?}"
    );
    assert!(!root.exists());
    fs::create_dir_all(&root).unwrap();
    assert_eq!(
        resolved_root(&root).as_os_str(),
        root.canonicalize().unwrap().as_os_str(),
        "an existing root resolves to its canonical path, with no trailing separator"
    );
}

#[test]
fn parent_checks_reject_instructions_files_and_hidden_directories() {
    for room_level in [false, true] {
        for name in ["CLAUDE.md", "AGENTS.md", "plain", ".git", ".claude"] {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let parent = if room_level {
                paths.empty_room_dir()
            } else {
                paths.empty_dir_root()
            };
            let expected_parent = if room_level {
                temp.path().join("empty").join(paths.dir_name.as_str())
            } else {
                temp.path().join("empty")
            };
            fs::create_dir_all(&expected_parent).unwrap();
            let offending = expected_parent.join(name);
            if name.starts_with('.') {
                fs::create_dir(&offending).unwrap();
            } else {
                fs::write(&offending, "instructions").unwrap();
            }
            assert!(
                matches!(check(&paths), Err(EmptyDirErr::Dirty(path)) if path == offending),
                "{parent:?}: {offending:?}"
            );
        }
    }
}

#[test]
fn preparation_owns_a_private_cwd_and_reuses_only_unrecorded_directories() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let result = prepare(&paths, &[], Some("searcher"));
    assert!(result.is_ok(), "fresh empty-dir preparation: {result:?}");
    let prepared = result.unwrap();
    assert_eq!(prepared.name, "searcher");
    assert_eq!(
        prepared.cwd,
        temp.path()
            .join("empty")
            .join(paths.dir_name.as_str())
            .join("searcher")
    );
    assert_eq!(
        fs::metadata(&prepared.cwd).unwrap().permissions().mode() & 0o777,
        0o700
    );
    fs::create_dir(prepared.cwd.join(".claude")).unwrap();
    fs::write(prepared.cwd.join(".claude/settings.local.json"), "{}").unwrap();
    check(&paths).unwrap();
    prepare_reentry(&paths, &prepared.cwd).unwrap();
    assert!(prepared.cwd.join(".claude/settings.local.json").exists());
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    let mut retained = crate::testkit::agent_state("claude", "ended", now);
    retained.name = Some("different-handle".into());
    retained.worktree_path = Some(prepared.cwd.display().to_string());
    retained.ended_at = Some(now);
    assert!(matches!(
        prepare(&paths, &[&retained], Some("searcher")),
        Err(EmptyDirErr::NameInUse(_))
    ));
    let result = prepare(&paths, &[], Some("searcher"));
    assert!(
        matches!(&result, Err(EmptyDirErr::Leftover(path)) if path == &prepared.cwd),
        "an explicit name must not clear an existing cwd: {result:?}"
    );
    assert!(prepared.cwd.join(".claude/settings.local.json").exists());
    fs::remove_dir_all(prepared.cwd.join(".claude")).unwrap();
    let retry = prepare(&paths, &[], Some("searcher"));
    assert!(
        matches!(&retry, Ok(retried) if retried.cwd == prepared.cwd),
        "an explicit name retries over its own empty leftover: {retry:?}"
    );
    fs::remove_dir_all(&prepared.cwd).unwrap();
    prepare_reentry(&paths, &prepared.cwd).unwrap();
    assert_eq!(
        fs::metadata(&prepared.cwd).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(prepare(&paths, &[], Some("../escape")).is_err());
    retained.name = Some("occupied".into());
    assert!(matches!(
        prepare(&paths, &[&retained], Some("occupied")),
        Err(EmptyDirErr::NameInUse(_))
    ));
    let minted = prepare(&paths, &[&retained], None).unwrap();
    assert!(super::super::petname::valid_agent_name(&minted.name));
    assert_ne!(minted.name, "occupied");
}

#[test]
fn reentry_accepts_resolved_cwd_under_a_symlinked_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let paths = paths(&link);
    let prepared = prepare(&paths, &[], Some("searcher")).unwrap();
    let resolved = prepared.cwd.canonicalize().unwrap();
    fs::remove_dir(&resolved).unwrap();
    let result = prepare_reentry(&paths, &resolved);
    assert!(result.is_ok(), "recreate a resolved cwd: {result:?}");
    assert!(resolved.is_dir());
}

#[test]
fn taken_check_recognizes_resolved_cwd_even_when_missing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let paths = paths(&link);
    let prepared = prepare(&paths, &[], Some("searcher")).unwrap();
    let resolved = prepared.cwd.canonicalize().unwrap();
    fs::remove_dir(&resolved).unwrap();
    let mut retained = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::UNIX_EPOCH);
    retained.name = Some("renamed".into());
    retained.worktree_path = Some(resolved.display().to_string());
    assert!(matches!(
        prepare(&paths, &[&retained], Some("searcher")),
        Err(EmptyDirErr::NameInUse(_))
    ));
}
