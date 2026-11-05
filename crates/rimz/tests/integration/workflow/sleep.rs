use super::*;

const SLEEP: &str = "[checks.pr]\nsleep='30m'\nthen='@done'\n";

#[test]
fn sleep_fields_warn_with_dotted_paths_in_validate_and_run() {
    for (fields, paths) in [
        (
            "wake=['pr.opened']\non.foo='@done'\ntimeout='1h'\nelse='other'\nwindow='5h.left>=40'\nprompt-file='prompt.md'\nprovider='claude'\naccount='default'\n",
            vec![
                "wake",
                "on.foo",
                "timeout",
                "else",
                "window",
                "prompt-file",
                "provider",
                "account",
            ],
        ),
        ("for='2m'\n", vec!["for"]),
        ("when=['ci=passed']\nfor='30m'\n", vec!["for"]),
        ("when=['ci=passed']\nfor='31m'\n", vec!["for"]),
        ("when=['ci=passed']\nfor=4\n", vec!["for"]),
        ("when=['ci=passed']\nfor='soon'\n", vec!["for"]),
        ("when=['ci=passed']\nfor='0s'\n", vec!["for"]),
    ] {
        let (env, _) = fixture(&format!("{HEADER}{SLEEP}{fields}"), "");
        let output = command(&env, &["workflow", "validate", "review"]);
        assert!(
            output.status.success(),
            "sleep optional fields must only warn: {output:?}"
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let value: Value = serde_json::from_str(&success(command(
            &env,
            &["workflow", "validate", "review", "--json"],
        )))
        .unwrap();
        let mut actual: Vec<_> = value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["path"].as_str().unwrap().to_owned())
            .collect();
        let mut expected: Vec<_> = paths
            .iter()
            .map(|path| format!("checks.pr.{path}"))
            .collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "{value}");
        for path in &expected {
            assert!(
                text.contains(&format!("warning: review: {path}: ignored field:")),
                "{text}"
            );
        }
        let output = command(&env, &["workflow", "run", "review"]);
        assert!(output.status.success(), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        for path in expected {
            assert!(
                stderr.contains(&format!("warning: review: {path}:")),
                "{stderr}"
            );
        }
        let stdout = String::from_utf8(output.stdout).unwrap();
        success(command(
            &env,
            &["workflow", "cancel", stdout.lines().next().unwrap()],
        ));
    }
}

#[test]
fn sleep_definition_and_run_views_use_the_authored_spelling_without_generated_cases() {
    for when in ["", "when=['ci=passed']\nfor='2m'\n"] {
        let (env, _) = fixture(&format!("{HEADER}{SLEEP}{when}"), "");
        let value: Value = serde_json::from_str(&success(command(
            &env,
            &["workflow", "validate", "review", "--json"],
        )))
        .unwrap();
        assert_eq!(value["rows"][0]["runner"], "sleep");
        let text = success(command(&env, &["workflow", "validate", "review"]));
        assert!(text.contains("sleep"), "{text}");
        for args in [
            vec!["workflow", "show", "review"],
            vec!["workflow", "show", "review", "pr"],
        ] {
            let text = success(command(&env, &args));
            assert!(
                text.contains("sleep")
                    && text.contains("30m")
                    && text.contains("then")
                    && text.contains("@done"),
                "{text}"
            );
            assert!(
                !text.contains("sleeping →")
                    && !text.contains("unknown →")
                    && !text.contains("invalid →")
                    && !text.contains("cases:"),
                "{text}"
            );
            if !when.is_empty() {
                assert!(
                    text.contains("when")
                        && text.contains("ci=passed")
                        && text.contains("for")
                        && text.contains("2m"),
                    "{text}"
                );
            }
        }
        let id = start(&env);
        until(&env, &id, |value| {
            value["record"]["wait"].is_object() && value["record"]["attempt"].is_null()
        });
        for args in [vec!["workflow", "list"], vec!["workflow", "show", &id]] {
            let text = success(command(&env, &args));
            let suffix = if when.is_empty() {
                "/30m → @done"
            } else {
                "/30m when ci=passed → @done"
            };
            assert!(
                text.contains("sleeping ") && text.contains(suffix),
                "{text}"
            );
            assert!(!text.contains(", recheck"), "{text}");
        }
        success(command(&env, &["workflow", "cancel", &id]));
    }
}

#[test]
fn parked_sleep_resume_restarts_the_clock_with_or_without_at() {
    for at in [false, true] {
        let (env, _) = fixture(&format!("{HEADER}{SLEEP}"), "");
        let id = start(&env);
        let waiting = until(&env, &id, |value| {
            value["record"]["wait"].is_object() && value["record"]["attempt"].is_null()
        });
        let mut record = waiting["record"].clone();
        record["state"] = json!("parked");
        record["entered_at"] = json!("2026-01-01T00:00:00Z");
        let state = env.state_path_for(&env.project_root);
        fs::write(
            state.workflow(&id.parse().unwrap()).record,
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let mut args = vec!["workflow", "resume", &id];
        if at {
            args.extend(["--at", "pr"]);
        }
        success(command(&env, &args));
        let resumed = until(&env, &id, |value| {
            value["record"]["state"] == "active"
                && value["record"]["wait"].is_object()
                && value["record"]["attempt"].is_null()
        });
        let entered: jiff::Timestamp = resumed["record"]["entered_at"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let end: jiff::Timestamp = resumed["record"]["recheck_at"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(end.duration_since(entered).as_secs(), 1800);
        assert_ne!(
            resumed["record"]["entered_at"],
            waiting["record"]["entered_at"]
        );
        success(command(&env, &["workflow", "cancel", &id]));
    }
}
