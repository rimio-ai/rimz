use super::*;
use crate::disk::paths::StatePaths;
use crate::store::workflow::{RunRecord, RunState};

const CONDITION: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={ ready='@done', pending='@wait', unknown='@wait' }\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\n";

#[test]
fn inline_cases_match_table_blocks_in_order() {
    let (_home, dir) = fixture();
    let header = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={merged='@done',red='@failed',pending='@wait',unknown='@wait'}\n";
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
            CONDITION.replace("timeout='1h'\n", "").replace(
                "pending='@wait', unknown='@wait'",
                "pending='@done', unknown='@failed'",
            ),
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
                "ready='@done', pending='@wait', unknown='@wait'",
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
fn a_condition_without_on_unknown_refuses_and_gets_no_implicit_wait() {
    let (_home, dir) = fixture();
    let text = CONDITION.replace(", unknown='@wait'", "");
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    let errors = report
        .errors
        .iter()
        .map(|error| (error.path.as_str(), error.message.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        errors,
        [(
            "checks.pr.on.unknown",
            "example: checks.pr.on.unknown: a condition check requires on.unknown; route it to @wait to keep waiting while a term is unknown"
        )]
    );
    let spec: Definition = toml::from_str(&text).unwrap();
    assert!(
        !spec
            .edges("pr")
            .iter()
            .any(|edge| edge.outcome == "unknown"),
        "{:?}",
        spec.edges("pr")
    );
}

/// Advance a condition run once, due now, and return the record it leaves.
fn advance_condition(state: &StatePaths, record: &RunRecord) -> RunRecord {
    let path = state.workflow(&record.id);
    let mut due = crate::store::workflow::load(&path).unwrap();
    due.recheck_at = None;
    attempts::update(state, &due);
    let result = advance(state, &record.id, &FakeHost::new());
    assert!(
        result.is_ok(),
        "a condition evaluates in-process: {result:?}"
    );
    crate::store::workflow::load(&path).unwrap()
}

/// Publish a forge probe for the run's checkout: `fresh` after the check's entry, else before.
fn publish_pr(dir: &Path, state: &StatePaths, record: &RunRecord, fresh: bool, pr: &str) {
    let at = if fresh {
        jiff::Timestamp::now()
    } else {
        record.entered_at - std::time::Duration::from_secs(1)
    };
    let path = crate::RuntimePaths::for_state(state)
        .unwrap()
        .lane_path("pr-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let checkout = dir.to_string_lossy();
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "branch_ci":{}, "path_repos":{(checkout.as_ref()):"repo"},
            "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(),"ok":true}},
            "states":{(checkout.as_ref()):{"state":pr}}
        }))
        .unwrap(),
    )
    .unwrap();
}

fn held_condition(unknown: &str) -> String {
    format!(
        "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\ntimeout='1h'\non={{ ready='@done', pending='@wait', unknown='{unknown}' }}\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='ready'\nfor='2m'\n"
    )
}

fn closed_waits(state: &StatePaths, record: &RunRecord) -> Vec<String> {
    crate::store::workflow::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            crate::store::workflow::JournalEvent::Wait { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect()
}

#[test]
fn a_hold_survives_unknown_and_completes_on_true_without_restarting() {
    let (dir, state, record) = attempts::setup(&held_condition("@wait"));
    publish_pr(dir.path(), &state, &record, true, "merged");
    let held = advance_condition(&state, &record);
    let wait = held.wait.clone().unwrap();
    assert!(wait.is_hold(), "{wait:?}");
    let first = wait.first;
    publish_pr(dir.path(), &state, &record, false, "merged");
    let unknown = advance_condition(&state, &record);
    let wait = unknown.wait.clone().unwrap();
    assert_eq!(
        (wait.outcome.as_str(), wait.case, wait.first, wait.count),
        ("hold", Some(0), first, 2),
        "an unknown answer keeps the hold's start"
    );
    assert!(closed_waits(&state, &record).is_empty());
    publish_pr(dir.path(), &state, &record, true, "merged");
    let mut again = advance_condition(&state, &record);
    let wait = again.wait.as_mut().unwrap();
    assert_eq!((wait.case, wait.first, wait.count), (Some(0), first, 3));
    wait.first = jiff::Timestamp::now() - std::time::Duration::from_secs(121);
    attempts::update(&state, &again);
    let done = advance_condition(&state, &record);
    assert_eq!(done.state, RunState::Done);
    assert_eq!(done.input.as_ref().unwrap()["outcome"], "ready");
    assert_eq!(closed_waits(&state, &record), ["hold"]);
}

#[test]
fn a_hold_under_unknown_records_the_answer_and_a_true_answer_clears_it() {
    let (dir, state, record) = attempts::setup(&held_condition("@wait"));
    publish_pr(dir.path(), &state, &record, true, "merged");
    let first = advance_condition(&state, &record).wait.unwrap().first;
    publish_pr(dir.path(), &state, &record, false, "merged");
    let unknown = advance_condition(&state, &record).wait.unwrap();
    assert!(
        unknown.is_hold()
            && unknown.first == first
            && unknown
                .unknown
                .as_deref()
                .is_some_and(|reason| reason.contains("pr")),
        "{unknown:?}"
    );
    publish_pr(dir.path(), &state, &record, true, "merged");
    let held = advance_condition(&state, &record).wait.unwrap();
    assert_eq!(
        (
            held.is_hold(),
            held.first,
            held.unknown,
            held.detail.as_deref()
        ),
        (true, first, None, Some("ready")),
    );
}

#[test]
fn a_hold_ends_when_its_case_reads_false() {
    let (dir, state, record) = attempts::setup(&held_condition("@wait"));
    publish_pr(dir.path(), &state, &record, true, "merged");
    let first = advance_condition(&state, &record).wait.unwrap().first;
    publish_pr(dir.path(), &state, &record, true, "open");
    let pending = advance_condition(&state, &record).wait.unwrap();
    assert_eq!(pending.outcome, "pending");
    assert_eq!(closed_waits(&state, &record), ["hold"]);
    publish_pr(dir.path(), &state, &record, true, "merged");
    let restarted = advance_condition(&state, &record).wait.unwrap();
    assert!(
        restarted.is_hold() && restarted.first > first,
        "{restarted:?}"
    );
}

#[test]
fn an_unknown_routed_away_from_wait_leaves_the_node_and_ends_the_hold() {
    let (dir, state, record) = attempts::setup(&held_condition("@done"));
    publish_pr(dir.path(), &state, &record, true, "merged");
    assert!(advance_condition(&state, &record).wait.unwrap().is_hold());
    publish_pr(dir.path(), &state, &record, false, "merged");
    let left = advance_condition(&state, &record);
    assert_eq!(left.state, RunState::Done);
    assert_eq!(left.input.as_ref().unwrap()["outcome"], "unknown");
    assert!(left.wait.is_none());
    assert_eq!(closed_waits(&state, &record), ["hold"]);
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
        let subscriptions = Node::of_check(&check).subscriptions().unwrap();
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
            let spec: Definition = toml::from_str(&text).unwrap();
            let expected = demanded.then(std::collections::BTreeSet::new);
            assert_eq!(spec.forge_branches().unwrap(), expected, "{text}");

            let spec: Definition = toml::from_str(&format!(
                "{text}[checks.condition]\nelse='pending'\non={{ready='@done',pending='@wait',unknown='@wait'}}\n[[checks.condition.cases]]\nwhen=['pr[main]=merged || ci[release]=passed']\noutcome='ready'\n"
            ))
            .unwrap();
            assert_eq!(
                spec.forge_branches().unwrap(),
                Some(["main".into(), "release".into()].into_iter().collect()),
                "{text}"
            );
        }
    }
    for (subscribe, demanded) in [
        ("subscribe=['ci.failed']", true),
        (
            "subscribe=[{signal='ci.failed', match={branch='main'}}]",
            false,
        ),
        ("subscribe=['review.ready']", false),
    ] {
        let text = format!(
            "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nlifecycle='stay'\nprompt-file='prompts/build.md'\nthen=['done']\n{subscribe}\n[checks.done]\nrun='true'\non.ok='@done'\n"
        );
        let spec: Definition = toml::from_str(&text).unwrap();
        let expected = demanded.then(std::collections::BTreeSet::new);
        assert_eq!(spec.forge_branches().unwrap(), expected, "{text}");
    }
}
