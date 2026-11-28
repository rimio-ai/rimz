//! The run's bounds, each reached inside a `try`: the run ends at once, and no `catch` or
//! `finally` sees it.

use serde_json::json;

use super::*;

#[test]
fn the_launch_cap_ends_the_run_at_the_launch_past_it() {
    let h = Harness::new();
    let mut input = h.input(script(
        "try { await agent({ profile: 'a', prompt: 'p' }) } catch {}\ntry { await agent({ profile: 'b', prompt: 'p' }) } catch { return 'caught' } finally { await now() }\nreturn 'after'",
    ));
    input.max_launches = Some(1);
    h.start_with(input);
    h.fake.end("L1", completed("a"));
    h.advance();
    let (reason, step, failure) = h.failed();
    assert_eq!(reason, "call 2 would launch past the run's launch cap of 1");
    assert_eq!(
        (step, failure.unwrap()["cause"].clone()),
        (Some(2), json!("launches"))
    );
    assert_eq!(h.fake.launched().len(), 1);
    assert_eq!(
        h.lines().iter().map(|line| line.number).collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(h.record().ended.unwrap().open, [2]);
}

#[test]
fn the_step_cap_ends_the_run_at_the_call_past_it() {
    let h = Harness::new();
    // The start stops before its first pass publishes, so the cap can be lowered first.
    h.fake.crash_at("zone", 1, true);
    assert!(
        h.start(script(
            "try { await now(); await now(); await now() } catch { return 'caught' } finally { await now() }",
        ))
        .is_none()
    );
    h.edit_record(|record| record.bounds.steps = 2);
    h.advance();
    let (reason, step, failure) = h.failed();
    assert_eq!(
        reason,
        "the script would make call 3, past the run's step cap of 2"
    );
    assert_eq!(
        (step, failure.unwrap()["cause"].clone()),
        (Some(3), json!("steps"))
    );
    assert_eq!(h.lines().len(), 2);
}

#[test]
fn the_runs_timeout_ends_it_failed_timeout() {
    let h = Harness::new();
    let mut input = h.input(script(
        "try { await watch({ when: 'pr=merged' }) } catch { return 'caught' } finally { await now() }",
    ));
    input.timeout = Some("1h".to_owned());
    let status = h.start_with(input).unwrap();
    assert_eq!(status.next, h.record().bounds.expires);
    h.fake.later(jiff::SignedDuration::from_hours(2));
    h.advance();
    let (reason, step, failure) = h.failed();
    assert_eq!(
        (reason.as_str(), step),
        ("the run ran past its timeout", None)
    );
    assert_eq!(failure.unwrap()["cause"], json!("timeout"));
    assert!(h.lines().is_empty());
    assert_eq!(h.record().ended.unwrap().open, [1]);
}

#[test]
fn a_steps_timeout_fails_it_timeout_and_stops_its_work() {
    let h = Harness::new();
    h.start(script(
        "try { await agent({ profile: 'a', prompt: 'p', timeout: '10m' }) } catch (e) { return [e.cause, e.reason, e.handle] }",
    ));
    h.fake.later(jiff::SignedDuration::from_mins(11));
    h.advance();
    assert_eq!(
        h.done(),
        json!(["timeout", "the call ran past its timeout of 10m", "agent"])
    );
    assert_eq!(h.fake.stopped(), ["L1"]);
}

#[test]
fn a_step_already_past_its_timeout_fails_timeout_before_any_cap_is_read() {
    for call in [
        "agent({ profile: 'a', prompt: 'p', timeout: '0s' })",
        "shell({ command: 'echo out', timeout: '0s' })",
    ] {
        let h = Harness::new();
        let mut input = h.input(script(&format!(
            "try {{ await {call} }} catch (e) {{ return e.cause }}"
        )));
        input.max_launches = Some(0);
        h.start_with(input);
        assert_eq!(h.done(), json!("timeout"), "{call}");
        assert!(h.fake.launched().is_empty(), "{call}");
        assert!(h.fake.take_hosts().is_empty(), "{call}");
    }
}

#[test]
fn a_step_under_a_workflow_call_past_its_timeout_fails_the_call_timeout_before_any_cap_is_read() {
    let h = Harness::new();
    let mut input = h.input(script(
        "try { await workflow({ source: 'script', value: \"return (await agent({ profile: 'a', prompt: 'p' })).report\", timeout: '0s' }) } catch (e) { return e.cause }",
    ));
    input.max_launches = Some(0);
    h.start_with(input);
    assert_eq!(h.done(), json!("timeout"));
    assert!(h.fake.launched().is_empty());
}
