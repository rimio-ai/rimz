use super::*;
use crate::store::workflow::StepLifecycle;

const JUDGED: &str = "[script.judge]\nrun='cat answer.json'\ntimeout='1m'\non.next='@done'\n";

fn report(text: &str) -> Vec<(String, String)> {
    let (_home, dir) = super::super::fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    validate(&dir, &LoadedDefinitions::default(), None)
        .errors
        .into_iter()
        .map(|issue| (issue.path, issue.message))
        .collect()
}

fn paths(errors: &[(String, String)]) -> Vec<&str> {
    errors.iter().map(|(path, _)| path.as_str()).collect()
}

#[test]
fn launch_settings_parse_validate_and_reach_the_host() {
    let settings = "account='work'\nmode='yolo'\neffort='high'\nfast=true\nbudget='$5/day'\n\
        system-prompt-file='prompts/build.md'\nworktree='feat-x'\n";
    let errors = report(&VALID.replace("max-visits = 3", &format!("max-visits = 3\n{settings}")));
    assert_eq!(errors, vec![]);
    let escaped = report(&VALID.replace(
        "max-visits = 3",
        "max-visits = 3\nsystem-prompt-file='../outside.md'",
    ));
    assert_eq!(
        escaped,
        [(
            "agent.build.system-prompt-file".to_owned(),
            "example: agent.build.system-prompt-file: invalid workflow path ../outside.md: use a relative path with only normal components"
                .to_owned()
        )]
    );

    let (dir, state, record) = setup(&format!(
        "{STEP}account='work'\nmode='yolo'\neffort='high'\nfast=true\nbudget='$5/day'\n\
         system-prompt-file='prompt.md'\nworktree='feat-x'\n"
    ));
    let host = FakeHost::new();
    // Admission resolves the step's agent on the named account, which this home lacks.
    *host.groups.borrow_mut() = Some(Vec::new());
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
fn either_lifecycle_may_end_the_run_at_then_and_then_names_one_target() {
    for lifecycle in ["'turn'", "'session'", "'session'\nrest-check = ['pick']"] {
        for terminal in ["@done", "@failed"] {
            let errors = report(&VALID.replace(
                "lifecycle = \"turn\"\nthen = \"pick\"",
                &format!("lifecycle = {lifecycle}\nthen = '{terminal}'"),
            ));
            assert_eq!(
                paths(&errors),
                Vec::<&str>::new(),
                "{lifecycle} {terminal}: {errors:?}"
            );
        }
    }
    let errors = report(&VALID.replace("then = \"pick\"", "then = ['@done', 'pick']"));
    assert_eq!(paths(&errors), ["agent.build.then"], "{errors:?}");
}

#[test]
fn on_failed_names_a_live_target_on_agent_steps() {
    // R10: pick routes back to build, so the visits and capacity failures need routes of
    // their own.
    let errors = report(&VALID.replace(
        "max-visits = 3",
        "max-visits = 3\non.failed = 'pick'\non.visits = '@failed'\non.capacity = '@failed'",
    ));
    assert_eq!(errors, vec![]);
    let errors = report(&VALID.replace("max-visits = 3", "max-visits = 3\non.failed = 'nowhere'"));
    assert_eq!(paths(&errors), ["agent.build.on.failed"], "{errors:?}");
}

#[test]
fn on_failed_takes_a_died_timed_out_or_limited_ending_and_a_clean_end_follows_then() {
    let rescued = STEP.replace("then='@done'", "then='@done'\non.failed='@done'");
    for (definition, ending, expected) in [
        (&rescued, "died", RunState::Done),
        (&rescued, "timeout", RunState::Done),
        (&rescued, "limited", RunState::Done),
        (&rescued, "refused", RunState::Done),
        (&rescued, "ended", RunState::Done),
        (&STEP.to_owned(), "died", RunState::Failed),
        (&STEP.to_owned(), "timeout", RunState::Failed),
        (&STEP.to_owned(), "limited", RunState::Failed),
        (&STEP.to_owned(), "refused", RunState::Failed),
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
        let failed = ending != "ended";
        assert_eq!(ended.state, expected, "{ending}: {ended:?}");
        let spec = super::super::super::spec::parse(&text).unwrap();
        let word = if ending == "refused" {
            "limited"
        } else {
            ending
        };
        let shown = if failed && !definition.contains("on.failed") {
            "@failed"
        } else {
            "@done"
        };
        assert_eq!(
            spec.target("build", word)
                .as_ref()
                .map(|target| target.as_str()),
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
        assert_eq!(
            trail[0].1.as_ref().map(|target| target.as_str()),
            Some(shown),
            "{ending}: {trail:?}"
        );
        assert_eq!(
            routed.iter().any(|outcome| outcome == word),
            failed,
            "{ending}: {routed:?}"
        );
    }
}

#[test]
fn a_session_timeout_ends_it_from_its_launch_with_or_without_a_rest_check() {
    for rest in ["", "\nrest-check=['judge']"] {
        let text = STEP
            .replace(
                "lifecycle='turn'",
                &format!("lifecycle='session'\ntimeout='1h'{rest}"),
            )
            .replace("then='@done'", "then='judge'")
            + JUDGED;
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        assert_eq!(
            live.attempt.as_ref().unwrap().lifecycle,
            StepLifecycle::Session
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
            (Some(AttemptOutcome::Timeout), "build", 1),
            "{rest}: {ended:?}"
        );
    }
}

#[test]
fn only_written_routes_classify_as_explicit() {
    let (_home, dir) = super::super::fixture();
    fs::write(dir.join("workflow.toml"), VALID).unwrap();
    let spec = load(&dir).unwrap();
    let written = &spec.authored().checks["pick"].on;
    let edges = spec.edges("pick");
    assert!(
        edges
            .iter()
            .any(|edge| edge.source != crate::harness::workflow::EdgeSource::Explicit)
    );
    for edge in &edges {
        assert_eq!(
            edge.source == crate::harness::workflow::EdgeSource::Explicit,
            written.contains_key(&edge.outcome),
            "{edge:?}"
        );
    }
}

#[test]
fn a_message_step_hands_its_pinned_session_to_the_host() {
    let tell = "[workflow]\nscope='worktree'\nstart='tell'\n[message.tell]\nmessage='@planner'\nprompt-file='prompt.md'\nthen='@done'\n";
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
