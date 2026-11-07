use super::*;

const CONDITION: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={ ready='@done', pending='@wait' }\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n";

#[test]
fn inline_cases_match_table_blocks_in_order() {
    let (_home, dir) = fixture();
    let header = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={merged='@done',red='@failed',pending='@wait'}\n";
    let actual: Vec<_> = [
        r#"cases = [
    { when = ["pr=merged"], outcome = "merged" },
    { when = ["ci=failed || pr.queue=dequeued"], outcome = "red", for = "2m" },
]
"#,
        r#"[[checks.pr.cases]]
when = ["pr=merged"]
outcome = "merged"
[[checks.pr.cases]]
when = ["ci=failed || pr.queue=dequeued"]
outcome = "red"
for = "2m"
"#,
    ]
    .into_iter()
    .map(|cases| {
        fs::write(dir.join("workflow.toml"), format!("{header}{cases}")).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(
            report.errors.is_empty() && report.warnings.is_empty(),
            "{report:?}"
        );
        load(&dir).unwrap().checks["pr"]
            .cases
            .iter()
            .map(|case| {
                serde_json::json!({
                    "when": case.when, "outcome": case.outcome,
                    "for": case.hold
                })
            })
            .collect::<Vec<_>>()
    })
    .collect();
    let expected = vec![
        serde_json::json!({"when":["pr=merged"], "outcome":"merged", "for":null}),
        serde_json::json!({
            "when":["ci=failed || pr.queue=dequeued"], "outcome":"red", "for":"2m"
        }),
    ];
    assert_eq!(actual, vec![expected.clone(), expected]);
}

#[test]
fn an_explicit_empty_cases_field_is_not_ignored_on_a_run_check() {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ncases=[]\non.ok='@done'\n").unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.path == "checks.pr.cases"),
        "run must refuse cases, even an empty array: {:?}",
        report.errors
    );
}

#[test]
fn condition_validation_resolves_defaults_waits_and_cycles() {
    let (_home, dir) = fixture();
    for (text, valid, message) in [
        (CONDITION.to_owned(), true, ""),
        (CONDITION.replace("timeout='1h'\n", ""), false, "timeout"),
        (
            CONDITION
                .replace("timeout='1h'\n", "")
                .replace("pending='@wait'", "pending='@done', unknown='@failed'"),
            true,
            "",
        ),
        (
            CONDITION.replace("ready='@done', pending='@wait'", "ready='pr', pending='pr'"),
            true,
            "",
        ),
        (
            CONDITION.replace(
                "ready='@done', pending='@wait'",
                "ready='pr', pending='pr', unknown='@failed'",
            ),
            false,
            "cycle",
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert_eq!(
            report.errors.is_empty(),
            valid,
            "{text}: {:?}",
            report.errors
        );
        if !valid {
            assert!(
                report.errors.iter().any(|e| e.message.contains(message)),
                "{:?}",
                report.errors
            );
        } else {
            assert_eq!(report.rows[0].runner, "condition");
            assert!(report.rows[0].edges.iter().any(|e| e.outcome == "invalid"
                && e.target == "@failed"
                && e.source == EdgeSource::OnBreak));
        }
    }
    fs::write(
        dir.join("workflow.toml"),
        CONDITION
            .replace("timeout='1h'\n", "")
            .replace("pending='@wait'", "pending='@done'"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    let error = report
        .errors
        .iter()
        .find(|e| e.path == "checks.pr.timeout")
        .unwrap();
    assert!(
        error.message.contains("timeout") && error.message.contains("on.unknown"),
        "{}",
        error.message
    );
}

#[test]
fn condition_validation_refuses_malformed_cases_and_missing_required_fields() {
    let (_home, dir) = fixture();
    for (label, text, path) in [
        (
            "missing else",
            CONDITION.replace("else='pending'\n", ""),
            "checks.pr.else",
        ),
        (
            "empty cases",
            CONDITION.replace(
                "[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n",
                "cases=[]\n",
            ),
            "checks.pr",
        ),
        (
            "two kinds",
            CONDITION.replace("else='pending'", "run='true'\nelse='pending'"),
            "checks.pr",
        ),
        (
            "missing route",
            CONDITION.replace("ready='@done', ", ""),
            "checks.pr.on.ready",
        ),
        (
            "reserved case",
            CONDITION.replace("outcome='ready'", "outcome='hold'"),
            "checks.pr.cases.0.outcome",
        ),
        (
            "reserved else",
            CONDITION.replace("else='pending'", "else='unknown'"),
            "checks.pr.else",
        ),
        (
            "unknown value",
            CONDITION.replace("pr=merged", "!pr=unknown"),
            "checks.pr.cases.0.when",
        ),
        (
            "empty when",
            CONDITION.replace("when=['pr=merged']", "when=[]"),
            "checks.pr.cases.0.when",
        ),
        (
            "missing provider",
            CONDITION.replace("pr=merged", "window.5h.left>=40"),
            "checks.pr.provider",
        ),
        (
            "unknown provider",
            CONDITION
                .replace("else='pending'", "provider='missing-kind'\nelse='pending'")
                .replace("pr=merged", "window.5h.left>=40"),
            "checks.pr.provider",
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(
            report.errors.iter().any(|e| e.path.contains(path)),
            "{label}: {text}: {:?}",
            report.errors
        );
    }
    fs::write(
        dir.join("workflow.toml"),
        CONDITION
            .replace(
                "else='pending'",
                "provider='claude'\naccount='default'\nelse='pending'",
            )
            .replace("pr=merged", "window.5h.left>=40"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn wake_validation_accepts_selectors_and_strict_match_tables() {
    let (_home, dir) = fixture();
    for runner in [
        "run='true'\non.ok='@done'",
        "shell='true'\non={pass='@done',fail='@failed'}",
        "agent='claude'\nsummary='Review the work'\nprompt-file='prompts/build.md'\non.ok='@done'",
    ] {
        let text = format!(
            "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\n{runner}\nwake=['ci.*', 'team.stage', 'review.ready', {{signal='pr.opened', match={{path='*', branch='main'}}}}]\n"
        );
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{text}: {:?}", report.errors);
    }
}

#[test]
fn condition_subscriptions_follow_checkout_terms_not_branches_or_windows() {
    for (terms, expected) in [
        ("ci=failed", vec!["ci.*"]),
        ("!pr=open || pr.queue=queued", vec!["pr.*"]),
        ("pr.mergeable=conflicting", vec!["pr.*"]),
        ("pr.review=changes_requested", vec!["pr.*"]),
        ("team.stage=Done && ci=passed", vec!["team.stage", "ci.*"]),
        (
            "ci[main]=passed || pr[main]=merged || pr.queue[main]=queued",
            vec![],
        ),
        ("window.5h.left>=40", vec![]),
        (
            "pr.mergeable[main]=mergeable || pr.review[main]=approved",
            vec![],
        ),
    ] {
        let check: Check = toml::from_str(&format!(
            "else='pending'\n[[cases]]\nwhen=['{terms}']\noutcome='ready'\n"
        ))
        .unwrap();
        let subscriptions = check.subscriptions().unwrap();
        assert_eq!(
            subscriptions
                .iter()
                .map(|(selector, _)| selector.to_string())
                .collect::<std::collections::BTreeSet<_>>(),
            expected
                .into_iter()
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>(),
            "{terms}"
        );
        assert!(subscriptions.iter().all(|(_, matches)| matches.is_empty()));
    }
}

#[test]
fn forge_demand_includes_only_default_scoped_declared_wakes() {
    for runner in [
        "run='true'\non.ok='@done'",
        "shell='true'\non={pass='@done',fail='@failed'}",
        "agent='claude'\nsummary='Review the work'\nprompt-file='prompts/build.md'\non.ok='@done'",
    ] {
        for (wake, demanded) in [
            ("wake=['ci.failed']", true),
            ("wake=['pr.*']", true),
            (
                "wake=[{signal='pr.opened', match={repo='owner/repo'}}]",
                true,
            ),
            ("wake=[{signal='ci.*', match={path='*'}}]", false),
            (
                "wake=[{signal='pr.opened', match={path='/another'}}]",
                false,
            ),
            ("wake=[{signal='ci.failed', match={branch='main'}}]", false),
            ("wake=['review.ready', 'team.stage']", false),
            ("wake=[]", false),
            ("", false),
        ] {
            let text = format!(
                "[workflow]\non-break='@failed'\nscope='worktree'\nstart='now'\n[checks.now]\nrun='true'\non.ok='@done'\n[checks.later]\n{runner}\n{wake}\n"
            );
            let spec: WorkflowSpec = toml::from_str(&text).unwrap();
            let expected = demanded.then(std::collections::BTreeSet::new);
            assert_eq!(spec.forge_branches().unwrap(), expected, "{text}");

            let spec: WorkflowSpec = toml::from_str(&format!(
                "{text}[checks.condition]\nelse='pending'\non={{ready='@done',pending='@wait'}}\n[[checks.condition.cases]]\nwhen=['pr[main]=merged || ci[release]=passed']\noutcome='ready'\n"
            ))
            .unwrap();
            assert_eq!(
                spec.forge_branches().unwrap(),
                Some(["main".into(), "release".into()].into_iter().collect()),
                "{text}"
            );
        }
    }
}
