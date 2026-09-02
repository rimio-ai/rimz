use std::fs;
use std::process::Command;

use scip::types::{Index, Occurrence, SymbolRole};

use super::super::references::{FnRef, References};
use super::super::sources::Source;
use super::super::target::Verdict;
use super::*;

#[test]
fn inspect_args_require_a_module_and_reject_old_output_flags() {
    let args = parse_args(&[
        "--module".into(),
        "crate::store".into(),
        "--from".into(),
        "cli".into(),
        "--item".into(),
        "store::open".into(),
        "--top".into(),
        "4".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(args.module, "crate::store");
    assert_eq!(args.from.as_deref(), Some("cli"));
    assert_eq!(args.item.as_deref(), Some("store::open"));
    assert_eq!(args.top, 4);
    assert!(
        parse_args(&[])
            .unwrap_err()
            .to_string()
            .contains("--module")
    );
    assert!(parse_args(&["--module".into(), "store".into(), "--json".into()]).is_err());
    assert!(
        parse_args(&["--module".into(), "store".into(), "--no-index".into()])
            .unwrap_err()
            .to_string()
            .contains("SCIP")
    );
}

#[test]
fn module_selectors_resolve_paths_and_module_names() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/demo/src/store")).unwrap();
    fs::write(
        root.path().join("crates/demo/src/store/mod.rs"),
        "mod writer;\n",
    )
    .unwrap();
    let syntax = super::super::syntax::analyze_sources(
        &[
            Source::new("crates/demo/src/store.rs", "fn entry() {}"),
            Source::new("crates/demo/src/store/writer.rs", "fn write() {}"),
            Source::new("crates/demo/src/cli.rs", "fn run() {}"),
        ],
        &BTreeSet::new(),
    );

    assert_eq!(
        resolve_module(
            root.path(),
            &syntax.files,
            "crates/demo/src/store",
            "inspect",
            "--module"
        )
        .unwrap(),
        ModuleSelector {
            module: "store".to_owned(),
            path: Some(PathBuf::from("crates/demo/src/store")),
            directory: true,
        }
    );
    let store = resolve_module(
        root.path(),
        &syntax.files,
        "crates/demo/src/store",
        "inspect",
        "--module",
    )
    .unwrap();
    assert!(store.matches("store", Path::new("crates/demo/src/store.rs")));
    assert_eq!(
        resolve_module(
            root.path(),
            &syntax.files,
            "crate::cli",
            "inspect",
            "--from"
        )
        .unwrap(),
        selector("cli")
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
    let syntax =
        super::super::syntax::analyze_sources(std::slice::from_ref(&source), &BTreeSet::new());
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
        edge("a", "caller", Some(("heavy", heavy.line)), false),
        edge("b", "caller", Some(("heavy", heavy.line)), false),
        edge("a", "caller", Some(("heavy", heavy.line)), false),
        edge("c", "caller", Some(("light", light.line)), false),
    ];

    let functions = assembly_functions(
        &edges,
        &syntax.files,
        &selector("caller"),
        &selector("store"),
    );

    assert_eq!(functions[0].function, "heavy");
    assert_eq!(functions[0].items, ["a", "b"]);
    assert_eq!(functions[0].sites, 3);
    let heaviest = quote_function(&functions[0], &[source]).unwrap();
    assert_eq!(heaviest.function, "heavy");
    assert_eq!(heaviest.source.lines().count(), 81);
    assert!(heaviest.source.ends_with("… 3 more lines"));
}

#[test]
fn inspect_lists_zero_production_refs_apart_from_unresolved() {
    let root = crate_fixture(
        "pub fn dead() {}\npub fn unknown() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn references_dead() { super::dead(); super::dead(); }\n}\n",
    );
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let symbol = "rust-analyzer cargo probe 0.0.0 dead().";
    let index = Index {
        documents: vec![scip::types::Document {
            relative_path: "src/store.rs".to_owned(),
            occurrences: vec![
                occurrence(0, symbol, true),
                occurrence(5, symbol, false),
                occurrence(5, symbol, false),
            ],
            ..scip::types::Document::default()
        }],
        ..Index::default()
    };
    let index_path = root.path().join("index.scip");
    scip::write_message_to_file(&index_path, index).unwrap();
    facts.references = Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());

    let (zero, unresolved) = zero_production_surface(&facts, &selector("store"));

    assert_eq!(zero.len(), 1);
    assert_eq!(zero[0].name, "dead");
    assert_eq!(zero[0].test_referrers, 2);
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].name, "unknown");
}

fn occurrence(line: i32, symbol: &str, definition: bool) -> Occurrence {
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

#[test]
fn inspect_groups_repeated_assembly_across_caller_modules() {
    let edges = ["a", "b", "c", "left_only"]
        .into_iter()
        .map(|item| edge(item, "left", Some(("build", 10)), false))
        .chain(
            ["a", "b", "c", "right_only"]
                .into_iter()
                .map(|item| edge(item, "right", Some(("assemble", 20)), false)),
        )
        .collect::<Vec<_>>();

    let rows = repeated_assembly(&edges, &selector("store"), 20);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].items, ["a", "b", "c"]);
    assert_eq!(rows[0].functions.len(), 2);
    assert_eq!(rows[0].score, 6);
}

#[test]
fn inspect_item_reports_every_validated_introducing_commit() {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), &["init", "--quiet"]);
    fs::write(root.path().join("lib.rs"), "").unwrap();
    commit(root.path(), "initial");
    fs::write(root.path().join("lib.rs"), "pub fn kept() {}\n").unwrap();
    commit(root.path(), "introduce kept");
    fs::write(root.path().join("lib.rs"), "").unwrap();
    commit(root.path(), "remove kept");
    fs::write(root.path().join("lib.rs"), "pub fn kept() {}\n").unwrap();
    commit(root.path(), "fix regression #42");

    let commits = history::introducing_commits(root.path(), Path::new("lib.rs"), "kept").unwrap();

    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "introduce kept");
    assert_eq!(commits[1].subject, "fix regression #42");
    assert_eq!(
        history::fix_markers(&commits[1].subject),
        ["fix regression #42"]
    );
}

#[test]
fn inspect_item_surfaces_persisted_verdict() {
    let target = Target {
        version: 5,
        layers: Vec::new(),
        modules: Vec::new(),
        strangler: Vec::new(),
        verdicts: vec![Verdict {
            kind: VerdictKind::Item,
            key: "store::kept".to_owned(),
            reason: "public compatibility seam".to_owned(),
        }],
    };

    let verdict = item_verdict(&target, "store::kept").unwrap();

    assert_eq!(verdict.reason, "public compatibility seam");
    assert!(item_verdict(&target, "store::missing").is_none());
}

#[test]
fn inspect_item_and_verdicts_report_name_collisions() {
    let root = crate_fixture(
        "pub struct Left;\npub struct Right;\nimpl Left { pub fn open() {} }\nimpl Right { pub fn open() {} }\npub fn forward(value: usize) { target(value) }\n",
    );
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let target = Target {
        version: 5,
        layers: Vec::new(),
        modules: Vec::new(),
        strangler: Vec::new(),
        verdicts: vec![
            Verdict {
                kind: VerdictKind::Item,
                key: "store::open".to_owned(),
                reason: "collision probe".to_owned(),
            },
            Verdict {
                kind: VerdictKind::PassThrough,
                key: "store::forward".to_owned(),
                reason: "known pass-through".to_owned(),
            },
        ],
    };

    let error = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        Some(&target),
        "store::open",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ambiguous: 2 public items named open"));
    assert!(error.contains("owner Left"));
    assert!(error.contains("owner Right"));

    let diagnostics = stale_module_verdicts(&target, "store", &facts);
    assert!(diagnostics.stale.is_empty());
    assert_eq!(diagnostics.ambiguous.len(), 1);
    assert!(diagnostics.ambiguous[0].contains("src/store.rs:3 (owner Left)"));
    assert!(diagnostics.ambiguous[0].contains("src/store.rs:4 (owner Right)"));
}

#[test]
fn target_rule_rows_use_each_rules_files_and_resolved_admissions() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("crates/demo/src/store")).unwrap();
    fs::write(root.path().join("crates/demo/src/store.rs"), "").unwrap();
    let sources = [
        Source::new(
            "crates/demo/src/store.rs",
            "fn run() { crate::harness::target::wake(); crate::agents::run(); }",
        ),
        Source::new(
            "crates/demo/src/store/atomic.rs",
            "fn save() { crate::diag::record(); }",
        ),
        Source::new("crates/demo/src/harness/target.rs", "pub fn wake() {}"),
        Source::new("crates/demo/src/agents.rs", "pub fn run() {}"),
        Source::new("crates/demo/src/diag.rs", "pub fn record() {}"),
    ];
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
        syntax,
        sources: sources.to_vec(),
        crate_names: BTreeSet::new(),
        sizes: BTreeMap::new(),
        history: None,
        metrics: None,
        references: None,
    };
    let target = Target {
        version: 5,
        layers: vec![
            vec!["store".into()],
            vec!["harness".into(), "agents".into()],
        ],
        modules: vec![
            ModuleRule {
                path: "crates/demo/src/store".into(),
                allowed_dependencies: None,
                upward_dependencies: Some(vec!["harness::target".into()]),
                surface_budget: 0,
                config_line: 1,
            },
            ModuleRule {
                path: "crates/demo/src/store/atomic.rs".into(),
                allowed_dependencies: Some(Vec::new()),
                upward_dependencies: None,
                surface_budget: 0,
                config_line: 1,
            },
        ],
        strangler: Vec::new(),
        verdicts: Vec::new(),
    };

    let rows = target_rules(root.path(), &target, &facts, &selector("store"));

    let admitted = rows
        .iter()
        .find(|row| row.provider == "harness::target")
        .unwrap();
    assert_eq!(admitted.admitted.as_deref(), Some("harness::target"));
    assert!(rows.iter().any(|row| row.provider == "agents"));
    assert!(rows.iter().any(|row| row.provider == "diag"));
    assert!(!rows.iter().any(|row| {
        row.path == Path::new("crates/demo/src/store/atomic.rs") && row.provider == "agents"
    }));
}

fn crate_fixture(store: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(root.path().join("src/lib.rs"), "mod store;\n").unwrap();
    fs::write(root.path().join("src/store.rs"), store).unwrap();
    root
}

fn edge(item: &str, from: &str, function: Option<(&str, usize)>, test: bool) -> Edge {
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

fn run(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(root: &Path, message: &str) {
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
