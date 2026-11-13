use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self as records, AttemptOutcome, Host, JournalEvent, Key, RunRecord, RunState,
};
use jiff::Timestamp;
use serde_json::{Value, json};

const STEP: &str = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\nmax-visits=2\n";

const OUTCOME_STEP: &str = "[workflow]\nscope='worktree'\nstart='watch'\n\
    [script.watch]\nrun='cat answer.json'\ntimeout='1h'\non={red='build',blue='build',queued='@wait'}\n\
    [script.judge]\nrun='cat answer.json'\ntimeout='1h'\non={red='build',queued='@wait'}\n\
    [agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=6\n";

mod brief;
mod capabilities;
mod keep;
mod reached;
mod restart;
mod stops;
mod subscribe;
mod until;

/// The definition with an on-break agent, so a failure edge leaves the run resumable at
/// `@break`.
pub(super) fn on_break(text: &str) -> String {
    text.replacen("[workflow]\n", "[workflow]\non-break={agent='claude'}\n", 1)
}

pub(super) fn setup(text: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let dir = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    fs::write(dir.path().join("prompt.md"), "Build it").unwrap();
    let spec: Definition = toml::from_str(text).unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":crate::store::workflow::VERSION,
        "id":WorkflowRunId::new(),
        "workflow":"test", "spec_dir":dir.path(),
        "spec_digest":"test", "prompt":"Run request", "project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "node":spec.authored().workflow.start,
        "entered_at":jiff::Timestamp::now(), "deadline_fired":false, "next_seq":1, "visits":{}, "visit_grant":0
    })).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(&state, &record, text.as_bytes(), &guard).unwrap();
    (dir, state, record)
}

pub(super) fn update(state: &StatePaths, record: &RunRecord) {
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&state.workflow(&record.id), record, &guard).unwrap();
}

fn advance_ok(state: &StatePaths, id: &WorkflowRunId, host: &FakeHost) -> RunRecord {
    let result = advance(state, id, host);
    assert!(result.is_ok(), "agent steps must be admitted: {result:?}");
    records::load(&state.workflow(id)).unwrap()
}

#[test]
fn on_break_launch_failure_opens_a_trace_and_cancel_detaches() {
    let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (_dir, state, mut record) = setup(&text);
    record.key = Some(Key::String("launch-key".into()));
    update(&state, &record);
    let host = FakeHost::new();
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "launch failed: missing account".into(),
    });
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(live.node, "@break");
    assert_eq!(live.state, RunState::Active);
    assert_eq!(live.input.as_ref().unwrap()["from"], "build");
    assert_eq!(live.input.as_ref().unwrap()["edge"], "invalid");
    assert_eq!(
        live.key, record.key,
        "a launch failure must preserve the key"
    );
    assert_eq!(live.input.as_ref().unwrap()["key"], "launch-key");
    assert_eq!(live.visits["build"], 1);
    let launches = host.launched.borrow();
    let prompt = &launches.last().unwrap().prompt;
    for text in [
        "launch failure",
        "missing account",
        "build",
        "invalid",
        "test",
        "workflow resume",
        "workflow cancel",
        "context.json",
        "journal",
        "visit",
        "native question",
        "end your turn",
        "@failed",
    ] {
        assert!(prompt.contains(text), "missing {text}: {prompt}");
    }
    assert!(prompt.contains(&record.spec_dir.display().to_string()));
    drop(launches);
    cancel(&state, &record.id, &host, None).unwrap();
    assert_eq!(host.stops.get(), 0);
    assert_eq!(host.detaches.get(), 1);
    assert_eq!(
        records::load(&state.workflow(&record.id)).unwrap().state,
        RunState::Canceled
    );
}

#[test]
fn on_break_launch_disables_the_leader_wrappers_cleanup() {
    let text = "[workflow]\nscope='worktree'\nstart='nap'\non-break={agent='claude'}\n[sleep.nap]\nsleep='invalid'\nthen='@done'\n";
    let (_dir, state, record) = setup(text);
    let host = FakeHost::new();
    assert_eq!(advance_ok(&state, &record.id, &host).node, "@break");
    assert!(
        host.launched.borrow().last().unwrap().keep,
        "the production host maps keep=false to wrapper cleanup, even when finish detaches"
    );
}

#[test]
fn on_break_sleep_keeps_failure_input_and_survives_deadline_until_session_exit() {
    let text = "[workflow]\nscope='worktree'\nstart='nap'\non-break={agent='claude'}\n[sleep.nap]\nsleep='invalid'\nthen='@done'\n";
    let (_dir, state, mut record) = setup(text);
    record.input = Some(json!({"outcome":"previous"}));
    update(&state, &record);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    assert_eq!(live.node, "@break");
    assert_eq!(live.input.as_ref().unwrap()["from"], "nap");
    assert_eq!(live.input.as_ref().unwrap()["edge"], "invalid");
    live.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
    update(&state, &live);
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.node, "@break");
    assert!(waiting.attempt.is_some());
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Failed);
    assert_eq!(host.stops.get(), 0);
    assert_eq!(host.detaches.get(), 1);
}

#[test]
fn on_break_resume_detaches_and_reenters_the_source_or_selected_node() {
    for at in [None, Some("other")] {
        let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}")
            + "[script.other]\nrun='cat answer.json'\non.ok='@done'\n";
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        *host.failure.borrow_mut() = Some(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason: "broken launch".into(),
        });
        assert_eq!(advance_ok(&state, &record.id, &host).node, "@break");
        fs::write(record.spec_dir.join("answer.json"), r#"{"outcome":"ok"}"#).unwrap();
        let result = resume(&state, &record.id, at, &[]);
        assert!(result.is_ok(), "an active break must resume: {result:?}");
        let resumed = advance_ok(&state, &record.id, &host);
        assert_eq!(host.detaches.get(), 1);
        assert_eq!(host.stops.get(), 0);
        if at.is_some() {
            assert_eq!(resumed.state, RunState::Done);
        } else {
            assert_eq!(resumed.node, "build");
            assert!(resumed.attempt.is_some());
        }
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        let entry = journal
            .iter()
            .find(|entry| matches!(entry.event, JournalEvent::Resume { .. }))
            .unwrap();
        let target = at.unwrap_or("build");
        assert_eq!(entry.node, target);
        let spec = spec::load(&state.workflow(&record.id).home).unwrap();
        let hops = hops(&spec, &journal, None);
        let resume = hops.iter().find(|hop| hop.kind == HopKind::Resume).unwrap();
        assert_eq!(resume.node, target);
        assert!(resume.target.is_none());
        assert!(
            !hops.iter().any(|hop| hop.node == "@break"
                && hop.target.as_ref().map(|target| target.as_str()) == Some("@failed")),
            "resuming must not invent a failed debugger hop: {hops:?}"
        );
        assert_eq!(
            routing_hop(&spec, &journal, target, None).unwrap().kind,
            HopKind::Resume,
            "the resume must be the new arrival's sender"
        );
    }
}

#[test]
fn on_break_resume_before_launch_is_the_arrival_sender() {
    let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "broken launch".into(),
    });
    let occupancy = host.occupancy.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        *occupancy.borrow_mut() = Occupancy::Occupied(vec!["@other".into()]);
    }));
    let broken = advance_ok(&state, &record.id, &host);
    assert_eq!(broken.node, "@break");
    assert!(broken.attempt.is_none());
    let resumed = resume(&state, &record.id, None, &[]).unwrap();
    let spec = spec::load(&state.workflow(&record.id).home).unwrap();
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert_eq!(
        routing_hop(&spec, &journal, "build", None).map(|hop| hop.kind),
        Some(HopKind::Resume),
        "a break resume is an arrival even before a debugger launches"
    );
    resume(&state, &record.id, None, &["visits=+1".into()]).unwrap();
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert_eq!(
        routing_hop(&spec, &journal, "build", None).unwrap().at,
        resumed.entered_at,
        "an active extension must not replace the break resume sender"
    );
}

#[test]
fn on_break_cap_resume_resets_the_source_visits_without_another_debugger() {
    let text = STEP
        .replace("start='build'", "start='build'\non-break={agent='claude'}")
        .replace("max-visits=2", "max-visits=1")
        .replace("then='@done'", "then='gate'")
        + "[script.gate]\nrun='cat answer.json'\non.again='build'\n";
    let (_dir, state, record) = setup(&text);
    fs::write(
        record.spec_dir.join("answer.json"),
        r#"{"outcome":"again"}"#,
    )
    .unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let observed = host.observed.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        observed.borrow_mut().status = RunStatus::Running;
    }));
    let broken = advance_ok(&state, &record.id, &host);
    assert_eq!(broken.node, "@break");
    assert_eq!(broken.input.as_ref().unwrap()["edge"], "cap");
    resume(&state, &record.id, None, &[]).unwrap();
    let resumed = advance_ok(&state, &record.id, &host);
    assert_eq!(resumed.node, "build", "a plain resume must clear the cap");
    assert_eq!(resumed.visits["build"], 1);
    let debugger_launches = host
        .launched
        .borrow()
        .iter()
        .filter(|launch| {
            matches!(
                &launch.automation,
                crate::agents::Automation::Workflow { step, .. } if step == "@break"
            )
        })
        .count();
    assert_eq!(debugger_launches, 1);
    assert_eq!(host.launched.borrow().len(), 3);
}

#[test]
fn on_break_detached_debuggers_neither_block_nor_get_evicted_on_resume() {
    for takeover in [false, true] {
        for working in [false, true] {
            let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}")
                + &format!(
                    "admission.occupancy='{}'\n",
                    if takeover { "takeover" } else { "wait" }
                );
            let (_dir, state, record) = setup(&text);
            let host = FakeHost::new();
            *host.failure.borrow_mut() = Some(LaunchFailure {
                outcome: AttemptOutcome::Died,
                reason: "broken launch".into(),
            });
            assert_eq!(advance_ok(&state, &record.id, &host).node, "@break");
            let occupancy = host.occupancy.clone();
            *host.on_detach.borrow_mut() = Some(Box::new(move || {
                *occupancy.borrow_mut() = if working {
                    Occupancy::Blocked(vec![(
                        "@builder".into(),
                        crate::harness::takeover::OpenTurn::Working,
                    )])
                } else {
                    Occupancy::Occupied(vec!["@builder".into()])
                };
            }));
            resume(&state, &record.id, None, &[]).unwrap();
            let resumed = advance_ok(&state, &record.id, &host);
            assert!(
                resumed.wait.is_none(),
                "the debugger is not an occupant: {resumed:?}"
            );
            assert!(resumed.attempt.is_some());
            assert_eq!(host.evictions.get(), 0, "the debugger must not be evicted");
            assert_eq!(host.stops.get(), 0);
        }
    }
}

#[test]
fn on_break_resume_refuses_a_live_settler() {
    let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "broken launch".into(),
    });
    let mut broken = advance_ok(&state, &record.id, &host);
    for cause in [records::EndCause::Cancel, records::EndCause::Lifecycle] {
        let attempt = broken.attempt.as_mut().unwrap();
        attempt.host = Host {
            pid: std::process::id(),
            start: None,
        };
        attempt.ending = Some(records::Ending {
            outcome: AttemptOutcome::Canceled,
            cause,
            route: None,
            reason: None,
        });
        update(&state, &broken);
        assert!(
            resume(&state, &record.id, None, &[]).is_err(),
            "a live settler owns the ending"
        );
        assert_eq!(records::load(&state.workflow(&record.id)).unwrap(), broken);
    }
}

#[test]
fn on_break_resume_refuses_the_reserved_target() {
    let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "broken launch".into(),
    });
    advance_ok(&state, &record.id, &host);
    assert!(
        resume(&state, &record.id, Some("@break"), &[]).is_err(),
        "@break is not a resume destination"
    );
}

#[test]
fn on_break_pending_held_failure_is_not_a_taken_hop() {
    let text = until::UNTIL.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let paths = state.workflow(&record.id);
    let spec = spec::load(&paths.home).unwrap();
    let mut checking = until::held(&live, false);
    checking.attempt = Some(
        serde_json::from_value(json!({
            "kind":"script", "seq":2, "node":"pr", "started_at":checking.entered_at,
            "host":{"pid":0,"start":null}
        }))
        .unwrap(),
    );
    let mut t = super::super::state::Transition {
        now: checking.entered_at,
        record: checking,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.finish_check(2, Err("bad answer".into()), None).unwrap();
    assert_eq!(
        t.record
            .held
            .as_ref()
            .unwrap()
            .verdict
            .as_ref()
            .unwrap()
            .target,
        "@break"
    );
    assert!(
        hops(&spec, &t.entries, t.record.held.as_ref()).is_empty(),
        "a pending verdict is not a hop"
    );
}

#[test]
fn on_break_held_judge_failure_uses_invalid_and_one_departure() {
    let text = until::UNTIL
        .replace("start='build'", "start='build'\non-break={agent='claude'}")
        .replace(
            "[script.pr]\nrun='sh check.sh'",
            "[judge.pr]\nagent='claude'\nprompt-file='prompt.md'",
        );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let paths = state.workflow(&record.id);
    let spec = spec::load(&paths.home).unwrap();
    let mut t = super::super::state::Transition {
        now: live.entered_at,
        record: until::held(&live, false),
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    for seq in 2..=3 {
        t.record.attempt = Some(
            serde_json::from_value(json!({
                "kind":"judge", "seq":seq, "node":"pr", "started_at":t.now,
                "host":{"pid":0,"start":null}
            }))
            .unwrap(),
        );
        t.record.next_seq = seq + 1;
        t.finish_judge(seq, evaluation(Err("judge exited 7".into()), "died"))
            .unwrap();
    }
    let verdict = t.record.held.as_ref().unwrap().verdict.as_ref().unwrap();
    assert_eq!(
        verdict.object["outcome"], "invalid",
        "route lookup must use the invalid edge"
    );
    assert_eq!(verdict.object["failure"], "died");
    assert!(
        hops(&spec, &t.entries, t.record.held.as_ref()).is_empty(),
        "pending verdicts must not depart"
    );
    t.record.held.as_mut().unwrap().verdict.as_mut().unwrap().at = Timestamp::UNIX_EPOCH;
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        for entry in &t.entries {
            records::append(&paths, entry, &guard).unwrap();
        }
        records::write(&paths, &t.record, &guard).unwrap();
    }
    host.observed.borrow_mut().turn = Some(until::turn());
    let applied = advance_ok(&state, &record.id, &host);
    let journal = records::journal(&paths).unwrap();
    let hops = hops(&spec, &journal, applied.held.as_ref());
    assert_eq!(
        hops.iter()
            .filter(|hop| hop.node == "pr"
                && hop.target.as_ref().map(|target| target.as_str()) == Some("@break"))
            .count(),
        1,
        "{hops:?}"
    );
    let launches = host.launched.borrow();
    let prompt = &launches.last().unwrap().prompt;
    assert!(prompt.contains("judge failure"), "{prompt}");
    assert!(
        prompt.contains("3.check.json"),
        "judges write check.json: {prompt}"
    );
    assert!(!prompt.contains("3.check.log"), "{prompt}");
}

#[test]
fn on_break_cap_trace_does_not_misname_an_earlier_command_exit() {
    let text = STEP
        .replace("start='build'", "start='build'\non-break={agent='claude'}")
        .replace(
            "[agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'",
            "[command.build]\nrun='exit 7'",
        )
        .replace("max-visits=2", "max-visits=1")
        .replace("then='@done'", "then='gate'")
        + "[script.gate]\nrun='cat answer.json'\non.again='build'\n";
    let (_dir, state, record) = setup(&text);
    fs::write(
        record.spec_dir.join("answer.json"),
        r#"{"outcome":"again"}"#,
    )
    .unwrap();
    let host = FakeHost::new();
    assert_eq!(advance_ok(&state, &record.id, &host).node, "@break");
    let launches = host.launched.borrow();
    let prompt = &launches.last().unwrap().prompt;
    assert!(
        prompt.starts_with("Investigate this run's workflow failure."),
        "a cap is not a launch failure: {prompt}"
    );
}

#[test]
fn on_break_trace_bounds_a_long_poll_history_below_the_prompt_guard() {
    let text = "[workflow]\nscope='worktree'\nstart='poll'\non-break={agent='claude'}\n[script.poll]\nrun='true'\non.pending='@wait'\ntimeout='1h'\n";
    let (_dir, state, record) = setup(text);
    let paths = state.workflow(&record.id);
    let spec = spec::load(&paths.home).unwrap();
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    for seq in 1..=500 {
        t.record.attempt = Some(
            serde_json::from_value(json!({
                "kind":"script", "seq":seq, "node":"poll", "started_at":t.now,
                "host":{"pid":0,"start":null}
            }))
            .unwrap(),
        );
        t.record.next_seq = seq + 1;
        t.finish_check(
            seq,
            Ok(json!({
                "outcome":"pending",
                "reason":format!("last-reading {seq} {}", "x".repeat(512))
            })),
            None,
        )
        .unwrap();
        t.now += std::time::Duration::from_secs(1);
    }
    let mut journal = String::new();
    for entry in &t.entries {
        journal.push_str(&serde_json::to_string(entry).unwrap());
        journal.push('\n');
    }
    fs::write(&paths.journal, journal).unwrap();
    t.record.deadline = Some(Timestamp::UNIX_EPOCH);
    update(&state, &t.record);
    let host = FakeHost::new();
    assert_eq!(advance_ok(&state, &t.record.id, &host).node, "@break");
    let launches = host.launched.borrow();
    let prompt = &launches.last().unwrap().prompt;
    assert!(
        prompt.len() < crate::harness::prompt_compose::TEXT_PROMPT_LIMIT,
        "a long history must not prevent the debugger launch: {} bytes",
        prompt.len()
    );
    assert!(prompt.contains("last-reading 500"));
    assert!(prompt.contains(&paths.journal.display().to_string()));
}

#[test]
fn on_break_held_invalid_answer_preserves_the_failure_context() {
    let text = until::UNTIL.replace("start='build'", "start='build'\non-break={agent='claude'}");
    let (dir, state, record) = setup(&text);
    until::check(
        dir.path(),
        json!({"outcome":"not-routed", "reason":"bad answer"}),
    );
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(until::turn());
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(live.node, "@break");
    assert_eq!(live.input.as_ref().unwrap()["from"], "pr");
    assert_eq!(live.input.as_ref().unwrap()["edge"], "invalid");
    assert!(live.input.as_ref().unwrap()["reason"].is_string());
    let context: Value =
        serde_json::from_slice(&fs::read(state.workflow(&record.id).context).unwrap()).unwrap();
    assert_eq!(context["input"], *live.input.as_ref().unwrap());
    let launches = host.launched.borrow();
    let prompt = &launches.last().unwrap().prompt;
    assert!(prompt.contains("check.log"), "{prompt}");
    assert!(prompt.contains("not-routed"), "{prompt}");
}

#[test]
fn on_break_uses_author_prompt_and_its_own_failed_launch_ends_failed() {
    for custom in [false, true] {
        let handler = if custom {
            "{agent='claude',prompt-file='prompt.md'}"
        } else {
            "{agent='claude'}"
        };
        let text = STEP.replace(
            "start='build'",
            &format!("start='build'\non-break={handler}"),
        );
        let (_dir, state, mut record) = setup(&text);
        record.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        update(&state, &record);
        let host = FakeHost::new();
        if !custom {
            *host.failure.borrow_mut() = Some(LaunchFailure {
                outcome: AttemptOutcome::Died,
                reason: "debugger unavailable".into(),
            });
        }
        let result = advance_ok(&state, &record.id, &host);
        if custom {
            assert_eq!(result.node, "@break");
            assert!(host.launched.borrow()[0].prompt.starts_with("Build it"));
        } else {
            assert_eq!(result.state, RunState::Failed);
            assert_eq!(host.launched.borrow().len(), 1);
        }
    }
}

#[test]
fn on_break_judge_second_failure_routes_invalid_and_history_records_the_hop() {
    for failure in ["died", "invalid"] {
        for explicit in [false, true] {
            let (_, paths, record) = judge_fixture();
            let text = format!(
                "[workflow]\nscope='worktree'\nstart='judge'\non-break={{agent='claude'}}\n[judge.judge]\nagent='claude'\nprompt-file='judge.md'\non.ok='@done'\n{}",
                if explicit { "on.invalid='@failed'" } else { "" }
            );
            let spec = super::super::spec::parse(&text).unwrap();
            let mut t = super::super::state::Transition {
                now: record.entered_at,
                record,
                spec: &spec,
                paths: &paths,
                entries: Vec::new(),
            };
            let attempt = t.record.attempt.clone();
            assert!(
                t.finish_judge(1, evaluation(Err("judge failed".into()), failure))
                    .unwrap()
            );
            assert_eq!(t.record.node, "judge");
            assert!(hops(&spec, &t.entries, None).is_empty());
            t.record.attempt = attempt;
            t.finish_judge(1, evaluation(Err("judge failed".into()), failure))
                .unwrap();
            let expected = if explicit { "@failed" } else { "@break" };
            let routes = hops(&spec, &t.entries, None);
            assert_eq!(
                routes
                    .iter()
                    .filter(|hop| hop.kind == HopKind::Route)
                    .count(),
                1,
                "{routes:?}"
            );
            assert_eq!(
                routes[0].target.as_ref().map(|target| target.as_str()),
                Some(expected)
            );
            assert_ne!(t.record.state, RunState::Parked);
        }
    }
}

fn answer(state: &StatePaths, record: &RunRecord, host: &FakeHost, object: Value) -> RunRecord {
    fs::write(
        record.spec_dir.join("answer.json"),
        serde_json::to_vec(&object).unwrap(),
    )
    .unwrap();
    let mut current = records::load(&state.workflow(&record.id)).unwrap();
    current.recheck_at = None;
    update(state, &current);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let observed = host.observed.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        observed.borrow_mut().status = RunStatus::Running;
    }));
    advance_ok(state, &record.id, host)
}

#[test]
fn resuming_a_cap_resets_the_capped_steps_count() {
    let mut actual = Vec::new();
    for at in [None, Some("build")] {
        let (_dir, state, record) = setup(&on_break(
            &OUTCOME_STEP.replace("max-visits=6", "max-visits=2"),
        ));
        let host = FakeHost::new();
        for _ in 0..3 {
            answer(
                &state,
                &record,
                &host,
                json!({"outcome":"red", "key":"lint"}),
            );
        }
        let paths = state.workflow(&record.id);
        assert_eq!(records::load(&paths).unwrap().node, "@break");
        resume(&state, &record.id, at, &[]).unwrap();
        advance_ok(&state, &record.id, &host);
        let context: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
        let next = answer(
            &state,
            &record,
            &host,
            json!({"outcome":"red", "key":"lint"}),
        );
        actual.push((
            context["visit"].as_u64(),
            next.state,
            next.visits["build"],
            host.launched.borrow().len(),
        ));
    }
    assert_eq!(
        actual,
        [
            (Some(1), RunState::Active, 2, 5),
            (Some(1), RunState::Active, 2, 5)
        ],
        "both resume forms reset all counts before counting the first new visit"
    );
}

#[test]
fn cap_routed_into_a_different_step_counts_under_cap() {
    let text = format!(
        "{}on.cap='recover'\n[agent.recover]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='turn'\nthen='watch'\n",
        OUTCOME_STEP.replace("max-visits=6", "max-visits=2")
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    for _ in 0..3 {
        answer(
            &state,
            &record,
            &host,
            json!({"outcome":"red", "key":"lint"}),
        );
    }
    let live = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(live.node, "recover");
    assert_eq!(live.visits["recover"], 1);
    assert_eq!(
        live.prev_attempt.as_ref().unwrap().routed_by.as_deref(),
        Some("red")
    );
    let ctx: Value =
        serde_json::from_slice(&fs::read(state.workflow(&record.id).context).unwrap()).unwrap();
    assert_eq!(ctx["input"]["routed_by"], "red");
}

#[test]
fn cap_resume_resets_all_counts_and_keeps_grants() {
    let text = on_break(&OUTCOME_STEP.replace("max-visits=6", "max-visits=3"));
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red"}));
    answer(&state, &record, &host, json!({"outcome":"queued"}));
    answer(&state, &record, &host, json!({"outcome":"red"}));
    answer(&state, &record, &host, json!({"outcome":"red"}));
    let capped = answer(&state, &record, &host, json!({"outcome":"red"}));
    assert_eq!(capped.input.as_ref().unwrap()["bound"], "max-visits");
    assert_eq!(capped.input.as_ref().unwrap()["visits"], 3);
    let extended = resume(&state, &record.id, Some("watch"), &["visits=+2".into()]).unwrap();
    assert_eq!(extended.visit_grant, 2);
    for count in 1..=5 {
        let live = answer(&state, &record, &host, json!({"outcome":"red"}));
        let ctx: Value =
            serde_json::from_slice(&fs::read(state.workflow(&record.id).context).unwrap()).unwrap();
        assert_eq!(ctx["visit"], count);
        assert_eq!(live.state, RunState::Active);
    }
    let capped = answer(&state, &record, &host, json!({"outcome":"red"}));
    assert_eq!(capped.input.as_ref().unwrap()["bound"], "max-visits");
    assert_eq!(capped.input.as_ref().unwrap()["max"], 5);
}

#[test]
fn step_then_forwards_the_routed_outcome_into_the_next_context() {
    let text = OUTCOME_STEP.replace("then='watch'", "then='finish'");
    let text = format!(
        "{text}[agent.finish]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='turn'\nthen='watch'\n"
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    answer(&state, &record, &host, json!({"outcome":"red", "key":7}));
    let live = answer(&state, &record, &host, json!({"outcome":"blue"}));
    assert_eq!(live.node, "finish");
    let ctx: Value =
        serde_json::from_slice(&fs::read(state.workflow(&record.id).context).unwrap()).unwrap();
    assert_eq!(ctx["input"], json!({"outcome":"red","key":7}));
    assert_eq!(live.visits["finish"], 1);
}

fn cancel_row(state: &StatePaths, id: &WorkflowRunId) -> Value {
    records::journal(&state.workflow(id))
        .unwrap()
        .into_iter()
        .map(|entry| serde_json::to_value(entry.event).unwrap())
        .find(|event| event["event"] == "cancel")
        .expect("cancel must be journaled")
}

#[test]
fn cancel_reason_is_recorded_for_waiting_and_broken_runs() {
    let mut rows = Vec::new();
    for broken in [false, true] {
        let text = if broken {
            "[workflow]\nscope='worktree'\nstart='nap'\non-break={agent='claude'}\n[sleep.nap]\nsleep='invalid'\nthen='@done'\n"
        } else {
            STEP
        };
        let (_dir, state, record) = setup(text);
        let host = FakeHost::new();
        if broken {
            assert_eq!(advance_ok(&state, &record.id, &host).node, "@break");
        } else {
            *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["builder".into()]);
            assert!(advance_ok(&state, &record.id, &host).wait.is_some());
        }
        cancel(&state, &record.id, &host, Some("worktree removed")).unwrap();
        rows.push(cancel_row(&state, &record.id));
    }
    assert_eq!(
        rows,
        vec![json!({"event":"cancel","reason":"worktree removed"}); 2]
    );
}

#[test]
fn cancel_reason_survives_agent_settlement() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let stop_state = state.clone();
    let id = record.id.clone();
    *host.on_stop.borrow_mut() = Some(Box::new(move || {
        let staged = records::load(&stop_state.workflow(&id)).unwrap();
        let ending = serde_json::to_value(staged.attempt.unwrap().ending.unwrap()).unwrap();
        assert_eq!(ending["reason"], "worktree removed");
    }));
    cancel(&state, &record.id, &host, Some("worktree removed")).unwrap();
    assert_eq!(host.stops.get(), 1);
    assert_eq!(
        cancel_row(&state, &record.id),
        json!({"event":"cancel","reason":"worktree removed"})
    );
}

#[test]
fn cancel_reason_is_recorded_for_script_attempts() {
    let text = "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nshell='true'\nthen='@done'\n";
    let (_dir, state, mut record) = setup(text);
    record.attempt = Some(
        serde_json::from_value(json!({
            "kind":"script", "seq":1, "node":"build", "started_at":record.entered_at,
            "host":{"pid":u32::MAX, "start":null}
        }))
        .unwrap(),
    );
    record.visits.insert("build".into(), 1);
    update(&state, &record);
    cancel(
        &state,
        &record.id,
        &FakeHost::new(),
        Some("worktree removed"),
    )
    .unwrap();
    assert_eq!(
        cancel_row(&state, &record.id),
        json!({"event":"cancel","reason":"worktree removed"})
    );
}

#[test]
fn cancel_without_reason_keeps_the_old_journal_shape() {
    let (_dir, state, record) = setup(STEP);
    cancel(&state, &record.id, &FakeHost::new(), None).unwrap();
    let old = json!({"event":"cancel"});
    assert_eq!(cancel_row(&state, &record.id), old);
    let decoded: JournalEvent = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), old);
}

#[test]
fn cancel_preserves_the_stopping_node_in_the_record_and_terminal_journal() {
    for command in [false, true] {
        let runner = if command {
            "[command.work]\nshell='sleep 60'\ntimeout='1h'\nthen='after'\n"
        } else {
            "[test.work]\nshell='false'\ntimeout='1h'\non.fail='@wait'\non.pass='@done'\n"
        };
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='begin'\n\
             [test.begin]\nshell='true'\non.pass='work'\non.fail='@failed'\n\
             {runner}[test.after]\nshell='true'\non.pass='@done'\non.fail='@failed'\n"
        );
        let (_dir, state, record) = setup(&text);
        let paths = state.workflow(&record.id);
        std::thread::scope(|threads| {
            let running =
                command.then(|| threads.spawn(|| advance_ok(&state, &record.id, &FakeHost::new())));
            if command {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let live = records::load(&paths).unwrap();
                    if live.node == "work"
                        && live
                            .attempt
                            .as_ref()
                            .is_some_and(|attempt| attempt.child.is_some())
                    {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "{live:?}");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            } else {
                let waiting = advance_ok(&state, &record.id, &FakeHost::new());
                assert_eq!(waiting.node, "work");
                assert!(waiting.attempt.is_none() && waiting.wait.is_some());
            }
            assert!(cancel(&state, &record.id, &FakeHost::new(), None).unwrap());
            if let Some(running) = running {
                running.join().unwrap();
            }
        });
        let canceled = records::load(&paths).unwrap();
        assert_eq!(canceled.state, RunState::Canceled, "command: {command}");
        assert_eq!(canceled.node, "work", "command: {command}");
        let journal = records::journal(&paths).unwrap();
        let terminal = journal.last().unwrap();
        assert_eq!(
            terminal.event,
            JournalEvent::Terminal {
                state: RunState::Canceled
            }
        );
        assert_eq!(terminal.node, canceled.node, "command: {command}");
    }
}

#[test]
fn cancel_helper_names_the_machine_wide_id_and_reason() {
    let id = WorkflowRunId::new();
    assert_eq!(
        cancel_args(&id, "worktree removed"),
        [
            "workflow",
            "cancel",
            id.as_str(),
            "--reason",
            "worktree removed"
        ]
        .map(std::ffi::OsString::from)
    );
}

#[test]
fn reread_prompt_file_type_error_refuses_without_panicking() {
    let (_dir, state, record) = setup(STEP);
    let paths = state.workflow(&record.id);
    fs::write(
        &paths.spec,
        STEP.replace("prompt-file='prompt.md'", "prompt-file=5"),
    )
    .unwrap();
    let host = FakeHost::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        advance(&state, &record.id, &host)
    }));
    assert!(
        matches!(&result, Ok(Err(error)) if error.to_string().contains("agent.build.prompt-file")),
        "a sealed prompt-file of the wrong type refuses the advance: {result:?}"
    );
    assert!(host.launched.borrow().is_empty());
}

#[test]
fn launch_counts_keyed_visits_and_composes_live_signs() {
    let (dir, state, mut record) = setup(STEP);
    record.key = Some(Key::Integer(7));
    record.input = Some(json!({"outcome":"work", "key":7}));
    update(&state, &record);
    let paths = state.workflow(&record.id);
    fs::create_dir_all(paths.signs(None)).unwrap();
    fs::write(paths.signs(Some("build")), "Keep the public API").unwrap();
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(
        host.launched.borrow().len(),
        1,
        "a step launches a supervised attempt"
    );
    let launches = host.launched.borrow();
    assert_eq!(
        launches[0].prompt,
        "Build it\n\nKeep the public API\n\nRun request"
    );
    assert_eq!(launches[0].workflow.run, record.id);
    assert_eq!(launches[0].workflow.seq, 1);
    assert_eq!(launches[0].cwd, record.scope.path);
    assert_eq!(live.visits["build"], 1);
    let ctx: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["visit"], 1);
    assert_eq!(ctx["visits"], 1);
    assert_eq!(ctx["input"], record.input.clone().unwrap());
    let attempt = live.attempt.unwrap();
    assert_eq!(
        attempt.digests["prompt.md"],
        super::super::script::digest(b"Build it")
    );
    assert_eq!(
        attempt.digests["signs/build.md"],
        super::super::script::digest(b"Keep the public API")
    );
    assert_eq!(
        attempt.digests["prompt"],
        super::super::script::digest(b"Run request")
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("prompt.md")).unwrap(),
        "Build it"
    );
    drop(launches);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    assert!(records::journal(&paths).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Attempt { digests, .. } if digests["signs/build.md"] == super::super::script::digest(b"Keep the public API"))));
    ended.state = RunState::Active;
    ended.key = Some(Key::Integer(8));
    ended.input = Some(json!({"outcome":"work", "key":8}));
    update(&state, &ended);
    host.observed.borrow_mut().status = RunStatus::Running;
    fs::write(paths.signs(Some("build")), "Updated sign").unwrap();
    advance_ok(&state, &record.id, &host);
    let ctx: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["visit"], 2, "a key does not split the step's count");
    assert_eq!(ctx["visits"], 2);
    assert_eq!(ctx["prev_attempt"]["key"], 7);
    assert_eq!(
        host.launched.borrow()[1].prompt,
        "Build it\n\nUpdated sign\n\nRun request"
    );
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    ended.state = RunState::Active;
    update(&state, &ended);
    let mut capped = advance_ok(&state, &record.id, &host);
    assert_eq!(capped.state, RunState::Failed);
    assert_eq!(
        capped.input,
        Some(json!({
            "outcome":"cap", "step":"build", "bound":"max-visits",
            "visits":2, "max":2, "key":8, "routed_by":"work"
        }))
    );
    assert_eq!(host.launched.borrow().len(), 2);
    capped.visit_grant = 1;
    capped.state = RunState::Active;
    capped.terminal_at = None;
    capped.outcome = None;
    update(&state, &capped);
    host.observed.borrow_mut().status = RunStatus::Running;
    let extended = advance_ok(&state, &record.id, &host);
    assert_eq!(extended.visits["build"], 3);
}

#[test]
fn cap_preserves_counts_sequence_and_route_evidence() {
    let (_dir, state, mut record) = setup(STEP);
    record.key = Some(Key::Integer(7));
    record.visits.insert("build".into(), 2);
    update(&state, &record);
    let host = FakeHost::new();
    let capped = advance_ok(&state, &record.id, &host);
    assert_eq!(capped.state, RunState::Failed);
    assert!(capped.park.is_none());
    let input = json!({
        "outcome":"cap", "step":"build", "bound":"max-visits",
        "visits":2, "max":2, "key":7, "routed_by":null
    });
    assert_eq!(capped.input.as_ref(), Some(&input));
    assert_eq!(capped.visits, record.visits);
    assert_eq!(capped.next_seq, record.next_seq);
    assert!(host.launched.borrow().is_empty());
    assert!(
        records::journal(&state.workflow(&record.id))
            .unwrap()
            .iter()
            .any(|row| matches!(
                &row.event,
                JournalEvent::Route { outcome, input: routed }
                    if outcome == "cap" && routed == &input
            ))
    );
}

#[test]
fn visit_grant_admits_exactly_that_many_more_visits() {
    let (_dir, state, mut record) = setup(STEP);
    record.key = Some(Key::String("lint".into()));
    record.visits.insert("build".into(), 2);
    update(&state, &record);
    let host = FakeHost::new();
    let mut capped = advance_ok(&state, &record.id, &host);
    assert_eq!(capped.state, RunState::Failed);
    capped.visit_grant = 2;
    for count in 3..=4 {
        capped.state = RunState::Active;
        capped.terminal_at = None;
        capped.outcome = None;
        update(&state, &capped);
        host.observed.borrow_mut().status = RunStatus::Running;
        let live = advance_ok(&state, &record.id, &host);
        assert_eq!(live.visits["build"], count);
        host.observed.borrow_mut().status = RunStatus::Completed;
        capped = advance_ok(&state, &record.id, &host);
    }
    capped.state = RunState::Active;
    update(&state, &capped);
    let capped = advance_ok(&state, &record.id, &host);
    assert_eq!(capped.state, RunState::Failed);
    assert_eq!(host.launched.borrow().len(), 2);
    assert_eq!(
        capped.input,
        Some(json!({
            "outcome":"cap", "step":"build", "bound":"max-visits",
            "visits":4, "max":4, "key":"lint", "routed_by":null
        }))
    );
}

#[test]
fn signs_cannot_read_outside_the_node_writable_work_directory() {
    let (_dir, state, record) = setup(STEP);
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("private.md");
    fs::write(&secret, "Outside workflow work").unwrap();
    let paths = state.workflow(&record.id);
    fs::create_dir_all(paths.signs(None)).unwrap();
    std::os::unix::fs::symlink(secret, paths.signs(Some("build"))).unwrap();
    let host = FakeHost::new();
    let ended = advance_ok(&state, &record.id, &host);
    assert!(
        host.launched.borrow().is_empty(),
        "a sign cannot expose files outside workflow work"
    );
    assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Died);
}

#[test]
fn stay_survives_live_asks_and_limits_and_records_only_session_end() {
    use crate::store::workflow::{Asked, Asking, Limited};
    let (_dir, state, record) = setup(&STEP.replace("lifecycle='turn'", "lifecycle='stay'"));
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let now = jiff::Timestamp::now();
    live.attempt.as_mut().unwrap().started_at = now - std::time::Duration::from_secs(120);
    update(&state, &live);
    let since = now - std::time::Duration::from_secs(60);
    let seq = live.attempt.as_ref().unwrap().seq;
    for status in [RunStatus::Pending, RunStatus::Running] {
        let mut observed = host.observed.borrow_mut();
        observed.status = status;
        observed.ask = Some(Asking {
            since,
            ask: Asked {
                text: "Which approach?".into(),
                kind: "question".into(),
                handle: "@builder".into(),
            },
        });
        observed.limited = Some(Limited { reset_at: None });
        drop(observed);
        let live = advance_ok(&state, &record.id, &host);
        assert_eq!(
            live.state,
            RunState::Active,
            "stay must not park on {status:?}"
        );
        assert_eq!(live.attempt.as_ref().unwrap().seq, seq);
        assert_eq!(
            live.attempt.as_ref().unwrap().asking,
            host.observed.borrow().ask
        );
        assert!(live.prev_attempt.is_none());
        assert_eq!(host.stops.get(), 0);
    }
    host.observed.borrow_mut().ask = None;
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(live.state, RunState::Active);
    assert!(live.attempt.as_ref().unwrap().asking.is_none());
    assert!(live.attempt.as_ref().unwrap().asked_ms >= 60_000);
    assert!(live.prev_attempt.is_none());
    assert_eq!(host.stops.get(), 0);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    let previous = ended.prev_attempt.unwrap();
    assert_eq!(previous.outcome, AttemptOutcome::Ended);
    assert!(previous.asked.is_none());
    assert!(previous.limited.is_none());
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn stay_lifecycle_is_persisted_and_a_turn_omits_it() {
    for lifecycle in ["turn", "stay", "until"] {
        let (_dir, state, record) =
            setup(&STEP.replace("lifecycle='turn'", &format!("lifecycle='{lifecycle}'")));
        let host = FakeHost::new();
        let live = advance_ok(&state, &record.id, &host);
        assert_eq!(
            host.turn_observations.get(),
            usize::from(lifecycle == "until"),
            "only until observations request the leader turn"
        );
        let value = serde_json::to_value(live.attempt.as_ref().unwrap()).unwrap();
        assert_eq!(
            value.get("lifecycle").cloned(),
            (lifecycle != "turn").then(|| json!(lifecycle))
        );
        assert_eq!(value["visit"], 1);
        host.observed.borrow_mut().status = RunStatus::Completed;
        let ended = advance_ok(&state, &record.id, &host);
        assert_eq!(ended.prev_attempt.unwrap().visit, 1);
    }
}

#[test]
fn asking_fields_round_trip_and_an_unasked_attempt_omits_them() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let unasked = serde_json::to_value(&live).unwrap();
    assert!(unasked["attempt"].get("asking").is_none());
    assert!(unasked["attempt"].get("asked_ms").is_none());
    let decoded: RunRecord = serde_json::from_value(unasked.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), unasked);

    let mut value = unasked;
    value["attempt"]["asking"] = json!({
        "since":live.attempt.as_ref().unwrap().started_at,
        "text":"Approve?", "kind":"permission", "handle":"@builder"
    });
    value["attempt"]["asked_ms"] = json!(5400000);
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    update(&state, &decoded);
    let loaded = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(serde_json::to_value(loaded).unwrap(), value);
}

#[test]
fn answered_ask_resumes_the_same_attempt_until_turn_end() {
    use std::os::unix::fs::MetadataExt;
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let now = jiff::Timestamp::now();
    live.attempt.as_mut().unwrap().started_at = now - std::time::Duration::from_secs(120);
    update(&state, &live);
    let since = now - std::time::Duration::from_secs(60);
    host.observed.borrow_mut().ask = Some(records::Asking {
        since,
        ask: records::Asked {
            text: "Which approach?".into(),
            kind: "question".into(),
            handle: "@child".into(),
        },
    });
    let paths = state.workflow(&record.id);
    let journal = records::journal(&paths).unwrap();
    let held = advance_ok(&state, &record.id, &host);
    assert_eq!(held.state, RunState::Active);
    assert_eq!(
        held.attempt
            .as_ref()
            .unwrap()
            .asking
            .as_ref()
            .map(|ask| ask.since),
        Some(since)
    );
    let published = fs::File::open(&paths.record).unwrap();
    assert_eq!(advance_ok(&state, &record.id, &host), held);
    assert_eq!(
        published.metadata().unwrap().ino(),
        fs::metadata(&paths.record).unwrap().ino(),
        "a stable hold must not rewrite the record"
    );

    host.observed.borrow_mut().ask = Some(records::Asking {
        since: jiff::Timestamp::now(),
        ask: records::Asked {
            text: "Approve now?".into(),
            kind: "permission".into(),
            handle: "@reviewer".into(),
        },
    });
    let refreshed = advance_ok(&state, &record.id, &host);
    let asking = refreshed.attempt.as_ref().unwrap().asking.as_ref().unwrap();
    assert_eq!(asking.since, since);
    assert_eq!(asking.ask, host.observed.borrow().ask.as_ref().unwrap().ask);
    host.observed.borrow_mut().ask = None;
    let answered = advance_ok(&state, &record.id, &host);
    assert_eq!(answered.state, RunState::Active);
    let attempt = answered.attempt.as_ref().unwrap();
    assert!(attempt.asking.is_none());
    assert!(attempt.asked_ms >= 60_000);
    assert_eq!(attempt.seq, held.attempt.as_ref().unwrap().seq);
    assert!(answered.prev_attempt.is_none());
    assert_eq!(host.stops.get(), 0);
    assert_eq!(
        records::journal(&paths).unwrap(),
        journal,
        "holds are not attempt ends"
    );

    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert!(ended.attempt.is_none());
    let previous = ended.prev_attempt.unwrap();
    assert_eq!(previous.outcome, AttemptOutcome::Ended);
    assert!(previous.asked.is_none());
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn step_timeout_pauses_from_ask_open_and_rejudges_after_the_answer() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (open_seconds_ago, overdue) in [(5400, false), (1800, true), (10800, false)] {
        let (_dir, state, record) = setup(&format!("{STEP}timeout='1h'\n"));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        let now = jiff::Timestamp::now();
        let start = now - std::time::Duration::from_secs(7200);
        let since = now - std::time::Duration::from_secs(open_seconds_ago);
        live.attempt.as_mut().unwrap().started_at = start;
        update(&state, &live);
        host.observed.borrow_mut().ask = Some(records::Asking {
            since,
            ask: records::Asked {
                text: "Approve?".into(),
                kind: "permission".into(),
                handle: "@child".into(),
            },
        });
        let held = advance_ok(&state, &record.id, &host);
        let held_since = held
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.asking.as_ref())
            .map(|ask| ask.since);
        host.observed.borrow_mut().ask = None;
        let earliest = jiff::Timestamp::now();
        let answered = advance_ok(&state, &record.id, &host);
        let latest = jiff::Timestamp::now();
        let clamped = since.max(start);
        let credit_matches = answered.attempt.as_ref().map(|attempt| {
            let credit = i128::from(attempt.asked_ms);
            earliest.duration_since(clamped).as_millis() <= credit
                && credit <= latest.duration_since(clamped).as_millis()
        });
        actual.push((
            open_seconds_ago,
            held.state,
            held_since,
            answered.state,
            credit_matches,
            answered.prev_attempt.map(|previous| previous.outcome),
            host.stops.get(),
        ));
        expected.push((
            open_seconds_ago,
            RunState::Active,
            Some(clamped),
            if overdue {
                RunState::Done
            } else {
                RunState::Active
            },
            (!overdue).then_some(true),
            overdue.then_some(AttemptOutcome::Timeout),
            usize::from(overdue),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn stay_deadline_beats_completed_observation_and_cancel_stops() {
    for cause in ["deadline", "cancel"] {
        let (_dir, state, record) = setup(&STEP.replace("lifecycle='turn'", "lifecycle='stay'"));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        host.observed.borrow_mut().status = RunStatus::Completed;
        if cause == "cancel" {
            cancel(&state, &record.id, &host, None).unwrap();
        } else {
            live.deadline = Some("2000-01-01T00:00:00Z".parse().unwrap());
            update(&state, &live);
            advance_ok(&state, &record.id, &host);
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(
            ended.prev_attempt.unwrap().outcome,
            AttemptOutcome::Canceled
        );
        assert_eq!(host.stops.get(), 1);
    }
}

#[test]
fn terminal_runs_and_live_asks_limits_route_with_full_previous_attempt() {
    use crate::store::workflow::{Asked, Asking, ChildRef, Limited};
    for (status, expected) in [
        (RunStatus::Completed, Some(AttemptOutcome::Ended)),
        (RunStatus::Failed, Some(AttemptOutcome::Died)),
        (RunStatus::VerifyFailed, Some(AttemptOutcome::Died)),
        (RunStatus::TimedOut, Some(AttemptOutcome::Died)),
        (RunStatus::Canceled, Some(AttemptOutcome::Canceled)),
        (RunStatus::BudgetExceeded, Some(AttemptOutcome::Limited)),
        (RunStatus::Running, None),
        (RunStatus::Pending, None),
    ] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        live.attempt.as_mut().unwrap().head_start = Some("recorded-head".into());
        update(&state, &live);
        let reset = "2099-01-01T00:00:00Z".parse().unwrap();
        {
            let mut observed = host.observed.borrow_mut();
            observed.status = status;
            observed.children.stopped.push(ChildRef {
                handle: "child".into(),
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                team: None,
            });
            observed.children.live.push(ChildRef {
                handle: "peer".into(),
                kind: crate::ids::AgentKind::new_unchecked("codex"),
                team: None,
            });
            observed.ask = Some(Asking {
                since: jiff::Timestamp::now(),
                ask: Asked {
                    text: "Which approach?".into(),
                    kind: "question".into(),
                    handle: "@child".into(),
                },
            });
            if matches!(status, RunStatus::Pending | RunStatus::BudgetExceeded) {
                observed.limited = Some(Limited {
                    reset_at: Some(reset),
                });
            }
        }
        let observed = Rc::clone(&host.observed);
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            observed.borrow_mut().ask = None;
            observed.borrow_mut().limited = None;
        }));
        let ended = advance_ok(&state, &record.id, &host);
        let Some(expected) = expected else {
            assert_eq!(ended.state, RunState::Active);
            assert_eq!(host.stops.get(), 0);
            assert!(ended.prev_attempt.is_none());
            let asking = ended.attempt.as_ref().unwrap().asking.as_ref().unwrap();
            assert_eq!(asking.ask.handle, "@child");
            assert_eq!(asking.ask.text, "Which approach?");
            continue;
        };
        assert_eq!(host.stops.get(), 1, "every agent end stops the tree");
        let prev = ended
            .prev_attempt
            .as_ref()
            .expect("agent ends populate previous attempt");
        assert_eq!(prev.outcome, expected);
        assert_eq!(prev.handle.as_deref(), Some("@builder"));
        assert!(prev.run_id.is_some());
        assert_eq!(prev.cost_usd, Some(0.25));
        assert_eq!(prev.head_start.as_deref(), Some("recorded-head"));
        assert_eq!(prev.children.as_ref().unwrap().stopped[0].handle, "child");
        assert_eq!(prev.children.as_ref().unwrap().live[0].handle, "peer");
        assert_eq!(ended.state, RunState::Done);
        assert!(prev.asked.is_none());
        if status == RunStatus::Pending {
            assert_eq!(prev.limited.as_ref().unwrap().reset_at, Some(reset));
        }
        if status == RunStatus::BudgetExceeded {
            assert_eq!(prev.limited.as_ref().unwrap().reset_at, None);
        }
    }
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(
        ended.state,
        RunState::Done,
        "a completion whose wake preceded launch publication must not be lost"
    );
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn asked_edge_routes_and_launch_failures_count_as_attempts() {
    for outcome in [
        AttemptOutcome::Died,
        AttemptOutcome::Limited,
        AttemptOutcome::Asked,
    ] {
        let (_dir, state, record) = setup(&format!("{STEP}on.asked='@failed'\ntimeout='1h'\n"));
        let host = FakeHost::new();
        if outcome != AttemptOutcome::Asked {
            *host.failure.borrow_mut() = Some(LaunchFailure {
                outcome,
                reason: "cannot launch".into(),
            });
        }
        let mut ended = advance_ok(&state, &record.id, &host);
        if outcome == AttemptOutcome::Asked {
            let now = jiff::Timestamp::now();
            ended.attempt.as_mut().unwrap().started_at = now - std::time::Duration::from_secs(7200);
            update(&state, &ended);
            host.observed.borrow_mut().ask = Some(records::Asking {
                since: now - std::time::Duration::from_secs(5400),
                ask: records::Asked {
                    text: "Approve?".into(),
                    kind: "permission".into(),
                    handle: "@builder".into(),
                },
            });
            ended = advance_ok(&state, &record.id, &host);
        }
        assert_eq!(ended.prev_attempt.as_ref().unwrap().outcome, outcome);
        if outcome == AttemptOutcome::Asked {
            assert_eq!(
                serde_json::to_value(&ended.prev_attempt.as_ref().unwrap().asked).unwrap(),
                json!({"text":"Approve?", "kind":"permission", "handle":"@builder"})
            );
        }
        assert_eq!(ended.visits["build"], 1);
        assert_eq!(
            ended.state,
            if outcome == AttemptOutcome::Limited {
                RunState::Done
            } else {
                RunState::Failed
            }
        );
        if outcome != AttemptOutcome::Asked {
            assert!(records::journal(&state.workflow(&record.id)).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Attempt { reason:Some(reason), .. } if reason == "cannot launch")));
        }
    }
}

#[test]
fn occupancy_waits_merge_without_consuming_input_key_or_visits() {
    for takeover in [false, true] {
        let (_dir, state, mut record) = setup(&format!(
            "{STEP}admission.occupancy='{}'\n",
            if takeover { "takeover" } else { "wait" }
        ));
        record.key = Some(Key::Integer(7));
        record.input = Some(json!({"outcome":"work", "key":7}));
        update(&state, &record);
        let host = FakeHost::new();
        *host.occupancy.borrow_mut() = if takeover {
            Occupancy::Blocked(vec![(
                "sweeper".into(),
                crate::harness::takeover::OpenTurn::Working,
            )])
        } else {
            Occupancy::Occupied(vec!["sweeper".into()])
        };
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            waiting.wait.unwrap().outcome,
            if takeover { "takeover" } else { "occupied" }
        );
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(waiting.wait.unwrap().count, 2);
        assert_eq!(waiting.input, record.input);
        assert_eq!(waiting.key, record.key);
        assert!(waiting.visits.is_empty());
        assert!(host.launched.borrow().is_empty());
        *host.occupancy.borrow_mut() = if takeover {
            Occupancy::Occupied(vec!["sweeper".into()])
        } else {
            Occupancy::Free
        };
        let live = advance_ok(&state, &record.id, &host);
        assert!(live.wait.is_none());
        assert_eq!(live.visits["build"], 1);
        assert_eq!(host.evictions.get(), usize::from(takeover));
        assert!(
            records::journal(&state.workflow(&record.id))
                .unwrap()
                .iter()
                .any(|row| matches!(row.event, JournalEvent::Wait { count: 2, .. }))
        );
    }
}

#[test]
fn successful_eviction_admits_only_the_immediate_launch() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.occupancy='takeover'\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.stale_after_evict.set(true);
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(
        host.launched.borrow().len(),
        1,
        "successful eviction admits its immediate launch despite stale occupancy"
    );
    assert_eq!(live.visits["build"], 1);
    assert_eq!(host.evictions.get(), 1);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    ended.state = RunState::Active;
    update(&state, &ended);
    host.observed.borrow_mut().status = RunStatus::Running;
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.wait.unwrap().outcome, "takeover");
    assert_eq!(waiting.visits["build"], 1);
    assert_eq!(host.launched.borrow().len(), 1);
}

/// The first failed eviction waits with outcome `retry` and counts nothing; the next advance
/// stops the occupant again, and a second failure is the counted `died` attempt.
#[test]
fn eviction_failure_retries_once_then_is_a_counted_died_attempt() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.occupancy='takeover'\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.evict_error.set(true);
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.state, RunState::Active, "{waiting:#?}");
    let wait = waiting.wait.as_ref().expect("retry wait");
    assert_eq!(
        (wait.outcome.as_str(), wait.detail.as_deref()),
        ("retry", Some("occupant did not stop"))
    );
    assert!(waiting.attempt.is_none() && waiting.prev_attempt.is_none());
    assert!(!waiting.visits.contains_key("build"), "{waiting:#?}");
    assert!(waiting.launch_retry);
    assert_eq!(host.evictions.get(), 1);

    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Died);
    assert_eq!(ended.visits["build"], 1);
    assert_eq!(
        ended.state,
        RunState::Failed,
        "the failure routes through `invalid`"
    );
    assert_eq!(host.evictions.get(), 2);
    assert!(host.launched.borrow().is_empty());
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    let died = journal
        .iter()
        .filter(|entry| {
            matches!(
                entry.event,
                JournalEvent::Attempt {
                    outcome: AttemptOutcome::Died,
                    ..
                }
            )
        })
        .count();
    assert_eq!(died, 1, "{journal:#?}");
    assert!(
        journal.iter().any(|entry| matches!(
            &entry.event,
            JournalEvent::Wait { outcome, .. } if outcome == "retry"
        )),
        "{journal:#?}"
    );
}

/// A retry whose stop succeeds launches with no failure on record, and a step entered again
/// has its retry back.
#[test]
fn a_retried_eviction_launches_and_a_reentered_step_retries_again() {
    let (_dir, state, record) = setup(&format!(
        "{}admission.occupancy='takeover'\n",
        STEP.replace("then='@done'", "then='build'")
    ));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.evict_error.set(true);
    assert!(advance_ok(&state, &record.id, &host).launch_retry);
    host.evict_error.set(false);
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(host.launched.borrow().len(), 1);
    assert!(live.attempt.as_ref().unwrap().run.is_some(), "{live:#?}");
    assert!(
        live.prev_attempt.is_none() && live.wait.is_none(),
        "{live:#?}"
    );
    assert_eq!(live.visits["build"], 1);
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert!(
        !journal
            .iter()
            .any(|entry| matches!(entry.event, JournalEvent::Attempt { .. })),
        "{journal:#?}"
    );

    // The attempt ends and `then` enters the step again, whose eviction fails once more.
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.evict_error.set(true);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let again = advance_ok(&state, &record.id, &host);
    assert_eq!(
        again.prev_attempt.as_ref().unwrap().outcome,
        AttemptOutcome::Ended
    );
    assert_eq!(
        again.wait.as_ref().map(|wait| wait.outcome.as_str()),
        Some("retry"),
        "{again:#?}"
    );
    assert_eq!(again.visits["build"], 1);
    assert_eq!(host.launched.borrow().len(), 1);
}

/// A second advance between the host's failure and the engine's settlement neither launches
/// nor takes the retry; the settled wait still retries once.
#[test]
fn an_advance_during_a_failed_eviction_neither_launches_nor_spends_the_retry() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.occupancy='takeover'\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.evict_error.set(true);
    let rival = Rc::new(FakeHost::new());
    let (callback_state, id, callback_rival) = (state.clone(), record.id.clone(), rival.clone());
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        advance(&callback_state, &id, &*callback_rival).unwrap();
    }));
    let waiting = advance_ok(&state, &record.id, &host);
    assert!(rival.launched.borrow().is_empty() && rival.evictions.get() == 0);
    assert_eq!(
        waiting.wait.as_ref().map(|wait| wait.outcome.as_str()),
        Some("retry"),
        "{waiting:#?}"
    );
    assert!(waiting.prev_attempt.is_none(), "{waiting:#?}");
    host.on_launch.borrow_mut().take();
    host.evict_error.set(false);
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(host.launched.borrow().len(), 1);
    assert_eq!(live.visits["build"], 1);
}

/// Only an unstarted `died` launch of a fresh step retries: a provider limit and a continued
/// restart settle on their first failure as before.
#[test]
fn limited_and_continued_restart_launch_failures_are_not_retried() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    host.unstarted.set(true);
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Limited,
        reason: "budget".into(),
    });
    let limited = advance_ok(&state, &record.id, &host);
    assert_eq!(
        limited.prev_attempt.unwrap().outcome,
        AttemptOutcome::Limited
    );
    assert!(!limited.launch_retry);

    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let seq = live.attempt.as_ref().unwrap().seq;
    restart(
        &state,
        &record.id,
        Some((seq, "conversation".into())),
        &host,
    )
    .unwrap();
    host.unstarted.set(true);
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "history pool changed".into(),
    });
    let failed = advance_ok(&state, &record.id, &host);
    assert_eq!(failed.state, RunState::Failed, "{failed:#?}");
    assert!(!failed.launch_retry);
}

/// A host error carries no word that the launch started nothing, so it settles as a counted
/// `died` attempt on the first failure.
#[test]
fn a_host_error_is_a_counted_died_attempt_without_a_retry() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    host.host_error.set(true);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Died);
    assert_eq!(ended.visits["build"], 1);
    assert!(!ended.launch_retry && ended.wait.is_none());
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert!(
        !journal
            .iter()
            .any(|entry| matches!(entry.event, JournalEvent::Wait { .. })),
        "{journal:#?}"
    );
}

#[test]
fn a_free_checkout_is_read_again_once_the_launch_is_prepared() {
    let occupied = || Occupancy::Occupied(vec!["sweeper".into()]);
    let blocked = || {
        Occupancy::Blocked(vec![(
            "sweeper".into(),
            crate::harness::takeover::OpenTurn::Working,
        )])
    };
    for (takeover, arrived, wait) in [
        (true, occupied(), None),
        (false, occupied(), Some("occupied")),
        (false, blocked(), Some("takeover")),
        (true, blocked(), Some("takeover")),
    ] {
        let (_dir, state, record) = setup(&format!(
            "{STEP}admission.occupancy='{}'\n",
            if takeover { "takeover" } else { "wait" }
        ));
        let host = FakeHost::new();
        *host.arrives_at_launch.borrow_mut() = Some(arrived);
        let live = advance_ok(&state, &record.id, &host);
        let launched = usize::from(wait.is_none());
        assert_eq!(
            (host.launched.borrow().len(), host.evictions.get()),
            (launched, launched),
            "takeover={takeover} {wait:?}: an arrival after a free read is stopped or waited on"
        );
        let Some(wait) = wait else {
            continue;
        };
        // The late read waits as the first read would: no attempt, no visit, no failure.
        assert_eq!(live.state, RunState::Active, "{live:#?}");
        assert_eq!(
            live.wait.as_ref().map(|wait| wait.outcome.as_str()),
            Some(wait)
        );
        assert!(
            live.attempt.is_none() && live.prev_attempt.is_none(),
            "{live:#?}"
        );
        assert!(
            live.visits
                .get(&live.node)
                .is_none_or(|visits| *visits == 0),
            "{live:#?}"
        );
    }
}

#[test]
fn sharing_step_launches_beside_any_occupant_without_reading_or_stopping_it() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.occupancy='share'\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Blocked(vec![(
        "sweeper".into(),
        crate::harness::takeover::OpenTurn::Working,
    )]);
    let live = advance_ok(&state, &record.id, &host);
    assert!(live.wait.is_none(), "{live:#?}");
    assert_eq!(host.launched.borrow().len(), 1);
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    restart(&state, &record.id, None, &host).unwrap();
    let restarted = advance_ok(&state, &record.id, &host);
    assert!(restarted.wait.is_none(), "{restarted:#?}");
    assert_eq!(host.launched.borrow().len(), 2);
    assert_eq!(
        (host.occupancies.get(), host.evictions.get()),
        (0, 0),
        "a sharing step never reads occupancy or stops an occupant"
    );
    assert!(
        records::journal(&state.workflow(&record.id))
            .unwrap()
            .iter()
            .all(|row| !matches!(row.event, JournalEvent::Wait { .. }))
    );
}

#[test]
fn timeout_deadline_and_cancel_stop_before_settlement() {
    let cleared = Rc::new(RefCell::new(Vec::new()));
    for cause in ["timeout", "deadline", "cancel"] {
        let (_dir, state, record) = setup(&format!("{STEP}timeout='1s'\n"));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        if cause != "timeout" {
            let asking = records::Asking {
                since: live.attempt.as_ref().unwrap().started_at,
                ask: records::Asked {
                    text: "Approve?".into(),
                    kind: "permission".into(),
                    handle: "@builder".into(),
                },
            };
            live.attempt.as_mut().unwrap().asking = Some(asking.clone());
            host.observed.borrow_mut().ask = Some(asking);
            update(&state, &live);
        }
        let stop_state = state.clone();
        let stop_id = record.id.clone();
        let cleared_on_stop = Rc::clone(&cleared);
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            let _guard = WorkspaceLock::acquire(&stop_state.workspace_lock).unwrap();
            let current = records::load(&stop_state.workflow(&stop_id)).unwrap();
            assert_eq!(current.state, RunState::Active);
            assert!(current.attempt.as_ref().unwrap().ending.is_some());
            cleared_on_stop
                .borrow_mut()
                .push((cause, current.attempt.as_ref().unwrap().asking.is_none()));
            assert_eq!(
                current.next_seq, 2,
                "no successor is admitted before stop returns"
            );
        }));
        if cause == "cancel" {
            assert!(cancel(&state, &record.id, &host, None).unwrap());
            assert!(!cancel(&state, &record.id, &host, None).unwrap());
        } else {
            if cause == "timeout" {
                live.attempt.as_mut().unwrap().started_at =
                    jiff::Timestamp::now() - std::time::Duration::from_secs(7200);
            } else {
                live.deadline = Some(jiff::Timestamp::now() - std::time::Duration::from_secs(1));
            }
            update(&state, &live);
            advance_ok(&state, &record.id, &host);
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(host.stops.get(), 1);
        assert!(ended.attempt.is_none());
        assert_eq!(
            ended.prev_attempt.unwrap().outcome,
            if cause == "timeout" {
                AttemptOutcome::Timeout
            } else {
                AttemptOutcome::Canceled
            }
        );
        assert_eq!(
            ended.state,
            match cause {
                "timeout" => RunState::Done,
                "deadline" => RunState::Failed,
                _ => RunState::Canceled,
            }
        );
    }
    assert_eq!(
        *cleared.borrow(),
        vec![("timeout", true), ("deadline", true), ("cancel", true)]
    );
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let launch_state = state.clone();
    let launch_id = record.id.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        assert!(cancel(&launch_state, &launch_id, &FakeHost::new(), None).unwrap());
        let pending = records::load(&launch_state.workflow(&launch_id)).unwrap();
        assert!(pending.attempt.is_some());
        assert!(cancel(&launch_state, &launch_id, &FakeHost::new(), None).unwrap());
        assert_eq!(
            records::load(&launch_state.workflow(&launch_id)).unwrap(),
            pending,
            "repeating cancel during launch must not restage ending evidence"
        );
    }));
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Canceled);
    assert_eq!(host.stops.get(), 1);
    assert!(ended.prev_attempt.unwrap().run_id.is_some());
}

#[test]
fn dead_launcher_stops_the_unpublished_supervised_run() {
    for canceling in [false, true] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        let run = live.attempt.as_mut().unwrap().run.take().unwrap();
        live.attempt.as_mut().unwrap().host = Host {
            pid: u32::MAX,
            start: None,
        };
        let mut orphan = crate::store::run::RunRecord::new(
            state.workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            "Build it".into(),
            live.scope.path.clone(),
        );
        orphan.run_id = run.run_id.clone();
        orphan.agent_name = Some(run.handle.trim_start_matches('@').into());
        orphan.workflow = Some(host.launched.borrow()[0].workflow.clone());
        crate::harness::run::create(&state, &orphan).unwrap();
        update(&state, &live);
        let ended = if canceling {
            cancel(&state, &record.id, &host, None).unwrap();
            records::load(&state.workflow(&record.id)).unwrap()
        } else {
            advance_ok(&state, &record.id, &host)
        };
        assert_eq!(
            host.stops.get(),
            1,
            "the durable orphan must stop before settlement"
        );
        let previous = ended.prev_attempt.unwrap();
        assert_eq!(previous.run_id, Some(run.run_id));
        assert_eq!(previous.handle, Some(run.handle));
        assert!(ended.attempt.is_none());
    }
}

#[test]
fn dead_launcher_refuses_an_unpublished_run_without_a_handle() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let run = live.attempt.as_mut().unwrap().run.take().unwrap();
    live.attempt.as_mut().unwrap().host = Host {
        pid: u32::MAX,
        start: None,
    };
    let mut orphan = crate::store::run::RunRecord::new(
        state.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        "Build it".into(),
        live.scope.path.clone(),
    );
    orphan.run_id = run.run_id;
    orphan.agent_name = None;
    orphan.workflow = Some(host.launched.borrow()[0].workflow.clone());
    crate::harness::run::create(&state, &orphan).unwrap();
    update(&state, &live);
    let error = advance(&state, &record.id, &host).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("workflow attempt run has no handle"),
        "the orphan without a handle must refuse: {error}"
    );
    assert_eq!(
        host.stops.get(),
        0,
        "nothing can be stopped without a handle"
    );
}

#[test]
fn live_stopper_is_left_alone_and_dead_stopper_is_retaken() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let attempt = live.attempt.as_mut().unwrap();
    attempt.ending = Some(records::Ending {
        outcome: AttemptOutcome::Timeout,
        cause: records::EndCause::Timeout,
        route: None,
        reason: None,
    });
    update(&state, &live);
    assert_eq!(advance_ok(&state, &record.id, &host), live);
    assert_eq!(host.stops.get(), 0);
    live.attempt.as_mut().unwrap().host = Host {
        pid: u32::MAX,
        start: None,
    };
    let spec = toml::from_str(STEP).unwrap();
    let paths = state.workflow(&record.id);
    let mut staged = super::super::state::Transition {
        record: live,
        spec: &spec,
        paths: &paths,
        now: jiff::Timestamp::now(),
        entries: Vec::new(),
    };
    let mut observed = host.observed.borrow().clone();
    observed.children.stopped.push(records::ChildRef {
        handle: "already-stopped".into(),
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        team: None,
    });
    staged.stage_agent_end(
        staged
            .record
            .attempt
            .as_ref()
            .unwrap()
            .ending
            .clone()
            .unwrap(),
        &observed,
    );
    let live = staged.record;
    update(&state, &live);
    let ended = advance_ok(&state, &record.id, &host);
    let previous = ended.prev_attempt.unwrap();
    assert_eq!(previous.outcome, AttemptOutcome::Timeout);
    assert!(
        previous
            .children
            .unwrap()
            .stopped
            .iter()
            .any(|child| child.handle == "already-stopped"),
        "a dead stopper must retain staged child evidence"
    );
    assert_eq!(host.stops.get(), 1);
    assert_eq!(host.launched.borrow().len(), 1);
}

fn judge_fixture() -> (Definition, crate::disk::paths::WorkflowPaths, RunRecord) {
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='judge'\n[judge.judge]\nagent='claude'\nsummary='Review the work'\nprompt-file='judge.md'\non.ok='@done'\n").unwrap();
    let state = StatePaths::for_project_root_under(
        std::path::Path::new("/scratch"),
        std::path::Path::new("/project"),
    )
    .unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":crate::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"test","spec_dir":"/spec","spec_digest":"test",
        "scope":{"kind":"worktree","path":"/project"},"project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z","state":"active","node":"judge","entered_at":"2026-01-01T00:00:00Z",
        "deadline_fired":false,"next_seq":2,"visits":{},"visit_grant":0,
        "attempt":{"kind":"judge","seq":1,"node":"judge","started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":null}}
    })).unwrap();
    (spec, state.workflow(&record.id), record)
}

fn evaluation(
    object: Result<Value, String>,
    failure: &'static str,
) -> super::super::script::Evaluation {
    super::super::script::Evaluation {
        object,
        failure,
        stderr_tail: None,
        limited: None,
        cost_usd: Some(0.1),
    }
}

#[test]
fn judge_routes_valid_objects_and_retries_died_or_invalid_once_then_routes_invalid() {
    for failure in ["ok", "died", "invalid", "canceled", "limited"] {
        let (spec, paths, record) = judge_fixture();
        let mut t = super::super::state::Transition {
            now: record.entered_at,
            record,
            spec: &spec,
            paths: &paths,
            entries: Vec::new(),
        };
        let reset = "2026-01-02T00:00:00Z".parse().unwrap();
        for turn in 0..2 {
            let mut result = evaluation(
                match failure {
                    "ok" => Ok(json!({"outcome":"ok"})),
                    "invalid" => Ok(json!({"outcome":"not-routed"})),
                    _ => Err("judge failed".into()),
                },
                failure,
            );
            if failure == "limited" {
                result.limited = Some(records::Limited {
                    reset_at: Some(reset),
                });
            }
            t.finish_judge(1, result).unwrap();
            let evaluation = t
                .entries
                .iter()
                .rev()
                .find(|entry| matches!(entry.event, JournalEvent::Evaluation { .. }))
                .unwrap();
            assert_eq!(
                serde_json::to_value(&evaluation.event).unwrap()["origin"],
                if failure == "ok" { "check" } else { "engine" }
            );
            if failure == "ok" {
                assert_eq!(
                    t.record.state,
                    RunState::Done,
                    "judge objects route through check edges"
                );
                break;
            }
            assert!(
                t.record.attempt.is_none(),
                "the hosted evaluation must settle"
            );
            if failure == "limited" {
                assert_eq!(t.record.recheck_at, Some(reset));
                assert_eq!(t.record.check_retry, 0);
            } else if failure == "canceled" {
                assert_eq!(t.record.check_retry, 0);
            } else if turn == 0 {
                assert_eq!(t.record.check_retry, 1);
                assert_eq!(t.record.state, RunState::Active);
                assert_eq!(
                    t.record.check_retry_context.as_ref().unwrap()["outcome"],
                    failure
                );
                assert!(t.record.check_retry_context.as_ref().unwrap()["reason"].is_string());
            } else {
                assert_eq!(t.record.state, RunState::Failed);
                assert!(t.record.park.is_none());
            }
            t.record.attempt = Some(serde_json::from_value(json!({"kind":"judge","seq":1,"node":"judge","started_at":t.now,"host":{"pid":123,"start":null},"retry":{"outcome":failure,"reason":"judge failed"}})).unwrap());
        }
    }
    let (mut spec, paths, mut record) = judge_fixture();
    spec.edit(|spec| {
        let judge = spec.checks.get_mut("judge").unwrap();
        judge.on.insert("again".into(), "@wait".into());
        judge.timeout = Some("1d".into());
    });
    record.check_retry = 1;
    record.check_retry_context = Some(json!({"outcome":"died", "reason":"no object"}));
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.finish_judge(1, evaluation(Ok(json!({"outcome":"again"})), "invalid"))
        .unwrap();
    assert_eq!(t.record.check_retry, 1, "a wait stays at the same check");
    assert!(
        t.record.check_retry_context.is_none(),
        "a successful object ends retry context"
    );
}

#[test]
fn abandoned_checks_record_engine_origin() {
    for outcome in ["canceled", "timeout"] {
        for published in [false, true] {
            let (spec, paths, mut record) = judge_fixture();
            let attempt = record.attempt.as_mut().unwrap();
            if published {
                attempt.digests.insert("judge.md".into(), "digest".into());
                attempt.retry = Some(json!({"outcome":"died", "reason":"no object"}));
            }
            let digests = attempt.digests.clone();
            let retry = attempt.retry.clone();
            let mut t = super::super::state::Transition {
                now: record.entered_at,
                record,
                spec: &spec,
                paths: &paths,
                entries: Vec::new(),
            };
            t.abandon_check(outcome);
            let entry = serde_json::to_value(&t.entries[0]).unwrap();
            assert_eq!(entry["event"], "evaluation");
            assert_eq!(entry["outcome"], outcome);
            assert_eq!(entry["origin"], "engine");
            assert_eq!(entry["digests"], json!(digests));
            assert_eq!(entry["retry"], json!(retry));
        }
    }
}

#[test]
fn invalid_checks_record_engine_origin() {
    let (spec, paths, record) = judge_fixture();
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.finish_check(1, Err("bad JSON".into()), None).unwrap();
    let entry = serde_json::to_value(&t.entries[0]).unwrap();
    assert_eq!(entry["event"], "evaluation");
    assert_eq!(entry["origin"], "engine");
}
