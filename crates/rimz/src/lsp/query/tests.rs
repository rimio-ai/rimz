use super::*;
use serde_json::json;

fn range() -> Value {
    json!({"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}})
}

#[test]
fn configured_but_absent_server_has_neutral_reason() {
    let config = serde_json::from_value(serde_json::json!({"command": ["server"], "extensions": ["rs"], "root-markers": ["Cargo.toml"]})).unwrap();
    let error = select(
        Path::new("/no-refusal-record"),
        Vec::new(),
        &BTreeMap::from([("rust".into(), config)]),
        Some("rust"),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "no language server for /no-refusal-record (not running); use grep"
    );
}

#[test]
fn missing_source_does_not_discard_locations() {
    let result = render_with_source(
        Verb::Refs,
        Path::new("/checkout"),
        None,
        json!([{"uri": "file:///checkout/gone.rs", "range": range()}]),
        Scope::Checkout,
        |_, _| Err(LspErr::Protocol("missing line".into())),
    );
    assert_eq!(result.unwrap(), "gone.rs:2:3\n");
}

#[test]
fn qualified_symbols_match_their_container() {
    let symbol = |container| json!({"name": "method", "containerName": container, "kind": 12, "location": {"uri": "file:///checkout/lib.rs", "range": range()}});
    assert!(matches!(
        resolve_symbol(
            Path::new("/checkout"),
            "Type::method",
            json!([symbol("Type"), symbol("Other")])
        )
        .unwrap(),
        SymbolResolution::Unique(_)
    ));
    assert!(matches!(
        resolve_symbol(
            Path::new("/checkout"),
            "module::Type::method",
            json!([symbol("Type"), symbol("Other")])
        )
        .unwrap(),
        SymbolResolution::Missing { .. }
    ));
    let mut located = symbol("Type<'a>");
    located["location"]["uri"] = json!("file:///checkout/module.rs");
    assert!(matches!(
        resolve_symbol(
            Path::new("/checkout"),
            "module::Type::method",
            json!([located])
        )
        .unwrap(),
        SymbolResolution::Unique(_)
    ));
}

#[test]
fn qualified_module_names_and_grammar_resolve() {
    for (written, name, file, container) in [
        (
            "launch_reminders::render",
            "render",
            "harness/launch_reminders.rs",
            None,
        ),
        (
            "harness::launch_reminders::render",
            "render",
            "harness/launch_reminders.rs",
            None,
        ),
        (
            "rimz::harness::launch_reminders::render",
            "render",
            "harness/launch_reminders.rs",
            None,
        ),
        (
            "crate::harness::launch_reminders::render",
            "render",
            "harness/launch_reminders.rs",
            None,
        ),
        (
            "lsp::resolve_symbol",
            "resolve_symbol",
            "lsp/query.rs",
            None,
        ),
        ("render()", "render", "harness/launch_reminders.rs", None),
        ("proc::user_shell", "user_shell", "proc/mod.rs", None),
        ("Type::method", "method", "store/mod.rs", Some("Type<'a>")),
        (
            "store::Type::method",
            "method",
            "store/mod.rs",
            Some("Type<'a>"),
        ),
        (
            "super::self::crate::store::Type<Vec<other::Item>>::method()",
            "method",
            "store/mod.rs",
            Some("Type<'a>"),
        ),
    ] {
        let symbol = json!({"name": name, "kind": 12, "containerName": container, "location": {"uri": format!("file:///checkout/crates/rimz/src/{file}"), "range": range()}});
        assert!(
            matches!(
                resolve_symbol(Path::new("/checkout"), written, json!([symbol])).unwrap(),
                SymbolResolution::Unique(_)
            ),
            "{written}"
        );
    }
}

fn render_ambiguous(root: &Path, name: &str, symbols: &[SymbolInformation]) -> Result<String> {
    render_outcome(
        root,
        &Output::Ambiguous {
            name: name.into(),
            symbols: symbols.to_vec(),
        },
        false,
    )
}

#[test]
fn lookup_outcomes_render_qualified_reusable_candidates() {
    let root = Path::new("/checkout");
    let missing = Output::NotFound {
        name: "nosuch".into(),
        symbols: vec![],
    };
    assert_eq!(
        render_outcome(root, &missing, false).unwrap(),
        "not found: nosuch\n"
    );
    let symbols: Vec<SymbolInformation> = serde_json::from_value(json!([
        {"name": "read", "kind": 12, "location": {"uri": "file:///checkout/src/harness/launch_env.rs", "range": range()}},
        {"name": "read", "kind": 12, "containerName": "Container<'a>", "location": {"uri": "file:///outside/lib.rs", "range": range()}}
    ])).unwrap();
    let missing = Output::NotFound {
        name: "launch_git::read".into(),
        symbols: symbols.clone(),
    };
    insta::assert_snapshot!(render_outcome(root, &missing, false).unwrap(), @"
    not found: launch_git::read; 2 symbols named read:
    function Container::read  /outside/lib.rs:2:3
    function harness::launch_env::read  src/harness/launch_env.rs:2:3
    ");
    let json: Value = serde_json::from_str(&render_outcome(root, &missing, true).unwrap()).unwrap();
    assert_eq!(json["outcome"], "not-found");
    assert_eq!(
        json["candidates"][0],
        json!({"name": "Container::read", "kind": "function", "position": "/outside/lib.rs:2:3"})
    );
    for candidate in json["candidates"].as_array().unwrap() {
        assert!(matches!(
            resolve_symbol(
                root,
                candidate["name"].as_str().unwrap(),
                serde_json::to_value(&symbols).unwrap()
            )
            .unwrap(),
            SymbolResolution::Unique(_)
        ));
    }
    for output in [
        Output::NotFound {
            name: "wrong::read".into(),
            symbols: vec![symbols[0].clone(); 21],
        },
        Output::Ambiguous {
            name: "read".into(),
            symbols: vec![symbols[0].clone(); 21],
        },
    ] {
        let text = render_outcome(root, &output, false).unwrap();
        assert_eq!(text.lines().count(), 22);
        assert!(text.ends_with("1 more; narrow with a qualifier or use find\n"));
        let json: Value =
            serde_json::from_str(&render_outcome(root, &output, true).unwrap()).unwrap();
        assert_eq!(json["candidates"].as_array().unwrap().len(), 21);
    }
}

#[test]
fn find_ranks_exact_prefix_contains_then_other() {
    let symbols = json!(["unrelated", "rework", "worker", "work"].map(|name| json!({"name": name, "kind": 12, "location": {"uri": "file:///checkout/lib.rs", "range": range()}})));
    let root = Path::new("/checkout");
    let result = rank_find(root, "WORK", symbols).unwrap();
    assert_eq!(
        result
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["work", "worker", "rework", "unrelated"]
    );
    assert_eq!(
        render(Verb::Find, root, None, result, Scope::Checkout).unwrap(),
        "function work  lib.rs:2:3\nfunction worker  lib.rs:2:3\nfunction rework  lib.rs:2:3\nfunction unrelated  lib.rs:2:3\n"
    );
}

#[test]
fn missing_qualifiers_preserve_exact_candidates_without_guessing() {
    for (written, name, file) in [
        ("launch_git::read", "read", "harness/launch_env.rs"),
        ("Store::open", "open", "store/mod.rs"),
        ("query::tests::foo", "foo", "lsp/query.rs"),
    ] {
        let symbol = json!({"name": name, "kind": 12, "location": {"uri": format!("file:///checkout/src/{file}"), "range": range()}});
        let SymbolResolution::Missing { candidates } = resolve_symbol(
            Path::new("/checkout"),
            written,
            json!([symbol.clone(), symbol]),
        )
        .unwrap() else {
            panic!("must not guess {written}")
        };
        assert_eq!(candidates.len(), 2, "{written}");
    }
    assert!(
        matches!(resolve_symbol(Path::new("/checkout"), "nosuch", json!([])).unwrap(), SymbolResolution::Missing { candidates } if candidates.is_empty())
    );
}

#[test]
fn aliases_collapse_to_definitions_without_guessing() {
    let location = |file: &str| {
        serde_json::from_value::<Location>(
            json!({"uri": format!("file:///checkout/{file}.rs"), "range": range()}),
        )
        .unwrap()
    };
    let symbols = || {
        ["alias", "original"]
            .map(|file| SymbolInformation {
                name: "work".into(),
                kind: 12,
                location: location(file),
                container_name: None,
            })
            .to_vec()
    };
    let resolved = collapse_symbols(symbols(), |_| Ok(vec![location("definition")])).unwrap();
    assert!(
        matches!(resolved, SymbolResolution::Unique(found) if found.location == location("definition"))
    );
    for unresolved in [vec![], vec![location("one"), location("two")]] {
        let resolved = collapse_symbols(symbols(), |candidate| {
            Ok(if candidate.uri.ends_with("alias.rs") {
                unresolved.clone()
            } else {
                vec![location("definition")]
            })
        })
        .unwrap();
        let SymbolResolution::Ambiguous(symbols) = resolved else {
            panic!("distinct definitions");
        };
        insta::allow_duplicates! {
            insta::assert_snapshot!(render_ambiguous(Path::new("/checkout"), "work", &symbols).unwrap(), @"
            ambiguous: 2 symbols named work; rerun with one of these names or a position
            function alias::work  alias.rs:2:3
            function original::work  original.rs:2:3
            ");
        }
    }
}

#[test]
fn incomplete_position_requires_a_column() {
    assert!(
        parse_target("src/lib.rs:12")
            .unwrap_err()
            .to_string()
            .contains("path:line:col")
    );
}

#[test]
fn collapsed_candidates_keep_names_accepted_by_workspace_search() {
    let root = Path::new("/checkout");
    let indexed = json!(["alias", "other"].map(|file| json!({"name": "work", "kind": 12, "location": {"uri": format!("file:///checkout/src/{file}.rs"), "range": range()}})));
    let symbols = serde_json::from_value(indexed.clone()).unwrap();
    let SymbolResolution::Ambiguous(symbols) = collapse_symbols(symbols, |location| {
        let mut resolved = location.clone();
        if resolved.uri.ends_with("/alias.rs") {
            resolved.uri = "file:///checkout/src/definition.rs".into();
        }
        Ok(vec![resolved])
    })
    .unwrap() else {
        panic!("two distinct definitions")
    };
    for symbol in symbols {
        let candidate = Candidate::new(root, &symbol).unwrap();
        assert!(
            matches!(
                resolve_symbol(root, &candidate.name, indexed.clone()).unwrap(),
                SymbolResolution::Unique(_)
            ),
            "{} is not an indexed name",
            candidate.name
        );
    }
}

#[test]
fn verb_text_renderers() {
    let root = Path::new("/checkout");
    let uri = "file:///checkout/src/lib.rs";
    let location = json!({"uri": uri, "range": range()});
    let show = |verb, result| {
        render_with_source(verb, root, Some(uri), result, Scope::Checkout, |_, _| {
            Ok("  fn work() {}  ".into())
        })
        .unwrap()
    };
    insta::assert_snapshot!(show(Verb::Def, location.clone()), @"src/lib.rs:2:3  fn work() {}");
    insta::assert_snapshot!(show(Verb::Refs, json!([location, location])), @"src/lib.rs:2:3  fn work() {}");
    insta::assert_snapshot!(show(Verb::Impl, json!([{"targetUri": uri, "targetSelectionRange": range()}])), @"src/lib.rs:2:3  fn work() {}");
    insta::assert_snapshot!(show(Verb::Hover, json!({"contents": [{"language": "rust", "value": "fn work()"}, "Does work."]})), @"
    ```rust
    fn work()
    ```

    Does work.
    ");
    let child = json!({"name": "work", "kind": 12, "range": range(), "selectionRange": range()});
    insta::assert_snapshot!(show(Verb::Symbols, json!([{"name": "Engine", "kind": 23, "range": range(), "selectionRange": range(), "children": [child]}])), @"
    struct Engine  src/lib.rs:2:3
      function work  src/lib.rs:2:3
    ");
    let symbol =
        json!({"name": "work", "kind": 12, "location": location, "containerName": "Engine"});
    insta::assert_snapshot!(show(Verb::Find, json!([symbol])), @"function Engine::work  src/lib.rs:2:3");
    let item = json!({"name": "work", "kind": 12, "uri": uri, "range": range(), "selectionRange": range()});
    insta::assert_snapshot!(show(Verb::Callers, json!([{"from": item}, {"from": item}])), @"work  src/lib.rs:2:3");
    insta::assert_snapshot!(show(Verb::Callees, json!([{"to": item}])), @"work  src/lib.rs:2:3");
    assert_eq!(show(Verb::Refs, json!([])), "no results\n");
    assert_eq!(
        show(
            Verb::Hover,
            json!({"contents": {"kind": "markdown", "value": "**work**"}})
        ),
        "**work**\n"
    );
    assert_eq!(
        show(Verb::Symbols, json!([symbol])),
        show(Verb::Find, json!([symbol]))
    );
}

#[test]
fn call_hierarchy_hides_distinct_external_items() {
    let item = |uri| json!({"name": "work", "kind": 12, "uri": uri, "range": range(), "selectionRange": range()});
    let inside = item("file:///checkout/lib.rs");
    let outside = item("file:///other/lib.rs");
    let other = item("file:///sdk/lib.rs");
    for verb in [Verb::Callers, Verb::Callees] {
        let key = if verb == Verb::Callers { "from" } else { "to" };
        let calls = json!([{key: inside}, {key: outside}, {key: other}, {key: outside}]);
        assert_eq!(
            render(
                verb,
                Path::new("/checkout"),
                None,
                calls.clone(),
                Scope::Checkout
            )
            .unwrap(),
            "work  lib.rs:2:3\n2 outside the checkout hidden; add --external to show them\n"
        );
        assert_eq!(
            render(verb, Path::new("/checkout"), None, calls, Scope::External).unwrap(),
            "work  /other/lib.rs:2:3\nwork  /sdk/lib.rs:2:3\nwork  lib.rs:2:3\n"
        );
        assert_eq!(
            render(
                verb,
                Path::new("/checkout"),
                None,
                json!([{key: outside}]),
                Scope::Checkout
            )
            .unwrap(),
            "no results\n1 outside the checkout hidden; add --external to show them\n"
        );
    }
}

#[test]
fn symbols_require_exact_names_and_ambiguity_is_not_a_guess() {
    let symbol = |name| json!({"name": name, "kind": 12, "location": {"uri": "file:///checkout/lib.rs", "range": range()}});
    assert!(matches!(
        resolve_symbol(Path::new("/checkout"), "work", json!([symbol("worker")])).unwrap(),
        SymbolResolution::Missing { .. }
    ));
    assert!(matches!(
        resolve_symbol(
            Path::new("/checkout"),
            "work",
            json!([symbol("worker"), symbol("work")])
        )
        .unwrap(),
        SymbolResolution::Unique(_)
    ));
    let SymbolResolution::Ambiguous(symbols) = resolve_symbol(
        Path::new("/checkout"),
        "work",
        json!([symbol("work"), symbol("work")]),
    )
    .unwrap() else {
        panic!("ambiguous symbol");
    };
    insta::assert_snapshot!(render_ambiguous(Path::new("/checkout"), "work", &symbols).unwrap(), @"
    ambiguous: 2 symbols named work; rerun with one of these names or a position
    function work  lib.rs:2:3
    function work  lib.rs:2:3
    ");
}

#[test]
fn unavailable_and_indexing_errors_preserve_skill_exit_contract() {
    assert_eq!(
        Output::Answer {
            result: Value::Null,
            document_uri: None
        }
        .exit_code(),
        0
    );
    assert_eq!(
        Output::NotFound {
            name: "missing".into(),
            symbols: vec![]
        }
        .exit_code(),
        5
    );
    assert_eq!(
        Output::Ambiguous {
            name: "work".into(),
            symbols: vec![]
        }
        .exit_code(),
        6
    );
    let reasons = [
        UnavailableReason::NoneConfigured,
        UnavailableReason::MemoryShort,
        UnavailableReason::MemoryPressure,
        UnavailableReason::Crashed,
        UnavailableReason::CheckoutRemoved,
        UnavailableReason::StoppedByHand,
        UnavailableReason::Stopped(crate::lsp::registry::StopReason::Idle),
        UnavailableReason::Stopped(crate::lsp::registry::StopReason::Evicted),
        UnavailableReason::Stopped(crate::lsp::registry::StopReason::TeamDone),
    ];
    let lines = reasons
        .into_iter()
        .map(|reason| {
            let error = QueryErr::Unavailable {
                root: "/checkout".into(),
                reason,
            };
            assert_eq!(error.exit_code(), 3);
            error.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(lines, @"
    no language server for /checkout (none configured); use grep
    no language server for /checkout (not started: memory short); use grep
    no language server for /checkout (stopped: memory pressure); use grep
    no language server for /checkout (stopped: crashed); use grep
    no language server for /checkout (stopped: checkout removed); use grep
    no language server for /checkout (stopped by hand); use grep
    no language server for /checkout (stopped: idle); use grep
    no language server for /checkout (stopped: evicted); use grep
    no language server for /checkout (stopped: team done); use grep
    ");
    let indexing = QueryErr::Indexing {
        server: "rust".into(),
        seconds: 47,
    };
    assert_eq!(indexing.exit_code(), 4);
    insta::assert_snapshot!(indexing.to_string(), @"rust still indexing after 47s; use grep for this question");
}

#[test]
fn editor_positions_are_one_based_and_symbols_are_not_guessed() {
    assert_eq!(
        parse_target("src/lib.rs:2:3").unwrap(),
        Target::Position {
            path: "src/lib.rs".into(),
            position: Position {
                line: 1,
                character: 2
            },
        }
    );
    assert_eq!(
        parse_target("MuxBackend").unwrap(),
        Target::Symbol("MuxBackend".into())
    );
    assert!(parse_target("src/lib.rs:0:1").is_err());
    assert_eq!(
        parse_target("Type::method").unwrap(),
        Target::Symbol("Type::method".into())
    );
}
