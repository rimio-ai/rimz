use std::fs;
use std::path::{Path, PathBuf};

use scip::types::Index;

use super::super::super::facts::{Facets, Facts};
use super::super::super::references::References;
use super::super::testkit::{commit, crate_with_files, occurrence, run, selector};
use super::*;

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
        outside_sites,
        outside_files: outside_sites,
        callers: Vec::new(),
        internal_sites,
        test_sites: 0,
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

    let no_bins = BTreeSet::new();

    assert_eq!(
        narrow_to("store", EXTERNAL_REACH, &callers(&[]), 0, &no_bins),
        "private"
    );
    assert_eq!(
        narrow_to(
            "store::writer",
            EXTERNAL_REACH,
            &callers(&["store::reader"]),
            1,
            &no_bins
        ),
        "pub(super)"
    );
    assert_eq!(
        narrow_to(
            "store::writer",
            EXTERNAL_REACH,
            &callers(&["cli"]),
            1,
            &no_bins
        ),
        "pub(crate)"
    );
    assert_eq!(
        narrow_to(
            "store::writer::record",
            EXTERNAL_REACH,
            &callers(&["store::reader"]),
            1,
            &no_bins
        ),
        "pub(in crate::store)"
    );
    assert_eq!(
        narrow_to(
            "store::writer",
            "store",
            &callers(&["store::reader"]),
            1,
            &no_bins
        ),
        "keep"
    );
    // Descendants see private items already.
    assert_eq!(
        narrow_to(
            "message",
            EXTERNAL_REACH,
            &callers(&["message::deliver", "message::send"]),
            2,
            &no_bins
        ),
        "private"
    );
    // A caller in the binary crate needs the item at least `pub`.
    assert_eq!(
        narrow_to(
            "store::writer",
            EXTERNAL_REACH,
            &callers(&["cli::show"]),
            1,
            &callers(&["cli"])
        ),
        "keep"
    );
}
