use std::fs;
use std::path::{Path, PathBuf};

use scip::types::Index;

use super::super::super::facts::{Facets, Facts};
use super::super::super::references::References;
use super::super::testkit::{commit, crate_with_files, occurrence, run, selector};
use super::*;

fn testkit_fixture() -> (tempfile::TempDir, Facts) {
    let root = crate_with_files(&[
        ("src/lib.rs", "pub mod render;\nmod app;\n"),
        (
            "src/render.rs",
            "pub fn cap() {}\npub fn render(alert: Option<u8>) {}\n",
        ),
        (
            "src/app.rs",
            "#[cfg(feature = \"testkit\")]\nmod demo;\nfn frame() { crate::render::render(None); }\n#[cfg(feature = \"testkit\")]\nfn fixture() { crate::render::cap(); crate::render::render(None); }\n#[cfg(test)]\nfn test() { crate::render::render(Some(1)); }\n",
        ),
        ("src/app/demo.rs", "fn demo() { crate::render::cap(); }\n"),
    ]);
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let cap = "rust-analyzer cargo probe 0.0.0 cap().";
    let render = "rust-analyzer cargo probe 0.0.0 render().";
    let index = Index {
        documents: [
            (
                "src/render.rs",
                vec![occurrence(0, cap, true), occurrence(1, render, true)],
            ),
            (
                "src/app.rs",
                vec![
                    occurrence(2, render, false),
                    occurrence(4, cap, false),
                    occurrence(4, render, false),
                    occurrence(6, render, false),
                ],
            ),
            ("src/app/demo.rs", vec![occurrence(0, cap, false)]),
        ]
        .into_iter()
        .map(|(path, occurrences)| scip::types::Document {
            relative_path: path.to_owned(),
            occurrences,
            ..Default::default()
        })
        .collect(),
        ..Index::default()
    };
    let index_path = root.path().join("index.scip");
    scip::write_message_to_file(&index_path, index).unwrap();
    facts.references = Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());
    (root, facts)
}

#[test]
fn testkit_readers_floor_surface_without_production_counts() {
    let (root, facts) = testkit_fixture();
    let (surface, _) = surface_section(&facts, &selector("render"));
    let cap = surface.items.iter().find(|row| row.name == "cap").unwrap();
    assert_eq!(cap.narrow_to, "pub(crate)");
    assert_eq!(
        (cap.outside_sites, cap.internal_sites, cap.test_sites),
        (0, 0, 0)
    );
    assert_eq!(serde_json::to_value(cap).unwrap()["testkit_sites"], 2);
    assert!(
        vestigial_items(root.path(), &surface.items)
            .unwrap()
            .is_empty()
    );
    let mut rendered = String::new();
    render_surface(&mut rendered, &surface, 20);
    assert!(rendered.contains("| internal | testkit | tests |"));
}

#[test]
fn testkit_calls_join_constant_flags_but_tests_do_not() {
    let (_root, facts) = testkit_fixture();
    let target = selector("render");
    let (surface, _) = surface_section(&facts, &target);
    let flags = super::super::flags::flag_section(&facts, &target, &surface);
    assert_eq!(flags.rows.len(), 1);
    let row = &flags.rows[0];
    assert_eq!(
        (row.param.as_str(), row.finding, row.finding_value.as_str()),
        ("alert", "constant", "None")
    );
    assert_eq!(row.values[0].1, 2);
}

#[test]
fn surface_measures_outside_reach_and_the_unreferenced_rest() {
    let root = crate_with_files(&[
        ("src/lib.rs", "mod store;\nmod cli;\n"),
        (
            "src/store.rs",
            "pub fn open() {}\npub fn dead() {}\npub fn unknown() {}\nmod inner { pub fn helper() {} }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn references_dead() { super::dead(); super::dead(); }\n}\n",
        ),
        (
            "src/cli.rs",
            "fn run() { crate::store::open(); crate::store::open(); }\nfn also() { crate::store::open(); }\n#[cfg(test)]\nmod tests { fn t() { crate::store::inner::helper(); crate::store::open(); } }\n",
        ),
    ]);
    run(root.path(), &["init", "--quiet"]);
    run(root.path(), &["add", "-A"]);
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
            "introduce fixture",
        ],
    );
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let open = "rust-analyzer cargo probe 0.0.0 open().";
    let dead = "rust-analyzer cargo probe 0.0.0 dead().";
    let helper = "rust-analyzer cargo probe 0.0.0 inner/helper().";
    let index = Index {
        documents: vec![
            scip::types::Document {
                relative_path: "src/store.rs".to_owned(),
                occurrences: vec![
                    occurrence(0, open, true),
                    occurrence(1, dead, true),
                    occurrence(3, helper, true),
                    occurrence(7, dead, false),
                    occurrence(7, dead, false),
                ],
                ..scip::types::Document::default()
            },
            scip::types::Document {
                relative_path: "src/cli.rs".to_owned(),
                occurrences: vec![
                    occurrence(0, open, false),
                    occurrence(0, open, false),
                    occurrence(1, open, false),
                    occurrence(3, helper, false),
                    occurrence(3, open, false),
                ],
                ..scip::types::Document::default()
            },
        ],
        ..Index::default()
    };
    let index_path = root.path().join("index.scip");
    scip::write_message_to_file(&index_path, index).unwrap();
    facts.references = Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());

    let (mut surface, declaration_only) = surface_section(&facts, &selector("store"));
    surface.vestigial = vestigial_items(root.path(), &surface.items).unwrap();

    let names = surface
        .items
        .iter()
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["open", "dead"]);
    let open = &surface.items[0];
    assert_eq!(
        (
            open.outside_sites,
            open.outside_files,
            open.internal_sites,
            open.test_sites
        ),
        (3, 1, 0, 1)
    );
    assert_eq!(open.callers, ["cli"]);
    assert_eq!(open.reach, "crate");
    assert_eq!(open.narrow_to, "keep");
    assert_eq!(surface.outside_sites, 3);
    assert_eq!(surface.head_items, 1);
    assert_eq!(surface.single_site, 0);
    assert_eq!(surface.internal_only, 0);
    assert_eq!(surface.vestigial.len(), 1);
    assert_eq!(surface.vestigial[0].name, "dead");
    assert_eq!(surface.vestigial[0].test_referrers, 2);
    assert_eq!(surface.unresolved.len(), 1);
    assert_eq!(surface.unresolved[0].name, "unknown");
    assert_eq!(declaration_only, 0);
}
#[test]
fn vestigial_items_need_zero_production_sites_and_keep_optional_blame() {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), &["init", "--quiet"]);
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("lib.rs"), "").unwrap();
    fs::write(
        root.path().join("src/store.rs"),
        "pub fn stale() {}\npub fn live() {\n}\npub fn one_site() {}\npub fn busy() {}\n",
    )
    .unwrap();
    run(root.path(), &["add", "-A"]);
    commit(root.path(), "introduce store");
    fs::write(
        root.path().join("src/store.rs"),
        "pub fn stale() {}\npub fn live() {\n    let _ = 1;\n}\npub fn one_site() {}\npub fn busy() {}\n",
    )
    .unwrap();
    run(root.path(), &["add", "-A"]);
    commit(root.path(), "touch live");
    let row = |name: &str, line, end_line, outside_sites, internal_sites| SurfaceRow {
        module: "store".to_owned(),
        name: name.to_owned(),
        kind: "fn".to_owned(),
        reach: "crate".to_owned(),
        narrow_to: "private".to_owned(),
        path: PathBuf::from("src/store.rs"),
        line,
        end_line,
        sloc: end_line - line + 1,
        outside_sites,
        outside_files: outside_sites,
        callers: Vec::new(),
        internal_sites,
        testkit_sites: 0,
        test_sites: 0,
        reexport_of: None,
        definition: (
            PathBuf::from("src/store.rs"),
            "store".to_owned(),
            name.to_owned(),
            line,
        ),
    };
    let rows = [
        row("stale", 1, 1, 0, 0),
        row("live", 2, 4, 0, 0),
        row("one_site", 5, 5, 1, 0),
        row("busy", 6, 6, 0, 3),
    ];

    let vestigial = vestigial_items(root.path(), &rows).unwrap();

    assert_eq!(vestigial.len(), 2, "{vestigial:?}");
    let stale = vestigial.iter().find(|item| item.name == "stale").unwrap();
    assert!(!stale.pins_fix);
    assert_eq!(
        stale.introduced.as_ref().unwrap().summary,
        "introduce store"
    );
    assert!(
        vestigial
            .iter()
            .find(|item| item.name == "live")
            .unwrap()
            .introduced
            .is_none()
    );
}

#[test]
fn narrow_visibility_covers_callers_without_exceeding_them() {
    let callers = |modules: &[&str]| {
        modules
            .iter()
            .map(|module| (*module).to_owned())
            .collect::<BTreeSet<_>>()
    };

    assert_eq!(
        narrow_to("store", EXTERNAL_REACH, &callers(&[]), false),
        "private"
    );
    assert_eq!(
        narrow_to(
            "store::writer",
            EXTERNAL_REACH,
            &callers(&["store::reader"]),
            false
        ),
        "pub(super)"
    );
    assert_eq!(
        narrow_to("store::writer", EXTERNAL_REACH, &callers(&["cli"]), false),
        "pub(crate)"
    );
    assert_eq!(
        narrow_to(
            "store::writer::record",
            EXTERNAL_REACH,
            &callers(&["store::reader"]),
            false
        ),
        "pub(in crate::store)"
    );
    assert_eq!(
        narrow_to(
            "store::writer",
            "store",
            &callers(&["store::reader"]),
            false
        ),
        "keep"
    );
    // Descendants see private items already.
    assert_eq!(
        narrow_to(
            "message",
            EXTERNAL_REACH,
            &callers(&["message::deliver", "message::send"]),
            false
        ),
        "private"
    );
    // A caller in the binary crate needs the item at least `pub`.
    assert_eq!(
        narrow_to(
            "store::writer",
            EXTERNAL_REACH,
            &callers(&["cli::show"]),
            true
        ),
        "keep"
    );
}

#[test]
fn cross_target_readers_keep_visibility_for_every_site_class() {
    let sites = [
        (
            "bench",
            "benches/hot.rs",
            "fn run() { probe::render::bench(); }",
            0,
        ),
        ("bin", "src/cli.rs", "fn run() { probe::render::bin(); }", 0),
        (
            "bin_test",
            "src/cli/tests.rs",
            "fn run() { probe::render::bin_test(); }",
            0,
        ),
        (
            "bin_support",
            "src/cli/demo.rs",
            "fn run() { probe::render::bin_support(); }",
            0,
        ),
        (
            "tool",
            "src/bin/tool.rs",
            "fn main() { probe::render::tool(); }",
            0,
        ),
        (
            "main",
            "src/main.rs",
            "mod cli;\nfn main() { probe::render::main(); }",
            1,
        ),
        (
            "api",
            "tests/api.rs",
            "fn test() { probe::render::api(); }",
            0,
        ),
        (
            "example",
            "examples/demo.rs",
            "fn main() { probe::render::example(); }",
            0,
        ),
        (
            "foreign",
            "other/src/reader.rs",
            "fn run() { probe::render::foreign(); }",
            0,
        ),
        (
            "local",
            "src/app.rs",
            "fn run() { crate::render::local(); }",
            0,
        ),
    ];
    let declarations = sites
        .iter()
        .map(|(name, ..)| format!("pub fn {name}() {{}}\n"))
        .collect::<String>();
    let mut files = vec![
        ("src/lib.rs", "pub mod render;\nmod app;\n"),
        ("src/render.rs", declarations.as_str()),
        ("other/src/main.rs", "mod app;\n"),
        ("other/src/lib.rs", "mod cli;\nmod reader;\n"),
    ];
    files.extend(sites.iter().map(|(_, path, text, _)| (*path, *text)));
    let root = crate_with_files(&files);
    fs::write(root.path().join("src/cli.rs"), "fn run() { probe::render::bin(); }\n#[cfg(test)]\nmod tests;\n#[cfg(feature = \"testkit\")]\nmod demo;\n").unwrap();
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let mut definitions = Vec::new();
    let mut documents = Vec::new();
    for (line, (name, path, _, site_line)) in sites.iter().enumerate() {
        let symbol = format!("rust-analyzer cargo probe 0.0.0 {name}().");
        definitions.push(occurrence(i32::try_from(line).unwrap(), &symbol, true));
        documents.push(scip::types::Document {
            relative_path: (*path).to_owned(),
            occurrences: vec![occurrence(*site_line, &symbol, false)],
            ..Default::default()
        });
    }
    documents.push(scip::types::Document {
        relative_path: "src/render.rs".to_owned(),
        occurrences: definitions,
        ..Default::default()
    });
    let index_path = root.path().join("index.scip");
    scip::write_message_to_file(
        &index_path,
        Index {
            documents,
            ..Default::default()
        },
    )
    .unwrap();
    facts.references = Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());
    let (surface, _) = surface_section(&facts, &selector("render"));
    assert_eq!(surface.items.len(), sites.len());
    for row in &surface.items {
        assert_eq!(
            row.narrow_to,
            if row.name == "local" {
                "pub(crate)"
            } else {
                "keep"
            },
            "{}",
            row.name
        );
    }
    assert!(surface.pins.is_empty());
}

#[test]
fn surface_measures_reexports_at_their_definitions_and_pins_tests_past_the_narrowing() {
    let root = crate_with_files(&[
        ("src/lib.rs", "pub mod store;\nmod cli;\n"),
        ("src/store.rs", "mod row;\npub use row::{Row, Hidden};\n"),
        ("src/store/row.rs", "pub struct Row;\npub struct Hidden;\n"),
        (
            "src/cli.rs",
            "fn run() { let _ = crate::store::Row; }\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = crate::store::Hidden; }\n}\n",
        ),
        ("tests/api.rs", "fn t() { let _ = probe::store::Row; }\n"),
    ]);
    run(root.path(), &["init", "--quiet"]);
    run(root.path(), &["add", "-A"]);
    let mut facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();
    let row = "rust-analyzer cargo probe 0.0.0 store/row/Row#";
    let hidden = "rust-analyzer cargo probe 0.0.0 store/row/Hidden#";
    let index = Index {
        documents: vec![
            scip::types::Document {
                relative_path: "src/store/row.rs".to_owned(),
                occurrences: vec![occurrence(0, row, true), occurrence(1, hidden, true)],
                ..scip::types::Document::default()
            },
            scip::types::Document {
                relative_path: "src/cli.rs".to_owned(),
                occurrences: vec![occurrence(0, row, false), occurrence(3, hidden, false)],
                ..scip::types::Document::default()
            },
            scip::types::Document {
                relative_path: "tests/api.rs".to_owned(),
                occurrences: vec![occurrence(0, row, false)],
                ..scip::types::Document::default()
            },
        ],
        ..Index::default()
    };
    let index_path = root.path().join("index.scip");
    scip::write_message_to_file(&index_path, index).unwrap();
    facts.references = Some(References::load(&index_path, &facts.syntax, &facts.sources).unwrap());

    let (surface, declaration_only) = surface_section(&facts, &selector("store"));

    assert_eq!(declaration_only, 0);
    assert_eq!(surface.reexports, 2);
    assert_eq!(
        surface.declarations, 2,
        "the two `pub use` leaves are the raw esc the other verbs count"
    );
    let keys = surface
        .items
        .iter()
        .map(|item| format!("{}::{} {}", item.module, item.name, item.kind))
        .collect::<Vec<_>>();
    assert_eq!(keys, ["store::Row struct", "store::Hidden struct"]);
    let row = &surface.items[0];
    assert_eq!(row.reexport_of.as_deref(), Some("store::row"));
    assert_eq!(row.path, Path::new("src/store/row.rs"));
    assert_eq!(row.line, 1);
    assert_eq!((row.outside_sites, row.test_sites), (1, 1));
    assert_eq!(row.reach, "extern");
    assert_eq!(row.narrow_to, "keep");
    let hidden = &surface.items[1];
    assert_eq!((hidden.outside_sites, hidden.test_sites), (0, 1));
    assert_eq!(hidden.narrow_to, "private");

    let pins = surface
        .pins
        .iter()
        .map(|pin| {
            (
                format!("{}::{}", pin.module, pin.name),
                pin.narrow_to.as_str(),
                pin.lost_sites,
                pin.tests.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pins,
        [(
            "store::Hidden".to_owned(),
            "private",
            1,
            vec!["t (src/cli.rs:4)".to_owned()]
        ),]
    );
}
