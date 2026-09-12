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
fn mismatch_reads_an_absent_stamp_as_the_default_account() {
    let session = AgentSessionId::from("s-1");
    assert_eq!(
        LoginMismatch::between(&kind("claude"), &session, None, None),
        None
    );
    assert_eq!(
        LoginMismatch::between(
            &kind("claude"),
            &session,
            Some(&LoginName::default_login()),
            None
        ),
        None
    );
    let mismatch = LoginMismatch::between(
        &kind("claude"),
        &session,
        Some(&name("personal")),
        Some(&name("work")),
    )
    .expect("mismatch");
    assert_eq!(
        mismatch.to_string(),
        "cannot resume claude session `s-1`: session account is `personal`, room account is `work`; \
         use a room started with `rimz start --account claude=personal`, or run \
         `rimz reset --account claude=personal` here"
    );
}
