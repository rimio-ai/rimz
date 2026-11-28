//! A crash at every point between the record and the work, per block: the fake crashes the
//! process at its nth call, before or after that call's effect, for every n the scenario reaches.
//! Each run still ends with one line per step, no more starts than the rules allow, and no
//! attempt with two launches.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::super::room::Look;
use super::*;

/// Run `body` with a crash at every call it makes, then drive it to its end with `world`.
fn every_crash(
    body: &str,
    setup: impl Fn(&Harness),
    world: impl Fn(&Harness),
    check: impl Fn(&Harness, &Value),
) {
    for after in [false, true] {
        for nth in 1.. {
            assert!(nth < 400, "the scenario never stops reaching a new call");
            let h = Harness::new();
            setup(&h);
            h.fake.crash_after_calls(nth, after);
            if h.start(script(body)).is_none() && !h.exists() {
                // A start that died before its home published left nothing; its caller starts again.
                h.start(script(body));
            }
            let ending = drive(&h, &world);
            let Ending::Done { result } = ending else {
                panic!(
                    "crash at call {nth} (after: {after}): the run ended {ending:?}\n{:#?}",
                    h.record()
                );
            };
            check(&h, &result);
            let mut lines = BTreeMap::<u64, usize>::new();
            for line in h.lines() {
                *lines.entry(line.number).or_default() += 1;
            }
            assert!(
                lines.values().all(|count| *count == 1),
                "crash at call {nth}: {lines:?}"
            );
            let mut starts = BTreeMap::<u64, u32>::new();
            for event in h.journal() {
                if let Event::Start { step, attempt } = event {
                    starts.insert(step, attempt);
                }
            }
            assert!(
                starts.values().all(|attempt| *attempt <= STARTS),
                "crash at call {nth}: {starts:?}"
            );
            let mut launches = BTreeMap::<(u64, u32, bool), usize>::new();
            for launch in h.fake.launched() {
                *launches
                    .entry((
                        launch.pin.step,
                        launch.pin.attempt,
                        launch.block == BlockName::Team,
                    ))
                    .or_default() += 1;
            }
            assert!(
                launches
                    .iter()
                    .all(|((_, _, team), count)| *count == if *team { 2 } else { 1 }),
                "crash at call {nth}: an attempt launched twice: {launches:?}"
            );
            assert!(
                h.fake.live().is_empty(),
                "crash at call {nth}: {:?}",
                h.fake.live()
            );
            if h.fake.crash_pending() {
                break;
            }
        }
    }
}

#[test]
fn an_agent_step_survives_a_crash_at_every_point() {
    every_crash(
        "return (await agent({ profile: 'opus', prompt: 'p' })).report",
        |_| {},
        world,
        |_, result| {
            assert!(
                result
                    .as_str()
                    .is_some_and(|report| report.ends_with(" done")),
                "{result}"
            )
        },
    );
}

#[test]
fn a_team_step_survives_a_crash_at_every_point() {
    every_crash(
        "return (await team({ team: 'crew', prompt: 'p', worktree: 'w' })).report",
        |_| {},
        world,
        |_, result| assert_eq!(*result, json!("team done")),
    );
}

#[test]
fn a_command_step_survives_a_crash_at_every_point() {
    every_crash(
        "return (await shell({ command: 'echo ran >> \"$RIMZ_WORKFLOW_DIR/ran\"; echo out', timeout: '1h' })).stdout",
        |_| {},
        world,
        |h, result| {
            assert_eq!(*result, json!("out\n"));
            let ran =
                std::fs::read_to_string(h.state.workflow_run(&h.id).step(1).join("ran")).unwrap();
            let starts = h
                .journal()
                .iter()
                .filter(|event| matches!(event, Event::Start { .. }))
                .count();
            assert!(
                ran.lines().count() <= starts,
                "{ran:?} over {starts} starts"
            );
        },
    );
}

#[test]
fn the_immediate_blocks_survive_a_crash_at_every_point() {
    every_crash(
        "const at = await now()\n\
         const booked = await schedule({ name: 'n', trigger: 'at', value: '1h', tool: 'agent', arguments: { profile: 'o', prompt: 'p' } })\n\
         const seen = await watch({ when: 'pr=merged' })\n\
         const called = await workflow({ source: 'script', value: 'return 7' })\n\
         return [typeof at, booked.id, seen.readings.pr, called]",
        |h| {
            h.fake.set_look(
                "pr=merged",
                Look {
                    truth: Some(true),
                    readings: BTreeMap::from([("pr".to_owned(), Some("merged".to_owned()))]),
                },
            )
        },
        world,
        |_, result| assert_eq!(*result, json!(["number", "booking-n", "merged", 7])),
    );
}

#[test]
fn a_spawner_that_returns_before_its_host_claims_causes_no_restart() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo out' })).stdout",
    ));
    assert!(matches!(h.work(1), Work::Command { child: None, .. }));
    h.advance();
    h.advance();
    assert_eq!(h.record().open[&1].attempt, 1);
    h.run_hosts();
    assert_eq!(h.done(), json!("out\n"));
}

#[test]
fn a_host_that_died_after_its_claim_and_before_its_child_is_started_again() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo out' })).stdout",
    ));
    let (_, host) = h.fake.take_hosts().remove(0);
    h.fake.kill_process(&host);
    h.edit_record(|record| {
        if let Some(Work::Command { child, .. }) =
            record.open.get_mut(&1).map(|step| &mut step.work)
        {
            *child = None;
        }
    });
    h.advance();
    assert_eq!(h.record().open[&1].attempt, 2);
    h.run_hosts();
    assert_eq!(h.done(), json!("out\n"));
}

#[test]
fn a_dead_hosts_group_is_killed_before_a_restart_even_with_its_leader_gone() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo out' })).stdout",
    ));
    let (_, host) = h.fake.take_hosts().remove(0);
    let child = h.fake.spawn();
    let recorded = child.clone();
    h.edit_record(move |record| {
        if let Some(Work::Command { child: slot, .. }) =
            record.open.get_mut(&1).map(|step| &mut step.work)
        {
            *slot = Some(recorded);
        }
    });
    h.fake.kill_process(&host);
    h.fake.kill_process(&child);
    let kills = h.fake.calls("kill");
    h.advance();
    assert_eq!(h.fake.calls("kill"), kills + 1);
    assert_eq!(h.record().open[&1].attempt, 2);
}
