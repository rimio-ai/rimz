use super::*;
use rimz::agents::AgentStatus;
use rimz::config::{Profile, ProfilesConfig};

static NO_LOGINS: std::sync::LazyLock<rimz::agents::RoomAccounts> =
    std::sync::LazyLock::new(rimz::agents::RoomAccounts::default);
static NO_ACCOUNTS: std::sync::LazyLock<rimz::agents::LoginCatalog> =
    std::sync::LazyLock::new(rimz::agents::LoginCatalog::default);

fn codex_accounts(work_history: &str) -> rimz::agents::LoginCatalog {
    rimz::agents::LoginCatalog::from_config(
        &toml::from_str(&format!(
            "[codex.work]\nhome = \"/srv/codex-work\"\nhistory = \"{work_history}\"\n\
             [codex.personal]\nhome = \"/srv/codex-personal\"\n"
        ))
        .unwrap(),
    )
    .unwrap()
}

fn room(kind: &str, name: &str) -> rimz::agents::RoomAccounts {
    rimz::ids::RoomLogins::from([(AgentKind::new_unchecked(kind), name.parse().unwrap())]).into()
}

#[test]
fn fork_validation_refuses_a_source_from_another_account() {
    let mut agent = source("session-1");
    agent.name = Some("x".to_owned());
    agent.login = Some("work".parse().unwrap());

    let seed = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &room("codex", "personal"),
        &codex_accounts("shared"),
        |_| true,
        |_| true,
    )
    .expect("a pooled source forks under the room's account");
    assert_eq!(seed.launch.login, Some("personal".parse().unwrap()));

    let err = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &room("codex", "personal"),
        &codex_accounts("standalone"),
        |_| true,
        |_| true,
    )
    .expect_err("account mismatch");

    assert_eq!(
        err.to_string(),
        "@x's session belongs to codex account `work`; this room now launches codex on \
         `personal`. Run `rimz accounts use codex work` to resume it, then switch back."
    );
}

fn source(id: &str) -> AgentState {
    let mut agent = AgentState::stub("codex", id, AgentStatus::Success);
    agent.worktree_path = Some("/repo/worktree".to_owned());
    agent
}

fn seed(kind: &str, profile: Option<&str>, mode: Option<PermissionMode>) -> ForkSeed {
    ForkSeed {
        kind: AgentKind::new_unchecked(kind),
        source_session_id: AgentSessionId::from("session-1"),
        cwd: PathBuf::from("/repo/worktree"),
        launch: rimz::agents::LaunchParams {
            profile: profile.map(ToOwned::to_owned),
            mode,
            ..Default::default()
        },
    }
}

fn profiles(name: &str, profile: Profile) -> ProfilesConfig {
    ProfilesConfig(BTreeMap::from([(name.to_owned(), profile)]))
}

fn profile(agent: &str) -> Profile {
    toml::from_str(&format!("agent = {agent:?}")).expect("profile fixture")
}

fn posture_with_profiles(seed: &ForkSeed, profiles: &ProfilesConfig) -> Result<ResumePosture> {
    posture_with_tables(seed, profiles, &ProfilesConfig::default())
}

fn posture_with_tables(
    seed: &ForkSeed,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
) -> Result<ResumePosture> {
    let dir = tempfile::tempdir().unwrap();
    let mut machine = rimz::config::MachineConfig::default();
    machine.agents.profiles = profiles.clone();
    machine.subagents.profiles = subagent_profiles.clone();
    let effective =
        rimz::config::effective::load_with_roots(&machine, dir.path(), &dir.path().join("home"))
            .unwrap();
    super::fork_posture(seed, &effective)
}

#[test]
fn fork_posture_refuses_profiles_and_recorded_overrides_on_a_failed_kind() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/claude.md"), "invalid frontmatter").unwrap();
    let mut machine =
        rimz::config::MachineConfig::parse_text(&dir.path().join("config.toml"), "", dir.path())
            .unwrap();
    machine.agents.profiles = profiles("worker", profile("claude"));
    let effective =
        rimz::config::effective::load_with_roots(&machine, dir.path(), &dir.path().join("home"))
            .unwrap();
    let direct = seed("claude", Some("worker"), None);
    let mut bare = seed("claude", None, None);
    bare.launch.record = Some(Box::default());
    let mut overridden = seed("codex", None, None);
    overridden.launch.record = Some(Box::new(rimz::agents::LaunchRecord {
        agent: Some("worker".into()),
        ..Default::default()
    }));
    let actual = [&direct, &bare, &overridden].map(|seed| {
        super::fork_posture(seed, &effective)
            .err()
            .map(|error| error.to_string())
    });
    assert_eq!(
        Vec::from(actual),
        vec![machine.definition_failure_for("claude"); 3]
    );
}

#[test]
fn fork_posture_replays_full_profile_argv() {
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            agent: "claude".to_owned(),
            mode: Some(PermissionMode::Yolo),
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: Some(prompt.path().to_path_buf().into()),
            auto_compact: Some("372k".into()),
            args: Some("--plugin-dir '/tmp/plugin dir'".to_owned()),
            ..profile("claude")
        },
    );

    let posture =
        posture_with_profiles(&seed("claude", Some("planner"), None), &profiles).expect("posture");

    assert_eq!(
        posture.launch.args,
        vec![
            "--model",
            "opus",
            "--effort",
            "high",
            "--autocompact",
            "372000",
            "--dangerously-skip-permissions",
            "--plugin-dir",
            "/tmp/plugin dir",
        ]
    );
    assert_eq!(
        posture
            .launch
            .system_prompt_file
            .as_ref()
            .and_then(rimz::config::PromptSource::file),
        Some(prompt.path())
    );
    assert_eq!(posture.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(posture.launch.model.as_deref(), Some("opus"));
    assert_eq!(posture.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        serde_json::to_value(posture.launch.record).unwrap()["auto_compact"],
        372_000
    );
}

#[test]
fn fork_posture_resolves_a_launched_child_in_the_subagent_table() {
    let explorer = profiles(
        "explorer",
        Profile {
            model: Some("opus".to_owned()),
            ..profile("claude")
        },
    );
    let mut child = seed("claude", Some("explorer"), None);
    child.launch.profile_scope = Some(ProfileScope::Subagents);
    let twin = profiles("explorer", profile("codex"));
    let actual = [&ProfilesConfig::default(), &twin].map(|agents| {
        posture_with_tables(&child, agents, &explorer)
            .map(|posture| (posture.launch.model, posture.degraded))
            .map_err(|error| error.to_string())
    });
    assert_eq!(actual.to_vec(), vec![Ok((Some("opus".into()), None)); 2]);
}

#[test]
fn fork_posture_refuses_provider_change() {
    let profiles = profiles("planner", profile("codex"));

    let err = posture_with_profiles(&seed("claude", Some("planner"), None), &profiles)
        .expect_err("provider change");

    assert!(
        err.to_string()
            .contains("a fork stays on the source provider")
    );
}

#[test]
fn fork_tab_names_follow_the_seed_profile_else_kind() {
    let pair = |title: &str, pane: &str| (title.to_string(), rimz::pane::PaneTitle::from(pane));
    let names = |seed: &ForkSeed, channel| {
        fork_tab_names(
            seed,
            channel,
            &AgentLaunchIdentity {
                kind: seed.kind.clone(),
                agent_id: "fork".into(),
                name: "bright-river".into(),
                name_explicit: false,
                launch: seed.launch.clone(),
                run_id: None,
                prompt: None,
            },
        )
    };
    let profiled = seed("claude", Some("brainstormer"), None);
    let bare = seed("claude", None, None);
    assert_eq!(
        names(&profiled, None),
        pair("brainstormer", "brainstormer (@bright-river)")
    );
    assert_eq!(names(&bare, None), pair("claude", "claude (@bright-river)"));
    assert_eq!(
        names(&profiled, Some("auth")),
        pair("#auth", "brainstormer (@bright-river)")
    );
}

#[test]
fn fork_posture_degrades_missing_profile_to_bare() {
    let seed = seed("claude", Some("retired"), Some(PermissionMode::Yolo));

    let posture =
        posture_with_profiles(&seed, &ProfilesConfig::default()).expect("bare degraded posture");

    assert_eq!(posture.launch.args, vec!["--dangerously-skip-permissions"]);
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::Unresolved { .. })
    ));
}

#[test]
fn fork_posture_keeps_bare_source_permission_argv() {
    let seed = seed("claude", None, Some(PermissionMode::Yolo));

    let posture = posture_with_profiles(&seed, &ProfilesConfig::default()).expect("bare posture");

    assert_eq!(posture.launch.args, vec!["--dangerously-skip-permissions"]);
    assert_eq!(posture.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(posture.degraded, None);
}

#[test]
fn fork_validation_refuses_subagents() {
    let mut agent = source("session-1");
    agent.parent_agent_id = Some(AgentSessionId::from("parent-1"));

    let err = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect_err("subagent");

    assert!(err.to_string().contains("fork its parent"));
}

#[test]
fn fork_validation_refuses_provisional_sessions() {
    let agent = source("launch_123");

    let err = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect_err("provisional");

    assert!(err.to_string().contains("has not registered"));
}

#[test]
fn fork_validation_refuses_missing_session_file() {
    let agent = source("session-1");

    let err = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| false,
        |_| true,
    )
    .expect_err("session file");

    assert!(err.to_string().contains("conversation file is gone"));
}

#[test]
fn fork_validation_refuses_missing_worktree() {
    let mut unrecorded = source("session-1");
    unrecorded.worktree_path = None;
    let err = validate_fork_source(
        &unrecorded,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect_err("recorded worktree");
    assert!(err.to_string().contains("no recorded worktree"));

    let missing = source("session-1");
    let err = validate_fork_source(
        &missing,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| false,
    )
    .expect_err("worktree path");
    assert!(
        err.to_string()
            .contains("worktree `/repo/worktree` is gone")
    );
}

#[test]
fn fork_validation_inherits_lane_identity_and_drops_cohort_identity() {
    let mut agent = source("session-1");
    agent.checkout = Some("/target".into());
    agent.add_dirs = vec!["/target".into()];
    agent.untrusted_checkout = true;
    agent.record = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "model": null, "effort": null, "agent": null,
            "notes": "20261107/002336Z-loop-review", "loop_section": "The original loop fire"
        }))
        .unwrap(),
    ));
    agent.profile = Some("planner".to_owned());
    agent.channel = Some("auth".to_owned());
    agent.team = Some("forge".to_owned());
    agent.role = Some("coder".to_owned());
    agent.mode = Some(rimz::agents::PermissionMode::Yolo);
    agent.tier = Some(Box::new(rimz::agents::TierStamp {
        tier: rimz::config::tiers::ModelTier::Senior,
        model: "gpt-6-astra".to_owned(),
        used_tier: None,
        unbuildable: Vec::new(),
        skipped: Vec::new(),
    }));

    agent.login = Some("work".parse().unwrap());
    let seed = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &room("codex", "work"),
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect("valid fork");

    assert_eq!(seed.launch.login, agent.login);
    assert_eq!(seed.launch.checkout, agent.checkout);
    assert_eq!(seed.launch.add_dirs, agent.add_dirs);
    assert!(seed.launch.untrusted_checkout);
    assert_eq!(seed.source_session_id, AgentSessionId::from("session-1"));
    assert_eq!(seed.cwd, PathBuf::from("/repo/worktree"));
    assert_eq!(seed.launch.profile.as_deref(), Some("planner"));
    assert_eq!(seed.launch.mode, Some(rimz::agents::PermissionMode::Yolo));
    assert_eq!(seed.launch.tier, agent.tier);
    assert_eq!(seed.launch.team, None);
    assert_eq!(seed.launch.role, None);
    assert_eq!(seed.launch.model, None);
    assert_eq!(seed.launch.effort, None);
    let record = serde_json::to_value(&seed.launch.record).unwrap();
    assert!(record["notes"].is_null() && record["loop_section"].is_null());
    assert_eq!(seed.launch.launch_depth, None);
}

#[test]
fn fork_keeps_the_sources_stored_lane_when_its_path_has_moved() {
    let mut agent = source("session-1");
    let stored: rimz::ids::Lane = serde_json::from_value(
        serde_json::json!({"home": "worktree", "path": "/repo-wt/auth", "name": "design"}),
    )
    .expect("lane");
    agent.lane = Some(stored.clone());
    agent.channel = Some("other".to_owned());

    let seed = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect("valid fork");

    assert_eq!(seed.cwd, PathBuf::from("/repo/worktree"));
    assert_eq!(seed.launch.lane, Some(stored));
}

#[test]
fn fork_validation_inherits_the_callers_automation_not_the_sources() {
    let mut agent = source("session-1");
    agent.automation = Some(rimz::agents::Automation::Loop);
    let mut caller = source("caller");
    for automation in [
        None,
        Some(rimz::agents::Automation::Loop),
        Some(rimz::agents::Automation::Workflow {
            workflow: "review".into(),
            step: "build".into(),
        }),
    ] {
        caller.automation = automation.clone();
        let seed = validate_fork_source(
            &agent,
            Path::new("/repo"),
            Some(&caller),
            &NO_LOGINS,
            &NO_ACCOUNTS,
            |_| true,
            |_| true,
        )
        .unwrap();
        assert_eq!(seed.launch.automation, automation);
    }
    let seed = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .unwrap();
    assert_eq!(seed.launch.automation, None);
}

#[test]
fn fork_validation_inherits_the_source_chain_generation() {
    let mut agent = source("session-1");
    agent.launch_depth = Some(3);
    agent.launched_by = Some(rimz::agents::LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launcher"),
    });

    let seed = validate_fork_source(
        &agent,
        Path::new("/repo"),
        None,
        &NO_LOGINS,
        &NO_ACCOUNTS,
        |_| true,
        |_| true,
    )
    .expect("valid fork");

    assert_eq!(seed.launch.launch_depth, Some(3));
    assert_eq!(seed.launch.launched_by, None);
    assert_eq!(seed.launch.parent_agent_id, None);
}

#[test]
fn fork_validation_records_the_source_profile_doorway_transitively() {
    let mut actual = Vec::new();
    for (child, recorded, peer, team_role) in [
        (true, false, false, false),
        (true, false, false, true),
        (false, true, false, false),
        (false, false, false, false),
        (false, false, true, false),
    ] {
        let mut agent = source("session-1");
        if child || peer || recorded {
            agent.launch_depth = Some(1);
        }
        if child {
            agent.parent_agent_id = Some("parent".into());
        }
        if team_role {
            agent.team = Some("forge".into());
            agent.role = Some("coder".into());
        }
        if recorded {
            agent.profile_scope = Some(ProfileScope::Subagents);
        }
        let seed = validate_fork_source(
            &agent,
            Path::new("/repo"),
            None,
            &NO_LOGINS,
            &NO_ACCOUNTS,
            |_| true,
            |_| true,
        )
        .unwrap();
        actual.push(seed.launch.profile_scope);
    }
    assert_eq!(
        actual,
        [
            Some(ProfileScope::Subagents),
            Some(ProfileScope::Subagents),
            Some(ProfileScope::Subagents),
            None,
            None,
        ]
    );
}
