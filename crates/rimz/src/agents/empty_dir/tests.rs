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
    assert!(matches!(
        verdict_at(&root, &claude, &env),
        Verdict::Grant(gap) if gap.grant.is_ok() && gap.key == root
    ));
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
    fs::remove_file(temp.path().join("codex.toml")).unwrap();
    assert!(matches!(
        verdict_at(&root, &codex, &env),
        Verdict::Fallback(FolderTrust::Undecided(gap))
            if gap.grant.is_ok() && gap.key == temp.path()
    ));
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
        matches!(
            &trust,
            Some(FolderTrust::Undecided(gap)) if gap.key == home.join("missing/empty")
        ),
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

struct GrantFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    env: BTreeMap<String, String>,
}

impl GrantFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("empty");
        let config = temp.path().join("provider.json");
        fs::write(&config, r#"{"other_writer": 0}"#).unwrap();
        let env = BTreeMap::from([
            ("HOME".into(), temp.path().display().to_string()),
            (
                "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
                config.display().to_string(),
            ),
            ("RIMZ_CODEX_CONFIG".into(), config.display().to_string()),
        ]);
        Self {
            _temp: temp,
            root,
            config,
            env,
        }
    }
}

#[test]
fn grant_retries_several_changes_and_preserves_the_other_writer() {
    let fixture = GrantFixture::new();
    let mut attempts = 0;
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("claude"),
        &fixture.env,
        |gap| {
            attempts += 1;
            if attempts < 3 {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&gap.path).unwrap()).unwrap();
                value["other_writer"] = attempts.into();
                fs::write(&gap.path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            super::super::grant_folder_trust(gap)
        },
    );
    assert!(
        matches!(&result, Ok(Some(gap)) if gap.key == fixture.root),
        "must grant after changed-file retries: {result:?}"
    );
    assert_eq!(attempts, 3);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
    assert_eq!(value["other_writer"], 2);
    assert_eq!(
        value["projects"][fixture.root.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
}

#[test]
fn concurrent_grant_applies_without_reporting_a_write() {
    let fixture = GrantFixture::new();
    let mut attempts = 0;
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("claude"),
        &fixture.env,
        |gap| {
            attempts += 1;
            fs::write(&gap.path, &gap.grant.as_ref().unwrap().candidate).unwrap();
            super::super::grant_folder_trust(gap)
        },
    );
    assert!(
        matches!(&result, Ok(None)),
        "another writer's grant must apply with no grant reported: {result:?}"
    );
    assert_eq!(attempts, 1);
}

#[test]
fn continuously_changed_file_exhausts_three_attempts_and_falls_back() {
    let fixture = GrantFixture::new();
    let mut attempts = 0;
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("claude"),
        &fixture.env,
        |gap| {
            attempts += 1;
            fs::write(
                &gap.path,
                serde_json::json!({"other_writer": attempts}).to_string(),
            )
            .unwrap();
            super::super::grant_folder_trust(gap)
        },
    );
    assert!(
        matches!(
            &result,
            Err(notice) if notice.contains(fixture.config.to_str().unwrap())
                && notice.contains("changed") && notice.contains("3 attempts")
        ),
        "continued changes must name the file and bound: {result:?}"
    );
    assert_eq!(attempts, 3);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
    assert!(value.get("projects").is_none());
}

#[test]
fn failed_grant_write_falls_back_with_the_file_and_error() {
    let fixture = GrantFixture::new();
    let original = fs::read(&fixture.config).unwrap();
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("claude"),
        &fixture.env,
        |gap| {
            Err(crate::disk::atomic::AtomicErr::Io {
                path: gap.path.clone(),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "blocked write"),
            }
            .into())
        },
    );
    assert!(
        matches!(
            &result,
            Err(notice) if notice.contains(fixture.config.to_str().unwrap())
                && notice.contains("blocked write")
        ),
        "write failure must fall back with its cause: {result:?}"
    );
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
}

#[test]
fn saved_untrusted_falls_back_without_calling_the_writer() {
    let fixture = GrantFixture::new();
    fs::write(
        &fixture.config,
        format!(
            "[projects.{}]\ntrust_level = 'untrusted'\n",
            toml::Value::String(fixture.root.display().to_string())
        ),
    )
    .unwrap();
    let original = fs::read(&fixture.config).unwrap();
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("codex"),
        &fixture.env,
        |_| panic!("must not rewrite saved distrust"),
    );
    assert!(
        matches!(
            &result,
            Err(notice) if notice.contains("saved the empty folder as untrusted")
                && notice.contains("rimz doctor")
        ),
        "saved distrust must explain the fallback: {result:?}"
    );
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
}

#[test]
fn non_grantable_falls_back_without_calling_the_writer() {
    let fixture = GrantFixture::new();
    fs::write(&fixture.config, "{ malformed").unwrap();
    let original = fs::read(&fixture.config).unwrap();
    let result = settle_trust(
        &fixture.root,
        &AgentKind::new_unchecked("claude"),
        &fixture.env,
        |_| panic!("must not rewrite malformed trust"),
    );
    assert!(
        matches!(
            &result,
            Err(notice) if notice.contains("cannot write its folder trust")
                && notice.contains("rimz doctor")
        ),
        "non-grantable trust must explain the fallback: {result:?}"
    );
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
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
