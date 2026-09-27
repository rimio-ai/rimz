use super::*;
use serde_json::json;

fn range() -> Value {
    json!({"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}})
}

#[test]
fn outline_members_use_selection_positions_and_direct_children() {
    let node = |name, kind, line, children| json!({"name":name,"kind":kind,"range":range(),"selectionRange":{"start":{"line":line,"character":0},"end":{"line":line,"character":4}},"children":children});
    let outline = json!([
        node(
            "Type",
            23,
            3,
            json!([node(
                "field",
                8,
                4,
                json!([node("grandchild", 8, 5, json!([]))])
            )])
        ),
        node("impl Type", 19, 6, json!([node("method", 6, 7, json!([]))])),
        node(
            "Enum",
            10,
            8,
            json!([node(
                "Variant",
                22,
                9,
                json!([node("field", 8, 10, json!([]))])
            )])
        )
    ]);
    for (name, parent, line, member_line, qualified) in [
        ("Type", None, 3, 4, "module::Type::field"),
        (
            "Variant",
            Some("Enum"),
            9,
            10,
            "module::Enum::Variant::field",
        ),
    ] {
        let container: SymbolInformation = serde_json::from_value(json!({"name":name,"kind":23,"containerName":parent,"location":{"uri":"file:///checkout/src/module.rs","range":{"start":{"line":line,"character":0},"end":{"line":line,"character":4}}}})).unwrap();
        let members = outline_members(
            &container,
            "field",
            &serde_json::from_value(outline.clone()).unwrap(),
        );
        assert_eq!(members.len(), 1);
        assert_eq!(
            members[0].location.range.start,
            Position {
                line: member_line,
                character: 0
            }
        );
        assert!(
            render_ambiguous(Path::new("/checkout"), "field", &members)
                .unwrap()
                .contains(&format!(
                    "field {qualified}  src/module.rs:{}:1",
                    member_line + 1
                ))
        );
        assert!(matches!(
            resolve_symbol(
                Path::new("/checkout"),
                qualified,
                serde_json::to_value(&members).unwrap()
            )
            .unwrap(),
            SymbolResolution::Unique(_)
        ));
        for missing in ["grandchild", "method", "nosuch"] {
            assert!(
                outline_members(
                    &container,
                    missing,
                    &serde_json::from_value(outline.clone()).unwrap()
                )
                .is_empty()
            );
        }
        assert!(outline_members(&container, "field", &Symbols::Flat(members)).is_empty());
    }
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
        &BTreeSet::new(),
        |_, _| Err(LspErr::Protocol("missing line".into())),
    );
    assert_eq!(result.unwrap(), "gone.rs\n  2:3\n");
}

#[test]
fn dirty_locations_never_read_disk_source() {
    let dirty = BTreeSet::from(["file:///checkout/gone.rs".to_owned()]);
    for verb in [Verb::Def, Verb::Refs, Verb::Impl] {
        let mut consulted = Vec::new();
        let rendered = render_with_source(
            verb,
            Path::new("/checkout"),
            None,
            json!([{"uri": "file:///checkout/clean.rs", "range": range()}, {"uri": "file:///checkout/gone.rs", "range": range()}]),
            Scope::Checkout,
            &dirty,
            |uri, _| {
                consulted.push(uri.to_owned());
                Ok("disk source".into())
            },
        )
        .unwrap();
        assert_eq!(
            rendered,
            if verb == Verb::Def {
                "clean.rs:2:3  disk source\ngone.rs:2:3  (unsaved in editor)\n"
            } else {
                "clean.rs\n  2:3  disk source\ngone.rs\n  2:3  (unsaved in editor)\n"
            }
        );
        assert_eq!(consulted, ["file:///checkout/clean.rs"]);
    }
}

#[test]
fn document_keys_normalize_file_spellings() {
    for (left, right) in [
        ("file:///a/b%40c/u.rs", "file:///a/b@c/u.rs"),
        ("file:///a/%c3%a9.rs", "file:///a/%C3%A9.rs"),
        ("FILE:///a/u.rs", "file:///a/u.rs"),
        ("file://localhost/a/u.rs", "file:///a/u.rs"),
    ] {
        assert_eq!(DocumentKey::new(left), DocumentKey::new(right));
    }
    let distinct = [
        "file:///C:/u.rs",
        "file:///c:/u.rs",
        "untitled:x",
        "not a uri",
    ];
    for left in distinct {
        for right in distinct {
            assert_eq!(
                DocumentKey::new(left) == DocumentKey::new(right),
                left == right
            );
        }
    }
}

#[test]
fn dirty_locations_match_equivalent_uri_spellings() {
    for (dirty, location) in [
        (
            "file:///checkout/b%40c/lib.rs",
            "file:///checkout/b@c/lib.rs",
        ),
        ("file:///checkout/%c3%a9.rs", "file:///checkout/%C3%A9.rs"),
    ] {
        for verb in [Verb::Def, Verb::Refs, Verb::Impl] {
            let rendered = render_with_source(
                verb,
                Path::new("/checkout"),
                None,
                json!([{"uri": location, "range": range()}]),
                Scope::Checkout,
                &BTreeSet::from([dirty.to_owned()]),
                |_, _| panic!("dirty location must not read disk"),
            )
            .unwrap();
            assert!(rendered.contains("(unsaved in editor)"));
        }
    }
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
            unresolved: 0,
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
        unresolved: 0,
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
        unresolved: 0,
        name: "launch_git::read".into(),
        symbols: symbols.clone(),
    };
    insta::assert_snapshot!(render_outcome(root, &missing, false).unwrap(), @"
    not found: launch_git::read; 2 other symbols named read:
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
            unresolved: 0,
            name: "wrong::read".into(),
            symbols: vec![symbols[0].clone(); 21],
        },
        Output::Ambiguous {
            unresolved: 0,
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
        render(
            Verb::Find,
            root,
            None,
            result,
            Scope::Checkout,
            &BTreeSet::new()
        )
        .unwrap(),
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
fn not_found_candidates_collapse_re_exports_to_their_definition() {
    let symbols: Vec<SymbolInformation> = serde_json::from_value(json!([
        {"name": "Store", "kind": 23, "location": {"uri": "file:///checkout/src/lib.rs", "range": range()}},
        {"name": "Store", "kind": 23, "location": {"uri": "file:///checkout/src/store/mod.rs", "range": range()}}
    ]))
    .unwrap();
    let definition = symbols[1].location.clone();
    let symbols = collapse_candidates(symbols, |symbols| {
        Ok(vec![vec![definition.clone()]; symbols.len()])
    })
    .unwrap();
    let missing = Output::NotFound {
        unresolved: 0,
        name: "wrong::Store".into(),
        symbols,
    };
    insta::assert_snapshot!(render_outcome(Path::new("/checkout"), &missing, false).unwrap(), @"
    not found: wrong::Store; 1 other symbol named Store:
    struct store::Store  src/store/mod.rs:2:3
    ");
}

fn broad_candidates(count: usize) -> Vec<SymbolInformation> {
    serde_json::from_value(json!(
        (0..count)
            .rev()
            .map(|n| json!({
                "name": "new", "kind": 12,
                "location": {"uri": format!("file:///checkout/src/c{n:02}.rs"), "range": range()}
            }))
            .collect::<Vec<_>>()
    ))
    .unwrap()
}

#[test]
fn collapse_resolves_only_the_ranked_head_and_collapses_its_aliases() {
    let mut requested = Vec::new();
    let (resolution, unresolved) = collapse_ranked(
        Path::new("/checkout"),
        "c44::new",
        broad_candidates(45),
        |symbols| {
            requested = symbols
                .iter()
                .map(|symbol| symbol.location.uri.clone())
                .collect();
            Ok(symbols
                .iter()
                .map(|symbol| {
                    let mut location = symbol.location.clone();
                    location.uri = location.uri.replace("c00.rs", "c01.rs");
                    vec![location]
                })
                .collect())
        },
    )
    .unwrap();
    let expected: Vec<_> = std::iter::once(44)
        .chain(0..29)
        .map(|n| format!("file:///checkout/src/c{n:02}.rs"))
        .collect();
    assert_eq!(
        requested, expected,
        "only the ranked top 30 get definition requests"
    );
    assert_eq!(unresolved, 15);
    let SymbolResolution::Ambiguous(symbols) = resolution else {
        panic!("unresolved candidates stay ambiguous")
    };
    assert_eq!(symbols.len(), 29);
    assert!(
        !symbols
            .iter()
            .any(|symbol| symbol.location.uri.ends_with("c00.rs"))
    );
    let output = Output::Ambiguous {
        name: "c44::new".into(),
        symbols,
        unresolved,
    };
    let json: Value =
        serde_json::from_str(&render_outcome(Path::new("/checkout"), &output, true).unwrap())
            .unwrap();
    assert_eq!(json["total"], 44);
    assert_eq!(json["truncated"], true);
    assert_eq!(json["candidates"].as_array().unwrap().len(), 29);
}

#[test]
fn complete_collapse_can_be_unique_but_an_unresolved_tail_cannot() {
    for count in [25, 45] {
        let mut requests = 0;
        let definition = broad_candidates(1).remove(0).location;
        let (resolution, unresolved) = collapse_ranked(
            Path::new("/checkout"),
            "new",
            broad_candidates(count),
            |symbols| {
                requests = symbols.len();
                Ok(vec![vec![definition.clone()]; symbols.len()])
            },
        )
        .unwrap();
        assert_eq!(requests, count.min(30));
        if count == 25 {
            assert_eq!(unresolved, 0);
            assert!(
                matches!(resolution, SymbolResolution::Unique(symbol) if symbol.location == definition)
            );
        } else {
            assert_eq!(unresolved, 15);
            assert!(
                matches!(resolution, SymbolResolution::Ambiguous(symbols) if symbols.len() == 1)
            );
        }
    }
}

#[test]
fn ranked_requests_keep_the_original_alias_representative() {
    let symbols = broad_candidates(3);
    let definition = broad_candidates(4).remove(0).location;
    let (resolution, unresolved) =
        collapse_ranked(Path::new("/checkout"), "new", symbols, |symbols| {
            Ok(symbols
                .iter()
                .map(|symbol| {
                    vec![if symbol.location.uri.ends_with("c00.rs") {
                        symbol.location.clone()
                    } else {
                        definition.clone()
                    }]
                })
                .collect())
        })
        .unwrap();
    assert_eq!(unresolved, 0);
    let SymbolResolution::Ambiguous(symbols) = resolution else {
        panic!("two definitions")
    };
    assert!(
        symbols
            .iter()
            .any(|symbol| symbol.location.uri.ends_with("c02.rs")),
        "keep the first indexed alias, not the first ranked alias"
    );
    assert!(
        !symbols
            .iter()
            .any(|symbol| symbol.location.uri.ends_with("c01.rs"))
    );
}

#[test]
fn unresolved_candidate_counts_are_explicit_even_when_the_head_collapses() {
    let root = Path::new("/checkout");
    for output in [
        Output::NotFound {
            name: "Wrong::new".into(),
            symbols: broad_candidates(1),
            unresolved: 15,
        },
        Output::Ambiguous {
            name: "new".into(),
            symbols: broad_candidates(1),
            unresolved: 15,
        },
    ] {
        let text = render_outcome(root, &output, false).unwrap();
        assert!(text.contains("up to 16"), "{text}");
        assert!(text.ends_with("15 more; narrow with a qualifier or use find\n"));
        let json: Value =
            serde_json::from_str(&render_outcome(root, &output, true).unwrap()).unwrap();
        assert_eq!(json["total"], 16);
        assert_eq!(json["truncated"], true);
    }
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
    let resolved = collapse_symbols(symbols(), |symbols| {
        Ok(vec![vec![location("definition")]; symbols.len()])
    })
    .unwrap();
    assert!(
        matches!(resolved, SymbolResolution::Unique(found) if found.location == location("definition"))
    );
    for unresolved in [vec![], vec![location("one"), location("two")]] {
        let resolved = collapse_symbols(symbols(), |symbols| {
            Ok(symbols
                .iter()
                .map(|candidate| {
                    if candidate.location.uri.ends_with("alias.rs") {
                        unresolved.clone()
                    } else {
                        vec![location("definition")]
                    }
                })
                .collect())
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
    let SymbolResolution::Ambiguous(symbols) = collapse_symbols(symbols, |symbols| {
        Ok(symbols
            .iter()
            .map(|symbol| {
                let mut resolved = symbol.location.clone();
                if resolved.uri.ends_with("/alias.rs") {
                    resolved.uri = "file:///checkout/src/definition.rs".into();
                }
                vec![resolved]
            })
            .collect())
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
fn grouped_locations_trim_unicode_snippets_and_sort_positions() {
    let location = |uri, line| json!({"uri":uri,"range":{"start":{"line":line,"character":2},"end":{"line":line,"character":3}}});
    let text = format!("  {}é{}  ", "a".repeat(98), "z".repeat(51));
    let rendered = render_with_source(
        Verb::Refs,
        Path::new("/checkout"),
        None,
        json!([
            location("file:///outside/a.rs", 0),
            location("file:///checkout/z.rs", 3),
            location("file:///checkout/z.rs", 1)
        ]),
        Scope::External,
        &BTreeSet::new(),
        |_, _| Ok(text.clone()),
    )
    .unwrap();
    let snippet = format!("{}é…", "a".repeat(98));
    assert_eq!(
        rendered,
        format!("z.rs\n  2:3  {snippet}\n  4:3  {snippet}\n/outside/a.rs\n  1:3  {snippet}\n")
    );
}

#[test]
fn verb_text_renderers() {
    let root = Path::new("/checkout");
    let uri = "file:///checkout/src/lib.rs";
    let location = json!({"uri": uri, "range": range()});
    let show = |verb, result| {
        render_with_source(
            verb,
            root,
            Some(uri),
            result,
            Scope::Checkout,
            &BTreeSet::new(),
            |_, _| Ok("  fn work() {}  ".into()),
        )
        .unwrap()
    };
    insta::assert_snapshot!(show(Verb::Def, location.clone()), @"src/lib.rs:2:3  fn work() {}");
    insta::assert_snapshot!(show(Verb::Refs, json!([location, location])), @"
    src/lib.rs
      2:3  fn work() {}
    ");
    insta::assert_snapshot!(show(Verb::Impl, json!([{"targetUri": uri, "targetSelectionRange": range()}])), @"
    src/lib.rs
      2:3  fn work() {}
    ");
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
    insta::assert_snapshot!(show(Verb::Callers, json!([{"from": item}, {"from": item}])), @"
    src/lib.rs
      2:3  work
    ");
    insta::assert_snapshot!(show(Verb::Callees, json!([{"to": item}])), @"
    src/lib.rs
      2:3  work
    ");
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
fn list_verbs_hide_external_items() {
    for verb in [
        Verb::Refs,
        Verb::Impl,
        Verb::Find,
        Verb::Callers,
        Verb::Callees,
    ] {
        let item = |uri| {
            let location = json!({"uri": uri, "range": range()});
            let call = json!({"name": "work", "kind": 12, "uri": uri, "range": range(), "selectionRange": range()});
            match verb {
                Verb::Refs | Verb::Impl => location,
                Verb::Find => json!({"name": "work", "kind": 12, "location": location}),
                Verb::Callers => json!({"from": call}),
                Verb::Callees => json!({"to": call}),
                _ => unreachable!(),
            }
        };
        let inside = item("file:///checkout/lib.rs");
        let outside = item("file:///other/lib.rs");
        let other = item("file:///sdk/lib.rs");
        let calls = json!([inside, outside, other, outside]);
        let entry = |path| match verb {
            Verb::Refs | Verb::Impl => format!("{path}\n  2:3\n"),
            Verb::Find => format!("function work  {path}:2:3\n"),
            _ => format!("{path}\n  2:3  work\n"),
        };
        let hidden = if verb == Verb::Find { 3 } else { 2 };
        assert_eq!(
            render(
                verb,
                Path::new("/checkout"),
                None,
                calls.clone(),
                Scope::Checkout,
                &BTreeSet::new()
            )
            .unwrap(),
            format!(
                "{}{hidden} outside the checkout hidden; add --external to show them\n",
                entry("lib.rs")
            )
        );
        assert_eq!(
            render(
                verb,
                Path::new("/checkout"),
                None,
                calls,
                Scope::External,
                &BTreeSet::new()
            )
            .unwrap(),
            if verb == Verb::Find {
                format!(
                    "{}{}{}{}",
                    entry("lib.rs"),
                    entry("/other/lib.rs"),
                    entry("/sdk/lib.rs"),
                    entry("/other/lib.rs")
                )
            } else {
                format!(
                    "{}{}{}",
                    entry("lib.rs"),
                    entry("/other/lib.rs"),
                    entry("/sdk/lib.rs")
                )
            }
        );
        assert_eq!(
            render(
                verb,
                Path::new("/checkout"),
                None,
                json!([outside]),
                Scope::Checkout,
                &BTreeSet::new()
            )
            .unwrap(),
            "no results\n1 outside the checkout hidden; add --external to show them\n"
        );
    }
}

#[test]
fn find_scope_preserves_ranked_order() {
    let symbol =
        |name, uri| json!({"name": name, "kind": 12, "location": {"uri": uri, "range": range()}});
    let root = Path::new("/checkout");
    let ranked = rank_find(
        root,
        "work",
        json!([
            symbol("rework", "file:///checkout/lib.rs"),
            symbol("work", "file:///outside/lib.rs"),
            symbol("worker", "file:///checkout/lib.rs"),
            symbol("work", "file:///checkout/lib.rs")
        ]),
    )
    .unwrap();
    assert_eq!(
        render(
            Verb::Find,
            root,
            None,
            ranked.clone(),
            Scope::External,
            &BTreeSet::new()
        )
        .unwrap(),
        "function work  lib.rs:2:3\nfunction worker  lib.rs:2:3\nfunction rework  lib.rs:2:3\nfunction work  /outside/lib.rs:2:3\n"
    );
    assert_eq!(
        render(
            Verb::Find,
            root,
            None,
            ranked,
            Scope::Checkout,
            &BTreeSet::new()
        )
        .unwrap(),
        "function work  lib.rs:2:3\nfunction worker  lib.rs:2:3\nfunction rework  lib.rs:2:3\n1 outside the checkout hidden; add --external to show them\n"
    );
}

#[test]
fn single_answer_verbs_ignore_scope() {
    let location = json!({"uri": "file:///outside/lib.rs", "range": range()});
    for (verb, result, expected) in [
        (Verb::Def, location.clone(), "/outside/lib.rs:2:3\n"),
        (
            Verb::Symbols,
            json!([{"name": "work", "kind": 12, "location": location}]),
            "function work  /outside/lib.rs:2:3\n",
        ),
        (Verb::Hover, json!({"contents": "docs"}), "docs\n"),
    ] {
        for scope in [Scope::Checkout, Scope::External] {
            assert_eq!(
                render(
                    verb,
                    Path::new("/checkout"),
                    None,
                    result.clone(),
                    scope,
                    &BTreeSet::new()
                )
                .unwrap(),
                expected
            );
        }
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
            unresolved: 0,
            name: "missing".into(),
            symbols: vec![]
        }
        .exit_code(),
        5
    );
    assert_eq!(
        Output::Ambiguous {
            unresolved: 0,
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
