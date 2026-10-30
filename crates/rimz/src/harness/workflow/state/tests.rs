use super::*;
use crate::store::workflow::RunState;
use serde_json::json;

mod conditions;

#[test]
fn child_start_mismatch_or_missing_token_never_kills_a_group() {
    use std::os::unix::process::CommandExt;
    for start in [None, Some("not-this-process".to_owned())] {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let spec = spec();
        let paths = paths();
        let mut record = record();
        record.attempt.as_mut().unwrap().child = Some(crate::store::workflow::HostChild {
            pid: child.id(),
            start,
        });
        let t = Transition {
            now: record.entered_at,
            record,
            spec: &spec,
            paths: &paths,
            entries: vec![],
        };
        t.kill_child();
        let survived = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        child.wait().unwrap();
        assert!(
            survived,
            "an unproven child identity must not receive a signal"
        );
    }
}

fn record() -> RunRecord {
    serde_json::from_value(json!({
        "version":1, "id":"wf_0190b8172c0070008000000000000001", "workflow":"review",
        "spec_dir":"/definitions/review", "spec_digest":"abc", "prompt":null,
        "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "park":null, "node":"pr",
        "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
        "wait":null, "visits":{}, "visit_grant":0, "deadline":null,
        "deadline_fired":false, "next_seq":2,
        "attempt":{"kind":"script","seq":1,"node":"pr","started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":"token"}},
        "prev_attempt":null, "terminal_at":null
    })).unwrap()
}

fn spec() -> WorkflowSpec {
    toml::from_str("[workflow]\nscope = 'worktree'\nstart = 'pr'\n[checks.pr]\nrun = 'true'\non.ok = '@done'\ntimeout = '1h'\n").unwrap()
}

#[test]
fn inspection_timeout_uses_the_node_entry_time() {
    let record = record();
    assert_eq!(
        super::super::timeout_at(&spec(), &record).unwrap(),
        Some(record.entered_at + jiff::SignedDuration::from_hours(1)),
    );
}

#[test]
fn inspection_forecast_uses_the_first_bound_with_engine_ties() {
    let spec: WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='pr'\non.deadline='@user'\n\
        [checks.pr]\nelse='pending'\ntimeout='5m'\non={ready='@done',pending='@wait',timeout='@failed'}\n\
        [[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\nfor='10m'\n").unwrap();
    let mut record = record();
    record.attempt = None;
    record.wait = serde_json::from_value(json!({"node":"pr", "outcome":"hold", "case":0,
        "first":record.entered_at, "last":record.entered_at, "count":1}))
    .unwrap();
    let mut actual = Vec::new();
    for (deadline_secs, fired) in [(60, false), (300, false), (600, false), (60, true)] {
        record.deadline = Some(record.entered_at + jiff::SignedDuration::from_secs(deadline_secs));
        record.deadline_fired = fired;
        actual.push(
            super::super::next_bound(&spec, &record, Some(0), record.entered_at)
                .unwrap()
                .map(|(at, target)| (at.duration_since(record.entered_at).as_secs(), target)),
        );
    }
    assert_eq!(
        actual,
        [
            Some((60, "@user".into())),
            Some((300, "@user".into())),
            Some((300, "@failed".into())),
            Some((300, "@failed".into())),
        ]
    );
    let mut spec = spec;
    spec.checks.get_mut("pr").unwrap().cases[0].hold = Some("5m".into());
    assert_eq!(
        super::super::next_bound(&spec, &record, Some(0), record.entered_at).unwrap(),
        Some((
            record.entered_at + jiff::SignedDuration::from_secs(300),
            "@failed".into()
        ))
    );
    spec.checks.get_mut("pr").unwrap().cases[0].hold = Some("1m".into());
    assert_eq!(
        super::super::next_bound(&spec, &record, Some(0), record.entered_at).unwrap(),
        Some((
            record.entered_at + jiff::SignedDuration::from_secs(60),
            "@done".into()
        ))
    );
}

#[test]
fn recheck_limit_takes_the_first_bound_without_routing_it() {
    let spec = spec();
    let paths = paths();
    let mut record = record();
    record.node = "unrouted".into();
    let deadline = record.entered_at + jiff::SignedDuration::from_secs(30);
    record.deadline = Some(deadline);
    assert_eq!(
        super::super::next_bound(&spec, &record, None, record.entered_at).unwrap(),
        None
    );
    let t = Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: vec![],
    };
    assert_eq!(
        t.bounded_recheck(Some(deadline + jiff::SignedDuration::from_secs(60)))
            .unwrap(),
        Some(deadline)
    );
}

#[test]
fn wait_backoff_cannot_hide_an_unfired_deadline() {
    let mut spec = spec();
    spec.checks
        .get_mut("pr")
        .unwrap()
        .on
        .insert("pending".into(), "@wait".into());
    let paths = paths();
    let mut record = record();
    let deadline = record.entered_at + jiff::SignedDuration::from_secs(30);
    record.deadline = Some(deadline);
    let mut t = Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: vec![],
    };
    t.finish_check(
        1,
        Ok(json!({"outcome":"pending","recheck_at":"2099-01-01T00:00:00Z"})),
        None,
    )
    .unwrap();
    assert_eq!(t.record.recheck_at, Some(deadline));
    assert_eq!(t.record.wait.as_ref().unwrap().recheck_at, Some(deadline));
}

fn paths() -> WorkflowPaths {
    WorkflowPaths {
        home: "/run".into(),
        record: "/run/record.json".into(),
        journal: "/run/journal.jsonl".into(),
        spec: "/run/workflow.toml".into(),
        work: "/run/work".into(),
        context: "/run/work/context.json".into(),
    }
}

#[test]
fn result_cas_routes_current_sequence_and_stale_results_write_nothing() {
    let spec = spec();
    let paths = paths();
    let record = record();
    let mut transition = Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: vec![],
    };
    let original = transition.record.clone();
    transition
        .finish_check(2, Ok(json!({"outcome":"ok"})), None)
        .unwrap();
    assert_eq!(transition.record, original);
    assert!(transition.entries.is_empty());
    transition
        .finish_check(1, Ok(json!({"outcome":"ok"})), None)
        .unwrap();
    assert_eq!(
        transition.record.state,
        RunState::Done,
        "only the current evaluation may route its result"
    );
    let terminal = transition.record.clone();
    let entries = transition.entries.clone();
    transition
        .finish_check(1, Ok(json!({"outcome":"ok"})), None)
        .unwrap();
    assert_eq!(transition.record, terminal);
    assert_eq!(transition.entries, entries);
}

#[test]
fn first_terminal_wins_for_every_terminal_state() {
    for (target, state) in [("@done", RunState::Done), ("@failed", RunState::Failed)] {
        let spec = spec();
        let paths = paths();
        let record = record();
        let mut t = Transition {
            now: record.entered_at,
            record,
            spec: &spec,
            paths: &paths,
            entries: vec![],
        };
        t.route(target, json!({"outcome":"ok"}), ParkReason::User)
            .unwrap();
        assert_eq!(
            t.record.state, state,
            "routing a terminal must settle the run"
        );
        let record = t.record.clone();
        let entries = t.entries.clone();
        t.route("@user", json!({"outcome":"bad"}), ParkReason::Invalid)
            .unwrap();
        t.route("@done", json!({"outcome":"ok"}), ParkReason::User)
            .unwrap();
        assert_eq!(t.record, record);
        assert_eq!(t.entries, entries);
    }
}

#[test]
fn deadline_wins_at_ties_and_fires_only_once() {
    let spec = spec();
    let paths = paths();
    let mut record = record();
    record.deadline = Some(record.entered_at);
    let mut t = Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: vec![],
    };
    assert!(
        t.bound().unwrap(),
        "a deadline reached exactly now must fire"
    );
    assert_eq!(t.record.park.as_ref().unwrap().reason, ParkReason::Deadline);
    assert!(t.record.deadline_fired);
    assert!(!t.bound().unwrap());
    assert_eq!(
        t.entries
            .iter()
            .filter(|e| matches!(e.event, crate::store::workflow::JournalEvent::Route { .. }))
            .count(),
        1
    );
}
