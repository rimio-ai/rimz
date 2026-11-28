//! Two processes at one run: a hook of the fake room runs the other process's step between two
//! calls of this one, and the step this one publishes after is compared with what moved.

use std::collections::BTreeMap;

use serde_json::json;

use super::super::room::Look;
use super::*;

fn true_look() -> Look {
    Look {
        truth: Some(true),
        readings: BTreeMap::new(),
    }
}

#[test]
fn a_resume_that_loses_to_another_leaves_the_files_its_record_names() {
    let h = Harness::new();
    h.start(script(
        "const t = await now()\nawait watch({ when: 'pr=merged' })\nreturn t",
    ));
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    let other = h.other();
    // `now` is the first room call a resume makes once it has planned.
    h.fake.hook("now", move || {
        other
            .as_process(|other| resume(&other.state, &other.room, &other.id, None, None))
            .expect("the other resume ran");
        other.fake.set_look("pr=merged", true_look());
        other.advance();
        true
    });
    let lost = resume(&h.state, &h.room, &h.id, None, None).unwrap_err();
    assert_eq!(
        format!("{lost:#}"),
        format!(
            "workflow run {} changed while it resumed; resume it again",
            h.id
        )
    );
    assert_eq!(h.lines().len(), 2);
    assert_eq!(h.done(), json!(h.fake.now().as_millisecond()));
    assert!(
        h.state
            .workflow_run(&h.id)
            .files(h.record().files)
            .join("root")
            .exists()
    );
}

#[test]
fn a_launch_another_process_made_after_this_one_read_the_record_is_not_stopped() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    h.fake.end("L1", None);
    let other = h.other();
    h.fake.hook("launches", move || {
        other.advance();
        true
    });
    h.advance();
    assert_eq!(h.fake.stopped(), Vec::<String>::new());
    assert_eq!(
        h.fake
            .live()
            .iter()
            .map(|launch| launch.id.as_str())
            .collect::<Vec<_>>(),
        ["L2"]
    );
    assert!(matches!(h.work(1), Work::Launch { .. }), "{:?}", h.work(1));
}

#[test]
fn a_launch_of_a_run_resumed_after_this_process_read_it_ended_is_not_stopped() {
    let h = Harness::new();
    h.start(script(
        "return (await agent({ profile: 'a', prompt: 'p' })).report",
    ));
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    let other = h.other();
    h.fake.hook("launches", move || {
        other
            .as_process(|other| resume(&other.state, &other.room, &other.id, None, None))
            .expect("the other resume ran");
        true
    });
    h.advance();
    assert_eq!(h.fake.stopped(), ["L1"]);
    assert_eq!(
        h.fake
            .live()
            .iter()
            .map(|launch| launch.id.as_str())
            .collect::<Vec<_>>(),
        ["L2"]
    );
}

#[test]
fn a_host_that_settled_its_step_before_its_spawner_published_is_not_killed() {
    let h = Harness::new();
    let other = h.other();
    h.fake.hook_after("start_host", move || {
        other.run_hosts();
        true
    });
    h.start(script(
        "return (await shell({ command: 'echo out' })).stdout",
    ));
    assert_eq!(h.done(), json!("out\n"));
    assert_eq!(h.fake.calls("kill"), 0);
}

#[test]
fn a_run_expiry_read_before_another_process_resumed_the_run_does_not_end_it() {
    let h = Harness::new();
    let mut input = h.input(script("await watch({ when: 'pr=merged' })\nreturn 1"));
    input.timeout = Some("1m".to_owned());
    h.start_with(input);
    h.fake.later(jiff::SignedDuration::from_mins(2));
    let other = h.other();
    h.fake.hook("now", move || {
        other
            .as_process(|other| cancel(&other.state, &other.room, &other.id))
            .expect("the other cancel ran");
        other
            .as_process(|other| resume(&other.state, &other.room, &other.id, Some("1h"), None))
            .expect("the other resume ran");
        true
    });
    h.advance();
    assert_eq!(h.ending(), None, "{:#?}", h.record());
    assert!(h.record().open.contains_key(&1));
}

#[test]
fn two_first_passes_at_once_journal_their_marks_once() {
    let h = Harness::new();
    let other = h.other();
    h.fake.hook("zone", move || {
        other.advance();
        true
    });
    h.start(script(
        "log('a')\nawait watch({ when: 'pr=merged' })\nreturn 1",
    ));
    let logs = h
        .journal()
        .into_iter()
        .filter(|event| matches!(event, Event::Log { .. }))
        .count();
    assert_eq!(logs, 1, "{:#?}", h.journal());
}

#[test]
fn a_launch_is_not_reserved_while_another_process_owes_a_stop_of_launches() {
    let h = Harness::new();
    h.start(script(
        "const one = team({ team: 'crew', prompt: 'one', worktree: 'w' })\n\
         await watch({ when: 'pr=merged' })\n\
         const two = await team({ team: 'crew', prompt: 'two', worktree: 'w' })\n\
         return [(await one).report, two.report]",
    ));
    h.fake.set_look("pr=merged", true_look());
    let other = h.other();
    h.fake.hook("admit", move || {
        let now = other.fake.now();
        other.publish(|change| {
            change.settle(1, Settled::Result(json!({ "report": "one done" })), now);
            change
                .next
                .owed
                .push(crate::store::workflow_run::Stop::Launches {
                    launches: vec!["L1".to_owned(), "L2".to_owned()],
                });
        });
        true
    });
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
fn a_pass_that_read_the_record_before_a_resume_removed_its_log_reads_again() {
    let h = Harness::new();
    h.start(script(
        "const t = await now()\nawait watch({ when: 'pr=merged' })\nawait watch({ when: 'ci=passed' })\nreturn t",
    ));
    h.fake.set_look("pr=merged", true_look());
    let other = h.other();
    // `zone` is the first room call of a pass, made once it has read the record.
    h.fake.hook("zone", move || {
        other
            .as_process(|other| cancel(&other.state, &other.room, &other.id))
            .expect("the other cancel ran");
        other
            .as_process(|other| resume(&other.state, &other.room, &other.id, None, None))
            .expect("the other resume ran");
        true
    });
    assert!(h.advance().is_some());
    assert_eq!(h.ending(), None, "{:#?}", h.record());
    assert!(h.record().open.contains_key(&3), "{:#?}", h.record());
}
