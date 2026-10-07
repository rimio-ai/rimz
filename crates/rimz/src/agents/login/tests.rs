use super::*;

fn reset_config() -> crate::config::MachineConfig {
    crate::config::MachineConfig {
        accounts: toml::from_str("[codex.work]\nhome = \"/srv/work\"\n[codex.team]\nhome = \"/srv/team\"\n[use]\ncodex = \"work\"\n").unwrap(),
        ..Default::default()
    }
}

fn project_accounts(root: &Path, value: &str) {
    std::fs::create_dir_all(root.join(".rimz")).unwrap();
    std::fs::write(root.join(".rimz/config.toml"), value).unwrap();
}

#[test]
fn inherited_selection_reports_each_layer() {
    let root = tempfile::tempdir().unwrap();
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let mut config = reset_config();
    assert_eq!(
        inherited_selection(root.path(), &config, &kind).unwrap(),
        ("work".parse().unwrap(), LoginSource::Machine)
    );
    config.accounts.use_accounts.clear();
    assert_eq!(
        inherited_selection(root.path(), &config, &kind).unwrap(),
        (
            crate::ids::LoginName::default_login(),
            LoginSource::Provider
        )
    );
    config
        .accounts
        .use_accounts
        .insert(kind.clone(), "missing".parse().unwrap());
    project_accounts(root.path(), "[accounts]\ncodex = \"team\"\n");
    crate::trust::grant(root.path()).unwrap();
    assert_eq!(
        inherited_selection(root.path(), &config, &kind).unwrap(),
        ("team".parse().unwrap(), LoginSource::Project)
    );
}

#[test]
fn inherited_selection_refuses_untrusted_and_stale_projects_even_for_other_kinds() {
    let root = tempfile::tempdir().unwrap();
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let config = reset_config();
    project_accounts(root.path(), "[accounts]\nclaude = \"default\"\n");
    let error = inherited_selection(root.path(), &config, &kind)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("project account selections in .rimz/config.toml are untrusted")
            && error.contains("rimz trust grant"),
        "{error}"
    );
    crate::trust::grant(root.path()).unwrap();
    project_accounts(root.path(), "[accounts]\nclaude = \"work\"\n");
    let error = inherited_selection(root.path(), &config, &kind)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("project account selections in .rimz/config.toml are stale")
            && error.contains("since your last grant")
            && error.contains("rimz trust grant"),
        "{error}"
    );
}

#[test]
fn inherited_selection_refuses_unreadable_core_before_project_trust() {
    let root = tempfile::tempdir().unwrap();
    project_accounts(root.path(), "[accounts]\nclaude = \"default\"\n");
    let mut config = reset_config();
    config.notices.unreadable_files.insert(
        crate::config::MachineConfig::config_path(),
        "broken TOML".to_owned(),
    );
    let error = inherited_selection(
        root.path(),
        &config,
        &crate::ids::AgentKind::new_unchecked("codex"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("cannot resolve this room's account") && error.contains("broken TOML"),
        "{error}"
    );
}

fn inherited_selection(
    root: &Path,
    machine: &crate::config::MachineConfig,
    kind: &AgentKind,
) -> Result<(LoginName, LoginSource), RoomLoginErr> {
    let account = resolve_room_accounts(&RoomLogins::new(), root, machine).account(kind)?;
    Ok((account.name, account.source))
}

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
fn live_accounts_resolve_each_layer_and_follow_changes_without_moving_pins() {
    let root = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig {
        accounts: accounts(
            "[claude.work]\nhome = \"/srv/claude-work\"\n[codex.work]\nhome = \"/srv/codex-work\"\n[use]\nclaude = \"work\"\ncodex = \"work\"\n",
        ),
        ..Default::default()
    };
    let pins = RoomLogins::from([(kind("claude"), name("default"))]);
    let selected = resolve_room_accounts(&pins, root.path(), &machine);
    assert_eq!(selected.name(&kind("claude")).unwrap(), name("default"));
    assert_eq!(
        selected.source(&kind("claude")).unwrap(),
        LoginSource::Pinned
    );
    assert_eq!(selected.name(&kind("codex")).unwrap(), name("work"));
    assert_eq!(
        selected.source(&kind("codex")).unwrap(),
        LoginSource::Machine
    );
    machine.accounts.use_accounts.clear();
    let selected = resolve_room_accounts(&pins, root.path(), &machine);
    assert_eq!(
        selected.source(&kind("claude")).unwrap(),
        LoginSource::Pinned
    );
    assert_eq!(
        selected.source(&kind("codex")).unwrap(),
        LoginSource::Provider
    );
    std::fs::create_dir(root.path().join(".rimz")).unwrap();
    std::fs::write(
        root.path().join(".rimz/config.toml"),
        "[accounts]\ncodex = \"work\"\n",
    )
    .unwrap();
    crate::trust::grant(root.path()).unwrap();
    let selected = resolve_room_accounts(&pins, root.path(), &machine);
    assert_eq!(selected.name(&kind("codex")).unwrap(), name("work"));
    assert_eq!(
        selected.source(&kind("codex")).unwrap(),
        LoginSource::Project
    );
}

#[test]
fn live_account_refusals_are_per_kind_and_default_pins_need_no_core_or_trust() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".rimz")).unwrap();
    std::fs::write(
        root.path().join(".rimz/config.toml"),
        "[accounts]\ncodex = \"work\"\n",
    )
    .unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let pins = RoomLogins::from([(kind("claude"), name("default"))]);
    let selected = resolve_room_accounts(&pins, root.path(), &machine);
    assert_eq!(selected.name(&kind("claude")).unwrap(), name("default"));
    assert!(
        selected
            .name(&kind("codex"))
            .unwrap_err()
            .to_string()
            .contains("untrusted")
    );
    machine.notices.unreadable_files.insert(
        crate::config::MachineConfig::config_path(),
        "broken TOML".to_owned(),
    );
    let selected = resolve_room_accounts(&pins, root.path(), &machine);
    assert_eq!(selected.name(&kind("claude")).unwrap(), name("default"));
    let unavailable = RoomAccounts::unavailable(selected.name(&kind("codex")).unwrap_err());
    for selection in [&selected, &unavailable] {
        for kind in [kind("codex"), kind("unregistered")] {
            let error = selection.account(&kind).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("broken TOML"), "{message}");
            assert_eq!(
                message
                    .matches("cannot resolve this room's account")
                    .count(),
                1
            );
            let RoomLoginErr::Resolution(inner) = error else {
                panic!("expected a shared resolution error");
            };
            assert!(
                !matches!(inner.as_ref(), RoomLoginErr::Resolution(_)),
                "resolution wrappers must not nest: {inner:?}"
            );
        }
    }
}

#[test]
fn reset_selection_validates_only_the_requested_kind() {
    let root = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig {
        accounts: accounts("[use]\nclaude = \"missing\"\n"),
        ..Default::default()
    };
    let selected = resolve_room_accounts(&RoomLogins::new(), root.path(), &machine);
    assert_eq!(selected.name(&kind("codex")).unwrap(), name("default"));
    let error = selected.name(&kind("claude")).unwrap_err().to_string();
    assert!(error.contains("selected by [accounts.use]"), "{error}");
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
    let set = RoomLoginSet::new(
        Some(RoomLogins::new().into()),
        Some(catalog),
        BTreeMap::new(),
    );
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
fn declared_answers_every_catalog_login_of_a_kind_and_none_without_a_catalog() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[codex.work]\nhome = \"/srv/work\"\n[claude.solo]\nhome = \"/srv/solo\"\n"),
        Some(Path::new("/home/u")),
    )
    .unwrap();
    let set = RoomLoginSet::new(
        Some(RoomLogins::from([(kind("codex"), name("work"))]).into()),
        Some(catalog),
        BTreeMap::new(),
    );
    let declared: Vec<_> = set
        .declared("codex")
        .iter()
        .map(|login| login.key().to_string())
        .collect();
    assert_eq!(declared, ["codex@default", "codex@work"]);
    let unloaded = RoomLoginSet::new(Some(RoomLogins::new().into()), None, BTreeMap::new());
    assert!(unloaded.declared("codex").is_empty());
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
    let config = accounts("[claude.work]\nhome = \"/srv/work\"");
    let selection: RoomAccounts = RoomLogins::from([(kind("claude"), name("work"))]).into();

    let room: Vec<_> = crate::agents::known_kinds()
        .map(|kind_name| selection.login(&kind(kind_name), &config).unwrap())
        .collect();
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
        selection.login(&kind("claude"), &config).unwrap(),
        claude.clone()
    );
    assert!(
        selection
            .login(&kind("codex"), &config)
            .unwrap()
            .is_default()
    );
}

#[test]
fn birth_selection_prefers_requested_over_project() {
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
        .birth_selection(&requested, &project, &RoomLogins::new())
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
            .birth_selection(&RoomLogins::new(), &RoomLogins::new(), &RoomLogins::new())
            .expect("default birth"),
        RoomLogins::from([
            (kind("claude"), LoginName::default_login()),
            (kind("codex"), LoginName::default_login()),
        ])
    );

    let unknown = RoomLogins::from([(kind("claude"), name("travel"))]);
    assert!(matches!(
        catalog.birth_selection(&RoomLogins::new(), &unknown, &RoomLogins::new()),
        Err(BirthLoginErr::Login(LoginErr::Unknown { .. }))
    ));
    let escape = RoomLogins::from([(kind("claude"), LoginName::default_login())]);
    assert_eq!(
        catalog
            .birth_selection(&escape, &unknown, &RoomLogins::new())
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
fn machine_selection_fills_only_unset_kinds() {
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
    let born = catalog.birth_selection(&empty, &empty, &machine).unwrap();
    assert_eq!(born, machine);
    let requested = RoomLogins::from([(kind("claude"), LoginName::default_login())]);
    let project = RoomLogins::from([(kind("codex"), LoginName::default_login())]);
    let overridden = catalog
        .birth_selection(&requested, &project, &machine)
        .unwrap();
    assert!(overridden.values().all(LoginName::is_default));
    let dangling = RoomLogins::from([(kind("codex"), name("missing"))]);
    assert_eq!(
        catalog
            .birth_selection(&project, &empty, &dangling)
            .unwrap()[&kind("codex")],
        LoginName::default_login()
    );
    assert_eq!(
        catalog
            .birth_selection(&empty, &project, &dangling)
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
            .birth_selection(&RoomLogins::new(), &RoomLogins::new(), &machine)
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
            error.contains(&format!("rimz accounts use --global {provider} default")),
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
        Some(RoomLogins::from([(kind("claude"), name("work"))]).into()),
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
        Some(RoomLogins::from([(kind("claude"), name("gone"))]).into()),
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
        message.contains("rimz accounts use --global codex rimio"),
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

#[test]
fn default_health_checks_the_provider_home_and_names_the_hooks_fix() {
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("u");
    let ambient = BTreeMap::from([("HOME".to_owned(), user.to_string_lossy().into_owned())]);
    let default = ProviderLogin::default_for(kind("claude"));

    let missing = default.health(&ambient).unwrap_err();
    assert_eq!(
        AccountStatus::of(Some(&missing)),
        AccountStatus::HomeMissing
    );
    assert!(
        missing
            .to_string()
            .ends_with("run `rimz hooks install claude`"),
        "{missing}"
    );
    std::fs::create_dir_all(user.join(".claude")).unwrap();
    let unhooked = default.health(&ambient).unwrap_err();
    assert_eq!(AccountStatus::of(Some(&unhooked)).as_str(), "hooks missing");
    assert!(
        unhooked
            .to_string()
            .ends_with("run `rimz hooks install claude`"),
        "{unhooked}"
    );
    assert_eq!(default.preflight(&ambient), Ok(()));

    crate::agents::find_definition("claude")
        .unwrap()
        .install_hooks(&ambient)
        .unwrap();
    assert_eq!(default.health(&ambient), Ok(()));
    assert_eq!(AccountStatus::of(None).as_str(), "ready");
    assert_eq!(AccountStatus::LoggedOut.as_str(), "logged out");
    assert_eq!(
        serde_json::to_value(AccountStatus::LoggedOut).unwrap(),
        "logged_out"
    );
    assert_eq!(
        serde_json::to_value(AccountStatus::HooksUntrusted).unwrap(),
        "hooks_untrusted"
    );
}

#[test]
fn native_ambient_drops_only_an_exported_home_naming_a_declared_account() {
    let catalog = LoginCatalog::from_config_under(
        &accounts("[codex.team]\nhome = \"/srv/team\"\n"),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let ambient = |value: &str| {
        BTreeMap::from([
            ("HOME".to_owned(), "/home/u".to_owned()),
            ("CODEX_HOME".to_owned(), value.to_owned()),
        ])
    };
    let default = ProviderLogin::default_for(kind("codex"));

    let native = catalog.native_ambient(&kind("codex"), &ambient("/srv/team"));
    assert_eq!(native.get("CODEX_HOME"), None);
    assert_eq!(
        default.home_dir(&native),
        Some(PathBuf::from("/home/u/.codex"))
    );
    let elsewhere = ambient("/srv/elsewhere");
    assert_eq!(
        catalog.native_ambient(&kind("codex"), &elsewhere),
        elsewhere
    );
    let team = ambient("/srv/team");
    assert_eq!(catalog.native_ambient(&kind("claude"), &team), team);
}

#[test]
fn pool_joins_default_and_shared_accounts_and_isolates_the_rest() {
    let catalog = LoginCatalog::from_config_under(
        &accounts(
            "[claude.work]\nhome = \"/srv/work\"\n[claude.personal]\nhome = \"/srv/personal\"\nhistory = \"shared\"\n[claude.solo]\nhome = \"/srv/solo\"\nhistory = \"standalone\"\n",
        ),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let key = |kind_name: &str, login: &str| LoginKey::new(kind(kind_name), name(login));
    for shared in ["default", "work", "personal"] {
        assert_eq!(
            catalog.pool(&key("claude", shared)),
            LoginKey::default_for(kind("claude")),
            "{shared}"
        );
    }
    assert_eq!(catalog.pool(&key("claude", "solo")), key("claude", "solo"));
    assert_eq!(catalog.pool(&key("claude", "gone")), key("claude", "gone"));
    assert_eq!(catalog.pool(&key("codex", "work")), key("codex", "work"));
    assert_eq!(
        catalog.pool(&key("codex", "default")),
        LoginKey::default_for(kind("codex"))
    );
    for login in catalog.all() {
        assert_eq!(login.pool(), catalog.pool(&login.key()), "{}", login.key());
    }
    let room = RoomLoginSet::new(None, Some(catalog), BTreeMap::new());
    assert_eq!(
        room.pool(&key("claude", "work")),
        LoginKey::default_for(kind("claude"))
    );
    assert_eq!(room.pool(&key("claude", "solo")), key("claude", "solo"));
    // A config that does not load leaves the empty catalog: no login shares.
    let unloaded = LoginCatalog::default();
    assert_eq!(unloaded.pool(&key("claude", "work")), key("claude", "work"));
    let unloaded = RoomLoginSet::new(None, None, BTreeMap::new());
    assert_eq!(unloaded.pool(&key("claude", "work")), key("claude", "work"));
}

#[test]
fn a_shared_codex_login_points_its_databases_at_the_default_home() {
    let catalog = LoginCatalog::from_config_under(
        &accounts(
            "[codex.work]\nhome = \"/srv/work\"\n[codex.solo]\nhome = \"/srv/solo\"\nhistory = \"standalone\"\n[claude.work]\nhome = \"/srv/claude\"\n",
        ),
        Some(Path::new("/home/u")),
    )
    .expect("catalog");
    let ambient = BTreeMap::from([("HOME".to_owned(), "/home/u".to_owned())]);
    let with = |key: &str, value: &str| {
        let mut env = ambient.clone();
        env.insert(key.to_owned(), value.to_owned());
        env
    };
    let work = catalog.select(&kind("codex"), &name("work")).unwrap();
    let solo = catalog.select(&kind("codex"), &name("solo")).unwrap();
    let claude = catalog.select(&kind("claude"), &name("work")).unwrap();
    assert!(work.shares_history());
    assert!(claude.shares_history());
    assert!(!solo.shares_history());
    assert!(!ProviderLogin::default_for(kind("codex")).shares_history());

    let databases = |env: BTreeMap<String, String>| env.get("CODEX_SQLITE_HOME").cloned();
    assert_eq!(
        work.overrides(&ambient),
        BTreeMap::from([
            ("CODEX_HOME".to_owned(), "/srv/work".to_owned()),
            ("CODEX_SQLITE_HOME".to_owned(), "/home/u/.codex".to_owned()),
        ])
    );
    // A pane born on either declared account exports that account's home.
    for exported in ["/srv/work", "/srv/solo"] {
        let pane = with("CODEX_HOME", exported);
        assert_eq!(
            work.default_home(&pane),
            Some(PathBuf::from("/home/u/.codex"))
        );
        assert_eq!(
            databases(work.env(&pane)).as_deref(),
            Some("/home/u/.codex")
        );
    }
    // The user's own default home, and the user's own database home, stand.
    assert_eq!(
        databases(work.env(&with("CODEX_HOME", "/data/codex"))).as_deref(),
        Some("/data/codex")
    );
    assert_eq!(
        databases(work.env(&with("CODEX_SQLITE_HOME", "/data/db"))).as_deref(),
        Some("/data/db")
    );
    assert_eq!(databases(solo.env(&ambient)), None);
    assert_eq!(databases(claude.env(&ambient)), None);
    assert_eq!(
        databases(ProviderLogin::default_for(kind("codex")).env(&ambient)),
        None
    );
}
