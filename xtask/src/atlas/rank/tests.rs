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
