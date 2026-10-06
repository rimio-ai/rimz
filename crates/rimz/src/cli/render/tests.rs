use super::*;
use serde::ser::Error as _;

#[test]
fn lsp_labels_share_registry_vocabulary() {
    use rimz::lsp::registry::{State, StopReason};
    for (state, expected) in [
        (State::Starting, "starting"),
        (State::Indexing, "indexing"),
        (State::Ready, "ready"),
        (
            State::Dormant {
                since_ms: 0,
                reason: None,
            },
            "not started",
        ),
        (
            State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::Idle),
            },
            "dormant: idle",
        ),
        (
            State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::Crashed),
            },
            "dormant: crashed",
        ),
        (
            State::Stopped {
                at_ms: 0,
                reason: StopReason::Released,
            },
            "stopped: released",
        ),
    ] {
        assert_eq!(lsp_state_label(&state), expected);
    }
}

fn strip(
    render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::io::Result<()>,
) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render to in-memory buffer");
    String::from_utf8(stream.into_inner()).expect("utf-8")
}

#[test]
fn finish_propagates_a_non_broken_pipe_error() {
    let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    assert!(finish(Err(err)).is_err());
}

#[test]
fn compact_json_has_one_trailing_newline() {
    let mut out = Vec::new();
    write_json(&mut out, &serde_json::json!({ "answer": 42 }), false).unwrap();
    assert_eq!(out, b"{\"answer\":42}\n");
}

#[test]
fn pretty_json_has_one_trailing_newline() {
    let mut out = Vec::new();

    write_json(&mut out, &serde_json::json!({ "answer": 42 }), true).unwrap();

    assert_eq!(out, b"{\n  \"answer\": 42\n}\n");
}

#[test]
fn json_propagates_serialization_failure() {
    struct Fails;

    impl serde::Serialize for Fails {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(S::Error::custom("serialization failed"))
        }
    }

    let error = write_json(&mut Vec::new(), &Fails, false).unwrap_err();

    assert!(error.to_string().contains("serialization failed"));
}

#[test]
fn json_propagates_ordinary_writer_failure() {
    let mut writer = FailingWriter(std::io::ErrorKind::PermissionDenied);

    let error = write_json(&mut writer, &true, false).unwrap_err();

    assert!(error.to_string().contains("permission denied"));
}

#[test]
fn json_treats_broken_pipe_as_clean() {
    let mut writer = FailingWriter(std::io::ErrorKind::BrokenPipe);

    assert!(write_json(&mut writer, &true, false).is_ok());
}

struct FailingWriter(std::io::ErrorKind);

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(self.0))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn gutter_writer_prefixes_lines_across_partial_writes() {
    let mut out = Vec::new();
    {
        let mut gutter = GutterWriter::new(&mut out);
        gutter.write_all(b"first\nsec").unwrap();
        gutter.write_all(b"ond\nthird").unwrap();
    }

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&paint(palette::faint(), "│ ")));
    assert_eq!(
        anstream::adapter::strip_str(&raw).to_string(),
        "  │ first\n  │ second\n  │ third"
    );
}

#[test]
fn report_prints_an_embedded_source_once() {
    #[derive(Debug, thiserror::Error)]
    #[error("opening config: {source}")]
    struct EmbeddedSource {
        #[source]
        source: std::io::Error,
    }

    let error = anyhow::Error::new(EmbeddedSource {
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
    });

    assert_eq!(
        strip(|w| write_report(w, &error)),
        "error: opening config: permission denied\n"
    );
}

#[test]
fn report_indents_distinct_causes() {
    let error = anyhow::anyhow!("leaf").context("middle").context("top");

    assert_eq!(
        strip(|w| write_report(w, &error)),
        "error: top\n  middle\n  leaf\n"
    );
}

#[test]
fn report_indents_every_multiline_cause_line() {
    let error = anyhow::anyhow!("first detail\nsecond detail").context("top");

    assert_eq!(
        strip(|w| write_report(w, &error)),
        "error: top\n  first detail\n  second detail\n"
    );
}

#[test]
fn report_prints_a_bare_error_on_one_line() {
    let error = anyhow::anyhow!("plain failure");

    assert_eq!(strip(|w| write_report(w, &error)), "error: plain failure\n");
}

#[test]
fn channel_miss_prints_a_shell_runnable_corrected_command() {
    let root = "/repo/checkout";
    let mut agent =
        rimz::agents::AgentState::stub("codex", "root-session", rimz::agents::AgentStatus::Idle);
    agent.name = Some("reader".into());
    agent.worktree_path = Some(root.into());
    let scope = rimz::address::AddressContext {
        channel: Some("scratch".into()),
        origin: rimz::address::ChannelOrigin::Directory,
        project_root: root.into(),
    };
    for (args, target, flag) in [
        (vec!["rimz", "asks", "show", "@reader"], "@reader", None),
        (
            vec![
                "rimz",
                "message",
                "@reader",
                "--channel",
                "scratch",
                "a quoted '$value'",
            ],
            "@reader",
            Some("scratch"),
        ),
        (
            vec![
                "rimz",
                "message",
                "@reader#scratch",
                "--worktree=scratch",
                "hello",
            ],
            "@reader#scratch",
            Some("scratch"),
        ),
        (
            vec!["rimz", "message", "@reader", "-wscratch", "hello"],
            "@reader",
            Some("scratch"),
        ),
    ] {
        let error = anyhow::Error::new(
            rimz::address::resolve_agent(target, flag, &scope, &[&agent]).unwrap_err(),
        )
        .context("finding recipient");
        let argv = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = strip(|w| write_report_with_args(w, &error, &argv));
        assert!(output.contains("try: rimz"), "{output}");
        let command = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("try: "))
            .unwrap();
        let corrected = shlex::split(command).unwrap();
        if flag.is_none() {
            assert_eq!(corrected, ["rimz", "asks", "show", "@reader#main"]);
        } else {
            assert!(
                !corrected.iter().any(|arg| arg.contains("scratch")),
                "{corrected:?}"
            );
            assert_eq!(corrected.last().unwrap(), args.last().unwrap());
            assert_eq!(
                rimz::address::resolve_agent("@reader", Some("main"), &scope, &[&agent])
                    .unwrap()
                    .agent_id,
                agent.agent_id
            );
        }
        let unrelated =
            strip(|w| write_report_with_args(w, &error, &["rimz".into(), "message".into()]));
        assert!(
            !unrelated.contains("try: "),
            "do not rewrite a non-verbatim target: {unrelated}"
        );
    }
}

#[test]
fn error_line_joins_distinct_causes_and_keeps_a_bare_error_verbatim() {
    #[derive(Debug, thiserror::Error)]
    #[error("opening config: {source}")]
    struct EmbeddedSource {
        #[source]
        source: std::io::Error,
    }
    let embedded = anyhow::Error::new(EmbeddedSource {
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
    })
    .context("starting");

    assert_eq!(
        error_line(&embedded),
        "starting: opening config: permission denied"
    );
    assert_eq!(
        error_line(&anyhow::anyhow!("first detail\nsecond detail")),
        "first detail\nsecond detail"
    );
}

#[test]
fn pane_frame_aligns_plain_text() {
    let rendered = strip(|w| pane_frame(w, "tmux:%3", "short\na longer line"));
    let widths: Vec<usize> = rendered.lines().map(UnicodeWidthStr::width).collect();

    assert_eq!(widths, vec![17, 17, 17, 17], "{rendered}");
    assert_eq!(
        rendered,
        "╭─ tmux:%3 ─────╮\n│ short         │\n│ a longer line │\n╰───────────────╯\n"
    );
}

#[test]
fn pane_frame_measures_ansi_styled_content_without_sgr_bytes() {
    let styled = format!(
        "{}ready{}\nlonger",
        palette::good().render(),
        palette::good().render_reset()
    );

    assert_eq!(
        strip(|w| pane_frame(w, "pane", &styled)),
        "╭─ pane ─╮\n│ ready  │\n│ longer │\n╰────────╯\n"
    );
}

#[test]
fn pane_frame_aligns_wide_unicode_content() {
    let rendered = strip(|w| pane_frame(w, "p", "文字\nx"));
    let widths: Vec<usize> = rendered.lines().map(UnicodeWidthStr::width).collect();

    assert_eq!(widths, vec![8, 8, 8, 8], "{rendered}");
    assert!(rendered.contains("│ 文字 │"), "{rendered}");
}

#[test]
fn pane_frame_fits_a_title_wider_than_its_content() {
    assert_eq!(
        strip(|w| pane_frame(w, "zellij:terminal_3", "ok")),
        "╭─ zellij:terminal_3 ╮\n│ ok                 │\n╰────────────────────╯\n"
    );
}

#[test]
fn pane_frame_ignores_a_trailing_content_newline() {
    assert_eq!(
        strip(|w| pane_frame(w, "p", "one\ntwo\n")),
        strip(|w| pane_frame(w, "p", "one\ntwo"))
    );
}

#[test]
fn pane_frame_renders_an_empty_capture_without_a_content_row() {
    assert_eq!(strip(|w| pane_frame(w, "p", "")), "╭─ p ╮\n╰────╯\n");
}

#[test]
fn pane_frame_resets_capture_style_before_padding() {
    let mut raw = Vec::new();

    pane_frame(&mut raw, "p", "\u{1b}[31mred\nlonger").expect("render pane frame");
    let raw = String::from_utf8(raw).expect("utf-8");

    assert!(raw.contains("\u{1b}[31mred\u{1b}[0m   "), "{raw:?}");
}

#[test]
fn table_auto_fits_columns_and_right_aligns() {
    let mut table = Table::new(["NAME", "CTX"]).right(&[1]);
    table.row([cell("right-yard"), cell("100%")]);
    table.row([cell("a"), cell("5%")]);
    // Columns fit the widest cell; CTX is right-aligned; the last column is
    // padded only because it is right-aligned, never trailing whitespace.
    assert_eq!(
        strip(|w| table.render(w)),
        "NAME         CTX\nright-yard  100%\na             5%\n"
    );
}

#[test]
fn table_max_width_limits_trailing_column() {
    let mut table = Table::new(["NAME", "DESC"]).max_width(20);
    table.row([cell("agent"), cell("unicode wide 文字 tail")]);

    let rendered = strip(|w| table.render(w));

    assert_eq!(rendered, "NAME   DESC\nagent  unicode wide…\n");
    assert!(rendered.lines().all(|line| line.width() <= 20));
}

#[test]
fn table_card_detail_wraps_with_an_aligned_indent() {
    let mut table = Table::new(["NAME"]).indent(2).max_width(20);
    table.card([cell("agent")], Some(cell("one two three four five")));

    assert_eq!(
        strip(|w| table.render(w)),
        "  NAME\n  agent\n    one two three\n    four five\n"
    );
}

#[test]
fn table_card_detail_caps_lines_and_marks_truncation() {
    let mut table = Table::new(["NAME"]).max_width(10);
    table.card(
        [cell("agent")],
        Some(cell("one two three four five six seven eight")),
    );

    let rendered = strip(|w| table.render(w));

    assert_eq!(rendered, "NAME\nagent\n  one two\n  three\n  four…\n");
    assert!(rendered.lines().all(|line| line.width() <= 10));
}

#[test]
fn table_card_detail_without_max_width_stays_on_one_line() {
    let mut table = Table::new(["NAME"]);
    table.card([cell("agent")], Some(cell("one two three")));

    assert_eq!(strip(|w| table.render(w)), "NAME\nagent\n  one two three\n");
}

#[test]
fn table_cards_separate_without_section_gaps() {
    let mut table = Table::new(["NAME"]).max_width(20);
    table.section("first");
    table.card([cell("one")], Some(cell("detail")));
    table.card([cell("two")], None);
    table.section("second");
    table.card([cell("three")], None);

    assert_eq!(
        strip(|w| table.render(w)),
        "NAME\n\nfirst\none\n  detail\n\ntwo\n\nsecond\nthree\n"
    );
}

#[test]
fn table_cards_without_detail_still_separate() {
    let mut table = Table::new(["NAME"]);
    table.card([cell("one")], None);
    table.card([cell("two")], None);

    assert_eq!(strip(|w| table.render(w)), "NAME\none\n\ntwo\n");
}

#[test]
fn table_rows_remain_dense_by_default() {
    let mut table = Table::new(["NAME"]);
    table.row([cell("one")]);
    table.row([cell("two")]);

    assert_eq!(strip(|w| table.render(w)), "NAME\none\ntwo\n");
}

#[test]
fn wrap_words_respects_wide_unicode_and_splits_long_tokens() {
    let wrapped = wrap_words("ab 文字列 abcdefgh", 4);

    assert_eq!(wrapped, ["ab", "文字", "列", "abcd", "efgh"]);
    assert!(wrapped.iter().all(|line| line.width() <= 4));
    assert_eq!(wrap_words("文", 1), ["…"]);
}

#[test]
fn table_indent_applies_to_header_and_rows() {
    let mut table = Table::new(["NAME", "CTX"]).indent(2);
    table.row([cell("right-yard"), cell("ok")]);

    assert_eq!(
        strip(|w| table.render(w)),
        "  NAME        CTX\n  right-yard  ok\n"
    );
}

#[test]
fn table_section_cells_join_styled_spans() {
    let mut table = Table::new(["NAME"]);
    table.section_cells(vec![
        cell("⑂ auth-refresh").fg(palette::accent().bold()),
        cell("· forge team").fg(palette::meta()),
    ]);
    table.row([cell("@coder")]);

    assert_eq!(
        strip(|w| table.render(w)),
        "NAME\n\n⑂ auth-refresh · forge team\n@coder\n"
    );
}

#[test]
fn clip_to_width_respects_unicode_width() {
    assert_eq!(clip_to_width("abcd", 4), "abcd");
    assert_eq!(clip_to_width("abcdef", 4), "abc…");
    assert_eq!(clip_to_width("文字abc", 5), "文字…");
    assert_eq!(clip_to_width("\u{301}ab", 1), "\u{301}…");
    assert_eq!(clip_to_width("ab", 1), "…");
}

#[test]
fn keyvals_scopes_span_styles_and_aligns_continuations() {
    let mut kv = KeyVals::new().indent(2);
    kv.push_spans(
        "m",
        [
            cell("used "),
            cell("$12.00").fg(palette::money()),
            cell(" today"),
        ],
    );
    kv.push_lines(
        "reset",
        [
            vec![cell("2 credits")],
            vec![cell("- first")],
            vec![cell("- second")],
        ],
    );
    assert_eq!(
        strip(|w| kv.render(w)),
        "  m:     used $12.00 today\n  reset: 2 credits\n         - first\n         - second\n"
    );
    let mut raw = Vec::new();
    kv.render(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    assert!(
        raw.contains(&format!(
            "used {}$12.00{} today",
            palette::money().render(),
            palette::money().render_reset()
        )),
        "{raw:?}"
    );
}

#[test]
fn home_relative_collapses_only_the_home_prefix() {
    let home = Some("/home/dev");
    assert_eq!(home_relative_to(home, "/home/dev"), "~");
    assert_eq!(
        home_relative_to(home, "/home/dev/code/query-engine"),
        "~/code/query-engine"
    );
    // A sibling that merely shares the prefix string is left untouched.
    assert_eq!(
        home_relative_to(home, "/home/developer/x"),
        "/home/developer/x"
    );
    assert_eq!(home_relative_to(home, "/srv/work"), "/srv/work");
    // No home → identity.
    assert_eq!(home_relative_to(None, "/home/dev/x"), "/home/dev/x");
}

#[test]
fn rel_age_uses_seconds_minutes_hours_days_and_clamps_future() {
    let now = Timestamp::from_second(200_000).expect("timestamp");
    assert_eq!(
        rel_age(Timestamp::from_second(199_959).expect("timestamp"), now),
        "41s ago"
    );
    assert_eq!(
        rel_age(Timestamp::from_second(199_880).expect("timestamp"), now),
        "2m ago"
    );
    assert_eq!(
        rel_age(Timestamp::from_second(192_800).expect("timestamp"), now),
        "2h ago"
    );
    assert_eq!(
        rel_age(Timestamp::from_second(27_200).expect("timestamp"), now),
        "2d ago"
    );
    assert_eq!(
        rel_age(Timestamp::from_second(200_001).expect("timestamp"), now),
        "now"
    );
}

#[test]
fn rel_until_uses_seconds_minutes_hours_days_and_marks_past_due() {
    let now = Timestamp::from_second(200_000).expect("timestamp");
    assert_eq!(
        rel_until(Timestamp::from_second(200_041).expect("timestamp"), now),
        "in 41s"
    );
    assert_eq!(
        rel_until(Timestamp::from_second(200_120).expect("timestamp"), now),
        "in 2m"
    );
    assert_eq!(
        rel_until(Timestamp::from_second(207_200).expect("timestamp"), now),
        "in 2h"
    );
    assert_eq!(
        rel_until(Timestamp::from_second(207_140).expect("timestamp"), now),
        "in 1h 59m"
    );
    assert_eq!(
        rel_until(Timestamp::from_second(372_800).expect("timestamp"), now),
        "in 2d"
    );
    assert_eq!(
        rel_until(Timestamp::from_second(199_999).expect("timestamp"), now),
        "due"
    );
}

#[test]
fn until_label_uses_bare_durations_and_marks_past_due() {
    let now = Timestamp::from_second(200_000).expect("timestamp");
    assert_eq!(
        until_label(Timestamp::from_second(200_041).expect("timestamp"), now),
        "41s"
    );
    assert_eq!(
        until_label(Timestamp::from_second(200_120).expect("timestamp"), now),
        "2m"
    );
    assert_eq!(
        until_label(Timestamp::from_second(207_200).expect("timestamp"), now),
        "2h"
    );
    assert_eq!(
        until_label(Timestamp::from_second(372_800).expect("timestamp"), now),
        "2d"
    );
    assert_eq!(
        until_label(Timestamp::from_second(199_999).expect("timestamp"), now),
        "due"
    );
    assert_eq!(until_label(now, now), "due");
}

#[test]
fn suffixed_cells_measure_and_render_both_styles() {
    let main = palette::meta();
    let suffix = palette::faint();
    let mut table = Table::new(["PANE", "VALUE"]);
    table.row([
        cell("x")
            .fg(main)
            .suffix("(self)", suffix)
            .suffix("pace", main),
        cell("one"),
    ]);
    table.row([cell("plain"), cell("two")]);
    let mut raw = Vec::new();
    table.render(&mut raw).expect("render suffixed cell");
    let raw = String::from_utf8(raw).expect("utf-8");
    assert!(raw.contains(&paint(main, "x")));
    assert!(raw.contains(&paint(suffix, "(self)")));
    assert!(raw.contains(&paint(main, "pace")));
    let clipped = cell("longname")
        .suffix("(self)", suffix)
        .suffix("pace", main)
        .clipped(15);
    assert_eq!(strip(|w| clipped.write_styled(w)), "lo… (self) pace");
    assert_eq!(
        strip(|w| table.render(w)),
        "PANE           VALUE\nx (self) pace  one\nplain          two\n"
    );
}

#[test]
fn window_cell_reads_what_is_left_and_when_it_resets() {
    use jiff::SignedDuration;
    use rimz::agents::RateLimitWindow;
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let window = |used, resets_in_secs: Option<i64>| RateLimitWindow {
        used_percentage: used,
        resets_at: resets_in_secs.map(|secs| now + SignedDuration::from_secs(secs)),
        duration_mins: Some(300),
        ..Default::default()
    };
    let lifted = RateLimitWindow {
        lifted: true,
        used_percentage: Some(40),
        ..Default::default()
    };
    for (window, left) in [
        (window(Some(69), Some(3_720)), Some("31% · 1h02m · 0.9x")),
        (
            RateLimitWindow {
                duration_mins: None,
                ..window(Some(69), None)
            },
            Some("31%"),
        ),
        (window(Some(0), Some(5 * 3_600)), Some("100% · ready")),
        (lifted, Some("∞")),
        (window(None, Some(3_720)), None),
    ] {
        let text = window_cell(&window, now).map(|cell| strip(|w| cell.write_styled(w)));
        assert_eq!(text.as_deref(), left);
    }
}

#[test]
fn pace_text_rounds_tenths_and_caps_at_ten() {
    for (ratio, expected) in [
        (0.0, "0.0x"),
        (0.75, "0.8x"),
        (9.94, "9.9x"),
        (9.96, ">10x"),
        (10.0, ">10x"),
        (20.0, ">10x"),
    ] {
        assert_eq!(pace_text(ratio), expected);
    }
}

#[test]
fn window_cell_gates_pace_at_the_floor_and_omits_blank_states() {
    use jiff::SignedDuration;
    use rimz::agents::RateLimitWindow;
    let now = Timestamp::UNIX_EPOCH;
    let window = RateLimitWindow {
        used_percentage: Some(20),
        duration_mins: Some(300),
        resets_at: Some(now + SignedDuration::from_secs(17_101)),
        ..Default::default()
    };
    let render = |window: &RateLimitWindow| {
        window_cell(window, now).map(|cell| strip(|w| cell.write_styled(w)))
    };
    assert_eq!(render(&window).as_deref(), Some("80% · 4h45m · –"));
    let mut early = Vec::new();
    window_cell(&window, now)
        .unwrap()
        .write_styled(&mut early)
        .unwrap();
    assert!(
        String::from_utf8(early)
            .unwrap()
            .contains(&paint(palette::faint(), "–"))
    );
    // Exactly at the floor (900 s of 18 000) and one second past it.
    for (secs, expected) in [
        (17_100, "80% · 4h45m · 4.0x"),
        (17_099, "80% · 4h44m · 4.0x"),
    ] {
        assert_eq!(
            render(&RateLimitWindow {
                resets_at: Some(now + SignedDuration::from_secs(secs)),
                ..window.clone()
            })
            .as_deref(),
            Some(expected)
        );
    }
    for (blank, expected) in [
        (
            RateLimitWindow {
                lifted: true,
                ..window.clone()
            },
            "∞",
        ),
        (
            RateLimitWindow {
                used_percentage: Some(100),
                ..window.clone()
            },
            "0% · 4h45m",
        ),
        (
            RateLimitWindow {
                used_percentage: Some(0),
                resets_at: Some(now + SignedDuration::from_secs(18_000)),
                ..window.clone()
            },
            "100% · ready",
        ),
        (
            RateLimitWindow {
                resets_at: None,
                duration_mins: None,
                ..window.clone()
            },
            "80%",
        ),
        (
            RateLimitWindow {
                duration_mins: None,
                ..window.clone()
            },
            "80% · 4h45m",
        ),
        (
            RateLimitWindow {
                resets_at: Some(now + SignedDuration::from_secs(18_001)),
                ..window.clone()
            },
            "80% · 5h00m",
        ),
    ] {
        assert_eq!(render(&blank).as_deref(), Some(expected));
    }
    assert_eq!(
        render(&RateLimitWindow {
            used_percentage: None,
            ..window
        }),
        None
    );
}
