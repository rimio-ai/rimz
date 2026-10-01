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
        parse_args(&["--item".into(), "open".into()])
            .unwrap_err()
            .to_string()
            .contains("--item <module::Name>")
    );
    let implied = parse_args(&["--item".into(), "store::snapshot::Fold".into()])
        .unwrap()
        .unwrap();
    assert_eq!(implied.module, "store::snapshot");
    assert_eq!(implied.item.as_deref(), Some("store::snapshot::Fold"));
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
    assert!(error.contains("ambiguous: 2 visible items named open"));
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
            "fn run() { crate::harness::target::wait(); crate::agents::run(); }",
        ),
        Source::new(
            "crates/demo/src/store/atomic.rs",
            "fn save() { crate::diag::record(); }",
        ),
        Source::new("crates/demo/src/harness/target.rs", "pub fn wait() {}"),
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
        binaries: super::super::modules::BinaryTargets::new(&syntax.files),
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

#[test]
fn record_reads_the_root_files_header_and_the_nearest_contract() {
    let root = crate_with_files(&[
        ("src/lib.rs", "//! Demo crate.\nmod store;\nmod lane;\n"),
        (
            "src/store/mod.rs",
            "//! Durable state engine.\n//! Every write is fsynced.\n//!\n//! Not part of the head.\npub fn open() {}\n",
        ),
        ("src/store/AGENTS.md", "# store\n"),
        ("src/lane.rs", "pub fn lane() {}\n"),
        ("AGENTS.md", "# crate\n"),
    ]);
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();

    let store = record(root.path(), &facts, &selector("store"));
    assert_eq!(store.path.as_deref(), Some(Path::new("src/store/mod.rs")));
    assert_eq!(
        store.header.as_deref(),
        Some("Durable state engine. Every write is fsynced.")
    );
    assert_eq!(
        store.contract.as_deref(),
        Some(Path::new("src/store/AGENTS.md"))
    );

    let lane = record(root.path(), &facts, &selector("lane"));
    assert_eq!(lane.path.as_deref(), Some(Path::new("src/lane.rs")));
    assert_eq!(lane.header, None);
    assert_eq!(lane.contract.as_deref(), Some(Path::new("AGENTS.md")));
}

#[test]
fn inspect_brief_presets_sections_and_top_unless_given() {
    let args = parse_args(&["--module".into(), "store".into(), "--brief".into()])
        .unwrap()
        .unwrap();
    assert_eq!(args.top, BRIEF_TOP);
    for section in BRIEF_SECTIONS {
        assert!(args.output.wants(section), "{section}");
    }
    assert!(!args.output.wants("callers"));
    assert!(!args.output.wants("providers"));

    let args = parse_args(&[
        "--module".into(),
        "store".into(),
        "--brief".into(),
        "--top".into(),
        "3".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(args.top, 3);

    let error = parse_args(&[
        "--module".into(),
        "store".into(),
        "--brief".into(),
        "--section".into(),
        "verdict".into(),
    ])
    .unwrap_err()
    .to_string();
    assert!(error.contains("mutually exclusive"), "{error}");
}

#[test]
fn inspect_brief_surface_lists_every_narrowable_item_past_top() {
    let row = |name: &str, narrow_to: &str| surface::SurfaceRow {
        module: "store".into(),
        name: name.into(),
        kind: "fn".into(),
        reach: "pub".into(),
        narrow_to: narrow_to.into(),
        path: PathBuf::from("src/store.rs"),
        line: 1,
        end_line: 1,
        sloc: 1,
        outside_sites: 0,
        outside_files: 0,
        callers: Vec::new(),
        internal_sites: 0,
        testkit_sites: 0,
        test_sites: 0,
        reexport_of: None,
        definition: (PathBuf::new(), String::new(), String::new(), 0),
    };
    let surface = SurfaceSection {
        items: vec![
            row("first", "keep"),
            row("late_keep", "keep"),
            row("late_narrow", "pub(crate)"),
        ],
        ..SurfaceSection::default()
    };
    let mut rendered = String::new();
    render_surface(&mut rendered, &surface, 1, true);
    assert!(rendered.contains("`store::late_narrow`"), "{rendered}");
    assert!(!rendered.contains("`store::late_keep`"), "{rendered}");
    assert!(rendered.contains("_1 more items omitted._"), "{rendered}");

    let mut rendered = String::new();
    render_surface(&mut rendered, &surface, 1, false);
    assert!(!rendered.contains("`store::late_narrow`"), "{rendered}");
    assert!(rendered.contains("_2 more items omitted._"), "{rendered}");
}

fn verdicts(verdicts: &[(VerdictKind, &str)]) -> Target {
    Target {
        version: 5,
        layers: Vec::new(),
        modules: Vec::new(),
        strangler: Vec::new(),
        verdicts: verdicts
            .iter()
            .map(|(kind, key)| Verdict {
                kind: *kind,
                key: (*key).to_owned(),
                reason: "probe".to_owned(),
            })
            .collect(),
    }
}

#[test]
fn item_verdicts_resolve_private_functions_after_public_items() {
    let root = crate_fixture(
        "pub struct Left;\nimpl Left { fn open(&self) {} }\npub fn open() {}\nfn helper() {}\nfn run() { helper() }\n",
    );
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let target = verdicts(&[
        (VerdictKind::Item, "store::helper"),
        (VerdictKind::Item, "store::gone"),
        (VerdictKind::Item, "store::open"),
    ]);

    let diagnostics = stale_module_verdicts(&target, "store", &facts);

    assert_eq!(diagnostics.stale, ["Item:store::gone"]);
    assert!(
        diagnostics.ambiguous.is_empty(),
        "the public `open` wins over the private method: {:?}",
        diagnostics.ambiguous
    );
    let check = check_item_verdicts(&target, "", &facts);
    assert_eq!(
        check
            .stale
            .iter()
            .map(|verdict| verdict.key.as_str())
            .collect::<Vec<_>>(),
        ["store::gone"]
    );
}

#[test]
fn inspect_item_resolves_private_functions_with_their_referrers() {
    let root = crate_fixture(
        "pub struct Left;\npub struct Right;\nimpl Left { fn shut(&self) {} }\nimpl Right { fn shut(&self) {} }\npub fn open() {}\nimpl Left { fn open(&self) {} }\n\nfn helper() -> usize {\n    1\n}\n\npub fn run() -> usize {\n    helper()\n}\n",
    );
    run(root.path(), &["init", "--quiet"]);
    run(root.path(), &["add", "."]);
    run(
        root.path(),
        &[
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=atlas@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "introduce helper",
        ],
    );
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let file = facts
        .syntax
        .files
        .iter()
        .find(|file| file.path == Path::new("src/store.rs"))
        .unwrap();
    let line = |name: &str| {
        file.fns
            .iter()
            .find(|function| function.name == name)
            .unwrap()
            .line
    };
    let (helper, caller) = (line("helper"), line("run"));
    let mut reference = super::testkit::edge(
        "helper",
        "store",
        Some(("run", caller)),
        SourceKind::Production,
    );
    reference.from_path = PathBuf::from("src/store.rs");
    reference.to_path = PathBuf::from("src/store.rs");
    reference.to_line = helper;
    facts.references = Some(super::super::references::References {
        fn_edges: vec![reference],
        ..Default::default()
    });
    let target = verdicts(&[(VerdictKind::Item, "store::helper")]);

    let evidence = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        Some(&target),
        "store::helper",
    )
    .unwrap();

    assert_eq!(evidence.key, "store::helper");
    assert_eq!(
        (evidence.path.as_path(), evidence.line),
        (Path::new("src/store.rs"), helper)
    );
    assert_eq!(evidence.sloc, 3);
    assert_eq!(evidence.declared, "private");
    assert_eq!(evidence.effective_reach, "store");
    assert_eq!(evidence.production_referrers, ["store::run"]);
    assert!(evidence.test_referrers.is_empty());
    assert_eq!(evidence.commits.len(), 1);
    assert_eq!(evidence.verdict.unwrap().reason, "probe");

    let error = item_evidence(root.path(), &facts, &selector("store"), None, "shut")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("ambiguous: 2 production functions named shut"),
        "{error}"
    );
    assert!(
        error.contains("owner Left") && error.contains("owner Right"),
        "{error}"
    );
    let owned = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        None,
        "store::Right::shut",
    )
    .unwrap();
    assert_eq!(owned.key, "store::Right::shut");
    assert_eq!(owned.line, line("shut") + 1);
    let public =
        item_evidence(root.path(), &facts, &selector("store"), None, "store::open").unwrap();
    assert_eq!(
        public.declared, "pub",
        "a public item wins over a private method of the same name"
    );
    let error = item_evidence(root.path(), &facts, &selector("store"), None, "store::gone")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("names no item or production function"),
        "{error}"
    );
}

/// A crate mirroring the verdict keys the resolver once missed: private
/// items in a private child (`disk::usage::FileIdentity`), a private trait at
/// a module root (`sidebar::SidebarMux`), a method keyed by its owner
/// (`lsp::check::Report::render`), and a method in a private child module
/// keyed from its parent (`mux::zellij::converge_presence_plugin_for`).
fn resolver_fixture() -> tempfile::TempDir {
    crate_with_files(&[
        ("src/lib.rs", "mod store;\n"),
        (
            "src/store.rs",
            "mod usage;\nmod presence;\ntrait SidebarMux {}\nstruct Shared {}\npub struct Report;\nimpl Report {\n    pub fn render(&self) {}\n}\n",
        ),
        (
            "src/store/usage.rs",
            "struct FileIdentity {\n    dev: u64,\n}\n\nenum RequiredDecision {\n    Run,\n}\n\npub fn walk() -> usize {\n    let identity = FileIdentity { dev: 0 };\n    identity.dev as usize\n}\n",
        ),
        (
            "src/store/presence.rs",
            "pub struct Shared;\npub struct Zellij;\nimpl Zellij {\n    pub fn converge_presence_plugin_for(&self) {}\n}\n",
        ),
    ])
}

#[test]
fn item_verdicts_resolve_private_items_owner_keys_and_child_modules() {
    let root = resolver_fixture();
    let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let target = verdicts(&[
        (VerdictKind::Item, "store::usage::FileIdentity"),
        (VerdictKind::Item, "store::usage::RequiredDecision"),
        (VerdictKind::Item, "store::SidebarMux"),
        (VerdictKind::Item, "store::FileIdentity"),
        (VerdictKind::Item, "store::Report::render"),
        (VerdictKind::Item, "store::render"),
        (VerdictKind::Item, "store::Wrong::render"),
        (VerdictKind::Item, "store::converge_presence_plugin_for"),
        (VerdictKind::Item, "store::Shared"),
        (VerdictKind::Item, "store::Gone"),
    ]);

    let diagnostics = stale_module_verdicts(&target, "store", &facts);

    assert_eq!(
        diagnostics.stale,
        ["Item:store::Gone", "Item:store::Wrong::render"]
    );
    assert!(
        diagnostics.ambiguous.is_empty(),
        "a visible `Shared` beneath the module wins over the private one in it: {:?}",
        diagnostics.ambiguous
    );
}

#[test]
fn inspect_item_resolves_private_items_with_their_referrers() {
    let root = resolver_fixture();
    run(root.path(), &["init", "--quiet"]);
    run(root.path(), &["add", "."]);
    run(
        root.path(),
        &[
            "-c",
            "user.name=Atlas Test",
            "-c",
            "user.email=atlas@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "introduce FileIdentity",
        ],
    );
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let usage = facts
        .syntax
        .files
        .iter()
        .find(|file| file.path == Path::new("src/store/usage.rs"))
        .unwrap();
    let walk = usage
        .fns
        .iter()
        .find(|function| function.name == "walk")
        .unwrap()
        .line;
    let mut reference = super::testkit::edge(
        "FileIdentity",
        "store::usage",
        Some(("walk", walk)),
        SourceKind::Production,
    );
    reference.from_path = PathBuf::from("src/store/usage.rs");
    reference.to_path = PathBuf::from("src/store/usage.rs");
    reference.to = "store::usage".to_owned();
    reference.to_line = 1;
    facts.references = Some(super::super::references::References {
        fn_edges: vec![reference],
        ..Default::default()
    });
    let target = verdicts(&[(VerdictKind::Item, "store::FileIdentity")]);

    let evidence = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        Some(&target),
        "store::FileIdentity",
    )
    .unwrap();

    assert_eq!(evidence.key, "store::usage::FileIdentity");
    assert_eq!(
        (evidence.path.as_path(), evidence.line, evidence.sloc),
        (Path::new("src/store/usage.rs"), 1, 3)
    );
    assert_eq!(evidence.declared, "private");
    assert_eq!(evidence.effective_reach, "store::usage");
    assert_eq!(evidence.production_referrers, ["store::usage::walk"]);
    assert_eq!(evidence.commits.len(), 1);
    assert_eq!(
        evidence.verdict.unwrap().key,
        "store::FileIdentity",
        "a verdict keyed from the parent module reaches the definition"
    );

    let owned = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        None,
        "store::Report::render",
    )
    .unwrap();
    assert_eq!(owned.key, "store::Report::render");
    let child = item_evidence(
        root.path(),
        &facts,
        &selector("store"),
        None,
        "store::converge_presence_plugin_for",
    )
    .unwrap();
    assert_eq!(child.key, "store::presence::converge_presence_plugin_for");
    let shared = item_evidence(root.path(), &facts, &selector("store"), None, "Shared").unwrap();
    assert_eq!(shared.declared, "pub", "the visible item wins");
    let error = item_evidence(root.path(), &facts, &selector("store"), None, "store::Gone")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("names no item or production function"),
        "{error}"
    );
}
