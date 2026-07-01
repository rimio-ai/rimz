use super::*;
use crate::run::PermissionMode;
use std::num::NonZeroU16;
use tempfile::tempdir;

fn write(dir: &tempfile::TempDir, text: &str) -> PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).expect("write config");
    path
}

fn write_named(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).expect("write config file");
    dir.path().join("config.toml")
}

fn write_agents_home_fragment(
    root: &Path,
    subdir: &str,
    name: &str,
    file: &str,
    text: &str,
) -> PathBuf {
    let dir = root.join(subdir).join(name);
    std::fs::create_dir_all(&dir).expect("create fragment dir");
    let path = dir.join(file);
    std::fs::write(&path, text).expect("write fragment");
    path
}

#[test]
fn missing_or_empty_file_is_default_off() {
    let dir = tempdir().expect("tempdir");
    for path in [dir.path().join("absent.toml"), write(&dir, "")] {
        let config = MachineConfig::load_from(&path).expect("load");
        assert_eq!(config, MachineConfig::default());
        assert!(!config.remote_control.claude);
        assert!(!config.remote_control.codex);
        assert_eq!(
            config
                .agents
                .teams
                .0
                .get("peer")
                .and_then(|team| team.layout.as_deref()),
            Some("claude,codex")
        );
    }
}

#[test]
fn agents_home_fragments_merge_profiles_commands_and_teams() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "codex-coder",
        AGENT_FRAGMENT_FILE,
        "[agents.commands]\n\
             lint = \"cargo clippy\"\n\
             [agents.profiles.codex-coder]\n\
             agent = \"codex\"\n",
    );
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_TEAMS_SUBDIR,
        "review",
        TEAM_FRAGMENT_FILE,
        "[agents.teams.review]\n\
             layout = \"coder\"\n\
             [[agents.teams.review.roles]]\n\
             role = \"coder\"\n\
             profile = \"codex-coder\"\n",
    );

    let mut agents = AgentsConfig::default();
    apply_agents_home(&mut agents, root.path(), &root.path().join("agents.toml")).expect("merge");

    assert_eq!(
        agents
            .profiles
            .0
            .get("codex-coder")
            .map(|profile| profile.agent.as_str()),
        Some("codex"),
    );
    assert_eq!(
        agents.commands.0.get("lint").map(String::as_str),
        Some("cargo clippy"),
    );
    assert_eq!(
        agents
            .teams
            .0
            .get("review")
            .and_then(|team| team.layout.as_deref()),
        Some("coder"),
    );
}

#[test]
fn agents_home_validates_cross_fragment_team_profiles() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "claude-planner",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.claude-planner]\nagent = \"claude\"\n",
    );
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_TEAMS_SUBDIR,
        "plan-code-review",
        TEAM_FRAGMENT_FILE,
        "[agents.teams.plan-code-review]\n\
             [[agents.teams.plan-code-review.roles]]\n\
             role = \"planner\"\n\
             profile = \"claude-planner\"\n",
    );

    let mut agents = AgentsConfig::default();
    apply_agents_home(&mut agents, root.path(), &root.path().join("agents.toml"))
        .expect("cross-fragment references validate after merge");
}

#[test]
fn agents_toml_entries_override_agents_home_fragments() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "planner",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.planner]\nagent = \"codex\"\n",
    );
    let mut agents = AgentsConfig::default();
    agents.profiles.0.insert(
        "planner".to_owned(),
        Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: Some("opus".to_owned()),
            effort: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        },
    );

    apply_agents_home(&mut agents, root.path(), &root.path().join("agents.toml")).expect("merge");

    let profile = agents.profiles.0.get("planner").expect("planner profile");
    assert_eq!(profile.agent, "claude");
    assert_eq!(profile.model.as_deref(), Some("opus"));
}

#[test]
fn absent_agents_home_is_noop() {
    let root = tempdir().expect("tempdir");
    let mut agents = AgentsConfig::default();
    let before = agents.clone();

    apply_agents_home(
        &mut agents,
        &root.path().join("missing"),
        &root.path().join("agents.toml"),
    )
    .expect("absent agents home");

    assert_eq!(agents, before);
}

#[test]
fn malformed_agents_home_fragment_leaves_config_unchanged() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "broken",
        AGENT_FRAGMENT_FILE,
        "not = = toml",
    );
    let mut agents = AgentsConfig::default();
    agents.profiles.0.insert(
        "planner".to_owned(),
        Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        },
    );
    let before = agents.clone();

    assert!(matches!(
        apply_agents_home(&mut agents, root.path(), &root.path().join("agents.toml")),
        Err(ConfigErr::Parse { .. })
    ));
    assert_eq!(agents, before);
}

#[test]
fn agents_home_team_prompt_paths_resolve_against_fragment_dir() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "planner",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.planner]\nagent = \"claude\"\n",
    );
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_TEAMS_SUBDIR,
        "review",
        TEAM_FRAGMENT_FILE,
        "[agents.teams.review]\n\
             [[agents.teams.review.roles]]\n\
             role = \"planner\"\n\
             profile = \"planner\"\n\
             system-prompt-file = \"prompts/planner.md\"\n",
    );

    let mut agents = AgentsConfig::default();
    apply_agents_home(&mut agents, root.path(), &root.path().join("agents.toml")).expect("merge");

    let role = agents
        .teams
        .0
        .get("review")
        .and_then(|team| team.roles.first())
        .expect("review role");
    let expected = root
        .path()
        .join(AGENTS_HOME_TEAMS_SUBDIR)
        .join("review")
        .join("prompts/planner.md");
    assert_eq!(role.system_prompt_file.as_deref(), Some(expected.as_path()));
}

#[test]
fn agents_home_fragment_name_clashes_are_sorted_last_wins() {
    let root = tempdir().expect("tempdir");
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "alpha",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.shared]\nagent = \"claude\"\n",
    );
    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "zulu",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.shared]\nagent = \"codex\"\n",
    );

    let fragment = discover_agents_home(root.path()).expect("discover");

    assert_eq!(
        fragment
            .profiles
            .0
            .get("shared")
            .map(|profile| profile.agent.as_str()),
        Some("codex"),
    );
}

#[test]
fn worktree_config_defaults_and_parses() {
    let dir = tempdir().expect("tempdir");
    let defaults_dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::load_from(&write(&defaults_dir, "")).expect("load");
    assert_eq!(defaults.agents.worktree.dir, "../{repo}-worktrees");
    assert_eq!(defaults.agents.worktree.base, WorktreeBase::Head);

    let config = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.worktree]\n\
             dir = \"../wt-{repo}\"\n\
             base = \"fresh\"\n",
    ))
    .expect("load");
    assert_eq!(config.agents.worktree.dir, "../wt-{repo}");
    assert_eq!(config.agents.worktree.base, WorktreeBase::Fresh);

    let explicit = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.worktree]\nbase = \"main\"\n",
    ))
    .expect("load");
    assert_eq!(
        explicit.agents.worktree.base,
        WorktreeBase::Explicit("main".to_owned())
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.worktree]\nbase = \"\"\n",
        ))
        .is_err()
    );
}

#[test]
fn agent_profiles_commands_and_teams_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.commands]\n\
             vim = \"nvim -p\"\n\
             htop = \"htop\"\n\
             [agents.profiles.codex-yolo]\n\
             agent = \"codex\"\n\
             mode = \"yolo\"\n\
             model = \"gpt-5-codex\"\n\
             effort = \"high\"\n\
             args = \"--model gpt-5-codex -c model_reasoning_effort=high\"\n\
             [agents.profiles.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"/prompts/planner.md\"\n\
             append-system-prompt-file = \"/prompts/planner-extra.md\"\n\
             [agents.teams.stacked]\n\
             layout = \"planner+coder\"\n\
             [[agents.teams.stacked.roles]]\n\
             role = \"planner\"\n\
             profile = \"planner\"\n\
             [[agents.teams.stacked.roles]]\n\
             role = \"coder\"\n\
             profile = \"codex-yolo\"\n",
    ))
    .expect("load");
    let commands = &config.agents.commands.0;
    assert_eq!(commands.get("vim").map(String::as_str), Some("nvim -p"));
    assert_eq!(commands.get("htop").map(String::as_str), Some("htop"));
    let profiles = &config.agents.profiles.0;
    assert_eq!(
        profiles.get("codex-yolo"),
        Some(&Profile {
            agent: "codex".to_owned(),
            mode: Some(PermissionMode::Yolo),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned())
        })
    );
    // A profile carries its own system prompt under the kebab-case key.
    assert_eq!(
        profiles.get("planner"),
        Some(&Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: Some("/prompts/planner.md".into()),
            append_system_prompt_file: Some("/prompts/planner-extra.md".into()),
            args: None,
        })
    );
    let team = config.agents.teams.0.get("stacked").expect("team");
    assert_eq!(team.layout.as_deref(), Some("planner+coder"));
    let roles = &team.roles;
    assert_eq!(roles[0].role, "planner");
    assert_eq!(roles[0].profile, "planner");
    assert_eq!(roles[1].role, "coder");
    assert_eq!(roles[1].profile, "codex-yolo");
}

#[test]
fn profile_system_prompt_file_resolves_against_the_config_dir() {
    // A relative profile prompt roots at the config file's directory, so it
    // points at the same file wherever the profile later launches — not at the
    // agent cwd.
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.profiles.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"prompts/planner.md\"\n",
    ))
    .expect("load");
    let Some(Profile {
        system_prompt_file: Some(path),
        ..
    }) = config.agents.profiles.0.get("planner")
    else {
        panic!("planner profile with a system prompt");
    };
    assert_eq!(path, &dir.path().join("prompts/planner.md"));
    // An absolute path is left untouched.
    let absolute = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.profiles.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"/etc/rimz/planner.md\"\n",
    ))
    .expect("load");
    let Some(Profile {
        system_prompt_file: Some(path),
        ..
    }) = absolute.agents.profiles.0.get("planner")
    else {
        panic!("planner profile with a system prompt");
    };
    assert_eq!(path, std::path::Path::new("/etc/rimz/planner.md"));
}

#[test]
fn loop_tasks_parse_and_default_empty() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::default().r#loop.tasks.0.is_empty(),
        "no loop tasks ship by default",
    );
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "loop.toml",
        "[tasks.morning]\n\
             spec = \"claude-ping\"\n\
             prompt = \"ping\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n\
             days = \"weekdays\"\n\
             worktree = \"main\"\n\
             mode = \"auto\"\n\
             effort = \"low\"\n\
             system-prompt-file = \"/prompts/primer.md\"\n\
             timeout = \"5m\"\n\
             once = true\n\
             [tasks.pr_watch]\n\
             spec = \"codex\"\n\
             prompt-file = \"prompts/pr-watch.md\"\n\
             root = \"/home/me/app\"\n\
             every = \"15m\"\n",
    ))
    .expect("load");
    let entry = config.r#loop.tasks.0.get("morning").expect("morning task");
    assert_eq!(entry.spec.as_deref(), Some("claude-ping"));
    assert_eq!(entry.bind, None);
    assert_eq!(entry.prompt.as_deref(), Some("ping"));
    assert_eq!(entry.root, std::path::Path::new("/home/me/app"));
    assert_eq!(entry.at.as_deref(), Some("07:00"));
    assert_eq!(entry.days.as_deref(), Some("weekdays"));
    assert_eq!(entry.worktree.as_deref(), Some("main"));
    assert_eq!(entry.mode.as_deref(), Some("auto"));
    assert_eq!(entry.effort.as_deref(), Some("low"));
    assert_eq!(
        entry.system_prompt_file.as_deref(),
        Some(std::path::Path::new("/prompts/primer.md"))
    );
    assert_eq!(entry.timeout.as_deref(), Some("5m"));
    assert_eq!(entry.cron, None);
    assert!(entry.once);
    let general = config.r#loop.tasks.0.get("pr_watch").expect("general task");
    assert_eq!(general.spec.as_deref(), Some("codex"));
    assert_eq!(
        general.prompt_file.as_deref(),
        Some(std::path::Path::new("prompts/pr-watch.md"))
    );
    assert_eq!(general.every.as_deref(), Some("15m"));
}

#[test]
fn loop_task_bind_mode_parses() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "loop.toml",
        "[tasks.self_wake]\n\
             bind = { kind = \"claude\", session = \"sess-1\", handle = \"@planner\" }\n\
             prompt = \"pick up the review\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n",
    ))
    .expect("load");
    let entry = config.r#loop.tasks.0.get("self_wake").expect("bind task");
    assert_eq!(entry.spec, None);
    let target = entry.bind.as_ref().expect("target");
    assert_eq!(target.kind, "claude");
    assert_eq!(target.session, "sess-1");
    assert_eq!(target.handle, "@planner");
}

#[test]
fn agents_loop_table_reports_the_loop_toml_migration() {
    let dir = tempdir().expect("tempdir");
    let err = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.loop.tasks.old]\n\
             spec = \"claude\"\n\
             prompt = \"wake\"\n\
             root = \"/repo\"\n\
             at = \"07:00\"\n",
    ))
    .expect_err("old loop table should fail");

    match err {
        ConfigErr::RemovedTable { detail, .. } => {
            assert!(detail.contains("loop.toml"), "{detail}");
        }
        other => panic!("expected RemovedTable, got {other:?}"),
    }
}

#[test]
fn retired_split_sections_are_ignored() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[worktree]\n\
             base = \"fresh\"\n\
             [sidebar]\n\
             refresh_ms = 100\n\
             focus_key = \"Alt+x\"\n",
    ))
    .expect("retired sections ignored");

    let mut expected = MachineConfig::default();
    expected.sidebar.focus_key = "Alt+x".to_owned();
    assert_eq!(config, expected);
}

#[test]
fn load_memo_reuses_unchanged_inputs_and_busts_on_file_change() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    let config_path = write(&dir, "[sidebar]\nfocus_key = \"Alt+x\"\n");

    let first = MachineConfig::load_with_memo(&config_path, agents_home.path()).expect("load");
    let second = MachineConfig::load_with_memo(&config_path, agents_home.path()).expect("load");
    assert_eq!(second, first);

    std::fs::write(&config_path, "[sidebar]\nfocus_key = \"Alt+yy\"\n").expect("rewrite config");
    let changed = MachineConfig::load_with_memo(&config_path, agents_home.path()).expect("reload");
    assert_eq!(changed.sidebar.focus_key, "Alt+yy");
    assert_ne!(changed, first);
}

#[test]
fn lenient_load_falls_back_only_for_the_broken_file() {
    let dir = tempdir().expect("tempdir");
    let config_path = write(&dir, "not = = toml");
    write_named(
        &dir,
        "agents.toml",
        "[agents.profiles.planner]\nagent = \"claude\"\n",
    );

    let config = MachineConfig::load_lenient_from(&config_path);
    assert_eq!(config.accounts, AccountsConfig::default());
    assert_eq!(config.sidebar, SidebarConfig::default());
    assert_eq!(
        config
            .agents
            .profiles
            .0
            .get("planner")
            .map(|profile| profile.agent.as_str()),
        Some("claude"),
    );
}

#[test]
fn lenient_load_resets_invalid_agents_but_keeps_core_config() {
    let dir = tempdir().expect("tempdir");
    let config_path = write(&dir, "[remote_control]\nclaude = true\n");
    write_named(
        &dir,
        "agents.toml",
        "[agents.profiles.term]\nagent = \"claude\"\n",
    );

    let config = MachineConfig::load_lenient_from(&config_path);
    assert!(config.remote_control.claude);
    assert_eq!(config.agents, AgentsConfig::default());
}

#[test]
fn profile_tables_reject_unknown_fields_and_missing_agent() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.profiles.mixed]\ncommand = \"nvim\"\nagent = \"claude\"\n",
        ))
        .is_err()
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.profiles.missing_agent]\nmode = \"yolo\"\n",
        ))
        .is_err()
    );
}

#[test]
fn profile_name_validation_runs_at_config_load() {
    let dir = tempdir().expect("tempdir");
    assert!(matches!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.profiles.term]\nagent = \"claude\"\n",
        )),
        Err(ConfigErr::Agents { .. })
    ));
    assert!(matches!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.profiles.claude-2]\nagent = \"claude\"\n",
        )),
        Err(ConfigErr::Agents { .. })
    ));
}

#[test]
fn removed_agents_tables_fail_fast_with_the_rename() {
    let dir = tempdir().expect("tempdir");
    for legacy in [
        "[tab]\nkeywords = []\n",
        "[agents.aliases]\nvim = \"nvim\"\n",
        "[agents.layouts]\nreview = \"claude,codex\"\n",
    ] {
        assert!(
            matches!(
                MachineConfig::load_from(&write_named(&dir, "agents.toml", legacy)),
                Err(ConfigErr::RemovedTable { .. })
            ),
            "expected a removed-table error for: {legacy}"
        );
    }
    // The current shape still loads.
    MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents]\nplacement = \"tab\"\n\n[agents.commands]\nvim = \"nvim\"\n",
    ))
    .expect("current agents config loads");
}

#[test]
fn per_agent_toggles_parse_independently() {
    let dir = tempdir().expect("tempdir");
    let config =
        MachineConfig::load_from(&write(&dir, "[remote_control]\nclaude = true\n")).expect("load");
    assert!(config.remote_control.claude);
    assert!(!config.remote_control.codex, "codex stays off when unset");

    let both = MachineConfig::load_from(&write(
        &dir,
        "[remote_control]\nclaude = true\ncodex = true\n",
    ))
    .expect("load");
    assert!(both.remote_control.claude);
    assert!(both.remote_control.codex);
}

#[test]
fn unknown_keys_are_ignored() {
    let dir = tempdir().expect("tempdir");
    let text = "sound_profile = \"chime\"\n\n[remote_control]\ncodex = true\ncapacity = 16\n";
    let config = MachineConfig::load_from(&write(&dir, text)).expect("load");
    assert!(config.remote_control.codex);
    assert!(!config.remote_control.claude);
}

#[test]
fn notification_defaults_cover_attention_transitions() {
    let config = MachineConfig::default();
    assert!(config.notifications.enabled);
    assert_eq!(
        config.notifications.triggers,
        NotificationTrigger::all().to_vec()
    );
    assert_eq!(config.notifications.desktop, DesktopNotificationMode::Auto);
    assert_eq!(config.notifications.sound, NotificationSoundMode::Bell);
    assert!(config.notifications.suppress_focused);
    assert_eq!(config.notifications.debounce_ms, 5_000);
    assert_eq!(config.notifications.coalesce_ms, 1_000);
    assert_eq!(config.notifications.remind_secs, 60);
    assert_eq!(config.notifications.title, None);
    assert_eq!(config.notifications.body, None);
    assert!(config.notifications.command().is_none());
    assert!(config.notifications.handler.is_empty());
}

#[test]
fn notifications_parse_per_machine_preferences() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[notifications]\n\
             enabled = false\n\
             triggers = [\"waiting\", \"failed\"]\n\
             desktop = \"osc\"\n\
             sound = \"off\"\n\
             suppress_focused = false\n\
             debounce_ms = 2500\n\
             coalesce_ms = 0\n\
             remind_secs = 15\n\
             title = \"Rimz: {{agent}} {{kind}}\"\n\
             body = \"{{task}}\"\n\
             command = \"ntfy publish rimz\"\n",
    ))
    .expect("load");
    assert!(!config.notifications.enabled);
    assert_eq!(
        config.notifications.triggers,
        vec![NotificationTrigger::Waiting, NotificationTrigger::Failed]
    );
    assert_eq!(config.notifications.desktop, DesktopNotificationMode::Osc);
    assert_eq!(config.notifications.sound, NotificationSoundMode::Off);
    assert!(!config.notifications.suppress_focused);
    assert_eq!(config.notifications.debounce_ms, 2_500);
    assert_eq!(config.notifications.coalesce_ms, 0);
    assert_eq!(config.notifications.remind_secs, 15);
    assert_eq!(
        config.notifications.title.as_deref(),
        Some("Rimz: {{agent}} {{kind}}")
    );
    assert_eq!(config.notifications.body.as_deref(), Some("{{task}}"));
    assert_eq!(config.notifications.command(), Some("ntfy publish rimz"));
}

#[test]
fn notifications_parse_handlers_and_validate_templates() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[notifications]\n\
             [[notifications.handler]]\n\
             name = \"urgent\"\n\
             command = \"ntfy publish --title {{title}} rimz {{body}}\"\n\
             when = { kind = [\"waiting\"], worktree = [\"feat/*\"], handle = [\"@planner\"] }\n",
    ))
    .expect("load");

    assert_eq!(config.notifications.handler.len(), 1);
    assert_eq!(
        config.notifications.handler[0].when.kind,
        vec![NotificationKind::Waiting]
    );
    assert_eq!(
        config.notifications.handler[0].when.worktree,
        vec!["feat/*".to_owned()]
    );
    assert_eq!(
        config.notifications.handler[0].when.handle,
        vec!["@planner".to_owned()]
    );

    let err = MachineConfig::load_from(&write(
        &dir,
        "[notifications]\n\
             [[notifications.handler]]\n\
             name = \"bad\"\n\
             command = \"notify {{nope}}\"\n",
    ))
    .expect_err("unknown var rejects");
    assert!(matches!(err, ConfigErr::Notifications { .. }));
}

#[test]
fn sidebar_max_cols_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nmax_cols = 100\n",
    ))
    .expect("load");
    assert_eq!(
        config.theme.display.max_cols,
        NonZeroU16::new(100).expect("nonzero")
    );
    assert_eq!(
        MachineConfig::default().theme.display.max_cols.get(),
        72,
        "unset caps the percentage split at the 72-column default",
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.display]\nmax_cols = 0\n"
        ))
        .is_err()
    );
}

#[test]
fn sidebar_refresh_ms_defaults_parses_and_clamps_at_use() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nrefresh_ms = 80\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.refresh_ms, 80);
    assert_eq!(config.theme.display.resolved_refresh_ms(), 80);
    assert_eq!(
        MachineConfig::default().theme.display.refresh_ms,
        crate::sidebar::timing::DEFAULT_REFRESH_MS
    );

    let too_low = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nrefresh_ms = 1\n",
    ))
    .expect("load");
    assert_eq!(
        too_low.theme.display.resolved_refresh_ms(),
        crate::sidebar::timing::MIN_REFRESH_MS
    );

    let too_high = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nrefresh_ms = 5000\n",
    ))
    .expect("load");
    assert_eq!(
        too_high.theme.display.resolved_refresh_ms(),
        crate::sidebar::timing::MAX_REFRESH_MS
    );
}

#[test]
fn sidebar_trunk_parses_and_defaults_unset() {
    let dir = tempdir().expect("tempdir");
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\ntrunk = \"develop\"\n")).expect("load");
    assert_eq!(config.sidebar.trunk.as_deref(), Some("develop"));
    assert_eq!(
        MachineConfig::default().sidebar.trunk,
        None,
        "unset leaves the trunk ladder to detection alone",
    );
}

#[test]
fn sidebar_spend_headline_window_parses_and_defaults_session() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "timezone = \"America/New_York\"\n[sidebar]\nspend_window = \"session\"\n",
    ))
    .expect("load");
    assert_eq!(
        config.sidebar.spend_window,
        crate::agents::SpendWindowMode::Session
    );
    assert_eq!(config.timezone.as_deref(), Some("America/New_York"));
    assert_eq!(
        config.headline_spec().timezone.as_deref(),
        Some("America/New_York")
    );
    assert_eq!(
        MachineConfig::default().sidebar.spend_window,
        crate::agents::SpendWindowMode::Session
    );
    assert_eq!(MachineConfig::default().timezone, None);
}

#[test]
fn sidebar_afk_window_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(
        config.sidebar.afk_after_secs.get(),
        DEFAULT_AFK_AFTER_SECS,
        "unset uses the shipped 15-minute AFK window",
    );
    assert_eq!(config.sidebar.afk_after_ms(), 15 * 60 * 1_000);

    let tuned =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nafk_after_secs = 60\n")).expect("load");
    assert_eq!(tuned.sidebar.afk_after_secs.get(), 60);
    assert_eq!(tuned.sidebar.afk_after_ms(), 60_000);

    assert!(
        MachineConfig::load_from(&write(&dir, "[sidebar]\nafk_after_secs = 0\n")).is_err(),
        "zero cannot disable the AFK badge"
    );
}

#[test]
fn sidebar_scrollbar_parses_and_defaults_auto() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nscrollbar = \"never\"\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.scrollbar, ScrollbarMode::Never);
    assert_eq!(
        MachineConfig::default().theme.display.scrollbar,
        ScrollbarMode::Auto,
        "unset auto-hides: the bar shows only while the viewport moves",
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.display]\nscrollbar = \"bogus\"\n"
        ))
        .is_err()
    );
}

#[test]
fn attention_config_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(
        config.agents.attention.stalled_after_secs.get(),
        crate::agents::DEFAULT_STALL_AFTER_SECS,
        "unset uses the shipped 30-minute stall window",
    );

    let tuned = MachineConfig::load_from(&write_named(
        &dir,
        "agents.toml",
        "[agents.attention]\nstalled_after_secs = 2700\n",
    ))
    .expect("load");
    assert_eq!(tuned.agents.attention.stalled_after_secs.get(), 2700);

    let partial =
        MachineConfig::load_from(&write_named(&dir, "agents.toml", "[agents.attention]\n"))
            .expect("load");
    assert_eq!(partial.agents.attention, AttentionConfig::default());

    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "agents.toml",
            "[agents.attention]\nstalled_after_secs = 0\n",
        ))
        .is_err()
    );
}

#[test]
fn sidebar_theme_parses_defaults_unset_and_rejects_out_of_range() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme]\nmode = 256\nscheme = \"TokyoNight Night\"\ngood = 34\nselection = \"#8ab3e0\"\n",
    ))
    .expect("load");
    assert_eq!(config.theme.mode, ThemeMode::Indexed);
    assert_eq!(config.theme.scheme.as_deref(), Some("TokyoNight Night"));
    assert_eq!(config.theme.good, Some(ThemeColor::Indexed(34)));
    assert_eq!(
        config.theme.selection,
        Some(ThemeColor::Rgb(0x8a, 0xb3, 0xe0))
    );
    assert_eq!(config.theme.alarm, None, "unset slots stay builtin");
    assert!(MachineConfig::default().theme.is_unset());
    assert!(
        MachineConfig::load_from(&write_named(&dir, "theme.toml", "[theme]\ngood = 300\n"))
            .is_err()
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme]\nselection = \"#bad\"\n"
        ))
        .is_err()
    );
}

#[test]
fn sidebar_animations_parse_as_partial_role_overrides() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.animations.thinking]\n\
             frames = \"⠁⠂\"\n\
             color = \"clay\"\n\
             speed = \"slow\"\n\
             [theme.animations.idle]\n\
             effect = \"breathe\"\n",
    ))
    .expect("load");
    let thinking = config.theme.animations.thinking.expect("thinking override");
    assert_eq!(
        thinking.frames.expect("frames").as_slice(),
        ["⠁".to_owned(), "⠂".to_owned()]
    );
    assert_eq!(thinking.color, Some(AnimationColor::Clay));
    assert_eq!(thinking.speed, Some(AnimationSpeed::Slow));
    assert_eq!(
        config.theme.animations.idle.expect("idle override").effect,
        Some(AnimationEffect::Breathe)
    );
    assert!(MachineConfig::default().theme.animations.is_unset());
}

#[test]
fn sidebar_animations_accept_attention_frames_and_reject_bad_shapes() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.animations.waiting]\nframes = \"?!\"\n",
        ))
        .is_ok(),
        "waiting now follows the uniform frame model"
    );
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.animations.idle]\nframes = [\"...\"]\n",
        ))
        .is_err()
    );
}

#[test]
fn sidebar_glyphs_parse_and_default_unicode() {
    let dir = tempdir().expect("tempdir");
    assert!(MachineConfig::default().theme.glyphs.is_unset());

    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.glyphs]\n\
             set = \"nerd_font\"\n\
             [theme.glyphs.nerd_font.tokens]\n\
             total = \"◇\"\n",
    ))
    .expect("load");
    assert_eq!(config.theme.glyphs.set.as_deref(), Some("nerd_font"));
    assert_eq!(
        config
            .theme
            .glyphs
            .glyph("nerd_font", crate::config::GlyphRole::TokensTotal),
        Some("◇")
    );

    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.glyphs.unicode.tokens]\ntotal = \"abc\"\n",
        ))
        .is_err(),
        "glyph overrides occupy at most two terminal cells"
    );

    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.glyphs.unicode.makr]\ntotal = \"Σ\"\n",
        ))
        .is_err(),
        "glyph namespaces must be known"
    );
}

#[test]
fn sidebar_glow_parses_and_defaults_auto() {
    let dir = tempdir().expect("tempdir");
    assert_eq!(
        MachineConfig::default().theme.display.glow,
        GlowMode::Auto,
        "transition flashes ship following the terminal's advertisement",
    );
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nglow = \"always\"\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.glow, GlowMode::Always);
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nglow = \"never\"\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.glow, GlowMode::Never);
    assert!(
        MachineConfig::load_from(&write_named(
            &dir,
            "theme.toml",
            "[theme.display]\nglow = false\n"
        ))
        .is_err()
    );
}

#[test]
fn zellij_room_defaults_are_agent_friendly() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(config.zellij.mouse_mode, None);
    assert!(config.zellij.mouse_click_through);
    assert_eq!(config.zellij.advanced_mouse_actions, None);
    assert_eq!(config.zellij.mouse_hover_effects, None);
    assert!(!config.zellij.focus_follows_mouse);
    assert_eq!(config.zellij.pane_frames, None);
    assert_eq!(config.zellij.on_force_close, None);
    assert_eq!(config.zellij.scroll_buffer_size, None);
    assert_eq!(config.zellij.show_startup_tips, None);
    assert_eq!(config.zellij.show_release_notes, None);
    assert_eq!(config.zellij.copy_clipboard, None);
    assert_eq!(config.zellij.copy_on_select, None);
    assert_eq!(config.zellij.support_kitty_keyboard_protocol, None);
    assert_eq!(config.zellij.osc8_hyperlinks, None);
    assert!(config.zellij.auto_layout);
    assert!(!config.zellij.session_serialization);
}

#[test]
fn zellij_room_options_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[zellij]\n\
             pane_frames = true\n\
             mouse_mode = false\n\
             advanced_mouse_actions = true\n\
             mouse_hover_effects = true\n\
             focus_follows_mouse = false\n\
             copy_clipboard = \"primary\"\n\
             copy_on_select = false\n\
             support_kitty_keyboard_protocol = false\n\
             osc8_hyperlinks = false\n\
             scroll_buffer_size = 200000\n\
             show_startup_tips = true\n\
             show_release_notes = true\n\
             on_force_close = \"quit\"\n\
             auto_layout = false\n",
    ))
    .expect("load");
    assert_eq!(config.zellij.pane_frames, Some(true));
    assert_eq!(config.zellij.mouse_mode, Some(false));
    assert_eq!(config.zellij.advanced_mouse_actions, Some(true));
    assert_eq!(config.zellij.mouse_hover_effects, Some(true));
    assert!(!config.zellij.focus_follows_mouse);
    assert_eq!(config.zellij.copy_clipboard, Some(ZellijClipboard::Primary));
    assert_eq!(config.zellij.copy_on_select, Some(false));
    assert_eq!(config.zellij.support_kitty_keyboard_protocol, Some(false));
    assert_eq!(config.zellij.osc8_hyperlinks, Some(false));
    assert_eq!(config.zellij.scroll_buffer_size, Some(200_000));
    assert_eq!(config.zellij.show_startup_tips, Some(true));
    assert_eq!(config.zellij.show_release_notes, Some(true));
    assert_eq!(config.zellij.on_force_close, Some(ZellijForceClose::Quit));
    assert!(!config.zellij.auto_layout);
}

#[test]
fn zellij_default_mode_config_is_legacy_noop() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "[zellij]\ndefault_mode = \"normal\"\n"))
        .expect("legacy default_mode key is ignored");
    assert_eq!(config.zellij, ZellijConfig::default());
}

#[test]
fn tmux_room_defaults_are_agent_friendly() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert!(config.tmux.mouse);
    assert!(config.tmux.focus_events);
    assert_eq!(config.tmux.history_limit, 100_000);
    assert!(config.tmux.allow_passthrough);
    assert_eq!(config.tmux.set_clipboard, TmuxSetClipboard::On);
    assert!(config.tmux.extended_keys);
    assert_eq!(
        config.tmux.extended_keys_format,
        TmuxExtendedKeysFormat::CsiU,
    );
    assert_eq!(config.tmux.escape_time_ms, 0);
    assert!(config.tmux.renumber_windows);
    assert!(config.tmux.aggressive_resize);
    assert_eq!(config.tmux.pane_border_status, None);
    assert_eq!(config.tmux.pane_border_lines, None);
}

#[test]
fn tmux_room_options_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[tmux]\n\
             set_clipboard = \"external\"\n\
             extended_keys_format = \"xterm\"\n\
             pane_border_status = \"top\"\n\
             pane_border_lines = \"heavy\"\n",
    ))
    .expect("load");
    assert_eq!(config.tmux.set_clipboard, TmuxSetClipboard::External);
    assert_eq!(
        config.tmux.extended_keys_format,
        TmuxExtendedKeysFormat::Xterm,
    );
    assert_eq!(
        config.tmux.pane_border_status,
        Some(TmuxPaneBorderStatus::Top)
    );
    assert_eq!(
        config.tmux.pane_border_lines,
        Some(TmuxPaneBorderLines::Heavy)
    );
}

#[test]
fn malformed_toml_surfaces_an_error() {
    let dir = tempdir().expect("tempdir");
    let err = MachineConfig::load_from(&write(&dir, "[remote_control]\nclaude = \"yes\"\n"))
        .expect_err("type mismatch should fail");
    assert!(matches!(err, ConfigErr::Parse { .. }));
}

#[test]
fn provider_block_cap_defaults_to_three() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(config.theme.display.max_provider_blocks, 3);
    assert_eq!(config.theme.display.provider_tabs, ProviderTabsMode::Auto);
    assert!(config.theme.display.provider_list.is_empty());
    let partial = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nmax_cols = 60\n",
    ))
    .expect("load");
    assert_eq!(partial.theme.display.max_provider_blocks, 3);
    assert_eq!(partial.theme.display.provider_tabs, ProviderTabsMode::Auto);
    assert!(partial.theme.display.provider_list.is_empty());
}

#[test]
fn provider_dashboard_tabs_and_list_parse_and_round_trip() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nprovider_tabs = \"always\"\nprovider_list = [\"codex\", \"all\"]\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(config.theme.display.provider_list, vec!["codex", "all"]);

    let encoded = toml::to_string(&config.theme.display).expect("serialize display");
    let round_tripped: DisplayConfig = toml::from_str(&encoded).expect("parse display");
    assert_eq!(round_tripped.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(round_tripped.provider_list, vec!["codex", "all"]);
}

#[test]
fn sidebar_pets_defaults_parse_and_round_trip() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.pets]\nenabled = true\npet = \"dewey\"\nglyphs = \"pixel\"\nvoice = false\n",
    ))
    .expect("load");
    assert!(config.theme.pets.enabled);
    assert_eq!(config.theme.pets.pet, "dewey");
    assert_eq!(config.theme.pets.glyphs, PetsGlyphMode::Pixel);
    assert!(!config.theme.pets.voice);

    let defaults_dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::load_from(&write(&defaults_dir, "")).expect("load");
    assert_eq!(defaults.theme.pets, PetsConfig::default());
    assert!(defaults.theme.pets.is_default());

    let encoded = toml::to_string(&config.theme.pets).expect("serialize pets");
    let round_tripped: PetsConfig = toml::from_str(&encoded).expect("parse pets");
    assert_eq!(round_tripped, config.theme.pets);
}

#[test]
fn context_severity_bands_default_and_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    let defaults = ContextMeterConfig::default();
    assert_eq!(config.theme.display.context_meter, defaults);
    assert_eq!(
        (defaults.green.percent, defaults.green.tokens),
        (40, 100_000)
    );
    assert_eq!(
        (defaults.yellow.percent, defaults.yellow.tokens),
        (60, 160_000)
    );
    assert_eq!(
        (defaults.amber.percent, defaults.amber.tokens),
        (75, 258_000)
    );
    assert_eq!((defaults.red.percent, defaults.red.tokens), (90, 420_000));
    let tuned = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display.context_meter]\nred = { percent = 50, tokens = 100000 }\n",
    ))
    .expect("load");
    assert_eq!(
        tuned.theme.display.context_meter.red,
        ContextBand {
            percent: 50,
            tokens: 100_000
        }
    );
    assert_eq!(tuned.theme.display.context_meter.green, defaults.green);
    assert_eq!(tuned.theme.display.context_meter.yellow, defaults.yellow);
    assert_eq!(tuned.theme.display.context_meter.amber, defaults.amber);
}

#[test]
fn budget_zones_default_and_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    let defaults = BudgetBarConfig::default();
    assert_eq!(config.theme.display.budget_bar, defaults);
    assert_eq!(
        (defaults.yellow, defaults.amber, defaults.red),
        (50, 25, 10)
    );
    assert_eq!(
        (
            defaults.burn_rate.yellow,
            defaults.burn_rate.amber,
            defaults.burn_rate.red
        ),
        (100, 150, 200)
    );

    let tuned = MachineConfig::load_from(&write_named(
        &dir,
        "theme.toml",
        "[theme.display.budget_bar]\nred = 20\n[theme.display.budget_bar.burn_rate]\nred = 300\n",
    ))
    .expect("load");
    let budget = tuned.theme.display.budget_bar;
    assert_eq!(
        (budget.yellow, budget.amber, budget.red),
        (defaults.yellow, defaults.amber, 20)
    );
    assert_eq!(
        (
            budget.burn_rate.yellow,
            budget.burn_rate.amber,
            budget.burn_rate.red
        ),
        (defaults.burn_rate.yellow, defaults.burn_rate.amber, 300)
    );
    let reparsed: MachineConfig =
        toml::from_str(&toml::to_string(&tuned).expect("serialize")).expect("reparse");
    assert_eq!(
        reparsed.theme.display.budget_bar,
        tuned.theme.display.budget_bar
    );
}

#[test]
fn provider_style_parses_art_and_color() {
    let dir = tempdir().expect("tempdir");
    let text = "[theme.providers.claude]\ncolor = \"#D97757\"\nascii_art = \" ▐▛███▜▌\"\n";
    let config = MachineConfig::load_from(&write_named(&dir, "theme.toml", text)).expect("load");
    let claude = config
        .theme
        .providers
        .get("claude")
        .expect("claude provider style");
    assert_eq!(claude.color, Some(ThemeColor::Rgb(0xd9, 0x77, 0x57)));
    assert_eq!(claude.ascii_art.as_deref(), Some(" ▐▛███▜▌"));
    assert_eq!(claude.product_name, None);
}

#[test]
fn sentry_config_defaults_off_and_parses() {
    let dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(defaults.sentry, SentryConfig::default());
    assert!(defaults.sentry.dsn.is_none());

    let config = MachineConfig::load_from(&write(
        &dir,
        "[sentry]\n\
             dsn = \"https://key@o1.ingest.sentry.io/2\"\n\
             environment = \"dev\"\n",
    ))
    .expect("load");
    assert_eq!(
        config.sentry.dsn.as_deref(),
        Some("https://key@o1.ingest.sentry.io/2")
    );
    assert_eq!(config.sentry.environment.as_deref(), Some("dev"));
}
