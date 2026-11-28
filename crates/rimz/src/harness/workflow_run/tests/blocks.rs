//! Each block as a step: the immediate blocks, agents, teams, called workflows, the starts and
//! their fences.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Value, json};

use super::super::room::{Admit, Ending as LaunchEnding, Launched, Look, Pin, Room};
use super::*;
use crate::store::workflow_run::Hold;

const AGENT: &str = "await agent({ profile: 'opus', prompt: 'build' })";

#[test]
fn now_is_answered_with_the_rooms_time() {
    let h = Harness::new();
    h.start(script("return await now()"));
    assert_eq!(h.done(), json!(h.fake.now().as_millisecond()));
}

#[test]
fn two_advances_at_once_answer_a_now_once() {
    let h = Harness::new();
    let (state, room, id) = (h.state.clone(), Rc::clone(&h.room), h.id.clone());
    let inner = Rc::clone(&h.fake);
    h.fake.hook("now", move || {
        let due = store::load(&state.workflow_run(&id))
            .ok()
            .flatten()
            .is_some_and(|record| {
                record
                    .open
                    .get(&1)
                    .is_some_and(|step| step.work == Work::Due)
            });
        if due {
            inner.later(jiff::SignedDuration::from_secs(5));
            advance(&state, &room, &id).unwrap();
        }
        due
    });
    h.start(script("const at = await now()\nreturn at"));
    let lines = h.lines();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(Settled::Result(h.done()), lines[0].settled);
}

#[test]
fn a_schedule_is_booked_and_answered_with_its_id() {
    let h = Harness::new();
    h.start(script(
        "return await schedule({ name: 'nightly', trigger: 'in', value: '1h', tool: 'agent', arguments: { profile: 'opus', prompt: 'p' } })",
    ));
    assert_eq!(h.done(), json!({ "id": "booking-nightly" }));
}

#[test]
fn a_booking_the_room_refuses_fails_the_call_invalid() {
    let h = Harness::new();
    h.fake.refuse_bookings("the name is another starter's");
    h.start(script(
        "try { await schedule({ name: 'n', trigger: 'in', value: '1h', tool: 'agent', arguments: { profile: 'o', prompt: 'p' } }) } catch (e) { return [e.cause, e.reason, e.step] }",
    ));
    assert_eq!(
        h.done(),
        json!(["invalid", "the name is another starter's", 1])
    );
}

#[test]
fn a_crash_between_a_booking_and_its_line_books_again_under_the_same_name() {
    let h = Harness::new();
    h.fake.crash_at("book", 1, true);
    h.start(script(
        "return await schedule({ name: 'n', trigger: 'in', value: '1h', tool: 'agent', arguments: { profile: 'o', prompt: 'p' } })",
    ));
    assert!(h.record().ended.is_none());
    h.advance();
    assert_eq!(h.done(), json!({ "id": "booking-n" }));
    let names: Vec<Value> = h
        .fake
        .bookings()
        .iter()
        .map(|booking| booking["name"].clone())
        .collect();
    assert_eq!(names, [json!("n"), json!("n")]);
    assert_eq!(h.lines().len(), 1);
}

#[test]
fn a_watch_answers_with_the_readings_of_the_look_that_read_it_true_in_one_line() {
    let h = Harness::new();
    h.start(script("return await watch({ when: 'pr=merged' })"));
    assert_eq!(h.work(1), Work::Watch);
    h.advance();
    h.fake.set_look(
        "pr=merged",
        Look {
            truth: Some(true),
            readings: BTreeMap::from([
                ("pr".to_owned(), Some("merged".to_owned())),
                ("ci".to_owned(), None),
            ]),
        },
    );
    h.advance();
    assert_eq!(
        h.done(),
        json!({ "readings": { "ci": null, "pr": "merged" } })
    );
    assert!(h.fake.calls("look") >= 3);
    assert_eq!(h.lines().len(), 1);
}

#[test]
fn an_agent_that_completed_answers_its_last_message_and_handle() {
    let h = Harness::new();
    h.start(script(
        "return await agent({ profile: 'opus', name: 'builder', prompt: 'build' })",
    ));
    let launch = &h.fake.live()[0];
    assert_eq!(
        launch.pin,
        Pin {
            run: h.id.clone(),
            step: 1,
            attempt: 1
        }
    );
    h.fake.end(&launch.id, completed("built it"));
    h.advance();
    assert_eq!(
        h.done(),
        json!({ "report": "built it", "handle": "builder" })
    );
    assert_eq!(h.record().launches, 1);
}

#[test]
fn an_agent_with_a_schema_answers_what_it_handed_in_or_fails_invalid() {
    let body = "try { return await agent({ profile: 'judge', prompt: 'p', schema: { type: 'object' } }) } catch (e) { return [e.cause, e.reason, e.handle] }";
    let h = Harness::new();
    h.start(script(body));
    let id = h.fake.live()[0].id.clone();
    h.fake.end(
        &id,
        Some(LaunchEnding::Completed {
            message: "m".to_owned(),
            answer: Some(json!({ "outcome": "pass" })),
        }),
    );
    h.advance();
    assert_eq!(h.done(), json!({ "outcome": "pass" }));

    let h = Harness::new();
    h.start(script(body));
    let id = h.fake.live()[0].id.clone();
    h.fake.end(&id, completed("no answer"));
    h.advance();
    assert_eq!(
        h.done(),
        json!([
            "invalid",
            "the agent ended without handing in the answer its schema asks for",
            "agent"
        ])
    );
}

#[test]
fn an_agent_that_failed_or_was_canceled_fails_died_at_once_with_its_handle() {
    for (ending, reason) in [
        (
            LaunchEnding::Failed("the provider crashed".to_owned()),
            "the agent failed: the provider crashed",
        ),
        (
            LaunchEnding::Canceled("the user stopped it".to_owned()),
            "the agent was canceled: the user stopped it",
        ),
    ] {
        let h = Harness::new();
        h.start(script(&format!(
            "try {{ {AGENT} }} catch (e) {{ return [e.cause, e.reason, e.handle, e.step] }}"
        )));
        let id = h.fake.live()[0].id.clone();
        h.fake.end(&id, Some(ending));
        h.advance();
        assert_eq!(h.done(), json!(["died", reason, "agent", 1]));
        assert_eq!(
            h.fake.launched().len(),
            1,
            "an ending is never started again"
        );
    }
}

#[test]
fn an_agent_gone_with_no_ending_is_started_three_times_then_fails_died() {
    let h = Harness::new();
    h.start(script(&format!(
        "try {{ {AGENT} }} catch (e) {{ return [e.cause, e.reason] }}"
    )));
    for attempt in 1..=3 {
        let live = h.fake.live();
        assert_eq!(live.len(), 1, "attempt {attempt}");
        assert_eq!(live[0].pin.attempt, attempt);
        h.fake.end(&live[0].id, None);
        h.advance();
    }
    assert_eq!(
        h.done(),
        json!(["died", "its work was gone with no ending after 3 starts"])
    );
    assert_eq!(h.record().launches, 3);
}

#[test]
fn a_launch_that_returns_to_a_record_that_no_longer_holds_it_is_stopped() {
    let h = Harness::new();
    let (state, room, id) = (h.state.clone(), Rc::clone(&h.room), h.id.clone());
    h.fake.hook("launch", move || {
        cancel(&state, &room, &id).unwrap();
        true
    });
    h.start(script(AGENT));
    assert_eq!(h.ending(), Some(Ending::Canceled));
    assert_eq!(h.fake.stopped(), ["L1"]);
    assert!(h.fake.live().is_empty());
}

#[test]
fn a_restored_agent_is_taken_up_by_its_launch_id() {
    let h = Harness::new();
    h.fake.crash_at("launch", 1, true);
    assert!(
        h.start(script(
            "return (await agent({ profile: 'opus', prompt: 'p' })).report"
        ))
        .is_none()
    );
    assert!(matches!(h.work(1), Work::Starting { .. }));
    // The room went down with the agent, and a rebirth brought it back under its pin.
    h.fake.end("L1", None);
    h.fake.restore(
        Pin {
            run: h.id.clone(),
            step: 1,
            attempt: 1,
        },
        "L1",
    );
    h.advance();
    assert_eq!(
        h.work(1),
        Work::Launch {
            launches: vec!["L1".to_owned()],
            handle: Some("agent".to_owned())
        }
    );
    h.fake.end("L1", completed("restored and done"));
    h.advance();
    assert_eq!(h.done(), json!("restored and done"));
    assert_eq!(h.fake.launched().len(), 1);
}

#[test]
fn a_restored_agent_of_an_older_attempt_is_stopped() {
    let h = Harness::new();
    h.start(script(AGENT));
    h.fake.end("L1", None);
    h.advance();
    assert_eq!(h.fake.live()[0].pin.attempt, 2);
    h.fake.restore(
        Pin {
            run: h.id.clone(),
            step: 1,
            attempt: 1,
        },
        "L1",
    );
    h.advance();
    assert_eq!(h.fake.stopped(), ["L1"]);
    let live: Vec<String> = h.fake.live().into_iter().map(|launch| launch.id).collect();
    assert_eq!(live, ["L2"]);
}

#[test]
fn a_launch_whose_outcome_is_unknown_is_found_by_its_pin() {
    let h = Harness::new();
    h.fake
        .answer_next_launch(Launched::Unknown("the launcher timed out".to_owned()));
    h.start(script(AGENT));
    assert!(
        matches!(h.work(1), Work::Launch { ref launches, .. } if launches == &["L1".to_owned()])
    );
    assert_eq!(h.fake.launched().len(), 1);
}

#[test]
fn a_launch_the_room_refuses_fails_invalid_and_stays_counted() {
    let h = Harness::new();
    h.fake
        .answer_next_launch(Launched::Refused("no profile is named opus".to_owned()));
    h.start(script(&format!(
        "try {{ {AGENT} }} catch (e) {{ return [e.cause, e.reason] }}"
    )));
    assert_eq!(h.done(), json!(["invalid", "no profile is named opus"]));
    assert_eq!(h.record().launches, 1);
}

#[test]
fn a_team_done_with_seats_alive_answers_the_leaders_message_and_stops_its_seats() {
    let h = Harness::new();
    h.start(script(
        "return await team({ team: 'crew', prompt: 'p', worktree: 'w' })",
    ));
    assert_eq!(h.fake.live().len(), 2);
    h.fake.board_done(1, 1, "the crew shipped it");
    h.advance();
    assert_eq!(h.done(), json!({ "report": "the crew shipped it" }));
    assert!(h.fake.live().is_empty());
    assert_eq!(h.fake.stopped(), ["L1", "L2"]);
}

#[test]
fn a_team_whose_seats_ended_without_done_fails_died_and_one_gone_starts_again() {
    let h = Harness::new();
    h.start(script("try { await team({ team: 'crew', prompt: 'p', worktree: 'w' }) } catch (e) { return [e.cause, e.reason] }"));
    h.fake.end("L1", None);
    h.fake.end("L2", None);
    h.advance();
    assert_eq!(
        h.fake
            .live()
            .iter()
            .map(|launch| launch.pin.attempt)
            .collect::<Vec<_>>(),
        [2, 2]
    );
    h.fake.end("L3", completed("left"));
    h.fake.end("L4", None);
    h.advance();
    assert_eq!(
        h.done(),
        json!([
            "died",
            "the team's seats ended and its board never read done"
        ])
    );
}

#[test]
fn launches_past_the_parallel_cap_wait_their_turn_lowest_number_first() {
    let h = Harness::new();
    h.start(script(
        "return await pipeline([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], lane => agent({ profile: 'opus', prompt: 'lane ' + lane }))",
    ));
    assert_eq!(h.fake.live().len(), 8);
    let held = |h: &Harness| -> Vec<u64> {
        h.record()
            .open
            .values()
            .filter(|step| {
                matches!(
                    step.work,
                    Work::Held {
                        hold: Hold::Parallel,
                        ..
                    }
                )
            })
            .map(|step| step.number)
            .collect()
    };
    assert_eq!(held(&h), [9, 10]);
    h.fake.end("L5", completed("5"));
    h.advance();
    assert_eq!(held(&h), [10]);
    assert_eq!(h.fake.live().last().unwrap().pin.step, 9);
}

#[test]
fn a_room_hold_waits_and_fails_capacity_at_give_up_and_skip_capacity_passes_it() {
    let h = Harness::new();
    h.fake
        .set_admit(Admit::Hold("5h headroom is 3%".to_owned()));
    let mut input = h.input(script(&format!(
        "try {{ {AGENT} }} catch (e) {{ return [e.cause, e.reason] }}"
    )));
    input.give_up = Some("1h".to_owned());
    let status = h.start_with(input).unwrap();
    assert!(
        matches!(h.work(1), Work::Held { hold: Hold::Room { ref reason }, .. } if reason == "5h headroom is 3%")
    );
    assert_eq!(status.next, h.record().give_up);
    h.fake.later(jiff::SignedDuration::from_hours(2));
    h.advance();
    assert_eq!(
        h.done(),
        json!([
            "capacity",
            "the room held the launch past its give_up: 5h headroom is 3%"
        ])
    );

    let h = Harness::new();
    h.fake
        .set_admit(Admit::Hold("5h headroom is 3%".to_owned()));
    h.start(script(
        "return await workflow({ source: 'script', value: \"return (await agent({ profile: 'opus', prompt: 'p' })).report\", skip_capacity: true })",
    ));
    assert_eq!(
        h.fake.live().len(),
        1,
        "skip_capacity of the enclosing call passes the room's hold"
    );
}

#[test]
fn a_refusal_from_admit_fails_the_call_invalid() {
    let h = Harness::new();
    h.fake
        .set_admit(Admit::Refuse("profile opus is unknown".to_owned()));
    h.start(script(&format!(
        "try {{ {AGENT} }} catch (e) {{ return [e.cause, e.reason] }}"
    )));
    assert_eq!(h.done(), json!(["invalid", "profile opus is unknown"]));
    assert_eq!(h.record().launches, 0);
}

#[test]
fn a_called_workflow_runs_in_the_run_with_no_line_of_its_own() {
    let h = Harness::new();
    let child = h.workflow("child", "const at = await now()\nreturn { at, n: args.n }");
    h.fake.install("child", &child);
    h.start(script(
        "return await workflow({ source: 'name', value: 'child', args: { n: 2 } })",
    ));
    assert_eq!(
        h.done(),
        json!({ "at": h.fake.now().as_millisecond(), "n": 2 })
    );
    let numbers: Vec<(u64, String)> = h
        .lines()
        .into_iter()
        .map(|line| (line.number, line.block))
        .collect();
    assert_eq!(numbers, [(2, "now".to_owned())]);
}

#[test]
fn a_workflow_calls_timeout_drops_the_steps_under_it_and_stops_their_work() {
    let h = Harness::new();
    h.start(script(
        "try { await workflow({ source: 'script', value: \"await agent({ profile: 'opus', prompt: 'p' })\", timeout: '1m' }) } catch (e) { return [e.cause, e.step] }",
    ));
    assert_eq!(h.fake.live().len(), 1);
    h.fake.later(jiff::SignedDuration::from_mins(2));
    h.advance();
    assert_eq!(h.done(), json!(["timeout", 1]));
    assert_eq!(h.fake.stopped(), ["L1"]);
    assert_eq!(h.lines().len(), 1);
}

#[test]
fn a_step_still_open_when_its_workflow_call_returns_is_stopped() {
    let h = Harness::new();
    h.start(script(
        "return await workflow({ source: 'script', value: \"agent({ profile: 'opus', prompt: 'p' })\\nawait now()\\nreturn 'early'\" })",
    ));
    assert_eq!(h.done(), json!("early"));
    assert_eq!(h.fake.stopped(), ["L1"]);
}

#[test]
fn a_workflow_call_whose_read_is_refused_fails_invalid_at_its_number_and_the_refusal_is_kept() {
    let h = Harness::new();
    h.start(script(
        "try { await workflow({ source: 'name', value: 'missing' }) } catch (e) { return [e.cause, e.reason, e.step] }",
    ));
    assert_eq!(
        h.done(),
        json!(["invalid", "no installed workflow is named `missing`", 1])
    );
    let sealed = store::sealed(&h.state.workflow_run(&h.id).files(1).join("1"))
        .unwrap()
        .unwrap();
    assert_eq!(
        sealed.refusal.as_deref(),
        Some("no installed workflow is named `missing`")
    );
}

#[test]
fn a_called_workflow_reads_its_own_files_and_a_relative_path_against_its_callers_directory() {
    let h = Harness::new();
    let root = h.workflow(
        "flow",
        "return await workflow({ source: 'path', value: 'inner' })",
    );
    std::fs::create_dir_all(root.join("inner")).unwrap();
    std::fs::write(
        root.join("inner/workflow.js"),
        format!("{META}return file('note.md')"),
    )
    .unwrap();
    std::fs::write(root.join("inner/note.md"), "inner note").unwrap();
    h.start(Origin::Path(root));
    assert_eq!(h.done(), json!("inner note"));
}

#[test]
fn a_called_directory_with_no_script_fails_its_call_invalid() {
    let h = Harness::new();
    let dir = h.workflow(
        "flow",
        "try { await workflow({ source: 'path', value: 'checks' }) } catch (e) { return [e.cause, e.step, e.reason] }",
    );
    std::fs::create_dir_all(dir.join("checks")).unwrap();
    h.start(Origin::Path(dir.clone()));
    assert_eq!(
        h.done(),
        json!([
            "invalid",
            1,
            format!("{} holds no `workflow.js`", dir.join("checks").display())
        ])
    );
}

#[test]
fn a_done_teams_seats_are_stopped_before_the_next_launch() {
    let h = Harness::new();
    h.start(script(
        "await team({ team: 'crew', prompt: 'one', worktree: 'w' })\n\
         return (await team({ team: 'crew', prompt: 'two', worktree: 'w' })).report",
    ));
    h.fake.board_done(1, 1, "one done");
    h.advance();
    let order: Vec<&str> = h
        .fake
        .trail()
        .into_iter()
        .filter(|call| matches!(*call, "launch" | "stop_launches"))
        .collect();
    assert_eq!(order, ["launch", "stop_launches", "launch"]);
    assert_eq!(h.fake.stopped(), ["L1", "L2"]);
}

#[test]
fn the_room_admits_each_launch_by_its_call() {
    let h = Harness::new();
    h.fake
        .set_admit_for("a", Admit::Hold("profile a has no headroom".to_owned()));
    h.start(script(
        "const a = agent({ profile: 'a', prompt: 'p' })\n\
         const b = agent({ profile: 'b', prompt: 'p' })\n\
         return [(await a).report, (await b).report]",
    ));
    assert!(
        matches!(
            h.work(1),
            Work::Held {
                hold: Hold::Room { .. },
                ..
            }
        ),
        "{:?}",
        h.work(1)
    );
    assert!(matches!(h.work(2), Work::Launch { .. }), "{:?}", h.work(2));
}

#[test]
fn a_workflow_call_and_its_child_expiring_together_owe_the_childs_stop_once() {
    let h = Harness::new();
    h.start(script(
        "try { await workflow({ source: 'script', value: \"return (await agent({ profile: 'c', prompt: 'c', timeout: '1m' })).report\", timeout: '1m' }) } catch (e) { return e.cause }",
    ));
    h.fake.later(jiff::SignedDuration::from_mins(2));
    h.advance();
    assert_eq!(h.done(), json!("timeout"));
    let stops = h
        .journal()
        .into_iter()
        .filter(|event| matches!(event, Event::Stop { .. }))
        .count();
    assert_eq!(stops, 1, "{:#?}", h.journal());
}

#[test]
fn an_agent_given_schema_null_completes_with_its_report() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p', schema: null })).report",
    ));
    h.fake.end("L1", completed("plain"));
    h.advance();
    assert_eq!(h.done(), json!("plain"));
}

#[test]
fn a_launch_waits_while_a_stop_of_launches_is_owed_and_the_advance_returns() {
    let h = Harness::new();
    h.start(script(
        "await team({ team: 'crew', prompt: 'one', worktree: 'w' })\n\
         return (await team({ team: 'crew', prompt: 'two', worktree: 'w' })).report",
    ));
    h.fake.board_done(1, 1, "one done");
    h.fake.fail("stop_launches", true);
    assert!(h.advance().is_some());
    assert_eq!(
        h.fake.launched().len(),
        2,
        "no launch beside the live seats"
    );
    assert!(matches!(h.work(2), Work::Due), "{:?}", h.work(2));
    h.fake.fail("stop_launches", false);
    h.advance();
    assert_eq!(h.fake.stopped(), ["L1", "L2"]);
    assert_eq!(h.fake.live().len(), 2);
}

#[test]
fn a_called_script_the_run_cannot_read_fails_its_call_invalid() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 000, so the fixture cannot deny the read.
    }
    let h = Harness::new();
    // Apart from the run's own directory, which the run copies whole.
    let dir = h.workflow("called", "return 1");
    let flow = h.workflow(
        "flow",
        &format!(
            "const out = []\n\
             for (const value of ['{}', '{}']) {{ try {{ await workflow({{ source: 'path', value }}) }} catch (e) {{ out.push([e.cause, e.reason]) }} }}\n\
             return out",
            dir.join("linked").display(),
            dir.join("closed").display()
        ),
    );
    let mode = |path: &std::path::Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    std::fs::create_dir_all(dir.join("linked")).unwrap();
    std::fs::write(dir.join("linked/secret.js"), format!("{META}return 1")).unwrap();
    std::os::unix::fs::symlink("secret.js", dir.join("linked/workflow.js")).unwrap();
    mode(&dir.join("linked/secret.js"), 0o000);
    std::fs::create_dir_all(dir.join("closed")).unwrap();
    std::fs::write(dir.join("closed/workflow.js"), format!("{META}return 1")).unwrap();
    mode(&dir.join("closed"), 0o000);
    let ran = caught(|| start(&h.state, &h.room, h.input(Origin::Path(flow))));
    mode(&dir.join("linked/secret.js"), 0o600);
    mode(&dir.join("closed"), 0o700);
    assert!(ran.is_some());
    let done = h.done();
    let calls = done.as_array().unwrap();
    assert_eq!(calls.len(), 2, "{done}");
    for call in calls {
        assert_eq!(call[0], json!("invalid"), "{done}");
        assert!(
            call[1].as_str().unwrap().contains("Permission denied"),
            "{done}"
        );
    }
}

#[test]
fn a_launch_whose_outcome_is_unknown_hands_its_step_back_when_the_launches_do_not_list() {
    let h = Harness::new();
    h.fake
        .answer_next_launch(Launched::Unknown("the launcher timed out".to_owned()));
    let fake = Rc::clone(&h.fake);
    h.fake.hook_after("launch", move || {
        fake.fail("launches", true);
        true
    });
    assert!(start(&h.state, &h.room, h.input(script(AGENT))).is_err());
    assert!(matches!(h.work(1), Work::Due), "{:?}", h.work(1));
}
