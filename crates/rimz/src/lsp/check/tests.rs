use super::*;
use serde_json::json;

#[test]
fn outline_requests_stop_after_server_failure() {
    let mut servers = BTreeMap::from([(
        "rust".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["rs", "shared"], "root-markers": []
        }))
        .unwrap(),
    )]);
    servers.insert(
        "other".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["shared"], "root-markers": []
        }))
        .unwrap(),
    );
    let entry: registry::Entry = serde_json::from_value(json!({
        "root":"/checkout", "server":"rust", "nonce":"test",
        "broker_pid":std::process::id(),
        "broker_start_token":crate::proc::process_start_token(std::process::id()).unwrap(),
        "state":"ready", "started_at_ms":0, "estimate_bytes":0,
        "settings_hash":"", "request_count":0, "peak_rss_kb":0, "leases":[]
    }))
    .unwrap();
    let mut other = entry.clone();
    other.server = "other".into();
    let entries = [entry, other];
    let mut counts = Vec::new();
    for code in [3, 4, 1] {
        let mut context = Context {
            checkout: Path::new("/checkout"),
            entries: &entries,
            servers: &servers,
            files: Vec::new(),
            outlines: BTreeMap::new(),
            failures: BTreeMap::new(),
        };
        let mut requests = 0;
        let mut errors = Vec::new();
        for file in ["first.rs", "second.rs"] {
            let error = context
                .outline_with(
                    Path::new(file),
                    || Ok(entries[0].clone()),
                    |_, requested| {
                        assert_eq!(requested, Path::new(file));
                        requests += 1;
                        Err(match code {
                            3 => query::QueryErr::Unavailable {
                                root: "/checkout".into(),
                                reason: query::UnavailableReason::Crashed,
                            },
                            4 => query::QueryErr::Indexing {
                                server: "rust".into(),
                                seconds: 110,
                            },
                            _ => LspErr::Protocol("bad outline".into()).into(),
                        })
                    },
                )
                .err()
                .unwrap();
            assert_eq!(error.exit_code(), code);
            errors.push(error.to_string());
        }
        assert_eq!(errors[0], errors[1]);
        counts.push(requests);
        let error = context
            .outline_with(
                Path::new("ambiguous.shared"),
                || Err(LspErr::Configuration("multiple language servers".into()).into()),
                |_, _| panic!("ambiguous server selection must not issue an outline request"),
            )
            .err()
            .unwrap();
        assert_eq!(error.exit_code(), 1);
    }
    assert_eq!(counts, [1, 1, 2]);
}

#[test]
fn fix_leaves_the_notes_alone_when_judging_fails() {
    let root = tempfile::tempdir().unwrap();
    let notes = root.path().join("notes.md");
    let servers = BTreeMap::from([(
        "rust".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["rs"], "root-markers": []
        }))
        .unwrap(),
    )]);
    let context = || Context {
        checkout: root.path(),
        entries: &[],
        servers: &servers,
        // `gone.txt` is listed but absent, so judging its line anchor fails.
        files: vec!["a.rs".into(), "gone.txt".into()],
        failures: BTreeMap::new(),
        outlines: BTreeMap::from([(
            "a.rs".into(),
            FileOutline {
                nodes: fixed_symbols(),
                dirty: false,
            },
        )]),
    };
    let stale = "`a.rs::Type ~4-5`";
    let source = format!("{stale} `gone.txt:1`");
    std::fs::write(&notes, &source).unwrap();
    let error = check_file(&notes, &mut context(), Mode::Fix).err().unwrap();
    assert_eq!(error.exit_code(), 1);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), source);

    std::fs::write(&notes, stale).unwrap();
    let report = check_file(&notes, &mut context(), Mode::Fix).unwrap();
    assert_eq!(report.fixes.map(|fixes| fixes.len()), Some(1));
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "`a.rs::Type ~1-3`"
    );
}

fn anchor(text: &str) -> Anchor {
    extract(&format!("`{text}`")).pop().unwrap()
}

fn rewrite_hints(source: &str, dirty: bool) -> String {
    rewrite_with(source, dirty, false).0
}

fn rewrite_with(
    source: &str,
    dirty: bool,
    hints: bool,
) -> (String, Vec<fix::Fix>, Vec<fix::Ambiguous>) {
    rewrite_in(source, vec![("a.rs", Some(fixed_symbols()), dirty)], hints)
}

fn fixed_symbols() -> Vec<Candidate> {
    let mut nodes = symbols();
    nodes[3].selection.start.line = 11;
    nodes[3].selection.start.character = 4;
    nodes
}

fn rewrite_in(
    source: &str,
    files: Vec<(&str, Option<Vec<Candidate>>, bool)>,
    hints: bool,
) -> (String, Vec<fix::Fix>, Vec<fix::Ambiguous>) {
    let servers = BTreeMap::from([(
        "rust".into(),
        serde_json::from_value(json!({
            "command": ["stub"], "extensions": ["rs"], "root-markers": []
        }))
        .unwrap(),
    )]);
    let entry: registry::Entry = serde_json::from_value(json!({
        "root":"/checkout", "server":"rust", "nonce":"test",
        "broker_pid":0, "broker_start_token":"unused",
        "state":"ready", "started_at_ms":0, "estimate_bytes":0,
        "settings_hash":"", "request_count":0, "peak_rss_kb":0, "leases":[],
        "attached":[{"pid":0,"name":"test","since_ms":0,"open":files.iter()
            .filter(|(_, _, dirty)| *dirty)
            .map(|(path, ..)| json!({
                "uri":url::Url::from_file_path(Path::new("/checkout").join(path)).unwrap().to_string(),
                "owner":true,"dirty":true
            })).collect::<Vec<_>>()}]
    })).unwrap();
    let entries = [entry];
    let mut context = Context {
        checkout: Path::new("/checkout"),
        entries: &entries,
        servers: &servers,
        files: files.iter().map(|(path, ..)| path.into()).collect(),
        failures: BTreeMap::new(),
        outlines: files
            .into_iter()
            .filter_map(|(path, nodes, dirty)| {
                nodes.map(|nodes| (path.into(), FileOutline { nodes, dirty }))
            })
            .collect(),
    };
    let rewritten = fix::rewrite(source, &mut context, hints).unwrap();
    assert_eq!(
        extract(&rewritten.0).len(),
        extract(source).len(),
        "a rewrite keeps every anchor an anchor: {}",
        rewritten.0
    );
    rewritten
}

/// Three `lib.rs` files: `two/` defines `Type::method` at 11-16, `three/`
/// (when `twice`) defines it at 201-206, and `one/` never does.
fn complete(source: &str, dirty: [bool; 3], twice: bool, hints: bool) -> (String, Vec<fix::Fix>) {
    let other = || outline(json!([node("Other", 23, 0, 2, json!([]))])).unwrap();
    let elsewhere = outline(json!([node(
        "impl Type",
        19,
        199,
        230,
        json!([node("method", 6, 200, 205, json!([]))])
    )]))
    .unwrap();
    let files = vec![
        ("one/lib.rs", Some(other()), dirty[0]),
        ("two/lib.rs", Some(fixed_symbols()), dirty[1]),
        (
            "three/lib.rs",
            Some(if twice { elsewhere } else { other() }),
            dirty[2],
        ),
    ];
    let (updated, fixes, _) = rewrite_in(source, files, hints);
    (updated, fixes)
}

#[test]
fn fix_completes_a_short_path_one_file_defines() {
    let clean = [false; 3];
    for (source, expected, hints) in [
        (
            "`lib.rs::Type::method`",
            "`two/lib.rs::Type::method`",
            false,
        ),
        (
            "`lib.rs::Type::method ~90`",
            "`two/lib.rs::Type::method ~11`",
            false,
        ),
        (
            "`lib.rs::Type::method` (~90-95)",
            "`two/lib.rs::Type::method` (~11-16)",
            true,
        ),
        (
            "`lib.rs::Type::method`",
            "`two/lib.rs::Type::method` (11-16)",
            true,
        ),
        (
            "é `` lib.rs::Type::method `` tail\r\n",
            "é `` two/lib.rs::Type::method `` tail\r\n",
            false,
        ),
        (
            "x\né ``lib.rs::Type::method()`` (~99-100)\n",
            "x\né ``two/lib.rs::Type::method()`` (~11-16)\n",
            false,
        ),
    ] {
        let (updated, fixes) = complete(source, clean, false, hints);
        assert_eq!(updated, expected, "{source}");
        assert_eq!(fixes.len(), 1, "{source}");
        let fix = &fixes[0];
        let span = |text: &str| {
            let anchor = extract(text).remove(0);
            anchor.text
        };
        assert_eq!(fix.line, source.lines().count(), "{source}");
        assert_eq!(fix.before, span(source));
        assert_eq!(fix.after, span(expected));
        let (again, fixes) = complete(expected, clean, false, hints);
        assert_eq!(again, expected);
        assert!(fixes.is_empty(), "{expected}");
    }
}

#[test]
fn fix_completes_unique_short_paths_without_a_saved_outline() {
    for (source, expected, file, nodes, dirty, hints) in [
        (
            "`lib.rs::Type::method`",
            "`src/lib.rs::Type::method`",
            "src/lib.rs",
            None,
            true,
            false,
        ),
        (
            "`lib.rs:~10`",
            "`src/lib.rs:~10`",
            "src/lib.rs",
            None,
            true,
            true,
        ),
        (
            "`design.md:~1`",
            "`docs/design.md:~1`",
            "docs/design.md",
            None,
            false,
            false,
        ),
        (
            "`lib.rs::Type::method ~90`",
            "`src/lib.rs::Type::method ~11`",
            "src/lib.rs",
            Some(fixed_symbols()),
            false,
            false,
        ),
        (
            "`lib.rs::Type::method`",
            "`src/lib.rs::Type::method` (11-16)",
            "src/lib.rs",
            Some(fixed_symbols()),
            false,
            true,
        ),
        (
            "`lib.rs::Type::method ~90`",
            "`src/lib.rs::Type::method ~90`",
            "src/lib.rs",
            Some(fixed_symbols()),
            true,
            false,
        ),
    ] {
        let files = vec![(file, nodes, dirty)];
        let (updated, fixes, _) = rewrite_in(source, files.clone(), hints);
        assert_eq!(updated, expected, "{source}");
        assert_eq!(fixes.len(), 1, "{source}");
        assert_eq!(fixes[0].before, extract(source)[0].text);
        assert_eq!(fixes[0].after, extract(expected)[0].text);
        let (again, fixes, _) = rewrite_in(expected, files, hints);
        assert_eq!(again, expected);
        assert!(fixes.is_empty(), "{expected}");
    }
    for (source, file) in [
        ("`lib.rs::Type::method\n`", "src/lib.rs"),
        ("`design.md:~1`", "docs/my notes/design.md"),
        ("`colon.txt:1`", "a:b/colon.txt"),
    ] {
        let (updated, fixes, _) = rewrite_in(source, vec![(file, None, true)], true);
        assert_eq!(updated, source);
        assert!(fixes.is_empty(), "{source}");
    }
}

#[test]
fn fix_leaves_a_short_path_no_one_file_settles() {
    let clean = [false; 3];
    for (source, dirty, twice) in [
        // Two defining files, even when the hint overlaps only one of them.
        ("`lib.rs::Type::method`", clean, true),
        ("`lib.rs::Type::method ~11`", clean, true),
        // No defining file.
        ("`lib.rs::absent`", clean, false),
        // No symbol to decide by.
        ("`lib.rs:~10`", clean, false),
        // An unsaved candidate, defining or not.
        ("`lib.rs::Type::method`", [false, true, false], false),
        ("`lib.rs::Type::method`", [true, false, false], false),
        // Qualified and unmappable spans.
        ("`o/r@v1:lib.rs::Type::method`", clean, false),
        ("`lib.rs::Type::method\n`", clean, false),
    ] {
        for hints in [false, true] {
            let (updated, fixes) = complete(source, dirty, twice, hints);
            assert_eq!(updated, source);
            assert!(fixes.is_empty(), "{source}");
        }
    }
}

fn insert_hints(source: &str) -> String {
    rewrite_with(source, false, true).0
}

#[test]
fn fix_hints_insert_the_range_of_the_one_named_item() {
    for (source, expected) in [
        ("`a.rs::Type::method`", "`a.rs::Type::method` (11-16)"),
        ("`a.rs::Type`", "`a.rs::Type` (1-3)"),
        ("`a.rs::Type::field`", "`a.rs::Type::field` (2)"),
    ] {
        assert_eq!(rewrite_hints(source, false), source);
        let (updated, fixes, ambiguous) = rewrite_with(source, false, true);
        assert_eq!(updated, expected);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].line, 1);
        assert_eq!(fixes[0].before, source.trim_matches('`'));
        assert_eq!(fixes[0].after, expected.replace('`', ""));
        assert!(ambiguous.is_empty());
    }
    let source = "x\n`a.rs::load` and `a.rs::Type`";
    let (updated, fixes, ambiguous) = rewrite_with(source, false, true);
    assert_eq!(updated, "x\n`a.rs::load` and `a.rs::Type` (1-3)");
    assert_eq!(fixes.len(), 1);
    assert_eq!(ambiguous.len(), 1);
    assert_eq!(
        (ambiguous[0].line, ambiguous[0].text.as_str()),
        (2, "a.rs::load")
    );
    let names: Vec<_> = ambiguous[0]
        .candidates
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(names, ["load", "impl SeatLoader<'_>::load"]);
    assert_eq!(
        ambiguous[0].detail,
        "function load is at 91-96; method impl SeatLoader<'_>::load is at 101-106"
    );
}

#[test]
fn fix_hints_leave_ineligible_anchors_and_refresh_hinted_ones() {
    assert_eq!(rewrite_with("`a.rs::Type`", true, true).0, "`a.rs::Type`");
    let source =
        "`o/r@v1:a.rs::Type` `a.rs:99` `a.rs::absent` `a.rs::Type::method\n` `gone.rs::Type`";
    let (updated, fixes, ambiguous) = rewrite_with(source, false, true);
    assert_eq!(updated, source);
    assert!(fixes.is_empty() && ambiguous.is_empty());
    for source in [
        "`a.rs::Type::method ~90`",
        "`a.rs::Type::method` (~11-16)",
        "`a.rs::Type ~4-5`",
        "`a.rs::load ~93`",
        "`a.rs::load ~200`",
        "``a.rs::Type::method()`` (~99-100)",
    ] {
        let (updated, fixes, ambiguous) = rewrite_with(source, false, true);
        assert_eq!(updated, rewrite_hints(source, false), "{source}");
        assert_eq!(fixes.len(), usize::from(updated != source));
        assert!(ambiguous.is_empty());
    }
}

#[test]
fn fix_hints_land_after_the_closing_delimiter_and_read_back() {
    for (source, expected) in [
        (
            "é `a.rs::Type::method` tail\r\n",
            "é `a.rs::Type::method` (11-16) tail\r\n",
        ),
        (
            "é `` a.rs::Type `` tail\n",
            "é `` a.rs::Type `` (1-3) tail\n",
        ),
        ("`a.rs::Type` (see below)", "`a.rs::Type` (1-3) (see below)"),
        ("`a.rs::Type`, then", "`a.rs::Type` (1-3), then"),
        ("`a.rs::Type`(see)", "`a.rs::Type` (1-3)(see)"),
        ("end `a.rs::Type`\nnext", "end `a.rs::Type` (1-3)\nnext"),
        ("*`a.rs::Type`*", "*`a.rs::Type` (1-3)*"),
        ("**bold `a.rs::Type`**", "**bold `a.rs::Type` (1-3)**"),
        ("[`a.rs::Type`](x.md)", "[`a.rs::Type` (1-3)](x.md)"),
        (
            "| a | `a.rs::Type` |\n|---|---|\n",
            "| a | `a.rs::Type` (1-3) |\n|---|---|\n",
        ),
    ] {
        assert_eq!(insert_hints(source), expected, "{source}");
        let (again, fixes, ambiguous) = rewrite_with(expected, false, true);
        assert_eq!(again, expected);
        assert!(fixes.is_empty() && ambiguous.is_empty(), "{expected}");
        let anchors = extract(expected);
        assert!(
            anchors[0].hint.is_some() && anchors[0].hint_source.is_some(),
            "{expected}"
        );
    }
}

#[test]
fn reports_list_ambiguous_anchors_only_with_hints() {
    let nodes = symbols();
    let ambiguous = |line| fix::Ambiguous {
        line,
        text: "a.rs::load".into(),
        candidates: vec![nodes[8].clone(), nodes[10].clone()],
        detail: "function load is at 91-96".into(),
    };
    let mut report = Report::new(Path::new("notes.md"), Path::new("/root"), vec![]);
    report.fixes = Some(Vec::new());
    let text = report.render(false).unwrap();
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert!(value.get("ambiguous").is_none());
    report.ambiguous = Some(Vec::new());
    assert_eq!(report.render(false).unwrap(), text);
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert_eq!(value["ambiguous"], json!([]));
    report.ambiguous = Some(vec![ambiguous(4)]);
    assert_eq!(
        report.render(false).unwrap(),
        format!(
            "notes.md:4  ambiguous-symbol  a.rs::load  function load is at 91-96\n1 anchor left without a hint: several items match\n{text}"
        )
    );
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert_eq!(
        value["ambiguous"][0],
        json!({"line":4,"text":"a.rs::load","candidates":[{"name":"load","kind":"function","range":[91,96]},{"name":"impl SeatLoader<'_>::load","kind":"method","range":[101,106]}]})
    );
    report.ambiguous = Some(vec![ambiguous(4), ambiguous(9)]);
    assert!(
        report
            .render(false)
            .unwrap()
            .contains("notes.md:9  ambiguous-symbol  a.rs::load  function load is at 91-96\n2 anchors left without a hint: several items match\n")
    );
}

#[test]
fn fix_preserves_hint_forms_and_every_other_byte() {
    for old in [1, 12] {
        for (before, after) in [
            (format!("~{old}"), "~11"),
            (format!(":{old}"), ":11"),
            (format!("({old})"), "(11)"),
            (format!("~{old}-{}", old + 1), "~11-16"),
            (format!("({old}-{})", old + 1), "(11-16)"),
            (format!(":{old}:99"), ":12:5"),
            (format!("({old}:99)"), "(12:5)"),
        ] {
            for (source, expected) in [
                (
                    format!("é `a.rs::Type::method {before}` tail\r\n"),
                    format!("é `a.rs::Type::method {after}` tail\r\n"),
                ),
                (
                    format!("é `a.rs::Type::method` {before} tail\n"),
                    format!("é `a.rs::Type::method` {after} tail\n"),
                ),
                (
                    format!("é `` a.rs::Type::method {before} `` tail\n"),
                    format!("é `` a.rs::Type::method {after} `` tail\n"),
                ),
            ] {
                assert_eq!(rewrite_hints(&source, false), expected, "{source}");
                assert_eq!(rewrite_hints(&expected, false), expected);
            }
        }
    }
}

#[test]
fn fix_skips_dirty_ambiguous_and_unmappable_anchors() {
    let source = "`a.rs::Type::method ~90`";
    assert_eq!(rewrite_hints(source, true), source);
    let source = "`a.rs::load ~98` `a.rs::load ~200` `a.rs::absent ~99` `a.rs::Type::method` `a.rs:99` `o/r@v1:a.rs::Type::method ~99` `a.rs::Type::method\n~99`";
    assert_eq!(rewrite_hints(source, false), source);
    assert_eq!(
        rewrite_hints("``a.rs::Type::method()`` (~99-100)", false),
        "``a.rs::Type::method()`` (~11-16)"
    );
}

#[test]
fn fix_uses_the_single_hit_selected_by_a_hint() {
    for (source, expected) in [
        ("`a.rs::Type ~4-5`", "`a.rs::Type ~1-3`"),
        ("`a.rs::load ~93`", "`a.rs::load ~91`"),
        // A type hint that drifted past every item returns to the type itself.
        ("`a.rs::Type (200-210)`", "`a.rs::Type (1-3)`"),
        // A hint on one impl block keeps naming that block.
        ("`a.rs::Type ~12`", "`a.rs::Type ~10`"),
    ] {
        assert_eq!(rewrite_hints(source, false), expected);
        assert_eq!(rewrite_hints(expected, false), expected);
        assert_eq!(rewrite_hints(source, true), source);
    }
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
fn bare_ranges_after_symbol_anchors_remain_prose() {
    let anchors = extract("`a.rs::X` 2020-2024");
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].hint, None);
    assert_eq!(anchors[0].text, "a.rs::X");
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
    let text = "`config.rs::ConfigErr::Definition(DefinitionErr)`\n`a.rs::Type<T>::method()` (~12-14)\n`a.rs::Type.field: Value`\n`a.rs::f:9:2`\n`a.rs:~7-8`\n`a.rs::` `Type::method` `config::Error` `a.rs` `cargo test 'a.rs::f'` `127.0.0.1:8080` `v1.2:3` `..Default::default()` `Foo{..Default::default()}` `a..b::c` `..Default:~3` `...rest::x`\n```rust\n`a.rs::fake`\n```\n";
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
    assert_eq!(anchor("../src/a.rs::f").path, "../src/a.rs");
    assert_eq!(anchor(".../x.rs::f").path, ".../x.rs");
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
    let report = run(&[notes], root.path(), &[], &BTreeMap::new(), Mode::Check)
        .unwrap()
        .remove(0)
        .unwrap();
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
fn exact_ignored_paths_resolve_without_joining_suffix_matches() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(root.path().join(".gitignore"), "plan-notes.md\nnotes.md\n").unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    for file in ["plan-notes.md", "tracked.md", "sub/notes.md"] {
        std::fs::write(root.path().join(file), "one\ntwo\nthree\n").unwrap();
    }
    assert!(
        std::process::Command::new("git")
            .args(["add", "tracked.md"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    let notes = root.path().join("anchors.md");
    std::fs::write(
        &notes,
        "`plan-notes.md:~2`\n`tracked.md:~2`\n`plan-notes.md:~99`\n`absent.md:~1`\n`notes.md:~1`\n",
    )
    .unwrap();
    let report = run(&[notes], root.path(), &[], &BTreeMap::new(), Mode::Check)
        .unwrap()
        .remove(0)
        .unwrap();
    assert_eq!(
        report
            .anchors
            .iter()
            .map(|anchor| anchor.status)
            .collect::<Vec<_>>(),
        [
            Status::Ok,
            Status::Ok,
            Status::LineOutside,
            Status::MissingPath,
            Status::MissingPath,
        ]
    );
    assert_eq!(report.anchors[0].path, Some("plan-notes.md".into()));
    assert_eq!(report.anchors[1].path, Some("tracked.md".into()));
    assert_eq!(report.anchors[2].detail, "file has 3 lines");
    for anchor in &report.anchors[3..] {
        assert_eq!(anchor.detail, "no matching checkout file");
    }
    assert_eq!(report.exit_code(), 7);
    assert_eq!(report.summary.anchors, 5);
    assert_eq!(report.summary.ok, 2);
    assert_eq!(report.summary.failed, 3);
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
        assert_eq!(
            resolve(root, &files, p),
            PathMatch::Exact(PathBuf::from("src/a.rs"))
        );
    }
    assert_eq!(
        resolve(root, &files, "config.rs"),
        PathMatch::Exact(PathBuf::from("config.rs"))
    );
    assert_eq!(
        resolve(root, &[PathBuf::from("other/config.rs")], "config.rs"),
        PathMatch::Suffix(vec![PathBuf::from("other/config.rs")])
    );
    assert_eq!(
        resolve(root, &files, "definitions/mod.rs"),
        PathMatch::Suffix(vec![PathBuf::from("one/definitions/mod.rs")])
    );
    assert_eq!(
        resolve(root, &files, "mod.rs"),
        PathMatch::Suffix(vec!["one/definitions/mod.rs".into(), "two/mod.rs".into()])
    );
    for p in ["onfig.rs", "/outside/src/a.rs", "../src/a.rs"] {
        assert_eq!(resolve(root, &files, p), PathMatch::Suffix(Vec::new()));
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
    let mut short = verdict(anchor("b.rs:1"), Some("src/b.rs".into()), Status::ShortPath);
    short.detail = "resolves to src/b.rs".into();
    anchors.push(short);
    let report = Report::new(Path::new("notes.md"), Path::new("/root"), anchors);
    assert_eq!(report.exit_code(), 7);
    assert_eq!(report.summary.failed, 2);
    let text = report.render(false).unwrap();
    assert_eq!(text.lines().count(), 4);
    assert!(text.ends_with("4 anchors in notes.md: 1 ok, 2 failed, 1 unchecked, 0 external\n"));
    assert!(text.contains("notes.md:1  line-outside  a.rs:2  "));
    assert!(text.contains("notes.md:1  short-path  b.rs:1  resolves to src/b.rs\n"));
    let value: serde_json::Value = serde_json::from_str(&report.render(true).unwrap()).unwrap();
    assert_eq!(value["anchors"].as_array().unwrap().len(), 4);
    assert_eq!(value["anchors"][3]["status"], "short-path");
    assert_eq!(value["anchors"][3]["path"], "src/b.rs");
    assert_eq!(value["anchors"][3]["files"], json!([]));
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
        json!({"anchors":4,"ok":1,"failed":2,"unchecked":1,"external":0})
    );
    assert_eq!(
        Report::new(Path::new("empty.md"), Path::new("/root"), vec![]).exit_code(),
        0
    );
}
