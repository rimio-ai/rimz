use super::*;
use std::collections::BTreeMap;
use std::fs;

fn names(document: &KdlDocument) -> Vec<&str> {
    document
        .nodes()
        .iter()
        .map(|node| node.name().value())
        .collect()
}

#[test]
fn resolves_config_precedence_defaults_verbatim_and_unknown_lists() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config_dir = root.path().join("explicit");
    let xdg = root.path().join("xdg");
    fs::create_dir_all(home.join(".config/zellij")).unwrap();
    fs::create_dir_all(xdg.join("zellij")).unwrap();
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(xdg.join("zellij/config.kdl"), "load_plugins { xdg; }\n").unwrap();
    let mut env = BTreeMap::from([
        ("HOME", home.into_os_string()),
        ("XDG_CONFIG_HOME", xdg.into_os_string()),
    ]);
    let lookup =
        |env: &BTreeMap<&str, OsString>| resolve_with(root.path(), |key| env.get(key).cloned());
    // An existing first directory with no file stops the search.
    let initial = lookup(&env);
    assert!(
        initial.is_some(),
        "missing config resolves to the default list"
    );
    assert_eq!(names(&initial.unwrap()), ["zellij:link"]);
    env.insert("ZELLIJ_CONFIG_DIR", config_dir.clone().into_os_string());
    let source = "// keep aliases, URLs, settings, arguments and comments\n\
        load_plugins {\n\
            zellij:link\n\
            \"https://example.org/plugin.wasm\"\n\
            my_alias cwd=\"/a b\" { setting \"v\\\"x\"; }\n\
        }\n\
        load_plugins { ignored; }\n";
    fs::write(config_dir.join("config.kdl"), source).unwrap();
    let resolved = lookup(&env).unwrap();
    let original: KdlDocument = source.parse().unwrap();
    let expected = original.get("load_plugins").unwrap().children().unwrap();
    assert_eq!(resolved.to_string(), expected.to_string());
    assert_eq!(
        resolved.to_string().parse::<KdlDocument>().unwrap(),
        *expected
    );
    assert_eq!(
        fs::read_to_string(config_dir.join("config.kdl")).unwrap(),
        source
    );
    let explicit = root.path().join("chosen.kdl");
    fs::write(&explicit, "load_plugins { chosen; }\n").unwrap();
    env.insert("ZELLIJ_CONFIG_FILE", "chosen.kdl".into());
    assert_eq!(names(&lookup(&env).unwrap()), ["chosen"]);
    fs::write(&explicit, "show_startup_tips false\n").unwrap();
    assert_eq!(names(&lookup(&env).unwrap()), ["zellij:link"]);
    fs::write(&explicit, "load_plugins {}\n").unwrap();
    assert!(lookup(&env).unwrap().nodes().is_empty());
    fs::write(&explicit, "load_plugins {\n").unwrap();
    assert!(lookup(&env).is_none());
    fs::remove_file(&explicit).unwrap();
    assert!(
        lookup(&env).is_none(),
        "explicit unreadable path is unknown, not defaults"
    );
    fs::create_dir(&explicit).unwrap();
    assert!(
        lookup(&env).is_none(),
        "directory cannot be read as config even as root"
    );
    env.remove("ZELLIJ_CONFIG_FILE");
    env.insert(
        "ZELLIJ_CONFIG_DIR",
        root.path().join("missing").into_os_string(),
    );
    assert_eq!(names(&lookup(&env).unwrap()), ["zellij:link"]);
    #[cfg(not(target_os = "macos"))]
    {
        env.remove("ZELLIJ_CONFIG_DIR");
        fs::remove_dir(root.path().join("home/.config/zellij")).unwrap();
        assert_eq!(names(&lookup(&env).unwrap()), ["xdg"]);
    }
    #[cfg(target_os = "macos")]
    {
        env.remove("ZELLIJ_CONFIG_DIR");
        fs::remove_dir(root.path().join("home/.config/zellij")).unwrap();
        let platform = root
            .path()
            .join("home/Library/Application Support/org.Zellij-Contributors.Zellij");
        fs::create_dir_all(&platform).unwrap();
        fs::write(platform.join("config.kdl"), "load_plugins { mac; }\n").unwrap();
        assert_eq!(names(&lookup(&env).unwrap()), ["mac"]);
    }
}

#[test]
fn birth_spec_environment_overrides_and_removals_beat_inherited_values() {
    let spec = CommandSpec::new("zellij")
        .env("HOME", "/birth")
        .env_remove("HOME")
        .env_remove("ZELLIJ_CONFIG_FILE");
    let inherited = |_: &str| Some(OsString::from("/inherited"));
    assert_eq!(
        effective_env(&spec, "HOME", inherited),
        Some("/birth".into())
    );
    assert_eq!(effective_env(&spec, "ZELLIJ_CONFIG_FILE", inherited), None);
    assert_eq!(
        effective_env(&spec, "XDG_CONFIG_HOME", inherited),
        Some("/inherited".into())
    );
}
