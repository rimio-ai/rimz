use std::collections::BTreeSet;
use std::fs;

use super::super::references::{FnRef, ItemKey, ItemRefs, References};
use super::super::sources::Source;
use super::*;

#[test]
fn diff_args_require_base_and_path_or_expect() {
    assert!(parse_args(&[]).is_err());
    assert!(parse_args(&["--base".into(), "HEAD".into()]).is_err());
    assert!(
        parse_args(&[
            "--base".into(),
            "HEAD".into(),
            "--path".into(),
            "src".into(),
            "--expect".into(),
            "pass.toml".into(),
        ])
        .is_err()
    );
    assert_eq!(
        parse_args(&[
            "--base".into(),
            "HEAD~1".into(),
            "--path".into(),
            "src".into(),
        ])
        .unwrap()
        .unwrap(),
        Args {
            input: Input::Base {
                base: "HEAD~1".to_owned(),
                path: PathBuf::from("src"),
            },
            top: 20,
            output: OutputArgs::default(),
            show_internal: false,
        }
    );
}

#[test]
fn diff_args_parse_json_sections_and_reject_unknown_sections() {
    let parsed = parse_args(&[
        "--base".into(),
        "HEAD~1".into(),
        "--path".into(),
        "src".into(),
        "--json".into(),
        "--section".into(),
        "totals,dependencies".into(),
    ])
    .unwrap()
    .unwrap();

    assert!(parsed.output.json);
    assert!(parsed.output.wants("totals"));
    assert!(parsed.output.wants("dependencies"));
    assert!(!parsed.output.wants("interface"));
    assert!(!parsed.show_internal);
    let internal = parse_args(&[
        "--base".into(),
        "HEAD~1".into(),
        "--path".into(),
        "src".into(),
        "--section".into(),
        "internal".into(),
    ])
    .unwrap()
    .unwrap();
    assert!(internal.show_internal);
    assert!(
        parse_args(&[
            "--base".into(),
            "HEAD~1".into(),
            "--path".into(),
            "src".into(),
            "--section".into(),
            "unknown".into(),
        ])
        .is_err()
    );
}

#[test]
fn assembly_delta_uses_the_per_function_maximum() {
    let base_edges = [
        reference_edge("src/a.rs", "One", Some(10)),
        reference_edge("src/a.rs", "Two", Some(10)),
        reference_edge("src/a.rs", "Three", Some(30)),
        reference_edge("src/a.rs", "Four", Some(30)),
        reference_edge("src/a.rs", "Outside", None),
    ];
    let current_edges = [
        reference_edge("src/a.rs", "One", Some(10)),
        reference_edge("src/a.rs", "Two", Some(10)),
        reference_edge("src/a.rs", "Three", Some(10)),
        reference_edge("src/a.rs", "Four", Some(10)),
        reference_edge("src/a.rs", "Outside", None),
    ];
    let paths = [PathBuf::from("src/a.rs")];
    let base = collect_reference_edges(base_edges.iter(), &paths, &[]);
    let current = collect_reference_edges(current_edges.iter(), &paths, &[]);
    let pair = ("caller".to_owned(), "target".to_owned());

    assert_eq!(base[&pair].items, current[&pair].items);
    assert_eq!(base[&pair].assembly(), 2);
    assert_eq!(current[&pair].assembly(), 4);
}

#[test]
fn assembly_folds_one_owner_type_into_one_item_like_the_dossier() {
    let target = Source::new(
        "src/target.rs",
        "pub struct Rec;\nimpl Rec {\n    pub fn new() -> Self { Self }\n    pub fn with(self) -> Self { self }\n}\npub fn open() {}\n",
    );
    let syntax = super::super::syntax::analyze_sources(&[target], &BTreeSet::new());
    let mut edges = [
        reference_edge("src/a.rs", "Rec", Some(10)),
        reference_edge("src/a.rs", "new", Some(10)),
        reference_edge("src/a.rs", "with", Some(10)),
        reference_edge("src/a.rs", "open", Some(10)),
    ];
    for (edge, line) in edges.iter_mut().zip([1, 3, 4, 6]) {
        edge.to_line = line;
    }
    let paths = [PathBuf::from("src/a.rs")];

    let unfolded = collect_reference_edges(edges.iter(), &paths, &[]);
    let folded = collect_reference_edges(edges.iter(), &paths, &syntax.files);
    let pair = ("caller".to_owned(), "target".to_owned());

    assert_eq!(unfolded[&pair].assembly(), 4);
    assert_eq!(folded[&pair].assembly(), 2);
    assert_eq!(folded[&pair].items.len(), 4);
}

#[test]
fn moved_interface_rows_are_filtered_and_sorted_by_delta_then_pair() {
    let rows = [
        interface_row("z", "target", 1, 2, false),
        interface_row("b", "target", 5, 2, false),
        interface_row("a", "target", 1, 1, true),
        interface_row("unchanged", "target", 2, 2, false),
    ];

    let moved = moved_interface_rows(&rows);

    assert_eq!(
        moved
            .iter()
            .map(|row| row.from.as_str())
            .collect::<Vec<_>>(),
        ["b", "z", "a"]
    );
}

#[test]
fn interface_line_shift_does_not_move_the_heaviest_function() {
    let paths = [PathBuf::from("src/a.rs")];
    let base_edges = [reference_edge("src/a.rs", "One", Some(10))];
    let current_edges = [reference_edge("src/a.rs", "One", Some(20))];
    let base = collect_reference_edges(base_edges.iter(), &paths, &[]);
    let current = collect_reference_edges(current_edges.iter(), &paths, &[]);

    let rows = interface_rows(&base, &current);

    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].base_heaviest, rows[0].current_heaviest);
    assert!(!rows[0].moved);
}

#[test]
fn diff_expect_requires_call_site_shrink() {
    let contract = contract(-1);
    let checks = [AssemblyCheck {
        expectation: contract.assembly[0].clone(),
        base: 4,
        current: 4,
    }];
    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            assembly: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );

    assert!(!rows[1].landed);
    assert!(rows[1].detail.contains("4 → 4"));
}

#[test]
fn diff_expect_enforces_negative_sloc_budget() {
    let contract = contract(-3);
    let checks = [AssemblyCheck {
        expectation: contract.assembly[0].clone(),
        base: 4,
        current: 2,
    }];

    let landed = |delta| {
        expectation_rows(
            &contract,
            delta,
            ExpectationChecks {
                assembly: &checks,
                ..ExpectationChecks::default()
            },
            &[],
            true,
        )[0]
        .landed
    };
    assert!(!landed(-2));
    assert!(landed(-3));
}

#[test]
fn diff_expect_positive_module_ceiling_accepts_falling_esc() {
    let mut contract = contract(2);
    contract.esc.push(EscExpectation {
        path: PathBuf::from("src/message"),
        max: 2,
    });
    let checks = [EscCheck {
        expectation: contract.esc[0].clone(),
        base: 3,
        current: 2,
    }];
    let rows = expectation_rows(
        &contract,
        2,
        ExpectationChecks {
            esc: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );

    assert!(rows.iter().any(|row| row.assertion == "module narrowing"));
    assert!(rows.iter().all(|row| row.landed));
}

#[test]
fn diff_expect_positive_module_ceiling_rejects_no_falling_esc() {
    for max in [None, Some(3), Some(4)] {
        let mut contract = contract(2);
        contract.esc = max
            .map(|max| EscExpectation {
                path: PathBuf::from("src/message"),
                max,
            })
            .into_iter()
            .collect();
        let checks: Vec<_> = contract
            .esc
            .iter()
            .map(|expectation| EscCheck {
                expectation: expectation.clone(),
                base: 3,
                current: 2,
            })
            .collect();
        let rows = expectation_rows(
            &contract,
            -1,
            ExpectationChecks {
                esc: &checks,
                ..ExpectationChecks::default()
            },
            &[],
            true,
        );
        let row = rows
            .iter()
            .find(|row| row.assertion == "module narrowing")
            .unwrap();

        assert!(!row.landed);
        assert!(
            row.detail
                .contains("a positive ceiling needs an esc max below its base")
        );
        match max {
            Some(max) => assert!(
                row.detail
                    .contains(&format!("`src/message` at {max} over base 3"))
            ),
            None => assert!(row.detail.contains("the contract has no esc rows")),
        }
        assert!(
            rows.iter()
                .filter(|row| row.assertion != "module narrowing")
                .all(|row| row.landed)
        );
    }
}

#[test]
fn diff_expect_positive_seam_ceiling_needs_no_falling_esc() {
    let mut contract = contract(2);
    contract.kind = PassKind::Seam;
    let rows = expectation_rows(&contract, 2, ExpectationChecks::default(), &[], true);

    assert!(!rows.iter().any(|row| row.assertion == "module narrowing"));
    assert!(rows.iter().all(|row| row.landed));
}

#[test]
fn diff_expect_positive_tooling_ceiling_needs_no_falling_esc() {
    let mut contract = contract(160);
    contract.kind = PassKind::Tooling;
    let rows = expectation_rows(&contract, 160, ExpectationChecks::default(), &[], true);

    assert!(!rows.iter().any(|row| row.assertion == "module narrowing"));
    assert!(rows.iter().all(|row| row.landed));
}

#[test]
fn diff_expect_rejects_esc_excess() {
    let mut contract = contract(-1);
    contract.esc.push(EscExpectation {
        path: PathBuf::from("src/message"),
        max: 2,
    });
    let checks = [EscCheck {
        expectation: contract.esc[0].clone(),
        base: 1,
        current: 3,
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            esc: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "esc `src/message`")
        .unwrap();

    assert!(!row.landed);
    assert_eq!(row.detail, "base 1 → current 3 → max 2; excess 1");
}

#[test]
fn diff_expect_rejects_still_defined_delete_item() {
    let mut contract = contract(-1);
    contract.delete.push(DeleteExpectation {
        item: "message::OLD".to_owned(),
    });
    let checks = [DeleteCheck {
        expectation: contract.delete[0].clone(),
        current: Some(DefinitionSite {
            path: PathBuf::from("src/message.rs"),
            line: 27,
        }),
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            delete: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "delete `message::OLD`")
        .unwrap();

    assert!(!row.landed);
    assert_eq!(row.detail, "still defined at src/message.rs:27");
}

#[test]
fn diff_expect_rejects_unmoved_rehome_item() {
    let mut contract = contract(-1);
    contract.rehome.push(item_rehome("message::Thing", "store"));
    let checks = [RehomeCheck {
        expectation: contract.rehome[0].clone(),
        item: "message::Thing".to_owned(),
        old: Some(DefinitionSite {
            path: PathBuf::from("src/message.rs"),
            line: 27,
        }),
        destinations: Vec::new(),
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            rehome: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "rehome message::Thing → store")
        .unwrap();

    assert!(!row.landed);
    assert_eq!(row.detail, "still defined at src/message.rs:27");
}

#[test]
fn diff_expect_accepts_moved_rehome_item() {
    let mut contract = contract(-1);
    contract.rehome.push(item_rehome("message::Thing", "store"));
    let checks = [RehomeCheck {
        expectation: contract.rehome[0].clone(),
        item: "message::Thing".to_owned(),
        old: None,
        destinations: vec![DefinitionSite {
            path: PathBuf::from("src/store/model.rs"),
            line: 12,
        }],
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            rehome: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "rehome message::Thing → store")
        .unwrap();

    assert!(row.landed);
    assert_eq!(row.detail, "moved");
}

#[test]
fn diff_expect_lists_every_site_of_a_rehome_defined_twice() {
    let mut contract = contract(-1);
    contract.rehome.push(item_rehome("message::Thing", "store"));
    let checks = [RehomeCheck {
        expectation: contract.rehome[0].clone(),
        item: "message::Thing".to_owned(),
        old: None,
        destinations: vec![
            DefinitionSite {
                path: PathBuf::from("src/store/record.rs"),
                line: 12,
            },
            DefinitionSite {
                path: PathBuf::from("src/store.rs"),
                line: 4,
            },
        ],
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            rehome: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "rehome message::Thing → store")
        .unwrap();

    assert!(!row.landed);
    assert_eq!(
        row.detail,
        "defined 2 times under store: src/store/record.rs:12, src/store.rs:4"
    );
}

#[test]
fn diff_expect_rejects_dependency_excess() {
    let base = two_file_dependency_facts("use crate::agents::Thing;");
    let current = two_file_dependency_facts("use crate::agents::Thing;");
    let expectation = DependencyExpectation {
        from: "store".to_owned(),
        to: "agents".to_owned(),
        max_sites: 0,
    };
    let mut contract = contract(-1);
    contract.dependency.push(expectation.clone());
    let checks = [DependencyCheck {
        base: contract_dependency_sites(&base, &expectation),
        current: contract_dependency_sites(&current, &expectation),
        expectation,
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            dependency: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "dependency store → agents")
        .unwrap();

    assert!(!row.landed);
    assert_eq!(row.detail, "base 1 → current 1 → max 0; excess 1");
}

#[test]
fn diff_expect_accepts_dependency_within_max() {
    let base = two_file_dependency_facts("use crate::agents::Thing;");
    let current = two_file_dependency_facts("");
    let expectation = DependencyExpectation {
        from: "store".to_owned(),
        to: "agents".to_owned(),
        max_sites: 0,
    };
    let mut contract = contract(-1);
    contract.dependency.push(expectation.clone());
    let checks = [DependencyCheck {
        base: contract_dependency_sites(&base, &expectation),
        current: contract_dependency_sites(&current, &expectation),
        expectation,
    }];

    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            dependency: &checks,
            ..ExpectationChecks::default()
        },
        &[],
        true,
    );
    let row = rows
        .iter()
        .find(|row| row.assertion == "dependency store → agents")
        .unwrap();

    assert!(row.landed);
    assert_eq!(row.detail, "base 1 → current 0 → max 0");
}

#[test]
fn dependency_sites_split_crossing_from_internal_movement() {
    let base = facts_for_sources(
        vec![
            Source::new("src/scope/mod.rs", "pub fn run() {}"),
            Source::new("src/scope/sibling.rs", "pub struct Inside;"),
            Source::new("src/outside.rs", "pub struct Outside;"),
        ],
        References::default(),
    );
    let current = facts_for_sources(
        vec![
            Source::new(
                "src/scope/mod.rs",
                "use crate::outside::Outside;\nuse crate::scope::sibling::Inside;\npub fn run(_: Outside, _: Inside) {}",
            ),
            Source::new("src/scope/sibling.rs", "pub struct Inside;"),
            Source::new("src/outside.rs", "pub struct Outside;"),
        ],
        References::default(),
    );
    let paths = [PathBuf::from("src/scope")];
    let base_sites = dependencies(&base, &paths, None);
    let current_sites = dependencies(&current, &paths, None);
    let added = difference(&current_sites, &base_sites);
    let counts = dependency_counts(&current_sites);

    assert_eq!(added.len(), 2);
    assert_eq!(added.iter().filter(|site| site.crossing).count(), 1);
    assert_eq!(added.iter().filter(|site| !site.crossing).count(), 1);
    assert_eq!(counts.get("unranked"), Some(&1));
    assert_eq!(counts.get("internal"), Some(&1));
}

#[test]
fn diff_expect_rejects_changes_outside_paths() {
    let changed = BTreeSet::from([
        PathBuf::from("src/store/mod.rs"),
        PathBuf::from("README.md"),
    ]);
    let (_, outside) = split_changed_paths(&changed, &[PathBuf::from("src/store")]);
    let contract = contract(-1);
    let checks = [AssemblyCheck {
        expectation: contract.assembly[0].clone(),
        base: 4,
        current: 2,
    }];
    let rows = expectation_rows(
        &contract,
        -2,
        ExpectationChecks {
            assembly: &checks,
            ..ExpectationChecks::default()
        },
        &outside,
        true,
    );

    assert_eq!(outside, [PathBuf::from("README.md")]);
    assert!(
        !rows
            .iter()
            .find(|row| row.assertion == "changed paths")
            .unwrap()
            .landed
    );
}

#[test]
fn contract_assembly_treats_a_base_only_missing_endpoint_as_zero() {
    let root = tempfile::tempdir().unwrap();
    let sources = vec![Source::new("src/caller.rs", "fn call() {}")];
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    let facts = Facts {
        root: root.path().to_path_buf(),
        scope: PathBuf::from("."),
        mod_index: super::super::syntax::ModIndex::new(&syntax.files),
        known_modules: syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect(),
        defined_names: super::super::facts::defined_names(&syntax),
        unique_fields: super::super::facts::unique_fields(&syntax),
        defining_modules: super::super::facts::defining_modules(&syntax),
        binaries: super::super::modules::BinaryTargets::new(&syntax.files),
        syntax,
        sources,
        crate_names: BTreeSet::new(),
        sizes: BTreeMap::new(),
        history: None,
        metrics: None,
        references: Some(References::default()),
    };
    let expectation = AssemblyExpectation {
        from: "caller".to_owned(),
        to: "newmod".to_owned(),
        max_items: 1,
    };

    assert_eq!(
        contract_assembly(root.path(), &facts, &expectation, true).unwrap(),
        0
    );
    assert!(
        contract_assembly(root.path(), &facts, &expectation, false)
            .unwrap_err()
            .to_string()
            .contains("atlas diff contract assembly.to")
    );
}

#[test]
fn declaration_only_mod_never_becomes_newly_unresolved() {
    let mut base = facts_for_source("pub mod child;", References::default());
    let key = {
        let file = &base.syntax.files[0];
        let item = file
            .pub_items
            .iter()
            .find(|item| item.kind == "mod")
            .unwrap();
        ItemKey::new(file, item)
    };
    base.references
        .as_mut()
        .unwrap()
        .items
        .insert(key, ItemRefs::default());
    let current = facts_for_source("pub mod child;", References::default());
    let paths = [PathBuf::from("src/lib.rs")];

    assert!(
        boundary_surfaces(&current, &paths)[0]
            .items
            .iter()
            .any(|item| item.id.kind == "mod")
    );
    assert!(
        evidence(&base, &current, &paths)
            .newly_unresolved
            .is_empty()
    );
}

#[test]
fn diff_reports_boundary_esc_not_leaf_sums() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("src/domain")).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"atlas-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(root.path().join("src/lib.rs"), "pub mod domain;\n").unwrap();
    fs::write(root.path().join("src/domain/mod.rs"), "mod detail;\n").unwrap();
    fs::write(
        root.path().join("src/domain/detail.rs"),
        "pub fn helper() {}\n",
    )
    .unwrap();
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();

    let boundary = boundary_surfaces(&facts, &[PathBuf::from("src/domain")]);
    let leaf = escaping_items_for_boundary(
        &[facts
            .syntax
            .files
            .iter()
            .find(|file| file.path == Path::new("src/domain/detail.rs"))
            .unwrap()],
        "domain::detail",
        &facts.mod_index,
    );

    assert!(boundary[0].items.is_empty());
    assert_eq!(leaf.len(), 1);
}

#[test]
fn changed_paths_are_grouped_and_bounded_for_markdown() {
    let paths = vec![
        PathBuf::from("README.md"),
        PathBuf::from("docs/guide/start.md"),
        PathBuf::from("crates/rimz/src/message/deliver.rs"),
        PathBuf::from("xtask/src/atlas/diff.rs"),
    ];
    let mut output = String::new();

    write_changed_paths(&mut output, "outside", &paths, 3);

    assert!(output.contains("| outside | `(root)` | `README.md` |"));
    assert!(output.contains("| outside | `docs/` | `docs/guide/start.md` |"));
    assert!(output.contains("| outside | `message` | `crates/rimz/src/message/deliver.rs` |"));
    assert!(!output.contains("xtask/src/atlas/diff.rs"));
    assert!(output.contains("_1 more omitted._"));
}

fn contract(max_production_sloc_delta: i64) -> PassContract {
    PassContract {
        version: 1,
        base: "HEAD".to_owned(),
        kind: super::super::contract::PassKind::Module,
        paths: vec![PathBuf::from("src")],
        max_production_sloc_delta,
        assembly: vec![AssemblyExpectation {
            from: "caller".to_owned(),
            to: "target".to_owned(),
            max_items: 3,
        }],
        esc: Vec::new(),
        delete: Vec::new(),
        rehome: Vec::new(),
        dependency: Vec::new(),
        cx: Vec::new(),
    }
}

#[test]
fn cx_rows_round_caps_and_report_absence_and_missing_metrics() {
    for (base, current, max, landed, detail) in [
        (
            Ok(Some(8.0)),
            Ok(Some(5.04)),
            5.0,
            true,
            "base 8.0 → current 5.0 → max 5.0",
        ),
        (Ok(Some(8.0)), Ok(Some(5.16)), 5.04, false, "; excess 0.2"),
        (Ok(None), Ok(Some(5.0)), 5.0, true, "base absent"),
        (Ok(Some(8.0)), Ok(None), 0.0, true, "current absent"),
        (Ok(Some(8.0)), Err("no metric"), 5.0, false, "no metric"),
        (Err("no metric"), Ok(Some(5.0)), 5.0, false, "no metric"),
    ] {
        let checks = [CxCheck {
            expectation: CxExpectation {
                item: Some("cli::run".into()),
                path: None,
                max,
            },
            base,
            current,
        }];
        let rows = expectation_rows(
            &contract(0),
            0,
            ExpectationChecks {
                cx: &checks,
                ..Default::default()
            },
            &[],
            true,
        );
        let row = rows
            .iter()
            .find(|row| row.assertion == "cx cli::run")
            .expect("cx row emitted");
        assert_eq!(row.landed, landed);
        assert!(row.detail.contains(detail), "{}", row.detail);
    }
}

#[test]
fn cx_rounding_matches_survey_display() {
    for value in [1.25, 1.15, 1.05, 5.04, 5.16] {
        assert_eq!(
            rounded_cx(value),
            format!("{value:.1}").parse::<f64>().unwrap()
        );
    }
}

#[test]
fn cx_pairs_by_name_and_span_and_sums_directory_with_sibling() {
    use super::super::metrics::{FunctionMetric, MetricsReport};
    let mut facts = facts_for_sources(
        vec![
            Source::new("src/cli.rs", "#[inline]\nfn run() { fn nested() {} }"),
            Source::new("src/cli/child.rs", "fn run() {}"),
            Source::new("src/client.rs", "fn run() {}"),
        ],
        References::default(),
    );
    facts.metrics = Some(MetricsReport {
        module_scores: BTreeMap::new(),
        functions: [
            ("src/cli.rs", "run", 2, 3.0),
            ("src/cli.rs", "wrong", 1, 20.0),
            ("src/cli.rs", "run", 99, 30.0),
            ("src/cli/child.rs", "run", 1, 4.0),
            ("src/client.rs", "run", 1, 50.0),
        ]
        .into_iter()
        .map(|(path, name, line, score)| FunctionMetric {
            path: path.into(),
            name: name.into(),
            line,
            score,
            module: String::new(),
            cyclomatic: 0.0,
            cognitive: 0.0,
            sloc: 0.0,
        })
        .collect(),
    });
    let mut row = CxExpectation {
        item: Some("cli::run".into()),
        path: None,
        max: 0.0,
    };
    assert_eq!(cx_value(&facts, &row), Ok(Some(3.0)));
    row.item = Some("cli::nested".into());
    assert_eq!(cx_value(&facts, &row), Err("no metric"));
    row.item = Some("cli::absent".into());
    assert_eq!(cx_value(&facts, &row), Ok(None));
    row.item = None;
    row.path = Some("src/cli".into());
    assert_eq!(cx_value(&facts, &row), Ok(Some(57.0)));
    row.path = Some("src/cli/child.rs".into());
    assert_eq!(cx_value(&facts, &row), Ok(Some(4.0)));
}

#[test]
fn bin_to_lib_rehome_lands() {
    let base = facts_for_sources(
        vec![
            Source::new("src/main.rs", "mod cli; fn main() {}"),
            Source::new("src/cli.rs", "mod loop_cmd;"),
            Source::new("src/cli/loop_cmd.rs", "pub(super) fn run_one() {}"),
            Source::new("src/lib.rs", "pub mod harness;"),
            Source::new("src/harness.rs", "pub mod schedule;"),
            Source::new("src/harness/schedule.rs", ""),
        ],
        References::default(),
    );
    let current = facts_for_sources(
        base.sources
            .iter()
            .map(|source| {
                Source::new(
                    source.path.clone(),
                    match source.path.to_str().unwrap() {
                        "src/cli/loop_cmd.rs" => "",
                        "src/harness/schedule.rs" => "pub fn run_one() {}",
                        _ => &source.text,
                    },
                )
            })
            .collect(),
        References::default(),
    );
    let item = "cli::loop_cmd::run_one";
    assert!(definition_site(&base.syntax.files, item).is_some());
    let checks = [RehomeCheck {
        expectation: item_rehome(item, "harness::schedule"),
        item: item.into(),
        old: definition_site(&current.syntax.files, item),
        destinations: definition_sites(&current.syntax.files, "harness::schedule", item, None),
    }];
    let rows = expectation_rows(
        &contract(0),
        0,
        ExpectationChecks {
            rehome: &checks,
            ..Default::default()
        },
        &[],
        true,
    );
    assert!(
        rows.iter()
            .find(|row| row.assertion.starts_with("rehome "))
            .unwrap()
            .landed
    );
}

fn item_rehome(item: &str, to: &str) -> RehomeExpectation {
    RehomeExpectation {
        item: Some(item.to_owned()),
        from: None,
        to: to.to_owned(),
        min_decisions: None,
    }
}

fn facts_for_source(source: &str, references: References) -> Facts {
    facts_for_sources(vec![Source::new("src/lib.rs", source)], references)
}

fn facts_for_sources(sources: Vec<Source>, references: References) -> Facts {
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    Facts {
        root: PathBuf::from("."),
        scope: PathBuf::from("."),
        mod_index: super::super::syntax::ModIndex::new(&syntax.files),
        known_modules: syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect(),
        defined_names: super::super::facts::defined_names(&syntax),
        unique_fields: super::super::facts::unique_fields(&syntax),
        defining_modules: super::super::facts::defining_modules(&syntax),
        binaries: super::super::modules::BinaryTargets::new(&syntax.files),
        syntax,
        sources,
        crate_names: BTreeSet::new(),
        sizes: BTreeMap::new(),
        history: None,
        metrics: None,
        references: Some(references),
    }
}

fn two_file_dependency_facts(store: &str) -> Facts {
    facts_for_sources(
        vec![
            Source::new("src/store.rs", store),
            Source::new("src/agents.rs", "pub struct Thing;"),
        ],
        References::default(),
    )
}

fn reference_edge(path: &str, item: &str, function_line: Option<usize>) -> Edge {
    Edge {
        from_path: PathBuf::from(path),
        to_path: PathBuf::from("src/target.rs"),
        from: "caller".to_owned(),
        to: "target".to_owned(),
        to_line: 1,
        item: item.to_owned(),
        kind: EdgeKind::Reference,
        site_kind: SourceKind::Production,
        cross_target: false,
        from_line: function_line.unwrap_or(1),
        from_fn: function_line.map(|line| FnRef {
            label: "run".to_owned(),
            line,
        }),
    }
}

fn interface_row(
    from: &str,
    to: &str,
    base: usize,
    current: usize,
    heaviest_moved: bool,
) -> InterfaceRow {
    InterfaceRow {
        from: from.to_owned(),
        to: to.to_owned(),
        base,
        current,
        base_heaviest: Some("base".to_owned()),
        current_heaviest: Some(if heaviest_moved { "current" } else { "base" }.to_owned()),
        moved: base != current || heaviest_moved,
    }
}

#[test]
fn contract_assembly_from_an_ancestor_skips_the_targets_own_functions() {
    let root = tempfile::tempdir().unwrap();
    let sources = vec![
        Source::new("src/cli.rs", "fn dispatch() {}"),
        Source::new(
            "src/cli/exec.rs",
            "pub fn run_exec() {}\npub fn settle() {}\npub fn spawn() {}",
        ),
    ];
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    let edge = |from: &str, path: &str, function: &str, item: &str| Edge {
        from_path: PathBuf::from(path),
        to_path: PathBuf::from("src/cli/exec.rs"),
        from: from.to_owned(),
        from_line: 1,
        from_fn: Some(FnRef {
            label: function.to_owned(),
            line: 1,
        }),
        to: "cli::exec".to_owned(),
        to_line: 1,
        item: item.to_owned(),
        kind: EdgeKind::Reference,
        site_kind: SourceKind::Production,
        cross_target: false,
    };
    let references = References {
        edges: vec![
            edge("cli", "src/cli.rs", "dispatch", "run_exec"),
            edge("cli::exec", "src/cli/exec.rs", "run_exec", "settle"),
            edge("cli::exec", "src/cli/exec.rs", "run_exec", "spawn"),
        ],
        ..References::default()
    };
    let facts = Facts {
        root: root.path().to_path_buf(),
        scope: PathBuf::from("."),
        mod_index: super::super::syntax::ModIndex::new(&syntax.files),
        known_modules: syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect(),
        defined_names: super::super::facts::defined_names(&syntax),
        unique_fields: super::super::facts::unique_fields(&syntax),
        defining_modules: super::super::facts::defining_modules(&syntax),
        binaries: super::super::modules::BinaryTargets::new(&syntax.files),
        syntax,
        sources,
        crate_names: BTreeSet::new(),
        sizes: BTreeMap::new(),
        history: None,
        metrics: None,
        references: Some(references),
    };
    let expectation = AssemblyExpectation {
        from: "cli".to_owned(),
        to: "cli::exec".to_owned(),
        max_items: 1,
    };

    assert_eq!(
        contract_assembly(root.path(), &facts, &expectation, false).unwrap(),
        1,
        "`run_exec` inside the target assembles nothing for `cli`"
    );
}

fn rows_for(checks: ExpectationChecks<'_>) -> Vec<ExpectationRow> {
    expectation_rows(&contract(0), 0, checks, &[], true)
}

fn row<'a>(rows: &'a [ExpectationRow], assertion: &str) -> &'a ExpectationRow {
    rows.iter()
        .find(|row| row.assertion == assertion)
        .unwrap_or_else(|| panic!("no `{assertion}` row in {rows:?}"))
}

#[test]
fn delete_of_a_private_function_lands_only_once_it_is_gone() {
    let key = "cli::exec::settle";
    for (source, landed, detail) in [
        (
            "/// Settles.\nfn settle() {}\nfn run() {}",
            false,
            "still defined at src/cli/exec.rs:1",
        ),
        ("fn run() {}", true, "deleted"),
    ] {
        let current = facts_for_sources(
            vec![Source::new("src/cli/exec.rs", source)],
            References::default(),
        );
        let checks = [DeleteCheck {
            expectation: DeleteExpectation { item: key.into() },
            current: definition_site(&current.syntax.files, key),
        }];
        let rows = rows_for(ExpectationChecks {
            delete: &checks,
            ..Default::default()
        });
        let row = row(&rows, "delete `cli::exec::settle`");
        assert_eq!((row.landed, row.detail.as_str()), (landed, detail));
    }
}

#[test]
fn private_function_and_method_rehome_land_when_defined_once_under_to() {
    let base = facts_for_sources(
        vec![Source::new(
            "src/cli/exec.rs",
            "fn settle() {}\nstruct Run;\nimpl Run { fn step(&self) {} }",
        )],
        References::default(),
    );
    for (key, destination, landed) in [
        // A documented `pub fn` is one item and one function: one site.
        (
            "cli::exec::settle",
            "/// Settles.\n#[inline]\npub fn settle() {}",
            true,
        ),
        ("cli::exec::settle", "fn settle() {}", true),
        // The method keeps its owner; a free `step` beside it is no match.
        (
            "cli::exec::Run::step",
            "pub struct Run;\nimpl Run { pub fn step(&self) {} }\nfn step() {}",
            true,
        ),
        // A function and a `pub use` of it are two sites.
        (
            "cli::exec::settle",
            "fn settle() {}\npub use self::settle;",
            false,
        ),
        (
            "cli::exec::settle",
            "mod a { pub fn settle() {} }\nfn settle() {}",
            false,
        ),
        ("cli::exec::settle", "fn other() {}", false),
    ] {
        let current = facts_for_sources(
            vec![
                Source::new("src/cli/exec.rs", "fn run() {}"),
                Source::new("src/harness/run.rs", destination),
            ],
            References::default(),
        );
        let checks = [RehomeCheck {
            expectation: item_rehome(key, "harness"),
            item: key.into(),
            old: definition_site(&current.syntax.files, key),
            destinations: definition_sites(
                &current.syntax.files,
                "harness",
                key,
                rehome_owner(&base.syntax.files, key),
            ),
        }];
        let rows = rows_for(ExpectationChecks {
            rehome: &checks,
            ..Default::default()
        });
        let row = row(&rows, &format!("rehome {key} → harness"));
        assert_eq!(row.landed, landed, "{destination}: {}", row.detail);
    }
}

#[test]
fn item_rehome_of_a_private_function_drifts_while_the_old_site_stands() {
    let key = "cli::exec::settle";
    let current = facts_for_sources(
        vec![
            Source::new("src/cli/exec.rs", "fn settle() {}"),
            Source::new("src/harness.rs", "pub fn settle() {}"),
        ],
        References::default(),
    );
    let checks = [RehomeCheck {
        expectation: item_rehome(key, "harness"),
        item: key.into(),
        old: definition_site(&current.syntax.files, key),
        destinations: definition_sites(&current.syntax.files, "harness", key, None),
    }];
    let rows = rows_for(ExpectationChecks {
        rehome: &checks,
        ..Default::default()
    });
    let row = row(&rows, "rehome cli::exec::settle → harness");
    assert!(!row.landed);
    assert_eq!(row.detail, "still defined at src/cli/exec.rs:1");
}

fn metric(path: &str, cyclomatic: f64) -> FunctionMetric {
    FunctionMetric {
        // The scope-relative directory, as `metrics::analyze` spells it.
        module: "crates".into(),
        path: path.into(),
        name: "f".into(),
        line: 1,
        cyclomatic,
        cognitive: 0.0,
        sloc: 0.0,
        score: 0.0,
    }
}

fn decision_row_for(base: &[FunctionMetric], current: &[FunctionMetric]) -> ExpectationRow {
    let checks = [DecisionCheck::measure(
        "cli::agents_cmd::exec",
        "harness",
        40,
        base,
        current,
    )];
    let rows = rows_for(ExpectationChecks {
        rehome_logic: &checks,
        ..Default::default()
    });
    row(&rows, "rehome cli::agents_cmd::exec → harness").clone()
}

const EXEC: &str = "crates/rimz/src/cli/agents_cmd/exec.rs";
const EXEC_CHILD: &str = "crates/rimz/src/cli/agents_cmd/exec/settle.rs";
const SIBLING: &str = "crates/rimz/src/cli/agents_cmd/exec_other.rs";
const HARNESS: &str = "crates/rimz/src/harness/run.rs";

#[test]
fn decisions_sum_branch_points_under_the_module_path() {
    let metrics = [
        metric(EXEC, 11.0),
        metric(EXEC_CHILD, 3.0),
        metric(EXEC, 1.0),
        metric(SIBLING, 50.0),
    ];
    assert_eq!(decisions(&metrics, "cli::agents_cmd::exec"), 12);
    assert_eq!(decisions(&metrics, "harness"), 0);
    // Extracting a branchless helper leaves the count where it was.
    assert_eq!(
        decisions(
            &[metric(EXEC, 11.0), metric(EXEC, 1.0)],
            "cli::agents_cmd::exec"
        ),
        decisions(&[metric(EXEC, 11.0)], "cli::agents_cmd::exec"),
    );
}

#[test]
fn logic_rehome_lands_when_from_falls_by_min_and_to_rises_no_more() {
    let row = decision_row_for(
        &[metric(EXEC, 213.0), metric(HARNESS, 81.0)],
        &[metric(EXEC, 161.0), metric(HARNESS, 119.0)],
    );
    assert!(row.landed, "{}", row.detail);
    assert_eq!(
        row.detail,
        "decisions cli::agents_cmd::exec 212 → 160 (-52, min 40); harness 80 → 118 (+38)"
    );
}

#[test]
fn logic_rehome_drifts_when_from_falls_below_min() {
    let row = decision_row_for(
        &[metric(EXEC, 213.0), metric(HARNESS, 81.0)],
        &[metric(EXEC, 201.0), metric(HARNESS, 81.0)],
    );
    assert!(!row.landed);
    assert!(row.detail.ends_with("; fell 12 < min 40"), "{}", row.detail);
}

#[test]
fn logic_rehome_drifts_when_to_rises_more_than_from_fell() {
    let row = decision_row_for(
        &[metric(EXEC, 213.0), metric(HARNESS, 81.0)],
        &[metric(EXEC, 161.0), metric(HARNESS, 141.0)],
    );
    assert!(!row.landed);
    assert!(
        row.detail.ends_with("; harness rose 60 > 52 moved"),
        "{}",
        row.detail
    );
}

#[test]
fn logic_rehome_to_a_new_owner_measures_zero_at_base() {
    let row = decision_row_for(
        &[metric(EXEC, 101.0)],
        &[metric(EXEC, 51.0), metric(HARNESS, 51.0)],
    );
    assert!(row.landed, "{}", row.detail);
    assert!(
        row.detail.ends_with("harness 0 → 50 (+50)"),
        "{}",
        row.detail
    );
}
