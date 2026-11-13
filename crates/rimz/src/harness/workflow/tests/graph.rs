use super::*;
use serde_json::{Value, json};

/// A `gate` check and a `work` step, each in the kind table its body's runner names; an agent
/// step that names no lifecycle is a turn.
fn spec(check: &str, step: &str) -> Definition {
    let has = |body: &str, key: &str| {
        body.lines()
            .any(|line| line.starts_with(&format!("{key}=")))
    };
    let check_kind = [
        ("agent", "judge"),
        ("sleep", "sleep"),
        ("cases", "condition"),
        ("shell", "test"),
    ]
    .into_iter()
    .find(|(key, _)| has(check, key))
    .map_or("script", |(_, kind)| kind);
    let step_kind = [("agent", "agent"), ("message", "message")]
        .into_iter()
        .find(|(key, _)| has(step, key))
        .map_or("command", |(_, kind)| kind);
    let lifecycle = if step_kind == "agent" && !has(step, "lifecycle") {
        "lifecycle='turn'\n"
    } else {
        ""
    };
    toml::from_str(&format!(
        "[workflow]\nscope='worktree'\nstart='gate'\n[{check_kind}.gate]\n{check}\n[{step_kind}.work]\n{lifecycle}{step}\n"
    ))
    .unwrap()
}

fn edge(spec: &Definition, node: &str, outcome: &str) -> (Value, Option<&'static str>) {
    let edge = spec
        .edges(node)
        .into_iter()
        .find(|edge| edge.outcome == outcome)
        .unwrap();
    (serde_json::to_value(&edge).unwrap(), edge.mark())
}

#[test]
fn failure_edges_name_the_chain_level_that_routes_them_in_every_kind() {
    for (kind, check, cause) in [
        (
            "script",
            "run='true'\non.ok='@done'\ntimeout='1m'",
            "invalid",
        ),
        (
            "test",
            "shell='false'\non.pass='@done'\non.fail='@failed'\ntimeout='1m'",
            "died",
        ),
        (
            "judge",
            "agent='claude'\nprompt-file='judge.md'\non.ok='@done'\ntimeout='1m'",
            "limited",
        ),
        (
            "condition",
            "cases=[{when=['pr=merged'],outcome='ok'}]\non.ok='@done'\ntimeout='1m'",
            "timeout",
        ),
        ("sleep", "sleep='1m'\nthen='@done'", "deadline"),
    ] {
        for (workflow, target, source) in [
            ("", "@failed", "default"),
            ("on.failed='@done'", "@done", "workflow.on.failed"),
        ] {
            let text = format!(
                "[workflow]\nscope='worktree'\nstart='gate'\n{workflow}\n[{kind}.gate]\n{check}\n[agent.work]\nlifecycle='turn'\nagent='claude'\nprompt-file='work.md'\nthen='@done'\nmax-visits=1\n"
            );
            let spec = super::super::spec::parse(&text).unwrap();
            for (node, outcome) in [
                ("gate", cause),
                ("gate", "deadline"),
                ("work", "died"),
                ("work", "visits"),
                ("work", "deadline"),
            ] {
                let (edge, _) = edge(&spec, node, outcome);
                assert_eq!(edge["target"], target, "{text}: {node}.{outcome}");
                assert_eq!(edge["source"], source);
                assert_eq!(
                    spec.target(node, outcome)
                        .as_ref()
                        .map(|target| target.as_str()),
                    Some(target)
                );
            }
            assert_eq!(spec.target("work", "asked"), None);
            let explicit =
                super::super::spec::parse(&(text + "on.failed='@failed'\non.visits='@done'\n"))
                    .unwrap();
            for (outcome, target, source) in [
                ("died", "@failed", "on.failed"),
                ("visits", "@done", "explicit"),
            ] {
                assert_eq!(
                    explicit
                        .target("work", outcome)
                        .as_ref()
                        .map(|target| target.as_str()),
                    Some(target)
                );
                assert_eq!(edge(&explicit, "work", outcome).0["source"], source);
            }
        }
    }
}

#[test]
fn a_sleep_routes_only_its_deadline_and_other_causes_refuse() {
    let spec = super::super::spec::parse("[workflow]\nscope='worktree'\nstart='nap'\n[sleep.nap]\nsleep='1m'\nthen='@done'\non.failed='@done'\n").unwrap();
    assert_eq!(
        spec.target("nap", "deadline")
            .as_ref()
            .map(|target| target.as_str()),
        Some("@done")
    );
    for cause in ["timeout", "invalid"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='nap'\n[sleep.nap]\nsleep='1m'\nthen='@done'\non.{cause}='@done'\n"
        );
        match super::super::spec::parse(&text) {
            Err(super::super::SpecErr::Toml { path, .. }) => {
                assert_eq!(path, format!("sleep.nap.on.{cause}"));
            }
            other => panic!("a sleep's on.{cause} refuses: {other:?}"),
        }
    }
}

#[test]
fn timeout_edges_name_the_missing_clock() {
    for (extra, fires, because, mark) in [
        ("", "never", Some("no-timeout"), Some("never: no timeout")),
        ("timeout='1h'", "yes", None, None),
    ] {
        let spec = spec(
            &format!("run='true'\non.ready='work'\n{extra}"),
            "shell='true'\nthen='gate'",
        );
        let (edge, actual_mark) = edge(&spec, "gate", "timeout");
        assert_eq!(edge["fires"], fires);
        assert_eq!(
            edge.get("because"),
            because.map(|text| json!(text)).as_ref()
        );
        assert_eq!(actual_mark, mark);
        assert_eq!(edge["target"], "@failed");
    }
}

#[test]
fn visits_edges_need_a_visit_bound() {
    for (bounds, fires, because) in [
        ("", "never", Some("no-max-visits")),
        ("max-visits=4", "yes", None),
    ] {
        let spec = spec(
            "run='true'\non.ready='work'",
            &format!("shell='true'\nthen='gate'\n{bounds}"),
        );
        let (edge, mark) = edge(&spec, "work", "visits");
        assert_eq!(edge["fires"], fires);
        assert_eq!(
            edge.get("because"),
            because.map(|text| json!(text)).as_ref()
        );
        assert_eq!(mark, because.map(|_| "never: no max-visits"));
    }
}

#[test]
fn capacity_edges_fire_on_every_agent_step() {
    // A start wider than a machine cap fails with capacity on any admitted run, so no
    // setting of the definition makes the edge unreachable.
    for (settings, limit) in [
        ("", None),
        ("admission.give-up='forever'", None),
        ("admission.give-up='5m'", None),
        ("admission.throttle='off'", None),
        (
            "admission.throttle='off'\nadmission.give-up='5m'",
            Some(false),
        ),
        (
            "admission.throttle='off'\nadmission.give-up='forever'",
            Some(true),
        ),
    ] {
        let mut spec = spec(
            "run='true'\non.ready='work'",
            &format!("agent='claude'\nthen='gate'\n{settings}"),
        );
        if let Some(workflow_limit) = limit {
            let limit = Some(toml::from_str("max-parallel=1").unwrap());
            spec.edit(|spec| {
                if workflow_limit {
                    spec.workflow.limit = limit;
                } else {
                    spec.steps.get_mut("work").unwrap().limit = limit;
                }
            });
        }
        let (edge, mark) = edge(&spec, "work", "capacity");
        assert_eq!(edge["fires"], "yes", "{settings}");
        assert_eq!(edge.get("because"), None, "{settings}");
        assert_eq!(mark, None, "{settings}");
        assert_eq!(edge["target"], "@failed");
    }
    let spec = spec("run='true'", "shell='true'\nthen='gate'");
    assert!(
        !spec
            .edges("work")
            .iter()
            .any(|edge| edge.outcome == "capacity")
    );
}

#[test]
fn judge_invalid_fires_after_retry_and_defaults_to_the_run_end() {
    for (settings, target) in [("", "@failed"), ("on.invalid='work'", "work")] {
        let spec = spec(
            &format!("agent='claude'\non.ready='work'\n{settings}"),
            "agent='claude'\nthen='gate'",
        );
        let (resolved, mark) = edge(&spec, "gate", "invalid");
        assert_eq!(resolved["fires"], "yes");
        assert_eq!(resolved["target"], target);
        assert_eq!(mark, None);
    }
}

#[test]
fn deadlines_require_a_sealed_or_granted_deadline_not_just_a_route() {
    for (deadline, target, fires) in [
        (None, None, "with-deadline"),
        (None, Some("work"), "with-deadline"),
        (Some("2030-01-01"), None, "yes"),
        (Some("2030-01-01"), Some("work"), "yes"),
    ] {
        let mut spec = spec("run='true'\non.ready='work'", "agent='claude'\nthen='gate'");
        spec.edit(|spec| {
            spec.workflow.deadline = deadline.map(str::to_owned);
            spec.workflow.on.failed = target.map(str::to_owned);
        });
        for node in ["gate", "work"] {
            let (edge, mark) = edge(&spec, node, "deadline");
            assert_eq!(edge["fires"], fires);
            assert_eq!(edge["target"], target.unwrap_or("@failed"));
            assert_eq!(
                edge["source"],
                if target.is_some() {
                    "workflow.on.failed"
                } else {
                    "default"
                }
            );
            assert!(edge.get("because").is_none());
            assert_eq!(mark, deadline.is_none().then_some("only with a deadline"));
        }
    }
}

#[test]
fn edge_order_follows_cases_then_else_then_explicit_then_engine_triggers() {
    let spec = spec(
        "cases=[{when=['pr=merged'],outcome='zeta'},{when=['ci=passed'],outcome='alpha'},{when=['ci=failed'],outcome='zeta'}]\nelse='pending'\non={alpha='@done',zeta='work',pending='@wait',unknown='@wait'}\ntimeout='1d'",
        "agent='claude'\nlifecycle='turn'\nthen='gate'\non={asked='gate',visits='@failed',capacity='@failed'}",
    );
    let outcomes = |node| {
        spec.edges(node)
            .into_iter()
            .map(|edge| edge.outcome)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        outcomes("gate"),
        ["zeta", "alpha", "pending", "unknown", "timeout", "deadline"]
    );
    assert_eq!(
        outcomes("work"),
        [
            "then", "asked", "died", "timeout", "limited", "visits", "capacity", "deadline"
        ]
    );
    let spec = self::spec(
        "run='true'\non={zeta='work',alpha='@done'}",
        "shell='true'\nthen='gate'",
    );
    assert_eq!(
        spec.edges("gate")
            .into_iter()
            .map(|edge| edge.outcome)
            .collect::<Vec<_>>(),
        ["alpha", "zeta", "died", "invalid", "timeout", "deadline"]
    );
}

pub(super) const LAND: &str = r#"
[workflow]
scope='worktree'
start='begin'
[test.begin]
shell='true'
on={pass='sweep',fail='@failed'}
[condition.watch]
cases=[{when=['pr=merged'],outcome='merged'},{when=['ci=failed'],outcome='red'}]
else='waiting'
on={merged='notes',red='fix',waiting='@wait',unknown='@wait'}
timeout='3d'
[script.notes]
run='true'
on={rule='triage',none='@done'}
[script.ruled]
run='true'
on={rule='triage',none='@done'}
[agent.sweep]
lifecycle='turn'
agent='claude'
then='watch'
[agent.fix]
lifecycle='turn'
agent='claude'
then='watch'
max-visits=4
[agent.triage]
lifecycle='turn'
agent='claude'
then='ruled'
max-visits=2
"#;

pub(super) const RALPH: &str = r#"
[workflow]
scope='project'
start='evidence'
on.failed='report'
[script.evidence]
run='true'
on={plan='plan',build='build',review='judge',report='report',wait='@wait',broken='@failed'}
timeout='1d'
[judge.judge]
agent='claude'
on={build='build',plan='plan',report='report',halt='@failed'}
[script.reported]
run='true'
on={complete='@done',halted='@failed',missing='report'}
[agent.plan]
lifecycle='turn'
agent='claude'
then='evidence'
max-visits=20
on.visits='report'
[agent.build]
lifecycle='turn'
agent='claude'
then='evidence'
max-visits=80
on.visits='report'
[agent.report]
lifecycle='turn'
agent='claude'
then='reported'
max-visits=2
"#;

#[test]
fn walks_match_land_ralph_and_fix_tests_without_hanging_forward_returns() {
    for (text, expected) in [
        (
            LAND,
            vec![
                ("begin", None),
                ("sweep", None),
                ("watch", None),
                ("fix", Some("watch")),
                ("notes", None),
                ("triage", None),
                ("ruled", None),
            ],
        ),
        (
            RALPH,
            vec![
                ("evidence", None),
                ("build", Some("evidence")),
                ("plan", Some("evidence")),
                ("judge", None),
                ("report", None),
                ("reported", None),
            ],
        ),
        (
            "[workflow]\nscope='worktree'\nstart='tests'\n[script.tests]\nrun='true'\non={pass='@done',fail='fix'}\n[agent.fix]\nlifecycle='turn'\nagent='claude'\nthen='tests'\nmax-visits=3",
            vec![("tests", None), ("fix", Some("tests"))],
        ),
    ] {
        let spec: Definition = toml::from_str(text).unwrap();
        let walk = spec.walk();
        assert_eq!(
            walk.nodes
                .iter()
                .map(|node| (node.node.as_str(), node.hangs_under.as_deref()))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(walk.unreachable.is_empty(), "{walk:?}");
    }
}

#[test]
fn walk_reaches_header_deadline_but_not_impossible_edges_and_sorts_unreachable() {
    let spec: Definition = toml::from_str(
        r#"
[workflow]
scope='worktree'
start='start'
on.failed='rescue'
[script.start]
run='true'
on={ready='work',timeout='sleeping'}
[script.rescue]
run='true'
on.done='@done'
[script.sleeping]
run='true'
[script.capped]
run='true'
[script.orphan]
run='true'
[agent.work]
lifecycle='turn'
agent='claude'
then='start'
on.visits='capped'
"#,
    )
    .unwrap();
    let walk = spec.walk();
    assert_eq!(
        walk.nodes
            .iter()
            .map(|node| node.node.as_str())
            .collect::<Vec<_>>(),
        ["start", "work", "rescue"]
    );
    assert_eq!(walk.unreachable, ["capped", "orphan", "sleeping"]);
}

#[test]
fn walk_keeps_a_step_whose_then_names_another_step_and_validation_reports_it() {
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='c'\n\
         [test.c]\nshell='true'\non.pass='a'\n\
         [command.a]\nshell='true'\nthen='c'\nmax-visits=1\non.visits='b'\n\
         [command.b]\nshell='true'\nthen='a'\n",
    )
    .unwrap();
    let walk = spec.walk();
    assert_eq!(
        walk.nodes
            .iter()
            .map(|node| (node.node.as_str(), node.hangs_under.as_deref()))
            .collect::<Vec<_>>(),
        [("c", None), ("a", Some("c")), ("b", None)]
    );
    assert!(walk.unreachable.is_empty(), "{walk:?}");
    let report = super::super::validate::validate_spec(
        Path::new("/workflows/review"),
        "review",
        &spec,
        &LoadedDefinitions::default(),
        None,
    );
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.path == "command.b.then"
                && error
                    .message
                    .ends_with("then must name a check, never a step or @done")),
        "{report:?}"
    );
}

#[test]
fn a_deadline_only_step_does_not_hang_under_its_return_check() {
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='gate'\non.failed='rescue'\n\
         [sleep.gate]\nsleep='1m'\nthen='@done'\n\
         [command.rescue]\nshell='true'\nthen='gate'\n",
    )
    .unwrap();
    let walk = spec.walk();
    assert_eq!(
        walk.nodes
            .iter()
            .map(|node| (node.node.as_str(), node.hangs_under.as_deref()))
            .collect::<Vec<_>>(),
        [("gate", None), ("rescue", None)]
    );
    assert!(walk.unreachable.is_empty(), "{walk:?}");
}

#[test]
fn walk_positions_distinguish_downward_routes_from_returns_and_cross_routes() {
    for (text, routes) in [
        (
            LAND,
            vec![
                ("begin", "sweep", true),
                ("sweep", "watch", true),
                ("watch", "fix", true),
                ("fix", "watch", false),
                ("watch", "notes", true),
                ("notes", "triage", true),
                ("triage", "ruled", true),
                ("ruled", "triage", false),
            ],
        ),
        (
            RALPH,
            vec![
                ("evidence", "build", true),
                ("build", "evidence", false),
                ("evidence", "judge", true),
                ("judge", "build", false),
                ("judge", "report", true),
                ("report", "reported", true),
                ("reported", "report", false),
            ],
        ),
    ] {
        let spec: Definition = toml::from_str(text).unwrap();
        let walk = spec.walk();
        let positions = walk
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.node.as_str(), index))
            .collect::<std::collections::BTreeMap<_, _>>();
        for (from, to, down) in routes {
            assert!(spec.edges(from).iter().any(|edge| edge.target == to));
            assert!(
                positions.contains_key(from) && positions.contains_key(to),
                "walk must include {from} and {to}: {walk:?}"
            );
            assert_eq!(
                positions[from] < positions[to],
                down,
                "{from} -> {to}: {walk:?}"
            );
        }
    }
}

#[test]
fn validation_rows_follow_the_walk_then_alphabetical_unreachable_nodes() {
    let spec: Definition = toml::from_str("[workflow]\nscope='worktree'\nstart='z'\n[test.z]\nshell='true'\non={pass='work',fail='@done',died='@failed'}\n[test.a]\nshell='true'\non={pass='@done',fail='@failed',died='@failed'}\n[command.work]\nshell='true'\ntimeout='1m'\nthen='z'\nmax-visits=3\non={visits='@failed'}").unwrap();
    let report = super::super::validate::validate_spec(
        std::path::Path::new("."),
        "example",
        &spec,
        &LoadedDefinitions::default(),
        None,
    );
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(
        report
            .rows
            .iter()
            .map(|row| row.node.as_str())
            .collect::<Vec<_>>(),
        ["z", "work", "a"]
    );
}
