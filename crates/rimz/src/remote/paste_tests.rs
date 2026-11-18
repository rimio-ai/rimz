use super::*;

fn scan_split(chunks: &[&[u8]]) -> Vec<Input> {
    let mut scanner = InputScanner::default();
    let mut out = Vec::new();
    for chunk in chunks {
        scanner.feed(chunk, &mut out);
    }
    out
}

fn scan_each_split(bytes: &[u8], check: impl Fn(&[Input])) {
    for split in 0..=bytes.len() {
        let (head, tail) = bytes.split_at(split);
        check(&scan_split(&[head, tail]));
    }
    let bytewise: Vec<&[u8]> = bytes.chunks(1).collect();
    check(&scan_split(&bytewise));
}

fn joined(inputs: &[Input]) -> Vec<u8> {
    inputs
        .iter()
        .flat_map(|input| input.bytes().to_vec())
        .collect()
}

#[test]
fn each_ctrl_v_encoding_is_a_trigger_across_any_split() {
    for key in [
        &b"\x16"[..],
        b"\x1b[118;5u",
        b"\x1b[118;5:1u",
        b"\x1b[118;5:2u",
        b"\x1b[27;5;118~",
    ] {
        let mut typed = b"a".to_vec();
        typed.extend_from_slice(key);
        typed.push(b'b');
        scan_each_split(&typed, |inputs| {
            assert_eq!(
                inputs,
                [
                    Input::Forward(b"a".to_vec()),
                    Input::CtrlV(key.to_vec()),
                    Input::Forward(b"b".to_vec()),
                ],
                "{key:?}"
            );
        });
    }
}

#[test]
fn ctrl_v_with_another_modifier_or_a_release_is_forwarded() {
    for key in [
        &b"\x1b[118;6u"[..],
        b"\x1b[118;7u",
        b"\x1b[118;5:3u",
        b"\x1b[118u",
        b"\x1b[86;5u",
        b"\x1b[27;6;118~",
        b"\x1b[27;5;119~",
    ] {
        assert_eq!(
            scan_split(&[key]),
            [Input::Forward(key.to_vec())],
            "{key:?}"
        );
    }
}

#[test]
fn lock_keys_do_not_stop_a_ctrl_v_trigger() {
    for key in [&b"\x1b[118;69u"[..], b"\x1b[118;133u", b"\x1b[118;197u"] {
        assert_eq!(scan_split(&[key]), [Input::CtrlV(key.to_vec())], "{key:?}");
    }
}

#[test]
fn ctrl_v_inside_a_bracketed_paste_is_not_a_trigger() {
    for key in [&b"\x16"[..], b"\x1b[118;5u", b"\x1b[27;5;118~"] {
        let mut typed = b"\x1b[200~x".to_vec();
        typed.extend_from_slice(key);
        typed.extend_from_slice(b"\x1b[201~z");
        scan_each_split(&typed, |inputs| {
            assert!(
                inputs
                    .iter()
                    .all(|input| matches!(input, Input::Forward(_))),
                "{key:?}: {inputs:?}"
            );
            assert_eq!(joined(inputs), typed);
        });
    }
}

#[test]
fn a_path_list_paste_is_held_whole() {
    let typed = b"q\x1b[200~/tmp/a\\ b /tmp/c\x1b[201~r";
    scan_each_split(typed, |inputs| {
        assert_eq!(
            inputs,
            [
                Input::Forward(b"q".to_vec()),
                Input::Paste(b"\x1b[200~/tmp/a\\ b /tmp/c\x1b[201~".to_vec()),
                Input::Forward(b"r".to_vec()),
            ]
        );
    });
    assert_eq!(
        paste_payload(b"\x1b[200~/tmp/a\x1b[201~"),
        b"/tmp/a".as_slice()
    );
}

#[test]
fn a_paste_that_stops_looking_like_a_path_list_streams_from_there() {
    for typed in [
        &b"\x1b[200~hello\x1b[201~"[..],
        b"\x1b[200~/tmp/a\rb\x1b[201~",
        b"\x1b[200~/tmp/a\x1b[A\x1b[201~",
        b"\x1b[200~\x1b[201~",
    ] {
        scan_each_split(typed, |inputs| {
            assert!(
                inputs
                    .iter()
                    .all(|input| matches!(input, Input::Forward(_))),
                "{typed:?}: {inputs:?}"
            );
            assert_eq!(joined(inputs), typed);
        });
    }
    let mut scanner = InputScanner::default();
    let mut out = Vec::new();
    scanner.feed(b"\x1b[200~/tmp/a", &mut out);
    assert!(out.is_empty());
    assert!(scanner.in_paste());
    assert!(scanner.hold_deadline().is_some());
    scanner.expire(&mut out);
    scanner.feed(b"b\x1b[201~", &mut out);
    assert_eq!(joined(&out), b"\x1b[200~/tmp/ab\x1b[201~");
    assert!(!scanner.in_paste());
}

#[test]
fn an_oversized_paste_streams_with_nothing_lost() {
    let mut typed = b"\x1b[200~/".to_vec();
    typed.extend(std::iter::repeat_n(b'x', PASTE_HOLD_BYTES + 10));
    typed.extend_from_slice(b"\x1b[201~");
    let inputs = scan_split(&[&typed]);
    assert_eq!(inputs, [Input::Forward(typed)]);
}

#[test]
fn other_input_is_forwarded_unchanged_and_in_order() {
    let typed: &[u8] = b"\x1b[<0;10;5M\x1b[I\x1b[O\x1b[13;2u\x1b[0n\x1bOA\x1b\x1b[1;5Ahi\x7f";
    scan_each_split(typed, |inputs| {
        assert!(
            inputs
                .iter()
                .all(|input| matches!(input, Input::Forward(_))),
            "{inputs:?}"
        );
        assert_eq!(joined(inputs), typed);
    });
}

#[test]
fn alt_ctrl_v_forwards_both_bytes_and_is_no_trigger() {
    scan_each_split(b"a\x1b\x16b", |inputs| {
        assert_eq!(inputs, [Input::Forward(b"a\x1b\x16b".to_vec())]);
    });
}

#[test]
fn a_lone_escape_is_released_after_its_pause() {
    let mut scanner = InputScanner::default();
    let mut out = Vec::new();
    scanner.feed(b"\x1b", &mut out);
    assert!(out.is_empty());
    assert_eq!(scanner.hold_deadline(), Some(KEY_HOLD));
    scanner.expire(&mut out);
    assert_eq!(out, [Input::Forward(b"\x1b".to_vec())]);
    assert_eq!(scanner.hold_deadline(), None);
}

#[test]
fn every_escaped_character_round_trips_through_the_drop_format() {
    let names: Vec<String> = DROP_ESCAPED
        .iter()
        .map(|byte| format!("/tmp/a{}b", char::from(*byte)))
        .chain(["/tmp/plain".to_owned(), "/tmp/ünï code".to_owned()])
        .collect();
    let encoded = encode_drop(&names);
    let parsed = parse_drop(encoded.as_bytes(), |_| true).expect("drop");
    let expected: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
    assert_eq!(parsed, expected);
    assert_eq!(
        encode_drop(&["/a b".to_owned(), "/c".to_owned()]),
        "/a\\ b /c"
    );
    assert_eq!(
        drop_paste(&["/a".to_owned()]),
        b"\x1b[200~/a\x1b[201~".to_vec()
    );
}

#[test]
fn anything_but_usable_absolute_paths_is_not_a_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("one file");
    std::fs::write(&file, b"x").expect("write file");
    let usable = |path: &Path| path.is_file();
    let escaped = encode_drop(&[file.display().to_string()]);
    assert_eq!(
        parse_drop(escaped.as_bytes(), usable),
        Some(vec![file.clone()])
    );
    let missing = dir.path().join("missing").display().to_string();
    let directory = dir.path().display().to_string();
    for payload in [
        "relative/path".to_owned(),
        directory,
        missing,
        format!("{escaped} trailing"),
        format!("{escaped} "),
        format!("{escaped}  {escaped}"),
        format!("{escaped}\\"),
        String::new(),
    ] {
        assert_eq!(parse_drop(payload.as_bytes(), usable), None, "{payload:?}");
    }
}
