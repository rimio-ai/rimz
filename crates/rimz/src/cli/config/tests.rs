use super::*;
use std::collections::BTreeMap;
use std::num::NonZeroU16;

use rimz::config::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationSpec, AnimationSpeed,
    BudgetBarConfig, BudgetBurnRateConfig, CardDensityMode, ContextBand, ContextMeterConfig,
    DisplayConfig, GlowMode, GlyphGroup, GlyphNamespaces, InlineAnsiColors, InlinePalette,
    InlinePrimaryColors, InlineSelectionColors, PaletteRole, ProviderTabsMode, ScrollbarMode,
    ThemeAnimationsConfig, ThemeColor, ThemeConfig, ThemeGlyphsConfig, ThemeMode,
    ThemeProviderStyle, ThemeStyle, UnreadEffect,
};

#[test]
fn validates_config_key_read_and_write_surfaces() {
    for key in [
        "theme.display.max_cols",
        "theme.display.budget_bar.burn_rate.red",
        "accounts.usage_limit_usd.codex",
        "agents.teams.review.roles",
        "agents.teams.review.layout",
        "agents.commands.vim",
        "agents.profiles.codex-slim.agent",
        "agents.profiles.codex-slim.mode",
        "agents.profiles.codex-slim.model",
        "agents.profiles.codex-slim.effort",
        "agents.profiles.codex-slim.args",
        "agents.profiles.codex-slim.system-prompt-file",
        "zellij.auto_layout",
        "theme.providers.claude.color",
        "agents.pets.enabled",
        "agents.pets.pet",
        "agents.pets.size",
        "agents.pets.glyphs",
        "agents.pets.voice",
        "theme.mode",
        "theme.scheme",
        "theme.caution",
        "sidebar.focus_key",
        "sidebar.spend_window",
        "sidebar.spend_timezone",
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
        "resume.auto_continue_text",
        "harness.smart_compact",
    ] {
        validate_set_key(&parse_key(key).unwrap()).unwrap_or_else(|err| panic!("{key}: {err}"));
    }

    for key in [
        "sidebar.nope",
        "accounts.nope",
        "accounts.usage_limit_usd",
        "accounts.usage_limit_usd.codex.extra",
        "agents.teams.peer.shape",
        "agents.profiles.codex-slim.flags",
        "agents.commands.vim.command",
        "theme.providers.claude.nope",
        "theme.animations",
        "theme.animations.nope.frames",
        "theme.animations.thinking.nope",
        "theme.animations.thinking.frames.extra",
        "theme.glyphs.nope",
        "theme.glyphs.unicode.tokens.nope",
        "theme.glyphs.unicode.tokens.total.extra",
    ] {
        assert!(validate_set_key(&parse_key(key).unwrap()).is_err(), "{key}");
    }

    for (key, known) in [
        ("theme.animations", true),
        ("theme.animations.thinking", true),
        ("theme.animations.thinking.frames", true),
        ("theme.animations.unread", true),
        ("theme.animations.nope", false),
        ("theme.glyphs", true),
        ("theme.glyphs.unicode.tokens", true),
        ("theme.glyphs.unicode.keys", true),
        ("theme.glyphs.unicode.tokens.total", true),
        ("theme.glyphs.unicode.keys.focus", true),
        ("theme.glyphs.unicode.tokens.nope", false),
        ("accounts", true),
        ("accounts.usage_limit_usd", true),
        ("accounts.usage_limit_usd.codex", true),
    ] {
        assert_eq!(is_known_get_key(&parse_key(key).unwrap()), known, "{key}");
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
    let found = collect_explicit_keys(FileKind::Theme, &doc);
    let mut saw_background = false;
    let mut saw_unknown = false;
    for item in found {
        match item {
            Found::Settable { logical, value } if logical == expected_background => {
                assert_eq!(value.as_str(), Some("#000000"));
                saw_background = true;
            }
            Found::Unknown(key) if key == "colors.primary.nope" => {
                saw_unknown = true;
            }
            other => panic!("unexpected key: {other:?}"),
        }
    }
    assert!(saw_background, "background override should be settable");
    assert!(saw_unknown, "unknown color leaf should be reported");
}

#[test]
fn merge_key_oracle_accepts_sentry_and_rejects_bogus_keys() {
    assert!(is_known_merge_key(&parse_key("sentry.dsn").expect("key")));
    assert!(is_known_merge_key(
        &parse_key("sentry.environment").expect("key")
    ));
    assert!(is_known_merge_key(
        &parse_key("notifications.enabled").expect("key")
    ));
    assert!(!is_known_merge_key(
        &parse_key("notifications.nope").expect("key")
    ));
}

#[test]
fn set_keys_cover_serialized_default_leaves() {
    let root = config_value(&MachineConfig::default()).expect("default config serializes");
    let mut leaves = BTreeSet::new();
    collect_leaf_paths("", &root, &mut leaves);
    let set_keys = exact_set_keys();

    for leaf in leaves {
        assert!(
            set_key_reaches_leaf(&set_keys, &leaf),
            "serialized default leaf `{leaf}` is not reachable by config set"
        );
    }
}

#[test]
fn set_keys_cover_fully_populated_theme_leaves() {
    // Dynamic provider names and glyph roles stay covered by
    // `validates_config_key_read_and_write_surfaces`; this sentinel proves
    // every serialized theme leaf has some `config set` path.
    let root = config_value(&MachineConfig {
        theme: fully_populated_theme(),
        ..MachineConfig::default()
    })
    .expect("populated theme config serializes");
    let mut leaves = BTreeSet::new();
    collect_leaf_paths("", &root, &mut leaves);
    let set_keys = exact_set_keys();

    for leaf in leaves {
        assert!(
            set_key_reaches_leaf(&set_keys, &leaf),
            "populated theme leaf `{leaf}` is not reachable by config set"
        );
    }
}

fn fully_populated_theme() -> ThemeConfig {
    ThemeConfig {
        style: Some(ThemeStyle::Modern),
        mode: ThemeMode::Truecolor,
        scheme: Some("TokyoNight Night".to_owned()),
        colors: Some(inline_palette_fixture()),
        good: Some(ThemeColor::Role(PaletteRole::Green)),
        warn: Some(ThemeColor::Role(PaletteRole::Yellow)),
        caution: Some(ThemeColor::Indexed(214)),
        alarm: Some(ThemeColor::Role(PaletteRole::Red)),
        accent: Some(ThemeColor::Rgb(0x7d, 0xcf, 0xff)),
        cool: Some(ThemeColor::Role(PaletteRole::Blue)),
        meta: Some(ThemeColor::Role(PaletteRole::Magenta)),
        body: Some(ThemeColor::Role(PaletteRole::Foreground)),
        muted: Some(ThemeColor::Indexed(245)),
        faint: Some(ThemeColor::Indexed(240)),
        rule: Some(ThemeColor::Rgb(0x43, 0x46, 0x59)),
        selection: Some(ThemeColor::Rgb(0xab, 0xc4, 0xff)),
        selection_bg: Some(ThemeColor::Rgb(0x1d, 0x20, 0x30)),
        display: DisplayConfig {
            refresh_ms: 200,
            max_provider_blocks: 4,
            provider_tabs: ProviderTabsMode::Always,
            provider_list: vec!["claude".to_owned(), "codex".to_owned()],
            max_cols: NonZeroU16::new(80).expect("non-zero literal"),
            scrollbar: ScrollbarMode::Always,
            glow: GlowMode::Never,
            card_density: CardDensityMode::Compact,
            context_meter: ContextMeterConfig {
                green: context_band(35, 90_000),
                yellow: context_band(55, 140_000),
                amber: context_band(75, 220_000),
                red: context_band(92, 400_000),
            },
            budget_bar: BudgetBarConfig {
                yellow: 60,
                amber: 30,
                red: 12,
                burn_rate: BudgetBurnRateConfig {
                    yellow: 110,
                    amber: 160,
                    red: 220,
                },
            },
        },
        glyphs: ThemeGlyphsConfig {
            set: Some("unicode".to_owned()),
            unicode: glyph_namespaces_fixture(),
            nerd_font: glyph_namespaces_fixture(),
        },
        providers: BTreeMap::from([(
            "claude".to_owned(),
            ThemeProviderStyle {
                product_name: Some("Claude".to_owned()),
                ascii_art: Some("C".to_owned()),
                color: Some(ThemeColor::Rgb(0xd9, 0x77, 0x57)),
            },
        )]),
        animations: ThemeAnimationsConfig {
            unread: Some(UnreadEffect::Blink),
            thinking: anim(
                "ab",
                AnimationColor::Accent,
                AnimationEffect::Breathe,
                AnimationSpeed::Fast,
            ),
            working: anim(
                "cd",
                AnimationColor::Good,
                AnimationEffect::Static,
                AnimationSpeed::Normal,
            ),
            compacting: anim(
                "ef",
                AnimationColor::Warn,
                AnimationEffect::Breathe,
                AnimationSpeed::Slow,
            ),
            delegating: anim(
                "gh",
                AnimationColor::Caution,
                AnimationEffect::Breathe,
                AnimationSpeed::Fast,
            ),
            resolving: anim(
                "ij",
                AnimationColor::Cool,
                AnimationEffect::Static,
                AnimationSpeed::Normal,
            ),
            idle: anim(
                "kl",
                AnimationColor::Muted,
                AnimationEffect::Static,
                AnimationSpeed::Slow,
            ),
            success: anim(
                "mn",
                AnimationColor::Good,
                AnimationEffect::Breathe,
                AnimationSpeed::Fast,
            ),
            paused: anim(
                "op",
                AnimationColor::Meta,
                AnimationEffect::Static,
                AnimationSpeed::Normal,
            ),
            waiting: anim(
                "qr",
                AnimationColor::Faint,
                AnimationEffect::Breathe,
                AnimationSpeed::Slow,
            ),
            failed: anim(
                "st",
                AnimationColor::Alarm,
                AnimationEffect::Static,
                AnimationSpeed::Fast,
            ),
        },
    }
}

fn inline_palette_fixture() -> InlinePalette {
    InlinePalette {
        primary: Some(InlinePrimaryColors {
            background: Some("#101010".to_owned()),
            foreground: Some("#f0f0f0".to_owned()),
        }),
        normal: Some(ansi_colors([
            "#000000", "#aa0000", "#00aa00", "#aa5500", "#0000aa", "#aa00aa", "#00aaaa", "#aaaaaa",
        ])),
        bright: Some(ansi_colors([
            "#555555", "#ff5555", "#55ff55", "#ffff55", "#5555ff", "#ff55ff", "#55ffff", "#ffffff",
        ])),
        selection: Some(InlineSelectionColors {
            background: Some("#223344".to_owned()),
            text: Some("#ddeeff".to_owned()),
        }),
    }
}

fn ansi_colors(
    [black, red, green, yellow, blue, magenta, cyan, white]: [&str; 8],
) -> InlineAnsiColors {
    InlineAnsiColors {
        black: Some(black.to_owned()),
        red: Some(red.to_owned()),
        green: Some(green.to_owned()),
        yellow: Some(yellow.to_owned()),
        blue: Some(blue.to_owned()),
        magenta: Some(magenta.to_owned()),
        cyan: Some(cyan.to_owned()),
        white: Some(white.to_owned()),
    }
}

fn context_band(percent: u8, tokens: u64) -> ContextBand {
    ContextBand { percent, tokens }
}

fn anim(
    frames: &str,
    color: AnimationColor,
    effect: AnimationEffect,
    speed: AnimationSpeed,
) -> Option<AnimationSpec> {
    Some(animation_spec(frames, color, effect, speed))
}

fn animation_spec(
    frames: &str,
    color: AnimationColor,
    effect: AnimationEffect,
    speed: AnimationSpeed,
) -> AnimationSpec {
    AnimationSpec {
        frames: Some(animation_frames(frames)),
        color: Some(color),
        effect: Some(effect),
        speed: Some(speed),
    }
}

fn animation_frames(frames: &str) -> AnimationFrames {
    let spec: AnimationSpec =
        toml::from_str(&format!("frames = {frames:?}\n")).expect("valid animation frames");
    spec.frames.expect("frames")
}

fn glyph_namespaces_fixture() -> GlyphNamespaces {
    GlyphNamespaces {
        status: glyph_group("status", "waiting", "?"),
        cockpit: glyph_group("cockpit", "workspace", "w"),
        tokens: glyph_group("tokens", "total", "t"),
        meter: glyph_group("meter", "context_full", "f"),
        clock: glyph_group("clock", "q1", "1"),
        worktree: glyph_group("worktree", "branch", "b"),
        card: glyph_group("card", "subagents", "s"),
        process: glyph_group("process", "cpu", "c"),
        keys: glyph_group("keys", "focus", "k"),
        chrome: glyph_group("chrome", "box_vertical", "|"),
    }
}

fn glyph_group(namespace: &str, role: &str, glyph: &str) -> GlyphGroup {
    let namespaces: GlyphNamespaces =
        toml::from_str(&format!("[{namespace}]\n{role} = {glyph:?}\n"))
            .expect("valid glyph namespace");
    match namespace {
        "status" => namespaces.status,
        "cockpit" => namespaces.cockpit,
        "tokens" => namespaces.tokens,
        "meter" => namespaces.meter,
        "clock" => namespaces.clock,
        "worktree" => namespaces.worktree,
        "card" => namespaces.card,
        "process" => namespaces.process,
        "keys" => namespaces.keys,
        "chrome" => namespaces.chrome,
        _ => panic!("unknown glyph namespace `{namespace}`"),
    }
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
}

#[test]
fn harness_smart_compact_validation_rejects_bad_values() {
    let key = parse_key("harness.smart_compact").expect("key");

    validate_set_value(&key, &Value::from("70%")).expect("percent threshold");
    validate_set_value(&key, &Value::from("120000")).expect("token threshold");

    let err = validate_set_value(&key, &Value::from("abc"))
        .expect_err("invalid smart-compact threshold")
        .to_string();
    assert!(
        err.contains("invalid auto-compact threshold `abc`"),
        "unexpected error: {err}"
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

fn set_key_reaches_leaf(set_keys: &BTreeSet<String>, leaf: &str) -> bool {
    set_keys.contains(leaf)
        || set_keys.iter().any(|key| {
            leaf.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('.'))
        })
        || parse_key(leaf)
            .ok()
            .is_some_and(|key| validate_set_key(&key).is_ok())
}

fn collect_leaf_paths(prefix: &str, value: &toml::Value, out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let next = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_leaf_paths(&next, value, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned());
        }
    }
}
