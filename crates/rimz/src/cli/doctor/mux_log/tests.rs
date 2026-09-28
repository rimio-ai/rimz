use super::*;

fn window(bytes: u64, issue_cap: usize) -> LogWindow {
    LogWindow {
        bytes,
        issue_cap,
        since: None,
    }
}

/// `WARN@5 text` stamps the record at second 5 of the epoch.
fn parse(line: &str) -> RecordLine {
    let (severity, rest) = if let Some(rest) = line.strip_prefix("INFO") {
        (None, rest)
    } else if let Some(rest) = line.strip_prefix("WARN") {
        (Some(LogSeverity::Warn), rest)
    } else if let Some(rest) = line.strip_prefix("ERROR") {
        (Some(LogSeverity::Error), rest)
    } else {
        return RecordLine::Continuation;
    };
    let (at, message) = match rest.strip_prefix('@') {
        Some(rest) => {
            let (secs, message) = rest.split_once(' ').unwrap();
            (
                Some(Timestamp::from_second(secs.parse().unwrap()).unwrap()),
                message,
            )
        }
        None => (None, rest.strip_prefix(' ').unwrap_or(rest)),
    };
    RecordLine::Start(LogRecordStart {
        severity,
        at,
        message: message.to_owned(),
        ..LogRecordStart::default()
    })
}

fn diagnose(
    _previous: Option<&LogicalRecord>,
    record: &LogicalRecord,
    _next: Option<&LogicalRecord>,
) -> Option<LogDiagnosis> {
    record.start.severity.map(|_| LogDiagnosis {
        key: normalized_issue_key(&record.start.message),
        state: model::DoctorState::Investigate,
        impact: model::DoctorImpact::Warn,
        summary: LogSummary::Authored(record.start.message.clone()),
        sample: None,
    })
}

#[test]
fn scan_report_preserves_group_order_timestamps_and_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    let log = format!(
        "WARN |test| 2026-07-17 00:00:00.000 [test] test.rs:1: dismissed\n\
         WARN |test| undated [test] test.rs:1: repeated 1\n\
         WARN |test| 2026-07-17 00:00:02.000 [test] test.rs:1: interleaved\n\
         WARN |test| 2026-07-17 00:00:03.000 [test] test.rs:1: repeated 2\n\
         ERROR |zellij_server::route| 2026-07-17 00:00:04.000 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.\n\
         ERROR |???| 2026-07-17 00:00:04.001 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured\n\
         Caused by:\n    0: failed to send message to client\n    1: Broken pipe (os error 32)\n\
         WARN |test| 2026-07-17 00:00:05.000 [test] test.rs:1: repeated 3\n{}\n",
        "x".repeat(RECORD_TEXT_LIMIT)
    );
    std::fs::write(&path, &log).unwrap();
    for mode in [LogText::Include, LogText::Omit] {
        for cap in [10, 2, 0] {
            let report = scan(
                path.clone(),
                model::LogScope::Server,
                LogWindow {
                    bytes: WINDOW_BYTES,
                    issue_cap: cap,
                    since: parse_zellij_timestamp("2026-07-17 00:00:01.000"),
                },
                parse_zellij_log_line,
                diagnose_zellij_log_record,
                mode,
            );
            let mut actual = serde_json::to_value(report).unwrap();
            actual["path"] = serde_json::json!("mux.log");
            let included = mode == LogText::Include;
            let mut issues = vec![
                serde_json::json!({
                    "source_severity": "warn", "state": "investigate", "impact": "warn",
                    "summary": if included { "interleaved" } else { "an unclassified warn record from test" },
                    "occurrences": 1,
                    "first_occurrence": parse_zellij_timestamp("2026-07-17 00:00:02.000"),
                    "last_occurrence": parse_zellij_timestamp("2026-07-17 00:00:02.000"),
                    "samples": ["WARN |test| 2026-07-17 00:00:02.000 [test] test.rs:1: interleaved"],
                    "evidence_truncated": false,
                }),
                serde_json::json!({
                    "source_severity": "error", "state": "expected", "impact": "info",
                    "summary": "a client left the session", "occurrences": 1,
                    "first_occurrence": parse_zellij_timestamp("2026-07-17 00:00:04.000"),
                    "last_occurrence": parse_zellij_timestamp("2026-07-17 00:00:04.000"),
                    "samples": ["ERROR |zellij_server::route| 2026-07-17 00:00:04.000 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.\nERROR |???| 2026-07-17 00:00:04.001 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured\nCaused by:\n    0: failed to send message to client\n    1: Broken pipe (os error 32)"],
                    "evidence_truncated": false,
                }),
                serde_json::json!({
                    "source_severity": "warn", "state": "investigate", "impact": "warn",
                    "summary": if included { "repeated 1" } else { "an unclassified warn record from test" },
                    "occurrences": 3,
                    "first_occurrence": parse_zellij_timestamp("2026-07-17 00:00:03.000"),
                    "last_occurrence": parse_zellij_timestamp("2026-07-17 00:00:05.000"),
                    "samples": ["WARN |test| undated [test] test.rs:1: repeated 1"],
                    "evidence_truncated": included,
                }),
            ];
            if !included {
                for issue in &mut issues {
                    issue["samples"] = serde_json::json!([]);
                }
            }
            let omitted = match cap {
                10 => 0,
                2 => 1,
                0 => 3,
                _ => unreachable!(),
            };
            issues.drain(..omitted);
            assert_eq!(
                actual,
                serde_json::json!({
                    "state": "ready", "path": "mux.log", "scope": {"kind": "server"},
                    "size_bytes": 8840, "scanned_bytes": 8840, "logical_records": 7,
                    "records_before_cutoff": 1,
                    "since": parse_zellij_timestamp("2026-07-17 00:00:01.000"),
                    "problem_records": 5, "omitted_issue_groups": omitted,
                    "log_text_omitted": !included, "issues": issues,
                }),
                "{mode:?}, cap {cap}"
            );
        }
    }
}

#[test]
fn assembles_records_and_non_problem_start_terminates_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    std::fs::write(
        &path,
        "orphan continuation\nWARN first\nCaused by: detail\n\nINFO ok\nERROR second\n",
    )
    .unwrap();

    let scan = scan_tail(&path, window(1024, 10), parse, diagnose, LogText::Include).unwrap();
    assert_eq!(scan.logical_records, 3);
    assert_eq!(scan.problem_records, 2);
    assert_eq!(scan.issues[0].samples[0], "WARN first\nCaused by: detail\n");
}

#[test]
fn groups_before_cap_and_keeps_one_sample() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    std::fs::write(
        &path,
        "WARN client 1\nWARN client 2\nERROR other\nWARN client 3\n",
    )
    .unwrap();

    let scan = scan_tail(&path, window(1024, 1), parse, diagnose, LogText::Include).unwrap();
    assert_eq!(scan.problem_records, 4);
    assert_eq!(scan.omitted_issue_groups, 1);
    assert_eq!(scan.issues.len(), 1);
}

#[test]
fn cutoff_judges_only_records_written_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    std::fs::write(&path, "WARN@10 dismissed\nWARN@20 kept\nWARN undated\n").unwrap();

    let scan = scan_tail(
        &path,
        LogWindow {
            bytes: 1024,
            issue_cap: 10,
            since: Some(Timestamp::from_second(15).unwrap()),
        },
        parse,
        diagnose,
        LogText::Include,
    )
    .unwrap();

    assert_eq!(scan.logical_records, 3);
    assert_eq!(scan.records_before_cutoff, 1);
    assert_eq!(scan.problem_records, 2, "undated records survive a cutoff");
    let summaries: Vec<_> = scan
        .issues
        .iter()
        .map(|issue| issue.summary.as_str())
        .collect();
    assert_eq!(summaries, ["kept", "undated"]);
    assert_eq!(
        scan.issues[0].first_occurrence,
        Some(Timestamp::from_second(20).unwrap())
    );
}

#[test]
fn window_seek_drops_partial_initial_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    std::fs::write(&path, "WARN too old\ndetail\nINFO boundary\nERROR recent\n").unwrap();

    let scan = scan_tail(&path, window(27, 10), parse, diagnose, LogText::Include).unwrap();
    assert_eq!(scan.problem_records, 1);
    assert_eq!(scan.issues[0].summary, "recent");
}

#[test]
fn window_seek_keeps_record_when_start_is_line_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    let recent = "ERROR recent\n";
    std::fs::write(&path, format!("WARN old\n{recent}")).unwrap();

    let scan = scan_tail(
        &path,
        window(recent.len() as u64, 10),
        parse,
        diagnose,
        LogText::Include,
    )
    .unwrap();

    assert_eq!(scan.problem_records, 1);
    assert_eq!(scan.issues[0].summary, "recent");
}

#[test]
fn record_truncation_is_utf8_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mux.log");
    std::fs::write(&path, format!("ERROR {}\n", "é".repeat(RECORD_TEXT_LIMIT))).unwrap();

    let scan = scan_tail(
        &path,
        window(32 * 1024, 10),
        parse,
        diagnose,
        LogText::Include,
    )
    .unwrap();
    assert!(scan.issues[0].evidence_truncated);
    assert!(scan.issues[0].samples[0].is_char_boundary(scan.issues[0].samples[0].len()));
}

#[test]
fn zellij_log_classifier_matches_levels() {
    for (line, expected) in [
        ("Panic occured: unknown messages", Some(LogSeverity::Panic)),
        ("Panic occurred: unknown messages", Some(LogSeverity::Panic)),
        ("ERROR failed to decode", Some(LogSeverity::Error)),
        (
            "ERROR  |zellij_utils::errors::not| 2026-07-17 04:06:02.158 [screen] zellij-utils/src/errors.rs:819: Panic occured:",
            Some(LogSeverity::Panic),
        ),
        ("WARN slow client", Some(LogSeverity::Warn)),
        ("INFO later WARN text is not a level", None),
        ("WARNING is not WARN token", None),
    ] {
        let actual = match parse_zellij_log_line(line) {
            RecordLine::Start(start) => start.severity,
            RecordLine::Continuation => None,
        };
        assert_eq!(actual, expected, "{line}");
    }
}

#[test]
fn zellij_diagnosis_names_a_wrapped_error_by_its_cause() {
    // Zellij prints the same wrapper above every recoverable failure, so two
    // unrelated failures share a header and differ only underneath it.
    let record = |causes: &str| {
        let line = "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-client/src/lib.rs:975: a non-fatal error occured";
        let RecordLine::Start(start) = parse_zellij_log_line(line) else {
            panic!("record start");
        };
        LogicalRecord {
            start,
            text: format!("{line}\n\nCaused by:\n{causes}"),
            truncated: false,
        }
    };
    let mouse = record("    0: failed to set the cursor shape\n    1: I/O error (os error 5)");
    let write = record("    0: failed to write to the pty\n    1: I/O error (os error 5)");

    let mouse = diagnose_zellij_log_record(None, &mouse, None).unwrap();
    let write = diagnose_zellij_log_record(None, &write, None).unwrap();

    assert_eq!(
        mouse.summary.resolve(LogText::Include),
        "failed to set the cursor shape: I/O error (os error 5)"
    );
    assert_eq!(mouse.state, model::DoctorState::Investigate);
    assert_eq!(mouse.impact, model::DoctorImpact::Alarm);
    assert_ne!(
        mouse.key, write.key,
        "two failures under one wrapper stay two issues"
    );
}

#[test]
fn zellij_diagnosis_requires_complete_known_lifecycle_evidence() {
    let unknown_line = "ERROR  |zellij_server::route     | 2026-07-17 12:23:34.169 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.";
    let RecordLine::Start(unknown_start) = parse_zellij_log_line(unknown_line) else {
        panic!("record start");
    };
    assert_eq!(
        unknown_start.target.as_deref(),
        Some("zellij_server::route")
    );
    assert!(
        unknown_start.at.is_some(),
        "the structured header carries a readable time"
    );
    assert_eq!(
        unknown_start.source.as_deref(),
        Some("zellij-server/src/route.rs:2642")
    );
    let broken_line = "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured";
    let RecordLine::Start(broken_start) = parse_zellij_log_line(broken_line) else {
        panic!("record start");
    };
    let unknown = LogicalRecord {
        start: unknown_start,
        text: unknown_line.to_owned(),
        truncated: false,
    };
    let broken_pipe = LogicalRecord {
        start: broken_start,
        text: format!(
            "{broken_line}\n\nCaused by:\n    0: failed to send message to client 2\n    1: Broken pipe (os error 32)"
        ),
        truncated: false,
    };

    let expected = diagnose_zellij_log_record(None, &unknown, Some(&broken_pipe)).unwrap();
    assert_eq!(expected.state, model::DoctorState::Expected);
    assert_eq!(expected.impact, model::DoctorImpact::Info);
    assert!(expected.sample.unwrap().contains("Broken pipe"));
    assert!(diagnose_zellij_log_record(Some(&unknown), &broken_pipe, None).is_none());
    let investigate = diagnose_zellij_log_record(None, &unknown, None).unwrap();
    assert_eq!(investigate.state, model::DoctorState::Investigate);
    assert_eq!(investigate.impact, model::DoctorImpact::Warn);
    assert!(
        investigate
            .summary
            .clone()
            .resolve(LogText::Include)
            .contains("version mismatch"),
        "an unpaired unknown message names what it usually means: {}",
        investigate.summary.resolve(LogText::Include)
    );

    let RecordLine::Start(start) = parse_zellij_log_line(
        "ERROR  |zellij_server::route| 2026-07-17 12:23:44.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout",
    ) else {
        panic!("record start");
    };
    let cli_pipe = LogicalRecord {
        text: start.message.clone(),
        start,
        truncated: false,
    };
    assert_eq!(
        diagnose_zellij_log_record(None, &cli_pipe, None)
            .unwrap()
            .state,
        model::DoctorState::Expected
    );
}

/// Both records are zellij ERRORs the room provokes on its own, and neither
/// costs the reader a pane: a pane-targeting action can always lose its target
/// to a close, and a stale directory still yields a live pane in the inherited
/// one. Reporting either as an alarm spends the reader's attention on nothing.
#[test]
fn zellij_diagnosis_grades_self_inflicted_pane_errors_below_alarm() {
    let record = |line: &str| {
        let RecordLine::Start(start) = parse_zellij_log_line(line) else {
            panic!("record start");
        };
        LogicalRecord {
            text: start.message.clone(),
            start,
            truncated: false,
        }
    };

    let closed = record(
        "ERROR  |zellij_server::screen    | 2026-07-20 00:02:56.758 [screen] zellij-server/src/screen.rs:9730: Pane with id Terminal(336) not found",
    );
    let closed = diagnose_zellij_log_record(None, &closed, None).unwrap();
    assert_eq!(closed.state, model::DoctorState::Expected);
    assert_eq!(closed.impact, model::DoctorImpact::Info);

    // The id varies per occurrence; one key keeps the race a single issue.
    let other = record(
        "ERROR  |zellij_server::screen    | 2026-07-20 00:02:57.758 [screen] zellij-server/src/screen.rs:9730: Pane with id Terminal(412) not found",
    );
    assert_eq!(
        closed.key,
        diagnose_zellij_log_record(None, &other, None).unwrap().key,
    );

    let cwd = record(
        "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:54.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/rimz-presence-probe' does not exist or is not a folder",
    );
    let cwd = diagnose_zellij_log_record(None, &cwd, None).unwrap();
    assert_eq!(cwd.state, model::DoctorState::Investigate);
    assert_eq!(cwd.impact, model::DoctorImpact::Warn);
    assert!(
        cwd.summary
            .clone()
            .resolve(LogText::Include)
            .contains("/tmp/rimz-presence-probe"),
        "the directory to fix is the whole point of the line: {}",
        cwd.summary.resolve(LogText::Include)
    );

    // Two stale directories are two fixes, so they stay two issues.
    let elsewhere = record(
        "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:55.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/gone' does not exist or is not a folder",
    );
    assert_ne!(
        cwd.key,
        diagnose_zellij_log_record(None, &elsewhere, None)
            .unwrap()
            .key,
    );

    // A reworded upstream message keeps its alarm rather than reporting a
    // truncated directory.
    let reworded = record(
        "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:56.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/gone' is unreadable",
    );
    assert_eq!(
        diagnose_zellij_log_record(None, &reworded, None)
            .unwrap()
            .impact,
        model::DoctorImpact::Alarm,
    );
}

#[test]
fn zellij_log_scan_groups_complete_0443_artifacts_conservatively() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zellij.log");
    std::fs::write(
        &path,
        concat!(
            "ERROR  |zellij_server::route     | 2026-07-17 12:23:34.169 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.\n",
            "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured\n",
            "\nCaused by:\n    0: failed to send message to client 2\n    1: Broken pipe (os error 32)\n",
            "INFO   |zellij_server            | 2026-07-17 12:23:35.000 [main] zellij-server/src/lib.rs:1: healthy\n",
            "ERROR  |zellij_server::route     | 2026-07-17 12:23:44.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout\n",
            "ERROR  |zellij_server::route     | 2026-07-17 12:23:45.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout\n",
            "ERROR  |zellij_server::pty       | 2026-07-17 12:23:46.000 [pty] zellij-server/src/pty.rs:9: pane query failed\n",
            "ERROR  |zellij_utils::errors::not| 2026-07-17 12:23:47.000 [screen] zellij-utils/src/errors.rs:819: Panic occurred:\n",
            "    thread: screen\n    message: fatal\n",
        ),
    )
    .unwrap();

    let scan = scan_tail(
        &path,
        LogWindow {
            bytes: 64 * 1024,
            issue_cap: 10,
            since: None,
        },
        parse_zellij_log_line,
        diagnose_zellij_log_record,
        LogText::Include,
    )
    .unwrap();

    assert_eq!(scan.logical_records, 7);
    assert_eq!(scan.problem_records, 5);
    assert_eq!(scan.issues.len(), 4);
    assert_eq!(scan.issues[0].state, model::DoctorState::Expected);
    assert!(scan.issues[0].samples[0].contains("Broken pipe"));
    assert_eq!(scan.issues[1].occurrences, 2);
    assert_eq!(scan.issues[1].state, model::DoctorState::Expected);
    assert_eq!(scan.issues[2].state, model::DoctorState::Investigate);
    assert_eq!(scan.issues[3].source_severity, "panic");
}

#[test]
fn omitted_log_text_preserves_diagnoses_and_grouping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zellij.log");
    std::fs::write(&path, concat!(
        "ERROR Failed to set CWD for new pane. '/private/cwd-marker' does not exist or is not a folder\n",
        "ERROR Action PrivateAction did not complete within 1s timeout\n",
        "ERROR Action PrivateAction did not complete within 2s timeout\n",
        "ERROR |zellij_server::panes| 2026-07-17 12:23:34.169 [main] source.rs:1: generic-marker\n",
        "ERROR Received unknown message from client.\n",
    )).unwrap();
    let scan = |mode| {
        scan_tail(
            &path,
            window(64 * 1024, 10),
            parse_zellij_log_line,
            diagnose_zellij_log_record,
            mode,
        )
        .unwrap()
    };
    let included = scan(LogText::Include);
    let omitted = scan(LogText::Omit);
    assert_eq!(included.problem_records, omitted.problem_records);
    assert_eq!(included.logical_records, omitted.logical_records);
    assert_eq!(
        included.records_before_cutoff,
        omitted.records_before_cutoff
    );
    assert_eq!(included.issues.len(), omitted.issues.len());
    assert_eq!(included.issues[1].occurrences, 2);
    assert_eq!(included.issues[3].summary, omitted.issues[3].summary);
    assert!(included.issues[0].summary.contains("/private/cwd-marker"));
    assert!(included.issues[1].summary.contains("PrivateAction"));
    assert_eq!(
        omitted.issues[2].summary,
        "an unclassified error record from zellij_server::panes"
    );
    for (included, omitted) in included.issues.iter().zip(&omitted.issues) {
        assert_eq!(included.occurrences, omitted.occurrences);
        assert_eq!(included.first_occurrence, omitted.first_occurrence);
        assert_eq!(included.last_occurrence, omitted.last_occurrence);
        assert!(omitted.samples.is_empty());
        assert!(!omitted.evidence_truncated);
        for marker in ["/private/cwd-marker", "PrivateAction", "generic-marker"] {
            assert!(!omitted.summary.contains(marker));
        }
    }
}

#[test]
fn tmux_log_classifier_matches_error_and_fatal_mentions() {
    let classify = |line: &str| match parse_tmux_log_line(line) {
        RecordLine::Start(start) => start.severity,
        RecordLine::Continuation => None,
    };
    assert_eq!(
        classify("server error: client lost"),
        Some(LogSeverity::Error)
    );
    assert_eq!(
        classify("fatal: control socket closed"),
        Some(LogSeverity::Error)
    );
    assert_eq!(
        classify("server panic: invariant failed"),
        Some(LogSeverity::Panic)
    );
    assert_eq!(classify("normal redraw"), None);
}

#[test]
fn tmux_log_lines_carry_epoch_stamp() {
    let stamped = |line: &str| match parse_tmux_log_line(line) {
        RecordLine::Start(start) => start.at,
        RecordLine::Continuation => panic!("record start"),
    };

    assert_eq!(
        stamped("1784493802.501234 server error: client lost"),
        Some(Timestamp::new(1_784_493_802, 501_234_000).unwrap())
    );
    // A line tmux did not stamp survives every `--clear` cutoff rather than
    // being dismissed on a guess.
    assert_eq!(stamped("server error: no stamp here"), None);
}
