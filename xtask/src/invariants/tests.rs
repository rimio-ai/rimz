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
fn package_include_list_covers_build_inputs() {
    let manifest = r#"
[package]
include = [
    "/presence/rimz-presence-zellij.wasm",
    "/pricing/litellm-pricing.json",
]
"#;

    ensure_include_covers_build_inputs(manifest).unwrap();
}

#[test]
fn package_include_list_flags_missing_presence_wasm() {
    let manifest = r#"
[package]
include = [
    "/pricing/litellm-pricing.json",
]
"#;

    let err = ensure_include_covers_build_inputs(manifest).unwrap_err();
    assert!(
        err.to_string()
            .contains("/presence/rimz-presence-zellij.wasm")
    );
}

#[test]
fn package_include_list_flags_missing_pricing_snapshot() {
    let manifest = r#"
[package]
include = [
    "/presence/rimz-presence-zellij.wasm",
]
"#;

    let err = ensure_include_covers_build_inputs(manifest).unwrap_err();
    assert!(err.to_string().contains("/pricing/litellm-pricing.json"));
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
fn sidebar_event_log_reads_must_route_through_rollup() {
    let root = temp_repo_root("sidebar-event-log-boundary");
    let bad = root.join("crates/rimz/src/sidebar/enrich/bad.rs");
    let renderer_bad = root.join("crates/rimz/src/sidebar_pane/app/bad.rs");
    let test_file = root.join("crates/rimz/src/sidebar/enrich/tests.rs");
    for path in [&bad, &renderer_bad, &test_file] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    let direct_read = concat!(
        "fn f() { crate::ledger::event_log",
        "::",
        "read_all(path); }\n"
    );
    let offset_read = concat!("fn f() { event_log", "::", "read_from_offset(path, 0); }\n");
    std::fs::write(&bad, direct_read).expect("write bad source");
    std::fs::write(&renderer_bad, offset_read).expect("write bad renderer source");
    std::fs::write(&test_file, direct_read).expect("write test source");

    let err = ensure_sidebar_event_log_reads_through_rollup(
        &root,
        &[bad.clone(), renderer_bad.clone(), test_file.clone()],
    )
    .unwrap_err();
    assert!(err.to_string().contains("fold through RollupCursor"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    assert!(!err.to_string().contains(&test_file.display().to_string()));

    let err =
        ensure_sidebar_event_log_reads_through_rollup(&root, std::slice::from_ref(&renderer_bad))
            .unwrap_err();
    assert!(err.to_string().contains("fold through RollupCursor"));
    assert!(
        err.to_string()
            .contains(&renderer_bad.display().to_string())
    );

    ensure_sidebar_event_log_reads_through_rollup(&root, &[test_file]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn sidebar_enrich_stays_projection_only() {
    let root = temp_repo_root("sidebar-enrich-projection-only");
    let bad = root.join("crates/rimz/src/sidebar/enrich/bad.rs");
    let refresh = root.join("crates/rimz/src/sidebar/refresh/usage.rs");
    for path in [&bad, &refresh] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(&bad, "fn f() { std::process::Command::new(\"git\"); }\n")
        .expect("write bad source");
    std::fs::write(
        &refresh,
        "fn f() { std::process::Command::new(\"rimz\"); }\n",
    )
    .expect("write refresh source");

    let err =
        ensure_sidebar_enrich_projection_only(&root, &[bad.clone(), refresh.clone()]).unwrap_err();
    assert!(err.to_string().contains("projection-only"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    assert!(!err.to_string().contains(&refresh.display().to_string()));

    ensure_sidebar_enrich_projection_only(&root, &[refresh]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn pane_auto_use_invariant_allows_marked_run_failure_capture_only() {
    let root = temp_repo_root("pane-capture-boundary");
    let allowed = root.join("crates/rimz/src/cli/agents_cmd/supervised/pane.rs");
    let codex_allowed = root.join("crates/rimz/src/sidebar/refresh/sessions.rs");
    let bad = root.join("crates/rimz/src/cli/agents_cmd/supervised/bad.rs");
    for path in [&allowed, &codex_allowed, &bad] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(
        &allowed,
        concat!(
            "fn f(backend: &dyn MuxBackend, pane: &PaneId) {\n",
            "    // rimz-invariant: run-failure-capture\n",
            "    backend.capture_pane(pane, None, false);\n",
            "}\n",
        ),
    )
    .expect("write allowed source");
    std::fs::write(
        &codex_allowed,
        concat!(
            "fn f(backend: &dyn MuxBackend, pane: &PaneId) {\n",
            "    // rimz-invariant: codex-turn-death-confirmation\n",
            "    backend.capture_pane(pane, Some(60), false);\n",
            "}\n",
        ),
    )
    .expect("write codex allowed source");
    std::fs::write(
        &bad,
        "fn f(backend: &dyn MuxBackend, pane: &PaneId) { backend.capture_pane(pane, None, false); }\n",
    )
    .expect("write bad source");

    ensure_no_core_pane_auto_use(&root, std::slice::from_ref(&allowed)).unwrap();
    ensure_no_core_pane_auto_use(&root, &[allowed.clone(), codex_allowed.clone()]).unwrap();
    let err =
        ensure_no_core_pane_auto_use(&root, &[bad.clone(), allowed, codex_allowed]).unwrap_err();
    assert!(err.to_string().contains("capture"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    let _ = std::fs::remove_dir_all(root);
}

fn temp_repo_root(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{unique}", std::process::id()))
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
    assert!(!ui_color_exempt(Path::new("compose.rs")));
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
