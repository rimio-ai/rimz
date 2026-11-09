use super::*;
use serde_json::json;

fn nodes() -> Vec<Candidate> {
    outline(json!([
        {"name":"Parent","kind":5,
         "range":{"start":{"line":0,"character":0},"end":{"line":204,"character":1}},
         "selectionRange":{"start":{"line":1,"character":3},"end":{"line":1,"character":9}},
         "children":[
            {"name":"child","kind":6,
             "range":{"start":{"line":2,"character":0},"end":{"line":4,"character":1}},
             "selectionRange":{"start":{"line":3,"character":3},"end":{"line":3,"character":8}}}
         ]},
        {"name":"child","kind":12,
         "range":{"start":{"line":207,"character":0},"end":{"line":208,"character":1}},
         "selectionRange":{"start":{"line":207,"character":3},"end":{"line":207,"character":8}}}
    ]))
    .unwrap()
}

fn source() -> String {
    (1..=210).map(|n| format!("  line {n}\n")).collect()
}

#[test]
fn show_folder_reading_wins_and_preserves_the_enclosing_line_target() {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    std::fs::create_dir(root.join("src")).unwrap();
    for file in ["lib.rs", "src/lib.rs"] {
        std::fs::write(root.join(file), source()).unwrap();
    }
    let context = Context {
        checkout: root,
        entries: &[],
        servers: &BTreeMap::new(),
        server: None,
        files: vec!["lib.rs".into(), "src/lib.rs".into()],
        outlines: BTreeMap::new(),
        failures: BTreeMap::new(),
        usage: None,
    };
    let (anchor, position) = parse_argument("lib.rs:4").unwrap();
    let (anchor, path) = resolve_argument(&context, anchor, Path::new("src")).unwrap();
    assert_eq!(
        render_item(&anchor, position, &path, &nodes(), &source(), false, false),
        (
            "src/lib.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n".into(),
            0
        )
    );
}

#[test]
fn show_missing_folder_path_names_both_readings_and_the_suffix_fallback() {
    let context = Context {
        checkout: Path::new("/checkout"),
        entries: &[],
        servers: &BTreeMap::new(),
        server: None,
        files: Vec::new(),
        outlines: BTreeMap::new(),
        failures: BTreeMap::new(),
        usage: None,
    };
    let anchor = parse_argument("gone.rs:1-2").unwrap().0;
    let error = resolve_argument(&context, anchor, Path::new("src"))
        .err()
        .unwrap();
    assert_eq!(
        (error.status, error.detail),
        (Status::MissingPath, "file not found: gone.rs; looked in current folder: src/gone.rs; checkout root: gone.rs; relative paths are read from the current folder when it is inside the checkout, then the checkout root; no checkout file ends with gone.rs".into())
    );
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("lib.rs");
    std::fs::write(&path, "outside\n").unwrap();
    let anchor = parse_argument(&format!("{}:1-1", path.display()))
        .unwrap()
        .0;
    let error = resolve_argument(&context, anchor, Path::new("src"))
        .err()
        .unwrap();
    assert_eq!(
        (error.status, error.detail),
        (Status::MissingPath, "no matching checkout file".into())
    );
}

fn read(argument: &str, dirty: bool, full: bool) -> (String, i32) {
    let (anchor, position) = parse_argument(argument).unwrap();
    render_item(
        &anchor,
        position,
        Path::new("src/lib.rs"),
        &nodes(),
        &source(),
        dirty,
        full,
    )
}

#[test]
fn show_symbol_positions_and_pasted_spans_read_the_same_body() {
    let expected = "src/lib.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n";
    for argument in [
        "src/lib.rs::Parent::child",
        "src/lib.rs::Parent::child ~100",
        "src/lib.rs:4:4",
        "src/lib.rs:5:1",
        "src/lib.rs:4:4 (3-5)",
        "src/lib.rs:3-5",
        "src/lib.rs:4",
        "src/lib.rs:~4",
    ] {
        assert_eq!(
            read(argument, false, false),
            (expected.into(), 0),
            "{argument}"
        );
    }
    assert_eq!(
        read("src/lib.rs::Parent::child", true, false).0,
        expected.replace(":3-5\n", ":3-5  (unsaved in editor)\n")
    );
}

#[test]
fn show_zoom_uses_child_selection_and_full_never_truncates() {
    assert_eq!(read("src/lib.rs::Parent", true, false), ("src/lib.rs:1-205  (unsaved in editor)  (outline: 205 lines; --full prints the body)\n     4\tline 4 (3-5)\n".into(), 0));
    for full in [false, true] {
        assert_eq!(
            read("src/lib.rs:2", true, full),
            read("src/lib.rs::Parent", true, full)
        );
    }
    let body = read("src/lib.rs::Parent", false, true);
    assert_eq!(body.1, 0);
    assert_eq!(body.0.lines().count(), 206);
    assert!(body.0.ends_with("   205\t  line 205\n"));
    let mut leaf = nodes();
    leaf.truncate(1);
    assert_eq!(
        render_item(
            &parse_argument("src/lib.rs::Parent").unwrap().0,
            None,
            Path::new("src/lib.rs"),
            &leaf,
            &source(),
            false,
            false
        ),
        body
    );
}

#[test]
fn show_lines_cover_indented_starts_but_not_exclusive_ends() {
    let mut nodes = nodes();
    nodes[1].extent.start.character = 4;
    nodes[1].extent.end = Position {
        line: 4,
        character: 0,
    };
    for (argument, expected) in [
        ("src/lib.rs:3", "src/lib.rs::Parent::child"),
        ("src/lib.rs:~3", "src/lib.rs::Parent::child"),
        ("src/lib.rs:5", "src/lib.rs::Parent"),
    ] {
        let (anchor, position) = parse_argument(argument).unwrap();
        assert_eq!(
            render_item(
                &anchor,
                position,
                Path::new("src/lib.rs"),
                &nodes,
                &source(),
                false,
                false
            ),
            read(expected, false, false),
            "{argument}"
        );
    }
}

#[test]
fn show_uncovered_lines_and_explicit_ranges_read_only_the_line() {
    for argument in ["src/lib.rs:206", "src/lib.rs:~206"] {
        assert_eq!(
            read(argument, false, false),
            ("src/lib.rs:206-206\n   206\t  line 206\n".into(), 0)
        );
    }
    for argument in ["src/lib.rs:4-4", "src/lib.rs:~4:1", "src/lib.rs:4:0"] {
        assert_eq!(
            read(argument, false, false),
            ("src/lib.rs:4-4\n     4\t  line 4\n".into(), 0)
        );
    }
}

#[test]
fn show_unavailable_outline_falls_back_only_for_bare_lines() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(root.path().join("lib.rs"), "first\nsecond\n").unwrap();
    let servers = BTreeMap::from([(
        "rust".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["rs"], "root-markers": []
        }))
        .unwrap(),
    )]);
    let blocks = show(
        &["lib.rs:2".into(), "lib.rs:~2".into(), "lib.rs:2:1".into()],
        false,
        root.path(),
        Path::new(""),
        &[],
        &servers,
        None,
        None,
    )
    .unwrap();
    assert_eq!(blocks[0], ("lib.rs:2-2\n     2\tsecond\n".into(), 0));
    assert_eq!(blocks[1], blocks[0]);
    assert_eq!(blocks[2].1, 3);
}

#[test]
fn show_clamps_line_range_to_the_last_disk_line() {
    assert_eq!(
        read("src/lib.rs:209-211", false, false),
        (
            "src/lib.rs:209-210\n   209\t  line 209\n   210\t  line 210\n".into(),
            0
        )
    );
    for argument in ["src/lib.rs:211-212", "src/lib.rs:0-2", "src/lib.rs:2-1"] {
        assert_eq!(
            read(argument, false, false),
            (format!("{argument}  line-outside  file has 210 lines\n"), 5)
        );
    }
}

#[test]
fn show_clamps_symbol_and_position_ranges_from_unsaved_outlines() {
    let mut nodes = nodes();
    nodes[2].range[1] = 215;
    nodes[2].extent.end.line = 214;
    for argument in [
        "src/lib.rs::child ~208",
        "src/lib.rs:208:4",
        "src/lib.rs:208",
    ] {
        let (anchor, position) = parse_argument(argument).unwrap();
        for dirty in [false, true] {
            let marker = if dirty { "  (unsaved in editor)" } else { "" };
            assert_eq!(
                render_item(
                    &anchor,
                    position,
                    Path::new("src/lib.rs"),
                    &nodes,
                    &source(),
                    dirty,
                    false
                ),
                (
                    format!(
                        "src/lib.rs:208-210{marker}\n   208\t  line 208\n   209\t  line 209\n   210\t  line 210\n"
                    ),
                    0
                )
            );
        }
    }
    let (anchor, position) = parse_argument("src/lib.rs:211").unwrap();
    assert_eq!(
        render_item(
            &anchor,
            position,
            Path::new("src/lib.rs"),
            &nodes,
            &source(),
            false,
            false
        ),
        (
            "src/lib.rs:211  line-outside  file has 210 lines\n".into(),
            5
        )
    );
}

#[test]
fn show_zoom_omits_children_past_the_clamped_range() {
    let mut nodes = nodes();
    nodes[0].range[1] = 215;
    let mut outside = nodes[1].clone();
    outside.range = [211, 215];
    outside.selection.start.line = 210;
    nodes.push(outside);
    assert_eq!(
        render_item(&parse_argument("src/lib.rs::Parent").unwrap().0, None, Path::new("src/lib.rs"), &nodes, &source(), false, false),
        ("src/lib.rs:1-210  (outline: 210 lines; --full prints the body)\n     4\tline 4 (3-5)\n".into(), 0)
    );
}

#[test]
fn show_reports_missing_and_outside() {
    let argument = "src/lib.rs::Parent::chld";
    let (anchor, _) = parse_argument(argument).unwrap();
    let checked = check_symbol(anchor, PathBuf::from("src/lib.rs"), &nodes());
    assert_eq!(checked.detail, "class Parent is at 1-205");
    assert_eq!(
        read(argument, false, false),
        (
            format!("{argument}  missing-symbol  {}\n", checked.detail),
            5
        )
    );
    assert_eq!(
        read("src/lib.rs::child ~4", false, false).0,
        read("src/lib.rs:208-209", false, false).0,
        "a hint does not select a nested namesake of a top-level item"
    );
    for argument in [
        "src/lib.rs::absent",
        "src/lib.rs:210:50",
        "src/lib.rs:211-212",
        "src/lib.rs:0-2",
        "src/lib.rs:0",
        "src/lib.rs:211",
    ] {
        assert_eq!(read(argument, false, false).1, 5, "{argument}");
    }
}

#[test]
fn show_prints_all_hits_unless_a_hint_selects_a_subset() {
    let mut nodes = nodes();
    nodes[2].path = vec!["Other".into(), "child".into()];
    let read = |argument| {
        let (anchor, position) = parse_argument(argument).unwrap();
        render_item(
            &anchor,
            position,
            Path::new("src/lib.rs"),
            &nodes,
            &source(),
            false,
            false,
        )
    };
    let first = read("src/lib.rs::Parent::child").0;
    let second = read("src/lib.rs::Other::child").0;
    for argument in [
        "src/lib.rs::child",
        "src/lib.rs::child ~100",
        "src/lib.rs::child ~4-208",
    ] {
        assert_eq!(read(argument), (format!("{first}\n{second}"), 0));
    }
    assert_eq!(read("src/lib.rs::child ~4"), (first, 0));
}

#[test]
fn show_type_prints_struct_and_impl_in_outline_order() {
    let mut nodes = nodes();
    nodes.remove(0);
    nodes[0].name = "Type".into();
    nodes[0].path = vec!["Type".into()];
    nodes[0].kind = "struct";
    nodes[0].parent = None;
    nodes[1].name = "impl Type".into();
    nodes[1].path = vec!["impl Type".into()];
    for (argument, spans) in [
        ("src/lib.rs::Type", vec!["3-5", "208-209"]),
        ("src/lib.rs::Type ~4", vec!["3-5"]),
    ] {
        let (text, exit) = render_item(
            &parse_argument(argument).unwrap().0,
            None,
            Path::new("src/lib.rs"),
            &nodes,
            &source(),
            false,
            false,
        );
        assert_eq!(exit, 0);
        let headers: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("src/lib.rs:"))
            .collect();
        assert_eq!(headers, spans);
    }
}

fn rust_servers() -> BTreeMap<String, crate::config::LspServerConfig> {
    BTreeMap::from([(
        "rust".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["rs"], "root-markers": []
        }))
        .unwrap(),
    )])
}

/// Show `arguments` over an outlined `src/lib.rs`, answering name lookups with `define`.
fn show_named(
    folder: &str,
    arguments: &[&str],
    mut define: impl FnMut(&Path, &str) -> Result<query::Output, query::QueryErr>,
) -> Vec<(String, i32)> {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), source()).unwrap();
    if !folder.is_empty() {
        // A same-named file under the caller's folder, which a typed path would read first.
        std::fs::create_dir_all(root.join(folder).join("src")).unwrap();
        std::fs::write(root.join(folder).join("src/lib.rs"), "other\n".repeat(8)).unwrap();
    }
    let servers = rust_servers();
    let mut context = Context {
        checkout: root,
        entries: &[],
        servers: &servers,
        server: None,
        files: vec!["src/lib.rs".into()],
        outlines: BTreeMap::from([(
            PathBuf::from("src/lib.rs"),
            FileOutline {
                nodes: nodes(),
                dirty: false,
            },
        )]),
        failures: BTreeMap::new(),
        usage: None,
    };
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| (*argument).into())
        .collect();
    let parsed = parse_arguments(&arguments).unwrap();
    read_arguments(
        &mut context,
        &arguments,
        parsed,
        Path::new(folder),
        false,
        |context, name| define(context.checkout, name),
    )
}

fn definition(root: &Path, line: u32, character: u32) -> query::Output {
    let position = json!({"line": line, "character": character});
    query::Output::Answer {
        result: json!([{
            "uri": format!("file://{}/src/lib.rs", root.display()),
            "range": {"start": position, "end": position}
        }]),
        document_uri: None,
    }
}

#[test]
fn show_names_read_the_block_of_their_definition_position() {
    let by_position = show_named("", &["src/lib.rs:4:4"], |_, name| {
        panic!("looked up {name}")
    });
    assert_eq!(
        by_position,
        [(
            "src/lib.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n".into(),
            0
        )]
    );
    let mut asked = Vec::new();
    let by_name = show_named("", &["Parent::child"], |root, name| {
        asked.push(name.to_owned());
        Ok(definition(root, 3, 3))
    });
    assert_eq!(
        (by_name, asked),
        (by_position.clone(), vec!["Parent::child".to_owned()])
    );
    let from_subfolder = show_named("sub", &["Parent::child"], |root, _| {
        Ok(definition(root, 3, 3))
    });
    assert_eq!(from_subfolder, by_position);
}

#[test]
fn show_name_outcomes_keep_their_exit_beside_other_blocks() {
    let twins = |root: &Path| {
        let symbols = ["a", "b"].map(|file| {
            json!({"name": "twin", "kind": 12, "location": {
                "uri": format!("file://{}/src/{file}.rs", root.display()),
                "range": {"start": {"line": 0, "character": 3},
                          "end": {"line": 0, "character": 7}}}})
        });
        query::Output::Ambiguous {
            name: "twin".into(),
            symbols: serde_json::from_value(json!(symbols)).unwrap(),
            unresolved: 0,
        }
    };
    let mut ambiguous = String::new();
    let blocks = show_named(
        "",
        &["missing", "src/lib.rs:4", "twin", "empty"],
        |root, name| {
            Ok(match name {
                "missing" => query::Output::NotFound {
                    name: name.into(),
                    symbols: Vec::new(),
                    unresolved: 0,
                    listed_symbols: 0,
                },
                "twin" => {
                    ambiguous = query::render_outcome(root, &twins(root), false).unwrap();
                    twins(root)
                }
                _ => query::Output::Answer {
                    result: serde_json::Value::Null,
                    document_uri: None,
                },
            })
        },
    );
    assert!(ambiguous.starts_with("ambiguous: 2 symbols named twin"));
    assert_eq!(
        blocks,
        [
            ("not found: missing\n".into(), 5),
            (
                "src/lib.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n".into(),
                0
            ),
            (ambiguous, 6),
            ("empty  missing-symbol  no definition found\n".into(), 5),
        ]
    );
}

#[test]
fn show_refuses_text_that_is_neither_an_anchor_nor_a_name() {
    let show_all = |arguments: &[&str]| {
        show(
            &arguments
                .iter()
                .map(|argument| (*argument).into())
                .collect::<Vec<_>>(),
            false,
            Path::new("/does-not-exist"),
            Path::new(""),
            &[],
            &BTreeMap::new(),
            None,
            None,
        )
    };
    for argument in ["a b", "src/lib.rs:12:batch", "Store::", "::open", "1abc"] {
        assert_eq!(
            show_all(&["src/lib.rs::Parent", argument])
                .unwrap_err()
                .to_string(),
            format!(
                "language-server protocol: invalid anchor {argument}; use path::Symbol, a \
                 symbol name, path:line, path:line:col, or path:start-end"
            ),
        );
    }
    assert!(show_all(&["Store::open", "batch", "lsp::query::Target"]).is_ok());
}

#[test]
fn show_validates_the_whole_batch_before_reading() {
    assert!(
        show(
            &["src/lib.rs::Parent".into(), "not-an-anchor".into()],
            false,
            Path::new("/does-not-exist"),
            Path::new(""),
            &[],
            &BTreeMap::new(),
            None,
            None,
        )
        .is_err()
    );
}
