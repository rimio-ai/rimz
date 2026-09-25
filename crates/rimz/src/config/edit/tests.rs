use super::*;
use crate::config::MachineConfigFileKind as Kind;

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
    "harness.compact_instruction",
    "harness.idle_compact",
    "harness.idle_compact_after",
    "harness.budget",
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
        "loop.tasks.watch.agent",
        "loop.default-timeout",
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
        "resume.auto_redeem_min_gain",
        "notifications.title",
        "notifications.body",
        "harness.smart_compact",
        "harness.compact_instruction",
        "harness.idle_compact",
        "harness.idle_compact_after",
        "harness.budget",
        "harness.turn_budget",
        "gc.auto",
        "gc.older_than",
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
        std::path::Path::new("config.toml"),
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
        std::path::Path::new("config.toml"),
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
        std::path::Path::new("config.toml"),
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
        std::path::Path::new("config.toml"),
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
        std::path::Path::new("config.toml"),
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
        .set("agents.isolation", "sandbox")
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
    assert_eq!(parsed["agents"]["isolation"].as_str(), Some("sandbox"));
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

    assert_eq!(first_kept, 3);
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
fn harness_idle_compact_values_are_parsed_as_strings() {
    let mode = parse_key("harness.idle_compact").expect("mode key");
    let after = parse_key("harness.idle_compact_after").expect("duration key");

    assert_eq!(parse_set_value(&mode, "auto").as_str(), Some("auto"));
    assert_eq!(parse_set_value(&after, "59m").as_str(), Some("59m"));
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

    let err = validate_set_value(&key, &Value::from("abc"))
        .expect_err("invalid smart-compact threshold")
        .to_string();
    assert!(
        err.contains("invalid auto-compact threshold `abc`"),
        "unexpected error: {err}"
    );
}

#[test]
fn harness_idle_compact_validation_accepts_modes_and_duration() {
    let mode = parse_key("harness.idle_compact").expect("mode key");
    for value in ["off", "auto", "always"] {
        validate_set_value(&mode, &Value::from(value)).expect("idle compact mode");
    }
    let err = validate_set_value(&mode, &Value::from("sometimes"))
        .expect_err("invalid idle compact mode")
        .to_string();
    assert_eq!(
        err,
        "harness.idle_compact must be one of off, auto, or always"
    );

    let after = parse_key("harness.idle_compact_after").expect("duration key");
    validate_set_value(&after, &Value::from("59m")).expect("idle compact duration");
    let err = validate_set_value(&after, &Value::from("soon"))
        .expect_err("invalid idle compact duration")
        .to_string();
    assert!(err.contains("use a duration such as 59m or 2h"), "{err}");
}

#[test]
fn gc_older_than_is_a_validated_duration_string() {
    let key = parse_key("gc.older_than").expect("key");

    let value = parse_set_value(&key, "3d");
    assert_eq!(value.as_str(), Some("3d"));
    validate_set_value(&key, &value).expect("day span");
    for bad in ["0d", "soon"] {
        let err = validate_set_value(&key, &Value::from(bad))
            .expect_err("invalid gc span")
            .to_string();
        assert!(err.contains("use a duration such as 8h or 3d"), "{err}");
    }
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
        .upsert_named_account(&claude, &work, Some(Path::new("/srv/homes/work")))
        .expect("declare work");
    editor
        .upsert_named_account(&claude, &personal, None)
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
    assert!(!text.contains("[accounts.claude]"), "{text}");
    assert!(
        text.find("[notifications]") < text.find("[accounts.claude.work]"),
        "a declared account must not split a table from its heading comments: {text}"
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
