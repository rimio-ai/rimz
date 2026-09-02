use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::*;

#[test]
fn rows_are_ranked_by_churn_weighted_size() {
    let row = |module: &str, code, churn, cx| Row {
        module: module.to_owned(),
        code,
        churn,
        cx,
        ..Row::default()
    };
    let mut rows = vec![
        row("cold", 1_000, 0.0, 100.0),
        row("small-hot", 100, 20.0, 1.0),
        row("large-warm", 500, 5.0, 2.0),
        row("cold-low-cx", 2_000, 0.0, 1.0),
    ];

    sort_rows(&mut rows);

    assert_eq!(
        rows.iter()
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>(),
        ["large-warm", "small-hot", "cold", "cold-low-cx"]
    );
}

#[test]
fn pin_thresholds_match_the_documented_boundaries() {
    assert!(is_pinned(3.4, Some(0.15)));
    assert!(!is_pinned(19.3, Some(0.30)));
    assert!(!is_pinned(2.9, Some(0.10)));
}

#[test]
fn totals_line_uses_scope_totals_not_the_top_n_rows() {
    let rows = vec![
        Row {
            module: "shown".to_owned(),
            code: 10,
            tests: 2,
            esc: 1,
            cx: 3.0,
            ..Row::default()
        },
        Row {
            module: "hidden".to_owned(),
            code: 50,
            tests: 13,
            esc: 5,
            cx: 7.0,
            ..Row::default()
        },
    ];

    assert_eq!(
        totals(&rows),
        Totals {
            code: 60,
            tests: 15,
            esc: 6,
            cx: 10.0,
        }
    );
    assert_eq!(rows.iter().take(1).map(|row| row.code).sum::<u64>(), 10);
}

#[test]
fn split_leaves_replace_the_parent_and_consume_top() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src/large/sub")).unwrap();
    std::fs::write(root.path().join("src/large.rs"), "pub fn large() {}\n").unwrap();
    std::fs::write(root.path().join("src/large/a.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(root.path().join("src/large/sub/b.rs"), "pub fn b() {}\n").unwrap();
    let sources = vec![
        super::super::sources::Source::new("src/large.rs", "pub fn large() {}\n"),
        super::super::sources::Source::new("src/large/a.rs", "pub fn a() {}\n"),
        super::super::sources::Source::new("src/large/sub/b.rs", "pub fn b() {}\n"),
    ];
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    let mod_index = super::super::syntax::ModIndex::new(&syntax.files);
    let facts = Facts {
        root: root.path().to_path_buf(),
        scope: PathBuf::from("src"),
        sources,
        known_modules: syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect(),
        defined_names: super::super::facts::defined_names(&syntax),
        crate_names: BTreeSet::new(),
        sizes: BTreeMap::from([
            (
                PathBuf::from("src/large.rs"),
                FileSize {
                    code: 100,
                    tests: 1,
                },
            ),
            (
                PathBuf::from("src/large/a.rs"),
                FileSize {
                    code: 4_500,
                    tests: 2,
                },
            ),
            (
                PathBuf::from("src/large/sub/b.rs"),
                FileSize {
                    code: 4_501,
                    tests: 3,
                },
            ),
        ]),
        syntax,
        mod_index,
        history: Some(history::Log::empty()),
        metrics: Some(super::super::metrics::MetricsReport {
            module_scores: BTreeMap::new(),
            functions: Vec::new(),
        }),
        references: None,
    };

    let rows = rows(&facts, Path::new("src")).unwrap();

    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.module != "large"));
    assert_eq!(rows.iter().map(|row| row.code).sum::<u64>(), 9_101);
    assert_eq!(rows.iter().map(|row| row.tests).sum::<u64>(), 6);
    assert_eq!(
        rows.iter()
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>(),
        ["large/sub", "large/a", "large/(root)"]
    );
    assert_eq!(rows.iter().take(1).count(), 1);
}
