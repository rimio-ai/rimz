use super::*;
use crate::run::PermissionMode;
use std::num::NonZeroU16;
use tempfile::tempdir;

fn write(dir: &tempfile::TempDir, text: &str) -> PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).expect("write config");
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
    }
}

#[test]
fn worktree_config_defaults_and_parses() {
    let dir = tempdir().expect("tempdir");
    let defaults = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(defaults.worktree.dir, "../{repo}-worktrees");
    assert_eq!(defaults.worktree.base, WorktreeBase::Head);

    let config = MachineConfig::load_from(&write(
        &dir,
        "[worktree]\n\
             dir = \"../wt-{repo}\"\n\
             base = \"fresh\"\n",
    ))
    .expect("load");
    assert_eq!(config.worktree.dir, "../wt-{repo}");
    assert_eq!(config.worktree.base, WorktreeBase::Fresh);

    let explicit =
        MachineConfig::load_from(&write(&dir, "[worktree]\nbase = \"main\"\n")).expect("load");
    assert_eq!(
        explicit.worktree.base,
        WorktreeBase::Explicit("main".to_owned())
    );
    assert!(MachineConfig::load_from(&write(&dir, "[worktree]\nbase = \"\"\n")).is_err());
}

#[test]
fn agent_aliases_and_layouts_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[agents.aliases]\n\
             vim = \"nvim -p\"\n\
             [agents.aliases.htop]\n\
             command = \"htop\"\n\
             [agents.aliases.codex-yolo]\n\
             agent = \"codex\"\n\
             mode = \"yolo\"\n\
             model = \"gpt-5-codex\"\n\
             effort = \"high\"\n\
             args = \"--model gpt-5-codex -c model_reasoning_effort=high\"\n\
             [agents.aliases.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"/prompts/planner.md\"\n\
             [agents.layouts]\n\
             stacked = \"claude,codex+vim\"\n",
    ))
    .expect("load");
    let aliases = &config.agents.aliases.0;
    assert_eq!(
        aliases.get("vim"),
        Some(&Alias::Command("nvim -p".to_owned()))
    );
    assert_eq!(
        aliases.get("htop"),
        Some(&Alias::CommandTable {
            command: "htop".to_owned()
        })
    );
    assert_eq!(
        aliases.get("codex-yolo"),
        Some(&Alias::Agent {
            agent: "codex".to_owned(),
            mode: Some(PermissionMode::Yolo),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: None,
            args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned())
        })
    );
    // A role preset carries its own system prompt under the kebab-case key.
    assert_eq!(
        aliases.get("planner"),
        Some(&Alias::Agent {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: Some("/prompts/planner.md".into()),
            args: None,
        })
    );
    assert_eq!(
        config.agents.layouts.0.get("stacked").map(String::as_str),
        Some("claude,codex+vim")
    );
}

#[test]
fn alias_system_prompt_file_resolves_against_the_config_dir() {
    // A relative role prompt roots at the config file's directory, so it points
    // at the same file wherever the role later launches — not at the agent cwd.
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[agents.aliases.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"prompts/planner.md\"\n",
    ))
    .expect("load");
    let Some(Alias::Agent {
        system_prompt_file: Some(path),
        ..
    }) = config.agents.aliases.0.get("planner")
    else {
        panic!("planner role with a system prompt");
    };
    assert_eq!(path, &dir.path().join("prompts/planner.md"));
    // An absolute path is left untouched.
    let absolute = MachineConfig::load_from(&write(
        &dir,
        "[agents.aliases.planner]\n\
             agent = \"claude\"\n\
             system-prompt-file = \"/etc/rimz/planner.md\"\n",
    ))
    .expect("load");
    let Some(Alias::Agent {
        system_prompt_file: Some(path),
        ..
    }) = absolute.agents.aliases.0.get("planner")
    else {
        panic!("planner role with a system prompt");
    };
    assert_eq!(path, std::path::Path::new("/etc/rimz/planner.md"));
}

#[test]
fn autoping_schedules_parse_and_default_empty() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::default().autoping.schedules.0.is_empty(),
        "no schedules ship by default",
    );
    let config = MachineConfig::load_from(&write(
        &dir,
        "[autoping.schedules.morning]\n\
             kind = \"claude\"\n\
             root = \"/home/me/app\"\n\
             at = \"07:00\"\n\
             days = \"weekdays\"\n\
             worktree = \"main\"\n",
    ))
    .expect("load");
    let entry = config
        .autoping
        .schedules
        .0
        .get("morning")
        .expect("morning schedule");
    assert_eq!(entry.kind, "claude");
    assert_eq!(entry.root, std::path::Path::new("/home/me/app"));
    assert_eq!(entry.at.as_deref(), Some("07:00"));
    assert_eq!(entry.days.as_deref(), Some("weekdays"));
    assert_eq!(entry.worktree.as_deref(), Some("main"));
    assert_eq!(entry.cron, None);
}

#[test]
fn legacy_tab_section_hard_errors() {
    let dir = tempdir().expect("tempdir");
    let err = MachineConfig::load_from(&write(&dir, "[tab]\n")).expect_err("legacy tab");
    assert!(matches!(err, ConfigErr::LegacyTab { .. }));
}

#[test]
fn agent_alias_tables_reject_mixed_forms() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::load_from(&write(
            &dir,
            "[agents.aliases.mixed]\ncommand = \"nvim\"\nagent = \"claude\"\n",
        ))
        .is_err()
    );
    assert!(
        MachineConfig::load_from(&write(
            &dir,
            "[agents.aliases.missing_agent]\ncommand = \"codex\"\nmode = \"yolo\"\n",
        ))
        .is_err()
    );
}

#[test]
fn agent_alias_validation_runs_at_config_load() {
    let dir = tempdir().expect("tempdir");
    assert!(matches!(
        MachineConfig::load_from(&write(&dir, "[agents.aliases.term]\ncommand = \"zsh\"\n",)),
        Err(ConfigErr::Agents { .. })
    ));
    assert!(matches!(
        MachineConfig::load_from(&write(
            &dir,
            "[agents.aliases.pi-deep]\nagent = \"pi\"\nmodel = \"large\"\n",
        )),
        Err(ConfigErr::Agents { .. })
    ));
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
    assert!(config.notifications.command().is_none());
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
    assert_eq!(config.notifications.command(), Some("ntfy publish rimz"));
}

#[test]
fn sidebar_max_cols_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 100\n")).expect("load");
    assert_eq!(
        config.sidebar.max_cols,
        NonZeroU16::new(100).expect("nonzero")
    );
    assert_eq!(
        MachineConfig::default().sidebar.max_cols.get(),
        72,
        "unset caps the percentage split at the 72-column default",
    );
    assert!(MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 0\n")).is_err());
}

#[test]
fn sidebar_refresh_ms_defaults_parses_and_clamps_at_use() {
    let dir = tempdir().expect("tempdir");
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 80\n")).expect("load");
    assert_eq!(config.sidebar.refresh_ms, 80);
    assert_eq!(config.sidebar.resolved_refresh_ms(), 80);
    assert_eq!(
        MachineConfig::default().sidebar.refresh_ms,
        crate::sidebar::timing::DEFAULT_REFRESH_MS
    );

    let too_low =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 1\n")).expect("load");
    assert_eq!(
        too_low.sidebar.resolved_refresh_ms(),
        crate::sidebar::timing::MIN_REFRESH_MS
    );

    let too_high =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 5000\n")).expect("load");
    assert_eq!(
        too_high.sidebar.resolved_refresh_ms(),
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
fn sidebar_scrollbar_parses_and_defaults_auto() {
    let dir = tempdir().expect("tempdir");
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nscrollbar = \"never\"\n")).expect("load");
    assert_eq!(config.sidebar.scrollbar, ScrollbarMode::Never);
    assert_eq!(
        MachineConfig::default().sidebar.scrollbar,
        ScrollbarMode::Auto,
        "unset auto-hides: the bar shows only while the viewport moves",
    );
    assert!(MachineConfig::load_from(&write(&dir, "[sidebar]\nscrollbar = \"bogus\"\n")).is_err());
}

#[test]
fn attention_config_defaults_parses_and_rejects_zero() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(
        config.sidebar.attention.stalled_after_secs.get(),
        crate::feed::DEFAULT_STALL_AFTER_SECS,
        "unset uses the shipped 30-minute stall window",
    );

    let tuned = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.attention]\nstalled_after_secs = 2700\n",
    ))
    .expect("load");
    assert_eq!(tuned.sidebar.attention.stalled_after_secs.get(), 2700);

    let partial = MachineConfig::load_from(&write(&dir, "[sidebar.attention]\n")).expect("load");
    assert_eq!(partial.sidebar.attention, AttentionConfig::default());

    assert!(
        MachineConfig::load_from(&write(
            &dir,
            "[sidebar.attention]\nstalled_after_secs = 0\n",
        ))
        .is_err()
    );
}

#[test]
fn sidebar_theme_parses_defaults_unset_and_rejects_out_of_range() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.theme]\nmode = 256\nscheme = \"TokyoNight Night\"\ngood = 34\nselection = \"#8ab3e0\"\n",
    ))
    .expect("load");
    assert_eq!(config.sidebar.theme.mode, ThemeMode::Indexed);
    assert_eq!(
        config.sidebar.theme.scheme.as_deref(),
        Some("TokyoNight Night")
    );
    assert_eq!(config.sidebar.theme.good, Some(ThemeColor::Indexed(34)));
    assert_eq!(
        config.sidebar.theme.selection,
        Some(ThemeColor::Rgb(0x8a, 0xb3, 0xe0))
    );
    assert_eq!(config.sidebar.theme.alarm, None, "unset slots stay builtin");
    assert!(MachineConfig::default().sidebar.theme.is_unset());
    assert!(MachineConfig::load_from(&write(&dir, "[sidebar.theme]\ngood = 300\n")).is_err());
    assert!(
        MachineConfig::load_from(&write(&dir, "[sidebar.theme]\nselection = \"#bad\"\n")).is_err()
    );
}

#[test]
fn sidebar_animations_parse_as_partial_role_overrides() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.animations.thinking]\n\
             frames = \"⠁⠂\"\n\
             color = \"clay\"\n\
             speed = \"slow\"\n\
             [sidebar.animations.idle]\n\
             effect = \"breathe\"\n",
    ))
    .expect("load");
    let thinking = config
        .sidebar
        .animations
        .thinking
        .expect("thinking override");
    assert_eq!(
        thinking.frames.expect("frames").as_slice(),
        ["⠁".to_owned(), "⠂".to_owned()]
    );
    assert_eq!(thinking.color, Some(AnimationColor::Clay));
    assert_eq!(thinking.speed, Some(AnimationSpeed::Slow));
    assert_eq!(
        config
            .sidebar
            .animations
            .idle
            .expect("idle override")
            .effect,
        Some(AnimationEffect::Breathe)
    );
    assert!(MachineConfig::default().sidebar.animations.is_unset());
}

#[test]
fn sidebar_animations_accept_attention_frames_and_reject_bad_shapes() {
    let dir = tempdir().expect("tempdir");
    assert!(
        MachineConfig::load_from(&write(
            &dir,
            "[sidebar.animations.waiting]\nframes = \"?!\"\n",
        ))
        .is_ok(),
        "waiting now follows the uniform frame model"
    );
    assert!(
        MachineConfig::load_from(&write(
            &dir,
            "[sidebar.animations.idle]\nframes = [\"...\"]\n",
        ))
        .is_err()
    );
}

#[test]
fn sidebar_glyphs_parse_and_default_unicode() {
    let dir = tempdir().expect("tempdir");
    assert!(MachineConfig::default().sidebar.glyphs.is_unset());

    let config = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.glyphs]\n\
             set = \"nerd-font\"\n\
             [sidebar.glyphs.tokens]\n\
             total = \"◇\"\n",
    ))
    .expect("load");
    assert_eq!(config.sidebar.glyphs.set.as_deref(), Some("nerd-font"));
    assert_eq!(
        config
            .sidebar
            .glyphs
            .glyph(crate::config::GlyphRole::TokensTotal),
        Some("◇")
    );

    assert!(
        MachineConfig::load_from(&write(&dir, "[sidebar.glyphs.tokens]\ntotal = \"abc\"\n",))
            .is_err(),
        "glyph overrides occupy at most two terminal cells"
    );

    assert!(
        MachineConfig::load_from(&write(&dir, "[sidebar.glyphs.makr]\ntotal = \"Σ\"\n",)).is_err(),
        "glyph namespaces must be known"
    );
}

#[test]
fn sidebar_glow_parses_and_defaults_auto() {
    let dir = tempdir().expect("tempdir");
    assert_eq!(
        MachineConfig::default().sidebar.glow,
        GlowMode::Auto,
        "transition flashes ship following the terminal's advertisement",
    );
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = \"always\"\n")).expect("load");
    assert_eq!(config.sidebar.glow, GlowMode::Always);
    let config =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = \"never\"\n")).expect("load");
    assert_eq!(config.sidebar.glow, GlowMode::Never);
    assert!(MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = false\n")).is_err());
}

#[test]
fn zellij_room_defaults_are_agent_friendly() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert!(config.zellij.mouse_mode);
    assert!(config.zellij.mouse_click_through);
    assert!(!config.zellij.advanced_mouse_actions);
    assert!(!config.zellij.mouse_hover_effects);
    assert!(!config.zellij.focus_follows_mouse);
    assert!(!config.zellij.pane_frames);
    assert_eq!(config.zellij.on_force_close, ZellijForceClose::Detach);
    assert_eq!(config.zellij.scroll_buffer_size, 100_000);
    assert!(!config.zellij.show_startup_tips);
    assert!(!config.zellij.show_release_notes);
    assert_eq!(config.zellij.copy_clipboard, ZellijClipboard::System);
    assert!(config.zellij.copy_on_select);
    assert!(config.zellij.support_kitty_keyboard_protocol);
    assert!(config.zellij.osc8_hyperlinks);
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
             advanced_mouse_actions = true\n\
             mouse_hover_effects = true\n\
             focus_follows_mouse = false\n\
             copy_clipboard = \"primary\"\n\
             on_force_close = \"quit\"\n\
             auto_layout = false\n",
    ))
    .expect("load");
    assert!(config.zellij.pane_frames);
    assert!(config.zellij.advanced_mouse_actions);
    assert!(config.zellij.mouse_hover_effects);
    assert!(!config.zellij.focus_follows_mouse);
    assert_eq!(config.zellij.copy_clipboard, ZellijClipboard::Primary);
    assert_eq!(config.zellij.on_force_close, ZellijForceClose::Quit);
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
    assert_eq!(config.tmux.pane_border_status, TmuxPaneBorderStatus::Off);
    assert_eq!(config.tmux.pane_border_lines, TmuxPaneBorderLines::Simple);
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
    assert_eq!(config.tmux.pane_border_status, TmuxPaneBorderStatus::Top);
    assert_eq!(config.tmux.pane_border_lines, TmuxPaneBorderLines::Heavy);
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
    assert_eq!(config.sidebar.max_provider_blocks, 3);
    assert_eq!(config.sidebar.provider_tabs, ProviderTabsMode::Auto);
    assert!(config.sidebar.provider_list.is_empty());
    let partial =
        MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 60\n")).expect("load");
    assert_eq!(partial.sidebar.max_provider_blocks, 3);
    assert_eq!(partial.sidebar.provider_tabs, ProviderTabsMode::Auto);
    assert!(partial.sidebar.provider_list.is_empty());
}

#[test]
fn provider_dashboard_tabs_and_list_parse_and_round_trip() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[sidebar]\nprovider_tabs = \"always\"\nprovider_list = [\"codex\", \"all\"]\n",
    ))
    .expect("load");
    assert_eq!(config.sidebar.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(config.sidebar.provider_list, vec!["codex", "all"]);

    let encoded = toml::to_string(&config.sidebar).expect("serialize sidebar");
    let round_tripped: SidebarConfig = toml::from_str(&encoded).expect("parse sidebar");
    assert_eq!(round_tripped.provider_tabs, ProviderTabsMode::Always);
    assert_eq!(round_tripped.provider_list, vec!["codex", "all"]);
}

#[test]
fn sidebar_pets_defaults_parse_and_round_trip() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.pets]\nenabled = true\npet = \"dewey\"\nsize = \"small\"\nglyphs = \"octant\"\nvoice = false\n",
    ))
    .expect("load");
    assert!(config.sidebar.pets.enabled);
    assert_eq!(config.sidebar.pets.pet, "dewey");
    assert_eq!(config.sidebar.pets.size, PetsSize::Small);
    assert_eq!(config.sidebar.pets.glyphs, PetsGlyphMode::Octant);
    assert!(!config.sidebar.pets.voice);

    let defaults = MachineConfig::load_from(&write(&dir, "")).expect("load");
    assert_eq!(defaults.sidebar.pets, PetsConfig::default());
    assert_eq!(defaults.sidebar.pets.size, PetsSize::Medium);

    let encoded = toml::to_string(&config.sidebar.pets).expect("serialize pets");
    let round_tripped: PetsConfig = toml::from_str(&encoded).expect("parse pets");
    assert_eq!(round_tripped, config.sidebar.pets);
}

#[test]
fn context_severity_bands_default_and_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    let defaults = ContextSeverityConfig::default();
    assert_eq!(config.sidebar.context, defaults);
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
    let tuned = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.context]\nred = { percent = 50, tokens = 100000 }\n",
    ))
    .expect("load");
    assert_eq!(
        tuned.sidebar.context.red,
        ContextBand {
            percent: 50,
            tokens: 100_000
        }
    );
    assert_eq!(tuned.sidebar.context.green, defaults.green);
    assert_eq!(tuned.sidebar.context.yellow, defaults.yellow);
    assert_eq!(tuned.sidebar.context.amber, defaults.amber);
}

#[test]
fn budget_zones_default_and_parse() {
    let dir = tempdir().expect("tempdir");
    let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
    let defaults = BudgetZonesConfig::default();
    assert_eq!(config.sidebar.budget, defaults);
    assert_eq!(
        (defaults.yellow, defaults.amber, defaults.red),
        (50, 25, 10)
    );
    assert_eq!(
        (defaults.pace.yellow, defaults.pace.amber, defaults.pace.red),
        (100, 150, 200)
    );

    let tuned = MachineConfig::load_from(&write(
        &dir,
        "[sidebar.budget]\nred = 20\n[sidebar.budget.pace]\nred = 300\n",
    ))
    .expect("load");
    let budget = tuned.sidebar.budget;
    assert_eq!(
        (budget.yellow, budget.amber, budget.red),
        (defaults.yellow, defaults.amber, 20)
    );
    assert_eq!(
        (budget.pace.yellow, budget.pace.amber, budget.pace.red),
        (defaults.pace.yellow, defaults.pace.amber, 300)
    );
    let reparsed: MachineConfig =
        toml::from_str(&toml::to_string(&tuned).expect("serialize")).expect("reparse");
    assert_eq!(reparsed.sidebar.budget, tuned.sidebar.budget);
}

#[test]
fn provider_style_parses_art_and_color() {
    let dir = tempdir().expect("tempdir");
    let text = "[sidebar.providers.claude]\ncolor = \"#D97757\"\nascii_art = \" ▐▛███▜▌\"\n";
    let config = MachineConfig::load_from(&write(&dir, text)).expect("load");
    let claude = config
        .sidebar
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
