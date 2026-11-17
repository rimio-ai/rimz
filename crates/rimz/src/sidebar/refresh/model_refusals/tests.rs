use super::*;
use crate::agents::account::{RateLimitCacheEntry, RateLimitsCache};
use crate::agents::{
    AgentContext, AgentRateLimits, AgentStatus, AgentTurnError, ModelRefusal, RateLimitWindow,
    TurnErrorClass,
};
use crate::ids::{AgentKind, LoginKey, WorkspaceId};

const SENTENCE: &str =
    "Claude Fable 5.2 is not available on your plan. Switch models with /model to keep working.";

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap()
}

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn login() -> LoginKey {
    LoginKey::default_for(AgentKind::new_unchecked("claude"))
}

fn window(duration_mins: u32, used: u8, observed: i64) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        duration_mins: Some(duration_mins),
        observed_at: Some(ts(observed)),
        ..Default::default()
    }
}

fn write_windows(runtime: &RuntimePaths, windows: Vec<RateLimitWindow>) {
    let cache = RateLimitsCache {
        entries: [(
            login().into(),
            RateLimitCacheEntry {
                limits: AgentRateLimits { windows },
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(runtime),
        &cache,
    )
    .unwrap();
}

/// A resting agent whose keep-warm ping, opened at 100, the provider refused at `at`.
fn refused_ping(id: &str, at: i64) -> AgentState {
    let mut agent = crate::testkit::agent_state("claude", id, ts(50));
    agent.status = AgentStatus::Success;
    agent.model = Some("claude-fable-5-2".to_owned());
    agent.ping_opened_at = Some(ts(100));
    agent.context = Some(AgentContext {
        model_id: Some("claude-fable-5-2".to_owned()),
        turn_error: Some(AgentTurnError {
            model: Some("claude-fable-5-2".to_owned()),
            ended_turn: false,
            class: TurnErrorClass::PausedRateLimit,
            at: ts(at),
            label: Some(SENTENCE.to_owned()),
        }),
        ..AgentContext::new("claude", ts(at))
    });
    agent
}

/// A resting agent whose clean turn on `model` ran from `started`.
fn clean_turn(id: &str, model: &str, started: i64) -> AgentState {
    let mut agent = crate::testkit::agent_state("claude", id, ts(started + 10));
    agent.status = AgentStatus::Success;
    agent.model = Some(model.to_owned());
    agent.turn_started_at = Some(ts(started));
    agent.turn_ended_at = Some(ts(started + 10));
    agent
}

fn pass(runtime: &RuntimePaths, agents: &[AgentState], now: i64) {
    let logins = RoomLoginSet::native().with_agents(agents);
    record_model_refusals(agents, runtime, &logins, ts(now));
}

fn record(runtime: &RuntimePaths) -> Option<ModelRefusal> {
    ModelRefusals::read(&runtime.shared_model_refusals_path())
        .login(&login())
        .get("fable 5.2")
        .cloned()
}

#[test]
fn a_fresh_unspent_read_after_a_refused_ping_records_the_refusal_once() {
    let (_dir, runtime) = runtime();
    let agents = [refused_ping("sess", 110)];
    pass(&runtime, &agents, 120);
    assert_eq!(record(&runtime), None, "no read since the marker yet");

    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &agents, 210);
    assert_eq!(
        record(&runtime),
        Some(ModelRefusal {
            refused_at: ts(110),
            succeeded_at: None,
            label: Some(SENTENCE.to_owned()),
        })
    );
    let logins = RoomLoginSet::native().with_agents(&agents);
    let capacities = ProviderCapacity::read_all(&runtime, &logins);
    assert!(
        !ModelRefusals::read(&runtime.shared_model_refusals_path()).observe(
            &agents,
            &capacities,
            ts(220)
        ),
        "a standing record raises nothing on the next pass"
    );
}

#[test]
fn a_read_that_does_not_confirm_records_nothing() {
    let cases = [
        (
            "a spent covering window",
            vec![window(300, 100, 200), window(10_080, 60, 200)],
        ),
        (
            "a read older than the marker",
            vec![window(300, 40, 105), window(10_080, 60, 105)],
        ),
        (
            "a read first seen past ten minutes",
            vec![window(300, 40, 711), window(10_080, 60, 711)],
        ),
        (
            "one stale covering window",
            vec![window(300, 40, 200), window(10_080, 60, 105)],
        ),
        ("a login with no windows", vec![]),
    ];
    for (case, windows) in cases {
        let (_dir, runtime) = runtime();
        write_windows(&runtime, windows);
        pass(&runtime, &[refused_ping("sess", 110)], 800);
        assert_eq!(record(&runtime), None, "{case}");
    }
}

#[test]
fn a_second_marker_raises_a_standing_record_with_no_read() {
    let (_dir, runtime) = runtime();
    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &[refused_ping("sess", 110)], 210);
    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &[refused_ping("other", 5_000)], 5_010);
    assert_eq!(
        record(&runtime).map(|record| record.refused_at),
        Some(ts(5_000))
    );
}

#[test]
fn only_a_clean_turn_started_after_the_refusal_on_its_model_clears_it() {
    let (_dir, runtime) = runtime();
    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &[refused_ping("sess", 110)], 210);

    pass(
        &runtime,
        &[
            clean_turn("before", "claude-fable-5-2", 90),
            clean_turn("sibling", "claude-fable-5-5", 300),
        ],
        320,
    );
    assert!(
        record(&runtime).unwrap().stands(),
        "an earlier turn or a sibling version clears nothing"
    );

    pass(
        &runtime,
        &[clean_turn("after", "claude-fable-5-2", 300)],
        320,
    );
    let cleared = record(&runtime).unwrap();
    assert_eq!(cleared.succeeded_at, Some(ts(300)));
    assert!(!cleared.stands());
}

/// An ended row's last clean turn is its own evidence; a provider-native subagent row, whose
/// stamps are its parent's, is none.
#[test]
fn a_clean_turn_on_a_row_that_has_since_ended_clears_it() {
    let (_dir, runtime) = runtime();
    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &[refused_ping("sess", 110)], 210);

    let mut native = clean_turn("native", "claude-fable-5-2", 300);
    native.parent_agent_id = Some("sess".into());
    native.ended_at = Some(ts(315));
    pass(&runtime, &[native], 320);
    assert!(record(&runtime).unwrap().stands(), "a native subagent row");

    let mut ended = clean_turn("ended", "claude-fable-5-2", 300);
    ended.ended_at = Some(ts(315));
    pass(&runtime, &[ended], 320);
    assert_eq!(record(&runtime).unwrap().succeeded_at, Some(ts(300)));
}

#[test]
fn two_writers_end_equal_in_either_order() {
    let first = [refused_ping("sess", 110)];
    let second = [refused_ping("other", 150)];
    let success = [clean_turn("after", "claude-fable-5-2", 300)];
    let orders = [
        [&first[..], &second[..], &success[..]],
        [&second[..], &success[..], &first[..]],
        [&success[..], &second[..], &first[..]],
    ];
    let ends: Vec<_> = orders
        .into_iter()
        .map(|order| {
            let (_dir, runtime) = runtime();
            write_windows(
                &runtime,
                vec![window(300, 40, 200), window(10_080, 60, 200)],
            );
            // Rows keep their evidence, so each room's next pass sees it again: a success one
            // room saw before any record existed lands there.
            for agents in order.iter().chain(&order) {
                pass(&runtime, agents, 450);
            }
            record(&runtime)
        })
        .collect();
    assert_eq!(ends[0], ends[1]);
    assert_eq!(ends[1], ends[2]);
    assert_eq!(
        ends[0]
            .as_ref()
            .map(|record| (record.refused_at, record.succeeded_at)),
        Some((ts(150), Some(ts(300))))
    );
}

#[test]
fn an_unreadable_record_reads_as_none_and_a_failed_write_is_survived() {
    let (_dir, runtime) = runtime();
    let path = runtime.shared_model_refusals_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(record(&runtime), None);
    write_windows(
        &runtime,
        vec![window(300, 40, 200), window(10_080, 60, 200)],
    );
    pass(&runtime, &[refused_ping("sess", 110)], 210);
    assert_eq!(
        record(&runtime).map(|record| record.refused_at),
        Some(ts(110))
    );

    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    pass(&runtime, &[refused_ping("sess", 110)], 210);
    assert!(
        path.is_dir(),
        "the failed write is logged and the pass goes on"
    );
}
