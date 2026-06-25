use std::path::{Path, PathBuf};

use super::*;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under the workspace root")
        .to_path_buf()
}

#[test]
fn presence_wasm_magic_predicate_accepts_wasm_header() {
    assert!(crate::build::is_wasm_module(b"\0asm\x01\0\0\0"));
    assert!(!crate::build::is_wasm_module(b"not-wasm"));
}

#[test]
fn vendored_presence_plugin_is_fresh_for_this_tree() {
    ensure_presence_plugin_vendored(&repo_root()).unwrap();
}

#[test]
fn spend_parser_path_predicate_covers_nested_modules() {
    let agents_root = Path::new("/repo/crates/rimz/src/agents");

    assert!(is_agent_spend_parser_path(
        &agents_root.join("codex/spend.rs"),
        agents_root,
    ));
    assert!(is_agent_spend_parser_path(
        &agents_root.join("codex/spend/wire.rs"),
        agents_root,
    ));
    assert!(is_agent_spend_parser_path(
        &agents_root.join("transcript_fs.rs"),
        agents_root,
    ));
    assert!(!is_agent_spend_parser_path(
        &agents_root.join("codex/mod.rs"),
        agents_root,
    ));
    assert!(!is_agent_spend_parser_path(
        Path::new("/repo/crates/rimz/src/sidebar/spend/wire.rs"),
        agents_root,
    ));
}

#[test]
fn tests_path_component_matches_nested_test_trees() {
    assert!(path_has_tests_component(Path::new("tests/mod.rs")));
    assert!(path_has_tests_component(Path::new(
        "labels/tests/glyphs.rs"
    )));
    assert!(!path_has_tests_component(Path::new(
        "labels/contest/glyphs.rs"
    )));
}

#[test]
fn ui_color_exemptions_cover_the_pipeline_and_tests() {
    // The color pipeline owns the named-ANSI vocabulary.
    assert!(ui_color_exempt(Path::new("theme.rs")));
    assert!(ui_color_exempt(Path::new("theme/component.rs")));
    assert!(ui_color_exempt(Path::new("theme/semantic.rs")));
    assert!(ui_color_exempt(Path::new("ansi.rs")));
    assert!(ui_color_exempt(Path::new("scheme.rs")));
    assert!(ui_color_exempt(Path::new("oklab.rs")));
    // Tests assert carrier→slot mappings, inline or in a tests tree.
    assert!(ui_color_exempt(Path::new("labels/tests/meters.rs")));
    assert!(ui_color_exempt(Path::new("tests/process.rs")));
    // Ordinary render code must name intent.
    assert!(!ui_color_exempt(Path::new("effects.rs")));
    assert!(!ui_color_exempt(Path::new("labels/meters.rs")));
    assert!(!ui_color_exempt(Path::new("sections/agent_card/gauge.rs")));
}

#[test]
fn ui_glyph_exemptions_cover_glyph_tables_animation_and_tests() {
    assert!(ui_glyph_exempt(Path::new("theme/glyphs.rs")));
    assert!(ui_glyph_exempt(Path::new("animation.rs")));
    assert!(ui_glyph_exempt(Path::new("labels/tests/meters.rs")));
    assert!(ui_glyph_exempt(Path::new("tests/process.rs")));
    assert!(!ui_glyph_exempt(Path::new("labels/meters.rs")));
    assert!(!ui_glyph_exempt(Path::new("sections/provider.rs")));
}

#[test]
fn ui_glyph_violations_flag_literals_but_skip_comments_and_tests() {
    assert_eq!(ui_glyph_violation_lines("let glyph = \"◇\";\n").len(), 1);
    assert_eq!(ui_glyph_violation_lines("let glyph = \"⋯ bg\";\n").len(), 1);
    assert!(ui_glyph_violation_lines("// `◇` in docs\n").is_empty());
    assert!(
        ui_glyph_violation_lines("mod tests {\nlet glyph = \"◇\";\n}\n").is_empty(),
        "inline tests may assert rendered shapes"
    );
}

#[test]
fn ui_color_violations_flag_color_variants_but_allow_the_reset_sentinel() {
    // A named ANSI hue is intent — flagged.
    let named = format!(
        "    theme.style({}, Modifier::empty());",
        concat!("Color", "::", "Yellow")
    );
    assert_eq!(ui_color_violation_lines(&named).len(), 1);

    // A hand-picked Indexed/Rgb literal bypasses the pipeline — flagged too, so
    // a `const HOT: Color = Color::Indexed(201)` cannot slip past the gate.
    let indexed = concat!("    const HOT: Color = Color", "::", "Indexed(201);");
    assert_eq!(ui_color_violation_lines(indexed).len(), 1);
    let rgb = concat!("    let c = Color", "::", "Rgb(1, 2, 3);");
    assert_eq!(ui_color_violation_lines(rgb).len(), 1);

    // `Color::Reset` is the one allowed path; a config `ThemeColor` is a
    // different type the boundary guard must not catch.
    let allowed = concat!(
        "fx.filter(Color",
        "::",
        "Reset);\n",
        "let g = ThemeColor",
        "::",
        "Indexed(34);\n",
    );
    assert!(ui_color_violation_lines(allowed).is_empty());

    // An inline test module legitimately asserts carrier→slot mappings.
    let in_tests = concat!(
        "fn prod() {}\n",
        "mod tests {\n",
        "    let c = Color",
        "::",
        "Red;\n",
        "}\n",
    );
    assert!(ui_color_violation_lines(in_tests).is_empty());
}
