use std::path::Path;

use super::*;

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
