use std::path::Path;

use super::*;
use crate::mcp::catalog::GUIDE;

fn tool(name: &str) -> &'static Tool {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

fn put(dir: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn messages(wording: &Wording) -> Vec<String> {
    wording.warnings().iter().map(ToString::to_string).collect()
}

#[test]
fn a_replacement_replaces_its_file_and_is_marked_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        "rimz-wait/wait_list.md",
        "List your waits, replaced.\n",
    );
    put(
        dir.path(),
        "rimz-lsp/instructions.md",
        "  Use `lsp`, replaced.\n\n",
    );
    put(
        dir.path(),
        "instructions.md",
        "Shared, replaced.\nSecond line.\n",
    );
    put(dir.path(), "guide.md", "Load a guide, replaced.");
    put(
        dir.path(),
        "rimz-wait/guides/conditions.md",
        "Conditions, replaced.",
    );
    put(dir.path(), "rimz-wait/notes.txt", "not wording");
    let wording = Wording::read(dir.path());
    assert_eq!(messages(&wording), Vec::<String>::new());

    let list = wording.description(tool("wait_list"));
    assert_eq!(list.text(), "List your waits, replaced.");
    assert_eq!(
        list.path(),
        Some(dir.path().join("rimz-wait/wait_list.md").as_path())
    );
    assert_eq!(
        wording.description(tool(GUIDE)).text(),
        "Load a guide, replaced."
    );
    let wait = wording.description(tool("wait"));
    assert_eq!(wait.path(), None);
    assert_eq!(
        wait.text(),
        Wording::built_in().description(tool("wait")).text()
    );

    assert_eq!(
        wording.instructions(&[Capability::Lsp]),
        "Use `lsp`, replaced.\nShared, replaced.\nSecond line."
    );
    let paths: Vec<_> = wording.instruction_paths(&[Capability::Lsp]).collect();
    assert_eq!(
        paths,
        [
            dir.path().join("rimz-lsp/instructions.md"),
            dir.path().join("instructions.md")
        ]
    );
    assert_eq!(wording.instruction_paths(&[Capability::Wait]).count(), 1);
    assert_eq!(
        wording.guide(&[Capability::Wait], "rimz-wait/conditions"),
        Ok("Conditions, replaced.")
    );
}

#[test]
fn the_instructions_name_only_the_served_groups_in_group_order() {
    let wording = Wording::built_in();
    let sentence = |group: Capability| wording.file(&group_sentence(group)).text().to_owned();
    let shared = wording.file(SHARED).text().to_owned();
    assert_eq!(
        wording.instructions(&[Capability::Notes, Capability::Lsp]),
        format!(
            "{} {}\n{shared}",
            sentence(Capability::Lsp),
            sentence(Capability::Notes)
        )
    );
    assert_eq!(
        wording.instructions(&[Capability::Wait]),
        format!("{}\n{shared}", sentence(Capability::Wait))
    );
    let all = wording.instructions(&Capability::ALL);
    assert!(!all.contains("only some of these tools"), "{all}");
    assert_eq!(wording.instruction_paths(&Capability::ALL).count(), 0);
}

/// Each reason a replacement cannot serve: the built-in text is served in its
/// place, so no wording file switches a tool, a group, or `guide` off.
#[test]
fn an_unusable_replacement_warns_and_the_built_in_text_is_served() {
    let built_in = Wording::built_in();
    let long_sentence = format!("Use `lsp` {}.", "word ".repeat(16));
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("rimz-wait/wait.md", vec![0xff, 0xfe, b'\n'], "not UTF-8"),
        ("rimz-wait/wait.md", b"  \n\t\n".to_vec(), "it is empty"),
        (
            "rimz-lsp/lsp.md",
            "x".repeat(4096).into_bytes(),
            "`lsp`'s description and its arguments' descriptions would hold",
        ),
        (
            "guide.md",
            "x".repeat(4096).into_bytes(),
            "`guide`'s description and its arguments' descriptions would hold",
        ),
        (
            "rimz-notes/instructions.md",
            b"Use notes.\nAnd more.".to_vec(),
            "a group's instructions are one line",
        ),
        (
            "instructions.md",
            "y".repeat(4000).into_bytes(),
            "the instructions with every group would hold",
        ),
        (
            "rimz-wait/guides/conditions.md",
            b"\n".to_vec(),
            "it is empty",
        ),
    ];
    for (path, bytes, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), path, &bytes);
        let wording = Wording::read(dir.path());
        let file = wording.file(path);
        assert_eq!(file.text(), built_in.file(path).text(), "{path}");
        assert_eq!(file.path(), None, "{path}");
        let warnings = messages(&wording);
        let shown = dir.path().join(path).display().to_string();
        assert!(
            warnings.len() == 1
                && warnings[0].starts_with(&format!(
                    "{shown} is not used, so the built-in text is served: "
                ))
                && warnings[0].contains(reason),
            "{path}: {warnings:?}"
        );
        assert_eq!(wording.breaches(), Vec::<String>::new(), "{path}");
    }

    let dir = tempfile::tempdir().unwrap();
    for group in Capability::ALL {
        put(dir.path(), &group_sentence(group), &long_sentence);
    }
    let wording = Wording::read(dir.path());
    assert_eq!(
        wording.instructions(&Capability::ALL),
        built_in.instructions(&Capability::ALL)
    );
    let warnings = messages(&wording);
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains("the instructions' first line")),
        "{warnings:?}"
    );

    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("rimz-wait")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("missing.md"),
            dir.path().join("rimz-wait/wait_cancel.md"),
        )
        .unwrap();
        let wording = Wording::read(dir.path());
        assert_eq!(wording.description(tool("wait_cancel")).path(), None);
        let warnings = messages(&wording);
        assert!(
            warnings.len() == 1 && warnings[0].contains("it cannot be read: "),
            "{warnings:?}"
        );
    }
}

#[test]
fn a_file_at_no_wording_path_is_ignored_and_named_where_it_belongs() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "rimz-lsp/wait.md", "Arm a watcher, misplaced.");
    put(
        dir.path(),
        "rimz-lsp/guides/conditions.md",
        "Conditions, misplaced.",
    );
    put(dir.path(), "rimz-wait/extra.md", "A tool of my own.");
    let wording = Wording::read(dir.path());
    assert_eq!(wording.description(tool("wait")).path(), None);
    let shown = |path: &str| dir.path().join(path).display().to_string();
    assert_eq!(
        messages(&wording),
        [
            format!(
                "{} is ignored: RimZ has no wording file rimz-lsp/guides/conditions.md; \
                 it belongs at {}",
                shown("rimz-lsp/guides/conditions.md"),
                shown("rimz-wait/guides/conditions.md")
            ),
            format!(
                "{} is ignored: RimZ has no wording file rimz-lsp/wait.md; it belongs at {}",
                shown("rimz-lsp/wait.md"),
                shown("rimz-wait/wait.md")
            ),
            format!(
                "{} is ignored: RimZ has no wording file rimz-wait/extra.md",
                shown("rimz-wait/extra.md")
            ),
        ]
    );
    assert_eq!(wording.warnings_for(&[Capability::Notes]).count(), 3);
}

/// A symlinked group folder is walked like a folder, and the walk stops at the
/// deepest wording path, so a symlink loop below it ends.
#[cfg(unix)]
#[test]
fn a_symlinked_group_folder_is_served_and_a_loop_in_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    put(
        elsewhere.path(),
        "rimz-wait/wait.md",
        "Arm a watcher, linked.",
    );
    std::fs::create_dir_all(elsewhere.path().join("rimz-wait/guides")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("rimz-wait"),
        elsewhere.path().join("rimz-wait/guides/back"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("rimz-wait"),
        dir.path().join("rimz-wait"),
    )
    .unwrap();
    let wording = Wording::read(dir.path());
    let wait = wording.description(tool("wait"));
    assert_eq!(wait.text(), "Arm a watcher, linked.");
    assert_eq!(
        wait.path(),
        Some(dir.path().join("rimz-wait/wait.md").as_path())
    );
    assert_eq!(messages(&wording), Vec::<String>::new());
}

/// A folder below the root that cannot be read warns by its own path, and
/// the replacements beside it are still served.
#[cfg(unix)]
#[test]
fn an_unreadable_folder_warns_by_its_path_and_its_siblings_are_served() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "rimz-wait/wait.md", "Arm a watcher, replaced.");
    put(dir.path(), "rimz-lsp/lsp.md", "Unreadable.");
    let locked = dir.path().join("rimz-lsp");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads a folder whatever its mode, so the case cannot be built.
    if std::fs::read_dir(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let wording = Wording::read(dir.path());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        wording.description(tool("wait")).text(),
        "Arm a watcher, replaced."
    );
    let warnings = messages(&wording);
    assert!(
        warnings.len() == 1
            && warnings[0].starts_with(&format!(
                "{} cannot be read, so no file in it is used: ",
                locked.display()
            )),
        "{warnings:?}"
    );
    assert_eq!(wording.warnings_for(&[Capability::Wait]).count(), 0);
    assert_eq!(wording.warnings_for(&[Capability::Lsp]).count(), 1);
}

#[test]
fn a_launch_carries_the_warnings_of_its_groups_and_shared_files() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "rimz-lsp/lsp.md", "");
    put(dir.path(), "guide.md", "");
    let wording = Wording::read(dir.path());
    let count = |groups: &[Capability]| wording.warnings_for(groups).count();
    assert_eq!(count(&[Capability::Lsp]), 2);
    assert_eq!(count(&[Capability::Notes]), 1);
    assert_eq!(
        Wording::read(&dir.path().join("absent")).warnings().len(),
        0
    );
}

#[test]
fn a_guide_answers_for_a_served_group_and_lists_the_served_guides_otherwise() {
    let wording = Wording::built_in();
    let conditions = wording
        .guide(&[Capability::Wait], "rimz-wait/conditions")
        .expect("the conditions guide");
    assert!(conditions.contains("`pr.queue`"), "{conditions}");
    let served = vec!["rimz-wait/conditions".to_owned()];
    assert_eq!(
        wording.guide(&Capability::ALL, "rimz-wait/nope"),
        Err(served.clone())
    );
    assert_eq!(wording.guide(&Capability::ALL, "conditions"), Err(served));
    assert_eq!(
        wording.guide(&[Capability::Lsp], "rimz-wait/conditions"),
        Err(Vec::new())
    );
    assert!(has_guides(Capability::Wait));
    assert!(!has_guides(Capability::Lsp));
}
