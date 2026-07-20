use super::*;

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
        ["config.toml", "theme.toml", "agents.toml", "loop.toml"].map(std::ffi::OsString::from)
    );
    assert_eq!(ordered[0].template(), MachineConfig::template_core());
    assert_eq!(ordered[1].template(), MachineConfig::template_theme());
    assert_eq!(ordered[2].template(), MachineConfig::template_agents());
    assert_eq!(ordered[3].template(), MachineConfig::template_loop());
}

const LEGACY_SET_KEYS: &[&str] = &[
    "agents.worktree.dir",
    "agents.worktree.base",
    "agents.placement",
    "harness.smart_compact",
    "harness.budget",
    "harness.rtk",
    "timezone",
    "resume.on_rebirth",
    "resume.max",
    "resume.auto_continue",
    "resume.auto_continue_backoff_secs",
    "resume.auto_continue_max_retries",
    "resume.auto_continue_text",
    "resume.auto_redeem",
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
    "sidebar.spend_window",
    "sidebar.afk_after_secs",
    "agents.attention.stalled_after_secs",
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
        "agents.teams.review.roles",
        "agents.teams.review.layout",
        "agents.commands.vim",
        "agents.profiles.codex-slim.agent",
        "agents.profiles.codex-slim.mode",
        "agents.profiles.codex-slim.model",
        "agents.profiles.codex-slim.effort",
        "agents.profiles.codex-slim.args",
        "agents.profiles.codex-slim.system-prompt-file",
        "loop.tasks.watch.agent",
        "loop.auto-ping",
        "loop.default-timeout",
        "loop.tasks.watch.prompt",
        "loop.tasks.watch.check",
        "loop.tasks.watch.on",
        "loop.tasks.watch.deadline",
        "loop.tasks.watch.wake.kind",
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
        "resume.auto_redeem_min_gain",
        "notifications.title",
        "notifications.body",
        "harness.smart_compact",
        "harness.budget",
        "harness.rtk",
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
fn set_top_level_timezone_keeps_core_config_valid() {
    let mut doc = MachineConfig::template_core()
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
        std::path::Path::new("config.toml"),
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
    let mut doc = MachineConfig::template_theme()
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
        std::path::Path::new("config.toml"),
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
    let mut doc = MachineConfig::template_core()
        .parse::<DocumentMut>()
        .expect("template parses");
    let key = parse_key("remote_control.claude").expect("key");
    let err = apply_logical_key(
        &mut doc,
        std::path::Path::new("config.toml"),
        &key,
        parse_set_value(&key, "flase"),
        std::path::Path::new("missing-agents-home"),
        std::path::Path::new("config.toml"),
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
    let mut doc = MachineConfig::template_core()
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
        std::path::Path::new("config.toml"),
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
        r#"{ pr_watch = { agent = "codex", prompt = "check CI", root = "/r", every = "15m" }, self_wake = { wake = { kind = "claude", session = "s1", handle = "@planner" }, prompt = "resume", root = "/r", at = "09:30" } }"#,
    );

    set_document_value(&mut doc, &path, value).expect("set tasks");

    let rendered = doc.to_string();
    assert!(
        rendered.contains("[tasks.pr_watch]"),
        "scalar-only task should render as a table block:\n{rendered}"
    );
    let task = rendered
        .find("[tasks.self_wake]")
        .unwrap_or_else(|| panic!("task should render as a table block:\n{rendered}"));
    let wake = rendered
        .find("[tasks.self_wake.wake]")
        .unwrap_or_else(|| panic!("wake should render as a nested table block:\n{rendered}"));
    assert!(
        task < wake,
        "task table should render before wake table:\n{rendered}"
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

    for key in ["nope", "theme.nope", "agents.profiles"] {
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
}

#[test]
fn harness_rtk_values_are_parsed_as_strings() {
    let key = parse_key("harness.rtk").expect("key");

    assert_eq!(parse_set_value(&key, "auto").as_str(), Some("auto"));
    assert_eq!(parse_set_value(&key, "on").as_str(), Some("on"));
    assert_eq!(parse_set_value(&key, "off").as_str(), Some("off"));
}

#[test]
fn harness_smart_compact_validation_rejects_bad_values() {
    let key = parse_key("harness.smart_compact").expect("key");

    validate_set_value(&key, &Value::from("70%")).expect("percent threshold");
    validate_set_value(&key, &Value::from("120000")).expect("token threshold");
    validate_set_value(&key, &Value::from("180k")).expect("k suffix");

    let err = validate_set_value(&key, &Value::from("abc"))
        .expect_err("invalid smart-compact threshold")
        .to_string();
    assert!(
        err.contains("invalid auto-compact threshold `abc`"),
        "unexpected error: {err}"
    );
}

#[test]
fn harness_rtk_validation_rejects_bad_values() {
    let key = parse_key("harness.rtk").expect("key");

    for mode in ["auto", "on", "off"] {
        validate_set_value(&key, &Value::from(mode)).expect("rtk mode");
    }

    let err = validate_set_value(&key, &Value::from("always"))
        .expect_err("invalid rtk mode")
        .to_string();
    assert_eq!(err, "harness.rtk must be one of auto, on, or off");
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
