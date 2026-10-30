use super::*;
use crate::store::workflow::{
    EvaluationOrigin, JournalEntry, JournalEvent, Key, RunState, StepVisits,
};
use serde_json::{Value, json};

fn spec() -> WorkflowSpec {
    toml::from_str(
        "[workflow]\nscope='worktree'\nstart='pr'\n\
         [checks.pr]\nrun='true'\non.pass='build'\non.fail='@wait'\n\
         [steps.build]\nrun='true'\nthen='pr'\nmax-visits=4\nmax-visits-per-key=2\n",
    )
    .unwrap()
}

fn entry(node: &str, event: Value) -> JournalEntry {
    let mut row = json!({"node":node, "seq":1, "at":"2026-01-01T00:00:01Z"});
    row.as_object_mut()
        .unwrap()
        .extend(event.as_object().unwrap().clone());
    serde_json::from_value(row).unwrap()
}

fn evaluation(outcome: &str) -> JournalEntry {
    entry(
        "pr",
        json!({
            "event":"evaluation", "outcome":outcome, "object":{"outcome":outcome},
            "digests":{}, "started_at":"2026-01-01T00:00:00Z",
            "ended_at":"2026-01-01T00:00:01Z"
        }),
    )
}

fn attempt(outcome: &str) -> JournalEntry {
    entry(
        "build",
        json!({
            "event":"attempt", "outcome":outcome, "visit":1, "key":null,
            "digests":{}, "started_at":"2026-01-01T00:00:00Z",
            "ended_at":"2026-01-01T00:00:01Z"
        }),
    )
}

fn route<'a>(
    outcome: &'a str,
    target: &'a str,
    count: u64,
) -> (HopKind, Option<&'a str>, Option<&'a str>, u64) {
    (HopKind::Route, Some(outcome), Some(target), count)
}

fn detail(hop: &Hop) -> (HopKind, Option<&str>, Option<&str>, u64) {
    (
        hop.kind,
        hop.outcome.as_deref(),
        hop.target.as_deref(),
        hop.count,
    )
}

#[test]
fn edge_lookup_includes_engine_defaults_and_attempt_outcomes() {
    let mut spec = spec();
    spec.checks.insert(
        "condition".into(),
        toml::from_str("cases=[{when=['pr.merged'],outcome='ready'}]\non.ready='@done'").unwrap(),
    );
    for (node, outcome, target) in [
        ("pr", "pass", "build"),
        ("pr", "timeout", "@user"),
        ("pr", "invalid", "@user"),
        ("pr", "hold", "@wait"),
        ("condition", "unknown", "@wait"),
        ("build", "ended", "pr"),
        ("build", "died", "pr"),
        ("build", "timeout", "pr"),
        ("build", "canceled", "pr"),
        ("build", "limited", "pr"),
        ("build", "asked", "@user"),
        ("build", "cap", "@user"),
        ("build", "capacity", "@failed"),
        ("pr", "deadline", "@user"),
        ("build", "deadline", "@user"),
    ] {
        assert_eq!(
            spec.target(node, outcome).as_deref(),
            Some(target),
            "{node}/{outcome}"
        );
    }
    assert_eq!(spec.target("missing", "pass"), None);
    assert_eq!(spec.target("pr", "missing"), None);
    assert_eq!(spec.target("build", "missing"), None);
}

#[test]
fn edge_lookup_keeps_explicit_overrides() {
    let mut spec = spec();
    spec.workflow.on.deadline = Some("@failed".into());
    for outcome in ["timeout", "invalid", "unknown"] {
        spec.checks
            .get_mut("pr")
            .unwrap()
            .on
            .insert(outcome.into(), "build".into());
        assert_eq!(spec.target("pr", outcome).as_deref(), Some("build"));
    }
    let step = spec.steps.get_mut("build").unwrap();
    step.on.asked = Some("pr".into());
    step.on.cap = Some("@done".into());
    step.on.capacity = Some("@user".into());
    for (outcome, target) in [
        ("asked", "pr"),
        ("cap", "@done"),
        ("capacity", "@user"),
        ("deadline", "@failed"),
    ] {
        assert_eq!(spec.target("build", outcome).as_deref(), Some(target));
    }
}

#[test]
fn visit_bounds_keep_total_first_typed_keys_and_grants() {
    let spec = spec();
    let visits: StepVisits = serde_json::from_value(json!({
        "total":4, "keys":[{"key":42,"visits":2},{"key":"42","visits":1}]
    }))
    .unwrap();
    let step = &spec.steps["build"];
    assert_eq!(
        step.visit_bounds(Some(&visits), Some(&Key::Integer(42)), 1)
            .collect::<Vec<_>>(),
        [("max-visits", 4, 5), ("max-visits-per-key", 2, 3)]
    );
    assert_eq!(
        step.visit_bounds(Some(&visits), Some(&Key::String("42".into())), 0)
            .collect::<Vec<_>>(),
        [("max-visits", 4, 4), ("max-visits-per-key", 1, 2)]
    );
    assert_eq!(
        step.visit_bounds(None, None, u64::MAX).collect::<Vec<_>>(),
        [
            ("max-visits", 0, u64::MAX),
            ("max-visits-per-key", 0, u64::MAX)
        ]
    );
    let mut unbounded = step.clone();
    unbounded.max_visits = None;
    unbounded.max_visits_per_key = None;
    assert_eq!(unbounded.visit_bounds(Some(&visits), None, 0).count(), 0);
}

#[test]
fn hops_fold_only_matching_consecutive_check_decisions() {
    let journal = [
        evaluation("pass"),
        attempt("ended"),
        evaluation("fail"),
        evaluation("fail"),
    ];
    let trail = hops(&spec(), &journal);
    assert_eq!(
        trail
            .iter()
            .map(|hop| (&*hop.node, detail(hop)))
            .collect::<Vec<_>>(),
        [
            ("pr", route("pass", "build", 1)),
            ("build", route("ended", "pr", 1)),
            ("pr", route("fail", "@wait", 2))
        ]
    );
    assert_eq!(trail[0].at, journal[0].at);
}

#[test]
fn hops_count_invalid_evaluation_and_route_once() {
    let journal = [
        evaluation("invalid"),
        entry(
            "pr",
            json!({"event":"route", "outcome":"invalid", "input":{}}),
        ),
    ];
    assert_eq!(
        hops(&spec(), &journal)
            .iter()
            .map(detail)
            .collect::<Vec<_>>(),
        [route("invalid", "@user", 1)]
    );
}

#[test]
fn hops_do_not_count_an_abandoned_check_as_a_routing_decision() {
    let mut abandoned = evaluation("timeout");
    if let JournalEvent::Evaluation { origin, object, .. } = &mut abandoned.event {
        *origin = EvaluationOrigin::Engine;
        *object = Value::Null;
    }
    let mut bound = entry(
        "pr",
        json!({"event":"route", "outcome":"timeout", "input":{}}),
    );
    bound.seq = None;
    assert_eq!(
        hops(&spec(), &[abandoned, bound])
            .iter()
            .map(detail)
            .collect::<Vec<_>>(),
        [route("timeout", "@user", 1)],
    );
}

#[test]
fn hops_do_not_route_a_judges_failed_evaluations_before_its_check_park() {
    use crate::store::workflow::ParkReason;
    let mut spec = spec();
    let check = spec.checks.get_mut("pr").unwrap();
    check.run = None;
    check.agent = Some("claude".into());
    check.prompt_file = Some("prompt.md".into());
    let mut failed = evaluation("invalid");
    if let JournalEvent::Evaluation { origin, .. } = &mut failed.event {
        *origin = EvaluationOrigin::Engine;
    }
    let mut retry = failed.clone();
    retry.seq = Some(2);
    let journal = [
        failed,
        retry,
        entry("pr", json!({"event":"park", "reason":"check"})),
    ];
    assert_eq!(
        hops(&spec, &journal)
            .iter()
            .map(|hop| &hop.kind)
            .collect::<Vec<_>>(),
        [&HopKind::Park {
            reason: ParkReason::Check
        }],
    );
}

#[test]
fn hops_keep_engine_caps_and_parks_separate() {
    use crate::store::workflow::ParkReason;
    let journal = [
        entry(
            "build",
            json!({"event":"route", "outcome":"cap", "input":{}}),
        ),
        entry("build", json!({"event":"park", "reason":"cap"})),
    ];
    assert_eq!(
        hops(&spec(), &journal)
            .iter()
            .map(detail)
            .collect::<Vec<_>>(),
        [
            route("cap", "@user", 1),
            (
                HopKind::Park {
                    reason: ParkReason::Cap
                },
                None,
                None,
                1
            )
        ]
    );
}

#[test]
fn hops_resume_names_the_reentered_node() {
    let journal = [entry(
        "pr",
        json!({"event":"resume", "visit_grant":1, "deadline":null}),
    )];
    let trail = hops(&spec(), &journal);
    assert_eq!(
        trail
            .iter()
            .map(|hop| (&*hop.node, &hop.kind))
            .collect::<Vec<_>>(),
        [("pr", &HopKind::Resume)]
    );
}

#[test]
fn hops_use_a_held_turn_verdict_instead_of_then() {
    use crate::harness::workflow::state::Transition;
    use crate::store::workflow::{
        AttemptKind, EndCause, Ending, HeldSession, TurnEnd, TurnEndAction,
    };

    let text = "[workflow]\nscope='worktree'\nstart='build'\n\
        [steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='until'\nthen='pr'\n\
        [checks.pr]\nrun='true'\non.pending='build'\non.done='@done'\n";
    let (_dir, state, record) = super::attempts::setup(text);
    let host = FakeHost::new();
    advance(&state, &record.id, &host).unwrap();
    let paths = state.workflow(&record.id);
    let live = crate::store::workflow::load(&paths).unwrap();
    let spec: WorkflowSpec = toml::from_str(text).unwrap();
    let mut actual = Vec::new();
    for action in [
        TurnEndAction::Kept,
        TurnEndAction::Closed,
        TurnEndAction::Discarded,
        TurnEndAction::Interrupted,
    ] {
        let mut t = Transition {
            record: live.clone(),
            spec: &spec,
            paths: &paths,
            now: live.entered_at,
            entries: Vec::new(),
        };
        for round in 0..2 {
            let mut session = t.record.attempt.take().unwrap();
            session.judged = Some(TurnEnd {
                agent_id: "leader".into(),
                started_at: t.now,
                ended_at: t.now,
            });
            let mut check = session.clone();
            check.kind = AttemptKind::Script;
            check.node = "pr".into();
            check.seq = t.record.next_seq;
            t.record.next_seq += 1;
            t.record.held = Some(HeldSession {
                attempt: session,
                entered_at: t.record.entered_at,
                verdict: None,
            });
            t.record.node = "pr".into();
            t.record.attempt = Some(check.clone());
            let outcome = if action == TurnEndAction::Kept {
                "pending"
            } else {
                "done"
            };
            t.finish_check(check.seq, Ok(json!({"outcome":outcome})), None)
                .unwrap();
            let verdict = t.record.held.as_ref().unwrap().verdict.clone().unwrap();
            t.restore_session();
            if action == TurnEndAction::Closed {
                t.stage_agent_end(
                    Ending {
                        outcome: AttemptOutcome::Ended,
                        cause: EndCause::Lifecycle,
                        route: Some(verdict),
                    },
                    &host.observed.borrow(),
                );
                t.settle_agent(Children::default(), None, None, None)
                    .unwrap();
            } else {
                let session = t.record.attempt.clone().unwrap();
                t.turn_end(&session, action, Some(&verdict), None);
            }
            if action != TurnEndAction::Kept || round == 1 {
                break;
            }
        }
        actual.push((
            action,
            hops(&spec, &t.entries)
                .into_iter()
                .filter(|hop| hop.kind == HopKind::Route)
                .map(|hop| {
                    (
                        hop.node,
                        hop.outcome.unwrap(),
                        hop.target.unwrap(),
                        hop.count,
                    )
                })
                .collect::<Vec<_>>(),
        ));
    }
    assert_eq!(
        actual,
        [
            (
                TurnEndAction::Kept,
                vec![("pr".into(), "pending".into(), "build".into(), 2)]
            ),
            (
                TurnEndAction::Closed,
                vec![("pr".into(), "done".into(), "@done".into(), 1)]
            ),
            (TurnEndAction::Discarded, vec![]),
            (TurnEndAction::Interrupted, vec![]),
        ],
    );
}

#[test]
fn hops_hold_routes_to_wait_not_the_case_target() {
    assert_eq!(
        hops(&spec(), &[evaluation("hold")])
            .iter()
            .map(detail)
            .collect::<Vec<_>>(),
        [route("hold", "@wait", 1)]
    );
}

#[test]
fn hops_preserve_terminal_rows_without_counting_cancel_twice() {
    let journal = [
        entry("pr", json!({"event":"cancel"})),
        entry("pr", json!({"event":"terminal", "state":"canceled"})),
    ];
    assert_eq!(
        hops(&spec(), &journal)
            .iter()
            .map(|hop| &hop.kind)
            .collect::<Vec<_>>(),
        [&HopKind::Terminal {
            state: RunState::Canceled
        }]
    );
}
