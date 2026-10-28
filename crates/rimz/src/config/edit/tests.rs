use super::*;
use crate::config::MachineConfigFileKind as Kind;

#[test]
fn spend_order_round_trips_with_config_set_and_get() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("definitions"),
    ));
    for value in ["credits_first", "resets_first"] {
        let result = editor.set("resume.spend_order", value);
        assert!(result.is_ok(), "spend order must be editable: {result:?}");
        assert_eq!(
            editor.get(Some("resume.spend_order")).unwrap().as_str(),
            Some(value)
        );
    }
    assert!(editor.set("resume.spend_order", "typo").is_err());
    assert!(
        Kind::Core
            .template()
            .contains("spend_order = \"credits_first\"")
    );
}

#[test]
fn model_aliases_round_trip_with_config_set() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("definitions"),
    ));
    for (alias, id) in [("sol", "gpt-6.1-sol"), ("custom", "123")] {
        let key = format!("models.codex.{alias}");
        editor.set(&key, id).unwrap();
        assert_eq!(editor.get(Some(&key)).unwrap().as_str(), Some(id));
    }
    assert!(editor.set("models.codex.sol", "").is_err());
    assert!(editor.set("models.typo.sol", "id").is_err());
    assert_eq!(
        editor.get(Some("models.codex.sol")).unwrap().as_str(),
        Some("gpt-6.1-sol")
    );
}

#[test]
fn model_tier_list_can_be_rebound_with_config_set() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("definitions"),
    ));
    editor.set("tiers.junior", "[\"sonnet\", \"sol\"]").unwrap();
    editor.set("tiers.routing", "off").unwrap();
    assert_eq!(
        editor
            .get(Some("tiers.junior"))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["sonnet", "sol"]
    );
    assert_eq!(
        editor.get(Some("tiers.routing")).unwrap().as_str(),
        Some("off")
    );
    assert!(editor.set("tiers.routing", "invalid").is_err());
}

#[test]
fn model_tier_list_edit_keeps_the_unwritten_tier_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("definitions"),
    ));
    editor.set("tiers.senior", "[]").unwrap();
    assert_eq!(
        editor
            .get(Some("tiers.senior"))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        editor.get(Some("tiers.principal")).unwrap()[0].as_str(),
        Some("fable")
    );
}

fn test_files() -> MachineConfigFiles {
    MachineConfigFiles::from_paths("/tmp/rimz/config.toml", "/tmp/rimz/agents-home")
}

#[test]
fn explicit_file_registry_preserves_path_and_template_order() {
    let files = test_files();
    let ordered = files.ordered();
    assert_eq!(
        ordered
            .each_ref()
            .map(|file| file.path().file_name().unwrap().to_owned()),
        ["config.toml", "theme.toml", "loop.toml"].map(std::ffi::OsString::from)
    );
    assert_eq!(ordered[0].template(), Kind::Core.template());
    assert_eq!(ordered[1].template(), Kind::Theme.template());
    assert_eq!(ordered[2].template(), MachineConfig::template_loop());
}

#[test]
fn config_editor_non_force_defaults_fill_the_gaps_beside_an_existing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let files = MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("agents-home"),
    );
    let editor = ConfigEditor::new(files);
    let existing = editor.files().ordered()[2].path().to_path_buf();
    let kept = "# existing loop config\n[tasks]\n";
    std::fs::write(&existing, kept).expect("write loop config");

    assert!(editor.write_defaults(false).expect("write defaults"));
    for file in editor.files().ordered() {
        assert!(
            file.path().exists(),
            "{} was not written",
            file.path().display()
        );
    }
    assert_eq!(
        std::fs::read_to_string(&existing).expect("read kept file"),
        kept,
        "an existing file must survive the bootstrap untouched"
    );

    assert!(
        !editor.write_defaults(false).expect("write defaults again"),
        "a complete set writes nothing"
    );
}

#[test]
fn non_force_defaults_preserve_a_dangling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let target = dir.path().join("missing.toml");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    editor.write_defaults(false).unwrap();
    assert_eq!(std::fs::read_link(&path).unwrap(), target);
    assert!(!target.exists());
}

#[test]
fn dangling_core_refuses_account_inheritance_but_allows_a_default_pin() {
    if std::env::var_os("RIMZ_TEST_DANGLING_CORE").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "config::edit::tests::dangling_core_refuses_account_inheritance_but_allows_a_default_pin", "--nocapture"])
            .env("RIMZ_HOME", home.path())
            .env("RIMZ_TEST_DANGLING_CORE", "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let path = MachineConfig::config_path();
    std::os::unix::fs::symlink(path.with_file_name("missing.toml"), &path).unwrap();
    ConfigEditor::machine().write_defaults(false).unwrap();
    let machine = MachineConfig::load_lenient();
    assert!(
        machine.notices.unreadable_files.contains_key(&path),
        "a dangling core must be recorded as unreadable"
    );
    assert!(machine.require_readable_core("select an account").is_err());
    let root = tempfile::tempdir().unwrap();
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let following =
        crate::agents::resolve_room_accounts(&Default::default(), root.path(), &machine);
    assert!(following.login(&kind, &machine.accounts).is_err());
    let pins = [(kind.clone(), crate::ids::LoginName::default_login())].into();
    let pinned = crate::agents::resolve_room_accounts(&pins, root.path(), &machine);
    assert!(
        pinned
            .login(&kind, &machine.accounts)
            .unwrap()
            .name()
            .is_default()
    );
}

#[test]
fn non_force_defaults_report_metadata_errors() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("not-a-directory");
    std::fs::write(&parent, "occupied").unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        parent.join("config.toml"),
        dir.path().join("definitions"),
    ));
    assert!(
        editor.write_defaults(false).is_err(),
        "an unreadable path must not silently count as an existing config"
    );
}

#[test]
fn set_classifies_a_duplicate_key_in_the_existing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[resume]\nauto_continue = false\nauto_continue = true\n",
    )
    .expect("write duplicate config");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));

    let error = editor
        .set("remote_control.claude", "true")
        .expect_err("duplicate key blocks editing");

    match error {
        ConfigEditErr::DocumentParse {
            path: error_path,
            diagnosis,
        } => {
            assert_eq!(error_path, path);
            assert_eq!(diagnosis.line(), Some(3));
            assert_eq!(
                diagnosis.problem(),
                "`auto_continue` is defined more than once in the same table"
            );
            assert_eq!(
                diagnosis.fix(),
                format!(
                    "remove the extra `auto_continue` at {}:3, then re-run",
                    path.display()
                )
            );
        }
        other => panic!("expected document parse error, got {other:?}"),
    }
}

const LEGACY_SET_KEYS: &[&str] = &[
    "agents.worktree.dir",
    "agents.worktree.base",
    "agents.worktree.hooks.created",
    "agents.worktree.hooks.removed",
    "agents.placement",
    "harness.smart_compact",
    "harness.auto_compact",
    "harness.flip_compact",
    "harness.compact_instruction",
    "harness.idle_compact",
    "harness.budget",
    "timezone",
    "resume.on_rebirth",
    "resume.max",
    "resume.auto_continue",
    "resume.auto_continue_backoff_secs",
    "resume.auto_continue_max_retries",
    "resume.auto_continue_text",
    "resume.auto_redeem",
    "resume.spend_order",
    "resume.auto_redeem_min_gain",
    "remote_control.claude",
    "remote_control.codex",
    "notifications.enabled",
    "notifications.triggers",
    "notifications.desktop",
    "notifications.sound",
    "notifications.suppress_focused",
    "notifications.debounce_ms",
    "notifications.coalesce_ms",
    "notifications.remind_secs",
    "notifications.title",
    "notifications.body",
    "notifications.command",
    "theme.style",
    "theme.display.refresh_ms",
    "theme.display.pixel",
    "theme.display.max_provider_blocks",
    "theme.display.provider_tabs",
    "theme.display.provider_list",
    "theme.display.max_cols",
    "theme.display.scrollbar",
    "theme.display.card_density",
    "theme.display.stale_after_secs",
    "theme.display.recent_subagent_secs",
    "theme.display.max_recent_subagents",
    "theme.display.context_meter.log_scale",
    "theme.display.context_meter.green",
    "theme.display.context_meter.yellow",
    "theme.display.context_meter.amber",
    "theme.display.context_meter.red",
    "theme.display.budget_bar.yellow",
    "theme.display.budget_bar.amber",
    "theme.display.budget_bar.red",
    "theme.display.budget_bar.burn_rate.green",
    "theme.display.budget_bar.burn_rate.deep_green",
    "theme.display.budget_bar.burn_rate.yellow",
    "theme.display.budget_bar.burn_rate.amber",
    "theme.display.budget_bar.burn_rate.red",
    "theme.display.highlight_steps.band",
    "theme.display.highlight_steps.wash",
    "theme.display.highlight_steps.indexed",
    "sidebar.focus_key",
    "sidebar.zoom_key",
    "sidebar.spend_window",
    "sidebar.afk_after_secs",
    "agents.attention.active_grace_secs",
    "agents.attention.stalled_after_secs",
    "agents.attention.tool_repeat_warn_after",
    "agents.attention.tool_repeat_attention_after",
    "agents.attention.inactive_after_secs",
    "agents.attention.archive_after_secs",
    "theme.pets.enabled",
    "theme.pets.pet",
    "theme.pets.glyphs",
    "theme.pets.cell_aspect",
    "theme.pets.voice",
    "loop.tasks",
    "theme.animations.unread",
    "theme.glyphs.set",
    "theme.colors.primary.background",
    "theme.colors.primary.foreground",
    "theme.colors.normal.black",
    "theme.colors.normal.red",
    "theme.colors.normal.green",
    "theme.colors.normal.yellow",
    "theme.colors.normal.blue",
    "theme.colors.normal.magenta",
    "theme.colors.normal.cyan",
    "theme.colors.normal.white",
    "theme.colors.bright.black",
    "theme.colors.bright.red",
    "theme.colors.bright.green",
    "theme.colors.bright.yellow",
    "theme.colors.bright.blue",
    "theme.colors.bright.magenta",
    "theme.colors.bright.cyan",
    "theme.colors.bright.white",
    "theme.colors.selection.background",
    "theme.colors.selection.text",
    "sidebar.trunk",
    "theme.mode",
    "theme.scheme",
    "theme.good",
    "theme.warn",
    "theme.caution",
    "theme.alarm",
    "theme.accent",
    "theme.cool",
    "theme.meta",
    "theme.body",
    "theme.muted",
    "theme.faint",
    "theme.rule",
    "theme.selection",
    "theme.selection_bg",
    "zellij.mouse_mode",
    "zellij.mouse_click_through",
    "zellij.advanced_mouse_actions",
    "zellij.mouse_hover_effects",
    "zellij.focus_follows_mouse",
    "zellij.pane_frames",
    "zellij.on_force_close",
    "zellij.scroll_buffer_size",
    "zellij.show_startup_tips",
    "zellij.show_release_notes",
    "zellij.copy_clipboard",
    "zellij.copy_on_select",
    "zellij.support_kitty_keyboard_protocol",
    "zellij.osc8_hyperlinks",
    "zellij.session_serialization",
    "tmux.mouse",
    "tmux.focus_events",
    "tmux.history_limit",
    "tmux.allow_passthrough",
    "tmux.set_clipboard",
    "tmux.extended_keys",
    "tmux.extended_keys_format",
    "tmux.escape_time_ms",
    "tmux.renumber_windows",
    "tmux.aggressive_resize",
    "tmux.pane_border_status",
    "tmux.pane_border_lines",
];

#[test]
fn validates_config_key_read_and_write_surfaces() {
    for key in [
        "theme.display.max_cols",
        "theme.display.pixel",
        "theme.display.context_meter.log_scale",
        "theme.display.budget_bar.burn_rate.red",
        "accounts.usage_limit_usd.codex",
        "accounts.budget.claude",
        "agents.commands.vim",
        "agents.startup-relaunches",
        "agents.startup-relaunch-wait",
        "loop.tasks.watch.agent",
        "loop.default-timeout",
        "loop.throttle.pace",
        "loop.throttle.max-active",
        "loop.tasks.watch.prompt",
        "loop.tasks.watch.check",
        "loop.tasks.watch.on",
        "loop.tasks.watch.deadline",
        "loop.tasks.watch.wait.kind",
        "theme.providers.claude.color",
        "theme.pets.enabled",
        "theme.pets.pet",
        "theme.pets.glyphs",
        "theme.pets.cell_aspect",
        "theme.pets.voice",
        "theme.mode",
        "theme.scheme",
        "theme.caution",
        "timezone",
        "sidebar.focus_key",
        "sidebar.zoom_key",
        "sidebar.spend_window",
        "sidebar.afk_after_secs",
        "theme.animations.thinking.frames",
        "theme.animations.working.color",
        "theme.animations.idle.effect",
        "theme.animations.success.speed",
        "theme.animations.unread",
        "theme.glyphs.set",
        "theme.glyphs.unicode.status.working",
        "theme.glyphs.unicode.tokens.total",
        "theme.glyphs.unicode.keys.focus",
        "theme.glyphs.unicode.chrome.box_vertical",
        "theme.glyphs.nerd_font.clock.over",
        "resume.auto_continue",
        "resume.auto_continue_backoff_secs",
        "resume.auto_continue_max_retries",
        "resume.auto_continue_text",
        "resume.auto_redeem",
        "resume.spend_order",
        "resume.auto_redeem_min_gain",
        "notifications.title",
        "notifications.body",
        "harness.smart_compact",
        "harness.auto_compact",
        "harness.flip_compact",
        "harness.compact_instruction",
        "harness.idle_compact",
        "harness.budget",
        "harness.turn_budget",
        "gc.auto",
        "gc.older_than",
        "gc.tmp_grace",
        "gc.tmp_floor_percent",
        "gc.tmp_floor_min",
    ] {
        validate_set_key(&test_files(), &parse_key(key).unwrap())
            .unwrap_or_else(|err| panic!("{key}: {err}"));
    }

    for key in [
        "sidebar.nope",
        "accounts.nope",
        "accounts.usage_limit_usd",
        "accounts.budget",
        "accounts.budget.claude.extra",
        "accounts.usage_limit_usd.codex.extra",
        "agents.teams.peer.shape",
        "agents.profiles.codex-slim.flags",
        "agents.commands.vim.command",
        "agents.pets.enabled",
        "notifications.handler",
        "notifications.handler.command",
        "theme.providers.claude.nope",
        "theme.animations",
        "theme.animations.nope.frames",
        "theme.animations.thinking.nope",
        "theme.animations.thinking.frames.extra",
        "theme.glyphs.nope",
        "theme.glyphs.unicode.tokens.nope",
        "theme.glyphs.unicode.tokens.total.extra",
    ] {
        assert!(
            validate_set_key(&test_files(), &parse_key(key).unwrap()).is_err(),
            "{key}"
        );
    }

    for (key, known) in [
        ("theme.animations", true),
        ("theme.animations.thinking", true),
        ("theme.animations.thinking.frames", true),
        ("theme.animations.unread", true),
        ("theme.animations.nope", false),
        ("theme.pets", true),
        ("theme.pets.enabled", true),
        ("theme.pets.cell_aspect", true),
        ("theme.glyphs", true),
        ("theme.glyphs.unicode.tokens", true),
        ("theme.glyphs.unicode.keys", true),
        ("theme.glyphs.unicode.tokens.total", true),
        ("theme.glyphs.unicode.keys.focus", true),
        ("theme.glyphs.unicode.tokens.nope", false),
        ("accounts", true),
        ("accounts.usage_limit_usd", true),
        ("accounts.usage_limit_usd.codex", true),
        ("accounts.budget", true),
        ("accounts.budget.claude", true),
        ("agents.startup-relaunches", true),
        ("agents.startup-relaunch-wait", true),
        ("agents.profiles.demo.agent", false),
        ("agents.profiles.demo.auto-compact", false),
        ("agents.profiles.demo.bogus", false),
        ("subagents.profiles.demo.effort", false),
        ("subagents.profiles.demo.auto-compact", false),
        ("subagents.profiles.demo.bogus", false),
        ("agents.teams.demo.layout", false),
        ("agents.teams.demo.bogus", false),
        ("agents.pets", false),
        ("loop", true),
        ("loop.tasks", true),
    ] {
        assert_eq!(
            is_known_get_key(&test_files(), &parse_key(key).unwrap()).unwrap(),
            known,
            "{key}"
        );
    }
}

#[test]
fn team_stages_parse_default_and_round_trip() {
    use crate::config::Team;

    let undeclared: Team = toml::from_str("layout = 'claude,codex'").expect("team");
    assert!(undeclared.stages.is_empty());
    assert!(
        !toml::to_string(&undeclared)
            .expect("serialize team")
            .contains("stages")
    );

    let declared: Team = toml::from_str(
        "layout = 'claude,codex'\nstages = ['Explore', 'Plan', 'Implement (delta)']",
    )
    .expect("stages");
    assert_eq!(declared.stages, ["Explore", "Plan", "Implement (delta)"]);
    let serialized = toml::to_string(&declared).expect("serialize stages");
    assert_eq!(
        toml::from_str::<Team>(&serialized).expect("round trip"),
        declared
    );
}

#[test]
fn set_top_level_timezone_keeps_core_config_valid() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let agents_home = std::path::Path::new("missing-agents-home");
    let key = parse_key("timezone").expect("key");
    apply_logical_key(
        &mut doc,
        std::path::Path::new("config.toml"),
        &key,
        parse_set_value(&key, "America/New_York"),
        agents_home,
    )
    .expect("set timezone");

    let rendered = doc.to_string();
    let timezone = rendered
        .lines()
        .position(|line| line.starts_with("timezone = "))
        .expect("timezone line");
    let first_table = rendered
        .lines()
        .position(|line| line.starts_with('['))
        .expect("first table");
    assert!(timezone < first_table);
}

#[test]
fn set_context_meter_log_scale_round_trips_through_scalar_path() {
    let mut doc = Kind::Theme
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let agents_home = std::path::Path::new("missing-agents-home");
    let key = parse_key("theme.display.context_meter.log_scale").expect("key");
    apply_logical_key(
        &mut doc,
        std::path::Path::new("theme.toml"),
        &key,
        parse_set_value(&key, "false"),
        agents_home,
    )
    .expect("set log scale");

    let config = MachineConfig::parse_text(
        std::path::Path::new("theme.toml"),
        &doc.to_string(),
        agents_home,
    )
    .expect("parse edited theme");
    assert!(!config.theme.display.context_meter.log_scale);
}

#[test]
fn round_trip_validation_reports_the_logical_key_value_and_message() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let key = parse_key("remote_control.claude").expect("key");
    let err = apply_logical_key(
        &mut doc,
        std::path::Path::new("config.toml"),
        &key,
        parse_set_value(&key, "flase"),
        std::path::Path::new("missing-agents-home"),
    )
    .expect_err("string toggle must fail validation");

    match err {
        ConfigEditErr::Validate {
            key,
            value,
            message,
        } => {
            assert_eq!(key, "remote_control.claude");
            assert_eq!(value, "\"flase\"");
            assert_eq!(
                message,
                "remote-control agent kind `claude` must be a boolean (true or false)"
            );
        }
        other => panic!("expected round-trip validation error, got {other:?}"),
    }
}

#[test]
fn collect_explicit_keys_maps_theme_colors_and_reports_unknowns() {
    let doc = r##"
[colors.primary]
background = "#000000"
nope = "surprise"
"##
    .parse::<DocumentMut>()
    .expect("parse theme snippet");

    let expected_background = parse_key("theme.colors.primary.background").expect("key");
    let found = collect_explicit_keys(MachineConfigFileKind::Theme, &doc);
    let mut saw_background = false;
    let mut saw_nope = false;
    for item in found {
        match item {
            PendingKey { logical, value } if logical == expected_background => {
                assert_eq!(value.as_str(), Some("#000000"));
                saw_background = true;
            }
            PendingKey { logical, value }
                if logical == parse_key("theme.colors.primary.nope").expect("key") =>
            {
                assert_eq!(value.as_str(), Some("surprise"));
                saw_nope = true;
            }
            other => panic!("unexpected key: {other:?}"),
        }
    }
    assert!(saw_background, "background override should be settable");
    assert!(
        saw_nope,
        "unknown color leaf should flow to trial validation"
    );
}

#[test]
fn merge_key_oracle_accepts_sentry_and_rejects_bogus_keys() {
    let agents_home = std::path::Path::new("missing-agents-home");
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![
            PendingKey {
                logical: parse_key("sentry.dsn").expect("key"),
                value: Value::from("https://public@example.com/1"),
            },
            PendingKey {
                logical: parse_key("notifications.nope").expect("key"),
                value: Value::from(true),
            },
        ],
        &mut skipped,
        agents_home,
    );

    assert_eq!(kept, 1);
    assert_eq!(
        item_at(&doc, &parse_key("sentry.dsn").expect("key"))
            .and_then(Item::as_value)
            .and_then(Value::as_str),
        Some("https://public@example.com/1")
    );
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].key, "notifications.nope");
    assert!(skipped[0].reason.contains("unknown config key"));
}

#[test]
fn merge_uncomments_section_key_on_its_template_line() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("notifications.desktop").expect("key"),
            value: Value::from("osc"),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
    );

    assert_eq!(kept, 1);
    assert!(skipped.is_empty());
    let rendered = doc.to_string();
    let desktop_lines: Vec<_> = rendered
        .lines()
        .filter(|line| line.contains("desktop = "))
        .collect();
    assert_eq!(desktop_lines.len(), 1, "{rendered}");
    assert!(
        desktop_lines[0].starts_with("desktop = \"osc\"")
            && desktop_lines[0].ends_with("# \"auto\", \"osc\", or \"off\""),
        "{rendered}"
    );
    let notifications = rendered.find("[notifications]").expect("notifications");
    let desktop = rendered.find("desktop = \"osc\"").expect("desktop");
    let sidebar = rendered.find("[sidebar]").expect("sidebar");
    assert!(notifications < desktop && desktop < sidebar, "{rendered}");
    assert!(!rendered.contains("# desktop = "), "{rendered}");
}

#[test]
fn merge_uncomments_root_scalar_at_its_template_position() {
    let template = Kind::Core.template();
    let mut doc = template.parse::<DocumentMut>().expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("timezone").expect("key"),
            value: Value::from("America/Los_Angeles"),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
    );

    assert_eq!(kept, 1);
    assert!(skipped.is_empty());
    let rendered = doc.to_string();
    let timezone_lines: Vec<_> = rendered
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("timezone = "))
        .collect();
    assert_eq!(timezone_lines.len(), 1, "{rendered}");
    assert_eq!(
        timezone_lines[0].0,
        template
            .lines()
            .position(|line| line.starts_with("## timezone = "))
            .expect("template timezone"),
        "{rendered}"
    );
    assert_eq!(
        timezone_lines[0].1,
        "timezone = \"America/Los_Angeles\" # IANA zone for displayed times and scheduling; default = system local"
    );
    assert!(!rendered.contains("## timezone = "), "{rendered}");
}

#[test]
fn merge_uncomments_optional_example_under_its_section() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("harness.budget").expect("key"),
            value: Value::from("50/day"),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
    );

    assert_eq!(kept, 1);
    assert!(skipped.is_empty());
    let rendered = doc.to_string();
    let harness = rendered.find("[harness]").expect("harness");
    let budget = rendered.find("budget = \"50/day\"").expect("budget");
    assert!(harness < budget, "{rendered}");
    assert!(!rendered.contains("## budget = \"50/day\""), "{rendered}");
}

#[test]
fn merge_skips_retired_definition_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[agents.teams.peer]\nleader = 'codex'").unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(&path, dir.path()));
    let report = editor.merge_defaults().unwrap();
    assert!(
        report.files[0]
            .skipped
            .iter()
            .any(|key| key.key == "agents.teams.peer.leader")
    );
    assert!(!std::fs::read_to_string(path).unwrap().contains("leader ="));
}

#[test]
fn set_definition_fields_names_the_markdown_source_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path(),
    ));
    for (key, tree) in [
        ("agents.profiles.worker.model", "agents"),
        ("subagents.profiles.worker.effort", "subagents"),
        ("agents.teams.worker.leader", "teams"),
    ] {
        let error = editor.set(key, "value").unwrap_err();
        assert!(
            error.to_string().contains(&format!("{tree}/worker.md")),
            "{error}"
        );
    }
    assert!(!dir.path().join("config.toml").exists());
}

#[test]
fn set_is_not_locked_out_by_a_broken_definition_set() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "[agents.commands]\nprobe = \"echo\"\n").unwrap();
    for (file, text) in [
        ("agents/claude.md", "---\ndescription: Base\n---\nBase."),
        (
            "agents/worker.md",
            "---\ndescription: Worker\nmodel: opus\ntools: [Bash]\n---\n",
        ),
        (
            "teams/probe.md",
            "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]\n---\nPipeline.",
        ),
    ] {
        let path = dir.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let loaded = MachineConfig::load_from(&config, dir.path()).unwrap();
    assert!(!loaded.notices.definition_errors.is_empty());
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(&config, dir.path()));
    editor.set("timezone", "UTC").unwrap();
    assert!(
        std::fs::read_to_string(config)
            .unwrap()
            .contains("timezone")
    );
}

#[test]
fn setup_merge_leaves_legacy_toml_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("profiles/old/agent.toml");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "not = = toml").unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path(),
    ));
    assert_eq!(editor.merge_defaults().unwrap().files.len(), 3);
    assert_eq!(std::fs::read_to_string(legacy).unwrap(), "not = = toml");
}

#[test]
fn uncomment_accepts_active_header_with_trailing_comment() {
    let text = "[notifications] # local choices\n# desktop = \"auto\" # delivery mode\n";
    let key = parse_key("notifications.desktop").expect("key");

    let rendered = uncomment_template_default(text, &key).expect("matching default");

    assert_eq!(
        rendered,
        "[notifications] # local choices\ndesktop = \"auto\" # delivery mode\n"
    );
}

#[test]
fn failed_merge_keeps_template_default_commented() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("notifications.desktop").expect("key"),
            value: Value::from(5),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
    );

    assert_eq!(kept, 0);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].key, "notifications.desktop");
    assert!(
        doc.to_string()
            .contains("# desktop = \"auto\"                    # \"auto\", \"osc\", or \"off\""),
        "{doc}"
    );
}

#[test]
fn merge_preserves_trailing_comment_on_existing_scalar() {
    let mut doc = Kind::Core
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("config.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("tmux.set_clipboard").expect("key"),
            value: Value::from("external"),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
    );

    assert_eq!(kept, 1);
    assert!(skipped.is_empty());
    let line = doc
        .to_string()
        .lines()
        .find(|line| line.starts_with("set_clipboard = "))
        .expect("set_clipboard")
        .to_owned();
    assert!(line.starts_with("set_clipboard = \"external\""), "{line}");
    assert!(
        line.ends_with("# \"on\", \"external\", or \"off\""),
        "{line}"
    );
}

#[test]
fn set_missing_file_uncomments_template_default_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));

    editor
        .set("notifications.desktop", "osc")
        .expect("set desktop");

    let rendered = std::fs::read_to_string(path).expect("read config");
    let desktop_lines: Vec<_> = rendered
        .lines()
        .filter(|line| line.contains("desktop = "))
        .collect();
    assert_eq!(desktop_lines.len(), 1, "{rendered}");
    assert!(desktop_lines[0].starts_with("desktop = \"osc\""));
    assert!(!rendered.contains("# desktop = "), "{rendered}");
}

#[test]
fn set_agents_isolation_keeps_gc_template_comments_under_gc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, Kind::Core.template()).expect("write fresh template");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));

    editor
        .set("agents.isolation", "scoped")
        .expect("set isolation");

    let rendered = std::fs::read_to_string(path).expect("read config");
    let gc_body = rendered
        .split_once("[gc]\n")
        .expect("gc table")
        .1
        .split_once("\n[agents]\n")
        .expect("agents table follows gc")
        .0;
    assert!(gc_body.contains("# auto = true"), "{rendered}");
    assert!(gc_body.contains("# older_than = \"7d\""), "{rendered}");
    let parsed: toml::Value = toml::from_str(&rendered).expect("valid config");
    assert_eq!(parsed["agents"]["isolation"].as_str(), Some("scoped"));
}

#[test]
fn first_prompt_cache_fallback_keeps_harness_template_comments() {
    assert_first_prompt_cache_value_keeps_harness_comments("fallback", "off");
}

#[test]
fn first_prompt_cache_provider_keeps_harness_template_comments() {
    assert_first_prompt_cache_value_keeps_harness_comments("claude", "5m");
}

fn assert_first_prompt_cache_value_keeps_harness_comments(kind: &str, value: &str) {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap();
    let key = format!("harness.prompt_cache_ttl.{kind}");
    editor.set(&key, value).unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_harness_template_comments_stay_under_harness(&rendered);
    let child = rendered
        .find("\n[harness.prompt_cache_ttl.codex]\n")
        .expect("child header");
    let parent = rendered
        .find("\n[harness.prompt_cache_ttl]\n")
        .expect("parent header");
    assert!(
        child < parent,
        "the first parent header must follow its child: {rendered}"
    );
    let uncommented = rendered.replace(
        "# prompt_cache_margin = \"1m\"",
        "prompt_cache_margin = \"1m\"",
    );
    let parsed: toml::Value = toml::from_str(&uncommented).unwrap();
    assert_eq!(
        parsed["harness"]["prompt_cache_margin"].as_str(),
        Some("1m")
    );
    assert!(
        parsed["harness"]["prompt_cache_ttl"]
            .get("prompt_cache_margin")
            .is_none()
    );
    assert_eq!(editor.get(Some(&key)).unwrap().as_str(), Some(value));
    assert_eq!(
        editor
            .get(Some("harness.prompt_cache_ttl.codex.api-key"))
            .unwrap()
            .as_str(),
        Some("20m")
    );
}

#[test]
fn replacing_implicit_parents_child_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap();
    editor.set("harness.prompt_cache_ttl.codex", "12h").unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_harness_template_comments_stay_under_harness(&rendered);
    assert!(
        rendered.contains("\n[harness.prompt_cache_ttl]\ncodex = \"12h\"\n"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("\n[harness.prompt_cache_ttl.codex]\n"),
        "{rendered}"
    );
    assert_eq!(
        editor
            .get(Some("harness.prompt_cache_ttl.codex"))
            .unwrap()
            .as_str(),
        Some("12h")
    );
}

#[test]
fn replacing_child_under_rendered_parent_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap();
    editor
        .set("harness.prompt_cache_ttl.fallback", "off")
        .unwrap();
    editor.set("harness.prompt_cache_ttl.codex", "12h").unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_eq!(
        rendered,
        format!(
            "{}\n[harness.prompt_cache_ttl]\ncodex = \"12h\"\nfallback = \"off\"\n",
            Kind::Core.template()
        )
    );
    assert_harness_template_comments_stay_under_harness(&rendered);
}

#[test]
fn replacing_implicit_parent_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap();
    editor
        .set("harness.prompt_cache_ttl", "{ claude = \"5m\" }")
        .unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_eq!(
        rendered,
        format!(
            "{}\n[harness.prompt_cache_ttl]\nclaude = \"5m\"\n",
            Kind::Core.template()
        )
    );
    assert_harness_template_comments_stay_under_harness(&rendered);
}

#[test]
fn replacing_nested_blocks_and_array_entries_keeps_header_comments() {
    for (blocks, comments) in [
        (
            "# over b\n[a.b] # header line\n# body\ny = 1 # body suffix\n# over child\n[a.b.child]\nz = 1\n",
            "# over b\n# over child\n",
        ),
        (
            "# over entry 0\n[[a.b]] # header line\ny = 1\n# over entry 1\n[[a.b]]\ny = 2\n",
            "# over entry 0\n# over entry 1\n",
        ),
    ] {
        let mut doc: DocumentMut = format!("[a]\nx = 1\n{blocks}\n[next]\nx = 2\n")
            .parse()
            .unwrap();
        set_document_value(&mut doc, &parse_key("a.b").unwrap(), Value::from(2)).unwrap();
        assert_eq!(
            doc.to_string(),
            format!("[a]\nx = 1\nb = 2\n{comments}\n[next]\nx = 2\n")
        );
    }
}

#[test]
fn replacing_final_blocks_keeps_header_comments_at_eof() {
    for (blocks, comments) in [
        (
            "# over b\n[a.b]\ny = 1\n# over child\n[a.b.child]\nz = 1\n",
            "# over b\n# over child\n",
        ),
        (
            "# over entry 0\n[[a.b]]\ny = 1\n# over entry 1\n[[a.b]]\ny = 2\n",
            "# over entry 0\n# over entry 1\n",
        ),
    ] {
        let mut doc: DocumentMut = format!("[a]\nx = 1\n{blocks}\n\n# tail\n").parse().unwrap();
        set_document_value(&mut doc, &parse_key("a.b").unwrap(), Value::from(2)).unwrap();
        assert_eq!(
            doc.to_string(),
            format!("[a]\nx = 1\nb = 2\n{comments}\n\n# tail\n")
        );
    }
}

#[test]
fn replacing_block_keeps_receiver_comments() {
    let mut doc: DocumentMut = "[a]\nx = 1\n# over b\n\n\n[a.b]\ny = 1\n\n# next\n[next]\nx = 2\n"
        .parse()
        .unwrap();
    set_document_value(&mut doc, &parse_key("a.b").unwrap(), Value::from(2)).unwrap();
    assert_eq!(
        doc.to_string(),
        "[a]\nx = 1\nb = 2\n# over b\n\n# next\n[next]\nx = 2\n"
    );
}

#[test]
fn replacing_same_key_block_keeps_header_decor() {
    let mut doc: DocumentMut = "[a]\nx = 1\n# over b\n[a.b] # line\ny = 1\n"
        .parse()
        .unwrap();
    set_document_value(
        &mut doc,
        &parse_key("a.b").unwrap(),
        parse_edit_value("{ y = 2 }"),
    )
    .unwrap();
    assert_eq!(
        doc.to_string(),
        "[a]\nx = 1\n# over b\n[a.b] # line\ny = 2\n"
    );
}

#[test]
fn replacing_array_entries_keeps_later_comments_after_the_new_array() {
    let two_entries = "# over entry 0\n[[a.b]]\ny = 1\n# over entry 1\n[[a.b]]\ny = 2\n";
    let rendered = [
        (two_entries, "[{ y = 3 }]", false),
        (
            "# e0\n[[a.b]]\ny = 1\n# e1\n[[a.b]]\ny = 2\n# e2\n[[a.b]]\ny = 3\n",
            "[{ y = 4 }, { y = 5 }]",
            false,
        ),
        (two_entries, "[{ y = 3 }]", true),
    ]
    .map(|(blocks, value, unpositioned_entry)| {
        let mut doc: DocumentMut = format!("[a]\nx = 1\n{blocks}").parse().unwrap();
        if unpositioned_entry {
            doc["a"]["b"]
                .as_array_of_tables_mut()
                .unwrap()
                .get_mut(1)
                .unwrap()
                .set_position(None);
        }
        set_document_value(
            &mut doc,
            &parse_key("a.b").unwrap(),
            parse_edit_value(value),
        )
        .unwrap();
        doc.to_string()
    });
    assert_eq!(
        rendered,
        [
            "[a]\nx = 1\n# over entry 0\n[[a.b]]\ny = 3\n# over entry 1\n",
            "[a]\nx = 1\n# e0\n[[a.b]]\ny = 4\n\n[[a.b]]\ny = 5\n# e1\n# e2\n",
            "[a]\nx = 1\n# over entry 0\n[[a.b]]\ny = 3\n# over entry 1\n",
        ]
    );
}

#[test]
fn replacing_array_with_block_keeps_later_comments_after_the_new_block() {
    let rendered = ["", "\n# next\n[next]\nx = 2\n"].map(|tail| {
        let mut doc: DocumentMut = format!(
            "[a]\nx = 1\n# over entry 0\n[[a.b]]\ny = 1\n# over entry 1\n[[a.b]]\ny = 2\n{tail}"
        )
        .parse()
        .unwrap();
        if let Some(next) = doc.get_mut("next").and_then(Item::as_table_mut) {
            next.set_position(None);
        }
        set_document_value(
            &mut doc,
            &parse_key("a.b").unwrap(),
            parse_edit_value("{ y = 3 }"),
        )
        .unwrap();
        doc.to_string()
    });
    assert_eq!(
        rendered,
        [
            "[a]\nx = 1\n# over entry 0\n[a.b]\ny = 3\n# over entry 1\n",
            "[a]\nx = 1\n# over entry 0\n[a.b]\ny = 3\n# over entry 1\n\n# next\n[next]\nx = 2\n",
        ]
    );
}

#[test]
fn replacing_parent_after_its_child_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap();
    editor
        .set("harness.prompt_cache_ttl.fallback", "off")
        .unwrap();
    editor
        .set("harness.prompt_cache_ttl", "{ fallback = \"5m\" }")
        .unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_eq!(
        rendered,
        format!(
            "{}\n[harness.prompt_cache_ttl]\nfallback = \"5m\"\n",
            Kind::Core.template()
        )
    );
    assert_harness_template_comments_stay_under_harness(&rendered);
}

#[test]
fn value_replacing_a_block_under_implicit_parent_keeps_prefix_spacing() {
    let rendered = ["", "\n\n\n", "# over b\n\n"].map(|prefix| {
        let mut doc: DocumentMut = format!("[x]\nk = 1\n{prefix}[a.b] # old header\ny = 1\n")
            .parse()
            .unwrap();
        set_document_value(&mut doc, &parse_key("a.b").unwrap(), Value::from(2)).unwrap();
        doc.to_string()
    });
    assert_eq!(
        rendered,
        [
            "[x]\nk = 1\n[a]\nb = 2\n",
            "[x]\nk = 1\n\n\n\n[a]\nb = 2\n",
            "[x]\nk = 1\n# over b\n\n[a]\nb = 2\n",
        ]
    );
}

#[test]
fn value_replacing_a_block_under_a_header_takes_default_key_spacing() {
    let (_dir, editor) = inline_editor(
        "[harness.prompt_cache_ttl]\nfallback = \"off\"\n\n[harness.prompt_cache_ttl.codex]\napi-key = \"20m\"\n",
    );
    editor.set("harness.prompt_cache_ttl.codex", "12h").unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert!(rendered.contains("\ncodex = \"12h\"\n"), "{rendered}");
}

#[test]
fn block_replacing_a_value_takes_default_header_spacing() {
    let (dir, editor) = inline_editor("");
    let loop_path = dir.path().join("loop.toml");
    std::fs::write(
        &loop_path,
        "[tasks.x]\ncheck = \"true\"\n\n[tasks.y]\n# the check note\ncheck = \"true\"\n",
    )
    .unwrap();
    let judge = r#"{ agent = "claude", prompt = "done?" }"#;
    editor.set("loop.tasks.x.check", judge).unwrap();
    editor.set("loop.tasks.y.check", judge).unwrap();

    let rendered = std::fs::read_to_string(&loop_path).unwrap();
    assert!(rendered.contains("\n[tasks.x.check]\n"), "{rendered}");
    let task = rendered.find("\n[tasks.y]\n").expect("y task");
    let note = rendered.find("# the check note\n").expect("check note");
    let header = rendered.find("\n[tasks.y.check]\n").expect("y check");
    assert!(task < note && note < header, "{rendered}");
}

#[test]
fn first_agents_value_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    editor.set("agents.worktree.dir", "../worktrees").unwrap();
    editor.set("agents.isolation", "scoped").unwrap();

    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_harness_template_comments_stay_under_harness(&rendered);
    assert_eq!(
        editor.get(Some("agents.isolation")).unwrap().as_str(),
        Some("scoped")
    );
}

#[test]
fn merging_implicit_parent_keeps_harness_template_comments() {
    let (_dir, editor) = inline_editor(
        "[harness.prompt_cache_ttl.codex]\napi-key = \"20m\"\n\n[harness.prompt_cache_ttl]\nfallback = \"off\"\n",
    );
    let report = editor.merge_defaults().unwrap();
    assert!(report.files[0].skipped.is_empty(), "{report:?}");
    assert!(
        matches!(report.files[0].action, MergeAction::Merged { kept: 2 }),
        "{report:?}"
    );
    let rendered = std::fs::read_to_string(editor.files.core_path()).unwrap();
    assert_harness_template_comments_stay_under_harness(&rendered);
}

fn assert_harness_template_comments_stay_under_harness(rendered: &str) {
    let harness_body = rendered
        .split_once("\n[harness]\n")
        .expect("harness table")
        .1
        .split_once("\n[")
        .expect("table after harness")
        .0;
    for comment in [
        "# cache_keepalive = true",
        "# prompt_cache_margin = \"1m\"",
        "## [harness.prompt_cache_ttl]",
    ] {
        assert_eq!(
            rendered.matches(comment).count(),
            1,
            "lost or duplicated {comment}: {rendered}"
        );
        assert!(
            harness_body.contains(comment),
            "harness comment escaped its section: {comment}: {rendered}"
        );
    }
}

#[test]
fn merge_keeps_lsp_server_entry_whole() {
    for seed in [
        format!("[lsp.servers.rust]\n{LSP_SERVER_FIELDS}"),
        format!(
            "lsp = {{ servers = {{ rust = {{ {} }} }} }}",
            LSP_SERVER_FIELDS.lines().collect::<Vec<_>>().join(", ")
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, seed).unwrap();
        let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
            &path,
            dir.path().join("agents-home"),
        ));
        let expected = MachineConfig::load_from(&path, editor.files.agents_home())
            .unwrap()
            .lsp
            .servers["rust"]
            .clone();
        let report = editor.merge_defaults().unwrap();
        assert!(report.files[0].skipped.is_empty(), "{report:?}");
        assert!(matches!(
            report.files[0].action,
            MergeAction::Merged { kept: 1 }
        ));
        let actual = MachineConfig::load_from(&path, editor.files.agents_home()).unwrap();
        assert_eq!(actual.lsp.servers["rust"], expected);
        assert_eq!(
            actual.lsp.servers["rust"].init_options,
            Some(
                serde_json::json!({"checkOnSave": false, "workspace": {"symbol": {"search": {"kind": "all_symbols", "limit": 10000}}}})
            )
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n[lsp.servers.rust]\n"));
        assert!(text.find("[harness]") < text.find("\n[lsp.servers.rust]\n"));
        assert!(text.contains("## [lsp.servers.rust]"));
        assert!(!text.contains("\n[lsp.servers]\n"));
    }
}

#[test]
fn merge_keeps_empty_named_account() {
    let claude = crate::ids::AgentKind::new_unchecked("claude");
    let personal = "personal".parse::<crate::ids::LoginName>().unwrap();
    for seed in [
        "[accounts.claude.personal]\n",
        "[accounts.claude]\npersonal = {}\n",
        "accounts = { claude = { personal = {} } }\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, seed).unwrap();
        let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
            &path,
            dir.path().join("agents-home"),
        ));
        let report = editor.merge_defaults().unwrap();
        assert!(report.files[0].skipped.is_empty(), "{seed}: {report:?}");
        assert!(
            matches!(report.files[0].action, MergeAction::Merged { kept: 1 }),
            "{seed}: {report:?}"
        );
        let config = MachineConfig::load_from(&path, editor.files.agents_home()).unwrap();
        assert_eq!(config.accounts.claude[&personal].home, None, "{seed}");
        let merged = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            merged.matches("\n[accounts.claude.personal]\n").count(),
            1,
            "{seed}: {merged}"
        );

        editor.merge_defaults().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), merged, "{seed}");
        editor
            .upsert_named_account(&claude, &personal, None, None, None)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), merged, "{seed}");
    }
}

#[test]
fn merge_keeps_empty_and_homed_named_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[accounts.claude.work]\nhome = \"/srv/homes/work\"\n\n[accounts.codex.personal]\n",
    )
    .unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let report = editor.merge_defaults().unwrap();
    assert!(report.files[0].skipped.is_empty(), "{report:?}");
    assert!(
        matches!(report.files[0].action, MergeAction::Merged { kept: 2 }),
        "{report:?}"
    );
    let accounts = MachineConfig::load_from(&path, editor.files.agents_home())
        .unwrap()
        .accounts;
    let work = "work".parse::<crate::ids::LoginName>().unwrap();
    let personal = "personal".parse::<crate::ids::LoginName>().unwrap();
    assert_eq!(
        accounts.claude[&work].home.as_deref(),
        Some(Path::new("/srv/homes/work"))
    );
    assert_eq!(accounts.codex[&personal].home, None);
}

const LSP_SERVER_FIELDS: &str = r#"command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
"#;

#[test]
fn merge_skips_incomplete_lsp_server_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            "[lsp.servers.rust]\n{}",
            LSP_SERVER_FIELDS.replace("extensions = [\"rs\"]\n", "")
        ),
    )
    .unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let report = editor.merge_defaults().unwrap();
    assert_eq!(report.files[0].skipped.len(), 1);
    assert_eq!(report.files[0].skipped[0].key, "lsp.servers.rust");
    assert!(
        report.files[0].skipped[0]
            .reason
            .contains("missing field `extensions`")
    );
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .any(|line| line.starts_with("[lsp.servers.rust]"))
    );
}

#[test]
fn merge_keeps_notification_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[[notifications.handler]]\nname = \"ntfy\"\ncommand = \"ntfy publish rimz\"\n",
    )
    .unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let report = editor.merge_defaults().unwrap();
    assert!(report.files[0].skipped.is_empty(), "{report:?}");
    let config = MachineConfig::load_from(&path, editor.files.agents_home()).unwrap();
    assert_eq!(config.notifications.handler.len(), 1);
    assert_eq!(
        config.notifications.handler[0].name.as_deref(),
        Some("ntfy")
    );
    assert_eq!(config.notifications.handler[0].command, "ntfy publish rimz");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\n[[notifications.handler]]\n"));
    assert!(text.find("[harness]") < text.find("\n[[notifications.handler]]\n"));
}

#[test]
fn set_lsp_server_entry_renders_one_block_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let value = format!(
        "{{ {} }}",
        LSP_SERVER_FIELDS.lines().collect::<Vec<_>>().join(", ")
    );
    editor.set("lsp.servers.rust", &value).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\n[lsp.servers]\n"));
    assert!(text.find("[harness]") < text.find("\n[lsp.servers.rust]\n"));
    assert!(text.contains("## flip_compact"));
    assert!(text.contains("\n\n[lsp.servers.rust]\n"));
    editor.set("lsp.servers.rust", &value).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        editor.get(Some("lsp.servers.rust")).unwrap()["extensions"]
            .as_array()
            .unwrap()[0]
            .as_str(),
        Some("rs")
    );
}

#[test]
fn merge_defaults_is_byte_idempotent_with_appended_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!("[lsp.servers.rust]\n{LSP_SERVER_FIELDS}\n[[notifications.handler]]\nname = \"ntfy\"\ncommand = \"ntfy publish rimz\"\n"),
    )
    .unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let first = editor.merge_defaults().unwrap();
    assert!(first.files[0].skipped.is_empty());
    assert!(matches!(
        first.files[0].action,
        MergeAction::Merged { kept: 2 }
    ));
    let first_bytes = std::fs::read(&path).unwrap();
    editor.merge_defaults().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), first_bytes);
}

#[test]
fn merge_defaults_is_byte_idempotent_with_kept_overrides() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"timezone = "America/Los_Angeles"

[notifications]
desktop = "osc"

[tmux]
set_clipboard = "external"
[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
"#,
    )
    .expect("seed config");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));

    let first = editor.merge_defaults().expect("first merge");
    let first_bytes = std::fs::read(&path).expect("first config");
    let second = editor.merge_defaults().expect("second merge");
    let second_bytes = std::fs::read(&path).expect("second config");
    let first_kept = match first.files[0].action {
        MergeAction::Merged { kept } => kept,
        ref action => panic!("expected merged core file, got {action:?}"),
    };
    let second_kept = match second.files[0].action {
        MergeAction::Merged { kept } => kept,
        ref action => panic!("expected merged core file, got {action:?}"),
    };

    assert_eq!(first_kept, 4);
    assert_eq!(second_kept, first_kept);
    assert_eq!(second_bytes, first_bytes);
}

#[test]
fn merge_defaults_removes_unknown_machine_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[daemon]\nfuture = true\n\n[[daemon.pane]]\ncommand = \"stats\"\n",
    )
    .expect("seed config");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));

    let report = editor.merge_defaults().expect("merge");
    let rendered = std::fs::read_to_string(&path).expect("read config");

    assert!(!rendered.contains("future = true"), "{rendered}");
    assert!(
        report.files[0]
            .skipped
            .iter()
            .any(|skipped| skipped.key == "daemon.future"),
        "{report:?}"
    );
}

#[test]
fn set_document_value_renders_inline_table_arrays_as_table_blocks() {
    let mut doc = r#"
[agents.teams.forge]
layout = "planner,coder"
"#
    .parse::<DocumentMut>()
    .expect("parse config snippet");
    let path = parse_key("agents.teams.forge.roles").expect("key");
    let value = parse_edit_value(
        r#"[
            { role = "planner", profile = "claude-planner" },
            { role = "coder", profile = "codex-coder" }
        ]"#,
    );

    set_document_value(&mut doc, &path, value).expect("set roles");

    let rendered = doc.to_string();
    assert!(
        rendered.contains("[[agents.teams.forge.roles]]"),
        "roles should render as array-of-tables:\n{rendered}"
    );
    assert!(
        !rendered.contains("roles = ["),
        "roles should not render as an inline array:\n{rendered}"
    );
    assert!(
        rendered
            .find("layout = \"planner,coder\"")
            .expect("layout survives")
            < rendered
                .find("[[agents.teams.forge.roles]]")
                .expect("roles block renders"),
        "layout should stay in the team table before role blocks:\n{rendered}"
    );
}

#[test]
fn set_document_value_renders_inline_tables_as_table_blocks() {
    let mut doc = DocumentMut::new();
    let path = document_key_for_set(&parse_key("loop.tasks").expect("key"));
    let value = parse_edit_value(
        r#"{ pr_watch = { agent = "codex", prompt = "check CI", root = "/r", every = "15m" }, self_wait = { wait = { kind = "claude", session = "s1", handle = "@planner" }, prompt = "resume", root = "/r", at = "09:30" } }"#,
    );

    set_document_value(&mut doc, &path, value).expect("set tasks");

    let rendered = doc.to_string();
    assert!(
        rendered.contains("[tasks.pr_watch]"),
        "scalar-only task should render as a table block:\n{rendered}"
    );
    let task = rendered
        .find("[tasks.self_wait]")
        .unwrap_or_else(|| panic!("task should render as a table block:\n{rendered}"));
    let wait = rendered
        .find("[tasks.self_wait.wait]")
        .unwrap_or_else(|| panic!("wait should render as a nested table block:\n{rendered}"));
    assert!(
        task < wait,
        "task table should render before wait table:\n{rendered}"
    );
    assert!(
        !rendered.contains("= { "),
        "inline tables should not survive:\n{rendered}"
    );
    assert!(
        !rendered.contains("tasks = {"),
        "tasks should not collapse to one inline table:\n{rendered}"
    );
}

#[test]
fn set_document_value_keeps_scalar_arrays_inline() {
    let mut doc = DocumentMut::new();
    let path = parse_key("agents.profiles.codex.args").expect("key");
    let value = parse_edit_value(r#"["--search", "none"]"#);

    set_document_value(&mut doc, &path, value).expect("set args");

    assert!(
        matches!(item_at(&doc, &path), Some(Item::Value(Value::Array(_)))),
        "scalar arrays should remain inline values:\n{doc}"
    );
}

#[test]
fn derived_set_keys_keep_legacy_surface() {
    for key in LEGACY_SET_KEYS {
        let parsed = parse_key(key).unwrap_or_else(|err| panic!("{key}: {err}"));
        validate_set_key(&test_files(), &parsed).unwrap_or_else(|err| panic!("set {key}: {err}"));
        assert!(
            is_known_get_key(&test_files(), &parsed).unwrap(),
            "get {key}"
        );
    }

    for key in ["nope", "theme.nope"] {
        let parsed = parse_key(key).expect("key");
        let err = validate_set_key(&test_files(), &parsed)
            .expect_err("legacy-invalid set key should stay rejected")
            .to_string();
        assert_eq!(err, format!("unknown config key `{key}`"));
    }

    let parsed = parse_key("notifications.handler").expect("key");
    let err = validate_set_key(&test_files(), &parsed)
        .expect_err("array-of-tables shorthand should stay rejected")
        .to_string();
    assert!(
        err.starts_with("config key `notifications.handler` is an array of tables; edit "),
        "unexpected error: {err}",
    );
    let parsed = parse_key("theme.display.context_meter.green.percent").expect("key");
    let err = validate_set_key(&test_files(), &parsed)
        .expect_err("context meter sub-field should stay rejected")
        .to_string();
    assert_eq!(
        err,
        "unknown config key `theme.display.context_meter.green.percent`"
    );
}

#[test]
fn validates_auto_redeem_min_gain_edits() {
    let key = parse_key("resume.auto_redeem_min_gain").unwrap();
    let value = parse_set_value(&key, "12h");
    assert_eq!(value.as_str(), Some("12h"));
    validate_set_value(&key, &value).unwrap();

    let err = validate_set_value(&key, &Value::from("one week"))
        .expect_err("invalid duration")
        .to_string();
    assert!(err.contains("auto_redeem_min_gain"), "{err}");
}

#[test]
fn bare_words_become_strings() {
    assert_eq!(parse_edit_value("always").as_str(), Some("always"));
    assert_eq!(parse_edit_value("80").as_integer(), Some(80));
    assert_eq!(parse_edit_value("false").as_bool(), Some(false));
}

#[test]
fn theme_scheme_values_are_parsed_as_strings() {
    let key = parse_key("theme.scheme").expect("key");
    assert_eq!(parse_set_value(&key, "0x96f").as_str(), Some("0x96f"));
    assert_eq!(
        parse_set_value(&key, "\"Catppuccin Mocha\"").as_str(),
        Some("Catppuccin Mocha")
    );

    let shorthand = parse_key("theme").expect("key");
    assert_eq!(parse_set_value(&shorthand, "0x96f").as_str(), Some("0x96f"));

    let numeric = parse_key("theme.display.max_cols").expect("key");
    assert_eq!(parse_set_value(&numeric, "80").as_integer(), Some(80));
}

#[test]
fn glyph_values_are_parsed_as_strings() {
    let set = parse_key("theme.glyphs.set").expect("key");
    assert_eq!(
        parse_set_value(&set, "nerd_font").as_str(),
        Some("nerd_font")
    );

    let shorthand = parse_key("theme.glyphs").expect("key");
    assert_eq!(
        parse_set_value(&shorthand, "nerd_font").as_str(),
        Some("nerd_font")
    );

    let leaf = parse_key("theme.glyphs.unicode.process.cpu").expect("key");
    assert_eq!(parse_set_value(&leaf, "1").as_str(), Some("1"));
}

#[test]
fn harness_smart_compact_values_are_parsed_as_strings() {
    let key = parse_key("harness.smart_compact").expect("key");

    assert_eq!(parse_set_value(&key, "70%").as_str(), Some("70%"));
    assert_eq!(parse_set_value(&key, "120000").as_str(), Some("120000"));
    assert_eq!(parse_set_value(&key, "180k").as_str(), Some("180k"));
    for raw in ["off", "272k", "272000"] {
        assert_eq!(parse_set_value(&key, raw).as_str(), Some(raw));
    }
}

#[test]
fn harness_auto_compact_values_are_parsed_as_strings() {
    let key = parse_key("harness.auto_compact").unwrap();
    for raw in ["off", "272k", "272000"] {
        assert_eq!(parse_set_value(&key, raw).as_str(), Some(raw));
    }
}

#[test]
fn harness_auto_compact_validation_accepts_off_and_native_windows() {
    let key = parse_key("harness.auto_compact").unwrap();
    for raw in ["off", "100k", "272000", "1m"] {
        validate_set_value(&key, &Value::from(raw)).unwrap();
    }
    for value in [
        Value::from("70%"),
        Value::from("99k"),
        Value::from("abc"),
        Value::from(272000),
    ] {
        assert_eq!(
            validate_set_value(&key, &value).unwrap_err().to_string(),
            "harness.auto_compact must be off or a token count from 100k through 1M, such as 272k"
        );
    }
}

#[test]
fn harness_compact_instruction_values_are_parsed_as_strings() {
    let key = parse_key("harness.compact_instruction").expect("key");

    for (raw, expected) in [
        ("keep the open questions", "keep the open questions"),
        ("\"\"", ""),
        ("[summary]", "[summary]"),
    ] {
        let value = parse_set_value(&key, raw);
        assert_eq!(value.as_str(), Some(expected));
        validate_set_value(&key, &value).expect("compact instruction string");
    }
    assert_eq!(
        validate_set_value(&key, &Value::from(true))
            .expect_err("non-string compact instruction")
            .to_string(),
        "harness.compact_instruction must be a string"
    );
}

#[test]
fn harness_flip_compact_values_are_parsed_as_strings() {
    let key = parse_key("harness.flip_compact").unwrap();
    for raw in ["120000", "off", "180k", "70%"] {
        assert_eq!(parse_set_value(&key, raw).as_str(), Some(raw));
    }
}

#[test]
fn harness_flip_compact_validation_accepts_off_and_thresholds() {
    let key = parse_key("harness.flip_compact").unwrap();
    for raw in ["off", "180k", "70%", "120000"] {
        validate_set_value(&key, &Value::from(raw)).expect("valid flip policy");
    }
    for value in [Value::from("soon"), Value::from("OFF"), Value::from(120000)] {
        assert_eq!(
            validate_set_value(&key, &value)
                .expect_err("invalid flip policy")
                .to_string(),
            "harness.flip_compact must be off, a token count such as 180k, or a percentage such as 70%"
        );
    }
}

#[test]
fn harness_idle_compact_values_are_parsed_as_strings() {
    let mode = parse_key("harness.idle_compact").expect("mode key");
    assert_eq!(parse_set_value(&mode, "off").as_str(), Some("off"));
    assert_eq!(parse_set_value(&mode, "100k").as_str(), Some("100k"));
}

#[test]
fn harness_prompt_cache_edits_validate_provider_and_duration() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path(),
    ));
    editor.set("harness.cache_keepalive", "false").unwrap();
    for value in ["5m", "off"] {
        editor
            .set("harness.prompt_cache_ttl.claude", value)
            .unwrap();
    }
    for (key, value) in [("claude", "1m"), ("claude", "on"), ("typo", "5m")] {
        let key = format!("harness.prompt_cache_ttl.{key}");
        let err = editor.set(&key, value).unwrap_err().to_string();
        assert!(err.contains(&key), "{err}");
    }
    for value in ["2h", "off"] {
        editor.set("harness.cache_keepalive_max", value).unwrap();
    }
    for value in ["1m", "6"] {
        let err = editor
            .set("harness.cache_keepalive_max", value)
            .unwrap_err()
            .to_string();
        assert!(err.contains("harness.cache_keepalive_max"), "{err}");
    }
    let split = editor.set("harness.prompt_cache_ttl.codex.api-key", "20m");
    let value = editor
        .get(Some("harness.prompt_cache_ttl.codex.api-key"))
        .ok();
    let fallback = editor.set("harness.prompt_cache_ttl.fallback", "off");
    let weekly = editor
        .set("harness.prompt_cache_ttl.amp.weekly", "1h")
        .unwrap_err()
        .to_string();
    editor.set("harness.prompt_cache_ttl.codex", "12h").unwrap();
    let over_plain = editor
        .set("harness.prompt_cache_ttl.codex.api-key", "20m")
        .unwrap_err()
        .to_string();
    assert_eq!(
        (
            split.is_ok(),
            value.as_ref().and_then(|value| value.as_str()),
            fallback.is_ok(),
            weekly.contains("subscription") && weekly.contains("api-key"),
            over_plain.contains("`codex` is not a table")
        ),
        (true, Some("20m"), true, true, true),
        "split: {split:?}; fallback: {fallback:?}; weekly: {weekly}; plain: {over_plain}"
    );
}

#[test]
fn prompt_cache_margin_edits_validate_the_merged_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path(),
    ));
    let fallback_error = editor
        .set("harness.prompt_cache_margin", "2h")
        .unwrap_err()
        .to_string();
    assert!(fallback_error.contains("fallback"), "{fallback_error}");
    let changed = editor.set("harness.prompt_cache_margin", "2m");
    assert!(
        changed.is_ok(),
        "the shared margin must be editable: {changed:?}"
    );
    assert!(editor.set("harness.prompt_cache_ttl.claude", "2m").is_err());
    assert!(editor.set("harness.prompt_cache_margin", "30m").is_err());
    editor
        .set("harness.prompt_cache_ttl.codex.api-key", "5m")
        .unwrap();
    let split_error = editor
        .set("harness.prompt_cache_margin", "10m")
        .unwrap_err()
        .to_string();
    assert!(split_error.contains("codex api-key"), "{split_error}");
    assert_eq!(
        editor
            .get(Some("harness.prompt_cache_margin"))
            .unwrap()
            .as_str(),
        Some("2m")
    );
    editor.set("harness.prompt_cache_margin", "30s").unwrap();
    editor.set("harness.prompt_cache_ttl.claude", "1m").unwrap();
    editor.set("harness.cache_keepalive_max", "1m").unwrap();
}

#[test]
fn harness_turn_budget_values_are_validated_as_plain_amount_strings() {
    let key = parse_key("harness.turn_budget").expect("key");

    let value = parse_set_value(&key, "3");
    assert_eq!(value.as_str(), Some("3"));
    validate_set_value(&key, &value).expect("plain turn cap");

    let err = validate_set_value(&key, &Value::from("3/day"))
        .expect_err("daily window is invalid for a turn cap")
        .to_string();
    assert!(
        err.contains("must be a plain dollar amount"),
        "unexpected error: {err}"
    );
}

#[test]
fn harness_smart_compact_validation_rejects_bad_values() {
    let key = parse_key("harness.smart_compact").expect("key");

    validate_set_value(&key, &Value::from("70%")).expect("percent threshold");
    validate_set_value(&key, &Value::from("120000")).expect("token threshold");
    validate_set_value(&key, &Value::from("180k")).expect("k suffix");
    assert!(validate_set_value(&key, &Value::from("off")).is_ok());

    let err = validate_set_value(&key, &Value::from("abc"))
        .expect_err("invalid smart-compact threshold")
        .to_string();
    assert!(
        err == "harness.smart_compact must be off, a token count such as 180k, or a percentage such as 70%",
        "unexpected error: {err}"
    );
}

#[test]
fn harness_idle_compact_validation_accepts_off_and_token_floors() {
    let mode = parse_key("harness.idle_compact").expect("mode key");
    for value in ["off", "100k", "250000", "1000k"] {
        validate_set_value(&mode, &Value::from(value)).expect("idle compact mode");
    }
    let err = validate_set_value(&mode, &Value::from("auto"))
        .expect_err("invalid idle compact mode")
        .to_string();
    assert_eq!(
        err,
        "harness.idle_compact must be off or a positive token floor in digits or k, such as 100k or 1000k"
    );
    for retired in ["on", "2h", "25m", "1m", "0"] {
        assert!(
            validate_set_value(&mode, &Value::from(retired)).is_err(),
            "{retired}"
        );
    }

    let editor = ConfigEditor::new(test_files());
    assert!(matches!(
        editor.get(Some("harness.idle_compact_after")),
        Err(ConfigEditErr::UnknownKey { .. })
    ));
}

#[test]
fn harness_keep_warm_floor_validation_accepts_off_and_durations() {
    let floor = parse_key("harness.keep_warm_min_ttl").expect("floor key");
    for value in ["off", "15m", "1h"] {
        validate_set_value(&floor, &Value::from(value)).expect("keep-warm floor");
    }
    for value in [Value::from("on"), Value::from("15"), Value::from(true)] {
        let err = validate_set_value(&floor, &value)
            .expect_err("invalid keep-warm floor")
            .to_string();
        assert_eq!(
            err,
            "harness.keep_warm_min_ttl must be off or a duration such as 15m"
        );
    }
}

#[test]
fn retired_idle_compaction_is_rejected_then_removed_preserving_other_settings() {
    for (mode, has_after) in [
        ("auto", true),
        ("always", false),
        ("on", false),
        ("2h", false),
        ("59m", true),
        ("off", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let after = if has_after {
            "idle_compact_after = \"59m\"\n"
        } else {
            ""
        };
        let text = format!(
            "# keep this\n[harness]\nidle_compact = \"{mode}\"\n{after}compact_instruction = \"keep me\" # retained\n"
        );
        std::fs::write(&path, &text).unwrap();
        let err = crate::config::parse_core_text_strict(&path, &text)
            .err()
            .expect("retired key rejected");
        assert!(err.to_string().contains("run `rimz setup`"));
        let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
            &path,
            dir.path().join("agents-home"),
        ));
        let removed = editor.retire_idle_compact_keys().unwrap();
        assert_eq!(
            removed.contains(&"harness.idle_compact_after".to_owned()),
            has_after
        );
        assert_eq!(
            removed.len(),
            usize::from(has_after) + usize::from(mode != "off")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep this"));
        assert!(text.contains("compact_instruction = \"keep me\" # retained"));
        let config = crate::config::parse_core_text_strict(&path, &text).unwrap();
        assert_eq!(
            serde_json::to_value(config.harness).unwrap()["idle_compact"],
            if mode == "off" { "off" } else { "100k" }
        );
        assert!(editor.retire_idle_compact_keys().unwrap().is_empty());
        assert!(matches!(
            editor.set("harness.idle_compact_after", "59m"),
            Err(ConfigEditErr::UnknownKey { .. })
        ));
    }
}

#[test]
fn gc_older_than_is_a_validated_duration_string() {
    for name in ["gc.older_than", "gc.tmp_grace"] {
        let key = parse_key(name).expect("key");
        let value = parse_set_value(&key, "3d");
        assert_eq!(value.as_str(), Some("3d"));
        validate_set_value(&key, &value).expect("day span");
        for bad in ["0d", "soon"] {
            let result = validate_set_value(&key, &Value::from(bad));
            assert!(result.is_err(), "accepted {name} {bad}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("use a duration such as 8h or 3d")
            );
        }
    }
    let key = parse_key("gc.tmp_floor_min").unwrap();
    assert!(validate_set_value(&key, &Value::from("10x")).is_err());
    assert_eq!(parse_set_value(&key, "0").as_str(), Some("0"));
    let key = parse_key("gc.tmp_floor_percent").unwrap();
    for bad in [-1, 101] {
        assert!(validate_set_value(&key, &Value::from(bad)).is_err());
    }
}

#[test]
fn gc_transcript_retention_round_trips_both_forms_and_refuses_invalid_values() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("definitions"),
    ));
    for value in ["forever", "30d", "12h"] {
        let result = editor.set("gc.transcript_retention", value);
        assert!(result.is_ok(), "retention must be editable: {result:?}");
        assert_eq!(
            editor
                .get(Some("gc.transcript_retention"))
                .unwrap()
                .as_str(),
            Some(value)
        );
    }
    for bad in ["never", "0d", "30"] {
        let result = editor.set("gc.transcript_retention", bad);
        assert!(result.is_err(), "accepted {bad}");
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("gc.transcript_retention")
                && error.contains("forever")
                && error.contains("duration"),
            "{error}"
        );
    }
    assert_eq!(
        editor
            .get(Some("gc.transcript_retention"))
            .unwrap()
            .as_str(),
        Some("12h")
    );
}

#[test]
fn theme_scheme_validation_accepts_bundled_names_and_rejects_auto() {
    let key = parse_key("theme.scheme").expect("key");

    validate_set_value(&key, &Value::from("Afterglow")).expect("bundled theme");
    validate_set_value(&key, &Value::from("0x96f")).expect("numeric-looking bundled theme");

    let err = validate_set_value(&key, &Value::from("auto"))
        .expect_err("auto is no longer a selectable scheme")
        .to_string();
    assert!(
        err.contains("unknown sidebar theme scheme `auto`"),
        "unexpected error: {err}"
    );
}

#[test]
fn glyph_validation_accepts_sets_and_rejects_bad_values() {
    let set = parse_key("theme.glyphs.set").expect("key");
    validate_set_value(&set, &Value::from("unicode")).expect("unicode");
    validate_set_value(&set, &Value::from("nerd_font")).expect("nerd_font");

    let err = validate_set_value(&set, &Value::from("auto"))
        .expect_err("unknown glyph set")
        .to_string();
    assert!(
        err.contains("unknown theme glyph set `auto`"),
        "unexpected error: {err}"
    );

    let leaf = parse_key("theme.glyphs.unicode.tokens.total").expect("key");
    validate_set_value(&leaf, &Value::from("◇")).expect("single-cell glyph");
    validate_set_value(&leaf, &Value::from("\u{efa0} ")).expect("double-width glyph");
    let err = validate_set_value(&leaf, &Value::from("abc"))
        .expect_err("over-wide glyph")
        .to_string();
    assert!(
        err.contains("must occupy one or two terminal cells"),
        "unexpected error: {err}"
    );
}

#[test]
fn sidebar_theme_set_key_is_scheme_shorthand() {
    let key = parse_key("theme").expect("key");
    assert_eq!(
        normalize_set_key(&key, &Value::from("Afterglow")).expect("normalize"),
        parse_key("theme.scheme").expect("scheme key")
    );

    let err = normalize_set_key(&key, &Value::from(256))
        .expect_err("shorthand only accepts a scheme string")
        .to_string();
    assert!(
        err.contains("theme shorthand sets a scheme string"),
        "unexpected error: {err}"
    );
}

#[test]
fn sidebar_glyphs_set_key_is_set_shorthand() {
    let key = parse_key("theme.glyphs").expect("key");
    assert_eq!(
        normalize_set_key(&key, &Value::from("nerd_font")).expect("normalize"),
        parse_key("theme.glyphs.set").expect("glyph set key")
    );

    let err = normalize_set_key(&key, &Value::from(256))
        .expect_err("shorthand only accepts a set string")
        .to_string();
    assert!(
        err.contains("theme.glyphs shorthand sets a glyph set string"),
        "unexpected error: {err}"
    );
}

#[test]
fn inline_account_table_update_preserves_comments_and_siblings() {
    assert_inline_account_update(
        "[accounts.codex]\nteam-2 = {}\n# team one\nteam-1 = { history = \"shared\" }   # hand\n\n[accounts.codex.team-9]\nlabel = 'other'\n\n[notifications]\n# tail\n",
        "[accounts.codex]\nteam-2 = {}\n\n# team one\n[accounts.codex.team-1]   # hand\nhistory = \"standalone\"\nlabel = \"work\"\n\n[accounts.codex.team-9]\nlabel = 'other'\n\n[notifications]\n# tail\n",
    );
}

#[test]
fn inline_multiline_account_comments_stay_with_entries() {
    assert_inline_account_update(
        "[accounts.codex]\nteam-1 = {\n  # pool choice\n  history = \"shared\", # history mode\n  # display choice\n  label = \"a\", # display\n  # after last\n} # hand\n",
        "[accounts.codex]\n\n[accounts.codex.team-1] # hand\n# pool choice\nhistory = \"standalone\" # history mode\n# display choice\nlabel = \"work\" # display\n# after last\n",
    );
}

#[test]
fn inline_multiline_account_brace_comments_survive() {
    assert_inline_account_update(
        "[accounts.codex]\nteam-1 = { # opening\n  history = \"shared\",\n  label = \"a\" # display before closing\n  # after last without comma\n} # closing\n",
        "[accounts.codex]\n\n[accounts.codex.team-1] # closing\n# opening\nhistory = \"standalone\"\nlabel = \"work\" # display before closing\n# after last without comma\n",
    );
}

#[test]
fn inline_multiline_empty_account_comments_follow_header() {
    let (_dir, editor) =
        inline_editor("[accounts.codex]\nteam-1 = { # opening\n  # empty account\n} # closing\n");
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let name = "team-1".parse().unwrap();
    let update = || editor.upsert_named_account(&kind, &name, None, None, None);
    update().unwrap();
    let path = editor.files.core_path();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        text,
        "[accounts.codex]\n\n[accounts.codex.team-1] # closing\n# opening\n# empty account\n"
    );
    let config = MachineConfig::parse_text(path, &text, editor.files.agents_home()).unwrap();
    assert!(config.accounts.codex.contains_key(&name));
    update().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn inline_multiline_nested_comments_stay_untouched() {
    assert_inline_account_update(
        "[accounts]\ncodex = {\n  # stay inline\n  team-2 = {\n    # nested account\n    history = \"shared\", # nested mode\n  },\n  # converted account\n  team-1 = { history = \"shared\" },\n}\n",
        "[accounts]\n\n[accounts.codex]\n# stay inline\nteam-2 = {\n    # nested account\n    history = \"shared\", # nested mode\n  }\n\n# converted account\n[accounts.codex.team-1]\nhistory = \"standalone\"\nlabel = \"work\"\n",
    );
}

#[test]
fn inline_multiline_dotted_entry_comments_stay_with_lines() {
    assert_inline_account_update(
        "[accounts]\ncodex = {\n  team-3 = {}, # beside team-3\n  # over dotted\n  team-2.label = \"x\", # beside dotted\n  team-2.history = \"shared\" # before comma\n  , # beside second\n  team-1 = {},\n}\n",
        "[accounts]\n\n[accounts.codex]\nteam-3 = {} # beside team-3\n# over dotted\nteam-2.label = \"x\" # beside dotted\nteam-2.history = \"shared\" # before comma\n# beside second\n\n[accounts.codex.team-1]\nhistory = \"standalone\"\nlabel = \"work\"\n",
    );
}

#[test]
fn inline_provider_table_update_converts_only_the_named_path() {
    assert_inline_account_update(
        "[accounts]\ncodex = { team-2 = {}, team-1 = { history = \"shared\" } } # provider\n\n[notifications]\n# tail\n",
        "[accounts]\n\n[accounts.codex] # provider\nteam-2 = {}\n\n[accounts.codex.team-1]\nhistory = \"standalone\"\nlabel = \"work\"\n\n[notifications]\n# tail\n",
    );
}

#[test]
fn inline_root_table_update_precedes_existing_sections() {
    assert_inline_account_update(
        "# accounts here\naccounts = { codex = { team-2 = {}, team-1 = { history = \"shared\" } } }  # root\n\n[notifications]\n# tail\n",
        "\n# accounts here\n[accounts]  # root\n\n[accounts.codex]\nteam-2 = {}\n\n[accounts.codex.team-1]\nhistory = \"standalone\"\nlabel = \"work\"\n\n[notifications]\n# tail\n",
    );
}

fn inline_editor(seed: &str) -> (tempfile::TempDir, ConfigEditor) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, seed).unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    (dir, editor)
}

fn assert_inline_account_update(seed: &str, expected: &str) {
    let (_dir, editor) = inline_editor(seed);
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let name = "team-1".parse().unwrap();
    let update = || {
        editor.upsert_named_account(
            &kind,
            &name,
            None,
            Some(super::super::AccountHistory::Standalone),
            Some("work"),
        )
    };
    let result = update();
    assert!(
        result.is_ok(),
        "inline account must be editable: {result:?}"
    );
    let path = editor.files.core_path();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text, expected);
    assert_eq!(text.matches("[accounts.codex.team-1]").count(), 1);
    let config = MachineConfig::parse_text(path, &text, editor.files.agents_home()).unwrap();
    assert_eq!(config.accounts.codex[&name].history.as_str(), "standalone");
    assert_eq!(config.accounts.codex[&name].label.as_deref(), Some("work"));
    update().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn inline_account_label_clear_keeps_an_empty_declaration() {
    let (_dir, editor) = inline_editor("[accounts.claude]\npersonal = { label = \"work\" }\n");
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let name = "personal".parse().unwrap();
    let result = editor.upsert_named_account(&kind, &name, None, None, Some(""));
    assert!(result.is_ok(), "inline label must clear: {result:?}");
    let path = editor.files.core_path();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text, "[accounts.claude]\n\n[accounts.claude.personal]\n");
    let config = MachineConfig::parse_text(path, &text, editor.files.agents_home()).unwrap();
    assert!(config.accounts.claude.contains_key(&name));
    assert_eq!(config.accounts.claude[&name].label, None);
}

#[test]
fn inline_account_no_field_update_preserves_the_declaration() {
    for fields in ["", "history = 'standalone', label = 'work'"] {
        let (_dir, editor) = inline_editor(&format!("[accounts.codex]\nteam-1 = {{ {fields} }}\n"));
        let result = editor.upsert_named_account(
            &crate::ids::AgentKind::new_unchecked("codex"),
            &"team-1".parse().unwrap(),
            None,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "inline declaration must survive: {result:?}"
        );
        let path = editor.files.core_path();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("[accounts.codex.team-1]\n"), "{text}");
        let config = MachineConfig::parse_text(path, &text, editor.files.agents_home()).unwrap();
        let account = &config.accounts.codex["team-1"];
        assert_eq!(
            account.label.as_deref(),
            (!fields.is_empty()).then_some("work")
        );
        assert_eq!(
            account.history.as_str(),
            if fields.is_empty() {
                "shared"
            } else {
                "standalone"
            }
        );
    }
}

#[test]
fn inline_config_set_converts_account_and_other_containers() {
    for (seed, key, value, expected) in [
        (
            "[accounts.codex]\nteam-1 = { history = 'shared' }\n",
            "accounts.codex.team-1.label",
            "work",
            "[accounts.codex]\n\n[accounts.codex.team-1]\nhistory = 'shared'\nlabel = \"work\"\n",
        ),
        (
            "harness = { auto_compact = 'off' } # harness\n\n[notifications]\n",
            "harness.auto_compact",
            "200k",
            "[harness] # harness\nauto_compact = \"200k\"\n\n[notifications]\n",
        ),
    ] {
        let (_dir, editor) = inline_editor(seed);
        let result = editor.set(key, value);
        assert!(
            result.is_ok(),
            "inline config path must be editable: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(editor.files.core_path()).unwrap(),
            expected
        );
    }
}

#[test]
fn non_table_account_segment_still_fails_with_document_shape() {
    let (_dir, editor) = inline_editor("[accounts]\nclaude = 'not a table'\n");
    assert!(matches!(
        editor.upsert_named_account(
            &crate::ids::AgentKind::new_unchecked("claude"),
            &"personal".parse().unwrap(),
            None,
            None,
            None,
        ),
        Err(ConfigEditErr::DocumentShape { segment }) if segment == "claude"
    ));
}

#[test]
fn inline_account_use_converts_the_selection_table() {
    let (_dir, editor) = inline_editor("[accounts]\nuse = { claude = 'work' } # selection\n");
    let result = editor.use_account(
        &crate::ids::AgentKind::new_unchecked("claude"),
        &"personal".parse().unwrap(),
    );
    assert!(
        result.is_ok(),
        "inline selection must be editable: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(editor.files.core_path()).unwrap(),
        "[accounts]\n\n[accounts.use] # selection\nclaude = \"personal\"\n"
    );
}

#[test]
fn inline_account_removal_drops_only_empty_provider_tables() {
    for seed in [
        "[accounts]\ncodex = { team-1 = {}, team-2 = {} }\n",
        "accounts = { codex = { team-1 = {}, team-2 = {} } }\n",
    ] {
        let (_dir, editor) = inline_editor(seed);
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let result = editor.remove_named_account(&kind, &"team-1".parse().unwrap());
        assert!(
            matches!(result, Ok(true)),
            "inline account must be removable: {result:?}"
        );
        let text = std::fs::read_to_string(editor.files.core_path()).unwrap();
        assert!(
            !text.contains("team-1") && text.contains("team-2 = {}"),
            "{text}"
        );
        editor
            .remove_named_account(&kind, &"team-2".parse().unwrap())
            .unwrap();
        let text = std::fs::read_to_string(editor.files.core_path()).unwrap();
        assert!(
            !text.contains("codex"),
            "empty provider table remains: {text}"
        );
    }
}

#[test]
fn inline_first_account_is_placed_beside_its_section() {
    for seed in [
        "[accounts]\ncodex = { team-2 = {} }\n\n[notifications]\n# tail\n",
        "accounts = { codex = { team-2 = {} } }\n\n[notifications]\n# tail\n",
    ] {
        let (_dir, editor) = inline_editor(seed);
        let result = editor.upsert_named_account(
            &crate::ids::AgentKind::new_unchecked("codex"),
            &"team-1".parse().unwrap(),
            None,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "inline provider must allow a first add: {result:?}"
        );
        let text = std::fs::read_to_string(editor.files.core_path()).unwrap();
        assert!(
            text.contains("team-2 = {}\n\n[accounts.codex.team-1]\n\n[notifications]\n# tail\n"),
            "{text}"
        );
    }
}

#[test]
fn removing_named_account_restores_template_comments() {
    let (dir, editor) = inline_editor(Kind::Core.template());
    let claude = crate::ids::AgentKind::new_unchecked("claude");
    let work = "work".parse().unwrap();
    editor
        .upsert_named_account(&claude, &work, Some(&dir.path().join("work")), None, None)
        .unwrap();
    assert!(editor.remove_named_account(&claude, &work).unwrap());
    assert_eq!(
        std::fs::read_to_string(editor.files.core_path()).unwrap(),
        Kind::Core.template()
    );
}

#[test]
fn clearing_account_selection_restores_template_comments() {
    let (_dir, editor) = inline_editor(Kind::Core.template());
    let codex = crate::ids::AgentKind::new_unchecked("codex");
    editor.set("accounts.use.codex", "personal").unwrap();
    editor
        .use_account(&codex, &"default".parse().unwrap())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(editor.files.core_path()).unwrap(),
        Kind::Core.template()
    );
}

#[test]
fn named_account_edits_preserve_comments_and_sibling_account_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "# my machine\n[accounts.budget]\nclaude = \"100/day\" # the cap\n\n[accounts.usage_limit_usd]\n# ceilings\n\n[notifications]\n",
    )
    .expect("write config");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let claude = crate::ids::AgentKind::new_unchecked("claude");
    let work = "work".parse::<crate::ids::LoginName>().unwrap();
    let personal = "personal".parse::<crate::ids::LoginName>().unwrap();

    editor
        .upsert_named_account(
            &claude,
            &work,
            Some(Path::new("/srv/homes/work")),
            None,
            None,
        )
        .expect("declare work");
    editor
        .upsert_named_account(&claude, &personal, None, None, None)
        .expect("declare personal");
    let text = std::fs::read_to_string(&path).expect("read config");
    assert!(text.contains("# my machine"), "{text}");
    assert!(text.contains("claude = \"100/day\" # the cap"), "{text}");
    let parsed: crate::config::AccountsConfig = toml::from_str::<toml::Table>(&text)
        .expect("parse")["accounts"]
        .clone()
        .try_into()
        .expect("accounts");
    assert_eq!(
        parsed.claude["work"].home.as_deref(),
        Some(Path::new("/srv/homes/work"))
    );
    assert_eq!(parsed.claude["personal"].home, None);
    assert!(!text.contains("history"), "{text}");
    assert!(!text.contains("[accounts.claude]"), "{text}");
    assert!(
        text.contains("[accounts.usage_limit_usd]\n# ceilings\n\n[accounts.claude.work]\n"),
        "a declared account follows the accounts section and its comments: {text}"
    );
    assert!(
        text.find("[accounts.claude.personal]") < text.find("\n\n[notifications]"),
        "{text}"
    );

    editor
        .upsert_named_account(
            &claude,
            &work,
            None,
            Some(crate::config::AccountHistory::Standalone),
            None,
        )
        .expect("set history");
    let edited = std::fs::read_to_string(&path).expect("read config");
    assert_eq!(
        edited,
        text.replace(
            "home = \"/srv/homes/work\"\n",
            "home = \"/srv/homes/work\"\nhistory = \"standalone\"\n"
        ),
        "only the history field changes"
    );

    assert!(editor.remove_named_account(&claude, &work).expect("remove"));
    assert!(
        !editor
            .remove_named_account(&claude, &work)
            .expect("remove twice")
    );
    let text = std::fs::read_to_string(&path).expect("read config");
    assert!(text.contains("# my machine"), "{text}");
    assert!(text.contains("claude = \"100/day\" # the cap"), "{text}");
    assert!(text.contains("[accounts.claude.personal]"), "{text}");
    assert!(!text.contains("work"), "{text}");

    assert!(
        editor
            .remove_named_account(&claude, &personal)
            .expect("remove last")
    );
    let text = std::fs::read_to_string(&path).expect("read config");
    assert!(!text.contains("[accounts.claude"), "{text}");
    assert!(text.contains("[accounts.budget]"), "{text}");
}

#[test]
fn account_label_edits_preserve_comments_siblings_and_strict_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "# machine\n[accounts.codex.team]\nhome = '/srv/team' # home\nhistory = 'standalone'\n\n[accounts.codex.other]\n# sibling\n";
    std::fs::write(&path, original).unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let name = "team".parse().unwrap();
    editor
        .upsert_named_account(&kind, &name, None, None, Some("work"))
        .unwrap();
    let labelled = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        labelled,
        original.replace(
            "history = 'standalone'\n",
            "history = 'standalone'\nlabel = \"work\"\n"
        )
    );
    editor
        .upsert_named_account(&kind, &name, None, None, None)
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), labelled);
    editor
        .upsert_named_account(&kind, &name, None, None, Some(""))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    std::fs::write(
        &path,
        original.replace(
            "history = 'standalone'\n",
            "history = 'standalone'\nlabel = ''\n",
        ),
    )
    .unwrap();
    let machine = MachineConfig::load_from(&path, &dir.path().join("agents-home"));
    assert!(
        machine.is_ok(),
        "an invalid display label never breaks strict loading: {machine:?}"
    );
    editor.set("accounts.codex.team.history", "shared").unwrap();
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(
        edited.contains("label = ''") && edited.contains("history = \"shared\""),
        "{edited}"
    );
    assert!(editor.set("accounts.codex.team.label", " work").is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn machine_selection_edits_preserve_comments_and_clear_on_removal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# machine\n[accounts.budget]\ncodex = \"25/day\" # cap\n[accounts.codex.work]\n[accounts.codex.personal]\n").unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents"),
    ));
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    let work = "work".parse().unwrap();
    editor.set("accounts.use.codex", "work").unwrap();
    assert_eq!(
        editor.get(Some("accounts.use.codex")).unwrap().as_str(),
        Some("work")
    );
    assert!(editor.set("accounts.use", "{}").is_err());
    assert!(editor.set("accounts.use.codex.extra", "work").is_err());
    assert!(editor.get(Some("accounts.use.codex.extra")).is_err());
    editor
        .use_account(&kind, &crate::ids::LoginName::default_login())
        .unwrap();
    let cleared = std::fs::read_to_string(&path).unwrap();
    assert!(!cleared.contains("[accounts.use]"), "{cleared}");
    assert!(
        cleared.contains("# machine")
            && cleared.contains("# cap")
            && cleared.contains("[accounts.codex.work]"),
        "{cleared}"
    );
    editor
        .use_account(&kind, &crate::ids::LoginName::default_login())
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), cleared);
    editor.use_account(&kind, &work).unwrap();
    editor
        .remove_named_account(&kind, &"personal".parse().unwrap())
        .unwrap();
    assert_eq!(
        editor.get(Some("accounts.use.codex")).unwrap().as_str(),
        Some("work")
    );
    editor
        .upsert_named_account(&kind, &"personal".parse().unwrap(), None, None, None)
        .unwrap();
    editor.remove_named_account(&kind, &work).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("[accounts.use]"), "{text}");
    assert!(
        text.contains("# machine")
            && text.contains("# cap")
            && text.contains("[accounts.codex.personal]"),
        "{text}"
    );
}

#[test]
fn machine_selection_numeric_and_boolean_names_stay_strings() {
    let dir = tempfile::tempdir().unwrap();
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        dir.path().join("agents"),
    ));
    let kind = crate::ids::AgentKind::new_unchecked("codex");
    for name in ["123", "true"] {
        editor.use_account(&kind, &name.parse().unwrap()).unwrap();
        assert_eq!(
            editor.get(Some("accounts.use.codex")).unwrap().as_str(),
            Some(name)
        );
    }
}

#[test]
fn account_tables_land_beside_the_templated_accounts_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        &path,
        dir.path().join("agents-home"),
    ));
    let codex = crate::ids::AgentKind::new_unchecked("codex");
    let rimio = "rimio".parse::<crate::ids::LoginName>().unwrap();

    editor
        .upsert_named_account(&codex, &rimio, None, None, None)
        .unwrap();
    editor.use_account(&codex, &rimio).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}: {text}"))
    };
    assert!(
        at("\n[accounts.usage_limit_usd]\n# Display-only") < at("## [accounts.use]"),
        "the section's own comments stay under its header: {text}"
    );
    assert!(
        at("## [accounts.use]") < at("\n[accounts.codex.rimio]\n"),
        "{text}"
    );
    assert!(
        at("\n[accounts.codex.rimio]\n") < at("\n[accounts.use]\ncodex = \"rimio\"\n"),
        "{text}"
    );
    assert!(
        at("codex = \"rimio\"\n\n[notifications]\n# enabled") > at("\n[accounts.use]\n"),
        "{text}"
    );
    assert_eq!(
        MachineConfig::load_from(&path, editor.files.agents_home())
            .unwrap()
            .accounts
            .use_accounts
            .get(&codex)
            .map(|name| name.as_str().to_owned()),
        Some("rimio".to_owned())
    );
}
