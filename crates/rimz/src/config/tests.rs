use super::*;
use crate::ids::MuxName;
use std::num::NonZeroU16;
use tempfile::tempdir;

#[test]
fn glyph_set_source_folds_style_after_explicit_set() {
    let mut theme = ThemeConfig::default();
    assert_eq!(theme.glyph_set_source(), None);
    theme.style = Some(ThemeStyle::Modern);
    assert_eq!(theme.glyph_set_source(), Some("nerd_font"));
    theme.glyphs.set = Some("unicode".to_owned());
    assert_eq!(theme.glyph_set_source(), Some("unicode"));
}

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

fn no_fragments(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("missing-agents-home")
}

fn load_no_fragments(path: &Path) -> Result<MachineConfig> {
    MachineConfig::load_from(path, &no_fragments(path))
}

fn load_lenient_no_fragments(path: &Path) -> MachineConfig {
    MachineConfig::load_lenient_from(path, &no_fragments(path))
}

fn expire_load_memo() {
    if let Ok(mut memo) = LOAD_MEMO.get_or_init(|| Mutex::new(None)).lock()
        && let Some(memo) = memo.as_mut()
    {
        memo.last_verified =
            Instant::now() - CONFIG_STAMP_TTL - std::time::Duration::from_millis(1);
    }
}

fn set_modified_time(path: &Path, modified: std::time::SystemTime) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open config file");
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("set modified time");
}

#[derive(Clone, Copy, Debug)]
enum ExpectedErr {
    Parse,
    Notifications,
    Loop,
    AccountBudget,
}

fn expect_err(file: &str, text: &str) -> ConfigErr {
    let dir = tempdir().expect("tempdir");
    load_no_fragments(&write_named(&dir, file, text)).expect_err("config should fail")
}

fn assert_config_err(err: ConfigErr, expected: ExpectedErr) {
    match (&err, expected) {
        (ConfigErr::Parse { .. }, ExpectedErr::Parse)
        | (ConfigErr::Notifications { .. }, ExpectedErr::Notifications) => {}
        (ConfigErr::Loop { .. }, ExpectedErr::Loop) => {}
        (ConfigErr::AccountBudget { .. }, ExpectedErr::AccountBudget) => {}
        _ => panic!("expected {expected:?}, got {err:?}"),
    }
}

type ConfigAssertion = fn(&MachineConfig);

fn assert_sentry_config(config: &MachineConfig) {
    assert_eq!(
        config.sentry.dsn.as_deref(),
        Some("https://key@o1.ingest.sentry.io/2")
    );
    assert_eq!(config.sentry.environment.as_deref(), Some("dev"));
}

fn assert_web_config(config: &MachineConfig) {
    assert!(!config.web.enabled);
    assert_eq!(
        config.web.base_url.as_deref(),
        Some("https://devbox.example/rimz")
    );
    assert_eq!(config.web.port, 9123);
    assert_eq!(config.web.share_port, 9124);
    assert_eq!(config.web.interface, "0.0.0.0");
    assert_eq!(
        config.web.share_base_url.as_deref(),
        Some("https://watch.example/rimz")
    );
    assert_eq!(config.web.auth_header.as_deref(), Some("X-Forwarded-User"));
    assert_eq!(config.web.auth_users, ["alice", "bob"]);
    assert_eq!(config.web.trusted_proxies, ["10.0.0.0/8", "fd00::/8"]);
    assert_eq!(config.web.font, "FiraCode Nerd Font Mono");
    assert_eq!(config.web.font_source.as_deref(), Some("/tmp/font.woff2"));
    assert!(!config.web.style_client);
}

fn assert_remote_control_config(config: &MachineConfig) {
    assert!(config.remote_control.enabled_for("claude"));
    assert!(config.remote_control.enabled_for("codex"));
}

#[test]
fn missing_or_empty_file_is_default_off() {
    let dir = tempdir().expect("tempdir");
    for path in [dir.path().join("absent.toml"), write(&dir, "")] {
        let config = load_no_fragments(&path).expect("load");
        assert_eq!(config, MachineConfig::default());
        assert!(!config.remote_control.enabled_for("claude"));
        assert!(!config.remote_control.enabled_for("codex"));
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
fn broken_machine_files_reports_only_the_unparseable_file() {
    let dir = tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(CONFIG_FILE),
        "[sidebar]\nfocus_key = \"Alt+p\"\n",
    )
    .expect("write core config");
    std::fs::write(
        dir.path().join(THEME_FILE),
        "[theme.display]\nmax_cols = 64\nmax_cols = 72\n",
    )
    .expect("write broken theme config");

    let errors = broken_machine_files_in(&MachineConfigFiles::from_paths(
        dir.path().join(CONFIG_FILE),
        dir.path().join("agents-home"),
    ));

    assert_eq!(errors.len(), 1, "only theme.toml is broken: {errors:?}");
    match &errors[0] {
        ConfigErr::Parse { path, diagnosis } => {
            assert_eq!(path, &dir.path().join(THEME_FILE));
            let detail = diagnosis.to_string();
            assert!(
                detail.contains("defined more than once") && detail.contains("max_cols"),
                "precise duplicate-key error: {detail}",
            );
        }
        other => panic!("expected theme parse error, got {other:?}"),
    }
}

#[test]
fn lenient_load_falls_back_only_for_the_broken_file() {
    let dir = tempdir().unwrap();
    let path = write(&dir, "not = = toml");
    write_definition(
        dir.path(),
        "agents/planner.md",
        "description: Planner\nagent: codex\ntools: []",
        "",
    );
    let config = MachineConfig::load_lenient_from(&path, dir.path());
    assert_eq!(config.accounts, AccountsConfig::default());
    assert_eq!(config.agents.profiles.0["planner"].agent, "codex");
}

#[test]
fn account_budget_validation_is_typed_and_lenient_load_preserves_preflight_detail() {
    let dir = tempdir().expect("tempdir");
    let config_path = write(
        &dir,
        "[accounts.budget]\nclaude = \"100/day\"\ncursor = \"50/day\"\n",
    );

    assert!(matches!(
        load_no_fragments(&config_path),
        Err(ConfigErr::AccountBudget {
            source: AccountBudgetConfigError::Unsupported { kind },
            ..
        }) if kind == "cursor"
    ));
    let lenient = load_lenient_no_fragments(&config_path);
    assert_eq!(
        lenient.accounts.budget("claude").map(DayCap::as_usd),
        Some(100.0)
    );
    assert_eq!(
        lenient.accounts.budget("cursor").map(DayCap::as_usd),
        Some(50.0)
    );
}

#[test]
fn core_parse_accepts_supported_budget_and_cursor_display_limit() {
    let dir = tempdir().expect("tempdir");
    let config_path = write(
        &dir,
        "[accounts.budget]\nclaude = \"100/day\"\n[accounts.usage_limit_usd]\ncursor = 25\n",
    );

    let config = load_no_fragments(&config_path).expect("valid account config");
    assert_eq!(
        config.accounts.budget("claude").map(DayCap::as_usd),
        Some(100.0)
    );
    assert_eq!(config.accounts.usage_limit("cursor"), Some(25.0));
}

#[test]
fn strict_load_ignores_unknown_keys_and_records_their_source_files() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    let config_path = write(
        &dir,
        "[[daemon.pane]]\ncommand = \"stats\"\nfuture = true\n",
    );
    write_named(&dir, THEME_FILE, "[theme.glyphs]\nfuture = true\n");
    let theme_path = dir.path().join(THEME_FILE);
    write_named(
        &dir,
        LOOP_FILE,
        "[tasks.nightly]\nagent = \"claude\"\nroot = \"/tmp\"\nfuture = true\n",
    );
    let loop_path = dir.path().join(LOOP_FILE);

    let config = MachineConfig::load_from(&config_path, agents_home.path()).expect("load");

    assert_eq!(
        config.notices.unknown_keys,
        [
            UnknownConfigKey {
                path: config_path,
                key: "daemon.pane.0.future".to_owned(),
            },
            UnknownConfigKey {
                path: theme_path,
                key: "theme.glyphs.future".to_owned(),
            },
            UnknownConfigKey {
                path: loop_path,
                key: "tasks.nightly.future".to_owned(),
            },
        ]
    );
}

#[test]
fn load_memo_reuses_unchanged_inputs_and_busts_on_file_change() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    let config_path = write(&dir, "[sidebar]\nfocus_key = \"Alt+x\"\n");

    let first = MachineConfig::load_with_memo(&config_path, agents_home.path());
    let second = MachineConfig::load_with_memo(&config_path, agents_home.path());
    assert_eq!(second, first);

    std::fs::write(&config_path, "[sidebar]\nfocus_key = \"Alt+yy\"\n").expect("rewrite config");
    expire_load_memo();
    let changed = MachineConfig::load_with_memo(&config_path, agents_home.path());
    assert_eq!(changed.sidebar.focus_key, "Alt+yy");
    assert_ne!(changed, first);
}

#[test]
fn load_memo_skips_torn_theme_pet_rewrite() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    let config_path = write(&dir, "");
    let theme_path = dir.path().join(THEME_FILE);
    std::fs::write(
        &theme_path,
        "[theme.pets]\nenabled = true\npet = \"dewey\"\n",
    )
    .expect("write initial theme");

    let first = MachineConfig::load_with_memo(&config_path, agents_home.path());
    assert!(first.theme.pets.enabled);
    assert_eq!(first.theme.pets.pet, "dewey");

    std::fs::write(&theme_path, "[theme.pets]\nenabled = true\n").expect("write torn theme");
    set_modified_time(
        &theme_path,
        std::time::SystemTime::now() + std::time::Duration::from_secs(1),
    );

    expire_load_memo();
    let during_rewrite = MachineConfig::load_with_memo(&config_path, agents_home.path());

    assert!(during_rewrite.theme.pets.enabled);
    assert_eq!(
        during_rewrite.theme.pets.pet, "dewey",
        "torn theme read must keep last-known-good pet"
    );

    std::fs::write(
        &theme_path,
        "[theme.pets]\nenabled = true\npet = \"seedy\"\n",
    )
    .expect("finish theme rewrite");
    set_modified_time(
        &theme_path,
        std::time::SystemTime::now() - std::time::Duration::from_secs(1),
    );

    expire_load_memo();
    let changed = MachineConfig::load_with_memo(&config_path, agents_home.path());
    assert!(changed.theme.pets.enabled);
    assert_eq!(changed.theme.pets.pet, "seedy");
}

#[test]
fn removed_agents_tables_fail_fast_with_the_rename() {
    let dir = tempdir().expect("tempdir");
    for (legacy, expected_detail) in [
        ("[tab]\nkeywords = []\n", "[tab]"),
        ("[agents.aliases]\nvim = \"nvim\"\n", "[agents.aliases]"),
        (
            "[agents.layouts]\nreview = \"claude,codex\"\n",
            "teams/<name>.md",
        ),
        (
            "[agents.loop.tasks.old]\n\
             spec = \"claude\"\n\
             prompt = \"wait\"\n\
             root = \"/repo\"\n\
             at = \"07:00\"\n",
            "loop.toml",
        ),
    ] {
        match load_no_fragments(&write_named(&dir, "config.toml", legacy)) {
            Err(ConfigErr::RemovedTable { detail, .. }) => {
                assert!(detail.contains(expected_detail), "{detail}");
            }
            other => panic!("expected RemovedTable for {legacy:?}, got {other:?}"),
        }
    }

    load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents]\nplacement = \"tab\"\n\n[agents.commands]\nvim = \"nvim\"\n",
    ))
    .expect("current agents config loads");
}

#[test]
fn agent_chain_length_defaults_parses_override_and_rejects_retired_key() {
    let dir = tempdir().expect("tempdir");
    let defaulted = load_no_fragments(&write_named(&dir, "config.toml", ""))
        .expect("load default agents config");
    assert_eq!(defaulted.agents.max_chain_length, 3);

    let tuned = load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents]\nmax-chain-length = 5\n",
    ))
    .expect("load chain length override");
    assert_eq!(tuned.agents.max_chain_length, 5);

    match load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents]\nmax-launch-depth = 1\n",
    )) {
        Err(ConfigErr::RemovedKey { detail, .. }) => {
            assert!(detail.contains("max-chain-length"), "{detail}");
            assert!(
                detail.contains("default also changed from 1 to 3"),
                "{detail}"
            );
        }
        other => panic!("expected RemovedKey for retired launch-depth key, got {other:?}"),
    }
}

#[test]
fn subagent_launch_defaults_parse_and_round_trip() {
    let defaulted: AgentsConfig = toml::from_str("").expect("parse defaults");
    assert_eq!(defaulted.subagents.timeout, "30m");

    let parsed: AgentsConfig = toml::from_str(
        "[subagents]\n\
         timeout = \"45m\"\n",
    )
    .expect("parse subagent defaults");
    assert_eq!(parsed.subagents.timeout, "45m");

    let encoded = toml::to_string(&parsed).expect("serialize agents");
    assert_eq!(
        toml::from_str::<AgentsConfig>(&encoded).expect("round-trip agents"),
        parsed
    );

    let parsed =
        toml::from_str::<AgentsConfig>("[subagents]\nbudget = \"5/day\"\n").expect("ignored key");
    assert_eq!(parsed.subagents, SubagentsConfig::default());
}

#[test]
fn forward_compat_keys_and_retired_sections_are_ignored() {
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write(
        &dir,
        "sound_profile = \"chime\"\n\
         [zellij]\n\
         default_mode = \"normal\"\n\
         [remote_control]\n\
         codex = true\n\
         capacity = 16\n\
         [worktree]\n\
         base = \"fresh\"\n\
         [sidebar]\n\
         refresh_ms = 100\n\
         focus_key = \"Alt+x\"\n",
    ))
    .expect("forward-compatible keys ignored");

    assert!(config.remote_control.enabled_for("codex"));
    assert!(!config.remote_control.enabled_for("claude"));
    assert_eq!(config.sidebar.focus_key, "Alt+x");
    assert_eq!(config.zellij, ZellijConfig::default());
    assert_eq!(config.agents.worktree, WorktreeConfig::default());
}

#[test]
fn team_owns_and_flip_compact_parse_default_and_round_trip() {
    use crate::store::message::AutoCompact;

    let harness: HarnessConfig =
        toml::from_str("flip_compact = \"180k\"").expect("parse harness threshold");
    assert_eq!(harness.flip_compact, Some(AutoCompact::Tokens(180_000)));
    let encoded = toml::to_string(&harness).expect("serialize harness threshold");
    assert_eq!(
        toml::from_str::<HarnessConfig>(&encoded).expect("round trip"),
        harness
    );
    assert_eq!(HarnessConfig::default().flip_compact, None);
    let team: Team = toml::from_str(
        r#"
        [[roles]]
        role = "planner"
        profile = "claude"
        owns = ["Explore", "Plan", "Reflect"]
        flip-compact = "220k"
        [[roles]]
        role = "coder"
        profile = "codex"
        [[roles]]
        role = "reviewer"
        profile = "claude"
        flip-compact = "off"
        "#,
    )
    .expect("parse ownership");
    assert_eq!(
        team.roles[0].flip_compact,
        Some(agents::FlipCompact::Threshold(AutoCompact::Tokens(220_000)))
    );
    assert!(team.roles[1].owns.is_empty());
    assert_eq!(team.roles[1].flip_compact, None);
    assert_eq!(team.roles[2].flip_compact, Some(agents::FlipCompact::Off));
    assert_eq!(
        team.flip_compact("planner", harness.flip_compact),
        Some(AutoCompact::Tokens(220_000))
    );
    assert_eq!(
        team.flip_compact("coder", harness.flip_compact),
        harness.flip_compact
    );
    assert_eq!(team.flip_compact("coder", None), None);
    assert_eq!(team.flip_compact("reviewer", harness.flip_compact), None);
    assert_eq!(team.owner_of("Plan"), Some("planner"));
    assert_eq!(team.owner_of("plan"), None);
    assert_eq!(team.owner_of(DONE_STAGE), None);
    assert_eq!(
        team.owned_stages().collect::<Vec<_>>(),
        ["Explore", "Plan", "Reflect"]
    );
    let encoded = toml::to_string(&team).expect("serialize ownership");
    assert_eq!(toml::from_str::<Team>(&encoded).expect("round trip"), team);
    let defaults = toml::to_string(&team.roles[1]).expect("serialize defaults");
    assert!(!defaults.contains("owns"));
    assert!(!defaults.contains("flip-compact"));
    for raw in ["70%", "off"] {
        let role: RoleBinding = toml::from_str(&format!(
            "role = \"coder\"\nprofile = \"codex\"\nflip-compact = \"{raw}\""
        ))
        .expect("parse role override");
        let encoded = toml::to_string(&role).expect("serialize role override");
        assert_eq!(
            toml::from_str::<RoleBinding>(&encoded).expect("round trip"),
            role
        );
    }
    assert!(
        toml::from_str::<RoleBinding>(
            "role = \"coder\"\nprofile = \"codex\"\nflip-compact = \"101%\""
        )
        .is_err()
    );
}

#[test]
fn team_signal_bindings_parse_default_and_round_trip() {
    let team: Team = toml::from_str(
        r#"
        [[roles]]
        role = "coder"
        profile = "codex"
        signals = [
            { signal = "ci.failed" },
            { signal = "agent.idle", match = { handle = "reviewer" }, prompt = "Review the result" },
        ]
        [[roles]]
        role = "reviewer"
        profile = "claude"
        "#,
    )
    .expect("parse bindings");
    let signals = &team.roles[0].signals;
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].signal, "ci.failed");
    assert!(signals[0].matches.is_empty());
    assert!(signals[0].prompt.is_none());
    assert_eq!(signals[1].matches["handle"], "reviewer");
    assert_eq!(signals[1].prompt.as_deref(), Some("Review the result"));
    assert!(team.roles[1].signals.is_empty());
    let encoded = toml::to_string(&team).expect("serialize bindings");
    assert_eq!(toml::from_str::<Team>(&encoded).expect("round trip"), team);
    for raw in ["", "signals = []"] {
        let empty: RoleBinding =
            toml::from_str(&format!("role = \"coder\"\nprofile = \"codex\"\n{raw}"))
                .expect("default bindings");
        assert!(empty.signals.is_empty());
        assert!(
            !toml::to_string(&empty)
                .expect("serialize empty")
                .contains("signals")
        );
    }
    for signals in [
        r#"["ci.failed"]"#,
        r#"[{ prompt = "missing signal" }]"#,
        r#"[{ signal = "ci.failed", match = { branch = 1 } }]"#,
    ] {
        assert!(
            toml::from_str::<RoleBinding>(&format!(
                "role = \"coder\"\nprofile = \"codex\"\nsignals = {signals}"
            ))
            .is_err()
        );
    }
}

#[test]
fn team_scratch_files_parse_default_and_round_trip() {
    let team: Team = toml::from_str(
        "layout = \"planner\"\n\
         scratch-files = [\"/plan.md\", \"/*-notes.md\"]\n\
         [[roles]]\n\
         role = \"planner\"\n\
         profile = \"claude\"\n",
    )
    .expect("parse team");
    assert_eq!(
        team.scratch_files.as_deref(),
        Some(&["/plan.md".to_owned(), "/*-notes.md".to_owned()][..])
    );

    let encoded = toml::to_string(&team).expect("serialize team");
    assert!(encoded.contains("scratch-files = ["));
    assert_eq!(
        toml::from_str::<Team>(&encoded).expect("round-trip team"),
        team
    );

    let defaulted: Team = toml::from_str("").expect("parse empty team");
    assert_eq!(defaulted.scratch_files, None);
    assert!(defaulted.scratch_patterns().is_empty());
    assert!(
        !toml::to_string(&defaulted)
            .expect("serialize default team")
            .contains("scratch-files")
    );

    let staged: Team = toml::from_str("stages = [\"Build\"]").expect("parse staged team");
    assert_eq!(staged.scratch_patterns(), ["/blackboard.md", "/*-notes.md"]);

    let none: Team =
        toml::from_str("stages = [\"Build\"]\nscratch-files = []").expect("parse empty list");
    assert_eq!(none.scratch_files, Some(Vec::new()));
    assert!(none.scratch_patterns().is_empty());
    assert_eq!(
        toml::from_str::<Team>(&toml::to_string(&none).expect("serialize empty list"))
            .expect("round-trip empty list"),
        none
    );
}

#[test]
fn worktree_config_defaults_and_parses() {
    let dir = tempdir().expect("tempdir");
    let defaults_dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&defaults_dir, "")).expect("load");
    assert_eq!(defaults.agents.worktree.dir, "../{repo}-worktrees");
    assert_eq!(defaults.agents.worktree.base, WorktreeBase::Head);

    let config = load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents.worktree]\n\
             dir = \"../wt-{repo}\"\n\
             base = \"fresh\"\n",
    ))
    .expect("load");
    assert_eq!(config.agents.worktree.dir, "../wt-{repo}");
    assert_eq!(config.agents.worktree.base, WorktreeBase::Fresh);

    let explicit = load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents.worktree]\nbase = \"main\"\n",
    ))
    .expect("load");
    assert_eq!(
        explicit.agents.worktree.base,
        WorktreeBase::Explicit("main".to_owned())
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.worktree]\nbase = \"\"\n",
        ))
        .is_err()
    );

    assert_eq!(
        " head ".parse::<WorktreeBase>().unwrap(),
        WorktreeBase::Head
    );
    assert_eq!(
        " fresh ".parse::<WorktreeBase>().unwrap(),
        WorktreeBase::Fresh
    );
    assert_eq!(
        " refs/heads/release ".parse::<WorktreeBase>().unwrap(),
        WorktreeBase::Explicit("refs/heads/release".to_owned())
    );
    assert_eq!(
        "   ".parse::<WorktreeBase>().unwrap_err().to_string(),
        "worktree base cannot be empty"
    );

    let serde_config: WorktreeConfig =
        toml::from_str("base = \"  fresh  \"").expect("whitespace-trimmed worktree base");
    assert_eq!(serde_config.base, WorktreeBase::Fresh);
    let serde_error =
        toml::from_str::<WorktreeConfig>("base = \"   \"").expect_err("empty serde worktree base");
    assert!(
        serde_error
            .to_string()
            .contains("worktree base cannot be empty")
    );
}

#[test]
fn loop_tasks_parse_and_default_empty() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::default().r#loop.tasks.0.is_empty(),
        "no loop tasks ship by default",
    );
    let config = load_no_fragments(&write_named(
        &dir,
        "loop.toml",
        "[tasks.morning]\n\
             agent = \"claude\"\n\
             prompt = \"triage\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n\
             every = \"weekdays\"\n\
             worktree = \"main\"\n\
             mode = \"auto\"\n\
             effort = \"low\"\n\
             system-prompt-file = \"/prompts/triage.md\"\n\
             timeout = \"5m\"\n\
             [tasks.pr_watch]\n\
             agent = \"codex\"\n\
             prompt-file = \"prompts/pr-watch.md\"\n\
             root = \"/home/me/app\"\n\
             every = \"15m\"\n\
             [tasks.self_wait]\n\
             wait = { kind = \"claude\", session = \"sess-1\", handle = \"@planner\" }\n\
             prompt = \"pick up the review\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n",
    ))
    .expect("load");
    let entry = config.r#loop.tasks.0.get("morning").expect("morning task");
    assert_eq!(entry.agent.as_deref(), Some("claude"));
    assert_eq!(entry.wait, None);
    assert_eq!(entry.prompt.as_deref(), Some("triage"));
    assert_eq!(entry.root, std::path::Path::new("/home/me/app"));
    assert_eq!(entry.at.as_deref(), Some("07:00"));
    assert_eq!(entry.every.as_deref(), Some("weekdays"));
    assert_eq!(entry.worktree.as_deref(), Some("main"));
    assert_eq!(entry.mode.as_deref(), Some("auto"));
    assert_eq!(entry.effort.as_deref(), Some("low"));
    assert_eq!(
        entry.system_prompt_file.as_deref(),
        Some(std::path::Path::new("/prompts/triage.md"))
    );
    assert_eq!(entry.timeout.as_deref(), Some("5m"));
    assert_eq!(entry.cron, None);

    let general = config.r#loop.tasks.0.get("pr_watch").expect("general task");
    assert_eq!(general.agent.as_deref(), Some("codex"));
    assert_eq!(
        general.prompt_file.as_deref(),
        Some(std::path::Path::new("prompts/pr-watch.md"))
    );
    assert_eq!(general.every.as_deref(), Some("15m"));

    let bound = config.r#loop.tasks.0.get("self_wait").expect("bind task");
    assert_eq!(bound.agent, None);
    let target = bound.wait.as_ref().expect("target");
    assert_eq!(target.kind, "claude");
    assert_eq!(target.session, "sess-1");
    assert_eq!(target.handle, "@planner");
}

#[test]
fn loop_task_budgets_validate_during_config_load() {
    let err = expect_err(
        "loop.toml",
        "[tasks.nightly]\nagent = \"codex\"\nprompt = \"work\"\nroot = \"/repo\"\nevery = \"day\"\nbudget-per-day = \"$20.00\"\n",
    );
    assert_config_err(err, ExpectedErr::Loop);

    let err = expect_err(
        "loop.toml",
        "[tasks.nightly]\nagent = \"codex\"\nprompt = \"work\"\nroot = \"/repo\"\nevery = \"day\"\nbudget = \"many dollars\"\n",
    );
    assert_config_err(err, ExpectedErr::Loop);
}

#[test]
fn account_budgets_validate_strictly_without_erasing_lenient_preflight_detail() {
    for kind in ["antigravity", "amp", "unknown"] {
        let text = format!("[accounts.budget]\n{kind} = \"50/day\"\n");
        let err = expect_err("config.toml", &text);
        assert_config_err(err, ExpectedErr::AccountBudget);

        let dir = tempdir().expect("tempdir");
        let path = write(&dir, &text);
        let lenient = load_lenient_no_fragments(&path);
        assert_eq!(
            lenient.accounts.budget(kind).map(DayCap::as_usd),
            Some(50.0)
        );
    }

    let valid = expect_err(
        "config.toml",
        "[accounts.budget]\nclaude = \"50/day\"\nnot-a-kind = \"20/day\"\n",
    );
    assert!(valid.to_string().contains("accounts.budget.not-a-kind"));
}

#[test]
fn load_from_surfaces_typed_config_errors() {
    for (file, text, expected) in [
        (
            "config.toml",
            "[remote_control]\nclaude = \"yes\"\n",
            ExpectedErr::Parse,
        ),
        (
            "config.toml",
            "[agents.worktree]\nbase = \"\"\n",
            ExpectedErr::Parse,
        ),
        (
            "config.toml",
            "[notifications]\n\
             [[notifications.handler]]\n\
             name = \"bad\"\n\
             command = \"notify {{nope}}\"\n",
            ExpectedErr::Notifications,
        ),
        ("theme.toml", "[theme]\ngood = 300\n", ExpectedErr::Parse),
        (
            "theme.toml",
            "[theme]\nselection = \"#bad\"\n",
            ExpectedErr::Parse,
        ),
        (
            "theme.toml",
            "[theme.glyphs.unicode.tokens]\ntotal = \"abc\"\n",
            ExpectedErr::Parse,
        ),
        (
            "theme.toml",
            "[theme.glyphs.unicode.makr]\ntotal = \"Σ\"\n",
            ExpectedErr::Parse,
        ),
        (
            "theme.toml",
            "[theme.animations.idle]\nframes = [\"...\"]\n",
            ExpectedErr::Parse,
        ),
    ] {
        assert_config_err(expect_err(file, text), expected);
    }
}

#[test]
fn zellij_room_options_parse_and_defaults_are_agent_friendly() {
    let dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&dir, "")).expect("load");
    assert_eq!(defaults.zellij.mouse_mode, None);
    assert_eq!(defaults.zellij.pane_frames, None);
    assert_eq!(defaults.zellij.copy_clipboard, None);
    assert!(defaults.zellij.mouse_click_through);
    assert!(!defaults.zellij.focus_follows_mouse);
    assert!(!defaults.zellij.session_serialization);

    let config = load_no_fragments(&write(
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
             on_force_close = \"quit\"\n",
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
    assert!(config.zellij.mouse_click_through);
}

#[test]
fn tmux_room_options_parse_and_defaults_are_agent_friendly() {
    let dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&dir, "")).expect("load");
    assert!(defaults.tmux.mouse);
    assert!(defaults.tmux.focus_events);
    assert_eq!(defaults.tmux.history_limit, 100_000);
    assert!(defaults.tmux.allow_passthrough);
    assert_eq!(defaults.tmux.set_clipboard, TmuxSetClipboard::On);
    assert!(defaults.tmux.extended_keys);
    assert_eq!(
        defaults.tmux.extended_keys_format,
        TmuxExtendedKeysFormat::CsiU,
    );
    assert_eq!(defaults.tmux.escape_time_ms, 10);
    assert!(defaults.tmux.renumber_windows);
    assert!(defaults.tmux.aggressive_resize);
    assert_eq!(defaults.tmux.pane_border_status, None);
    assert_eq!(defaults.tmux.pane_border_lines, None);

    let config = load_no_fragments(&write(
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
fn mux_default_parse_and_defaults_to_unset() {
    let dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&dir, "")).expect("load");
    assert_eq!(defaults.mux.default, None);

    let config = load_no_fragments(&write(
        &dir,
        "[mux]\n\
             default = \"tmux\"\n",
    ))
    .expect("load");
    assert_eq!(config.mux.default, Some(MuxName::Tmux));
}

#[test]
fn display_numeric_bounds_parse_and_clamp_at_use() {
    let dir = tempdir().expect("tempdir");
    let absent = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nmax_cols = 100\nrefresh_ms = 80\n",
    ))
    .expect("load");
    assert_eq!(absent.theme.display.width_percent, None);

    let config = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nwidth_percent = 25\nmax_cols = 100\nrefresh_ms = 80\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.width_percent, Some(25));
    assert_eq!(
        config.theme.display.max_cols,
        NonZeroU16::new(100).expect("nonzero")
    );
    assert_eq!(config.theme.display.refresh_ms, 80);
    assert_eq!(config.theme.display.resolved_refresh_ms(), 80);
    assert_eq!(MachineConfig::default().theme.display.width_percent, None);
    assert_eq!(MachineConfig::default().theme.display.max_cols.get(), 72);
    assert_eq!(
        MachineConfig::default().theme.display.refresh_ms,
        crate::config::DEFAULT_REFRESH_MS
    );

    assert!(
        load_no_fragments(&write_named(
            &dir,
            "theme.toml",
            "[theme.display]\nmax_cols = 0\n"
        ))
        .is_err()
    );

    let too_low = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nrefresh_ms = 1\n",
    ))
    .expect("load");
    assert_eq!(
        too_low.theme.display.resolved_refresh_ms(),
        crate::config::MIN_REFRESH_MS
    );

    let too_high = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nrefresh_ms = 5000\n",
    ))
    .expect("load");
    assert_eq!(
        too_high.theme.display.resolved_refresh_ms(),
        crate::config::MAX_REFRESH_MS
    );
}

#[test]
fn display_enums_lists_and_nested_bands_parse() {
    let dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::default().theme.display;
    assert_eq!(defaults.pixel, PixelMode::Auto);
    assert_eq!(defaults.scrollbar, ScrollbarMode::Auto);
    assert_eq!(defaults.max_provider_blocks, 3);
    assert_eq!(defaults.provider_tabs, ProviderTabsMode::Auto);
    assert!(defaults.provider_list.is_empty());
    assert!(defaults.context_meter.log_scale);
    assert_eq!(
        (
            defaults.context_meter.green.percent,
            defaults.context_meter.green.tokens
        ),
        (50, 128_000)
    );
    assert_eq!(
        (
            defaults.context_meter.yellow.percent,
            defaults.context_meter.yellow.tokens
        ),
        (70, 192_000)
    );
    assert_eq!(
        (
            defaults.context_meter.amber.percent,
            defaults.context_meter.amber.tokens
        ),
        (80, 256_000)
    );
    assert_eq!(
        (
            defaults.context_meter.red.percent,
            defaults.context_meter.red.tokens
        ),
        (90, 384_000)
    );
    assert_eq!(
        (
            defaults.budget_bar.yellow,
            defaults.budget_bar.amber,
            defaults.budget_bar.red
        ),
        (50, 25, 10)
    );
    assert_eq!(
        (
            defaults.budget_bar.burn_rate.green,
            defaults.budget_bar.burn_rate.deep_green,
            defaults.budget_bar.burn_rate.yellow,
            defaults.budget_bar.burn_rate.amber,
            defaults.budget_bar.burn_rate.red
        ),
        (67, 33, 100, 150, 200)
    );
    assert_eq!(
        (
            defaults.highlight_steps.band,
            defaults.highlight_steps.wash,
            defaults.highlight_steps.indexed
        ),
        (5, 1, 4)
    );

    let config = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\n\
             scrollbar = \"never\"\n\
             pixel = \"off\"\n\
             provider_tabs = \"always\"\n\
             provider_list = [\"codex\", \"all\"]\n\
             [theme.display.context_meter]\n\
             log_scale = false\n\
             red = { percent = 50, tokens = 100000 }\n\
             [theme.display.budget_bar]\n\
             red = 20\n\
             [theme.display.budget_bar.burn_rate]\n\
             green = 60\n\
             deep_green = 25\n\
             red = 300\n\
             [theme.display.highlight_steps]\n\
             band = 10\n",
    ))
    .expect("load");
    let display = &config.theme.display;
    assert_eq!(display.pixel, PixelMode::Off);
    assert_eq!(display.scrollbar, ScrollbarMode::Never);
    assert_eq!(display.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(display.provider_list, vec!["codex", "all"]);
    assert_eq!(display.max_provider_blocks, 3);
    assert!(!display.context_meter.log_scale);
    assert_eq!(
        display.context_meter.red,
        ContextBand {
            percent: 50,
            tokens: 100_000
        }
    );
    assert_eq!(display.context_meter.green, defaults.context_meter.green);
    assert_eq!(display.context_meter.yellow, defaults.context_meter.yellow);
    assert_eq!(display.context_meter.amber, defaults.context_meter.amber);
    assert_eq!(
        (
            display.budget_bar.yellow,
            display.budget_bar.amber,
            display.budget_bar.red
        ),
        (defaults.budget_bar.yellow, defaults.budget_bar.amber, 20)
    );
    assert_eq!(
        (
            display.budget_bar.burn_rate.green,
            display.budget_bar.burn_rate.deep_green,
            display.budget_bar.burn_rate.yellow,
            display.budget_bar.burn_rate.amber,
            display.budget_bar.burn_rate.red
        ),
        (
            60,
            25,
            defaults.budget_bar.burn_rate.yellow,
            defaults.budget_bar.burn_rate.amber,
            300
        )
    );
    assert_eq!(
        (
            display.highlight_steps.band,
            display.highlight_steps.wash,
            display.highlight_steps.indexed
        ),
        (
            10,
            defaults.highlight_steps.wash,
            defaults.highlight_steps.indexed
        )
    );

    let round_tripped: DisplayConfig =
        toml::from_str(&toml::to_string(display).expect("serialize display"))
            .expect("parse display");
    assert_eq!(round_tripped, *display);

    assert!(
        load_no_fragments(&write_named(
            &dir,
            "theme.toml",
            "[theme.display]\nscrollbar = \"bogus\"\n"
        ))
        .is_err()
    );
}

#[test]
fn sidebar_fields_parse_defaults_and_reject_zero() {
    let dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::default();
    assert_eq!(defaults.sidebar.trunk, None);
    assert_eq!(
        defaults.sidebar.spend_window,
        crate::agents::SpendWindowMode::Session
    );
    assert_eq!(defaults.timezone, None);
    assert_eq!(
        defaults.sidebar.afk_after_secs.get(),
        SidebarConfig::default().afk_after_secs.get()
    );
    assert_eq!(defaults.sidebar.afk_after_ms(), 15 * 60 * 1_000);
    assert_eq!(
        SidebarConfig::key_label(&defaults.sidebar.focus_key),
        Some("Alt+p")
    );
    assert_eq!(
        SidebarConfig::key_label(&defaults.sidebar.zoom_key),
        Some("Alt+g")
    );

    let config = load_no_fragments(&write(
        &dir,
        "timezone = \"America/New_York\"\n\
         [sidebar]\n\
         trunk = \"develop\"\n\
         spend_window = \"session\"\n\
         afk_after_secs = 60\n\
         focus_key = \"Alt+x\"\n\
         zoom_key = \"off\"\n",
    ))
    .expect("load");
    assert_eq!(config.sidebar.trunk.as_deref(), Some("develop"));
    assert_eq!(
        config.sidebar.spend_window,
        crate::agents::SpendWindowMode::Session
    );
    assert_eq!(config.timezone.as_deref(), Some("America/New_York"));
    assert_eq!(
        config.headline_spec().timezone.as_deref(),
        Some("America/New_York")
    );
    assert_eq!(config.sidebar.afk_after_secs.get(), 60);
    assert_eq!(config.sidebar.afk_after_ms(), 60_000);
    assert_eq!(config.sidebar.focus_key, "Alt+x");
    assert_eq!(SidebarConfig::key_label(&config.sidebar.zoom_key), None);

    assert!(
        load_no_fragments(&write(&dir, "[sidebar]\nafk_after_secs = 0\n")).is_err(),
        "zero cannot disable the AFK badge"
    );
}

#[test]
fn attention_config_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write(&dir, "")).expect("load");
    assert_eq!(
        config.agents.attention.active_grace_secs.get(),
        crate::agents::DEFAULT_ACTIVE_GRACE_SECS,
    );
    assert_eq!(
        config.agents.attention.stalled_after_secs.get(),
        crate::agents::DEFAULT_STALL_AFTER_SECS,
    );
    assert_eq!(
        config.agents.attention.tool_repeat_warn_after.get(),
        crate::agents::DEFAULT_TOOL_REPEAT_WARN_AFTER,
    );
    assert_eq!(
        config.agents.attention.tool_repeat_attention_after.get(),
        crate::agents::DEFAULT_TOOL_REPEAT_ATTENTION_AFTER,
    );
    assert_eq!(
        config.agents.attention.archive_after_secs.get(),
        crate::agents::DEFAULT_ARCHIVE_AFTER_SECS,
    );

    let tuned = load_no_fragments(&write_named(
        &dir,
        "config.toml",
        "[agents.attention]\nactive_grace_secs = 60\nstalled_after_secs = 2700\ntool_repeat_warn_after = 4\ntool_repeat_attention_after = 30\narchive_after_secs = 7200\n",
    ))
    .expect("load");
    assert_eq!(tuned.agents.attention.active_grace_secs.get(), 60);
    assert_eq!(tuned.agents.attention.stalled_after_secs.get(), 2700);
    assert_eq!(tuned.agents.attention.tool_repeat_warn_after.get(), 4);
    assert_eq!(tuned.agents.attention.tool_repeat_attention_after.get(), 30);
    assert_eq!(tuned.agents.attention.archive_after_secs.get(), 7200);

    let partial =
        load_no_fragments(&write_named(&dir, "config.toml", "[agents.attention]\n")).expect("load");
    assert_eq!(partial.agents.attention, AttentionConfig::default());

    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.attention]\nactive_grace_secs = 0\n",
        ))
        .is_err()
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.attention]\nstalled_after_secs = 0\n",
        ))
        .is_err()
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.attention]\ntool_repeat_warn_after = 0\n",
        ))
        .is_err()
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.attention]\ntool_repeat_attention_after = 0\n",
        ))
        .is_err()
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "config.toml",
            "[agents.attention]\narchive_after_secs = 0\n",
        ))
        .is_err()
    );
}

#[test]
fn theme_sub_tables_wire_through_theme_file() {
    let dir = tempdir().expect("tempdir");
    assert!(MachineConfig::default().theme.is_unset());
    assert!(MachineConfig::default().theme.animations.is_unset());
    assert!(MachineConfig::default().theme.glyphs.is_unset());

    let config = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme]\n\
             mode = 256\n\
             scheme = \"TokyoNight Night\"\n\
             good = 34\n\
             selection = \"#8ab3e0\"\n\
             [theme.animations.thinking]\n\
             frames = \"⠁⠂\"\n\
             color = \"clay\"\n\
             speed = \"slow\"\n\
             [theme.animations.idle]\n\
             effect = \"breathe\"\n\
             [theme.glyphs]\n\
             set = \"nerd_font\"\n\
             [theme.glyphs.nerd_font.tokens]\n\
             total = \"◇\"\n\
             [theme.providers.claude]\n\
             color = \"#D97757\"\n\
             ascii_art = \" ▐▛███▜▌\"\n",
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

    assert_eq!(config.theme.glyphs.set.as_deref(), Some("nerd_font"));
    assert_eq!(
        config
            .theme
            .glyphs
            .glyph("nerd_font", crate::config::GlyphRole::TokensTotal),
        Some("◇")
    );

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
fn sidebar_pets_defaults_parse_and_round_trip() {
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.pets]\nenabled = true\npet = \"dewey\"\nglyphs = \"pixel\"\ncell_aspect = 2.5\nvoice = false\n",
    ))
    .expect("load");
    assert!(config.theme.pets.enabled);
    assert_eq!(config.theme.pets.pet, "dewey");
    assert_eq!(config.theme.pets.glyphs, PetsGlyphMode::Pixel);
    assert_eq!(config.theme.pets.cell_aspect, CellAspect::from_ratio(2.5));
    assert!(!config.theme.pets.voice);

    let defaults_dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&defaults_dir, "")).expect("load");
    assert_eq!(defaults.theme.pets, PetsConfig::default());
    assert!(defaults.theme.pets.is_default());

    let encoded = toml::to_string(&config.theme.pets).expect("serialize pets");
    let round_tripped: PetsConfig = toml::from_str(&encoded).expect("parse pets");
    assert_eq!(round_tripped, config.theme.pets);
}

#[test]
fn notifications_parse_per_machine_preferences() {
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write(
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
             title = \"RimZ: {{agent}} {{kind}}\"\n\
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
        Some("RimZ: {{agent}} {{kind}}")
    );
    assert_eq!(config.notifications.body.as_deref(), Some("{{task}}"));
    assert_eq!(config.notifications.command(), Some("ntfy publish rimz"));
}

#[test]
fn web_enabled_defaults_on_and_parses_off() {
    assert!(WebPrefs::default().enabled);
    assert_eq!(WebPrefs::default().port, 8200);
    assert_eq!(WebPrefs::default().share_port, 8201);
    assert_eq!(WebPrefs::default().interface, "127.0.0.1");
    assert!(WebPrefs::default().auth_header.is_none());
    assert!(WebPrefs::default().auth_users.is_empty());
    assert!(WebPrefs::default().trusted_proxies.is_empty());

    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write(&dir, "[web]\nenabled = false\n")).expect("load");
    assert!(!config.web.enabled);
}

#[test]
fn scalar_sections_parse_non_default_values() {
    let cases: [(&str, ConfigAssertion); 3] = [
        (
            "[sentry]\n\
             dsn = \"https://key@o1.ingest.sentry.io/2\"\n\
             environment = \"dev\"\n",
            assert_sentry_config,
        ),
        (
            "[web]\n\
             enabled = false\n\
             port = 9123\n\
             share_port = 9124\n\
             interface = \"0.0.0.0\"\n\
             base_url = \"https://devbox.example/rimz\"\n\
             share_base_url = \"https://watch.example/rimz\"\n\
             auth_header = \"X-Forwarded-User\"\n\
             auth_users = [\"alice\", \"bob\"]\n\
             trusted_proxies = [\"10.0.0.0/8\", \"fd00::/8\"]\n\
             font = \"FiraCode Nerd Font Mono\"\n\
             font_source = \"/tmp/font.woff2\"\n\
             style_client = false\n",
            assert_web_config,
        ),
        (
            "[remote_control]\n\
             claude = true\n\
             codex = true\n",
            assert_remote_control_config,
        ),
    ];

    for (text, assert_config) in cases {
        let dir = tempdir().expect("tempdir");
        let config = load_no_fragments(&write(&dir, text)).expect("load");
        assert_config(&config);
    }
}

#[test]
fn web_auth_users_round_trip() {
    let prefs: WebPrefs = toml::from_str("auth_users = [\"alice\", \"bob\"]\n").expect("parse");
    let encoded = toml::to_string(&prefs).expect("serialize");
    let round_tripped: WebPrefs = toml::from_str(&encoded).expect("parse serialized prefs");

    assert_eq!(round_tripped, prefs);
    assert_eq!(round_tripped.auth_users, ["alice", "bob"]);
}

fn write_definition(root: &Path, path: &str, fields: &str, body: &str) -> PathBuf {
    let frontmatter: serde_json::Value = serde_saphyr::from_str(fields).unwrap();
    if let Some(kind) = frontmatter["agent"]
        .as_str()
        .filter(|kind| crate::agents::find_definition(kind).is_some())
    {
        let base = format!("agents/{kind}.md");
        if !root.join(&base).exists() {
            write_definition(
                root,
                &base,
                &format!("description: {kind} base"),
                &format!("{kind} base."),
            );
        }
    }
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("---\n{fields}\n---\n{body}")).unwrap();
    path
}

#[test]
fn definitions_load_namespaces_chains_and_sources_and_ignore_legacy_toml() {
    let dir = tempdir().unwrap();
    let path = write(
        &dir,
        "[agents]\nplacement = 'tab'\n[agents.commands]\nvim = 'nvim'\n",
    );
    std::fs::write(dir.path().join("agents.toml"), "not = = toml").unwrap();
    std::fs::create_dir_all(dir.path().join("profiles/old")).unwrap();
    std::fs::write(dir.path().join("profiles/old/agent.toml"), "not = = toml").unwrap();
    let base = write_definition(dir.path(), "agents/codex.md", "description: Base", "Base.");
    write_definition(dir.path(), "traits/craft.md", "", "Care.");
    write_definition(
        dir.path(),
        "agents/parent.md",
        "description: Parent\nagent: codex\ntools: []\neffort: high\ntraits: [craft]",
        "Parent. ${traits}",
    );
    let child = write_definition(
        dir.path(),
        "agents/child.md",
        "description: Child\nagent: parent",
        "Child.",
    );
    let subagent = write_definition(
        dir.path(),
        "subagents/reviewer.md",
        "description: Reviewer\nagent: codex\ntools: []",
        "",
    );
    let (config, sources) =
        MachineConfig::load_from_with_agent_spec_sources(&path, dir.path()).unwrap();
    assert_eq!(config.agents.placement, LaunchPlacement::Tab);
    assert_eq!(
        config.agents.profiles.0["child"].effort.as_deref(),
        Some("high")
    );
    assert_eq!(
        config.agents.profiles.0["child"]
            .append_system_prompt_files
            .len(),
        2
    );
    assert_eq!(
        sources.profile(effective::ProfileScope::Agents, "child"),
        Some(child.as_path())
    );
    assert_eq!(
        sources.profile(effective::ProfileScope::Agents, "codex"),
        Some(base.as_path())
    );
    assert_eq!(
        sources.profile(effective::ProfileScope::Subagents, "reviewer"),
        Some(subagent.as_path())
    );
    assert_eq!(sources.command("vim"), Some(path.as_path()));
    assert!(config.agents.teams.0.contains_key("peer"));
    assert!(!config.agents.profiles.0.contains_key("reviewer"));
    assert!(config.notices.definition_errors.is_empty());
}

#[test]
fn definition_failures_keep_good_siblings_and_match_doctor_and_launch_preconditions() {
    let dir = tempdir().unwrap();
    let path = write(&dir, "");
    write_definition(
        dir.path(),
        "agents/good.md",
        "description: Good\nagent: codex\ntools: []",
        "",
    );
    let bad = write_definition(
        dir.path(),
        "agents/bad.md",
        "description: Bad\nagent: missing",
        "",
    );
    let strict = MachineConfig::load_from(&path, dir.path()).unwrap_err();
    assert!(matches!(strict, ConfigErr::Definition { .. }));
    assert_eq!(strict.path(), bad);
    let config = MachineConfig::load_lenient_from(&path, dir.path());
    assert!(config.agents.profiles.0.contains_key("good"));
    assert!(!config.agents.profiles.0.contains_key("bad"));
    assert_eq!(config.notices.definition_errors.len(), 1);
    assert_eq!(config.notices.definition_errors[0].path, bad);
    assert!(
        config
            .definition_failure()
            .unwrap()
            .contains(bad.to_str().unwrap())
    );
    let broken = broken_machine_files_in(&MachineConfigFiles::from_paths(path, dir.path()));
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].to_string(), strict.to_string());
}

#[test]
fn definition_stamp_tracks_definitions_traits_and_skill_files() {
    let dir = tempdir().unwrap();
    let path = write(&dir, "");
    let before = ConfigStamp::from_inputs(&path, dir.path());
    let source = write_definition(
        dir.path(),
        "agents/worker.md",
        "description: Worker\nagent: codex\ntools: []",
        "",
    );
    let added = ConfigStamp::from_inputs(&path, dir.path());
    assert_ne!(before, added);
    std::fs::write(
        &source,
        "---\ndescription: Changed\nagent: codex\ntools: []\n---\n",
    )
    .unwrap();
    let edited = ConfigStamp::from_inputs(&path, dir.path());
    assert_ne!(added, edited);
    write_definition(dir.path(), "traits/check.md", "", "Check.");
    let trait_added = ConfigStamp::from_inputs(&path, dir.path());
    assert_ne!(edited, trait_added);
    std::fs::remove_file(source).unwrap();
    let removed = ConfigStamp::from_inputs(&path, dir.path());
    assert_ne!(trait_added, removed);
    let skill = dir.path().join("skills/one");
    std::fs::create_dir_all(skill.join("agents")).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: one\n---\n").unwrap();
    let skill_added = ConfigStamp::from_inputs(&path, dir.path());
    assert_ne!(removed, skill_added);
    std::fs::write(
        skill.join("agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();
    assert_ne!(skill_added, ConfigStamp::from_inputs(&path, dir.path()));
}

#[test]
fn machine_definition_tables_are_removed_and_agents_preferences_stay_toml() {
    let dir = tempdir().unwrap();
    for (table, tree) in [
        ("agents.profiles", "agents"),
        ("agents.teams", "teams"),
        ("subagents.profiles", "subagents"),
        ("profiles", "agents"),
    ] {
        let path = write(&dir, &format!("[{table}]"));
        let ConfigErr::RemovedTable { detail, .. } = load_no_fragments(&path).unwrap_err() else {
            panic!("expected removed table")
        };
        assert!(detail.contains(&format!("<agents_home>/{tree}/<name>.md")));
    }
}

#[test]
fn sandbox_config_enables_definition_skill_library_checks() {
    let dir = tempdir().unwrap();
    write_definition(
        dir.path(),
        "agents/worker.md",
        "description: Worker\nagent: codex\ntools: [Skill]\nskills: [missing]",
        "",
    );
    let path = write(&dir, "[agents]\nisolation = 'host'\n");
    MachineConfig::load_from(&path, dir.path()).unwrap();
    write(&dir, "[agents]\nisolation = 'sandbox'\n");
    let error = MachineConfig::load_from(&path, dir.path()).unwrap_err();
    assert!(matches!(error, ConfigErr::Definition { .. }));
    assert!(error.to_string().contains("missing"));
}

#[test]
fn loaded_team_replaces_peer_and_retains_its_source() {
    let dir = tempdir().unwrap();
    let path = write(&dir, "");
    write_definition(dir.path(), "agents/codex.md", "description: Base", "Base.");
    write_definition(
        dir.path(),
        "agents/worker.md",
        "description: Worker\nagent: codex\ntools: []",
        "",
    );
    let team_path = write_definition(
        dir.path(),
        "teams/peer.md",
        "leader: lead\nstages: [Plan]\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]",
        "Pipeline.",
    );
    let (config, sources) =
        MachineConfig::load_from_with_agent_spec_sources(&path, dir.path()).unwrap();
    assert_eq!(config.agents.teams.0["peer"].roles[0].profile, "peer.lead");
    assert_eq!(sources.team("peer"), Some(team_path.as_path()));
    assert_eq!(
        sources.profile(effective::ProfileScope::Agents, "peer.lead"),
        Some(team_path.as_path())
    );
    assert_eq!(config.agents.profiles.0["peer.lead"].agent, "codex");
}

#[test]
fn lenient_definitions_collect_all_errors_and_post_load_validation_failure() {
    let dir = tempdir().unwrap();
    let path = write(&dir, "[agents.commands]\n'bad/name' = 'echo invalid'\n");
    write_definition(
        dir.path(),
        "agents/worker.md",
        "description: Worker\nagent: codex\ntools: []",
        "",
    );
    write_definition(
        dir.path(),
        "agents/bad.md",
        "description: Bad\nagent: unknown",
        "",
    );
    write_definition(
        dir.path(),
        "subagents/bad-child.md",
        "description: Bad child\nagent: unknown",
        "",
    );
    let config = MachineConfig::load_lenient_from(&path, dir.path());
    assert!(config.agents.profiles.0.contains_key("worker"));
    assert_eq!(
        config.notices.definition_errors.len(),
        3,
        "{:?}",
        config.notices.definition_errors
    );
    assert_eq!(config.notices.definition_errors[2].path, dir.path());
}
