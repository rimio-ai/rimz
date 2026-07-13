use super::*;
use crate::harness::run::PermissionMode;
use crate::ids::MuxName;
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

#[derive(Clone, Copy, Debug)]
enum ExpectedErr {
    Parse,
    Agents,
    Notifications,
    Loop,
}

fn expect_err(file: &str, text: &str) -> ConfigErr {
    let dir = tempdir().expect("tempdir");
    load_no_fragments(&write_named(&dir, file, text)).expect_err("config should fail")
}

fn assert_config_err(err: ConfigErr, expected: ExpectedErr) {
    match (&err, expected) {
        (ConfigErr::Parse { .. }, ExpectedErr::Parse)
        | (ConfigErr::Agents { .. }, ExpectedErr::Agents)
        | (ConfigErr::Notifications { .. }, ExpectedErr::Notifications) => {}
        (ConfigErr::Loop { .. }, ExpectedErr::Loop) => {}
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

fn assert_zellij_web_config(config: &MachineConfig) {
    assert!(!config.web.enabled);
    assert_eq!(
        config.web.zellij.base_url.as_deref(),
        Some("https://devbox.example/zellij")
    );
    assert!(!config.web.zellij.auto_start);
    assert_eq!(config.web.zellij.font, "FiraCode Nerd Font Mono");
    assert!(!config.web.zellij.style_client);
}

fn assert_remote_control_config(config: &MachineConfig) {
    assert!(config.remote_control.claude);
    assert!(config.remote_control.codex);
}

#[test]
fn missing_or_empty_file_is_default_off() {
    let dir = tempdir().expect("tempdir");
    for path in [dir.path().join("absent.toml"), write(&dir, "")] {
        let config = load_no_fragments(&path).expect("load");
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
    std::fs::write(dir.path().join(AGENTS_FILE), "").expect("write agents config");

    let errors = broken_machine_files_in(dir.path());

    assert_eq!(errors.len(), 1, "only theme.toml is broken: {errors:?}");
    match &errors[0] {
        ConfigErr::Parse { path, source } => {
            assert_eq!(path, &dir.path().join(THEME_FILE));
            let detail = source.to_string();
            assert!(
                detail.contains("duplicate key") && detail.contains("max_cols"),
                "precise duplicate-key error: {detail}",
            );
        }
        other => panic!("expected theme parse error, got {other:?}"),
    }
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

    let config = load_lenient_no_fragments(&config_path);
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

    let config = load_lenient_no_fragments(&config_path);
    assert!(config.remote_control.claude);
    assert_eq!(config.agents, AgentsConfig::default());
}

#[test]
fn lenient_load_falls_back_to_defaults_plus_agents_home() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    write_agents_home_fragment(
        agents_home.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "claude-planner",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.claude-planner]\nagent = \"claude\"\n",
    );
    let config_path = write_named(
        &dir,
        "agents.toml",
        "[agents.teams.review]\nlayout = \"missing-profile,codex\"\n",
    );

    let config = MachineConfig::load_lenient_from(&config_path, agents_home.path());

    assert!(!config.agents.teams.0.contains_key("review"));
    assert_eq!(
        config
            .agents
            .teams
            .0
            .get("peer")
            .and_then(|team| team.layout.as_deref()),
        Some("claude,codex")
    );
    assert_eq!(
        config
            .agents
            .profiles
            .0
            .get("claude-planner")
            .map(|profile| profile.agent.as_str()),
        Some("claude")
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
            budget: None,
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
fn absent_agents_home_is_noop_and_malformed_fragment_leaves_config_unchanged() {
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

    write_agents_home_fragment(
        root.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "broken",
        AGENT_FRAGMENT_FILE,
        "not = = toml",
    );
    agents.profiles.0.insert(
        "planner".to_owned(),
        Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
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
fn strict_load_validates_teams_after_agents_home_profiles_merge() {
    let dir = tempdir().expect("tempdir");
    let agents_home = tempdir().expect("agents home");
    write_agents_home_fragment(
        agents_home.path(),
        AGENTS_HOME_AGENTS_SUBDIR,
        "claude-planner",
        AGENT_FRAGMENT_FILE,
        "[agents.profiles.claude-planner]\nagent = \"claude\"\n",
    );
    write_agents_home_fragment(
        agents_home.path(),
        AGENTS_HOME_TEAMS_SUBDIR,
        "plan-code-review",
        TEAM_FRAGMENT_FILE,
        "[agents.teams.plan-code-review]\n\
             [[agents.teams.plan-code-review.roles]]\n\
             role = \"planner\"\n\
             profile = \"claude-planner\"\n",
    );
    let config_path = write_named(
        &dir,
        "agents.toml",
        "[agents.profiles.codex-reviewer]\n\
             agent = \"codex\"\n\
             [agents.teams.peer]\n\
             layout = \"claude-planner,codex-reviewer\"\n",
    );

    // Regression guard for cd200739: validation runs after ~/.agents fragments merge.
    let config = MachineConfig::load_from(&config_path, agents_home.path()).expect("load");

    assert_eq!(
        config
            .agents
            .teams
            .0
            .get("peer")
            .and_then(|team| team.layout.as_deref()),
        Some("claude-planner,codex-reviewer")
    );
    assert_eq!(
        config
            .agents
            .profiles
            .0
            .get("claude-planner")
            .map(|profile| profile.agent.as_str()),
        Some("claude")
    );
    assert_eq!(
        config
            .agents
            .profiles
            .0
            .get("codex-reviewer")
            .map(|profile| profile.agent.as_str()),
        Some("codex")
    );
    assert_eq!(
        config
            .agents
            .teams
            .0
            .get("plan-code-review")
            .and_then(|team| team.roles.first())
            .map(|role| role.profile.as_str()),
        Some("claude-planner")
    );
}

#[test]
fn removed_agents_tables_fail_fast_with_the_rename() {
    let dir = tempdir().expect("tempdir");
    for (legacy, expected_detail) in [
        ("[tab]\nkeywords = []\n", "[tab]"),
        ("[agents.aliases]\nvim = \"nvim\"\n", "[agents.aliases]"),
        (
            "[agents.layouts]\nreview = \"claude,codex\"\n",
            "[agents.teams]",
        ),
        (
            "[agents.loop.tasks.old]\n\
             spec = \"claude\"\n\
             prompt = \"wake\"\n\
             root = \"/repo\"\n\
             at = \"07:00\"\n",
            "loop.toml",
        ),
    ] {
        match load_no_fragments(&write_named(&dir, "agents.toml", legacy)) {
            Err(ConfigErr::RemovedTable { detail, .. }) => {
                assert!(detail.contains(expected_detail), "{detail}");
            }
            other => panic!("expected RemovedTable for {legacy:?}, got {other:?}"),
        }
    }

    load_no_fragments(&write_named(
        &dir,
        "agents.toml",
        "[agents]\nplacement = \"tab\"\n\n[agents.commands]\nvim = \"nvim\"\n",
    ))
    .expect("current agents config loads");
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

    assert!(config.remote_control.codex);
    assert!(!config.remote_control.claude);
    assert_eq!(config.sidebar.focus_key, "Alt+x");
    assert_eq!(config.zellij, ZellijConfig::default());
    assert_eq!(config.agents.worktree, WorktreeConfig::default());
}

#[test]
fn agent_profiles_commands_and_teams_parse() {
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write_named(
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
            budget: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned())
        })
    );
    assert_eq!(
        profiles.get("planner"),
        Some(&Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
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
    let dir = tempdir().expect("tempdir");
    let config = load_no_fragments(&write_named(
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

    let absolute = load_no_fragments(&write_named(
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
fn worktree_config_defaults_and_parses() {
    let dir = tempdir().expect("tempdir");
    let defaults_dir = tempdir().expect("tempdir");
    let defaults = load_no_fragments(&write(&defaults_dir, "")).expect("load");
    assert_eq!(defaults.agents.worktree.dir, "../{repo}-worktrees");
    assert_eq!(defaults.agents.worktree.base, WorktreeBase::Head);

    let config = load_no_fragments(&write_named(
        &dir,
        "agents.toml",
        "[agents.worktree]\n\
             dir = \"../wt-{repo}\"\n\
             base = \"fresh\"\n",
    ))
    .expect("load");
    assert_eq!(config.agents.worktree.dir, "../wt-{repo}");
    assert_eq!(config.agents.worktree.base, WorktreeBase::Fresh);

    let explicit = load_no_fragments(&write_named(
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
        load_no_fragments(&write_named(
            &dir,
            "agents.toml",
            "[agents.worktree]\nbase = \"\"\n",
        ))
        .is_err()
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
             agent = \"claude-ping\"\n\
             prompt = \"ping\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n\
             every = \"weekdays\"\n\
             worktree = \"main\"\n\
             mode = \"auto\"\n\
             effort = \"low\"\n\
             system-prompt-file = \"/prompts/primer.md\"\n\
             timeout = \"5m\"\n\
             [tasks.pr_watch]\n\
             agent = \"codex\"\n\
             prompt-file = \"prompts/pr-watch.md\"\n\
             root = \"/home/me/app\"\n\
             every = \"15m\"\n\
             [tasks.self_wake]\n\
             wake = { kind = \"claude\", session = \"sess-1\", handle = \"@planner\" }\n\
             prompt = \"pick up the review\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n",
    ))
    .expect("load");
    let entry = config.r#loop.tasks.0.get("morning").expect("morning task");
    assert_eq!(entry.agent.as_deref(), Some("claude-ping"));
    assert_eq!(entry.wake, None);
    assert_eq!(entry.prompt.as_deref(), Some("ping"));
    assert_eq!(entry.root, std::path::Path::new("/home/me/app"));
    assert_eq!(entry.at.as_deref(), Some("07:00"));
    assert_eq!(entry.every.as_deref(), Some("weekdays"));
    assert_eq!(entry.worktree.as_deref(), Some("main"));
    assert_eq!(entry.mode.as_deref(), Some("auto"));
    assert_eq!(entry.effort.as_deref(), Some("low"));
    assert_eq!(
        entry.system_prompt_file.as_deref(),
        Some(std::path::Path::new("/prompts/primer.md"))
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

    let bound = config.r#loop.tasks.0.get("self_wake").expect("bind task");
    assert_eq!(bound.agent, None);
    let target = bound.wake.as_ref().expect("target");
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
fn load_from_surfaces_typed_config_errors() {
    for (file, text, expected) in [
        (
            "config.toml",
            "[remote_control]\nclaude = \"yes\"\n",
            ExpectedErr::Parse,
        ),
        (
            "agents.toml",
            "[agents.profiles.mixed]\ncommand = \"nvim\"\nagent = \"claude\"\n",
            ExpectedErr::Parse,
        ),
        (
            "agents.toml",
            "[agents.profiles.missing_agent]\nmode = \"yolo\"\n",
            ExpectedErr::Parse,
        ),
        (
            "agents.toml",
            "[agents.profiles.term]\nagent = \"claude\"\n",
            ExpectedErr::Agents,
        ),
        (
            "agents.toml",
            "[agents.profiles.claude-2]\nagent = \"claude\"\n",
            ExpectedErr::Agents,
        ),
        (
            "agents.toml",
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
    assert_eq!(defaults.tmux.escape_time_ms, 0);
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
    assert_eq!(absent.theme.display.width_percent, 30);

    let config = load_no_fragments(&write_named(
        &dir,
        "theme.toml",
        "[theme.display]\nwidth_percent = 25\nmax_cols = 100\nrefresh_ms = 80\n",
    ))
    .expect("load");
    assert_eq!(config.theme.display.width_percent, 25);
    assert_eq!(
        config.theme.display.max_cols,
        NonZeroU16::new(100).expect("nonzero")
    );
    assert_eq!(config.theme.display.refresh_ms, 80);
    assert_eq!(config.theme.display.resolved_refresh_ms(), 80);
    assert_eq!(MachineConfig::default().theme.display.width_percent, 30);
    assert_eq!(MachineConfig::default().theme.display.max_cols.get(), 72);
    assert_eq!(
        MachineConfig::default().theme.display.refresh_ms,
        crate::sidebar::timing::DEFAULT_REFRESH_MS
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
        crate::sidebar::timing::MIN_REFRESH_MS
    );

    let too_high = load_no_fragments(&write_named(
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
fn display_enums_lists_and_nested_bands_parse() {
    let dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::default().theme.display;
    assert_eq!(defaults.scrollbar, ScrollbarMode::Auto);
    assert_eq!(defaults.max_provider_blocks, 3);
    assert_eq!(defaults.provider_tabs, ProviderTabsMode::Auto);
    assert!(defaults.provider_list.is_empty());
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
             provider_tabs = \"always\"\n\
             provider_list = [\"codex\", \"all\"]\n\
             [theme.display.context_meter]\n\
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
    assert_eq!(display.scrollbar, ScrollbarMode::Never);
    assert_eq!(display.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(display.provider_list, vec!["codex", "all"]);
    assert_eq!(display.max_provider_blocks, 3);
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
        DEFAULT_AFK_AFTER_SECS
    );
    assert_eq!(defaults.sidebar.afk_after_ms(), 15 * 60 * 1_000);

    let config = load_no_fragments(&write(
        &dir,
        "timezone = \"America/New_York\"\n\
         [sidebar]\n\
         trunk = \"develop\"\n\
         spend_window = \"session\"\n\
         afk_after_secs = 60\n\
         focus_key = \"Alt+x\"\n",
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
        config.agents.attention.stalled_after_secs.get(),
        crate::agents::DEFAULT_STALL_AFTER_SECS,
    );
    assert_eq!(
        config.agents.attention.archive_after_secs.get(),
        crate::agents::DEFAULT_ARCHIVE_AFTER_SECS,
    );

    let tuned = load_no_fragments(&write_named(
        &dir,
        "agents.toml",
        "[agents.attention]\nstalled_after_secs = 2700\narchive_after_secs = 7200\n",
    ))
    .expect("load");
    assert_eq!(tuned.agents.attention.stalled_after_secs.get(), 2700);
    assert_eq!(tuned.agents.attention.archive_after_secs.get(), 7200);

    let partial =
        load_no_fragments(&write_named(&dir, "agents.toml", "[agents.attention]\n")).expect("load");
    assert_eq!(partial.agents.attention, AttentionConfig::default());

    assert!(
        load_no_fragments(&write_named(
            &dir,
            "agents.toml",
            "[agents.attention]\nstalled_after_secs = 0\n",
        ))
        .is_err()
    );
    assert!(
        load_no_fragments(&write_named(
            &dir,
            "agents.toml",
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
fn web_enabled_defaults_on_and_parses_off() {
    assert!(WebPrefs::default().enabled);

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
             [web.zellij]\n\
             base_url = \"https://devbox.example/zellij\"\n\
             auto_start = false\n\
             font = \"FiraCode Nerd Font Mono\"\n\
             style_client = false\n",
            assert_zellij_web_config,
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
