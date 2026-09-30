use super::*;

fn kind(value: &str) -> AgentKind {
    AgentKind::new_unchecked(value)
}

fn name(value: &str) -> LoginName {
    value.parse().expect("login name")
}

fn accounts(toml: &str) -> AccountsConfig {
    toml::from_str(toml).expect("accounts config")
}

#[test]
fn session_login_resolves_the_stamp_without_a_room_default() {
    let accounts = accounts("[claude.work]\nhome = \"/srv/work\"\n");
    let login = session_login(&kind("claude"), Some(&name("work")), &accounts).unwrap();
    assert_eq!(login.home(), Some(Path::new("/srv/work")));
    assert!(
        session_login(&kind("claude"), None, &accounts)
            .unwrap()
            .is_default()
    );
    assert!(matches!(
        session_login(&kind("claude"), Some(&name("missing")), &accounts),
        Err(RoomLoginErr::Login(LoginErr::Unknown { .. }))
    ));
}

#[test]
fn room_login_set_includes_only_defaults_and_live_root_stamps() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\nhome = \"/srv/work\"\n[claude.old]\nhome = \"/srv/old\"\n"),
        Some(Path::new("/home/u")),
    )
    .unwrap();
    let set = RoomLoginSet::new(Some(RoomLogins::new()), Some(catalog), BTreeMap::new());
    let mut root = super::super::AgentState::seed(
        kind("claude"),
        "root".into(),
        super::super::AgentStatus::Idle,
        jiff::Timestamp::UNIX_EPOCH,
    );
    root.login = Some(name("work"));
    let mut ended = root.clone();
    ended.login = Some(name("old"));
    ended.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let mut native_child = root.clone();
    native_child.login = Some(name("old"));
    native_child.parent_agent_id = Some("root".into());
    let set = set.with_agents(&[root.clone(), root, ended, native_child]);
    let keys: Vec<_> = set
        .in_use("claude")
        .iter()
        .map(|login| login.key().to_string())
        .collect();
    assert_eq!(keys, ["claude@default", "claude@work"]);
    assert!(
        set.keys_in_use()
            .contains(&LoginKey::new(kind("claude"), name("work")))
    );
    assert!(
        !set.keys_in_use()
            .contains(&LoginKey::new(kind("claude"), name("old")))
    );
    let reset = set.with_agents(&[]);
    assert_eq!(reset.in_use("claude").len(), 1);
    assert!(
        reset
            .keys_in_use()
            .contains(&LoginKey::new(kind("claude"), name("default")))
    );
}

#[test]
fn named_login_overrides_only_the_provider_home_key() {
    let ambient = BTreeMap::from([
        ("HOME".to_owned(), "/home/u".to_owned()),
        ("PATH".to_owned(), "/usr/bin".to_owned()),
    ]);
    for (kind_name, key) in [("claude", "CLAUDE_CONFIG_DIR"), ("codex", "CODEX_HOME")] {
        let login = ProviderLogin::named(kind(kind_name), name("work"), PathBuf::from("/srv/work"))
            .unwrap();
        let env = login.env(&ambient);
        assert_eq!(env.get(key).map(String::as_str), Some("/srv/work"));
        assert_eq!(env.get("PATH"), ambient.get("PATH"));
        assert_eq!(login.home_dir(&ambient), Some(PathBuf::from("/srv/work")));
        assert_eq!(login.key().to_string(), format!("{kind_name}@work"));

        let default = ProviderLogin::default_for(kind(kind_name));
        assert_eq!(default.env(&ambient), ambient);
        assert!(default.is_default());
        assert_eq!(
            default.home_dir(&ambient),
            Some(PathBuf::from(format!("/home/u/.{kind_name}")))
        );
    }
}

#[test]
fn session_login_env_resolves_named_accounts_and_refuses_an_undeclared_one() {
    let ambient = BTreeMap::from([("HOME".to_owned(), "/home/u".to_owned())]);
    let accounts = accounts("[claude.work]\nhome = \"/srv/work\"\n");
    let work = session_login(&kind("claude"), Some(&name("work")), &accounts)
        .expect("declared account")
        .env(&ambient);
    assert_eq!(
        work.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("/srv/work")
    );
    assert_eq!(work.get("HOME"), ambient.get("HOME"));
    assert!(matches!(
        session_login(&kind("claude"), Some(&name("missing")), &accounts),
        Err(RoomLoginErr::Login(LoginErr::Unknown { .. }))
    ));
    assert_eq!(
        session_login_env(&kind("claude"), None).expect("ambient"),
        ambient_env()
    );
}

#[test]
fn a_kind_without_a_home_override_carries_no_named_login() {
    assert_eq!(
        ProviderLogin::named(kind("amp"), name("work"), PathBuf::from("/srv/work")),
        Err(LoginErr::Unsupported { kind: kind("amp") })
    );
}

#[test]
fn catalog_carries_a_default_per_kind_and_every_declared_account() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\nhome = \"/srv/work\"\n[codex.personal]\n"),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");

    let work = catalog.select(&kind("claude"), &name("work")).unwrap();
    assert_eq!(work.home(), Some(Path::new("/srv/work")));
    let personal = catalog.select(&kind("codex"), &name("personal")).unwrap();
    assert_eq!(
        personal.home(),
        Some(default_named_home(&kind("codex"), &name("personal")).as_path())
    );
    assert!(
        catalog
            .select(&kind("claude"), &LoginName::default_login())
            .unwrap()
            .is_default()
    );
    assert_eq!(
        catalog.select(&kind("claude"), &name("missing")),
        Err(LoginErr::Unknown {
            kind: kind("claude"),
            name: name("missing"),
            configured: vec![LoginName::default_login(), name("work")],
        })
    );
    assert_eq!(
        catalog.select(&kind("amp"), &name("work")),
        Err(LoginErr::Unsupported { kind: kind("amp") })
    );
}

#[test]
fn catalog_refuses_reserved_duplicate_relative_and_native_homes() {
    let native = LoginCatalog::from_config_under(
        &accounts("[claude.work]\nhome = \"/home/u/.claude\""),
        Some(Path::new("/home/u")),
    );
    assert!(matches!(native, Err(LoginConfigErr::NativeHome { .. })));

    let reserved = LoginCatalog::from_config_under(
        &accounts("[claude.default]\nhome = \"/srv/work\""),
        Some(Path::new("/home/u")),
    );
    assert!(matches!(reserved, Err(LoginConfigErr::ReservedName { .. })));

    let relative = LoginCatalog::from_config_under(
        &accounts("[codex.work]\nhome = \"relative/home\""),
        Some(Path::new("/home/u")),
    );
    assert!(matches!(relative, Err(LoginConfigErr::RelativeHome { .. })));

    for toml in ["[claude.work]\nhome = \"/srv/a,b\"", "[claude.projects]\n"] {
        let ambiguous =
            LoginCatalog::from_config_under(&accounts(toml), Some(Path::new("/home/u")));
        assert!(matches!(
            ambiguous,
            Err(LoginConfigErr::AmbiguousHome { .. })
        ));
    }

    let duplicate = LoginCatalog::from_config_under(
        &accounts("[claude.a]\nhome = \"/srv/work\"\n[claude.b]\nhome = \"/srv/./work\""),
        Some(Path::new("/home/u")),
    );
    assert!(matches!(
        duplicate,
        Err(LoginConfigErr::DuplicateHome { name, first, .. }) if name.as_str() == "b" && first.as_str() == "a"
    ));
}

#[test]
fn room_selection_names_one_login_per_kind() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\nhome = \"/srv/work\""),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let selection = RoomLogins::from([(kind("claude"), name("work"))]);

    let room = catalog.room(&selection).expect("room logins");
    assert_eq!(room.len(), crate::agents::known_kinds().count());
    let claude = room
        .iter()
        .find(|login| login.kind() == &kind("claude"))
        .unwrap();
    assert_eq!(claude.home(), Some(Path::new("/srv/work")));
    assert!(
        room.iter()
            .filter(|login| login.kind() != &kind("claude"))
            .all(ProviderLogin::is_default)
    );

    assert_eq!(
        catalog.room_login(&selection, &kind("claude")).unwrap(),
        claude.clone()
    );
    assert!(
        catalog
            .room_login(&selection, &kind("codex"))
            .unwrap()
            .is_default()
    );
}

#[test]
fn birth_selection_prefers_requested_over_project_and_keeps_a_frozen_room() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\n[claude.personal]\n[codex.work]\n"),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let requested = RoomLogins::from([(kind("claude"), name("work"))]);
    let project = RoomLogins::from([
        (kind("claude"), name("personal")),
        (kind("codex"), name("work")),
    ]);

    let born = catalog
        .birth_selection(None, &requested, &project, &RoomLogins::new())
        .expect("fresh birth");
    assert_eq!(
        born,
        RoomLogins::from([
            (kind("claude"), name("work")),
            (kind("codex"), name("work"))
        ])
    );
    assert_eq!(
        catalog
            .birth_selection(
                None,
                &RoomLogins::new(),
                &RoomLogins::new(),
                &RoomLogins::new()
            )
            .expect("default birth"),
        RoomLogins::from([
            (kind("claude"), LoginName::default_login()),
            (kind("codex"), LoginName::default_login()),
        ])
    );

    assert_eq!(
        catalog.birth_selection(
            Some(&born),
            &RoomLogins::new(),
            &RoomLogins::new(),
            &RoomLogins::new()
        ),
        Ok(born.clone())
    );
    assert_eq!(
        catalog.birth_selection(Some(&born), &requested, &project, &RoomLogins::new()),
        Ok(born.clone())
    );
    let other = RoomLogins::from([(kind("claude"), name("personal"))]);
    let refused = catalog
        .birth_selection(Some(&born), &other, &RoomLogins::new(), &RoomLogins::new())
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "this room uses claude account `work`, not `personal`; accounts are fixed until reset, so run `rimz reset --account claude=personal`"
    );

    let unknown = RoomLogins::from([(kind("claude"), name("travel"))]);
    assert!(matches!(
        catalog.birth_selection(None, &RoomLogins::new(), &unknown, &RoomLogins::new()),
        Err(BirthLoginErr::Login(LoginErr::Unknown { .. }))
    ));
    let escape = RoomLogins::from([(kind("claude"), LoginName::default_login())]);
    assert_eq!(
        catalog
            .birth_selection(None, &escape, &unknown, &RoomLogins::new())
            .expect("a flag overrides an undeclared project account")
            .get(&kind("claude")),
        Some(&LoginName::default_login())
    );
}

#[test]
fn a_named_account_preflights_its_home_and_hooks() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("work");
    let ambient = BTreeMap::from([(
        "HOME".to_owned(),
        temp.path().join("u").to_string_lossy().into_owned(),
    )]);
    let login = ProviderLogin::named(kind("claude"), name("work"), home.clone()).unwrap();

    assert!(matches!(
        login.preflight(&ambient),
        Err(BirthLoginErr::MissingHome { .. })
    ));
    std::fs::create_dir_all(&home).unwrap();
    let missing = login.preflight(&ambient).unwrap_err();
    assert!(matches!(missing, BirthLoginErr::HooksMissing { .. }));
    assert!(
        missing
            .to_string()
            .ends_with("run `rimz accounts add claude work`")
    );

    crate::agents::find_definition("claude")
        .unwrap()
        .install_hooks(&login.env(&ambient))
        .unwrap();
    assert_eq!(login.preflight(&ambient), Ok(()));
    assert_eq!(
        ProviderLogin::default_for(kind("claude")).preflight(&ambient),
        Ok(())
    );
}

#[test]
fn machine_selection_fills_only_unset_kinds_and_never_changes_frozen_accounts() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\n[codex.work]\n"),
        Some(Path::new("/home/u")),
    )
    .unwrap();
    let empty = RoomLogins::new();
    let machine = RoomLogins::from([
        (kind("claude"), name("work")),
        (kind("codex"), name("work")),
    ]);
    let born = catalog
        .birth_selection(None, &empty, &empty, &machine)
        .unwrap();
    assert_eq!(born, machine);
    let requested = RoomLogins::from([(kind("claude"), LoginName::default_login())]);
    let project = RoomLogins::from([(kind("codex"), LoginName::default_login())]);
    let overridden = catalog
        .birth_selection(None, &requested, &project, &machine)
        .unwrap();
    assert!(overridden.values().all(LoginName::is_default));
    let dangling = RoomLogins::from([(kind("codex"), name("missing"))]);
    assert_eq!(
        catalog
            .birth_selection(Some(&born), &empty, &empty, &dangling)
            .unwrap(),
        born
    );
    assert_eq!(
        catalog
            .birth_selection(None, &project, &empty, &dangling)
            .unwrap()[&kind("codex")],
        LoginName::default_login()
    );
    assert_eq!(
        catalog
            .birth_selection(None, &empty, &project, &dangling)
            .unwrap()[&kind("codex")],
        LoginName::default_login()
    );
}

#[test]
fn deciding_machine_selection_refuses_unknown_names_and_unsupported_kinds() {
    let catalog =
        LoginCatalog::from_config_under(&AccountsConfig::default(), Some(Path::new("/home/u")))
            .unwrap();
    for provider in ["codex", "grok"] {
        let machine = RoomLogins::from([(kind(provider), name("missing"))]);
        let error = catalog
            .birth_selection(None, &RoomLogins::new(), &RoomLogins::new(), &machine)
            .unwrap_err()
            .to_string();
        assert!(error.contains("[accounts.use]"), "{error}");
        // `accounts add` refuses a kind without named accounts, so only a
        // declarable kind names it, and names it once.
        assert_eq!(
            error
                .matches(&format!("rimz accounts add {provider} missing"))
                .count(),
            usize::from(provider == "codex"),
            "{error}"
        );
        assert!(
            error.contains(&format!("rimz accounts use {provider} default")),
            "{error}"
        );
        assert!(
            error.contains(
                &crate::config::MachineConfig::config_path()
                    .display()
                    .to_string()
            ),
            "{error}"
        );
    }
}

#[test]
fn room_login_set_answers_the_room_account_and_nothing_it_cannot_resolve() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[claude.work]\nhome = \"/srv/work\"\n"),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let ambient = BTreeMap::from([("HOME".to_owned(), "/home/u".to_owned())]);
    let set = RoomLoginSet::new(
        Some(RoomLogins::from([(kind("claude"), name("work"))])),
        Some(catalog.clone()),
        ambient.clone(),
    );

    let claude = set.default_login("claude").expect("claude login");
    assert_eq!(claude.key().to_string(), "claude@work");
    assert_eq!(
        set.env(&claude)
            .get("CLAUDE_CONFIG_DIR")
            .map(String::as_str),
        Some("/srv/work")
    );
    assert_eq!(
        set.default_key("codex").map(|key| key.to_string()),
        Some("codex@default".to_owned())
    );

    let removed = RoomLoginSet::new(
        Some(RoomLogins::from([(kind("claude"), name("gone"))])),
        Some(catalog),
        ambient.clone(),
    );
    assert_eq!(removed.default_login("claude"), None);
    assert!(
        removed
            .default_login("codex")
            .is_some_and(|login| login.is_default())
    );

    let unreadable = RoomLoginSet::new(None, None, ambient);
    assert_eq!(unreadable.default_login("codex"), None);
}

#[test]
fn exported_provider_home_naming_an_account_is_refused() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[codex.rimio]\nhome = \"/srv/rimio\"\n[claude.work]\nhome = \"/srv/work\""),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let rimio = catalog.select(&kind("codex"), &name("rimio")).unwrap();
    let work = catalog.select(&kind("claude"), &name("work")).unwrap();
    let ambient = |key: &str, value: &str| {
        BTreeMap::from([
            ("HOME".to_owned(), "/home/u".to_owned()),
            (key.to_owned(), value.to_owned()),
        ])
    };

    assert!(matches!(
        rimio.check_exported_home(&ambient("CODEX_HOME", "/srv/./rimio/")),
        Err(LoginConfigErr::ExportedHome {
            env_key: "CODEX_HOME",
            ..
        })
    ));
    assert!(matches!(
        work.check_exported_home(&ambient("CLAUDE_CONFIG_DIR", "/srv/work")),
        Err(LoginConfigErr::ExportedHome {
            env_key: "CLAUDE_CONFIG_DIR",
            ..
        })
    ));
    let message = rimio
        .check_exported_home(&ambient("CODEX_HOME", "/srv/rimio"))
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("rimz accounts use codex rimio"),
        "{message}"
    );

    assert_eq!(
        rimio.check_exported_home(&ambient("CODEX_HOME", "/srv/other")),
        Ok(())
    );
    assert_eq!(
        rimio.check_exported_home(&ambient("CODEX_HOME", "")),
        Ok(())
    );
    assert_eq!(
        rimio.check_exported_home(&ambient("CLAUDE_CONFIG_DIR", "/srv/rimio")),
        Ok(())
    );
    let default = catalog
        .select(&kind("codex"), &LoginName::default_login())
        .unwrap();
    assert_eq!(
        default.check_exported_home(&ambient("CODEX_HOME", "/srv/rimio")),
        Ok(())
    );
}
