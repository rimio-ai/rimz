use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use scip::types::{Occurrence, SymbolRole};

use super::super::references::{Edge, EdgeKind, FnRef};
use super::selector::ModuleSelector;

pub(super) fn crate_with_files(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("temporary fixture directory");
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write fixture manifest");
    for (path, text) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().expect("fixture path has a parent"))
            .expect("create fixture directory");
        fs::write(path, text).expect("write fixture source");
    }
    root
}

pub(super) fn occurrence(line: i32, symbol: &str, definition: bool) -> Occurrence {
    Occurrence {
        range: vec![line, 0, 1],
        symbol: symbol.to_owned(),
        symbol_roles: if definition {
            SymbolRole::Definition as i32
        } else {
            0
        },
        ..Occurrence::default()
    }
}

pub(super) fn edge(item: &str, from: &str, function: Option<(&str, usize)>, test: bool) -> Edge {
    Edge {
        from_path: PathBuf::from(format!("crates/demo/src/{from}.rs")),
        to_path: PathBuf::from("crates/demo/src/store.rs"),
        from_line: function.map_or(1, |(_, line)| line + 1),
        from_fn: function.map(|(label, line)| FnRef {
            label: label.to_owned(),
            line,
        }),
        from: from.to_owned(),
        to: "store".to_owned(),
        to_line: 1,
        item: item.to_owned(),
        kind: EdgeKind::Reference,
        test,
    }
}

pub(super) fn selector(module: &str) -> ModuleSelector {
    ModuleSelector {
        module: module.to_owned(),
        path: None,
        directory: false,
    }
}

pub(super) fn run(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn commit(root: &Path, message: &str) {
    run(root, &["add", "lib.rs"]);
    run(
        root,
        &[
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=atlas@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        ],
    );
}
