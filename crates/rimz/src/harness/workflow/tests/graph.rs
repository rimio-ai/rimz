use super::*;
use serde_json::{Value, json};

fn spec(check: &str, step: &str) -> WorkflowSpec {
    toml::from_str(&format!(
        "[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\n{check}\n[steps.work]\n{step}\n"
    ))
    .unwrap()
}

fn edge(spec: &WorkflowSpec, node: &str, outcome: &str) -> (Value, Option<&'static str>) {
    let edge = spec
        .edges(node)
        .into_iter()
        .find(|edge| edge.outcome == outcome)
        .unwrap();
    (serde_json::to_value(&edge).unwrap(), edge.mark())
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
        assert_eq!(edge["target"], "@user");
    }
}

#[test]
fn cap_edges_need_either_visit_bound() {
    for (bounds, fires, because) in [
        ("", "never", Some("no-max-visits")),
        ("max-visits=4", "yes", None),
        ("max-visits-per-key=2", "yes", None),
        ("max-visits=4\nmax-visits-per-key=2", "yes", None),
    ] {
        let spec = spec(
            "run='true'\non.ready='work'",
            &format!("shell='true'\nthen='gate'\n{bounds}"),
        );
        let (edge, mark) = edge(&spec, "work", "cap");
        assert_eq!(edge["fires"], fires);
        assert_eq!(
            edge.get("because"),
            because.map(|text| json!(text)).as_ref()
        );
        assert_eq!(mark, because.map(|_| "never: no max-visits"));
    }
}

#[test]
fn capacity_edges_name_the_loop_and_definition_preconditions() {
    for (settings, fires, because, mark) in [
        (
            "",
            "never",
            Some("capacity-wait-forever"),
            "never: capacity-wait is forever",
        ),
        (
            "capacity-wait='forever'",
            "never",
            Some("capacity-wait-forever"),
            "never: capacity-wait is forever",
        ),
        (
            "capacity-wait='5m'",
            "in-loop-runs",
            None,
            "only when a loop task starts the run",
        ),
        (
            "throttle='off'",
            "never",
            Some("throttle-off"),
            "never: throttle is off",
        ),
        (
            "throttle='off'\ncapacity-wait='5m'",
            "never",
            Some("throttle-off"),
            "never: throttle is off",
        ),
    ] {
        let spec = spec(
            "run='true'\non.ready='work'",
            &format!("agent='claude'\nthen='gate'\n{settings}"),
        );
        let (edge, actual_mark) = edge(&spec, "work", "capacity");
        assert_eq!(edge["fires"], fires);
        assert_eq!(
            edge.get("because"),
            because.map(|text| json!(text)).as_ref()
        );
        assert_eq!(actual_mark, Some(mark));
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
fn judge_invalid_never_fires_even_with_an_override() {
    for (settings, target, source) in [
        ("", "@user", "default"),
        ("on.invalid='work'", "work", "explicit"),
    ] {
        let spec = spec(
            &format!("agent='claude'\non.ready='work'\n{settings}"),
            "agent='claude'\nthen='gate'",
        );
        let (edge, mark) = edge(&spec, "gate", "invalid");
        assert_eq!(
            edge,
            json!({
                "outcome":"invalid", "target":target, "source":source,
                "fires":"never", "because":"judge-retries"
            })
        );
        assert_eq!(
            mark,
            Some("never: a judge retries once, then parks at @user")
        );
        assert_eq!(spec.target("gate", "invalid").as_deref(), Some(target));
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
        spec.workflow.deadline = deadline.map(str::to_owned);
        spec.workflow.on.deadline = target.map(str::to_owned);
        for node in ["gate", "work"] {
            let (edge, mark) = edge(&spec, node, "deadline");
            assert_eq!(edge["fires"], fires);
            assert_eq!(edge["target"], target.unwrap_or("@user"));
            assert_eq!(
                edge["source"],
                if target.is_some() {
                    "explicit"
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
        "cases=[{when=['pr=merged'],outcome='zeta'},{when=['ci=passed'],outcome='alpha'},{when=['ci=failed'],outcome='zeta'}]\nelse='pending'\non={alpha='@done',zeta='work',pending='@wait'}\ntimeout='1d'",
        "agent='claude'\nthen='gate'\non={asked='gate',cap='@user',capacity='@failed'}",
    );
    let outcomes = |node| {
        spec.edges(node)
            .into_iter()
            .map(|edge| edge.outcome)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        outcomes("gate"),
        [
            "zeta", "alpha", "pending", "unknown", "timeout", "invalid", "deadline"
        ]
    );
    assert_eq!(
        outcomes("work"),
        ["then", "asked", "cap", "capacity", "deadline"]
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
        ["alpha", "zeta", "timeout", "invalid", "deadline"]
    );
}

const LAND: &str = r#"
[workflow]
scope='worktree'
start='begin'
[checks.begin]
shell='true'
on={pass='sweep',fail='@user'}
[checks.watch]
cases=[{when=['pr=merged'],outcome='merged'},{when=['ci=failed'],outcome='red'}]
else='waiting'
on={merged='notes',red='fix',waiting='@wait'}
timeout='3d'
[checks.notes]
run='true'
on={rule='triage',none='@done'}
[checks.ruled]
run='true'
on={rule='triage',none='@done'}
[steps.sweep]
agent='claude'
then='watch'
[steps.fix]
agent='claude'
then='watch'
max-visits=4
[steps.triage]
agent='claude'
then='ruled'
max-visits=2
"#;

const RALPH: &str = r#"
[workflow]
scope='project'
start='evidence'
on.deadline='report'
[checks.evidence]
run='true'
on={plan='plan',build='build',review='judge',report='report',wait='@wait',broken='@user'}
timeout='1d'
[checks.judge]
agent='claude'
on={build='build',plan='plan',report='report',halt='@user'}
[checks.reported]
run='true'
on={complete='@done',halted='@failed',missing='report'}
[steps.plan]
agent='claude'
then='evidence'
max-visits=20
on.cap='report'
[steps.build]
agent='claude'
then='evidence'
max-visits=80
on.cap='report'
[steps.report]
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
            "[workflow]\nscope='worktree'\nstart='tests'\n[checks.tests]\nrun='true'\non={pass='@done',fail='fix'}\n[steps.fix]\nagent='claude'\nthen='tests'\nmax-visits=3",
            vec![("tests", None), ("fix", Some("tests"))],
        ),
    ] {
        let spec: WorkflowSpec = toml::from_str(text).unwrap();
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
    let spec: WorkflowSpec = toml::from_str(
        r#"
[workflow]
scope='worktree'
start='start'
on.deadline='rescue'
[checks.start]
run='true'
on={ready='work',timeout='sleeping'}
[checks.rescue]
run='true'
on.done='@done'
[checks.sleeping]
run='true'
[checks.capped]
run='true'
[checks.orphan]
run='true'
[steps.work]
agent='claude'
then='start'
on.cap='capped'
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
    let spec: WorkflowSpec = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='c'\n\
         [checks.c]\nshell='true'\non.pass='a'\n\
         [steps.a]\nshell='true'\nthen='c'\nmax-visits=1\non.cap='b'\n\
         [steps.b]\nshell='true'\nthen='a'\n",
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
            .any(|error| error.path == "steps.b.then"
                && error
                    .message
                    .ends_with("then must name a check, never a step or @done")),
        "{report:?}"
    );
}

#[test]
fn a_deadline_only_step_does_not_hang_under_its_return_check() {
    let spec: WorkflowSpec = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='gate'\non.deadline='rescue'\n\
         [checks.gate]\nshell='true'\non.pass='@done'\n\
         [steps.rescue]\nshell='true'\nthen='gate'\n",
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
        let spec: WorkflowSpec = toml::from_str(text).unwrap();
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
    let spec: WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='z'\n[checks.z]\nshell='true'\non={pass='work',fail='@done'}\n[checks.a]\nshell='true'\non={pass='@done',fail='@user'}\n[steps.work]\nshell='true'\ntimeout='1m'\nthen='z'\nmax-visits=3").unwrap();
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
