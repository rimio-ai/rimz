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
        &agents_root.join("adapters/codex/spend.rs"),
        agents_root,
    ));
    assert!(is_agent_spend_parser_path(
        &agents_root.join("adapters/codex/spend/wire.rs"),
        agents_root,
    ));
    assert!(is_agent_spend_parser_path(
        &agents_root.join("transcript_fs.rs"),
        agents_root,
    ));
    assert!(is_agent_spend_parser_path(
        &agents_root.join("adapters/plugin/probes.rs"),
        agents_root,
    ));
    assert!(!is_agent_spend_parser_path(
        &agents_root.join("adapters/codex/mod.rs"),
        agents_root,
    ));
    assert!(!is_agent_spend_parser_path(
        Path::new("/repo/crates/rimz/src/sidebar/spend/wire.rs"),
        agents_root,
    ));
}

#[test]
fn generic_process_consumers_require_normalized_adapter_decisions() {
    let root = temp_repo_root("normalized-agent-process-decisions");
    let hooks = root.join("crates/rimz/src/cli/hooks.rs");
    let owner = root.join("crates/rimz/src/cli/hooks/owner.rs");
    let pane_probe = root.join("crates/rimz/src/proc/pane_probe.rs");
    for path in [&hooks, &owner, &pane_probe] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(
        &hooks,
        "fn f() { rimz::agents::codex::pid_is_codex_daemon(1); }\n",
    )
    .expect("write forbidden hook decision");
    std::fs::write(
        &owner,
        "fn f(definition: &AgentDefinition) { definition.hook_ingress(None); }\n",
    )
    .expect("write normalized hook decision");
    std::fs::write(
        &pane_probe,
        "fn f(command: &str) { crate::agents::registry::command_agent_kind(command); }\n",
    )
    .expect("write normalized process decision");

    let err = ensure_normalized_agent_process_decisions(
        &root,
        &[hooks.clone(), owner.clone(), pane_probe.clone()],
    )
    .unwrap_err();
    assert!(err.to_string().contains("normalized adapter or registry"));
    assert!(err.to_string().contains(&hooks.display().to_string()));
    assert!(!err.to_string().contains(&owner.display().to_string()));
    assert!(!err.to_string().contains(&pane_probe.display().to_string()));

    std::fs::write(
        &hooks,
        "fn f(definition: &AgentDefinition) { definition.hook_ingress(None); }\n",
    )
    .expect("write normalized hook decision");
    ensure_normalized_agent_process_decisions(&root, &[hooks, owner, pane_probe]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn provider_implementations_stay_behind_the_agents_boundary() {
    let root = temp_repo_root("private-agent-adapters");
    let consumer = root.join("crates/rimz/src/cli/bad.rs");
    let provider = root.join("crates/rimz/src/agents/adapters/codex/mod.rs");
    for path in [&consumer, &provider] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(
        &consumer,
        "fn f() { crate::agents::adapters::codex::probe(); }\n",
    )
    .expect("write forbidden provider import");
    std::fs::write(&provider, "pub struct CodexAdapter;\n").expect("write private adapter");

    let err = ensure_private_agent_adapter_boundary(&root, &[consumer.clone(), provider.clone()])
        .expect_err("consumer provider import is rejected");
    assert!(err.to_string().contains(&consumer.display().to_string()));
    assert!(!err.to_string().contains(&provider.display().to_string()));

    std::fs::write(
        &consumer,
        "fn f() { crate::agents::definition_by_kind(\"codex\"); }\n",
    )
    .expect("write neutral lookup");
    ensure_private_agent_adapter_boundary(&root, &[consumer, provider]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn adapter_derived_kinds_use_the_typed_descriptor_accessor() {
    let root = temp_repo_root("typed-adapter-kind");
    let supervised = root.join("crates/rimz/src/cli/supervised.rs");
    std::fs::create_dir_all(supervised.parent().expect("test path has parent")).expect("mkdir");
    std::fs::write(
        &supervised,
        "fn f(definition: &AgentDefinition) { AgentKind::new_unchecked(\n definition.spec().kind\n); }\n",
    )
    .expect("write unchecked adapter kind");

    let err = ensure_adapter_kinds_stay_typed(&root, std::slice::from_ref(&supervised))
        .expect_err("adapter-derived unchecked kind is rejected");
    assert!(err.to_string().contains("typed kind_id"));

    std::fs::write(
        &supervised,
        "fn f(definition: &AgentDefinition) { let _ = definition.spec().kind_id(); }\n",
    )
    .expect("write typed adapter kind");
    ensure_adapter_kinds_stay_typed(&root, &[supervised]).unwrap();
    let _ = std::fs::remove_dir_all(root);
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
        "fn f() { crate::store::event_log",
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
fn long_lived_renderers_cannot_construct_spending_walkers() {
    let root = temp_repo_root("spending-walker-ownership");
    let data_plane = root.join("crates/rimz/src/sidebar/refresh/mod.rs");
    let data_plane_test = root.join("crates/rimz/src/sidebar/enrich/tests.rs");
    let sidebar = root.join("crates/rimz/src/sidebar_pane/app/cache_refresh.rs");
    let held = root.join("crates/rimz/src/cli/stats/hold.rs");
    let direct = root.join("crates/rimz/src/cli/stats/mod.rs");
    for path in [&data_plane, &data_plane_test, &sidebar, &held, &direct] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
        std::fs::write(path, "fn f() { SpendingWalker::new(); }\n").expect("write source");
    }

    let err = ensure_spending_walker_ownership(
        &root,
        &[
            data_plane.clone(),
            data_plane_test.clone(),
            sidebar.clone(),
            held.clone(),
            direct.clone(),
        ],
    )
    .unwrap_err();
    assert!(err.to_string().contains("elected spending service"));
    assert!(err.to_string().contains(&data_plane.display().to_string()));
    assert!(
        !err.to_string()
            .contains(&data_plane_test.display().to_string())
    );
    assert!(err.to_string().contains(&sidebar.display().to_string()));
    assert!(err.to_string().contains(&held.display().to_string()));
    assert!(!err.to_string().contains(&direct.display().to_string()));

    ensure_spending_walker_ownership(&root, &[direct]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn agent_domain_cannot_depend_on_sidebar_code() {
    let root = temp_repo_root("agents-sidebar-boundary");
    let production = root.join("crates/rimz/src/agents/spending/engine.rs");
    let comment_only = root.join("crates/rimz/src/agents/account.rs");
    let tests = root.join("crates/rimz/src/agents/spending/tests.rs");
    for path in [&production, &comment_only, &tests] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(&production, "fn f() { crate::sidebar::refresh(); }\n").expect("source");
    std::fs::write(
        &comment_only,
        "//! `crate::sidebar` consumes published state.\n/* crate::sidebar */\n",
    )
    .expect("comments");
    std::fs::write(&tests, "fn f() { crate::sidebar::fixture(); }\n").expect("tests");

    let err = ensure_agents_do_not_depend_on_sidebar(
        &root,
        &[production.clone(), comment_only.clone(), tests.clone()],
    )
    .unwrap_err();
    assert!(err.to_string().contains("must not depend on sidebar"));
    assert!(err.to_string().contains(&production.display().to_string()));
    assert!(
        !err.to_string()
            .contains(&comment_only.display().to_string())
    );
    assert!(!err.to_string().contains(&tests.display().to_string()));

    ensure_agents_do_not_depend_on_sidebar(&root, &[comment_only, tests]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn snapshot_projection_diagnostics_stay_debug_level() {
    let root = temp_repo_root("snapshot-projection-diagnostics");
    let bad = root.join("crates/rimz/src/store/snapshot/view/bad.rs");
    let test_file = root.join("crates/rimz/src/store/snapshot/view/tests/logging.rs");
    for path in [&bad, &test_file] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    std::fs::write(
        &bad,
        "fn f() { tracing::warn!(\"repeated projection\"); }\n",
    )
    .expect("write bad source");
    std::fs::write(
        &test_file,
        "fn f() { tracing::warn!(\"test diagnostic\"); }\n",
    )
    .expect("write test source");

    let err = ensure_snapshot_projection_stays_quiet(&root, &[bad.clone(), test_file.clone()])
        .unwrap_err();
    assert!(err.to_string().contains("diagnostics stay debug!-level"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    assert!(!err.to_string().contains(&test_file.display().to_string()));

    ensure_snapshot_projection_stays_quiet(&root, &[test_file]).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn pane_auto_use_invariant_allows_marked_run_failure_capture_only() {
    let root = temp_repo_root("pane-capture-boundary");
    let allowed = root.join("crates/rimz/src/cli/supervised/pane.rs");
    let codex_allowed = root.join("crates/rimz/src/sidebar/refresh/sessions.rs");
    let bad = root.join("crates/rimz/src/cli/supervised/bad.rs");
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
fn ui_color_exemptions_cover_the_carrier_edge_and_tests() {
    // The render-side theme module mints ratatui carriers; ansi.rs quantizes them.
    assert!(ui_color_exempt(Path::new("theme.rs")));
    assert!(ui_color_exempt(Path::new("theme/component.rs")));
    assert!(ui_color_exempt(Path::new("ansi.rs")));
    // Tests assert carrier→slot mappings, inline or in a tests tree.
    assert!(ui_color_exempt(Path::new("labels/tests/meters.rs")));
    assert!(ui_color_exempt(Path::new("tests/process.rs")));
    // Ordinary render code must name intent. Scheme parsing and OKLab math live
    // in the shared theme core, so those names carry no exemption here.
    assert!(!ui_color_exempt(Path::new("scheme.rs")));
    assert!(!ui_color_exempt(Path::new("oklab.rs")));
    assert!(!ui_color_exempt(Path::new("compose.rs")));
    assert!(!ui_color_exempt(Path::new("labels/meters.rs")));
    assert!(!ui_color_exempt(Path::new("sections/agent_card/gauge.rs")));
}

#[test]
fn ui_glyph_exemptions_cover_animation_and_tests() {
    // Animation owns the spinner frame sequences.
    assert!(ui_glyph_exempt(Path::new("animation.rs")));
    // Tests assert rendered shapes, inline or in a tests tree.
    assert!(ui_glyph_exempt(Path::new("labels/tests/meters.rs")));
    assert!(ui_glyph_exempt(Path::new("tests/process.rs")));
    // Ordinary render code routes through theme.glyph(GlyphRole::…), including
    // the render-side theme module — the shipped catalog lives in the shared
    // theme core, outside this scan.
    assert!(!ui_glyph_exempt(Path::new("theme.rs")));
    assert!(!ui_glyph_exempt(Path::new("theme/component.rs")));
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

#[test]
fn cli_color_provenance_flags_carriers_but_exempts_palette_and_inline_tests() {
    let root = temp_repo_root("cli-color-provenance");
    let bad = root.join("crates/rimz/src/cli/list.rs");
    let palette = root.join("crates/rimz/src/cli/render/palette.rs");
    for path in [&bad, &palette] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    let carrier = concat!("let c = anstyle", "::", "Color::Ansi256(v);\n");
    std::fs::write(&bad, carrier).expect("write bad source");
    std::fs::write(&palette, carrier).expect("write palette source");

    let err = ensure_cli_color_provenance(&root, &[bad.clone(), palette.clone()]).unwrap_err();
    assert!(err.to_string().contains("render::palette accessors"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    assert!(!err.to_string().contains(&palette.display().to_string()));
    assert!(
        cli_color_violation_lines(&format!("mod tests {{\n{carrier}}}\n")).is_empty(),
        "inline tests may assert carrier shapes"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn brand_resolution_invariant_keeps_descriptor_reads_in_theme_core() {
    let root = temp_repo_root("brand-resolution-home");
    let bad = root.join("crates/rimz/src/cli/agents_cmd/list.rs");
    let provider = root.join("crates/rimz/src/theme/provider.rs");
    for path in [&bad, &provider] {
        std::fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
    }
    let read = concat!("let c = descriptor.brand", ".color_rgb;\n");
    std::fs::write(&bad, read).expect("write bad source");
    std::fs::write(&provider, read).expect("write provider source");

    let err =
        ensure_brand_resolution_single_home(&root, &[bad.clone(), provider.clone()]).unwrap_err();
    assert!(err.to_string().contains("resolve_provider_identity"));
    assert!(err.to_string().contains(&bad.display().to_string()));
    assert!(!err.to_string().contains(&provider.display().to_string()));
    let _ = std::fs::remove_dir_all(root);
}
