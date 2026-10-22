use super::*;

#[test]
fn a_keyed_hold_preserves_routed_context_until_the_case_fires() {
    let spec: WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\non={ready='next', pending='next'}\ntimeout='1h'\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\nfor='2m'\nkey='case'\n[checks.next]\nrun='true'\non.ok='@done'\n").unwrap();
    let paths = paths();
    for completes in [false, true] {
        let mut record = record();
        record.key = Some(Key::String("prior".into()));
        record.input = Some(json!({"outcome":"seed", "key":"prior"}));
        let original = record.clone();
        let mut t = Transition {
            now: record.entered_at,
            record,
            spec: &spec,
            paths: &paths,
            entries: vec![],
        };
        let answer = json!({"outcome":"ready", "case":0, "key":"case"});
        t.finish_check(1, Ok(answer.clone()), None).unwrap();
        assert_eq!(t.record.key, original.key, "a hold has not fired its key");
        assert_eq!(
            t.record.input, original.input,
            "a hold has not routed an answer"
        );
        assert_eq!(t.record.node, original.node);
        assert_eq!(t.record.entered_at, original.entered_at);
        assert_eq!(t.record.visits, original.visits);
        assert!(t.record.attempt.is_none());
        assert!(t.record.wait.as_ref().is_some_and(Wait::is_hold));
        t.record.attempt = original.attempt;
        t.now += jiff::SignedDuration::from_secs(if completes { 120 } else { 10 });
        t.finish_check(
            1,
            Ok(if completes {
                answer
            } else {
                json!({"outcome":"pending"})
            }),
            None,
        )
        .unwrap();
        assert_eq!(t.record.node, "next");
        assert_eq!(
            t.record.key,
            Some(Key::String(if completes { "case" } else { "prior" }.into()))
        );
        assert_eq!(
            t.record.input.as_ref().unwrap()["key"],
            if completes { "case" } else { "prior" }
        );
        assert_eq!(t.record.visits, original.visits);
        assert!(t.record.wait.is_none());
    }
}

#[test]
fn a_different_case_with_the_same_outcome_restarts_the_hold() {
    let spec: WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\non={ready='@done', pending='@wait'}\ntimeout='1h'\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\nfor='2m'\n[[checks.pr.cases]]\nwhen=['team.stage!=Done']\noutcome='ready'\nfor='2m'\n").unwrap();
    let paths = paths();
    let record = record();
    let attempt = record.attempt.clone();
    let mut t = Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: vec![],
    };
    t.finish_check(1, Ok(json!({"outcome":"ready", "case":0})), None)
        .unwrap();
    assert_eq!(t.record.wait.as_ref().map(|w| w.case), Some(Some(0)));
    t.now += jiff::SignedDuration::from_secs(10);
    t.record.attempt = attempt;
    t.finish_check(1, Ok(json!({"outcome":"ready", "case":1})), None)
        .unwrap();
    let wait = t.record.wait.as_ref().unwrap();
    assert_eq!(wait.case, Some(1));
    assert_eq!(wait.first, t.now);
    assert_eq!(wait.count, 1);
    assert!(t.entries.iter().any(|entry| matches!(&entry.event, JournalEvent::Wait { outcome, detail, .. } if outcome == "hold" && detail.as_deref() == Some("ready"))));
}

#[test]
fn condition_hold_settles_at_the_inclusive_boundary_and_clamps_to_bounds() {
    let spec: WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nelse='pending'\non={ready='@done', pending='@wait'}\ntimeout='1h'\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\nfor='2m'\n").unwrap();
    let paths = paths();
    for bound in [None, Some(30)] {
        let mut record = record();
        let first = record.entered_at + jiff::SignedDuration::from_secs(10);
        let attempt = record.attempt.clone().unwrap();
        record.deadline = bound.map(|s| first + jiff::SignedDuration::from_secs(s));
        let mut t = Transition {
            record,
            spec: &spec,
            paths: &paths,
            now: first,
            entries: vec![],
        };
        let object = json!({"outcome":"ready", "case":0, "readings":{"team.stage":"Done"}});
        t.finish_check(1, Ok(object.clone()), None).unwrap();
        assert_eq!(
            t.record.state,
            RunState::Active,
            "a hold must wait before routing"
        );
        let wait = t.record.wait.as_ref().unwrap();
        assert_eq!(
            wait.first, first,
            "hold starts at settlement, not entry or evaluation start"
        );
        assert_eq!(wait.case, Some(0));
        assert_eq!(wait.detail.as_deref(), Some("ready"));
        assert_eq!(
            wait.recheck_at,
            Some(first + jiff::SignedDuration::from_secs(bound.unwrap_or(120)))
        );
        if let Some(seconds) = bound {
            t.now = first + jiff::SignedDuration::from_secs(seconds);
            assert!(t.bound().unwrap());
            assert_eq!(t.record.input.as_ref().unwrap()["outcome"], "deadline");
            continue;
        }
        t.record.attempt = Some(attempt.clone());
        t.now = first + jiff::SignedDuration::from_secs(120) - jiff::SignedDuration::from_nanos(1);
        t.finish_check(1, Ok(object.clone()), None).unwrap();
        assert_eq!(t.record.state, RunState::Active);
        assert_eq!(t.record.wait.as_ref().unwrap().first, first);
        t.record.attempt = Some(attempt);
        t.now = first + jiff::SignedDuration::from_secs(120);
        t.finish_check(1, Ok(object), None).unwrap();
        assert_eq!(
            t.record.state,
            RunState::Done,
            "the hold completes at equality"
        );
        assert_eq!(t.record.input.as_ref().unwrap()["outcome"], "ready");
    }
}
