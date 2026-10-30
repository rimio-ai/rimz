use super::*;
use crate::harness::workflow::state::Transition;

#[test]
fn kept_waits_merge_the_saved_answers_instant_instead_of_the_later_advance() {
    let (_dir, state, record) = setup(UNTIL);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let first = Timestamp::UNIX_EPOCH;
    for (index, at) in [first, first + std::time::Duration::from_secs(10)]
        .into_iter()
        .enumerate()
    {
        let mut pending = held(&live, true);
        let verdict = pending.held.as_mut().unwrap().verdict.as_mut().unwrap();
        verdict.at = Some(at);
        verdict.target = "@wait".into();
        verdict.object = json!({"outcome":"pending"});
        update(&state, &pending);
        live = advance_ok(&state, &record.id, &host);
        let wait = live.wait.as_ref().unwrap();
        assert_eq!(
            (wait.first, wait.last, wait.count),
            (first, at, index as u64 + 1)
        );
    }
}

#[test]
fn a_legacy_held_verdict_without_an_answer_instant_uses_the_merge_instant() {
    let (_dir, state, record) = setup(UNTIL);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut pending = held(&live, true);
    let verdict = pending.held.as_mut().unwrap().verdict.as_mut().unwrap();
    verdict.at = Some(Timestamp::UNIX_EPOCH);
    verdict.target = "@wait".into();
    verdict.object = json!({"outcome":"pending"});
    let mut saved = serde_json::to_value(pending).unwrap();
    saved["held"]["verdict"]
        .as_object_mut()
        .unwrap()
        .remove("at");
    let pending: RunRecord = serde_json::from_value(saved).unwrap();
    assert!(
        pending
            .held
            .as_ref()
            .unwrap()
            .verdict
            .as_ref()
            .unwrap()
            .at
            .is_none()
    );
    update(&state, &pending);
    let kept = advance_ok(&state, &record.id, &host);
    let wait = kept.wait.as_ref().unwrap();
    let merged_at: Timestamp =
        serde_json::from_value(decisions(&state, &kept)[0]["at"].clone()).unwrap();
    assert_eq!(
        (wait.first, wait.last, wait.count),
        (merged_at, merged_at, 1)
    );
}

#[test]
fn forge_probe_during_the_session_can_close_a_later_held_check() {
    let (_dir, state, host, ended) = forge_check("pr=merged", 60, json!({"state":"merged"}));
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(ended.input.as_ref().unwrap()["readings"]["pr"], "merged");
    assert_eq!(decisions(&state, &ended)[0]["action"], "closed");
}

#[test]
fn forge_probe_before_the_session_keeps_with_a_session_start_reason() {
    let (_dir, state, host, kept) = forge_check("pr=merged", -1, json!({"state":"merged"}));
    assert_eq!(
        kept.wait.as_ref().unwrap().detail.as_deref(),
        Some("pr (probed before this session started)")
    );
    assert_eq!(kept.state, RunState::Active);
    assert_eq!(host.stops.get(), 0);
    assert_eq!(decisions(&state, &kept)[0]["action"], "kept");
}

#[test]
fn a_held_forge_check_still_requires_ci_for_the_pr_head() {
    let (_dir, state, host, kept) = forge_check(
        "ci=passed",
        60,
        json!({"state":"open", "ci":"passing", "ci_sha":"aaaaaaa1", "open":{"head":"bbbbbbb2"}}),
    );
    assert_eq!(
        kept.wait.as_ref().unwrap().detail.as_deref(),
        Some("ci (read at aaaaaaa, PR head bbbbbbb)")
    );
    assert_eq!(host.stops.get(), 0);
    assert_eq!(decisions(&state, &kept)[0]["outcome"], "unknown");
}

fn forge_check(
    when: &str,
    seconds_after_start: i64,
    link: Value,
) -> (tempfile::TempDir, StatePaths, FakeHost, RunRecord) {
    let text = UNTIL.split("[checks.pr]").next().unwrap().to_owned()
        + &format!(
            "[checks.pr]\nelse='pending'\ntimeout='1h'\non={{done='@done',pending='@wait'}}\n[[checks.pr.cases]]\nwhen=['{when}']\noutcome='done'\n"
        );
    let (dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    live.entered_at -= std::time::Duration::from_secs(120);
    update(&state, &live);
    let at = live.entered_at + jiff::SignedDuration::from_secs(seconds_after_start);
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let cache = runtime.pr_state_path();
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(
        cache,
        serde_json::to_vec(&json!({
            "branch_ci":{},
            "path_repos":{(live.scope.path.to_string_lossy().as_ref()):"repo"},
            "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(), "ok":true}},
            "states":{(live.scope.path.to_string_lossy().as_ref()):link}
        }))
        .unwrap(),
    )
    .unwrap();
    host.observed.borrow_mut().turn = Some(turn());
    let resolved = advance_ok(&state, &record.id, &host);
    (dir, state, host, resolved)
}

#[test]
fn condition_hold_matures_from_the_first_kept_wait_across_turns() {
    let text = UNTIL.split("[checks.pr]").next().unwrap().to_owned()
        + "[checks.pr]\nelse='pending'\ntimeout='1h'\non={done='@done',pending='build',unknown='build'}\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='done'\nfor='1h'\n";
    let (dir, state, record) = setup(&text);
    fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let mut kept = advance_ok(&state, &record.id, &host);
    assert_eq!(
        kept.wait.as_ref().map(|wait| wait.outcome.as_str()),
        Some("hold")
    );
    assert_eq!(host.stops.get(), 0);
    kept.wait.as_mut().unwrap().first -= std::time::Duration::from_secs(7200);
    update(&state, &kept);
    host.observed
        .borrow_mut()
        .turn
        .as_mut()
        .unwrap()
        .end
        .ended_at += std::time::Duration::from_secs(1);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(
        decisions(&state, &ended)
            .iter()
            .map(|row| row["action"].clone())
            .collect::<Vec<_>>(),
        vec![json!("kept"), json!("closed")]
    );
}

#[test]
fn a_held_checks_timeout_closes_through_the_same_verdict_gate() {
    let (_dir, state, record) = setup(UNTIL);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut pending = held(&live, false);
    pending.entered_at = Timestamp::UNIX_EPOCH;
    update(&state, &pending);
    host.observed.borrow_mut().turn = Some(turn());
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(ended.state, RunState::Parked);
    let rows = decisions(&state, &ended);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], "closed");
    assert_eq!(rows[0]["outcome"], "timeout");
}

#[test]
fn held_judges_retry_and_back_off_before_resolving_their_verdict() {
    judge(false);
}

#[test]
fn held_judges_keep_the_session_during_limit_backoff() {
    judge(true);
}

fn judge(limited: bool) {
    let text = UNTIL.replace(
        "run='sh check.sh'",
        "agent='claude'\nprompt-file='prompt.md'",
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut pending = held(&live, false);
    let mut check = live.attempt.unwrap();
    check.node = "pr".into();
    check.kind = records::AttemptKind::Judge;
    check.lifecycle = StepLifecycle::Turn;
    check.judged = None;
    check.seq = pending.next_seq;
    pending.next_seq += 1;
    pending.attempt = Some(check.clone());
    let paths = state.workflow(&record.id);
    let spec: WorkflowSpec = toml::from_str(&text).unwrap();
    let mut t = Transition {
        record: pending,
        spec: &spec,
        paths: &paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    let mut result = evaluation(
        Err("judge failed".into()),
        if limited { "limited" } else { "died" },
    );
    let reset = Timestamp::now() + std::time::Duration::from_secs(1800);
    if limited {
        result.limited = Some(records::Limited {
            reset_at: Some(reset),
        });
    }
    let retry = t.finish_judge(check.seq, result).unwrap();
    assert_eq!(retry, !limited);
    assert_eq!(t.record.check_retry, u8::from(!limited));
    assert!(t.record.held.is_some());
    if limited {
        assert_eq!(t.record.recheck_at, Some(reset));
        update(&state, &t.record);
        let deferred = advance_ok(&state, &record.id, &host);
        assert!(deferred.held.is_some());
        assert_eq!(host.stops.get(), 0);
    }
    t.record.attempt = Some(check.clone());
    t.finish_judge(
        check.seq,
        if limited {
            evaluation(Ok(json!({"outcome":"pending"})), "invalid")
        } else {
            evaluation(Err("judge failed again".into()), "died")
        },
    )
    .unwrap();
    assert_eq!(
        (
            t.record.state,
            t.record
                .held
                .as_ref()
                .and_then(|held| held.verdict.as_ref())
                .map(|verdict| verdict.target.clone())
        ),
        (
            RunState::Active,
            Some(if limited { "build" } else { "@user" }.into())
        )
    );
    if t.record.held.is_some() {
        update(&state, &t.record);
        host.observed.borrow_mut().turn = Some(turn());
        let resolved = advance_ok(&state, &record.id, &host);
        assert_eq!(
            resolved.state,
            if limited {
                RunState::Active
            } else {
                RunState::Parked
            }
        );
        assert_eq!(host.stops.get(), usize::from(!limited));
    }
}
