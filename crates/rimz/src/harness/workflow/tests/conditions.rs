use super::*;

const CONDITION: &str = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={ ready='@done', pending='@wait' }\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n";

#[test]
fn an_explicit_empty_cases_field_is_not_ignored_on_a_run_check() {
    let (home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ncases=[]\non.ok='@done'\n").unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(
        report.errors.iter().any(|error| error.path == "checks.pr"),
        "run must refuse cases, even an empty array: {:?}",
        report.errors
    );
}

#[test]
fn condition_validation_resolves_defaults_waits_and_cycles() {
    let (home, dir) = fixture();
    for (text, valid, message) in [
        (CONDITION.to_owned(), true, ""),
        (CONDITION.replace("timeout='1h'\n", ""), false, "timeout"),
        (
            CONDITION
                .replace("timeout='1h'\n", "")
                .replace("pending='@wait'", "pending='@done', unknown='@user'"),
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
                "ready='pr', pending='pr', unknown='@user'",
            ),
            false,
            "cycle",
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
            assert!(
                report.rows[0].edges.iter().any(|e| e.outcome == "invalid"
                    && e.target == "@user"
                    && e.source == "default")
            );
        }
    }
    fs::write(
        dir.join("workflow.toml"),
        CONDITION
            .replace("timeout='1h'\n", "")
            .replace("pending='@wait'", "pending='@done'"),
    )
    .unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
fn condition_validation_refuses_malformed_cases_and_inapplicable_fields() {
    let (home, dir) = fixture();
    for (label, text, path) in [
        ("missing else", CONDITION.replace("else='pending'\n", ""), "checks.pr.else"),
        ("empty cases", CONDITION.replace("[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n", "cases=[]\n"), "checks.pr"),
        ("two kinds", CONDITION.replace("else='pending'", "run='true'\nelse='pending'"), "checks.pr"),
        ("missing route", CONDITION.replace("ready='@done', ", ""), "checks.pr.on.ready"),
        ("dead route", CONDITION.replace("ready='@done'", "dead='@done', ready='@done'"), "checks.pr.on.dead"),
        ("reserved case", CONDITION.replace("outcome='ready'", "outcome='hold'"), "checks.pr.cases.0.outcome"),
        ("reserved else", CONDITION.replace("else='pending'", "else='unknown'"), "checks.pr.else"),
        ("reserved edge", CONDITION.replace("ready='@done'", "hold='@done', ready='@done'"), "checks.pr.on.hold"),
        ("unknown value", CONDITION.replace("pr=merged", "!pr=unknown"), "checks.pr.cases.0.when"),
        ("empty when", CONDITION.replace("when=['pr=merged']", "when=[]"), "checks.pr.cases.0.when"),
        ("zero hold", format!("{CONDITION}for='0s'\n"), "checks.pr.cases.0.for"),
        ("bad hold", format!("{CONDITION}for='3ms'\n"), "checks.pr.cases.0.for"),
        (
            "held wait",
            format!("{}for='2m'\n", CONDITION.replace("ready='@done'", "ready='@wait'")),
            "checks.pr.cases.0.for",
        ),
        ("bad key", format!("{CONDITION}key=true\n"), "checks.pr"),
        ("prompt", CONDITION.replace("else='pending'", "prompt-file='prompts/build.md'\nelse='pending'"), "checks.pr.prompt-file"),
        ("window", CONDITION.replace("else='pending'", "window='5h.left>=40'\nelse='pending'"), "checks.pr.window"),
        ("wake", CONDITION.replace("else='pending'", "wake=[]\nelse='pending'"), "checks.pr.wake"),
        ("missing provider", CONDITION.replace("pr=merged", "window.5h.left>=40"), "checks.pr.provider"),
        ("unneeded provider", CONDITION.replace("else='pending'", "provider='claude'\nelse='pending'"), "checks.pr.provider"),
        ("unknown provider", CONDITION.replace("else='pending'", "provider='missing-kind'\nelse='pending'").replace("pr=merged", "window.5h.left>=40"), "checks.pr.provider"),
        ("account", CONDITION.replace("else='pending'", "account='work'\nelse='pending'"), "checks.pr.account"),
        ("run else", "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\nelse='pending'\non.ok='@done'\n".into(), "checks.pr.else"),
        ("run prompt", "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\nprompt-file='prompts/build.md'\non.ok='@done'\n".into(), "checks.pr.prompt-file"),
        ("run unknown", "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.unknown='@done'\n".into(), "checks.pr.on.unknown"),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
        assert!(report.errors.iter().any(|e| e.path.contains(path)), "{label}: {text}: {:?}", report.errors);
        if label == "held wait" {
            assert!(report.errors.iter().any(|error| error
                .message
                .contains("a held case must route away from @wait")));
        }
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
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn wake_validation_accepts_selectors_and_strict_match_tables() {
    let (home, dir) = fixture();
    for runner in [
        "run='true'\non.ok='@done'",
        "shell='true'\non={pass='@done',fail='@user'}",
        "agent='claude'\nprompt-file='prompts/build.md'\non.ok='@done'",
    ] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\n{runner}\nwake=['ci.*', 'team.stage', 'review.ready', {{signal='pr.opened', match={{path='*', branch='main'}}}}]\n"
        );
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{text}: {:?}", report.errors);
    }
}

#[test]
fn wake_validation_refuses_invalid_and_unreachable_subscriptions() {
    let (home, dir) = fixture();
    for entry in [
        "'bad selector'",
        "{signal='review.ready', match={env=' '}}",
        "'ci.finished'",
        "{signal='ci.*', match={conclusion='success'}}",
        "'agent.*'",
        "'agent.idle'",
        "'team.idle'",
        "'team.*'",
        "{signal='review.ready', extra=true}",
        "{match={env='prod'}}",
        "{signal='review.ready', match={env=42}}",
    ] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.ok='@done'\nwake=[{entry}]\n"
        );
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.path == "checks.pr.wake"),
            "{entry}: {:?}",
            report.errors
        );
    }
}

#[test]
fn condition_subscriptions_follow_checkout_terms_not_branches_or_windows() {
    for (terms, expected) in [
        ("ci=failed", vec!["ci.*"]),
        ("!pr=open || pr.queue=queued", vec!["pr.*"]),
        ("team.stage=Done && ci=passed", vec!["team.stage", "ci.*"]),
        (
            "ci[main]=passed || pr[main]=merged || pr.queue[main]=queued",
            vec![],
        ),
        ("window.5h.left>=40", vec![]),
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
        "shell='true'\non={pass='@done',fail='@user'}",
        "agent='claude'\nprompt-file='prompts/build.md'\non.ok='@done'",
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
                "[workflow]\nscope='worktree'\nstart='now'\n[checks.now]\nrun='true'\non.ok='@done'\n[checks.later]\n{runner}\n{wake}\n"
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
