use super::*;

fn metric_sum(sum: u64) -> MetricSum {
    MetricSum { sum: sum as f64 }
}

fn metrics(cyclomatic: u64, cognitive: u64, sloc: u64) -> Metrics {
    Metrics {
        cyclomatic: metric_sum(cyclomatic),
        cognitive: metric_sum(cognitive),
        loc: LocMetrics { sloc: sloc as f64 },
    }
}

fn space(
    name: &str,
    kind: &str,
    start_line: u64,
    cyclomatic: u64,
    cognitive: u64,
    sloc: u64,
    children: Vec<Space>,
) -> Space {
    Space {
        name: name.to_owned(),
        kind: kind.to_owned(),
        start_line,
        spaces: children,
        metrics: metrics(cyclomatic, cognitive, sloc),
    }
}

fn function_metrics(
    name: &str,
    start_line: u64,
    cyclomatic: u64,
    cognitive: u64,
    sloc: u64,
) -> FunctionMetrics {
    FunctionMetrics {
        name: name.to_owned(),
        start_line,
        cyclomatic: cyclomatic as f64,
        cognitive: cognitive as f64,
        sloc: sloc as f64,
    }
}

fn scored(name: &str, start_line: u64, score: f64) -> ScoredFunction {
    ScoredFunction {
        metrics: function_metrics(name, start_line, 16, 16, 1),
        severity: Severity::High,
        score,
    }
}

fn group(path: &str, score: f64) -> FileGroup {
    FileGroup {
        path: PathBuf::from(path),
        score,
        split_first: false,
        offenders: vec![scored("run", 1, score)],
        near: Vec::new(),
    }
}

#[test]
fn parse_complexity_args_accepts_top_n_and_json_in_either_order() {
    assert_eq!(
        parse_complexity_args(&[]).unwrap(),
        ComplexityArgs {
            top_n: DEFAULT_TOP_N,
            json: false,
        }
    );
    assert_eq!(
        parse_complexity_args(&["5".to_owned(), "--json".to_owned()]).unwrap(),
        ComplexityArgs {
            top_n: 5,
            json: true,
        }
    );
    assert_eq!(
        parse_complexity_args(&["--json".to_owned(), "7".to_owned()]).unwrap(),
        ComplexityArgs {
            top_n: 7,
            json: true,
        }
    );
}

#[test]
fn parse_complexity_args_rejects_zero_duplicates_and_unknowns() {
    assert!(parse_complexity_args(&["0".to_owned()]).is_err());
    assert!(parse_complexity_args(&["3".to_owned(), "4".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--json".to_owned(), "--json".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--gate".to_owned()]).is_err());
}

#[test]
fn severity_classification_uses_strict_threshold_boundaries() {
    assert_eq!(classify(&function_metrics("clean", 1, 10, 15, 60)), None);
    assert_eq!(
        classify(&function_metrics("warn", 1, 11, 15, 60)),
        Some(Severity::Warn)
    );
    assert_eq!(
        classify(&function_metrics("high-boundary", 1, 15, 25, 100)),
        Some(Severity::Warn)
    );
    assert_eq!(
        classify(&function_metrics("high", 1, 16, 25, 100)),
        Some(Severity::High)
    );
    assert_eq!(
        classify(&function_metrics("critical-boundary", 1, 25, 50, 100)),
        Some(Severity::High)
    );
    assert_eq!(
        classify(&function_metrics("critical", 1, 26, 50, 100)),
        Some(Severity::Critical)
    );
    assert_eq!(
        classify(&function_metrics("critical-cognitive", 1, 1, 51, 1)),
        Some(Severity::Critical)
    );
    assert_eq!(
        classify(&function_metrics("high-sloc", 1, 1, 1, 101)),
        Some(Severity::High)
    );
}

#[test]
fn score_weights_overruns_from_warn_thresholds() {
    let high = function_metrics("high", 1, 15, 30, 80);
    let expected = 2.0 * (1.0 + 0.25 + 1.0 / 12.0);
    assert!((offender_score(&high, Severity::High) - expected).abs() < f64::EPSILON);

    let flat_match = function_metrics("unicode_glyph", 1, 80, 1, 60);
    assert_eq!(classify(&flat_match), Some(Severity::Warn));
    assert_eq!(offender_score(&flat_match, Severity::Critical), 0.0);

    let long_flat_match = function_metrics("dispatch", 1, 80, 1, 120);
    assert_eq!(classify(&long_flat_match), Some(Severity::High));
    assert_eq!(offender_score(&long_flat_match, Severity::High), 0.5);
}

#[test]
fn collect_functions_keeps_parent_aggregate_and_skips_nested_closures() {
    let root = space(
        "crate",
        "unit",
        1,
        40,
        30,
        80,
        vec![
            space(
                "outer",
                "function",
                10,
                20,
                18,
                40,
                vec![space("<anonymous>", "closure", 12, 12, 9, 8, vec![])],
            ),
            space("sibling", "function", 30, 3, 2, 5, vec![]),
        ],
    );

    let mut functions = Vec::new();
    collect_functions(&root, &mut functions);

    assert_eq!(
        functions,
        vec![
            function_metrics("outer", 10, 20, 18, 40),
            function_metrics("sibling", 30, 3, 2, 5),
        ]
    );
}

#[test]
fn inline_test_marker_finds_inline_and_sibling_test_modules() {
    assert_eq!(
        inline_test_marker_line("fn live() {}\n#[cfg(test)]\n\nmod tests {\n}\n"),
        Some(2)
    );
    assert_eq!(
        inline_test_marker_line("fn live() {}\n#[cfg(test)]\nmod tests;\n"),
        Some(2)
    );
    assert_eq!(
        inline_test_marker_line("#[cfg(test)]\nfn helper() {}\n"),
        None
    );
    assert_eq!(inline_test_marker_line("fn live() {}\n"), None);
}

#[test]
fn build_file_group_excludes_inline_tests_and_warn_only_files() {
    let group = build_file_group(
        PathBuf::from("src/example.rs"),
        vec![
            function_metrics("live", 10, 16, 16, 1),
            function_metrics("near", 20, 11, 1, 1),
            function_metrics("test", 40, 30, 60, 120),
        ],
        Some(30),
    )
    .unwrap();

    assert_eq!(group.offenders.len(), 1);
    assert_eq!(group.offenders[0].metrics.name, "live");
    assert_eq!(group.near, vec![function_metrics("near", 20, 11, 1, 1)]);
    assert!(
        build_file_group(
            PathBuf::from("src/near.rs"),
            vec![function_metrics("near", 1, 11, 1, 1)],
            None,
        )
        .is_none()
    );
}

#[test]
fn file_groups_and_functions_sort_by_score_then_stable_tiebreakers() {
    let mut groups = [group("b.rs", 3.0), group("c.rs", 4.0), group("a.rs", 3.0)];
    groups.sort_by(compare_file_groups);
    assert_eq!(
        groups
            .iter()
            .map(|group| group.path.as_path())
            .collect::<Vec<_>>(),
        vec![Path::new("c.rs"), Path::new("a.rs"), Path::new("b.rs")]
    );

    let mut functions = [
        scored("late", 20, 2.0),
        scored("high", 30, 4.0),
        scored("early", 10, 2.0),
    ];
    functions.sort_by(compare_scored_functions);
    assert_eq!(
        functions
            .iter()
            .map(|function| function.metrics.name.as_str())
            .collect::<Vec<_>>(),
        vec!["high", "early", "late"]
    );
}

#[test]
fn split_first_starts_above_five_offenders() {
    let functions = (1..=6)
        .map(|line| function_metrics(&format!("f{line}"), line, 16, 16, 1))
        .collect();
    assert!(
        build_file_group(PathBuf::from("six.rs"), functions, None)
            .unwrap()
            .split_first
    );

    let functions = (1..=5)
        .map(|line| function_metrics(&format!("f{line}"), line, 16, 16, 1))
        .collect();
    assert!(
        !build_file_group(PathBuf::from("five.rs"), functions, None)
            .unwrap()
            .split_first
    );
}

#[test]
fn complexity_json_has_versioned_truncated_shape_and_rounded_scores() {
    let files = vec![
        FileGroup {
            path: PathBuf::from("src/example.rs"),
            score: 2.666,
            split_first: false,
            offenders: vec![ScoredFunction {
                metrics: function_metrics("parse", 12, 15, 30, 80),
                severity: Severity::High,
                score: 2.666,
            }],
            near: vec![function_metrics("near", 30, 11, 1, 1)],
        },
        group("src/second.rs", 1.0),
    ];
    let value = serde_json::to_value(complexity_json(&files, 1)).unwrap();

    assert_eq!(value["version"], 1);
    assert_eq!(value["thresholds"]["warn"]["cognitive"], 15.0);
    assert_eq!(value["total_files"], 2);
    assert_eq!(value["files"].as_array().unwrap().len(), 1);
    assert_eq!(value["files"][0]["path"], "src/example.rs");
    assert_eq!(value["files"][0]["score"], 2.7);
    assert_eq!(value["files"][0]["offenders"][0]["severity"], "high");
    assert_eq!(value["files"][0]["offenders"][0]["score"], 2.7);
    assert_eq!(value["files"][0]["near"][0]["name"], "near");
}

#[test]
fn is_test_file_classifies_conventional_paths() {
    assert!(is_test_file(Path::new("xtask/src/pricing/tests.rs")));
    assert!(is_test_file(Path::new(
        "crates/rimz/tests/integration/backend/zellij.rs"
    )));
    assert!(!is_test_file(Path::new("crates/rimz/src/worktree.rs")));
}

#[test]
fn worst_space_selects_function_or_closure_descendant() {
    let root = space(
        "crate",
        "unit",
        1,
        40,
        30,
        40,
        vec![
            space("module", "mod", 2, 30, 25, 30, vec![]),
            space("small", "function", 4, 4, 3, 4, vec![]),
            space(
                "outer",
                "function",
                10,
                8,
                4,
                8,
                vec![space("inner", "closure", 12, 12, 9, 4, vec![])],
            ),
        ],
    );

    assert_eq!(
        worst_space(&root),
        Some(WorstSpace {
            name: "inner".to_owned(),
            start_line: 12,
            cyclomatic: 12.0,
            cognitive: 9.0,
        })
    );
}

#[test]
fn top_complexity_sorts_and_truncates_by_cyclomatic_then_cognitive() {
    let files = vec![
        FileComplexity {
            path: PathBuf::from("a.rs"),
            sloc: 10.0,
            cyclomatic: 5.0,
            cognitive: 3.0,
            worst: None,
        },
        FileComplexity {
            path: PathBuf::from("b.rs"),
            sloc: 10.0,
            cyclomatic: 8.0,
            cognitive: 2.0,
            worst: None,
        },
        FileComplexity {
            path: PathBuf::from("c.rs"),
            sloc: 10.0,
            cyclomatic: 8.0,
            cognitive: 6.0,
            worst: None,
        },
    ];

    let paths = top_complexity(files, 2)
        .into_iter()
        .map(|file| file.path)
        .collect::<Vec<_>>();

    assert_eq!(paths, vec![PathBuf::from("c.rs"), PathBuf::from("b.rs")]);
}

#[test]
fn complexity_json_deserializes_trimmed_rust_code_analysis_report() {
    let raw = r#"{
        "name": "example.rs",
        "start_line": 1,
        "kind": "unit",
        "spaces": [{
            "name": "parse",
            "start_line": 5,
            "kind": "function",
            "spaces": [],
            "metrics": {
                "cyclomatic": { "sum": 7 },
                "cognitive": { "sum": 4 },
                "loc": { "sloc": 8 }
            }
        }],
        "metrics": {
            "cyclomatic": { "sum": 9 },
            "cognitive": { "sum": 5 },
            "loc": { "sloc": 20 }
        }
    }"#;

    let report: Space = serde_json::from_str(raw).unwrap();
    let mut functions = Vec::new();
    collect_functions(&report, &mut functions);

    assert_eq!(report.metrics.cyclomatic.sum, 9.0);
    assert_eq!(functions, vec![function_metrics("parse", 5, 7, 4, 8)]);
    assert_eq!(
        worst_space(&report),
        Some(WorstSpace {
            name: "parse".to_owned(),
            start_line: 5,
            cyclomatic: 7.0,
            cognitive: 4.0,
        })
    );
}
