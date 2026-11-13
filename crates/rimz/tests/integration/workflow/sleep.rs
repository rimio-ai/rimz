use super::*;

const SLEEP: &str = "[sleep.pr]\nsleep='30m'\nthen='@done'\n";

#[test]
fn sleep_fields_refuse_at_their_dotted_paths_in_validate_and_run() {
    for (fields, path) in [
        ("wake=['pr.opened']\n", "wake"),
        ("on.foo='@done'\n", "on.foo"),
        ("timeout='1h'\n", "timeout"),
        ("else='other'\n", "else"),
        ("window='5h.left>=40'\n", "window"),
        ("prompt-file='prompt.md'\n", "prompt-file"),
        ("provider='claude'\n", "provider"),
        ("account='default'\n", "account"),
        ("for='2m'\n", "for"),
        ("when=['ci=passed']\nfor='30m'\n", "for"),
        ("when=['ci=passed']\nfor='31m'\n", "for"),
        ("when=['ci=passed']\nfor=4\n", "for"),
        ("when=['ci=passed']\nfor='soon'\n", "for"),
        ("when=['ci=passed']\nfor='0s'\n", "for"),
    ] {
        let (env, _) = fixture(&format!("{HEADER}{SLEEP}{fields}"), "");
        let path = format!("sleep.pr.{path}");
        let output = command(&env, &["workflow", "validate", "review", "--json"]);
        assert!(!output.status.success(), "{fields}: {output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let errors: Vec<_> = value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(errors, std::slice::from_ref(&path), "{fields}: {value}");
        assert_eq!(value["warnings"], json!([]), "{fields}: {value}");
        let output = command(&env, &["workflow", "run", "review"]);
        assert!(!output.status.success(), "{fields}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(&path), "{fields}: {stderr}");
        assert!(output.stdout.is_empty(), "{fields}: no run starts");
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
