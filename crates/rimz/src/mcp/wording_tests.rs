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
    put(dir.path(), "stop.md", "End what you started, replaced.\n");
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
        "rimz-watch/guides/conditions.md",
        "Conditions, replaced.",
    );
    put(dir.path(), "rimz-watch/notes.txt", "not wording");
    let wording = Wording::read(dir.path());
    assert_eq!(messages(&wording), Vec::<String>::new());

    let stop = wording.description(tool("stop"));
    assert_eq!(stop.text(), "End what you started, replaced.");
    assert_eq!(stop.path(), Some(dir.path().join("stop.md").as_path()));
    assert_eq!(
        wording.description(tool(GUIDE)).text(),
        "Load a guide, replaced."
    );
    let shell = wording.description(tool("shell"));
    assert_eq!(shell.path(), None);
    assert_eq!(
        shell.text(),
        Wording::built_in().description(tool("shell")).text()
    );

    assert_eq!(
        wording.instructions(&[Capability::Lsp]),
        "RimZ tools to read code.\nUse `lsp`, replaced.\nShared, replaced.\nSecond line."
    );
    let paths: Vec<_> = wording.instruction_paths(&[Capability::Lsp]).collect();
    assert_eq!(
        paths,
        [
            dir.path().join("rimz-lsp/instructions.md"),
            dir.path().join("instructions.md")
        ]
    );
    assert_eq!(wording.instruction_paths(&[Capability::Watch]).count(), 1);
    assert_eq!(
        wording.guide(&[Capability::Watch], "rimz-watch/conditions"),
        Ok("Conditions, replaced.")
    );
}

#[test]
fn the_instructions_name_only_the_served_groups_in_group_order() {
    let wording = Wording::built_in();
    let sentence = |group: Capability| wording.file(&group_sentence(group)).text().to_owned();
    let shared = wording.file(SHARED).text().to_owned();
    let started = "Each start returns an `id` (`<kind>:<name>`), how its result arrives in \
                   `report`, and any `warnings`; use `tasks`, `join`, and `stop` to list, block \
                   on, or end what you started.";
    let guide = "Use `guide` to load a usage guide a tool description names.";
    assert_eq!(
        wording.instructions(&[Capability::Notes, Capability::Lsp]),
        format!(
            "RimZ tools to read code and keep notes.\n{}\n{}\n{shared}",
            sentence(Capability::Lsp),
            sentence(Capability::Notes)
        )
    );
    assert_eq!(
        wording.instructions(&[Capability::Control]),
        format!(
            "RimZ tools to manage yourself and load usage guides.\n{}\n{guide}\n{shared}",
            sentence(Capability::Control)
        )
    );
    assert_eq!(
        wording.instructions(&[Capability::Shell]),
        format!(
            "RimZ tools to wait outside your turn and track what you started.\n{}\n{started}\n\
             {shared}",
            sentence(Capability::Shell)
        )
    );
    assert_eq!(
        wording.instructions(&[Capability::Subagents]),
        format!(
            "RimZ tools to delegate, track what you started, and load usage guides.\n{}\n\
             {started}\n{guide}\n{shared}",
            sentence(Capability::Subagents)
        )
    );
    let all = wording.instructions(&Capability::ALL);
    assert!(!all.contains("only some of these tools"), "{all}");
    assert!(all.contains(started), "{all}");
    assert!(all.contains(guide), "{all}");
    assert_eq!(wording.instruction_paths(&Capability::ALL).count(), 0);
    // `instructions_breach` counts both shared-tool sentences against `TEXT_LIMIT`.
    let fixed_len = all.chars().count() - shared.chars().count();
    for (extra, overflows) in [(0, false), (1, true)] {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            SHARED,
            "x".repeat(TEXT_LIMIT - fixed_len + extra),
        );
        let loaded = Wording::read(dir.path());
        assert_eq!(
            !messages(&loaded).is_empty(),
            overflows,
            "extra={extra} warnings={:?}",
            messages(&loaded)
        );
    }
}

/// Each reason a replacement cannot serve: the built-in text is served in its
/// place, so no wording file switches a tool, a group, or `guide` off.
#[test]
fn an_unusable_replacement_warns_and_the_built_in_text_is_served() {
    let built_in = Wording::built_in();
    let long_sentence = format!("Use `lsp` {}.", "word ".repeat(16));
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("rimz-shell/shell.md", vec![0xff, 0xfe, b'\n'], "not UTF-8"),
        ("rimz-shell/shell.md", b"  \n\t\n".to_vec(), "it is empty"),
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
            "rimz-notes/instructions.md",
            "y".repeat(4000).into_bytes(),
            "the instructions with every group would hold",
        ),
        (
            "rimz-watch/guides/conditions.md",
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
    // Each group's sentence has a line of its own, so a long one leaves the generated first
    // line as it is.
    let wording = Wording::read(dir.path());
    assert_eq!(messages(&wording), Vec::<String>::new());
    let all = wording.instructions(&Capability::ALL);
    assert_eq!(
        all.lines().next(),
        built_in.instructions(&Capability::ALL).lines().next()
    );
    assert!(all.contains(&long_sentence), "{all}");

    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("rimz-notes")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("missing.md"),
            dir.path().join("rimz-notes/notes.md"),
        )
        .unwrap();
        let wording = Wording::read(dir.path());
        assert_eq!(wording.description(tool("notes")).path(), None);
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
    put(dir.path(), "rimz-lsp/shell.md", "Run a command, misplaced.");
    put(
        dir.path(),
        "rimz-lsp/guides/conditions.md",
        "Conditions, misplaced.",
    );
    put(dir.path(), "rimz-shell/extra.md", "A tool of my own.");
    let wording = Wording::read(dir.path());
    assert_eq!(wording.description(tool("shell")).path(), None);
    let shown = |path: &str| dir.path().join(path).display().to_string();
    assert_eq!(
        messages(&wording),
        [
            format!(
                "{} is ignored: RimZ has no wording file rimz-lsp/guides/conditions.md; \
                 it belongs at {}",
                shown("rimz-lsp/guides/conditions.md"),
                shown("rimz-watch/guides/conditions.md")
            ),
            format!(
                "{} is ignored: RimZ has no wording file rimz-lsp/shell.md; it belongs at {}",
                shown("rimz-lsp/shell.md"),
                shown("rimz-shell/shell.md")
            ),
            format!(
                "{} is ignored: RimZ has no wording file rimz-shell/extra.md",
                shown("rimz-shell/extra.md")
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
        "rimz-shell/shell.md",
        "Arm a watcher, linked.",
    );
    std::fs::create_dir_all(elsewhere.path().join("rimz-shell/guides")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("rimz-shell"),
        elsewhere.path().join("rimz-shell/guides/back"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("rimz-shell"),
        dir.path().join("rimz-shell"),
    )
    .unwrap();
    let wording = Wording::read(dir.path());
    let shell = wording.description(tool("shell"));
    assert_eq!(shell.text(), "Arm a watcher, linked.");
    assert_eq!(
        shell.path(),
        Some(dir.path().join("rimz-shell/shell.md").as_path())
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
    put(
        dir.path(),
        "rimz-shell/shell.md",
        "Arm a watcher, replaced.",
    );
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
        wording.description(tool("shell")).text(),
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
    assert_eq!(wording.warnings_for(&[Capability::Shell]).count(), 0);
    assert_eq!(wording.warnings_for(&[Capability::Lsp]).count(), 1);
}

#[test]
fn a_launch_carries_the_warnings_of_its_groups_and_shared_files() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "rimz-lsp/lsp.md", "");
    put(dir.path(), "guide.md", "");
    put(dir.path(), "rimz-watch/guides/conditions.md", "");
    let wording = Wording::read(dir.path());
    let count = |groups: &[Capability]| wording.warnings_for(groups).count();
    assert_eq!(count(&[Capability::Lsp]), 2);
    assert_eq!(count(&[Capability::Notes]), 1);
    assert_eq!(count(&[Capability::Watch]), 2);
    assert_eq!(
        count(&[Capability::Loop]),
        2,
        "a launch serving rimz-loop alone is served the conditions guide, so it hears its warning"
    );
    assert_eq!(
        Wording::read(&dir.path().join("absent")).warnings().len(),
        0
    );
}

#[test]
fn a_guide_answers_for_a_served_group_and_lists_the_served_guides_otherwise() {
    let wording = Wording::built_in();
    let conditions = wording
        .guide(&[Capability::Watch], "rimz-watch/conditions")
        .expect("the conditions guide");
    assert!(conditions.contains("`pr.queue`"), "{conditions}");
    let served = vec![
        "rimz-watch/conditions".to_owned(),
        "rimz-subagents/delegating".to_owned(),
        "rimz-agents/launching".to_owned(),
        "rimz-teams/launching".to_owned(),
        "rimz-workflow/steering".to_owned(),
        "rimz-workflow/debugging".to_owned(),
        "rimz-loop/targets".to_owned(),
        "rimz-loop/signals".to_owned(),
        "rimz-loop/managing".to_owned(),
        "rimz-control/fresh-eyes".to_owned(),
    ];
    assert_eq!(
        wording.guide(&Capability::ALL, "rimz-watch/nope"),
        Err(served.clone())
    );
    assert_eq!(wording.guide(&Capability::ALL, "conditions"), Err(served));
    assert_eq!(
        wording.guide(&[Capability::Shell], "rimz-watch/conditions"),
        Err(Vec::new())
    );
    assert_eq!(
        wording.guide(&[Capability::Loop], "rimz-watch/conditions"),
        Ok(conditions),
        "a launch serving rimz-loop alone loads the one conditions guide"
    );
    assert!(has_guides(Capability::Watch));
    assert!(!has_guides(Capability::Shell));
    assert!(has_guides(Capability::Loop));
    assert!(has_guides(Capability::Subagents));
    assert!(has_guides(Capability::Agents));
    assert!(has_guides(Capability::Teams));
    assert!(!has_guides(Capability::Lsp));
}
