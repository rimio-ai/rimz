use super::*;

#[test]
fn timed_out_capture_renders_extract_before_next_step() {
    let mut timeout = crate::runner::CaptureTimeout {
        summary: "xtask `test` exceeded its 8s budget after 8s: terminated `cargo nextest run`"
            .into(),
        next_step: "NEXT: rerun the slow step on its own".into(),
        output: "PASS [ 0.1s] ok\nFAIL [ 0.2s] broken\n".into(),
    };
    let error = capture_timeout_error(&timeout).to_string();
    assert_eq!(error.lines().next(), Some(timeout.summary.as_str()));
    assert!(error.contains("FAIL [ 0.2s] broken"), "{error}");
    assert!(!error.contains("PASS ["), "{error}");
    assert_eq!(error.lines().last(), Some(timeout.next_step.as_str()));
    timeout.output = "PASS [ 0.1s] ok\n   Compiling xtask\n".into();
    let error = capture_timeout_error(&timeout).to_string();
    assert!(
        error.contains("step printed nothing beyond progress lines"),
        "{error}"
    );
    assert!(!error.contains("command failed without output"));
    assert_eq!(error.lines().last(), Some(timeout.next_step.as_str()));
}

#[test]
fn semver_baseline_missing_matches_first_publish_error_only() {
    assert!(semver_registry_baseline_missing(
        b"error: failed to retrieve index\nCaused by:\n    rimz not found in registry (crates.io)"
    ));
    assert!(!semver_registry_baseline_missing(
        b"error: failed to retrieve index\nCaused by:\n    registry request failed"
    ));
}

#[test]
fn perf_hoists_no_run_ahead_of_the_bench_separator() {
    let args = |args: &[&str]| {
        perf_bench_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    };
    assert_eq!(args(&["--", "--no-run"]), ["--no-run"]);
    assert_eq!(args(&["--no-run"]), ["--no-run"]);
    assert_eq!(args(&["--no-run", "--", "--no-run"]), ["--no-run"]);
    assert_eq!(
        args(&["--bench", "sidebar", "--", "--no-run", "fold"]),
        ["--bench", "sidebar", "--no-run", "--", "fold"]
    );
    assert_eq!(args(&["--", "fold"]), ["--", "fold"]);
    assert!(args(&[]).is_empty());
}

#[test]
fn gate_defaults_to_fixing_and_failing_fast() {
    let parse = |args: &[&str]| {
        parse_gate_options(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    };
    let options = |fmt, keep_going| GateOptions { fmt, keep_going };
    assert_eq!(parse(&[]).unwrap(), options(FmtMode::Fix, false));
    assert_eq!(parse(&["--check"]).unwrap(), options(FmtMode::Check, false));
    assert_eq!(
        parse(&["--keep-going"]).unwrap(),
        options(FmtMode::Fix, true)
    );
    for both in [["--check", "--keep-going"], ["--keep-going", "--check"]] {
        assert_eq!(parse(&both).unwrap(), options(FmtMode::Check, true));
    }
    for rejected in [
        &["--fix"][..],
        &["--check", "--check"],
        &["--keep-going", "--keep-going"],
    ] {
        let err = parse(rejected).unwrap_err().to_string();
        assert!(err.contains("each at most once"), "{rejected:?}: {err}");
    }
}

#[test]
fn gate_failure_hint_repeats_the_invocation_that_ran() {
    let invocation = |fmt, keep_going| gate_invocation(GateOptions { fmt, keep_going });
    assert_eq!(invocation(FmtMode::Fix, false), "cargo xtask gate");
    assert_eq!(
        invocation(FmtMode::Check, false),
        "cargo xtask gate --check"
    );
    assert_eq!(
        invocation(FmtMode::Check, true),
        "cargo xtask gate --check --keep-going"
    );
}

#[test]
fn keep_going_reports_a_compile_failure_once() {
    let compile = "error[E0308]: mismatched types\nerror: could not compile `rimz` (lib) due to 1 previous error";
    assert_eq!(compile_cascade_source(compile, Some("lint")), Some("lint"));
    assert_eq!(compile_cascade_source(compile, None), None);
    let document = "error: could not document `rimz`";
    assert_eq!(compile_cascade_source(document, Some("lint")), None);
    let test_failure = "FAIL [ 0.1s] rimz::a\nerror: test run failed";
    assert_eq!(compile_cascade_source(test_failure, Some("lint")), None);
}

#[test]
fn deny_offline_enabled_only_for_truthy_flag() {
    assert!(deny_offline(Some("1")));
    assert!(deny_offline(Some("true")));
    assert!(!deny_offline(Some("0")));
    assert!(!deny_offline(Some("")));
    assert!(!deny_offline(None));
}

#[test]
fn deny_offline_is_a_global_option() {
    assert_eq!(
        deny_args(true),
        vec!["deny", "--offline", "check", "-D", "warnings"]
    );
    assert_eq!(deny_args(false), vec!["deny", "check", "-D", "warnings"]);
}

#[test]
fn install_host_lints_cover_both_non_test_feature_shapes() {
    assert!(!INSTALL_HOST_LINT_ARGS.contains(&"--features"));
    assert_eq!(
        INSTALL_DEV_HOST_LINT_ARGS
            .windows(2)
            .filter(|pair| pair[0] == "--features")
            .map(|pair| pair[1])
            .collect::<Vec<_>>(),
        ["sentry"]
    );
    for args in [INSTALL_HOST_LINT_ARGS, INSTALL_DEV_HOST_LINT_ARGS] {
        assert!(!args.contains(&"--all-features"));
        assert!(
            !args
                .iter()
                .any(|arg| arg.split(',').any(|feature| feature == "testkit"))
        );
        assert!(args.windows(2).any(|pair| pair == ["-D", "warnings"]));
    }
}

#[test]
fn trim_cargo_noise_drops_progress_and_keeps_diagnostics() {
    let output = "\
   Compiling foo v1.2.3
    Finished `dev` profile
     Running `cargo clippy`
        PASS [   0.002s] crate tests::already_passed
       START [   0.003s] crate tests::still_running


error[E0599]: no method named `run`


warning: unused variable
  --> src/x.rs:3:1
";

    assert_eq!(
        trim_cargo_noise(output),
        "\
error[E0599]: no method named `run`

warning: unused variable
  --> src/x.rs:3:1"
    );
}

// Captured from cargo 1.9x with rustc SIGKILLed directly, under sccache's
// text report, under sccache's silent exit 2, and a real type error.
#[test]
fn a_compile_without_diagnostics_reads_as_a_killed_compiler() {
    let killed = "\
error: could not compile `toy` (lib test)

Caused by:
  process didn't exit successfully: `rustc --crate-name toy` (signal: 9, SIGKILL: kill)
warning: build failed, waiting for other jobs to finish...
error: command `cargo test --no-run --message-format json-render-diagnostics` exited with code 101";
    let sccache_text = "\
sccache: Compile terminated by signal 9
error: could not compile `toy` (lib)

Caused by:
  process didn't exit successfully: `sccache clippy-driver --crate-name toy` (exit status: 2)";
    let sccache_bare = "\
error: could not compile `rimz` (lib test)

Caused by:
  process didn't exit successfully: `sccache rustc --crate-name rimz` (exit status: 2)";
    let real = "\
error[E0308]: mismatched types
 --> src/lib.rs:1:20
error: could not compile `toy` (lib test) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
error: could not compile `toy` (lib) due to 1 previous error; 2 warnings emitted";

    for output in [killed, sccache_text, sccache_bare] {
        assert!(compiler_died_without_diagnostic(output), "{output}");
    }
    assert!(!compiler_died_without_diagnostic(real));
    assert!(!compiler_died_without_diagnostic(&format!(
        "{real}\n{sccache_bare}"
    )));
    assert!(!compiler_died_without_diagnostic(
        "Summary [   1.0s] 3 tests run: 2 passed, 1 failed"
    ));
}

#[test]
fn bounded_failure_output_keeps_the_first_and_final_diagnostics() {
    let output = format!(
        "error: first compiler diagnostic\n{}\nSummary: final failing test",
        "unhelpful middle output\n".repeat(1_000)
    );
    let bounded = bound_trimmed_output(output);

    assert!(bounded.starts_with("error: first compiler diagnostic"));
    assert!(bounded.contains("output truncated; showing final diagnostics"));
    assert!(bounded.ends_with("Summary: final failing test"));
}

#[test]
fn compact_failure_drops_default_profile_passes_before_bounding() {
    let mut output = (0..135)
        .map(|index| {
            format!(
                "        PASS [   0.002s] ({index}/135) rimz::integration tests::passing_{index}\n"
            )
        })
        .collect::<String>();
    output.push_str(
        "        FAIL [   0.010s] rimz::integration tests::broken\n\
             panic: expected durable record\n\
             Summary [   1.0s] 135 tests run: 134 passed, 1 failed\n",
    );

    let detail = failure_detail(&output);
    assert!(!detail.contains("PASS ["), "{detail}");
    assert!(detail.contains("FAIL ["), "{detail}");
    assert!(
        detail.contains("panic: expected durable record"),
        "{detail}"
    );
    assert!(detail.contains("1 failed"), "{detail}");
}

#[test]
fn extract_test_summary_reads_nextest_summary_line() {
    let output = "\
some setup line
Summary [   12.3s] 2611 tests run: 2611 passed, 42 skipped
";

    assert_eq!(
        extract_test_summary(output).as_deref(),
        Some("Summary [   12.3s] 2611 tests run: 2611 passed, 42 skipped")
    );
    assert_eq!(
        extract_test_summary("Summary [ 0.01s] 1 test run: 1 passed").as_deref(),
        Some("Summary [ 0.01s] 1 test run: 1 passed")
    );
    assert_eq!(
        extract_test_summary("cargo nextest: 1 passed, 42 skipped (0.01s)").as_deref(),
        Some("cargo nextest: 1 passed, 42 skipped (0.01s)")
    );
    assert_eq!(extract_test_summary("no summary here"), None);
}

#[test]
fn extract_test_summary_keeps_nextest_flaky_recap() {
    let output = "\
  TRY 1 FAIL [   0.003s] (───) rimz::integration backend::zellij::tab
    FLAKY 1/1 [   0.001s] printed by the test itself, before the summary
  TRY 2 PASS [   0.003s] (2/2) rimz::integration backend::zellij::tab
────────────
     Summary [   0.009s] 2 tests run: 2 passed (1 flaky), 0 skipped
   FLAKY 2/5 [   0.003s] (2/2) rimz::integration backend::zellij::tab
";

    assert_eq!(
        extract_test_summary(output).as_deref(),
        Some(
            "Summary [   0.009s] 2 tests run: 2 passed (1 flaky), 0 skipped\n\
                 FLAKY 2/5 [   0.003s] (2/2) rimz::integration backend::zellij::tab"
        )
    );
}

#[test]
fn extract_test_summary_strips_color_from_flaky_recap() {
    let output = "\
\x1b[33;1m  TRY 2 PASS\x1b[0m [   0.002s] (1/1) \x1b[35;1mfl\x1b[0m \x1b[34;1mflaky_one\x1b[0m
\x1b[32;1m     Summary\x1b[0m [   0.007s] \x1b[1m1\x1b[0m test run: \x1b[1m1\x1b[0m \x1b[32;1mpassed\x1b[0m (\x1b[1m1\x1b[0m \x1b[33;1mflaky\x1b[0m), \x1b[1m0\x1b[0m skipped
\x1b[33;1m   FLAKY 2/3\x1b[0m [   0.002s] (1/1) \x1b[35;1mfl\x1b[0m \x1b[34;1mflaky_one\x1b[0m
";

    assert_eq!(
        extract_test_summary(output).as_deref(),
        Some(
            "Summary [   0.007s] 1 test run: 1 passed (1 flaky), 0 skipped\n\
                 FLAKY 2/3 [   0.002s] (1/1) fl flaky_one"
        )
    );
}

#[test]
fn skip_report_rides_under_the_test_result() {
    let summary = "2 tests run: 2 passed, 0 skipped";
    let report = "self-skipped 1 test:\n  tmux not on PATH (1): one";
    let pass = || GateResult::Pass {
        note: Some(summary.to_owned()),
    };
    assert_eq!(pass().with_skip_report(None), pass());
    assert_eq!(
        pass().with_skip_report(Some(report.to_owned())),
        GateResult::Pass {
            note: Some(format!("{summary}\n{report}")),
        }
    );
    assert_eq!(
        GateResult::Fail {
            detail: "1 failed".to_owned(),
        }
        .with_skip_report(Some(report.to_owned())),
        GateResult::Fail {
            detail: format!("1 failed\n{report}"),
        }
    );
}

#[test]
fn a_denied_skip_fails_a_passing_run_and_keeps_its_report() {
    let passed =
        "2 tests run: 2 passed, 0 skipped\nself-skipped 1 test:\n  tmux not on PATH (1): one";
    let denied = "--deny-skips: 1 test self-skipped\n  tmux not on PATH (1): one\nfix";
    let pass = || GateResult::Pass {
        note: Some(passed.to_owned()),
    };
    assert_eq!(pass().denying_skips(None), pass());
    assert_eq!(
        pass().denying_skips(Some(denied.to_owned())),
        GateResult::Fail {
            detail: format!("{passed}\n{denied}"),
        }
    );
    assert_eq!(
        GateResult::Pass { note: None }.denying_skips(Some(denied.to_owned())),
        GateResult::Fail {
            detail: denied.to_owned(),
        }
    );
    assert_eq!(
        GateResult::Fail {
            detail: "1 failed".to_owned(),
        }
        .denying_skips(Some(denied.to_owned())),
        GateResult::Fail {
            detail: format!("1 failed\n{denied}"),
        }
    );
}

#[test]
fn failed_test_capture_saves_full_output_after_reports() {
    let dir = tempfile::tempdir().unwrap();
    let captured = test_capture(
        false,
        format!(
            "PASS [ 0.01s] progress\n{}\nmiddle diagnostic\n{}\n\x1b[31mFAIL\x1b[0m café\nSummary: 1 failed\n",
            "head diagnostic\n".repeat(1_000),
            "tail diagnostic\n".repeat(1_000),
        ),
    );
    let GateResult::Fail { detail } = captured_test_result(
        &captured,
        Some("self-skipped 1 test".to_owned()),
        Some("--deny-skips: refused".to_owned()),
        dir.path(),
    ) else {
        panic!("failed capture must fail");
    };
    let (excerpt, saved) = detail
        .rsplit_once("\nfull test output: ")
        .expect("saved output line");
    assert!(excerpt.ends_with("self-skipped 1 test\n--deny-skips: refused"));
    assert!(excerpt.contains("output truncated"));
    assert!(!excerpt.contains("PASS ["));
    assert!(!excerpt.contains("middle diagnostic"));
    let (path, counts) = saved.rsplit_once(" (").unwrap();
    let path = Path::new(path);
    assert!(path.is_absolute());
    assert_eq!(path.parent(), Some(dir.path()));
    assert!(
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("rimz-xtask-test-")
    );
    assert_eq!(path.extension().unwrap(), "output");
    assert_eq!(fs::read(path).unwrap(), captured.output.as_bytes());
    assert_eq!(
        counts,
        format!(
            "{} lines, {} KB)",
            captured.output.lines().count(),
            captured.output.len().div_ceil(1024)
        )
    );
}

#[test]
fn failed_test_captures_get_unique_files() {
    let dir = tempfile::tempdir().unwrap();
    let captured = test_capture(false, "FAIL: same capture".to_owned());
    let first = captured_test_result(&captured, None, None, dir.path());
    let second = captured_test_result(&captured, None, None, dir.path());
    assert_ne!(first, second, "each result must name a distinct file");
    let files = fs::read_dir(dir.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 2);
    for file in files {
        assert_eq!(fs::read(file.path()).unwrap(), captured.output.as_bytes());
    }
}

#[test]
fn passed_test_captures_save_nothing_even_with_denied_skips() {
    let dir = tempfile::tempdir().unwrap();
    let captured = test_capture(true, "Summary [ 0.1s] 1 test run: 1 passed\n".to_owned());
    for denied in [None, Some("--deny-skips: refused".to_owned())] {
        let report = Some("self-skipped 1 test".to_owned());
        assert_eq!(
            captured_test_result(&captured, report.clone(), denied.clone(), dir.path()),
            captured_result(&captured, Some(extract_test_summary))
                .with_skip_report(report)
                .denying_skips(denied),
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[test]
fn failed_test_capture_keeps_detail_when_save_fails() {
    let dir = tempfile::tempdir().unwrap();
    let captured = test_capture(
        false,
        "PASS [ 0.01s] progress\nFAIL: diagnostic\n".to_owned(),
    );
    let GateResult::Fail { detail } =
        captured_test_result(&captured, None, None, &dir.path().join("missing"))
    else {
        panic!("save failure must not hide the test failure");
    };
    assert!(
        detail.starts_with("FAIL: diagnostic\nfull test output: not saved ("),
        "{detail}"
    );
    assert!(detail.ends_with(')'));
    assert_eq!(detail.lines().count(), 2);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

fn test_capture(success: bool, output: String) -> Captured {
    use std::os::unix::process::ExitStatusExt;
    Captured {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
        stdout: String::new(),
        output,
    }
}

#[test]
fn deny_skips_is_consumed_before_the_separator_and_never_forwarded() {
    let command = parse_test_command(&[
        "--deny-skips".to_owned(),
        "--profile".to_owned(),
        "gate".to_owned(),
        "--".to_owned(),
        "--deny-skips".to_owned(),
    ])
    .unwrap();
    assert!(command.deny_skips);
    assert_eq!(
        command.forwarded,
        ["--profile", "gate", "--", "--deny-skips"].map(str::to_owned)
    );
    assert!(!parse_test_command(&["auth".to_owned()]).unwrap().deny_skips);
}

#[test]
fn zero_match_detection_uses_nextest_exit_code_or_message() {
    assert!(nextest_matched_no_tests(Some(4), ""));
    assert!(nextest_matched_no_tests(
        Some(1),
        "error: no tests to run\n"
    ));
    assert!(!nextest_matched_no_tests(
        Some(1),
        "test failed for another reason"
    ));
}

#[test]
fn test_command_extracts_names_and_list_without_rewriting_nextest_args() {
    let command = parse_test_command(&[
        "--name".to_owned(),
        "leaf".to_owned(),
        "--name=tests::full".to_owned(),
        "-P".to_owned(),
        "gate".to_owned(),
        "auth".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        command,
        TestCommand {
            names: vec!["leaf".to_owned(), "tests::full".to_owned()],
            forwarded: vec!["-P".to_owned(), "gate".to_owned(), "auth".to_owned()],
            list: false,
            deny_skips: false,
        }
    );

    let command = parse_test_command(&["--list".to_owned(), "auth".to_owned()]).unwrap();
    assert_eq!(
        command,
        TestCommand {
            names: Vec::new(),
            forwarded: vec!["auth".to_owned()],
            list: true,
            deny_skips: false,
        }
    );
}

#[test]
fn test_command_leaves_libtest_args_after_separator_untouched() {
    let command = parse_test_command(&[
        "auth".to_owned(),
        "--".to_owned(),
        "--name".to_owned(),
        "literal".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        command.forwarded,
        ["auth", "--", "--name", "literal"].map(str::to_owned)
    );
    assert!(command.names.is_empty());
}

#[test]
fn test_command_rejects_missing_names_and_mixed_discovery_modes() {
    assert!(
        parse_test_command(&["--name".to_owned()])
            .unwrap_err()
            .to_string()
            .contains("requires a test name")
    );
    assert!(
        parse_test_command(&["--name=".to_owned()])
            .unwrap_err()
            .to_string()
            .contains("cannot be empty")
    );
    assert!(
        parse_test_command(&["--list".to_owned(), "--name=leaf".to_owned()])
            .unwrap_err()
            .to_string()
            .contains("does not combine")
    );
}

#[test]
fn exact_name_filterset_anchors_leaf_and_full_names() {
    assert_eq!(
        exact_name_filterset(&["leaf".to_owned(), "tests::full.name".to_owned()]),
        r"test(/(^|::)leaf$/) | test(/(^|::)tests::full\.name$/)"
    );
    assert_eq!(
        exact_name_filterset(&["unicode::café".to_owned()]),
        "test(/(^|::)unicode::café$/)"
    );
}

#[test]
fn list_step_forwards_only_nextest_profiles() {
    assert_eq!(
        nextest_profile_args(&[
            "auth".to_owned(),
            "-P".to_owned(),
            "live".to_owned(),
            "--profile=journey".to_owned(),
            "--no-capture".to_owned(),
            "--".to_owned(),
            "-P".to_owned(),
            "ignored".to_owned(),
        ]),
        ["-P", "live", "--profile=journey"].map(str::to_owned)
    );
}

#[test]
fn no_capture_is_recognized_in_both_spellings_and_after_the_separator() {
    assert!(requests_no_capture(&["--no-capture".to_owned()]));
    assert!(requests_no_capture(&[
        "--".to_owned(),
        "--nocapture".to_owned()
    ]));
    assert!(!requests_no_capture(&[
        "auth".to_owned(),
        "-P".to_owned(),
        "live".to_owned()
    ]));
}

#[test]
fn nextest_json_keeps_only_filterset_matches() {
    let output = r#"{
            "rust-suites": {
                "xtask::bin/xtask": {
                    "binary-path": "/target/debug/deps/xtask-0123",
                    "cwd": "/checkout/xtask",
                    "testcases": {
                        "tests::matched": {
                            "filter-match": {"status": "matches"}
                        },
                        "tests::other": {
                            "filter-match": {"status": "mismatch", "reason": "expression"}
                        }
                    }
                }
            }
        }"#;
    assert_eq!(
        parse_nextest_list(output).unwrap(),
        vec!["tests::matched".to_owned()]
    );
}

fn listing(suites: &[(&str, &[(&str, &str)])]) -> String {
    let suites: serde_json::Map<String, serde_json::Value> = suites
        .iter()
        .map(|(id, tests)| {
            let testcases: serde_json::Map<String, serde_json::Value> = tests
                .iter()
                .map(|(name, status)| {
                    (
                        (*name).to_owned(),
                        serde_json::json!({"filter-match": {"status": status}}),
                    )
                })
                .collect();
            (
                (*id).to_owned(),
                serde_json::json!({
                    "binary-path": format!("/target/debug/deps/{id}-0123"),
                    "cwd": format!("/checkout/{id}"),
                    "testcases": testcases,
                }),
            )
        })
        .collect();
    serde_json::json!({"rust-suites": suites}).to_string()
}

#[test]
fn one_listed_match_resolves_to_its_binary_and_cwd() {
    let output = listing(&[
        ("unit", &[("doctor::mixed", "mismatch")]),
        (
            "integration",
            &[("doctor::mixed", "matches"), ("doctor::other", "mismatch")],
        ),
    ]);

    let test = sole_listed_test(&output, "doctor::mixed").unwrap();

    assert_eq!(test.name, "doctor::mixed");
    assert_eq!(
        test.binary,
        Path::new("/target/debug/deps/integration-0123")
    );
    assert_eq!(test.cwd, Path::new("/checkout/integration"));
}

#[test]
fn zero_or_several_listed_matches_refuse_with_the_exact_name_fix() {
    let none = listing(&[("integration", &[("doctor::mixed", "mismatch")])]);
    let err = sole_listed_test(&none, "mixed").unwrap_err().to_string();
    assert!(err.contains("no test is named `mixed`"), "{err}");
    assert!(err.contains("cargo xtask test --list mixed"), "{err}");

    let several = listing(&[
        ("integration", &[("doctor::mixed", "matches")]),
        (
            "unit",
            &[("cli::mixed", "matches"), ("cli::other", "mismatch")],
        ),
    ]);
    let err = sole_listed_test(&several, "mixed").unwrap_err().to_string();
    assert!(err.contains("`mixed` names 2 tests"), "{err}");
    assert!(err.contains("\n  integration doctor::mixed"), "{err}");
    assert!(err.contains("\n  unit cli::mixed"), "{err}");
    assert!(!err.contains("cli::other"), "{err}");
    assert!(err.contains("cargo xtask test --list mixed"), "{err}");
}

#[test]
fn requested_names_match_whole_path_suffixes_only() {
    let matches = match_requested_names(
        &[
            "leaf".to_owned(),
            "nested::leaf".to_owned(),
            "missing".to_owned(),
        ],
        &["tests::leaf".to_owned(), "tests::nested::leaf".to_owned()],
    );
    assert_eq!(
        matches,
        RequestedMatches {
            matched_requests: 2,
            matched_tests: 2,
            unmatched: vec!["missing".to_owned()],
        }
    );
    assert!(!test_name_matches_request("tests::leaf_extra", "leaf"));
}

#[test]
fn nextest_archive_file_reads_split_and_equals_forms() {
    let split = [
        "--archive-file".to_owned(),
        "target/ci/archive.tar.zst".to_owned(),
    ];
    let equals = ["--archive-file=archive.tar.zst".to_owned()];

    assert_eq!(
        nextest_archive_file(&split).as_deref(),
        Some(Path::new("target/ci/archive.tar.zst"))
    );
    assert_eq!(
        nextest_archive_file(&equals).as_deref(),
        Some(Path::new("archive.tar.zst"))
    );
    assert_eq!(nextest_archive_file(&[]), None);
}
