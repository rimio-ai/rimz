use std::fs;

use super::super::sources::Source;
use super::super::target::Verdict;
use super::testkit::{commit, crate_with_files, run, selector};
use super::*;

#[test]
fn inspect_args_require_a_module_and_parse_output_flags() {
    let args = parse_args(&[
        "--module".into(),
        "crate::store".into(),
        "--from".into(),
        "cli".into(),
        "--item".into(),
        "store::open".into(),
        "--top".into(),
        "4".into(),
        "--json".into(),
        "--section".into(),
        "callers,item".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(args.module, "crate::store");
    assert_eq!(args.from.as_deref(), Some("cli"));
    assert_eq!(args.item.as_deref(), Some("store::open"));
    assert_eq!(args.top, 4);
    assert!(args.output.json);
    assert!(args.output.wants("callers"));
    assert!(args.output.wants("item"));
    assert!(!args.output.wants("surface"));
    assert!(
        parse_args(&[])
            .unwrap_err()
            .to_string()
            .contains("--module")
    );
    assert!(
        parse_args(&["--module".into(), "store".into(), "--no-index".into()])
            .unwrap_err()
            .to_string()
            .contains("SCIP")
    );
}
#[test]
fn module_item_guards_keep_only_guards_naming_the_modules_items() {
    let caller = |name: &str| {
        format!(
            "fn {name}(s: &crate::store::S, v: &[u8]) {{\n    if s.phase == crate::store::Phase::Ready {{}}\n    if s.is_ready() {{}}\n    if crate::cli::is_stale(v) && v.len() > 2 {{}}\n    if crate::event::poll(v) {{}}\n}}\n"
        )
    };
    let root = crate_with_files(&[
        (
            "src/lib.rs",
            "mod store;\nmod cli;\nmod event;\nmod other;\nmod a;\nmod b;\nmod c;\n",
        ),
        (
            "src/event.rs",
            "pub fn poll(v: &[u8]) -> bool { v.is_empty() }\n",
        ),
        ("src/other.rs", "pub fn poll() -> bool { true }\n"),
        (
            "src/store.rs",
            "#[derive(PartialEq)]\npub enum Phase { Ready }\npub struct S { pub phase: Phase }\nimpl S {\n    pub fn is_ready(&self) -> bool { true }\n}\n",
        ),
        (
            "src/cli.rs",
            "pub fn is_stale(v: &[u8]) -> bool { v.is_empty() }\n",
        ),
        ("src/a.rs", &caller("a")),
        ("src/b.rs", &caller("b")),
        ("src/c.rs", &caller("c")),
    ]);
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();

    let families = module_item_guards(&facts, &selector("store"), false);

    // `s.is_ready()` alone is predicate use, not composed knowledge.
    assert_eq!(families.len(), 1, "{families:?}");
    assert!(families[0].key.contains("Phase::Ready"));
    assert_eq!(families[0].files, 3);
    let all = module_item_guards(&facts, &selector("store"), true);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all.iter().any(|family| family.key.contains("is_ready")));
    let cli = module_item_guards(&facts, &selector("cli"), false);
    assert_eq!(cli.len(), 1, "{cli:?}");
    assert!(cli[0].key.contains("is_stale"));
    // `event::poll` names `event`'s item, not `other`'s same-named `poll`.
    let event = module_item_guards(&facts, &selector("event"), false);
    assert_eq!(event.len(), 1, "{event:?}");
    assert!(event[0].key.contains("event::poll"));
    assert!(module_item_guards(&facts, &selector("other"), false).is_empty());
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
    assert_eq!(
        commit_markers(&commits),
        [format!("{} fix regression #42", commits[1].short)]
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
        defined_names: super::super::facts::defined_names(&syntax),
        unique_fields: super::super::facts::unique_fields(&syntax),
        defining_modules: super::super::facts::defining_modules(&syntax),
        bin_modules: super::super::facts::bin_modules(&syntax),
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
