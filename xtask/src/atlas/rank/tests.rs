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
