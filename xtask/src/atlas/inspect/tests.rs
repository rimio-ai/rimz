use std::fs;

use super::super::references::{EdgeKind, FnRef};
use super::super::sources::Source;
use super::super::target::{Layer, ModuleRule};
use super::*;

#[test]
fn inspect_args_resolve_modules_from_paths_and_reject_no_index() {
    let args = parse_args(&[
        "--from".into(),
        "crate::store".into(),
        "--to".into(),
        "cli".into(),
        "--top".into(),
        "4".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(args.from, "crate::store");
    assert_eq!(args.to, "cli");
    assert_eq!(args.top, 4);
    assert!(
        parse_args(&[
            "--from".into(),
            "store".into(),
            "--to".into(),
            "cli".into(),
            "--no-index".into(),
        ])
        .unwrap_err()
        .to_string()
        .contains("reference view")
    );

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/demo/src/store")).unwrap();
    fs::write(
        root.path().join("crates/demo/src/store/mod.rs"),
        "mod writer;\n",
    )
    .unwrap();
    let syntax = super::super::syntax::analyze_sources(&[
        Source::new("crates/demo/src/store/writer.rs", "fn write() {}"),
        Source::new("crates/demo/src/cli.rs", "fn run() {}"),
    ]);
    assert_eq!(
        resolve_module(
            root.path(),
            &syntax.files,
            "crates/demo/src/store",
            "--from",
        )
        .unwrap(),
        ModuleSelector {
            module: "store".to_owned(),
            path: Some(PathBuf::from("crates/demo/src/store")),
            directory: true,
        }
    );
    assert_eq!(
        resolve_module(root.path(), &syntax.files, "crate::cli", "--to").unwrap(),
        ModuleSelector {
            module: "cli".to_owned(),
            path: None,
            directory: false,
        }
    );
    fs::create_dir(root.path().join("docs")).unwrap();
    assert!(
        resolve_module(root.path(), &syntax.files, "docs", "--to")
            .unwrap_err()
            .to_string()
            .contains("does not match a Rust module")
    );
}

#[test]
fn functions_rank_by_distinct_items_and_quote_the_heaviest() {
    let statements = (0..81)
        .map(|index| format!("    let value_{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = Source::new(
        "crates/demo/src/caller.rs",
        format!("fn heavy() {{\n{statements}\n}}\n\nfn light() {{\n    let value = 1;\n}}\n"),
    );
    let syntax = super::super::syntax::analyze_sources(std::slice::from_ref(&source));
    let heavy = syntax.files[0]
        .fns
        .iter()
        .find(|function| function.name == "heavy")
        .unwrap();
    let light = syntax.files[0]
        .fns
        .iter()
        .find(|function| function.name == "light")
        .unwrap();
    let edges = vec![
        edge("a", Some(("heavy", heavy.line)), false),
        edge("b", Some(("heavy", heavy.line)), false),
        edge("a", Some(("heavy", heavy.line)), false),
        edge("c", Some(("light", light.line)), false),
        edge("d", None, false),
        Edge {
            from_path: PathBuf::from("crates/demo/tests/api.rs"),
            to_path: PathBuf::from("crates/demo/src/store.rs"),
            from_line: 3,
            from_fn: None,
            from: "tests::api".to_owned(),
            to: "store".to_owned(),
            item: "a".to_owned(),
            kind: EdgeKind::Reference,
            test: true,
        },
    ];

    let (totals, functions, heaviest, tests) = assembly_report(
        &edges,
        std::slice::from_ref(&source),
        &syntax.files,
        &selector("caller"),
        &selector("store"),
        Path::new("."),
    );

    assert_eq!(totals.functions, 2);
    assert_eq!(totals.items, 4);
    assert_eq!(totals.sites, 5);
    assert_eq!(functions[0].function, "heavy");
    assert_eq!(functions[0].items, ["a", "b"]);
    assert_eq!(functions[0].items_total, 2);
    assert_eq!(functions.last().unwrap().function, "(outside any function)");
    let heaviest = heaviest.unwrap();
    assert_eq!(heaviest.function, "heavy");
    assert_eq!(heaviest.source.lines().count(), 81);
    assert!(heaviest.source.ends_with("… 3 more lines"));
    assert_eq!(tests[0].items, ["a"]);
    assert_eq!(tests[0].items_total, 1);
}

#[test]
fn compact_json_preserves_complete_item_counts() {
    let mut report = Report {
        version: REPORT_VERSION,
        verb: "inspect",
        from: "caller".to_owned(),
        to: "store".to_owned(),
        path: PathBuf::from("."),
        totals: Totals {
            functions: 1,
            items: 3,
            sites: 3,
        },
        functions: vec![FunctionRow {
            function: "run".to_owned(),
            path: PathBuf::from("src/lib.rs"),
            line: 1,
            end_line: 4,
            items: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            items_total: 3,
            sites: 3,
            outside: false,
        }],
        heaviest: None,
        tests: vec![TestRow {
            path: PathBuf::from("tests/api.rs"),
            sites: 3,
            items: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            items_total: 3,
        }],
        rules: Vec::new(),
        parse_failures: Vec::new(),
        target_configured: false,
    };

    compact_json(&mut report, 1);

    assert_eq!(report.functions[0].items, ["a"]);
    assert_eq!(report.functions[0].items_total, 3);
    assert_eq!(report.tests[0].items, ["a"]);
    assert_eq!(report.tests[0].items_total, 3);
}

#[test]
fn rules_report_direction_admission_and_debt() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        root.path().join("src/lib.rs"),
        "mod cli;\nmod config;\nmod store;\n",
    )
    .unwrap();
    fs::write(root.path().join("src/cli.rs"), "pub struct Thing;\n").unwrap();
    fs::write(root.path().join("src/config.rs"), "pub struct Peer;\n").unwrap();
    fs::write(
        root.path().join("src/store.rs"),
        "use crate::cli::Thing;\npub fn visible() -> Thing { Thing }\n",
    )
    .unwrap();
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let target = Target {
        version: 4,
        layers: vec![
            Layer::Group(vec!["store".to_owned(), "config".to_owned()]),
            Layer::Module("cli".to_owned()),
        ],
        modules: vec![ModuleRule {
            path: PathBuf::from("src/store.rs"),
            allowed_imports: None,
            upward_imports: Some(vec!["cli".to_owned()]),
            surface_budget: 2,
            surface_goal: None,
            upward_debt: Some(vec!["cli".to_owned()]),
            config_line: 4,
        }],
        strangler: Vec::new(),
    };

    let rules = target_rules(
        root.path(),
        &target,
        &root.path().join("target.toml"),
        &facts,
        &selector("store"),
        &selector("cli"),
    )
    .unwrap();

    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].direction, "upward");
    assert_eq!(rules[0].admitted.as_deref(), Some("cli"));
    let debt = rules[0].debt.as_ref().unwrap();
    assert_eq!(debt.prefix, "cli");
    assert_eq!(debt.sites, 1);

    let partial = rule_row(
        &ModuleRule {
            path: PathBuf::from("src/store.rs"),
            allowed_imports: Some(vec!["cli::render".to_owned()]),
            upward_imports: None,
            surface_budget: 2,
            surface_goal: None,
            upward_debt: None,
            config_line: 4,
        },
        &BTreeMap::new(),
        &target.layer_ranks(),
        "store",
        "cli",
    );
    assert_eq!(partial.admitted, None);
}

fn edge(item: &str, function: Option<(&str, usize)>, test: bool) -> Edge {
    Edge {
        from_path: PathBuf::from("crates/demo/src/caller.rs"),
        to_path: PathBuf::from("crates/demo/src/store.rs"),
        from_line: function.map_or(1, |(_, line)| line + 1),
        from_fn: function.map(|(label, line)| FnRef {
            label: label.to_owned(),
            line,
        }),
        from: "caller".to_owned(),
        to: "store".to_owned(),
        item: item.to_owned(),
        kind: EdgeKind::Reference,
        test,
    }
}

fn selector(module: &str) -> ModuleSelector {
    ModuleSelector {
        module: module.to_owned(),
        path: None,
        directory: false,
    }
}
