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

    sort_rows(&mut rows, RankBy::Accretion);

    assert_eq!(
        rows.iter()
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>(),
        ["large-warm", "small-hot", "cold", "cold-low-cx"]
    );
}

#[test]
fn rows_can_be_ranked_by_tc_with_thinnest_tests_first() {
    let row = |module: &str, code, tests| Row {
        module: module.to_owned(),
        code,
        tests,
        ..Row::default()
    };
    let mut rows = vec![
        row("well-tested", 100, 80),
        row("thin", 200, 10),
        row("medium", 100, 30),
    ];

    sort_rows(&mut rows, RankBy::parse("tc").unwrap());

    assert_eq!(
        rows.iter()
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>(),
        ["thin", "medium", "well-tested"]
    );
}

#[test]
fn rows_can_be_ranked_by_depth_with_shallowest_first_and_no_surface_last() {
    let row = |module: &str, depth| Row {
        module: module.to_owned(),
        depth,
        ..Row::default()
    };
    let mut rows = vec![
        row("deep", Some(100.0)),
        row("none", None),
        row("shallow", Some(20.0)),
    ];

    sort_rows(&mut rows, RankBy::parse("depth").unwrap());

    assert_eq!(
        rows.iter()
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>(),
        ["shallow", "deep", "none"]
    );
}

#[test]
fn top_complexity_decile_is_flagged() {
    let mut rows = (1..=10)
        .map(|cx| Row {
            module: format!("module-{cx}"),
            cx: cx as f64,
            ..Row::default()
        })
        .collect::<Vec<_>>();

    add_outlier_flags(&mut rows);

    assert!(rows[9].flags.contains(&"cx"));
    assert!(!rows[8].flags.contains(&"cx"));
}

#[test]
fn large_modules_with_thin_tests_are_flagged() {
    let mut rows = vec![
        Row {
            module: "thin".to_owned(),
            code: 200,
            tests: 59,
            ..Row::default()
        },
        Row {
            module: "small".to_owned(),
            code: 199,
            tests: 0,
            ..Row::default()
        },
        Row {
            module: "covered".to_owned(),
            code: 200,
            tests: 60,
            ..Row::default()
        },
    ];

    add_outlier_flags(&mut rows);

    assert!(rows[0].flags.contains(&"thin"));
    assert!(!rows[1].flags.contains(&"thin"));
    assert!(!rows[2].flags.contains(&"thin"));
}

#[test]
fn file_churn_can_make_lower_complexity_function_hotter() {
    let function = |name: &str, path: &str, score| super::super::metrics::FunctionMetric {
        module: "fixture".to_owned(),
        path: PathBuf::from(path),
        name: name.to_owned(),
        line: 10,
        cyclomatic: 0.0,
        cognitive: 0.0,
        sloc: 0.0,
        score,
    };
    let metrics = MetricsReport {
        module_scores: BTreeMap::new(),
        functions: vec![
            function("complex", "src/stable.rs", 10.0),
            function("churning", "src/churning.rs", 5.0),
        ],
    };
    let shares = BTreeMap::from([
        (PathBuf::from("src/stable.rs"), 0.1),
        (PathBuf::from("src/churning.rs"), 0.5),
    ]);

    let hotspots = hotspots(&metrics, &shares);

    assert_eq!(hotspots[0].function, "churning");
    assert_eq!(hotspots[0].hot, 250.0);
    assert_eq!(hotspots[1].hot, 100.0);
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
        unique_fields: super::super::facts::unique_fields(&syntax),
        defining_modules: super::super::facts::defining_modules(&syntax),
        bin_modules: super::super::facts::bin_modules(&syntax),
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

    let rows = rows_by(&facts, Path::new("src"), RankBy::Accretion).unwrap();

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
    assert_eq!(rows[0].depth, Some(4_501.0));
}
