use super::*;

#[test]
fn restarting_peer_fails_only_its_open_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
    let store = rimz::Store::open(
        rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
        rimz::RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let mut peer = rimz::testkit::agent_state("claude", "peer", jiff::Timestamp::now());
    peer.launch_id = Some("peer-launch".into());
    peer.launched_by = Some(rimz::agents::LaunchedBy {
        kind: peer.kind.clone(),
        agent_id: "launcher".into(),
    });
    let adapter = rimz::agents::find_definition("claude").unwrap();
    let record = rimz::harness::run::create_peer_prompt(
        store.paths(),
        &peer,
        None,
        adapter,
        "task",
        dir.path(),
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .unwrap();
    rimz::harness::run::record_lifecycle(
        store.paths(),
        &record.run_id,
        "claude",
        &rimz::agents::AgentLifecycleObservation::new(
            Some(peer.agent_id.clone()),
            rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
        ),
        None,
        || None,
    )
    .unwrap();
    // The seat's team run stays open for the session that holds it next.
    let mut team = rimz::store::run::RunRecord::new(
        store.paths().workspace_id.clone(),
        peer.kind.clone(),
        PermissionMode::Auto,
        "ship the feature".to_owned(),
        dir.path().to_path_buf(),
    );
    team.team = Some(rimz::store::run::TeamRun {
        launch_id: "peer-launch".into(),
        instance: "forge#x".parse().unwrap(),
    });
    rimz::harness::run::create(store.paths(), &team).unwrap();
    settle_peer_before_restart(&store, &peer).unwrap();
    let failed = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(failed.status, rimz::store::run::RunStatus::Failed);
    assert_eq!(failed.failure_tail.as_deref(), Some("peer restarted"));
    assert!(
        failed.report_message_id.is_none(),
        "failed turn remains eligible for the fleet reporter"
    );
    settle_peer_before_restart(&store, &peer).unwrap();
    let mut runs = rimz::harness::run::list(store.paths()).unwrap();
    runs.sort_by_key(|run| run.team.is_some());
    assert_eq!(runs, vec![failed, team]);
}

#[test]
fn resume_classification_names_each_fresh_reason() {
    assert_eq!(
        fresh_reason(false, true),
        Some(FreshReason::NoResumeSupport)
    );
    assert_eq!(
        fresh_reason(true, false),
        Some(FreshReason::NoRecordedConversation)
    );
    assert_eq!(fresh_reason(true, true), None);
}

fn child_run(child: &AgentState) -> rimz::store::run::RunRecord {
    let mut run = rimz::store::run::RunRecord::new(
        rimz::ids::WorkspaceId::from_project_root(Path::new("/tmp")),
        child.kind.clone(),
        PermissionMode::Auto,
        "task".to_owned(),
        PathBuf::from("/tmp"),
    );
    run.agent_id = Some(child.agent_id.clone());
    run
}

#[test]
fn newest_child_run_requires_kind_and_present_names() {
    let child = AgentState::stub("codex", "child", rimz::agents::AgentStatus::Success);
    let mut run = child_run(&child);
    run.agent_id = None;
    assert!(newest_run_for_child(&[run.clone()], &child).is_none());
    run.agent_id = Some(child.agent_id.clone());
    assert!(newest_run_for_child(&[run.clone()], &child).is_some());
    run.kind = rimz::ids::AgentKind::new_unchecked("claude");
    assert!(newest_run_for_child(&[run], &child).is_none());
}

#[test]
fn interrupted_reads_a_working_paused_or_asking_row_and_not_one_at_rest() {
    use rimz::agents::AgentStatus;
    let now = jiff::Timestamp::now();
    for (status, expected) in [
        (AgentStatus::Running, true),
        (AgentStatus::Waiting, true),
        (AgentStatus::Paused, true),
        (AgentStatus::Idle, false),
        (AgentStatus::Success, false),
        (AgentStatus::Failed, false),
    ] {
        let agent = AgentState::stub("claude", "a", status);
        assert_eq!(interrupted(&agent, now), expected, "{status:?}");
    }
}

#[test]
fn account_flag_takes_an_account_in_the_session_pool_and_refuses_others() {
    let agent = AgentState {
        login: Some("personal".parse().expect("login name")),
        name: Some("x".to_owned()),
        ..rimz::testkit::agent_state("claude", "session-1", jiff::Timestamp::now())
    };
    let config: rimz::config::AccountsConfig = toml::from_str(
        "[claude.work]\nhome = \"/srv/claude-work\"\nhistory = \"standalone\"\n\
         [claude.personal]\nhome = \"/srv/claude-personal\"\n\
         [claude.team]\nhome = \"/srv/claude-team\"\n",
    )
    .expect("accounts toml");
    let catalog = rimz::agents::LoginCatalog::from_config(&config).expect("login catalog");
    let name = |name: &str| -> rimz::ids::LoginName { name.parse().expect("login name") };

    assert_eq!(
        flag_login(&agent, &name("team"), true, &config, &catalog).unwrap(),
        Some(name("team"))
    );
    assert_eq!(
        flag_login(&agent, &name("default"), true, &config, &catalog).unwrap(),
        None
    );
    let err = flag_login(&agent, &name("work"), true, &config, &catalog).unwrap_err();
    assert_eq!(
        err.to_string(),
        "restart refused: @x's session belongs to claude account `personal`, and `work` keeps \
         its history apart, so the session cannot resume there; pass an account that shares \
         `personal`'s history, or `personal` itself"
    );
    assert!(!err.to_string().contains("accounts use"));
    // A fresh fallback resumes nothing, so the pool does not bind it.
    assert_eq!(
        flag_login(&agent, &name("work"), false, &config, &catalog).unwrap(),
        Some(name("work"))
    );
    let unknown = flag_login(&agent, &name("nobody"), true, &config, &catalog).unwrap_err();
    assert_eq!(
        unknown.to_string(),
        rimz::agents::session_login(&agent.kind, Some(&name("nobody")), &config)
            .unwrap_err()
            .to_string()
    );
}

#[test]
fn relaunch_refuses_a_session_from_another_account_and_reopens_its_own() {
    let agent = AgentState {
        login: Some("personal".parse().expect("login name")),
        name: Some("x".to_owned()),
        ..rimz::testkit::agent_state("claude", "session-1", jiff::Timestamp::now())
    };
    let room = |name: &str| {
        rimz::agents::RoomAccounts::from(rimz::ids::RoomLogins::from([(
            rimz::ids::AgentKind::new_unchecked("claude"),
            name.parse().expect("login name"),
        )]))
    };

    let accounts = |work_history: &str| {
        rimz::agents::LoginCatalog::from_config(
            &toml::from_str(&format!(
                "[claude.work]\nhome = \"/srv/claude-work\"\nhistory = \"{work_history}\"\n\
                 [claude.personal]\nhome = \"/srv/claude-personal\"\n"
            ))
            .expect("accounts toml"),
        )
        .expect("login catalog")
    };
    let standalone = accounts("standalone");

    let err = relaunch_action(&agent, &room("work"), &standalone, Path::new("/repo")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "@x's session belongs to claude account `personal`; this room now launches claude \
         on `work`. Run `rimz accounts use claude personal` to resume it, then switch \
         back."
    );

    // Resume and fresh alike: a pooled relaunch runs under the room's account.
    let (action, login, _) = relaunch_action(
        &agent,
        &room("work"),
        &accounts("shared"),
        Path::new("/repo"),
    )
    .expect("pooled relaunch");
    assert_eq!(login, Some("work".parse().expect("login name")));
    let request = relaunch_request(&agent, &ResumePosture::default(), action, login, None);
    assert_eq!(
        request.identity.params.login,
        Some("work".parse().expect("login name"))
    );
    let (_, login, _) = relaunch_action(
        &agent,
        &room("default"),
        &accounts("shared"),
        Path::new("/repo"),
    )
    .expect("pooled relaunch on the default account");
    assert_eq!(login, None);

    let (action, login, _) =
        relaunch_action(&agent, &room("personal"), &standalone, Path::new("/repo"))
            .expect("same-account relaunch");
    let request = relaunch_request(&agent, &ResumePosture::default(), action, login, None);
    let dir = tempfile::tempdir().unwrap();
    rimz::harness::launch_plan::testkit::assert_claude_stamped_home(
        &request,
        dir.path(),
        Some("personal"),
    );
}

#[test]
fn restart_only_hands_over_an_open_run_when_the_turn_can_continue() {
    let child = AgentState::stub("claude", "child", rimz::agents::AgentStatus::Success);
    let mut run = child_run(&child);
    assert_eq!(open_run_fate(Some(&run), true, false), (Some(&run), None));
    assert_eq!(open_run_fate(Some(&run), false, false), (None, Some(&run)));
    assert_eq!(open_run_fate(Some(&run), true, true), (None, Some(&run)));
    assert_eq!(open_run_fate(None, true, false), (None, None));
    run.status = rimz::store::run::RunStatus::Completed;
    assert_eq!(open_run_fate(Some(&run), true, false), (None, None));
}
