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
fn parse_complexity_args_accepts_top_filter_and_json_in_any_order() {
    assert_eq!(
        parse_complexity_args(&[]).unwrap(),
        Some(ComplexityArgs {
            top_n: DEFAULT_TOP_N,
            filter: SectionFilter::Both,
            path: None,
            json: false,
        })
    );
    assert_eq!(
        parse_complexity_args(&[
            "--tests".to_owned(),
            "--top".to_owned(),
            "5".to_owned(),
            "--path".to_owned(),
            "crates/rimz/src/message".to_owned(),
            "--json".to_owned(),
        ])
        .unwrap(),
        Some(ComplexityArgs {
            top_n: 5,
            filter: SectionFilter::Tests,
            path: Some(PathBuf::from("crates/rimz/src/message")),
            json: true,
        })
    );
    assert_eq!(
        parse_complexity_args(&[
            "--json".to_owned(),
            "--top".to_owned(),
            "7".to_owned(),
            "--code".to_owned(),
        ])
        .unwrap(),
        Some(ComplexityArgs {
            top_n: 7,
            filter: SectionFilter::Code,
            path: None,
            json: true,
        })
    );
}

#[test]
fn parse_complexity_args_detects_help_anywhere() {
    assert_eq!(parse_complexity_args(&["-h".to_owned()]).unwrap(), None);
    assert_eq!(
        parse_complexity_args(&["--top".to_owned(), "5".to_owned(), "--help".to_owned(),]).unwrap(),
        None
    );
}

#[test]
fn parse_complexity_args_rejects_invalid_values_duplicates_and_unknowns() {
    assert!(parse_complexity_args(&["--top".to_owned(), "0".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--top".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--top".to_owned(), "many".to_owned()]).is_err());
    assert!(
        parse_complexity_args(&[
            "--top".to_owned(),
            "3".to_owned(),
            "--top".to_owned(),
            "4".to_owned(),
        ])
        .is_err()
    );
    assert!(parse_complexity_args(&["5".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--json".to_owned(), "--json".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--code".to_owned(), "--code".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--tests".to_owned(), "--tests".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--code".to_owned(), "--tests".to_owned()]).is_err());
    assert!(parse_complexity_args(&["--path".to_owned()]).is_err());
    assert!(
        parse_complexity_args(&[
            "--path".to_owned(),
            "src".to_owned(),
            "--path".to_owned(),
            "tests".to_owned(),
        ])
        .is_err()
    );
    assert!(parse_complexity_args(&["--gate".to_owned()]).is_err());
}

#[test]
fn path_scope_matches_directories_files_and_component_boundaries() {
    let root = Path::new("/repo");

    assert!(path_is_in_scope(
        root,
        Path::new("/repo/crates/rimz/src/message/send.rs"),
        Path::new("crates/rimz/src/message")
    ));
    assert!(path_is_in_scope(
        root,
        Path::new("/repo/crates/rimz/src/message.rs"),
        Path::new("crates/rimz/src/message.rs")
    ));
    assert!(!path_is_in_scope(
        root,
        Path::new("/repo/crates/rimz/src/message2/send.rs"),
        Path::new("crates/rimz/src/message")
    ));
    assert!(path_is_in_scope(
        root,
        Path::new("/repo/crates/rimz/src/message/send.rs"),
        Path::new("crates/rimz/src/message/")
    ));
    assert!(path_is_in_scope(
        root,
        Path::new("/repo/crates/rimz/src/message/send.rs"),
        Path::new("./crates/rimz/src/message")
    ));
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
fn build_file_group_excludes_warn_only_files() {
    assert!(
        build_file_group(
            PathBuf::from("src/near.rs"),
            vec![function_metrics("near", 1, 11, 1, 1)],
        )
        .is_none()
    );
}

#[test]
fn source_file_groups_partition_inline_tests_under_the_same_path() {
    let path = PathBuf::from("src/example.rs");
    let (code_group, test_group) = build_source_file_groups(
        path.clone(),
        vec![
            function_metrics("live", 10, 16, 16, 1),
            function_metrics("live_near", 20, 11, 1, 1),
            function_metrics("test", 40, 30, 60, 120),
            function_metrics("test_near", 50, 11, 1, 1),
        ],
        Some(30),
    );
    let code_group = code_group.unwrap();
    let test_group = test_group.unwrap();

    assert_eq!(code_group.path, path);
    assert_eq!(test_group.path, path);
    assert_eq!(code_group.offenders[0].metrics.name, "live");
    assert_eq!(code_group.near[0].name, "live_near");
    assert_eq!(test_group.offenders[0].metrics.name, "test");
    assert_eq!(test_group.near[0].name, "test_near");
}

#[test]
fn source_file_groups_without_inline_tests_build_only_code() {
    let (code_group, test_group) = build_source_file_groups(
        PathBuf::from("src/example.rs"),
        vec![function_metrics("live", 10, 16, 16, 1)],
        None,
    );

    assert!(code_group.is_some());
    assert!(test_group.is_none());
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
        build_file_group(PathBuf::from("six.rs"), functions)
            .unwrap()
            .split_first
    );

    let functions = (1..=5)
        .map(|line| function_metrics(&format!("f{line}"), line, 16, 16, 1))
        .collect();
    assert!(
        !build_file_group(PathBuf::from("five.rs"), functions)
            .unwrap()
            .split_first
    );
}

#[test]
fn complexity_json_has_versioned_truncated_shape_and_rounded_scores() {
    let code_files = vec![
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
    let test_files = vec![
        group("tests/integration.rs", 4.0),
        group("src/example/tests.rs", 2.0),
    ];
    let value = serde_json::to_value(complexity_json(
        &code_files,
        &test_files,
        1,
        SectionFilter::Both,
    ))
    .unwrap();

    assert_eq!(value["version"], 2);
    assert_eq!(value["thresholds"]["warn"]["cognitive"], 15.0);
    assert_eq!(value["code"]["total_files"], 2);
    assert_eq!(value["code"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(value["code"]["files"][0]["path"], "src/example.rs");
    assert_eq!(value["code"]["files"][0]["score"], 2.7);
    assert_eq!(
        value["code"]["files"][0]["offenders"][0]["severity"],
        "high"
    );
    assert_eq!(value["code"]["files"][0]["offenders"][0]["score"], 2.7);
    assert_eq!(value["code"]["files"][0]["near"][0]["name"], "near");
    assert_eq!(value["tests"]["total_files"], 2);
    assert_eq!(value["tests"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(value["tests"]["files"][0]["path"], "tests/integration.rs");

    let code_only = serde_json::to_value(complexity_json(
        &code_files,
        &test_files,
        1,
        SectionFilter::Code,
    ))
    .unwrap();
    assert!(code_only.get("code").is_some());
    assert!(code_only.get("tests").is_none());

    let tests_only = serde_json::to_value(complexity_json(
        &code_files,
        &test_files,
        1,
        SectionFilter::Tests,
    ))
    .unwrap();
    assert!(tests_only.get("code").is_none());
    assert!(tests_only.get("tests").is_some());
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
}
