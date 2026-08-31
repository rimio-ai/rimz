use super::*;

#[test]
fn rank_flags_are_parsed_and_window_is_bounded() {
    assert!(parse_args(&["--window".into(), "101".into()]).is_err());
    let args = parse_args(&[
        "--top".into(),
        "4".into(),
        "--verbose".into(),
        "--pin-tc".into(),
        "0.4".into(),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(args.top, 4);
    assert!(args.verbose);
    assert_eq!(args.pin_tc, 0.4);
    assert_eq!(args.shallow_pub, 20);
    assert_eq!(args.shallow_locpub, 120.0);
}

#[test]
fn wide_thin_surfaces_are_flagged_without_reference_evidence() {
    assert_eq!(
        surface_flag(20, Some(29.9), None, 20, 30.0, 2.0),
        Some("thin")
    );
    assert_eq!(surface_flag(19, Some(1.0), None, 20, 30.0, 2.0), None);
    assert_eq!(surface_flag(20, Some(30.0), None, 20, 30.0, 2.0), None);
    assert_eq!(
        surface_flag(20, Some(1.0), Some(1.0), 20, 30.0, 2.0),
        Some("shallow")
    );
    assert_eq!(
        surface_flag(20, Some(1.0), Some(2.0), 20, 30.0, 2.0),
        Some("hub")
    );
}

#[test]
fn rows_are_ranked_by_churn_weighted_size() {
    let row = |module: &str, code, churn_pct, complexity| Row {
        module: module.to_owned(),
        code,
        churn_pct,
        complexity,
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
    assert!(is_pinned(3.4, Some(0.15), 3.0, 0.30));
    assert!(!is_pinned(19.3, Some(0.30), 3.0, 0.30));
    assert!(!is_pinned(2.9, Some(0.10), 3.0, 0.30));
}

#[test]
fn totals_line_uses_scope_totals_not_the_top_n_rows() {
    let report = Report {
        version: REPORT_VERSION,
        verb: "rank",
        path: PathBuf::from("src"),
        history_commits: 1,
        total_modules: 3,
        total_code: 60,
        total_tests: 15,
        total_pub_items: 12,
        total_escaping_items: 6,
        delta_code: Some(-9),
        delta_tests: Some(4),
        delta_pub: Some(-3),
        delta_esc: Some(-2),
        total_complexity: 0.0,
        rows: vec![Row {
            module: "only-shown".to_owned(),
            code: 10,
            ..Row::default()
        }],
        offenders: Vec::new(),
        parse_failures: 0,
    };

    assert_eq!(
        totals_line(&report, true),
        "overall: code 60, tests 15, pub 12, esc 6, cx 0.0; Δcode -9, Δtests +4, Δpub -3, Δesc -2"
    );
}
