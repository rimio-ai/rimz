use super::*;
use std::collections::BTreeMap;

fn gap(kind: &str, config: &Path, cwd: &Path, root: Option<&Path>) -> FolderTrustGap {
    let env = BTreeMap::from([
        ("RIMZ_CODEX_CONFIG".into(), config.display().to_string()),
        (
            "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
            config.display().to_string(),
        ),
    ]);
    let trust = crate::agents::definition_by_kind(kind)
        .unwrap()
        .folder_trust(cwd, root, &env);
    match trust {
        Some(FolderTrust::Undecided(gap)) => gap,
        other => panic!("expected a modeled gap, got {other:?}"),
    }
}

#[test]
fn codex_preview_preserves_config_and_uses_main_root() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let original = "# keep me\nmodel = \"test\"\n\n[projects.\"/other\"]\ntrust_level = \"untrusted\" # choice\n\n[hooks]\n# hook comment\n";
    std::fs::write(&config, original).unwrap();
    let gap = gap(
        "codex",
        &config,
        &dir.path().join("linked"),
        Some(dir.path()),
    );
    let preview = gap.grant.as_ref().unwrap();
    assert_eq!(preview.original.as_deref(), Some(original));
    assert!(preview.candidate.starts_with(original));
    assert_eq!(gap.key, dir.path().canonicalize().unwrap());
    let parsed: toml::Value = toml::from_str(&preview.candidate).unwrap();
    assert_eq!(
        parsed["projects"][gap.key.to_str().unwrap()]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    assert!(gap.fix("codex").contains("rimz trust grant --agents codex"));
}

#[test]
fn codex_missing_file_preview_is_only_the_project_table() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let gap = gap("codex", &config, dir.path(), None);
    let preview = gap.grant.unwrap();
    assert_eq!(preview.original, None);
    assert_eq!(
        preview.candidate,
        format!(
            "[projects.{}]\ntrust_level = \"trusted\"\n",
            toml::Value::String(gap.key.display().to_string())
        )
    );
    assert!(!config.exists());
}

#[test]
fn claude_previews_change_only_the_trust_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".claude.json");
    let key = dir.path().canonicalize().unwrap().display().to_string();
    let quoted = serde_json::to_string(&key).unwrap();
    for projects in [
        String::new(),
        ",\"projects\": {}".into(),
        format!(",\"projects\": {{{quoted}: {{\"other\": 7}}}}"),
        format!(",\"projects\": {{{quoted}: {{\"hasTrustDialogAccepted\": false}}}}"),
    ] {
        let original =
            format!("{{\n  \"z\": [1,  2], \"a\": \"escaped \\\" braces {{}}\"{projects}\n}}\n");
        std::fs::write(&config, &original).unwrap();
        let gap = gap("claude", &config, dir.path(), None);
        let preview = gap.grant.unwrap();
        let mut expected: serde_json::Value = serde_json::from_str(&original).unwrap();
        expected["projects"][&key]["hasTrustDialogAccepted"] = true.into();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&preview.candidate).unwrap(),
            expected
        );
        assert!(
            preview
                .candidate
                .contains("\"z\": [1,  2], \"a\": \"escaped \\\" braces {}\"")
        );
        assert_eq!(preview.original.as_deref(), Some(original.as_str()));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    }
}

#[test]
fn claude_decisions_cover_ancestors_and_main_root_but_not_false() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".claude.json");
    let cwd = dir.path().join("child");
    std::fs::create_dir(&cwd).unwrap();
    let env = BTreeMap::from([(
        "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
        config.display().to_string(),
    )]);
    let adapter = crate::agents::definition_by_kind("claude").unwrap();
    for accepted in [false, true] {
        std::fs::write(&config, serde_json::json!({"projects": {dir.path().to_str().unwrap(): {"hasTrustDialogAccepted": accepted}}}).to_string()).unwrap();
        for root in [None, Some(dir.path())] {
            assert_eq!(
                matches!(
                    adapter.folder_trust(&cwd, root, &env),
                    Some(FolderTrust::Decided)
                ),
                accepted
            );
        }
    }
    assert!(matches!(
        adapter.folder_trust(&cwd, Some(&cwd), &env),
        Some(FolderTrust::Undecided(_))
    ));
}

#[test]
fn claude_unwritable_shapes_and_home_have_no_preview() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".claude.json");
    for text in ["[]", "{", "{\"projects\": false}"] {
        std::fs::write(&config, text).unwrap();
        assert!(gap("claude", &config, dir.path(), None).grant.is_err());
    }
    std::fs::write(&config, "{}").unwrap();
    let env = BTreeMap::from([("HOME".into(), dir.path().display().to_string())]);
    let Some(FolderTrust::Undecided(gap)) = crate::agents::definition_by_kind("claude")
        .unwrap()
        .folder_trust(dir.path(), None, &env)
    else {
        panic!("home must remain undecided")
    };
    assert!(gap.grant.unwrap_err().contains("never persists"));
}

#[test]
fn grants_refuse_changed_bytes_and_non_grantable_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    let mut gap = FolderTrustGap {
        path: path.clone(),
        key: dir.path().into(),
        grant: Ok(FolderTrustPreview {
            original: None,
            candidate: "approved".into(),
        }),
    };
    std::fs::write(&path, "concurrent write").unwrap();
    assert!(matches!(
        grant_folder_trust(&gap),
        Err(FolderTrustErr::Changed { .. })
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "concurrent write");
    gap.grant = Err("repair file".into());
    assert!(matches!(
        grant_folder_trust(&gap),
        Err(FolderTrustErr::NotGrantable(_))
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "concurrent write");
}

#[test]
fn grants_publish_approved_bytes_for_missing_and_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/config");
    let path = dir.path().join("shared-config");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    for original in [None, Some("first".to_owned())] {
        let candidate = if original.is_none() {
            "first"
        } else {
            "second"
        };
        let gap = FolderTrustGap {
            path: path.clone(),
            key: dir.path().into(),
            grant: Ok(FolderTrustPreview {
                original,
                candidate: candidate.into(),
            }),
        };
        grant_folder_trust(&gap).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), candidate);
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), candidate);
    }
}

#[test]
fn rows_require_detection_and_a_model_and_use_the_room_login() {
    use crate::agents::login::LoginCatalog;
    use crate::ids::{AgentKind, RoomLogins};
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("work");
    let accounts = toml::from_str(&format!(
        "[claude.work]\nhome = {}\n[codex.work]\nhome = {}\n",
        toml::Value::String(home.display().to_string()),
        toml::Value::String(home.display().to_string())
    ))
    .unwrap();
    let catalog = LoginCatalog::from_config(&accounts).unwrap();
    let selection = RoomLogins::from([
        (AgentKind::new_unchecked("claude"), "work".parse().unwrap()),
        (AgentKind::new_unchecked("codex"), "work".parse().unwrap()),
    ]);
    for overrides in [false, true] {
        let mut env = BTreeMap::from([(
            "HOME".into(),
            dir.path().join("ambient").display().to_string(),
        )]);
        if overrides {
            env.insert(
                "RIMZ_CODEX_CONFIG".into(),
                dir.path().join("override.toml").display().to_string(),
            );
            env.insert(
                "RIMZ_CLAUDE_GLOBAL_CONFIG".into(),
                dir.path().join("override.json").display().to_string(),
            );
        }
        let logins = RoomLoginSet::new(Some(selection.clone().into()), Some(catalog.clone()), env);
        let rows = rows_with_locator(&logins, dir.path(), dir.path(), |_| {
            Some("/detected".into())
        });
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.login.as_str(), "work");
            let FolderTrust::Undecided(gap) = row.trust else {
                panic!("missing decision")
            };
            let expected = match (row.kind, overrides) {
                ("codex", false) => home.join("config.toml"),
                ("claude", false) => home.join(".claude.json"),
                ("codex", true) => dir.path().join("override.toml"),
                ("claude", true) => dir.path().join("override.json"),
                _ => panic!("unexpected model"),
            };
            assert_eq!(gap.path, expected);
        }
        assert!(rows_with_locator(&logins, dir.path(), dir.path(), |_| None).is_empty());
        let rows = rows_with_locator(&logins, dir.path(), dir.path(), |spec| {
            (spec.kind == "codex").then(|| "/detected".into())
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "codex");
        let native = crate::agents::AgentState::seed(
            AgentKind::new_unchecked("codex"),
            "native".into(),
            crate::agents::AgentStatus::Idle,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let mixed = logins.with_agents(&[native]);
        let rows = rows_with_locator(&mixed, dir.path(), dir.path(), |spec| {
            (spec.kind == "codex").then(|| "/detected".into())
        });
        assert_eq!(
            rows.iter()
                .map(|row| row.login.as_str())
                .collect::<Vec<_>>(),
            ["default", "work"]
        );
    }
}
