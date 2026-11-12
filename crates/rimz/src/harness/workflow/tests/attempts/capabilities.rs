use super::*;
use crate::store::workflow::StepLifecycle;

const JUDGED: &str = "[checks.judge]\nrun='cat answer.json'\ntimeout='1m'\non.next='@done'\n";

fn report(text: &str) -> (Vec<(String, String)>, Vec<String>) {
    let (_home, dir) = super::super::fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    let ignored = load(&dir)
        .map(|spec| spec.ignored.into_iter().map(|issue| issue.path).collect())
        .unwrap_or_default();
    let errors = report
        .errors
        .into_iter()
        .map(|issue| (issue.path, issue.message))
        .collect();
    (errors, ignored)
}

fn paths(errors: &[(String, String)]) -> Vec<&str> {
    errors.iter().map(|(path, _)| path.as_str()).collect()
}

#[test]
fn launch_settings_parse_validate_and_reach_the_host() {
    let settings = "account='work'\nmode='yolo'\neffort='high'\nfast=true\nbudget='$5/day'\n\
        system-prompt-file='prompts/build.md'\nworktree='feat-x'\n";
    let (errors, ignored) =
        report(&VALID.replace("max-visits = 3", &format!("max-visits = 3\n{settings}")));
    assert_eq!((errors, ignored), (vec![], vec![]));
    let (escaped, _) = report(&VALID.replace(
        "max-visits = 3",
        "max-visits = 3\nsystem-prompt-file='../outside.md'",
    ));
    assert_eq!(
        escaped,
        [(
            "steps.build.system-prompt-file".to_owned(),
            "example: steps.build.system-prompt-file: invalid workflow path ../outside.md: use a relative path with only normal components"
                .to_owned()
        )]
    );

    let (dir, state, record) = setup(&format!(
        "{STEP}account='work'\nmode='yolo'\neffort='high'\nfast=true\nbudget='$5/day'\n\
         system-prompt-file='prompt.md'\nworktree='feat-x'\n"
    ));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let launched = host.launched.borrow();
    let [launch] = launched.as_slice() else {
        panic!("one launch: {}", launched.len());
    };
    assert_eq!(
        launch.overrides,
        LaunchOverrides {
            account: Some("work".parse().unwrap()),
            mode: Some(crate::agents::PermissionMode::Yolo),
            effort: Some("high".into()),
            fast: Some(true),
            budget: Some("$5/day".parse().unwrap()),
            system_prompt_file: Some(dir.path().join("prompt.md").canonicalize().unwrap()),
            worktree: Some("feat-x".into()),
        }
    );
}

#[test]
fn a_bare_step_launches_with_no_overrides() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    assert_eq!(
        host.launched.borrow()[0].overrides,
        LaunchOverrides::default()
    );
}

#[test]
fn turn_and_stay_steps_may_end_the_run_at_then_and_until_steps_may_not() {
    for lifecycle in ["turn", "stay", "until"] {
        for terminal in ["@done", "@failed"] {
            let (errors, _) = report(&VALID.replace(
                "lifecycle = \"turn\"\nthen = \"pick\"",
                &format!("lifecycle = '{lifecycle}'\nthen = '{terminal}'"),
            ));
            let expected: &[&str] = if lifecycle == "until" {
                &["steps.build.then"]
            } else {
                &[]
            };
            assert_eq!(
                paths(&errors),
                expected,
                "{lifecycle} {terminal}: {errors:?}"
            );
        }
    }
    let (errors, _) = report(&VALID.replace("then = \"pick\"", "then = ['@done', 'pick']"));
    assert_eq!(
        errors,
        [
            (
                "steps.build.then".to_owned(),
                "example: steps.build.then: then lists only checks; @done or @failed stands alone as then".to_owned()
            ),
            (
                "steps.build.then".to_owned(),
                "example: steps.build.then: @done requires a direct route to the next listed check pick".to_owned()
            ),
        ]
    );
}

#[test]
fn on_failed_names_a_live_target_on_agent_steps() {
    let (errors, ignored) =
        report(&VALID.replace("max-visits = 3", "max-visits = 3\non.failed = 'pick'"));
    assert_eq!((errors, ignored), (vec![], vec![]));
    let (errors, _) =
        report(&VALID.replace("max-visits = 3", "max-visits = 3\non.failed = 'nowhere'"));
    assert_eq!(paths(&errors), ["steps.build.on.failed"], "{errors:?}");
}

#[test]
fn on_failed_takes_a_died_timed_out_or_limited_ending_and_a_clean_end_follows_then() {
    let rescued = STEP.replace("then='@done'", "then='@done'\non.failed='@failed'");
    for (definition, ending, expected) in [
        (&rescued, "died", RunState::Failed),
        (&rescued, "timeout", RunState::Failed),
        (&rescued, "limited", RunState::Failed),
        (&rescued, "refused", RunState::Failed),
        (&rescued, "ended", RunState::Done),
        (&STEP.to_owned(), "died", RunState::Done),
        (&STEP.to_owned(), "timeout", RunState::Done),
        (&STEP.to_owned(), "limited", RunState::Done),
        (&STEP.to_owned(), "refused", RunState::Done),
    ] {
        let text = if ending == "timeout" {
            format!("{definition}timeout='1h'\n")
        } else {
            definition.clone()
        };
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        if ending == "refused" {
            // A budget-refused launch: the attempt ends limited before any turn runs.
            *host.failure.borrow_mut() = Some(LaunchFailure {
                outcome: AttemptOutcome::Limited,
                reason: "fleet budget closed".into(),
            });
        }
        let mut live = advance_ok(&state, &record.id, &host);
        match ending {
            "died" => host.observed.borrow_mut().status = RunStatus::Failed,
            "limited" => host.observed.borrow_mut().status = RunStatus::BudgetExceeded,
            "ended" => host.observed.borrow_mut().status = RunStatus::Completed,
            "refused" => {}
            _ => {
                live.attempt.as_mut().unwrap().started_at =
                    Timestamp::now() - std::time::Duration::from_secs(7200);
                update(&state, &live);
            }
        }
        let ended = advance_ok(&state, &record.id, &host);
        let routed = records::journal(&state.workflow(&record.id))
            .unwrap()
            .into_iter()
            .filter_map(|row| match row.event {
                JournalEvent::Route { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        let failed = definition.contains("on.failed") && ending != "ended";
        assert_eq!(ended.state, expected, "{ending}: {ended:?}");
        let spec = super::super::super::spec::parse(&text).unwrap();
        let word = if ending == "refused" {
            "limited"
        } else {
            ending
        };
        let shown = if failed { "@failed" } else { "@done" };
        assert_eq!(
            spec.target("build", word).as_deref(),
            Some(shown),
            "{ending}: inspection names the route settlement takes"
        );
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        let trail = hops(&spec, &journal, None)
            .into_iter()
            .filter(|hop| hop.node == "build" && hop.target.is_some())
            .map(|hop| (hop.outcome, hop.target))
            .collect::<Vec<_>>();
        assert_eq!(
            trail.len(),
            1,
            "{ending}: one hop out of the step: {trail:?}"
        );
        assert_eq!(trail[0].1.as_deref(), Some(shown), "{ending}: {trail:?}");
        assert_eq!(
            routed.iter().any(|outcome| outcome == "failed"),
            failed,
            "{ending}: {routed:?}"
        );
    }
}

#[test]
fn an_until_timeout_ends_the_session_from_its_launch() {
    let text = STEP
        .replace("lifecycle='turn'", "lifecycle='until'\ntimeout='1h'")
        .replace("then='@done'", "then='judge'")
        + JUDGED;
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    assert_eq!(
        live.attempt.as_ref().unwrap().lifecycle,
        StepLifecycle::Until
    );
    live.attempt.as_mut().unwrap().started_at =
        Timestamp::now() - std::time::Duration::from_secs(7200);
    update(&state, &live);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(
        (
            ended.prev_attempt.as_ref().map(|attempt| attempt.outcome),
            ended.node.as_str(),
            host.stops.get(),
        ),
        (Some(AttemptOutcome::Timeout), "judge", 1),
        "{ended:?}"
    );
}

#[test]
fn a_default_route_classifies_as_default() {
    let text = VALID.replace(
        "ready = \"build\"",
        "ready = { to = 'build', default = true }",
    );
    let (errors, ignored) = report(&text);
    assert_eq!((errors, ignored), (vec![], vec![]));
    let (_home, dir) = super::super::fixture();
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    let spec = load(&dir).unwrap();
    let edges = serde_json::to_value(spec.edges("pick")).unwrap();
    let ready = edges
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["outcome"] == "ready")
        .unwrap();
    assert_eq!(
        (&ready["target"], &ready["source"]),
        (&json!("build"), &json!("default"))
    );
    let done = edges
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["outcome"] == "done")
        .unwrap();
    assert_eq!(done["source"], "explicit");
}

#[test]
fn a_message_step_hands_its_pinned_session_to_the_host() {
    let tell = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[steps.tell]\nmessage='@planner'\nprompt-file='prompt.md'\nthen='@done'\n";
    for (definition, expected) in [
        (format!("{tell}session='ses-1'\n"), Some("ses-1")),
        (tell.to_owned(), None),
    ] {
        let (_dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance(&state, &record.id, &host).unwrap();
        assert_eq!(
            *host.message_sessions.borrow(),
            vec![expected.map(crate::ids::AgentSessionId::from)]
        );
    }
}
