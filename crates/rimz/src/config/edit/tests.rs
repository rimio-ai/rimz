use super::*;
use crate::config::{
    AGENT_FRAGMENT_FILE, AGENTS_HOME_PROFILES_SUBDIR, AGENTS_HOME_TEAMS_SUBDIR,
    MachineConfigFileKind as Kind, TEAM_FRAGMENT_FILE,
};

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
    assert_eq!(ordered[0].template(), Kind::Core.template());
    assert_eq!(ordered[1].template(), Kind::Theme.template());
    assert_eq!(ordered[2].template(), Kind::Agents.template());
    assert_eq!(ordered[3].template(), MachineConfig::template_loop());
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
    "agents.placement",
    "harness.smart_compact",
    "harness.idle_compact",
    "harness.idle_compact_after",
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
        "agents.teams.review.roles",
        "agents.teams.review.layout",
        "agents.commands.vim",
        "agents.profiles.codex-slim.agent",
        "agents.profiles.codex-slim.description",
        "agents.profiles.codex-slim.mode",
        "agents.profiles.codex-slim.model",
        "agents.profiles.codex-slim.effort",
        "agents.profiles.codex-slim.args",
        "agents.profiles.codex-slim.system-prompt-file",
        "subagents.profiles.codex-review.agent",
        "subagents.profiles.codex-review.description",
        "subagents.profiles.codex-review.model",
        "loop.tasks.watch.agent",
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
        "harness.idle_compact",
        "harness.idle_compact_after",
        "harness.budget",
        "harness.turn_budget",
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
        ("agents.profiles.demo.agent", true),
        ("agents.profiles.demo.bogus", false),
        ("subagents.profiles.demo.effort", true),
        ("subagents.profiles.demo.bogus", false),
        ("agents.teams.demo.layout", true),
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
fn dynamic_profile_and_team_field_lists_match_serialized_schema() {
    use crate::config::{Profile, RoleBinding, Team};
    use crate::harness::run::PermissionMode;

    let profile = Profile {
        agent: "claude".to_owned(),
        description: Some("test".to_owned()),
        mode: Some(PermissionMode::Yolo),
        model: Some("model".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$1".to_owned()),
        system_prompt_file: Some(PathBuf::from("system.md")),
        append_system_prompt_files: vec![PathBuf::from("append.md")],
        args: Some("--flag".to_owned()),
    };
    let team = Team {
        roles: vec![RoleBinding {
            role: "lead".to_owned(),
            profile: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        }],
        leader: Some("lead".to_owned()),
        layout: Some("lead".to_owned()),
        scratch_files: vec!["notes/".to_owned()],
    };

    let profile_keys: std::collections::BTreeSet<_> = toml::Value::try_from(profile)
        .expect("serialize profile")
        .as_table()
        .expect("profile table")
        .keys()
        .cloned()
        .collect();
    let team_keys: std::collections::BTreeSet<_> = toml::Value::try_from(team)
        .expect("serialize team")
        .as_table()
        .expect("team table")
        .keys()
        .cloned()
        .collect();

    assert_eq!(
        profile_keys,
        PROFILE_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect(),
        "PROFILE_FIELDS drifted from Profile serialization"
    );
    assert_eq!(
        team_keys,
        TEAM_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect(),
        "TEAM_FIELDS drifted from Team serialization"
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
fn merge_skips_keys_inside_commented_table_examples() {
    let mut doc = Kind::Agents
        .template()
        .parse::<DocumentMut>()
        .expect("template parses");
    let mut skipped = Vec::new();
    let kept = apply_merge_keys(
        std::path::Path::new("agents.toml"),
        &mut doc,
        vec![PendingKey {
            logical: parse_key("agents.teams.peer.leader").expect("key"),
            value: Value::from("codex"),
        }],
        &mut skipped,
        std::path::Path::new("missing-agents-home"),
        std::path::Path::new("config.toml"),
    );

    assert_eq!(kept, 1);
    assert!(skipped.is_empty());
    let rendered = doc.to_string();
    let peer = rendered.find("[agents.teams.peer]").expect("peer team");
    let leader = rendered.find("leader = \"codex\"").expect("peer leader");
    let review = rendered
        .find("## [agents.teams.review]")
        .expect("review example");
    assert!(peer < leader && leader < review, "{rendered}");
    assert!(
        rendered.contains("## [agents.teams.review]\n## leader = \"planner\""),
        "{rendered}"
    );
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
fn repair_agents_home_removes_unknown_keys_preserves_comments_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agents_home = dir.path().join("agents-home");
    let profile_dir = agents_home
        .join(AGENTS_HOME_PROFILES_SUBDIR)
        .join("planner");
    std::fs::create_dir_all(&profile_dir).expect("profile dir");
    let profile_path = profile_dir.join(AGENT_FRAGMENT_FILE);
    std::fs::write(
        &profile_path,
        "# keep this comment\n[agents.profiles.planner]\nagent = \"claude\"\nfuture = true\n\
         [subagents.profiles.planner-child]\nagent = \"claude\"\neffort = \"high\"\n",
    )
    .expect("profile fragment");
    let team_dir = agents_home.join(AGENTS_HOME_TEAMS_SUBDIR).join("forge");
    std::fs::create_dir_all(&team_dir).expect("team dir");
    let team_path = team_dir.join(TEAM_FRAGMENT_FILE);
    std::fs::write(
        &team_path,
        "[agents.teams.forge]\nfuture = true\n\
         [[agents.teams.forge.roles]]\nrole = \"planner\"\nprofile = \"planner\"\nextra = 1\n",
    )
    .expect("team fragment");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        &agents_home,
    ));

    let first = editor.repair_agents_home().expect("repair");
    let profile = std::fs::read_to_string(&profile_path).expect("read profile");
    let team = std::fs::read_to_string(&team_path).expect("read team");
    let second = editor.repair_agents_home().expect("repair again");

    assert_eq!(first.files.len(), 2, "{first:?}");
    assert!(profile.contains("# keep this comment"), "{profile}");
    assert!(
        profile.contains("[subagents.profiles.planner-child]"),
        "{profile}"
    );
    assert!(profile.contains("effort = \"high\""), "{profile}");
    assert!(!profile.contains("future = true"), "{profile}");
    assert!(!team.contains("future = true"), "{team}");
    assert!(!team.contains("extra = 1"), "{team}");
    assert!(second.files.is_empty(), "{second:?}");
}

#[test]
fn repair_agents_home_leaves_unparseable_fragments_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agents_home = dir.path().join("agents-home");
    let profile_dir = agents_home.join(AGENTS_HOME_PROFILES_SUBDIR).join("broken");
    std::fs::create_dir_all(&profile_dir).expect("profile dir");
    let path = profile_dir.join(AGENT_FRAGMENT_FILE);
    std::fs::write(&path, "not = = toml").expect("broken fragment");
    let editor = ConfigEditor::new(MachineConfigFiles::from_paths(
        dir.path().join("config.toml"),
        &agents_home,
    ));

    editor
        .merge_defaults()
        .expect("broken fragment does not block machine-file refresh");
    let report = editor.repair_agents_home().expect("repair");

    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, path);
    assert!(report.files[0].removed.is_empty());
    assert!(report.files[0].error.is_some());
    assert_eq!(
        std::fs::read_to_string(&report.files[0].path).expect("unchanged"),
        "not = = toml"
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
