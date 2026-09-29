use super::*;
use serde_json::json;

fn anchor(text: &str) -> Anchor {
    extract(&format!("`{text}`")).pop().unwrap()
}

fn node(
    name: &str,
    kind: u32,
    start: u32,
    end: u32,
    children: serde_json::Value,
) -> serde_json::Value {
    let range = json!({"start":{"line":start,"character":0},"end":{"line":end,"character":1}});
    json!({"name":name,"kind":kind,"range":range,"selectionRange":range,"children":children})
}

fn symbols() -> Vec<Candidate> {
    outline(json!([
        node("Type", 23, 0, 2, json!([node("field", 8, 1, 1, json!([]))])),
        node(
            "impl Type<'a>",
            19,
            9,
            30,
            json!([node("method", 6, 10, 15, json!([]))])
        ),
        node(
            "impl std::fmt::Trait for Type<T>",
            19,
            39,
            60,
            json!([node("second", 6, 40, 45, json!([]))])
        ),
        node(
            "tests",
            2,
            70,
            80,
            json!([node("helper", 12, 72, 75, json!([]))])
        ),
        node("load", 12, 90, 95, json!([])),
        node(
            "impl SeatLoader<'_>",
            19,
            99,
            120,
            json!([node("load", 6, 100, 105, json!([]))])
        ),
        node(
            "Foo",
            5,
            130,
            150,
            json!([node("bar", 6, 132, 140, json!([]))])
        )
    ]))
    .unwrap()
}

#[test]
fn pasted_definition_span_is_a_line_anchor() {
    let anchors = extract("`src/lib.rs:124:12 (120-184)`");
    assert_eq!(anchors.len(), 1, "pasted definition must be recognized");
    let parsed = &anchors[0];
    assert_eq!(parsed.symbol, None);
    assert_eq!(parsed.hint, Some([120, 184]));
    assert_eq!(anchor("src/lib.rs:124:12").hint, Some([124, 124]));
}

#[test]
fn sample_anchors_match_the_hand_read_grammar() {
    let text = include_str!("../../../tests/fixtures/lsp-check/explore-sample.md");
    let expected = [
        (
            7,
            "crates/rimz/src/cli/room/start_notice.rs",
            "report_start_notices",
            None,
        ),
        (7, "cli/room/mod.rs", "run", None),
        (8, "start_notice.rs", "broken_config_notice_for", None),
        (8, "config.rs", "ConfigErr", None),
        (
            9,
            "crates/rimz/src/cli/render/mod.rs",
            "definition_notice",
            None,
        ),
        (9, "start_notice.rs", "broken_config_notice_for", None),
        (9, "cli/doctor/render.rs", "render_machine_config", None),
        (
            10,
            "crates/rimz/src/config.rs",
            "broken_machine_files",
            None,
        ),
        (10, "start_notice.rs", "report_start_notices", None),
        (10, "cli/doctor.rs", "collect_machine_config", None),
        (11, "config.rs", "ConfigErr::validation_message", None),
        (12, "config.rs", "ConfigNotices::definition_errors", None),
        (12, "config.rs", "DefinitionError", None),
        (12, "config/definitions/mod.rs", "DefinitionErr", None),
        (12, "harness/plan/tests.rs", "", Some([407, 407])),
        (12, "cli/agents_cmd/tests.rs", "", Some([2495, 2495])),
        (
            13,
            "crates/rimz/src/config/definitions/mod.rs",
            "DefinitionErr",
            None,
        ),
        (14, "definitions/mod.rs", "LoadedDefinitions::failed", None),
        (
            16,
            "definitions/agent.rs",
            "Resolver::resolve_one",
            Some([169, 169]),
        ),
        (
            18,
            "definitions/team.rs",
            "SeatLoader::load",
            Some([276, 276]),
        ),
        (18, "definitions/tests.rs", "", Some([581, 581])),
        (19, "definitions/agent.rs", "skills", Some([467, 467])),
        (19, "config/skills.rs", "SkillName", None),
        (
            20,
            "crates/rimz/src/cli/doctor.rs",
            "collect_machine_config",
            None,
        ),
        (20, "doctor/render.rs", "render_machine_config", None),
        (
            20,
            "doctor/render/tests.rs",
            "machine_config_definition_problem_names_launch_precondition",
            None,
        ),
        (20, "doctor/render.rs", "detail", None),
        (
            21,
            "crates/rimz/src/cli/agents_cmd/validate.rs",
            "run",
            None,
        ),
        (21, "validate.rs", "load", None),
        (
            22,
            "crates/rimz/src/cli/mod.rs",
            "report_definition_errors",
            None,
        ),
        (23, "cli/render/mod.rs", "home_relative", None),
        (24, "docs/guide/configuration.md", "", Some([96, 96])),
        (36, "cli/room/mod.rs", "run", None),
    ];
    let anchors = extract(text);
    let actual: Vec<_> = anchors
        .iter()
        .map(|a| {
            (
                a.line,
                a.path.as_str(),
                a.symbol.as_ref().map(|s| s.join("::")).unwrap_or_default(),
                a.hint,
            )
        })
        .collect();
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(l, p, s, h)| (l, p, s.to_owned(), h))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn extractor_handles_decorations_hints_and_excludes_nonanchors() {
    let text = "`config.rs::ConfigErr::Definition(DefinitionErr)`\n`a.rs::Type<T>::method()` (~12-14)\n`a.rs::Type.field: Value`\n`a.rs::f:9:2`\n`a.rs:~7-8`\n`a.rs::` `Type::method` `config::Error` `a.rs` `cargo test 'a.rs::f'` `127.0.0.1:8080` `v1.2:3`\n```rust\n`a.rs::fake`\n```\n";
    let anchors = extract(text);
    assert_eq!(anchors.len(), 5);
    assert_eq!(
        anchors[0].symbol.as_ref().unwrap(),
        &["ConfigErr", "Definition"]
    );
    assert_eq!(anchors[1].symbol.as_ref().unwrap(), &["Type", "method"]);
    assert_eq!(anchors[1].hint, Some([12, 14]));
    assert!(anchors[1].text.ends_with("(~12-14)"));
    assert_eq!(anchors[2].symbol.as_ref().unwrap(), &["Type", "field"]);
    assert_eq!(anchors[3].hint, Some([9, 9]));
    assert_eq!(anchors[4].hint, Some([7, 8]));
    for hint in [
        "(~12)", "(12)", "(~12-14)", "(12-14)", "~12", ":~12", ":12", ":12:3",
    ] {
        assert_eq!(
            extract(&format!("`a.rs::f` {hint}"))[0].hint.unwrap()[0],
            12
        );
        assert_eq!(anchor(&format!("a.rs::f {hint}")).hint.unwrap()[0], 12);
    }
    assert!(extract("```md\n`a.rs::f`\n```").is_empty());
    assert!(
        !extract(include_str!(
            "../../../tests/fixtures/lsp-check/plan-sample.md"
        ))
        .is_empty()
    );
}

#[test]
fn repo_qualifiers_wrap_local_anchor_grammar() {
    for (text, qualifier, path, symbol, hint) in [
        (
            "ghostty-org/ghostty@v1.3.1:src/terminal/kitty/graphics_storage.zig::ImageStorage.delete",
            "ghostty-org/ghostty@v1.3.1",
            "src/terminal/kitty/graphics_storage.zig",
            Some(vec!["ImageStorage", "delete"]),
            None,
        ),
        (
            "kovidgoyal/kitty@c73326a:kitty/graphics.c:~120",
            "kovidgoyal/kitty@c73326a",
            "kitty/graphics.c",
            None,
            Some([120, 120]),
        ),
        (
            "kovidgoyal/kitty@c73326a:kitty/graphics.c::filter_refs",
            "kovidgoyal/kitty@c73326a",
            "kitty/graphics.c",
            Some(vec!["filter_refs"]),
            None,
        ),
        (
            "o/r@release/1.2:a.rs:~3",
            "o/r@release/1.2",
            "a.rs",
            None,
            Some([3, 3]),
        ),
        (
            "o/r@v1:a.rs::Type<T>::f() (~12)",
            "o/r@v1",
            "a.rs",
            Some(vec!["Type", "f"]),
            Some([12, 12]),
        ),
    ] {
        let extracted = extract(&format!("`{text}`"));
        assert_eq!(extracted.len(), 1, "{text}");
        let actual = &extracted[0];
        assert_eq!(actual.qualifier.as_deref(), Some(qualifier));
        assert_eq!(actual.path, path);
        assert_eq!(
            actual.symbol,
            symbol.map(|s| s.into_iter().map(str::to_owned).collect())
        );
        assert_eq!(actual.hint, hint);
        assert_eq!(actual.text, text);
    }
    let trailing = extract("`o/r@v1:a.zig::f` (~12)");
    assert_eq!(trailing.len(), 1);
    assert_eq!(trailing[0].hint, Some([12, 12]));
    assert_eq!(trailing[0].text, "o/r@v1:a.zig::f (~12)");
    for text in [
        "o/r@v1",
        "o/r@v1:",
        "o/r@:a.rs::f",
        "a/b/c@v1:a.rs::f",
        "o/r@v1:notapath",
        "o/r@v 1:a.rs::f",
        "o!/r@v1:a.rs::f",
    ] {
        assert!(extract(&format!("`{text}`")).is_empty(), "{text}");
    }
    assert_eq!(anchor("node_modules/@types/x.ts::T").qualifier, None);
}

#[test]
fn external_only_report_needs_no_server_or_checkout_path() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    let notes = root.path().join("notes.md");
    let text = "o/r@v1:absent.rs::f (~12)";
    std::fs::write(&notes, format!("`{text}`")).unwrap();
    let report = run(&notes, root.path(), &[], &BTreeMap::new()).unwrap();
    assert_eq!(report.exit_code(), 0);
    let rendered = report.render(false).unwrap();
    assert_eq!(rendered.lines().count(), 1);
    assert!(rendered.ends_with("0 ok, 0 failed, 0 unchecked, 1 external\n"));
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert_eq!(
        value["summary"],
        json!({"anchors":1,"ok":0,"failed":0,"unchecked":0,"external":1})
    );
    assert_eq!(
        value["anchors"][0],
        json!({"line":1,"text":text,"status":"external","path":null,"symbol":["f"],"hint":[12,12],"range":null,"candidates":[],"files":[],"detail":"o/r@v1"})
    );
}

#[test]
fn hint_markers_distinguish_locations_from_ordinary_numbers() {
    for text in ["`a.rs::f` 12 callers", "`a.rs::f 12 callers`"] {
        let anchors = extract(text);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].hint, None);
    }
    for text in ["`a.rs:(12)`", "`a.rs:12items`", "`a.rs: 12`"] {
        assert!(extract(text).is_empty(), "{text}");
    }
}

#[test]
fn numeric_symbol_segments_stay_distinct_from_line_hints() {
    for name in ["Tuple::0", "Tuple.0"] {
        let extracted = anchor(&format!("a.rs::{name}"));
        assert_eq!(extracted.symbol.unwrap(), ["Tuple", "0"]);
        assert_eq!(extracted.hint, None);
    }
    assert_eq!(anchor("a.rs::Tuple::0:12").hint, Some([12, 12]));
}

#[test]
fn paths_use_exact_then_component_suffix_and_reject_outside_root() {
    let files = [
        "src/a.rs",
        "one/definitions/mod.rs",
        "two/mod.rs",
        "config.rs",
        "other/config.rs",
    ]
    .map(PathBuf::from);
    let root = Path::new("/abs/root");
    for p in ["src/a.rs", "./src/a.rs", "/abs/root/src/a.rs"] {
        assert_eq!(resolve(root, &files, p), vec![PathBuf::from("src/a.rs")]);
    }
    assert_eq!(
        resolve(root, &files, "config.rs"),
        vec![PathBuf::from("config.rs")]
    );
    assert_eq!(
        resolve(root, &files, "definitions/mod.rs"),
        vec![PathBuf::from("one/definitions/mod.rs")]
    );
    assert_eq!(resolve(root, &files, "mod.rs").len(), 2);
    for p in ["onfig.rs", "/outside/src/a.rs", "../src/a.rs"] {
        assert!(resolve(root, &files, p).is_empty());
    }
}

#[test]
fn outlines_match_language_neutral_ancestor_subsequences() {
    let nodes = symbols();
    for name in [
        "Type::method",
        "method",
        "Type::second",
        "tests::helper",
        "Type.field",
        "Type::field",
        "Foo.bar",
        "Foo::bar",
    ] {
        assert_eq!(
            check_symbol(anchor(&format!("a.rs::{name}")), "a.rs".into(), &nodes).status,
            Status::Ok,
            "{name}"
        );
    }
    for line in [93, 103] {
        assert_eq!(
            check_symbol(
                anchor(&format!("a.rs::load (~{line})")),
                "a.rs".into(),
                &nodes
            )
            .status,
            Status::Ok
        );
    }
    assert_eq!(
        check_symbol(anchor("a.rs::fmt::second"), "a.rs".into(), &nodes).status,
        Status::MissingSymbol
    );
    let missing = check_symbol(anchor("a.rs::Other::load"), "a.rs".into(), &nodes);
    assert_eq!(missing.status, Status::MissingSymbol);
    assert_eq!(missing.candidates.len(), 2);
    assert_eq!(missing.candidates[1].name, "impl SeatLoader<'_>::load");
    let flat = outline(json!([{"name":"method","kind":6,"containerName":"outer.Type","location":{"uri":"file:///a.rs","range":{"start":{"line":3,"character":0},"end":{"line":4,"character":1}}}}])).unwrap();
    assert_eq!(
        check_symbol(anchor("a.rs::Type::method"), "a.rs".into(), &flat).status,
        Status::Ok
    );
    let module = outline(json!([node("tests", 2, 0, 0, json!([]))])).unwrap();
    let missing = check_symbol(anchor("a.rs::tests::helper"), "a.rs".into(), &module);
    assert_eq!(missing.status, Status::MissingSymbol);
    assert_eq!(missing.candidates[0].name, "tests");
    assert_eq!(missing.candidates[0].kind, "module");
}

#[test]
fn hints_accept_three_lines_of_slack_and_report_actual_ranges() {
    let nodes = symbols();
    for (line, status) in [
        (7, Status::LineOutside),
        (8, Status::Ok),
        (19, Status::Ok),
        (20, Status::LineOutside),
    ] {
        let verdict = check_symbol(
            anchor(&format!("a.rs::Type::method (~{line})")),
            "a.rs".into(),
            &nodes,
        );
        assert_eq!(verdict.status, status);
        if status == Status::LineOutside {
            assert_eq!(verdict.candidates[0].range, [11, 16]);
        }
    }
    assert_eq!(
        check_lines(anchor("a.rs:12"), "a.rs".into(), 12).status,
        Status::Ok
    );
    let outside = check_lines(anchor("a.rs:13"), "a.rs".into(), 12);
    assert_eq!(outside.status, Status::LineOutside);
    assert!(outside.detail.contains("12 lines"));
    assert_eq!(
        check_lines(anchor("a.rs:0"), "a.rs".into(), 12).status,
        Status::LineOutside
    );
    assert_eq!(
        check_lines(anchor("a.rs:12-11"), "a.rs".into(), 12).status,
        Status::LineOutside
    );
}

#[test]
fn reports_include_all_json_keys_but_only_non_ok_text_lines() {
    let mut unchecked = check_lines(anchor("a.py:1"), "a.py".into(), 1);
    unchecked.status = Status::Unchecked;
    let report = Report::new(
        Path::new("notes.md"),
        Path::new("/root"),
        vec![check_lines(anchor("a.rs:1"), "a.rs".into(), 1), unchecked],
    );
    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.render(false).unwrap().lines().count(), 2);
    let mut anchors = report.anchors;
    anchors.push(check_lines(anchor("a.rs:2"), "a.rs".into(), 1));
    let report = Report::new(Path::new("notes.md"), Path::new("/root"), anchors);
    assert_eq!(report.exit_code(), 7);
    let text = report.render(false).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(text.ends_with("3 anchors in notes.md: 1 ok, 1 failed, 1 unchecked, 0 external\n"));
    assert!(text.contains("notes.md:1  line-outside  a.rs:2  "));
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert_eq!(value["anchors"].as_array().unwrap().len(), 3);
    let keys: Vec<_> = value["anchors"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "candidates",
            "detail",
            "files",
            "hint",
            "line",
            "path",
            "range",
            "status",
            "symbol",
            "text"
        ]
    );
    assert_eq!(
        value["summary"],
        json!({"anchors":3,"ok":1,"failed":1,"unchecked":1,"external":0})
    );
    assert_eq!(
        Report::new(Path::new("empty.md"), Path::new("/root"), vec![]).exit_code(),
        0
    );
}
