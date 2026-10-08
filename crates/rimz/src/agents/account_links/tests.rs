use super::*;
use std::fs;
use std::os::unix::fs::symlink;

fn names(path: &Path) -> Vec<String> {
    top_level(path).unwrap().into_iter().collect()
}

struct Fixture {
    _temp: tempfile::TempDir,
    named: PathBuf,
    native: PathBuf,
    lock: PathBuf,
    account: LoginKey,
}

impl Fixture {
    fn new(kind: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        fs::create_dir(&named).unwrap();
        Self {
            named,
            native: temp.path().join("native"),
            lock: temp.path().join("account.lock"),
            account: format!("{kind}@work").parse().unwrap(),
            _temp: temp,
        }
    }

    /// A second account of the same kind, linking into the same default home.
    fn sibling(&self, name: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let named = temp.path().join("named");
        fs::create_dir(&named).unwrap();
        Self {
            named,
            native: self.native.clone(),
            lock: self.lock.clone(),
            account: LoginKey::new(self.account.kind.clone(), name.parse().unwrap()),
            _temp: temp,
        }
    }

    fn run_with(&self, shared: bool, agents: Option<usize>) -> Result<ShareReport, ShareErr> {
        self.run_under(shared, agents, DaemonSessions::Clear)
    }

    /// A reconcile with `daemon` answering for the live sessions a daemon
    /// holds under the account home.
    fn run_under(
        &self,
        shared: bool,
        agents: Option<usize>,
        daemon: DaemonSessions,
    ) -> Result<ShareReport, ShareErr> {
        reconcile_homes(
            &Homes {
                adapter: super::super::definition_by_kind(self.account.kind.as_str()).unwrap(),
                account: &self.account,
                named: &self.named,
                default: &self.native,
                shared,
                lock: &self.lock,
                daemon_writes: &|| daemon,
            },
            &|| agents,
        )
    }

    fn run(&self, shared: bool) -> ShareReport {
        self.run_with(shared, Some(0)).unwrap()
    }

    /// The one timestamp directory under the account's set-aside root.
    fn aside(&self) -> PathBuf {
        let root = self.named.join(ASIDE_DIR);
        let [stamp] = &names(&root)[..] else {
            panic!("expected one set-aside directory, found {:?}", names(&root));
        };
        root.join(stamp)
    }
}

#[test]
fn a_fresh_shared_account_links_every_entry_that_is_not_private() {
    let home = Fixture::new("codex");
    fs::create_dir_all(home.native.join("sessions")).unwrap();
    fs::create_dir_all(home.native.join("packages")).unwrap();
    for file in ["history.jsonl", "auth.json", "config.toml"] {
        fs::write(home.native.join(file), file).unwrap();
    }
    let report = home.run(true);
    assert_eq!(
        report.linked,
        [
            "config.toml",
            "AGENTS.md",
            "archived_sessions",
            "history.jsonl",
            "sessions"
        ]
    );
    assert!(report.warnings().is_empty());
    assert_eq!(
        names(&home.named),
        report
            .linked
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    for name in &report.linked {
        assert_eq!(
            fs::read_link(home.named.join(name)).unwrap(),
            home.native.join(name)
        );
    }
    fs::write(home.named.join("sessions/rollout.jsonl"), "turn").unwrap();
    assert_eq!(
        fs::read_to_string(home.native.join("sessions/rollout.jsonl")).unwrap(),
        "turn"
    );

    let before = names(&home.named);
    let again = home.run(true);
    assert!(again.linked.is_empty());
    assert_eq!(again.current, report.linked);
    assert!(again.moved.is_empty() && again.unlinked.is_empty());
    assert_eq!(names(&home.named), before);
    assert_eq!(
        again.to_string(),
        format!("already linked to {}", home.native.display())
    );
}

#[test]
fn an_entry_only_the_account_has_moves_to_the_default_home() {
    let home = Fixture::new("claude");
    fs::create_dir_all(home.named.join("projects/repo")).unwrap();
    fs::write(home.named.join("projects/repo/session.jsonl"), "mine").unwrap();
    fs::write(home.named.join("history.jsonl"), "typed").unwrap();
    let report = home.run(true);
    assert_eq!(report.moved, ["history.jsonl", "projects"]);
    assert!(report.warnings().is_empty());
    for name in ["history.jsonl", "projects"] {
        assert_eq!(
            fs::read_link(home.named.join(name)).unwrap(),
            home.native.join(name)
        );
        assert!(
            !fs::symlink_metadata(home.native.join(name))
                .unwrap()
                .is_symlink()
        );
    }
    assert_eq!(
        fs::read_to_string(home.native.join("projects/repo/session.jsonl")).unwrap(),
        "mine"
    );
    assert!(!home.named.join(ASIDE_DIR).exists());
}

#[test]
fn a_conflict_is_set_aside_and_a_second_one_lands_beside_the_first() {
    let home = Fixture::new("claude");
    fs::create_dir(&home.native).unwrap();
    fs::write(home.native.join("history.jsonl"), "default").unwrap();
    fs::write(home.named.join("history.jsonl"), "first").unwrap();
    symlink("elsewhere", home.named.join("plugins")).unwrap();
    let report = home.run(true);
    let aside = home.aside();
    assert_eq!(
        report.set_aside,
        [
            (home.named.join("plugins"), aside.join("plugins")),
            (
                home.named.join("history.jsonl"),
                aside.join("history.jsonl")
            ),
        ]
    );
    assert_eq!(report.warnings().len(), 2);
    for warning in report.warnings() {
        assert!(report.to_string().contains(&warning), "{report}");
    }
    assert!(
        report.warnings()[1].contains(&aside.join("history.jsonl").display().to_string()),
        "{:?}",
        report.warnings()
    );
    assert_eq!(
        fs::read_to_string(aside.join("history.jsonl")).unwrap(),
        "first"
    );
    // A relative link still names the entry beside the slot it left.
    assert_eq!(
        fs::read_link(aside.join("plugins")).unwrap(),
        home.named.join("elsewhere")
    );
    assert_eq!(
        fs::read_to_string(home.named.join("history.jsonl")).unwrap(),
        "default"
    );
    assert_eq!(
        fs::read_to_string(home.native.join("history.jsonl")).unwrap(),
        "default"
    );

    // A provider that replaces the file by rename leaves a real one again.
    fs::remove_file(home.named.join("history.jsonl")).unwrap();
    fs::write(home.named.join("history.jsonl"), "second").unwrap();
    let again = home.run(true);
    let [(_, second)] = &again.set_aside[..] else {
        panic!("expected one set-aside entry, got {:?}", again.set_aside);
    };
    assert_ne!(second.parent(), Some(aside.as_path()));
    assert_eq!(fs::read_to_string(second).unwrap(), "second");
    assert_eq!(
        fs::read_to_string(aside.join("history.jsonl")).unwrap(),
        "first"
    );
}

#[test]
fn private_names_and_their_dotted_siblings_stay_real() {
    for (kind, private) in [
        (
            "codex",
            &[
                "auth.json",
                "auth.json.bak",
                "app-server-control",
                "packages",
                "models_cache.json",
                "models_cache.json.bak",
            ][..],
        ),
        (
            "claude",
            &[
                ".credentials.json",
                ".claude.json",
                ".claude.json.backup",
                ".oauth_refresh.lock",
                ".last-update-result.json",
            ][..],
        ),
    ] {
        let home = Fixture::new(kind);
        fs::create_dir(&home.native).unwrap();
        fs::create_dir_all(home.named.join(ASIDE_DIR).join("old")).unwrap();
        for name in private {
            fs::write(home.native.join(name), "default").unwrap();
            fs::write(home.named.join(name), "account").unwrap();
        }
        fs::write(home.native.join("auth.jsonl"), "not private").unwrap();
        let report = home.run(true);
        assert!(report.set_aside.is_empty(), "{kind}");
        assert!(report.linked.contains(&"auth.jsonl".to_owned()), "{kind}");
        for name in private {
            assert!(!report.linked.iter().any(|linked| linked == name), "{kind}");
            assert_eq!(
                fs::read_to_string(home.named.join(name)).unwrap(),
                "account"
            );
            assert_eq!(
                fs::read_to_string(home.native.join(name)).unwrap(),
                "default"
            );
        }
        assert_eq!(names(&home.named.join(ASIDE_DIR)), ["old"]);
        assert!(!home.native.join(ASIDE_DIR).exists());
    }
}

#[test]
fn switching_to_standalone_unlinks_history_and_keeps_settings() {
    let home = Fixture::new("codex");
    fs::create_dir_all(home.native.join("sessions")).unwrap();
    fs::write(home.native.join("history.jsonl"), "typed").unwrap();
    fs::write(home.named.join("auth.json"), "token").unwrap();
    symlink("/elsewhere/notes", home.named.join("notes")).unwrap();
    home.run(true);
    fs::remove_file(home.named.join("notes")).unwrap();
    symlink("/elsewhere/notes", home.named.join("notes")).unwrap();

    let report = home.run(false);
    assert_eq!(
        report.unlinked,
        ["archived_sessions", "history.jsonl", "sessions"]
    );
    assert_eq!(report.current, ["config.toml", "AGENTS.md"]);
    assert_eq!(
        names(&home.named),
        [ASIDE_DIR, "AGENTS.md", "auth.json", "config.toml", "notes"]
    );
    assert!(home.native.join("sessions").is_dir());
    assert_eq!(
        fs::read_to_string(home.native.join("history.jsonl")).unwrap(),
        "typed"
    );
    assert_eq!(
        report.to_string(),
        format!(
            "unlinked from {home}: archived_sessions, history.jsonl, sessions\nalready linked to {home}",
            home = home.native.display()
        )
    );
}

#[test]
fn concurrent_reconciles_leave_one_consistent_home() {
    let home = Fixture::new("claude");
    fs::create_dir_all(home.native.join("projects")).unwrap();
    fs::write(home.native.join("history.jsonl"), "default").unwrap();
    fs::write(home.named.join("history.jsonl"), "account").unwrap();
    let reports: Vec<ShareReport> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..4).map(|_| scope.spawn(|| home.run(true))).collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    let set_aside: Vec<_> = reports
        .iter()
        .flat_map(|report| &report.set_aside)
        .collect();
    assert_eq!(set_aside.len(), 1, "{set_aside:?}");
    assert_eq!(
        reports
            .iter()
            .filter(|report| report.linked.iter().any(|name| name == "projects"))
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(home.aside().join("history.jsonl")).unwrap(),
        "account"
    );
    for name in ["history.jsonl", "projects"] {
        assert_eq!(
            fs::read_link(home.named.join(name)).unwrap(),
            home.native.join(name)
        );
    }
}

#[test]
fn a_directory_conflict_under_live_agents_refuses_before_any_change() {
    let home = Fixture::new("claude");
    fs::create_dir_all(home.native.join("projects")).unwrap();
    fs::create_dir_all(home.named.join("projects")).unwrap();
    fs::write(home.named.join("projects/session.jsonl"), "live").unwrap();
    fs::write(home.native.join("history.jsonl"), "default").unwrap();
    let before = names(&home.named);
    for agents in [Some(2), None] {
        let error = home.run_with(true, agents).unwrap_err();
        assert!(
            matches!(&error, ShareErr::LiveAgents { entry, .. } if entry == "projects"),
            "{error}"
        );
        let text = error.to_string();
        assert!(text.contains("claude@work"), "{text}");
        assert!(text.contains("`projects`"), "{text}");
        assert!(
            text.contains("set `history = \"standalone\"` under `[accounts.claude.work]`"),
            "{text}"
        );
        assert_eq!(text.contains("2 live agent(s)"), agents.is_some(), "{text}");
        assert_eq!(names(&home.named), before);
    }

    let report = home.run_with(true, Some(0)).unwrap();
    assert_eq!(
        report.set_aside,
        [(home.named.join("projects"), home.aside().join("projects"))]
    );
    assert_eq!(
        fs::read_to_string(home.aside().join("projects/session.jsonl")).unwrap(),
        "live"
    );
}

#[test]
fn a_file_conflict_never_asks_about_live_agents() {
    let home = Fixture::new("claude");
    fs::create_dir(&home.native).unwrap();
    fs::write(home.native.join("history.jsonl"), "default").unwrap();
    fs::write(home.named.join("history.jsonl"), "account").unwrap();
    let report = reconcile_homes(
        &Homes {
            adapter: super::super::definition_by_kind("claude").unwrap(),
            account: &home.account,
            named: &home.named,
            default: &home.native,
            shared: true,
            lock: &home.lock,
            daemon_writes: &|| panic!("a file moves aside under any daemon"),
        },
        &|| -> Option<usize> { panic!("a file moves aside under any agent") },
    )
    .unwrap();
    assert_eq!(report.set_aside.len(), 1);
}

#[test]
fn a_launch_from_a_pane_on_the_account_links_to_the_providers_own_home() {
    let temp = tempfile::tempdir().unwrap();
    let named = temp.path().join("work");
    let native = temp.path().join("home/.codex");
    fs::create_dir_all(native.join("sessions")).unwrap();
    fs::create_dir(&named).unwrap();
    let accounts: crate::config::AccountsConfig = toml::from_str(&format!(
        "[codex.work]\nhome = {:?}\n",
        named.display().to_string()
    ))
    .unwrap();
    let login = crate::agents::LoginCatalog::from_config(&accounts)
        .unwrap()
        .select(&AgentKind::new_unchecked("codex"), &"work".parse().unwrap())
        .unwrap();
    let ambient = BTreeMap::from([
        (
            "HOME".to_owned(),
            temp.path().join("home").display().to_string(),
        ),
        ("CODEX_HOME".to_owned(), named.display().to_string()),
    ]);
    let report = reconcile(&login, &ambient, &|| Some(0)).unwrap().unwrap();
    assert_eq!(report.default_home, native);
    assert_eq!(
        fs::read_link(named.join("sessions")).unwrap(),
        native.join("sessions")
    );
    assert!(
        reconcile(
            &ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
            &ambient,
            &|| Some(0)
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn migration_removes_owned_skill_links_and_preserves_user_entries() {
    for user_entry in [false, true] {
        let home = Fixture::new("claude");
        let skills = home.named.join("skills");
        fs::create_dir_all(&skills).unwrap();
        symlink(
            crate::disk::paths::skills_library().join("owned"),
            skills.join("owned"),
        )
        .unwrap();
        if user_entry {
            fs::create_dir(skills.join("user")).unwrap();
        }
        fs::create_dir_all(home.native.join("skills")).unwrap();
        home.run(false);
        assert_eq!(fs::read_link(&skills).unwrap(), home.native.join("skills"));
        let expected: &[&str] = if user_entry { &["user"] } else { &[] };
        assert_eq!(names(&home.aside().join("skills")), expected);
    }
}

#[test]
fn a_standalone_account_shares_settings_alone_and_rerun_changes_nothing() {
    for (kind, files, dirs) in [
        ("codex", &["config.toml", "AGENTS.md"][..], &[][..]),
        (
            "claude",
            &["settings.json", "settings.local.json", "CLAUDE.md"][..],
            &["skills", "plugins", "agents", "commands", "output-styles"][..],
        ),
    ] {
        let home = Fixture::new(kind);
        let (named, native) = (&home.named, &home.native);
        fs::create_dir_all(native.join("sessions")).unwrap();
        fs::create_dir_all(named.join("projects")).unwrap();
        let report = home.run(false);
        assert_eq!(report.linked.len(), files.len() + dirs.len());
        assert!(report.current.is_empty());
        assert!(report.moved.is_empty());
        assert!(named.join("projects").is_dir());
        assert!(!named.join("sessions").exists());
        for name in files.iter().chain(dirs) {
            assert_eq!(fs::read_link(named.join(name)).unwrap(), native.join(name));
            assert_eq!(native.join(name).is_dir(), dirs.contains(name));
            if files.contains(name) {
                assert!(
                    !native.join(name).exists(),
                    "missing files stay unconfigured"
                );
            }
        }
        let before = names(named);
        let again = home.run(false);
        assert!(again.linked.is_empty());
        assert_eq!(again.current, report.linked);
        assert!(again.set_aside.is_empty());
        assert!(again.notes.is_empty());
        assert_eq!(names(named), before);
        assert_eq!(
            again.to_string(),
            format!("already linked to {}", native.display())
        );
    }
}

#[test]
fn relative_links_to_the_native_entries_are_current() {
    let home = Fixture::new("codex");
    for name in ["config.toml", "AGENTS.md"] {
        symlink(Path::new("../native").join(name), home.named.join(name)).unwrap();
    }
    let report = home.run(false);
    assert_eq!(report.current, ["config.toml", "AGENTS.md"]);
    assert!(report.linked.is_empty());
    assert_eq!(
        fs::read_link(home.named.join("config.toml")).unwrap(),
        Path::new("../native/config.toml")
    );
}

#[test]
fn same_home_through_symlink_refuses_without_changes() {
    let temp = tempfile::tempdir().unwrap();
    let native = temp.path().join("native");
    let named = temp.path().join("named");
    fs::create_dir(&native).unwrap();
    symlink(&native, &named).unwrap();
    let account: LoginKey = "codex@work".parse().unwrap();
    let error = reconcile_homes(
        &Homes {
            adapter: super::super::definition_by_kind("codex").unwrap(),
            account: &account,
            named: &named,
            default: &native,
            shared: true,
            lock: &temp.path().join("account.lock"),
            daemon_writes: &|| DaemonSessions::Clear,
        },
        &|| Some(0),
    )
    .unwrap_err();
    assert!(matches!(error, ShareErr::SameHome { .. }));
    assert!(names(&native).is_empty());
    assert_eq!(fs::read_link(named).unwrap(), native);
}

#[test]
fn a_shared_account_links_history_neither_home_holds_yet() {
    for (kind, history) in [
        ("claude", &["projects"][..]),
        ("codex", &["sessions", "archived_sessions"][..]),
    ] {
        let home = Fixture::new(kind);
        let report = home.run(true);
        for name in history {
            assert!(report.linked.iter().any(|linked| linked == name), "{kind}");
            assert_eq!(
                fs::read_link(home.named.join(name)).unwrap(),
                home.native.join(name),
                "{kind}"
            );
            assert!(home.native.join(name).is_dir(), "{kind}");
        }

        let solo = home.sibling("solo");
        solo.run(false);
        for name in history {
            assert!(
                fs::symlink_metadata(solo.named.join(name)).is_err(),
                "{kind}"
            );
        }
    }
}

#[test]
fn unlinking_a_directory_under_live_agents_refuses_and_leaves_the_link() {
    let home = Fixture::new("codex");
    fs::create_dir_all(home.native.join("sessions")).unwrap();
    fs::write(home.native.join("history.jsonl"), "typed").unwrap();
    home.run(true);
    let before = names(&home.named);
    for agents in [Some(1), None] {
        let error = home.run_with(false, agents).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("cannot unlink `"), "{text}");
        assert!(text.contains("codex@work"), "{text}");
        assert!(
            text.contains("remove `history = \"standalone\"` from `[accounts.codex.work]`"),
            "{text}"
        );
        assert_eq!(text.contains("1 live agent(s)"), agents.is_some(), "{text}");
        assert_eq!(names(&home.named), before);
        assert_eq!(
            fs::read_link(home.named.join("sessions")).unwrap(),
            home.native.join("sessions")
        );
    }
    assert!(!home.run_with(false, Some(0)).unwrap().unlinked.is_empty());
}

#[test]
fn a_link_to_a_directory_elsewhere_is_not_set_aside_under_live_agents() {
    let home = Fixture::new("claude");
    let elsewhere = home.named.parent().unwrap().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, home.named.join("projects")).unwrap();
    let error = home.run_with(true, Some(1)).unwrap_err();
    assert!(
        matches!(&error, ShareErr::LiveAgents { entry, .. } if entry == "projects"),
        "{error}"
    );
    assert_eq!(
        fs::read_link(home.named.join("projects")).unwrap(),
        elsewhere
    );
}

#[test]
fn a_failure_after_a_set_aside_still_names_what_moved() {
    let home = Fixture::new("claude");
    fs::create_dir(&home.native).unwrap();
    for dir in [&home.native, &home.named] {
        fs::write(dir.join("CLAUDE.md"), "memory").unwrap();
    }
    fs::write(home.native.join("skills"), "not a directory").unwrap();
    let text = home.run_with(false, Some(0)).unwrap_err().to_string();
    let aside = home.aside().join("CLAUDE.md");
    assert!(aside.is_file());
    assert!(text.contains("skills"), "{text}");
    assert!(
        text.contains(&format!(
            "moved {} aside to {}",
            home.named.join("CLAUDE.md").display(),
            aside.display()
        )),
        "{text}"
    );
}

#[test]
fn two_accounts_moving_one_name_into_the_default_home_overwrite_nothing() {
    let work = Fixture::new("codex");
    let team = work.sibling("team");
    assert_eq!(
        crate::disk::paths::account_lock(&work.account.kind),
        crate::disk::paths::account_lock(&team.account.kind)
    );
    assert_ne!(
        crate::disk::paths::account_lock(&work.account.kind),
        crate::disk::paths::account_lock(&AgentKind::new_unchecked("claude"))
    );
    fs::write(work.named.join("history.jsonl"), "work").unwrap();
    fs::write(team.named.join("history.jsonl"), "team").unwrap();
    let reports: Vec<ShareReport> = std::thread::scope(|scope| {
        let runs: Vec<_> = [&work, &team, &work, &team]
            .map(|home| scope.spawn(|| home.run(true)))
            .into_iter()
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    let moved = reports
        .iter()
        .filter(|report| report.moved.iter().any(|name| name == "history.jsonl"))
        .count();
    let set_aside: Vec<_> = reports
        .iter()
        .flat_map(|report| &report.set_aside)
        .collect();
    assert_eq!((moved, set_aside.len()), (1, 1), "{set_aside:?}");
    let kept = fs::read_to_string(work.native.join("history.jsonl")).unwrap();
    let aside = fs::read_to_string(&set_aside[0].1).unwrap();
    let mut both = [kept.as_str(), aside.as_str()];
    both.sort_unstable();
    assert_eq!(both, ["team", "work"]);
}

/// A reconcile that fails the test if it asks about live agents.
fn run_unguarded(home: &Fixture, shared: bool) -> ShareReport {
    reconcile_homes(
        &Homes {
            adapter: super::super::definition_by_kind(home.account.kind.as_str()).unwrap(),
            account: &home.account,
            named: &home.named,
            default: &home.native,
            shared,
            lock: &home.lock,
            daemon_writes: &|| panic!("an unshared name never waits on a daemon"),
        },
        &|| -> Option<usize> { panic!("an unshared name never waits on live agents") },
    )
    .unwrap()
}

#[test]
fn only_dotted_lock_and_tmp_components_are_unshared() {
    let codex = super::super::definition_by_kind("codex").unwrap();
    let claude = super::super::definition_by_kind("claude").unwrap();
    for name in [
        ".oauth_refresh.lock",
        ".oauth_refresh.lock.owner",
        ".sqlite-maintenance.lock",
        "session_index.jsonl.tmp",
        "settings.json.tmp.123.ab",
    ] {
        assert!(is_unshared(codex, name), "{name}");
        assert!(is_unshared(claude, name), "{name}");
    }
    assert!(is_unshared(claude, ".last-update-result.json"));
    assert!(!is_unshared(codex, ".last-update-result.json"));
    for name in [
        ".tmp",
        "tmp",
        "thread-writer-locks",
        "mcp-oauth-locks",
        ".lockfile",
        "notes.locked",
        ".lock",
    ] {
        assert!(!is_unshared(codex, name), "{name}");
        assert!(!is_unshared(claude, name), "{name}");
    }
}

#[test]
fn a_lock_or_temp_file_in_either_home_is_never_linked() {
    let launch = Fixture::new("codex");
    fs::create_dir_all(launch.native.join(".oauth_refresh.lock")).unwrap();
    fs::create_dir_all(launch.native.join("thread-writer-locks")).unwrap();
    fs::create_dir_all(launch.native.join(".tmp")).unwrap();
    for file in [".sqlite-maintenance.lock", "session_index.jsonl.tmp"] {
        fs::write(launch.native.join(file), file).unwrap();
    }
    let report = launch.run_with(true, Some(2)).unwrap();
    assert!(report.warnings().is_empty(), "{report}");
    for name in [
        ".oauth_refresh.lock",
        ".sqlite-maintenance.lock",
        "session_index.jsonl.tmp",
    ] {
        assert!(
            fs::symlink_metadata(launch.named.join(name)).is_err(),
            "{name}"
        );
    }
    for name in ["thread-writer-locks", ".tmp"] {
        assert_eq!(
            fs::read_link(launch.named.join(name)).unwrap(),
            launch.native.join(name)
        );
    }

    let add = Fixture::new("claude");
    fs::create_dir(add.named.join(".oauth_refresh.lock")).unwrap();
    let report = run_unguarded(&add, true);
    assert!(report.moved.is_empty() && report.warnings().is_empty());
    assert!(
        fs::symlink_metadata(add.named.join(".oauth_refresh.lock"))
            .unwrap()
            .is_dir()
    );
    assert!(!add.native.join(".oauth_refresh.lock").exists());
}

#[test]
fn a_real_lock_in_both_homes_is_no_conflict_in_either_mode() {
    for shared in [true, false] {
        let home = Fixture::new("claude");
        for dir in [&home.native, &home.named] {
            fs::create_dir_all(dir.join(".oauth_refresh.lock")).unwrap();
        }
        let report = run_unguarded(&home, shared);
        assert!(report.set_aside.is_empty(), "{shared}");
        assert!(report.warnings().is_empty(), "{shared}");
        assert!(!home.named.join(ASIDE_DIR).exists(), "{shared}");
        for dir in [&home.native, &home.named] {
            assert!(
                fs::symlink_metadata(dir.join(".oauth_refresh.lock"))
                    .unwrap()
                    .is_dir(),
                "{shared}"
            );
        }
    }
}

#[test]
fn a_link_at_an_unshared_name_is_removed_under_any_live_count() {
    for (kind, shared, agents) in [
        ("claude", true, Some(2)),
        ("claude", true, None),
        ("claude", false, Some(2)),
        ("claude", false, None),
        ("codex", true, Some(2)),
        ("codex", true, None),
        ("codex", false, Some(2)),
        ("codex", false, None),
    ] {
        let home = Fixture::new(kind);
        let private_name = if kind == "codex" {
            "models_cache.json"
        } else {
            ".last-update-result.json"
        };
        fs::create_dir_all(home.native.join(".oauth_refresh.lock")).unwrap();
        fs::write(home.native.join(private_name), "update").unwrap();
        let links = [
            (".oauth_refresh.lock", true),
            (private_name, false),
            (".oauth_refresh.lock.owner", true),
            ("settings.json.tmp.123.ab", false),
        ];
        for (name, absolute) in links {
            let target = if absolute {
                home.native.join(name)
            } else {
                Path::new("../native").join(name)
            };
            symlink(target, home.named.join(name)).unwrap();
        }

        let report = home.run_with(shared, agents).unwrap();
        let warnings = report.warnings();
        assert_eq!(warnings.len(), links.len(), "{warnings:?}");
        for (name, _) in links {
            assert!(
                fs::symlink_metadata(home.named.join(name)).is_err(),
                "{name}"
            );
            let slot = home.named.join(name).display().to_string();
            let warning = warnings
                .iter()
                .find(|warning| warning.contains(&format!("{slot} ")))
                .unwrap_or_else(|| panic!("no warning names {slot}: {warnings:?}"));
            assert!(warning.contains(&format!("{kind}@work")), "{warning}");
            assert!(
                warning.contains(&home.native.display().to_string()),
                "{warning}"
            );
            assert!(report.to_string().contains(warning), "{report}");
        }
        assert!(report.unlinked.is_empty(), "{:?}", report.unlinked);
        assert!(home.native.join(".oauth_refresh.lock").is_dir());
        assert_eq!(
            fs::read_to_string(home.native.join(private_name)).unwrap(),
            "update"
        );

        let before = names(&home.named);
        let again = home.run_with(shared, agents).unwrap();
        assert!(again.warnings().is_empty(), "{again}");
        assert!(again.linked.is_empty() && again.unlinked.is_empty());
        assert_eq!(names(&home.named), before);
    }
}

#[test]
fn a_link_at_an_unshared_name_that_points_elsewhere_survives() {
    for shared in [true, false] {
        let home = Fixture::new("claude");
        let elsewhere = home.named.parent().unwrap().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(home.native.join(".oauth_refresh.lock")).unwrap();
        symlink(&elsewhere, home.named.join(".oauth_refresh.lock")).unwrap();
        let report = run_unguarded(&home, shared);
        assert!(report.warnings().is_empty(), "{report}");
        assert_eq!(
            fs::read_link(home.named.join(".oauth_refresh.lock")).unwrap(),
            elsewhere
        );
    }
}

/// Every entry of a home with what it is: a link's target, a file's bytes.
fn contents(home: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![home.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            let what = if kind.is_symlink() {
                format!("-> {}", fs::read_link(&path).unwrap().display())
            } else if kind.is_dir() {
                pending.push(path.clone());
                "dir".to_owned()
            } else {
                fs::read_to_string(&path).unwrap()
            };
            found.push((path, what));
        }
    }
    found.sort();
    found
}

fn live(sessions: usize) -> DaemonSessions {
    DaemonSessions::Live(NonZeroUsize::new(sessions).unwrap())
}

const TOGGLE: &str =
    "`rimz config set remote_control.codex false`, rerun, then set it back to `true`";

#[test]
fn a_daemon_refuses_a_directory_set_aside_only_while_it_holds_or_hides_sessions() {
    let home = Fixture::new("codex");
    fs::create_dir_all(home.native.join("sessions")).unwrap();
    fs::create_dir_all(home.named.join("sessions/2026")).unwrap();
    fs::write(home.named.join("sessions/2026/rollout.jsonl"), "live").unwrap();
    fs::write(home.named.join("history.jsonl"), "account").unwrap();
    fs::write(home.native.join("history.jsonl"), "default").unwrap();
    let before = contents(&home.named);
    let native = home.native.display();

    let error = home.run_under(true, Some(0), live(2)).unwrap_err();
    assert!(
        matches!(&error, ShareErr::LiveDaemon { entry, .. } if entry == "sessions"),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "cannot share `sessions` of codex@work with {native}: the remote-control daemon on the account holds 2 live session(s) that write to the copy in its home, which would move aside under them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with {TOGGLE}, or set `history = \"standalone\"` under `[accounts.codex.work]`"
        )
    );
    let error = home
        .run_under(true, Some(0), DaemonSessions::Unknown)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "cannot share `sessions` of codex@work with {native}: the remote-control daemon on the account did not report its sessions, so it may write to the copy in its home, which would move aside under it; stop it with {TOGGLE}, or set `history = \"standalone\"` under `[accounts.codex.work]`"
        )
    );
    assert_eq!(contents(&home.named), before);

    // Agents are asked first, so their refusal stands under any daemon answer.
    for agents in [Some(1), None] {
        for daemon in [DaemonSessions::Clear, live(1), DaemonSessions::Unknown] {
            let error = home.run_under(true, agents, daemon).unwrap_err();
            assert!(matches!(error, ShareErr::LiveAgents { .. }), "{error}");
        }
    }
    assert_eq!(contents(&home.named), before);

    // A daemon that holds no session does not block the move.
    let report = home
        .run_under(true, Some(0), DaemonSessions::Clear)
        .unwrap();
    assert!(
        report
            .set_aside
            .contains(&(home.named.join("sessions"), home.aside().join("sessions")))
    );
}

#[test]
fn a_daemon_refuses_the_unlink_of_a_directory_only_while_it_holds_or_hides_sessions() {
    let home = Fixture::new("codex");
    fs::create_dir_all(home.native.join("sessions")).unwrap();
    fs::write(home.native.join("history.jsonl"), "typed").unwrap();
    home.run(true);
    let before = contents(&home.named);
    let native = home.native.display();

    let error = home.run_under(false, Some(0), live(1)).unwrap_err();
    let ShareErr::LiveDaemonUnlink { entry, .. } = &error else {
        panic!("{error}");
    };
    assert_eq!(
        error.to_string(),
        format!(
            "cannot unlink `{entry}` of codex@work from {native}: the remote-control daemon on the account holds 1 live session(s) that write through that link, which would be removed under them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with {TOGGLE}, or remove `history = \"standalone\"` from `[accounts.codex.work]`"
        )
    );
    let error = home
        .run_under(false, Some(0), DaemonSessions::Unknown)
        .unwrap_err();
    let ShareErr::LiveDaemonUnlink { entry, .. } = &error else {
        panic!("{error}");
    };
    assert_eq!(
        error.to_string(),
        format!(
            "cannot unlink `{entry}` of codex@work from {native}: the remote-control daemon on the account did not report its sessions, so it may write through that link, which would be removed under it; stop it with {TOGGLE}, or remove `history = \"standalone\"` from `[accounts.codex.work]`"
        )
    );
    assert_eq!(contents(&home.named), before);
    for agents in [Some(1), None] {
        for daemon in [DaemonSessions::Clear, live(1), DaemonSessions::Unknown] {
            let error = home.run_under(false, agents, daemon).unwrap_err();
            assert!(
                matches!(error, ShareErr::LiveAgentsUnlink { .. }),
                "{error}"
            );
        }
    }
    assert_eq!(contents(&home.named), before);

    assert!(
        !home
            .run_under(false, Some(0), DaemonSessions::Clear)
            .unwrap()
            .unlinked
            .is_empty()
    );
}
