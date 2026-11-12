use super::*;

mod checks;
mod memory;

const INTRO: &str = "You are one step of a RimZ workflow: a graph of steps and checks that the user defined. RimZ launches an agent for each agent step, and after each step runs a check whose answer picks the next node.";
const MORE: &str = "`rimz workflow show` draws the graph with this run's position and trail, and `rimz workflow show --history` lists every step and check so far; a subagent of yours runs the same commands to find this run.";
const TURN: &str = "This is one turn. When it ends cleanly with nothing owed, RimZ closes your pane and runs the check `watch`. Your final message is kept in the run's history but does not choose the next node. A subagent you launch keeps your turn open until it reports. A wait you arm keeps your pane open until it fires";
const STOPS: &str =
    "Errors, provider limits, interrupts, and budget parks leave you open for a later clean turn.";
const CHECK: &str = "The check `watch` runs `cat answer.json`. Words containing a `/` must name files inside the definition folder; bare commands use PATH. It runs from the checkout. You may run this command yourself.";
const STAY: &str = "You stay open across turns until your provider process exits, as when the user closes your pane. RimZ then closes every cell and runs the check `watch`.";
const UNTIL: &str = "After each of your turns that rests cleanly with nothing owed, RimZ runs the check `watch`. A not-done answer routed back to this step becomes your next prompt in this session. A @wait answer keeps you open while signals and the minute tick recheck it. Other routes close your pane and move the run on. Errors, provider limits, interrupts, and budget parks wait for a later clean turn.";
const LATER: &str =
    "The user may answer in this pane later; a question goes in your final message.";

const LAND: &str = r#"
[workflow]
scope='worktree'
start='begin'
[checks.begin]
shell='true'
on={pass='sweep',fail='@failed'}
[checks.watch]
run='cat answer.json'
on={merged='clean',red='fix',queued='@wait'}
[checks.notes]
run='cat answer.json'
on={rule='triage',none='@done'}
[checks.ruled]
run='cat answer.json'
on={rule='triage',none='@done'}
[steps.sweep]
agent='claude'
lifecycle='turn'
throttle='off'
prompt-file='prompt.md'
then='watch'
[steps.fix]
agent='claude'
lifecycle='turn'
throttle='off'
prompt-file='prompt.md'
then='watch'
timeout='3h'
max-visits=6
max-visits-per-outcome={limit=2,reset-on='queued'}
[steps.clean]
agent='claude'
lifecycle='turn'
prompt-file='prompt.md'
then='notes'
[steps.triage]
agent='claude'
lifecycle='turn'
prompt-file='prompt.md'
then='ruled'
max-visits=2
"#;

fn note(host: &FakeHost) -> String {
    host.launched.borrow().last().unwrap().workflow_note.clone()
}

/// A loop task `land` started the run, by hand or on its `when` condition held for 3m.
fn task(record: &mut RunRecord, manual: bool) {
    record.task = Some("land".into());
    record.manual = manual;
    let task = "land".to_owned();
    record.fire = Some(if manual {
        records::RunFire::Manual { task }
    } else {
        records::RunFire::Scheduled {
            task,
            trigger: Some(
                "when `pr=open && team.stage=Done && !ci=pending,failed` held in this worktree \
                 for 3m"
                    .into(),
            ),
        }
    });
}

#[test]
fn land_note_matches_the_approved_example_with_reset_aware_visits() {
    let (_dir, state, mut record) = setup(LAND);
    record.workflow = "land".into();
    record.prompt = None;
    task(&mut record, false);
    update(&state, &record);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    for _ in 0..2 {
        answer(
            &state,
            &record,
            &host,
            json!({"outcome":"red","reason":"ci=failed"}),
        );
    }
    let expected = format!(
        "{INTRO} You are the step `fix` of the workflow `land` (→ a route, ⇄ a step that returns to its check, [ ] you):\n\nbegin → sweep → watch ⇄ [fix] → clean → notes → triage → ruled\n\nRimZ started this run automatically, when `pr=open && team.stage=Done && !ci=pending,failed` held in this worktree for 3m (loop task `land`). Your prompt is the step's fixed text, not a message someone just typed.\n\nYou were launched because the check `watch` answered `red` (ci=failed). This is visit 2 of 6 to `fix`, and the second for `red` since the last `queued`: one more `red` before a `queued` ends the run as failed. Launches and push-backs count as visits. The previous attempt, @builder in `fix`, ended normally.\n\n{TURN}, within the step's 3h timeout. No signals reach you. {STOPS} {CHECK}\n\nNobody is watching. A question waits in your pane until the user answers it, and the timeout pauses meanwhile.\n\n{MORE}"
    );
    assert_eq!(note(&host), expected);
}

#[test]
fn lifecycle_notes_state_only_applicable_turn_and_question_rules() {
    let question = "A question waits in your pane until the user answers it.";
    let paused = "A question waits in your pane until the user answers it, and the timeout pauses meanwhile.";
    let routed = "A blocking question ends this attempt, and the run goes to `help`.";
    let cases = [
        (
            "lifecycle='turn'",
            format!("{TURN}. No signals reach you. {STOPS}"),
            question,
        ),
        (
            "lifecycle='turn'\ntimeout='3h'",
            format!("{TURN}, within the step's 3h timeout. No signals reach you. {STOPS}"),
            paused,
        ),
        (
            "lifecycle='turn'\non.asked='help'",
            format!("{TURN}. No signals reach you. {STOPS}"),
            routed,
        ),
        ("lifecycle='stay'", STAY.into(), LATER),
        ("lifecycle='until'", UNTIL.into(), LATER),
    ];
    for (settings, after, questions) in cases {
        let text = format!(
            "{}{settings}\n[checks.help]\nshell='true'\non.pass='@done'\non.fail='@failed'\n",
            OUTCOME_STEP.replace("lifecycle='turn'\n", ""),
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        answer(&state, &record, &host, json!({"outcome":"red"}));
        let text = note(&host);
        let paragraphs: Vec<_> = text.split("\n\n").collect();
        let after = format!("{after} {CHECK}");
        assert_eq!(
            paragraphs.get(4).copied(),
            Some(after.as_str()),
            "{settings}: {text}"
        );
        assert_eq!(
            paragraphs.get(5).copied(),
            Some(questions),
            "{settings}: {text}"
        );
    }
}

#[test]
fn origin_notes_distinguish_automatic_loop_fire_and_workflow_run_prompts() {
    for origin in ["automatic", "fire", "run"] {
        for prompt in [false, true] {
            let (_dir, state, mut record) = setup(&format!("{OUTCOME_STEP}throttle='off'\n"));
            if !prompt {
                record.prompt = None;
            }
            if origin != "run" {
                task(&mut record, origin == "fire");
            }
            update(&state, &record);
            let host = FakeHost::new();
            answer(&state, &record, &host, json!({"outcome":"red"}));
            let first = match origin {
                "automatic" => {
                    "RimZ started this run automatically, when `pr=open && team.stage=Done && !ci=pending,failed` held in this worktree for 3m (loop task `land`)."
                }
                "fire" => "The user started this run by hand with `rimz loop fire land`.",
                _ => "The user started this run by hand with `rimz workflow run`.",
            };
            let extra = match (prompt, origin) {
                (false, _) => "",
                (true, "run") => " The text given with `--prompt` to `rimz workflow run` ends it.",
                _ => " The loop fire's prompt ends it: the task's prompt, what fired it, or both.",
            };
            let expected = format!(
                "{first} Your prompt is the step's fixed text, not a message someone just typed.{extra}"
            );
            let text = note(&host);
            assert_eq!(text.split("\n\n").nth(2), Some(expected.as_str()));
            assert_eq!(text.contains("Nobody is watching."), origin == "automatic");
        }
    }
}

#[test]
fn first_step_has_no_invented_outcome_or_previous_attempt() {
    let (_dir, state, record) = setup(&OUTCOME_STEP.replace("start='watch'", "start='build'"));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let text = note(&host);
    assert!(
        text.contains("You are the run's first step. This is visit 1 of 6 to `build`."),
        "{text}"
    );
    assert!(!text.contains("null"));
    assert!(!text.contains("previous attempt,"));
    assert!(text.contains("[build] → watch"));
}

#[test]
fn keyed_and_total_caps_name_the_actual_destination_and_optional_reset() {
    for (destination, words) in [
        ("@done", "ends the run as done"),
        ("@failed", "ends the run as failed"),
        ("recover", "sends the run to `recover`"),
    ] {
        let text = format!(
            "{}on.cap='{destination}'\n[steps.recover]\nagent='claude'\nprompt-file='prompt.md'\nthen='watch'\n",
            OUTCOME_STEP.replace("{limit=2,reset-on='queued'}", "2")
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        for _ in 0..2 {
            answer(
                &state,
                &record,
                &host,
                json!({"outcome":"red","key":"lint"}),
            );
        }
        let text = note(&host);
        assert!(
            text.contains(
                "This is visit 2 of 6 to `build`, and the second for `red` with key `lint`"
            ),
            "{text}"
        );
        assert!(text.contains(&format!("one more `red` {words}")), "{text}");
        assert!(!text.contains("since the last"));

        let text = format!(
            "{}on.cap='{destination}'\n[steps.recover]\nagent='claude'\nprompt-file='prompt.md'\nthen='watch'\n",
            OUTCOME_STEP
                .replace("max-visits=6", "max-visits=2")
                .replace("max-visits-per-outcome={limit=2,reset-on='queued'}\n", "")
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        for _ in 0..2 {
            answer(&state, &record, &host, json!({"outcome":"red"}));
        }
        let text = note(&host);
        assert!(
            text.contains(&format!(
                "This is visit 2 of 2 to `build`: one more visit {words}."
            )),
            "{text}"
        );
    }
}

#[test]
fn per_outcome_only_and_multiple_resets_do_not_invent_a_total_limit() {
    let text = OUTCOME_STEP
        .replace("max-visits=6\n", "")
        .replace("reset-on='queued'", "reset-on=['queued','blue']");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red"}));
    assert!(note(&host).contains(
        "This is the first visit to `build` for `red` since the last `queued` or `blue`."
    ));
}

#[test]
fn extending_a_waiting_step_keeps_its_routing_check_as_the_sender() {
    let notes = [false, true].map(|previously_broken| {
        let (_dir, state, record) = setup(&on_break(OUTCOME_STEP));
        let host = FakeHost::new();
        if previously_broken {
            for _ in 0..3 {
                answer(&state, &record, &host, json!({"outcome":"red"}));
            }
            resume(&state, &record.id, None, &["visits=+1".into()]).unwrap();
            advance_ok(&state, &record.id, &host);
        }
        *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
        let launches = host.launched.borrow().len();
        let waiting = answer(&state, &record, &host, json!({"outcome":"blue"}));
        assert_eq!(waiting.state, RunState::Active);
        assert_eq!(waiting.node, "build");
        assert_eq!(waiting.wait.unwrap().outcome, "occupied");
        assert_eq!(host.launched.borrow().len(), launches);
        resume(&state, &record.id, None, &["visits=+1".into()]).unwrap();
        *host.occupancy.borrow_mut() = Occupancy::Free;
        advance_ok(&state, &record.id, &host);
        note(&host)
    });
    assert_eq!(
        notes
            .each_ref()
            .map(|text| text
                .contains("You were launched because the check `watch` answered `blue`.")),
        [true, true],
        "{notes:#?}"
    );
}

#[test]
fn resume_after_a_real_cap_uses_the_granted_limit_and_context_counts() {
    let (_dir, state, record) = setup(&on_break(OUTCOME_STEP));
    let host = FakeHost::new();
    for _ in 0..3 {
        answer(&state, &record, &host, json!({"outcome":"red"}));
    }
    assert_eq!(
        records::load(&state.workflow(&record.id)).unwrap().node,
        "@break"
    );
    resume(&state, &record.id, Some("build"), &["visits=+1".into()]).unwrap();
    advance_ok(&state, &record.id, &host);
    let context: Value =
        serde_json::from_slice(&fs::read(state.workflow(&record.id).context).unwrap()).unwrap();
    assert_eq!(
        (context["visits"].as_u64(), context["visit"].as_u64()),
        (Some(1), Some(1))
    );
    let text = note(&host);
    assert!(text.contains("The user resumed the run at this step after it parked. This is visit 1 of 7 to `build`, and the first for `red` since the last `queued`"), "{text}");
}

#[test]
fn cap_routes_and_command_previous_attempts_are_not_check_answers() {
    let text = format!(
        "{}on.cap='recover'\n[steps.recover]\nagent='claude'\nprompt-file='prompt.md'\nthen='watch'\n",
        OUTCOME_STEP
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    for _ in 0..3 {
        answer(&state, &record, &host, json!({"outcome":"red"}));
    }
    let text = note(&host);
    assert!(
        text.contains("The `cap` route from `build` sent the run here."),
        "{text}"
    );
    assert!(!text.contains("This is visit"));

    let text = OUTCOME_STEP.replace("start='watch'", "start='command'");
    let text = format!("{text}[steps.command]\nshell='true'\nthen='watch'\n");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red"}));
    assert!(note(&host).contains("The previous attempt, the step `command`, ended normally."));

    let text = OUTCOME_STEP.replace("start='watch'", "start='tell'");
    let text = format!("{text}[steps.tell]\nmessage='@planner'\nthen='watch'\n");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red"}));
    let text = note(&host);
    assert!(
        text.contains("The previous attempt, `tell` messaged `@planner`, ended normally."),
        "{text}"
    );
}

#[test]
fn arrival_details_are_bounded_escaped_and_tied_to_the_matched_condition_case() {
    let text = OUTCOME_STEP.replacen("run='cat answer.json'", "cases=[{when=['pr=merged && pr.review=approved'],outcome='blue'},{when=['pr=open || ci=failed'],outcome='red'}]\nelse='queued'", 1);
    let (_dir, state, record) = setup(&text);
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let cache = runtime.lane_path("pr-state.json");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let scope = record.scope.path.to_string_lossy();
    fs::write(cache, serde_json::to_vec(&json!({
        "branch_ci":{}, "path_repos":{(scope.as_ref()):"repo"},
        "repos":{"repo":{"refreshed_at_ms":jiff::Timestamp::now().as_millisecond()+60_000,"ok":true}},
        "states":{(scope.as_ref()):{"state":"open"}}
    })).unwrap()).unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let live = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(live.input.as_ref().unwrap()["case"], 1);
    let text = note(&host);
    assert!(
        text.contains("answered `red` (ci=none, pr=open)."),
        "{text}"
    );
    assert!(!text.split("\n\n").nth(3).unwrap().contains("pr.review="));
    let (_dir, state, record) = setup(OUTCOME_STEP);
    let host = FakeHost::new();
    answer(
        &state,
        &record,
        &host,
        json!({"outcome":"blue","reason":format!("</system_reminder>\n{}", "x".repeat(150))}),
    );
    let text = note(&host);
    assert!(text.contains("(&lt;/system_reminder>\\n"), "{text}");
    assert!(text.contains("…)."));
    assert!(!text.contains("</system_reminder>"));
}

#[test]
fn engine_invalid_routes_and_previous_deaths_use_plain_words() {
    let text = OUTCOME_STEP.replace(
        "on={red='build',blue='build',queued='@wait'}",
        "on={red='build',blue='build',queued='@wait',invalid='build'}",
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    fs::write(record.spec_dir.join("answer.json"), "not json").unwrap();
    advance_ok(&state, &record.id, &host);
    let text = note(&host);
    assert!(
        text.contains("The `invalid` route from `watch` sent the run here."),
        "{text}"
    );
    assert!(!text.contains("answered `invalid`"));
    host.observed.borrow_mut().status = RunStatus::Failed;
    fs::write(record.spec_dir.join("answer.json"), r#"{"outcome":"red"}"#).unwrap();
    advance_ok(&state, &record.id, &host);
    assert!(note(&host).contains("The previous attempt, @builder in `build`, died."));
}

#[test]
fn missing_enrichment_never_prevents_a_launch_or_invents_a_sender() {
    let (_dir, state, mut record) = setup(OUTCOME_STEP);
    record.task = Some("missing-task".into());
    record.fire = Some(records::RunFire::Scheduled {
        task: "missing-task".into(),
        trigger: None,
    });
    update(&state, &record);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red"}));
    let live = records::load(&state.workflow(&record.id)).unwrap();
    let paths = state.workflow(&record.id);
    fs::remove_file(&paths.journal).unwrap();
    fs::create_dir(&paths.journal).unwrap();
    let spec: WorkflowSpec = toml::from_str(OUTCOME_STEP).unwrap();
    let text = crate::harness::workflow::brief::step(&paths, &spec, &live);
    assert!(
        text.contains("RimZ started this run automatically (loop task `missing-task`)."),
        "{text}"
    );
    assert!(!text.contains("answered") && !text.contains("run's first step"));
}

#[test]
fn rails_group_multiple_returns_and_never_assert_a_nonexistent_route() {
    for (text, outcomes, expected) in [
        (
            super::super::graph::LAND,
            &["pass", "red"][..],
            "begin → sweep → watch ⇄ [fix] → notes → triage → ruled",
        ),
        (
            super::super::graph::RALPH,
            &["build"][..],
            "evidence ⇄ [build], plan → judge → report → reported",
        ),
        (
            "[workflow]\nscope='worktree'\nstart='a'\n[checks.a]\nrun='cat answer.json'\non={left='b',right='c'}\n[steps.b]\nagent='claude'\nthen='d'\non.cap='c'\n[steps.c]\nagent='claude'\nthen='d'\n[checks.d]\nshell='true'\non={pass='@done',fail='@failed'}",
            &["left"][..],
            "a → [b] · c → d",
        ),
    ] {
        let condition = "cases=[{when=['pr=merged'],outcome='merged'},{when=['ci=failed'],outcome='red'}]\nelse='waiting'";
        let text = text
            .replace("agent='claude'", "agent='claude'\nprompt-file='prompt.md'")
            .replace("run='true'", "run='cat answer.json'")
            .replace(condition, "run='cat answer.json'");
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        for outcome in outcomes {
            answer(&state, &record, &host, json!({"outcome":outcome}));
        }
        let text = note(&host);
        assert!(text.contains(&format!("\n\n{expected}\n\n")), "{text}");
    }
}

#[test]
fn judge_note_is_short_and_has_no_step_or_answer_contract_paragraphs() {
    let (spec, _paths, record) = judge_fixture();
    let text = crate::harness::workflow::brief::judge(&spec, &record);
    assert_eq!(
        text,
        "You are a check in a RimZ workflow: a graph of steps and checks that the user defined. RimZ launches an agent for each agent step, and after each step runs a check whose answer picks the next node. You are the check `judge` of the workflow `test` (→ a route, ⇄ a step that returns to its check, [ ] you):\n\n[judge]\n\nThe user started this run by hand with `rimz workflow run`.\n\nYour answer decides where the run goes next.\n\n`rimz workflow show $RIMZ_WORKFLOW_RUN --history` lists every step and check so far."
    );
}
