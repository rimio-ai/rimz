//! The failure chain: every failure, whatever raised it, takes the node's `on.<cause>`, else
//! its `on.failed`, else the workflow's `on.failed`, else ends the run failed, and hands the
//! next node the failure input with the run's key.

use super::attempts::{setup, update};
use super::*;
use crate::store::workflow::{self as records, AttemptOutcome, Key, RunRecord, RunState};
use jiff::Timestamp;
use serde_json::json;

/// Three sleeps a failure route can land on, so a route is read from the node it reached.
const LANDINGS: &str = "[sleep.by_cause]\nsleep='1h'\nthen='@done'\n\
    [sleep.by_node]\nsleep='1h'\nthen='@done'\n\
    [sleep.by_workflow]\nsleep='1h'\nthen='@done'\n\
    [sleep.by_then]\nsleep='1h'\nthen='@done'\n";

fn advanced(
    state: &crate::disk::paths::StatePaths,
    id: &WorkflowRunId,
    host: &FakeHost,
) -> RunRecord {
    let result = advance(state, id, host);
    assert!(result.is_ok(), "{result:?}");
    records::load(&state.workflow(id)).unwrap()
}

/// The definition with `node` routing `cause` at the last `levels` levels of the chain: the
/// workflow's `on.failed` from one, the node's `on.failed` from two, its `on.<cause>` at three.
fn chained(head: &str, node: &str, cause: &str, levels: usize) -> String {
    let mut text = head.to_owned();
    if levels >= 1 {
        text = text.replacen("[workflow]\n", "[workflow]\non.failed='by_workflow'\n", 1);
    }
    let mut on = Vec::new();
    if levels >= 3 {
        on.push(format!("{cause}='by_cause'"));
    }
    if levels >= 2 {
        on.push("failed='by_node'".to_owned());
    }
    if !on.is_empty() {
        let table = text.find(&format!(".{node}]\n")).unwrap() + node.len() + 3;
        text.insert_str(table, &format!("on={{{}}}\n", on.join(",")));
    }
    format!("{text}{LANDINGS}")
}

/// Where each depth of the chain sends the failure: three levels, then the run's end.
fn assert_chain(record: &RunRecord, levels: usize) {
    match levels {
        3 => assert_eq!(record.node, "by_cause"),
        2 => assert_eq!(record.node, "by_node"),
        1 => assert_eq!(record.node, "by_workflow"),
        _ => assert_eq!(record.state, RunState::Failed, "{record:?}"),
    }
}

fn failure_input(record: &RunRecord, cause: &str, node: &str) -> serde_json::Value {
    let input = record
        .input
        .clone()
        .expect("a failure route hands on its input");
    assert_eq!(input["outcome"], "failed", "{input}");
    assert_eq!(input["cause"], cause, "{input}");
    assert_eq!(input["node"], node, "{input}");
    assert!(input["reason"].is_string(), "{input}");
    assert!(input["detail"].is_object(), "{input}");
    assert_eq!(input["key"], "the-key", "{input}");
    assert_eq!(record.key, Some(Key::String("the-key".into())));
    input
}

fn keyed(text: &str) -> (tempfile::TempDir, crate::disk::paths::StatePaths, RunRecord) {
    let (dir, state, mut record) = setup(text);
    record.key = Some(Key::String("the-key".into()));
    record.input = Some(json!({"outcome":"red", "key":"the-key"}));
    update(&state, &record);
    (dir, state, record)
}

const AGENT: &str = "[workflow]\nscope='worktree'\nstart='build'\n\
    [agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='by_then'\n";

#[test]
fn a_died_agent_takes_the_nearest_route_of_the_chain_and_never_then() {
    // A session's supervised run settles failed when its provider exits away from rest.
    for (lifecycle, levels) in ["turn", "session"]
        .into_iter()
        .flat_map(|lifecycle| [3, 2, 1, 0].map(|levels| (lifecycle, levels)))
    {
        let agent = AGENT.replace("lifecycle='turn'", &format!("lifecycle='{lifecycle}'"));
        let text = chained(&agent, "build", "died", levels);
        let (_dir, state, record) = keyed(&text);
        let host = FakeHost::new();
        advanced(&state, &record.id, &host);
        host.observed.borrow_mut().status = crate::store::run::RunStatus::Failed;
        let ended = advanced(&state, &record.id, &host);
        assert_eq!(
            ended.prev_attempt.as_ref().unwrap().outcome,
            AttemptOutcome::Died
        );
        assert_ne!(ended.node, "by_then", "a death never follows then");
        assert_chain(&ended, levels);
        if levels > 0 {
            let input = failure_input(&ended, "died", "build");
            assert_eq!(input["detail"], json!({}), "{input}");
        }
    }
}

#[test]
fn a_launch_failure_is_a_died_attempt_on_the_chain() {
    for levels in [3, 0] {
        let (_dir, state, record) = keyed(&chained(AGENT, "build", "died", levels));
        let host = FakeHost::new();
        *host.failure.borrow_mut() = Some(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason: "launch failed: missing account".into(),
        });
        let ended = advanced(&state, &record.id, &host);
        assert_chain(&ended, levels);
        if levels > 0 {
            let input = failure_input(&ended, "died", "build");
            assert_eq!(input["reason"], "launch failed: missing account");
        }
    }
}

#[test]
fn a_spent_visit_cap_fails_the_node_without_counting_a_visit() {
    let head = AGENT.replace("then='by_then'", "then='by_then'\nmax-visits=1");
    for levels in [3, 2, 1, 0] {
        let text = chained(&head, "build", "visits", levels);
        let (_dir, state, mut record) = keyed(&text);
        record.visits.insert("build".into(), 1);
        update(&state, &record);
        let host = FakeHost::new();
        let ended = advanced(&state, &record.id, &host);
        assert!(host.launched.borrow().is_empty());
        assert_eq!(ended.visits["build"], 1, "a visits failure counts no visit");
        assert_chain(&ended, levels);
        if levels > 0 {
            let input = failure_input(&ended, "visits", "build");
            assert_eq!(input["detail"], json!({"visits":1, "max":1}));
        }
    }
}

#[test]
fn the_deadline_fails_the_node_the_run_is_at_once() {
    let sleep = "[workflow]\nscope='worktree'\nstart='cooldown'\n[sleep.cooldown]\nsleep='1h'\nthen='by_then'\n";
    for levels in [3, 0] {
        let (_dir, state, record) = keyed(&chained(sleep, "cooldown", "deadline", levels));
        let mut waiting = advanced(&state, &record.id, &FakeHost::new());
        waiting.deadline = Some(Timestamp::now() - std::time::Duration::from_secs(1));
        update(&state, &waiting);
        let ended = advanced(&state, &record.id, &FakeHost::new());
        assert!(ended.deadline_fired);
        assert_chain(&ended, levels);
        if levels > 0 {
            let input = failure_input(&ended, "deadline", "cooldown");
            assert!(input["detail"]["deadline"].is_string(), "{input}");
            // The landing sleep runs past the spent deadline without failing again.
            let still = advanced(&state, &record.id, &FakeHost::new());
            assert_eq!(still.node, "by_cause");
            assert_eq!(still.state, RunState::Active);
        }
    }
}

#[test]
fn a_command_that_exits_nonzero_dies_rather_than_following_then() {
    let text = "[workflow]\nscope='worktree'\nstart='build'\n\
        [command.build]\nrun='false'\nthen='by_then'\n";
    for levels in [3, 0] {
        let (_dir, state, record) = keyed(&chained(text, "build", "died", levels));
        let host = FakeHost::new();
        let mut ended = advanced(&state, &record.id, &host);
        for _ in 0..50 {
            if ended.node != "build" || ended.state.is_terminal() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            ended = advanced(&state, &record.id, &host);
        }
        assert_chain(&ended, levels);
        if levels > 0 {
            let input = failure_input(&ended, "died", "build");
            assert_eq!(input["detail"]["code"], 1, "{input}");
            assert!(input["detail"]["log"].is_string(), "{input}");
        }
    }
}

#[test]
fn a_definition_without_failure_routes_validates() {
    let (_home, dir) = super::fixture();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\nscope='worktree'\nstart='build'\n\
         [agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompts/build.md'\nthen='check'\ntimeout='1h'\nmax-visits=3\n\
         [test.check]\nrun='true'\ntimeout='5m'\non={pass='@done',fail='build'}\n",
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn unknown_failure_fields_and_reserved_outcomes_refuse() {
    let head = "[workflow]\nscope='worktree'\nstart='build'\n";
    let agent = "[test.gate]\nrun='true'\ntimeout='5m'\non={pass='@done',fail='@failed'}\n\
                 [agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompts/build.md'\nthen='gate'\n";
    let cases = [
        (
            format!("{}on-break='@failed'\n{agent}", head),
            "workflow.on-break",
        ),
        (
            format!("{}on.deadline='@failed'\n{agent}", head),
            "workflow.on.deadline",
        ),
        (
            format!("{head}{agent}max-visits=2\non.cap='@failed'\n"),
            "agent.build.on.cap",
        ),
        (
            format!("{head}{agent}on.died='@break'\n"),
            "agent.build.on.died",
        ),
        (
            format!("{}on.failed='@wait'\n{agent}", head),
            "workflow.on.failed",
        ),
        (
            format!("{head}{agent}on.died='@wait'\n"),
            "agent.build.on.died",
        ),
        (
            format!("{head}{agent}[script.pick]\nrun='checks/pick.py'\non={{unknown='@done'}}\n")
                .replace("then='gate'", "then='pick'"),
            "script.pick.on.unknown",
        ),
        (
            format!("{head}{agent}[script.pick]\nrun='checks/pick.py'\non={{limited='@done'}}\n")
                .replace("then='gate'", "then='pick'"),
            "script.pick.on.limited",
        ),
    ];
    for (text, path) in cases {
        let (_home, dir) = super::fixture();
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        let paths: Vec<_> = report
            .errors
            .iter()
            .map(|issue| issue.path.as_str())
            .collect();
        assert_eq!(paths, [path], "{text}\n{:?}", report.errors);
        if ["on-break", "on.deadline", "on.cap"]
            .iter()
            .any(|field| path.ends_with(field))
        {
            let field = path.rsplit('.').next().unwrap();
            assert!(
                report.errors[0]
                    .message
                    .contains(&format!("unknown field `{field}`")),
                "{text}\n{:?}",
                report.errors
            );
        }
    }
}

#[test]
fn a_visits_or_capacity_route_that_reaches_its_node_again_refuses() {
    let head = "[workflow]\nscope='worktree'\nstart='build'\n";
    let build = "[test.gate]\nrun='true'\ntimeout='5m'\non={pass='@done',fail='@failed'}\n\
                 [agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompts/build.md'\nthen='gate'\nmax-visits=2\n";
    let cases = [
        (
            format!("{head}{build}on.visits='build'\n"),
            Some("agent.build.on.visits"),
        ),
        (
            format!("{head}{build}on.failed='fix'\n[sleep.fix]\nsleep='1m'\nthen='build'\n"),
            Some("agent.build.on.failed"),
        ),
        (
            format!("{head}on.failed='fix'\n{build}[sleep.fix]\nsleep='1m'\nthen='build'\n"),
            Some("workflow.on.failed"),
        ),
        (
            format!(
                "{head}on.failed='fix'\n{build}on.visits='@failed'\non.capacity='@done'\n[sleep.fix]\nsleep='1m'\nthen='build'\n"
            ),
            None,
        ),
    ];
    for (text, path) in cases {
        let (_home, dir) = super::fixture();
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        let paths: Vec<_> = report
            .errors
            .iter()
            .map(|issue| issue.path.as_str())
            .collect();
        assert_eq!(
            paths,
            path.into_iter().collect::<Vec<_>>(),
            "{text}\n{:?}",
            report.errors
        );
    }
}

#[test]
fn every_kind_table_names_exactly_the_causes_its_kind_raises() {
    let schema = serde_json::to_value(schema()).unwrap();
    for (kind, extra) in [
        ("AgentEdges", &["asked"][..]),
        ("CommandEdges", &[][..]),
        ("MessageEdges", &[][..]),
        ("TestEdges", &["pass", "fail"][..]),
        ("SleepEdges", &[][..]),
    ] {
        let mut keys: Vec<_> = schema["$defs"][kind]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{kind}"))
            .keys()
            .cloned()
            .collect();
        keys.sort();
        let table = kind.trim_end_matches("Edges").to_lowercase();
        let mut expected: Vec<String> = spec::Cause::raised_by(&table)
            .iter()
            .map(|cause| cause.word().to_owned())
            .chain(extra.iter().map(|word| (*word).to_owned()))
            .chain(["failed".to_owned()])
            .collect();
        expected.sort();
        assert_eq!(keys, expected, "{kind}");
    }
}

#[test]
fn resume_at_moves_an_active_run() {
    let text = "[workflow]\nscope='worktree'\nstart='cooldown'\n\
        [sleep.cooldown]\nsleep='1h'\nthen='@done'\n[sleep.later]\nsleep='1h'\nthen='@done'\n";
    let (_dir, state, record) = setup(text);
    advanced(&state, &record.id, &FakeHost::new());
    let moved = resume(&state, &record.id, Some("later"), &[]).unwrap();
    assert_eq!(moved.node, "later");
    assert_eq!(moved.state, RunState::Active);
}

#[test]
fn a_handled_limited_then_a_death_reports_the_run_failed_not_budget() {
    let text = "[workflow]\nscope='worktree'\nstart='gate'\n\
        [judge.gate]\nagent='codex'\nprompt-file='prompt.md'\non={ok='@done',limited='report'}\n\
        [script.report]\nshell='exit 1'\non.ok='@done'\n";
    let (_dir, state, record) = setup(text);
    let routed: Definition = toml::from_str(text).unwrap();
    let unrouted: Definition = toml::from_str(&text.replace(",limited='report'", "")).unwrap();
    let paths = state.workflow(&record.id);
    let attempt = |kind: &str, seq: u64, node: &str| {
        serde_json::from_value(json!({"kind":kind, "seq":seq, "node":node,
            "started_at":record.entered_at, "host":{"pid":123, "start":null}}))
        .unwrap()
    };
    let mut handled = Vec::new();
    for route in [false, true] {
        let mut t = state::Transition {
            now: record.entered_at,
            record: record.clone(),
            spec: if route { &routed } else { &unrouted },
            paths: &paths,
            entries: Vec::new(),
        };
        t.record.attempt = Some(attempt("judge", 1, "gate"));
        t.record.next_seq = 2;
        let refusal = script::Evaluation {
            object: Err("account codex:default budget $10.00/day is spent".into()),
            failure: "budget",
            stderr_tail: None,
            limited: None,
            cost_usd: None,
            detail: json!({}),
        };
        t.finish_judge(1, refusal).unwrap();
        if route {
            assert_eq!(t.record.node, "report");
            t.record.attempt = Some(attempt("script", 2, "report"));
            t.check_failure(
                2,
                Some(spec::Cause::Died),
                "exited 1".into(),
                json!({}),
                None,
            )
            .unwrap();
        }
        assert_eq!(t.record.state, RunState::Failed, "{:?}", t.entries);
        let outcome = engine::outcome(&t.record, &t.entries, false);
        handled.push((route, outcome.ending, outcome.reason));
    }
    assert_eq!(
        handled,
        [
            (
                false,
                records::RunEnding::Budget { turned: false },
                Some("account codex:default budget $10.00/day is spent".into())
            ),
            (true, records::RunEnding::Failed, Some("exited 1".into())),
        ]
    );
}

#[test]
fn a_start_that_can_never_fit_fails_the_run_where_an_expired_wait_gave_up() {
    let text = "[workflow]\nscope='worktree'\nstart='build'\n\
        [agent.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='turn'\nthen='@done'\n";
    let (_dir, state, record) = setup(text);
    let spec: Definition = toml::from_str(text).unwrap();
    let paths = state.workflow(&record.id);
    let first = record.entered_at;
    let mut endings = Vec::new();
    // A loop fire's give-up instant can fall at the hold's first instant, so an expired wait
    // can record 0 as well.
    for (reason, detail) in [
        (
            "this start opens 2 agents",
            json!({"waited_ms":0, "first":first, "never_fits":true}),
        ),
        (
            "the capacity wait gave up",
            json!({"waited_ms":0, "first":first}),
        ),
    ] {
        let mut t = state::Transition {
            now: first,
            record: record.clone(),
            spec: &spec,
            paths: &paths,
            entries: Vec::new(),
        };
        t.fail(None, "build", spec::Cause::Capacity, reason.into(), detail)
            .unwrap();
        assert_eq!(t.record.state, RunState::Failed, "{:?}", t.entries);
        let outcome = engine::outcome(&t.record, &t.entries, false);
        endings.push((outcome.ending, outcome.reason));
    }
    assert_eq!(
        endings,
        [
            (
                records::RunEnding::Failed,
                Some("this start opens 2 agents".into())
            ),
            (
                records::RunEnding::CapacityGaveUp {
                    waited_ms: 0,
                    reason: Some("the capacity wait gave up".into()),
                },
                None
            ),
        ]
    );
}

#[test]
fn a_handled_limited_then_a_spent_visit_cap_reports_the_run_failed_not_budget() {
    let text = "[workflow]\nscope='worktree'\nstart='gate'\n\
        [judge.gate]\nagent='codex'\nprompt-file='prompt.md'\non={ok='@done',limited='fix'}\n\
        [agent.fix]\nagent='codex'\nprompt-file='prompt.md'\nlifecycle='turn'\nmax-visits=1\n\
        then='@done'\n";
    let (_dir, state, record) = setup(text);
    let spec: Definition = toml::from_str(text).unwrap();
    let paths = state.workflow(&record.id);
    let mut t = state::Transition {
        now: record.entered_at,
        record: record.clone(),
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.record.attempt = Some(
        serde_json::from_value(json!({"kind":"judge", "seq":1, "node":"gate",
            "started_at":record.entered_at, "host":{"pid":123, "start":null}}))
        .unwrap(),
    );
    t.record.next_seq = 2;
    let refusal = script::Evaluation {
        object: Err("account codex:default budget $10.00/day is spent".into()),
        failure: "budget",
        stderr_tail: None,
        limited: None,
        cost_usd: None,
        detail: json!({}),
    };
    t.finish_judge(1, refusal).unwrap();
    assert_eq!(t.record.node, "fix");
    // The step's cap raises `visits` before a launch: a newer route, and no newer evaluation.
    let route = t.visits_failure(1, 1);
    t.route(&Target::parse(&route.target), route.object, route.reason)
        .unwrap();
    assert_eq!(t.record.state, RunState::Failed, "{:?}", t.entries);
    let outcome = engine::outcome(&t.record, &t.entries, false);
    assert_eq!(
        (outcome.ending, outcome.reason),
        (
            records::RunEnding::Failed,
            Some("fix has used 1 of 1 visits".into())
        )
    );
}
