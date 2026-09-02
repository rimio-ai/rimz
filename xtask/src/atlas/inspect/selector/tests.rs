use std::collections::BTreeSet;
use std::fs;

use super::super::super::sources::Source;
use super::super::testkit::selector;
use super::*;

#[test]
fn module_selectors_resolve_paths_and_module_names() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/demo/src/store")).unwrap();
    fs::write(
        root.path().join("crates/demo/src/store/mod.rs"),
        "mod writer;\n",
    )
    .unwrap();
    let syntax = super::super::super::syntax::analyze_sources(
        &[
            Source::new("crates/demo/src/store.rs", "fn entry() {}"),
            Source::new("crates/demo/src/store/writer.rs", "fn write() {}"),
            Source::new("crates/demo/src/cli.rs", "fn run() {}"),
        ],
        &BTreeSet::new(),
    );

    assert_eq!(
        resolve_module(
            root.path(),
            &syntax.files,
            "crates/demo/src/store",
            "inspect",
            "--module"
        )
        .unwrap(),
        ModuleSelector {
            module: "store".to_owned(),
            path: Some(PathBuf::from("crates/demo/src/store")),
            directory: true,
        }
    );
    let store = resolve_module(
        root.path(),
        &syntax.files,
        "crates/demo/src/store",
        "inspect",
        "--module",
    )
    .unwrap();
    assert!(store.matches("store", Path::new("crates/demo/src/store.rs")));
    assert_eq!(
        resolve_module(
            root.path(),
            &syntax.files,
            "crate::cli",
            "inspect",
            "--from"
        )
        .unwrap(),
        selector("cli")
    );
}
