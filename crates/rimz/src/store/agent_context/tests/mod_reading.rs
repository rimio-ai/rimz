use super::*;
use crate::agents::context::WindowSource;
use crate::agents::{
    AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, AgentTurnError,
    RateLimitWindow, TurnErrorClass, TurnSettle, TurnSettleOutcome,
};

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + second).unwrap()
}

/// What `rimz mod feed` hands the merge for one report with usage.
fn reading(observed_at: Timestamp, tokens: u64, cost: f64) -> AgentContext {
    AgentContext {
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(4),
            current_context_tokens: Some(tokens),
            ..AgentTokenUsage::default()
        }),
        rate_limits: Some(
            AgentRateLimits {
                windows: vec![RateLimitWindow {
                    duration_mins: Some(10_080),
                    used_percentage: Some(45),
                    resets_at: Some(at(600_000)),
                    source: WindowSource::BestEffort,
                    ..Default::default()
                }],
            }
            .stamped_at(observed_at),
        ),
        cost: Some(AgentCost {
            total_cost_usd: Some(cost),
            ..AgentCost::default()
        }),
        ..AgentContext::new("claude", observed_at)
    }
}

fn interrupted(second: i64) -> TurnSettle {
    TurnSettle::new(at(second), TurnSettleOutcome::Interrupted)
}

fn died(second: i64) -> AgentTurnError {
    AgentTurnError {
        model: None,
        ended_turn: false,
        class: TurnErrorClass::PausedOverloaded,
        at: at(second),
        label: Some("API Error: Overloaded".into()),
    }
}

#[test]
fn a_mod_reading_sets_its_fields_and_keeps_the_statusline_rest() {
    let (_dir, runtime) = runtime();
    let mut pushed = ctx(at(0));
    pushed.fast = Some(true);
    pushed.effort = Some("high".into());
    pushed.settle = Some(interrupted(-5));
    pushed.turn_error = Some(died(-10));
    pushed.tokens = Some(AgentTokenUsage {
        context_window_size: Some(200_000),
        used_percentage: Some(1),
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(3),
            output_tokens: Some(9),
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(1_900),
        }),
        ..AgentTokenUsage::default()
    });
    pushed.cost = Some(AgentCost {
        total_cost_usd: Some(0.001),
        ..AgentCost::default()
    });
    write(&runtime, "claude", "s", &pushed, None).unwrap();
    let stored = read_one(&runtime, "claude", "s").unwrap().context;

    assert!(
        merge_mod_reading(
            &runtime,
            "claude",
            "s",
            reading(at(10), 38_328, 0.0046),
            None
        )
        .unwrap()
    );
    let context = read_one(&runtime, "claude", "s").unwrap().context;
    let tokens = context.tokens.as_ref().unwrap();
    assert_eq!(
        (
            tokens.current_context_tokens,
            tokens.context_window_size,
            tokens.used_percentage
        ),
        (Some(38_328), Some(1_000_000), Some(4))
    );
    assert_eq!(
        tokens.current_usage,
        pushed.tokens.as_ref().unwrap().current_usage,
        "the split stays"
    );
    assert_eq!(
        context.rate_limits.as_ref().unwrap().windows[0].used_percentage,
        Some(45)
    );
    assert_eq!(context.cost.as_ref().unwrap().total_cost_usd, Some(0.0046));
    assert_eq!(context.model_id.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(context.effort.as_deref(), Some("high"));
    assert_eq!(context.fast, Some(true));
    assert_eq!(context.settle, stored.settle);
    assert_eq!(context.turn_error, stored.turn_error);

    let lower = reading(at(20), 38_328, 0.0001);
    merge_mod_reading(&runtime, "claude", "s", lower, None).unwrap();
    assert_eq!(
        read_one(&runtime, "claude", "s")
            .unwrap()
            .context
            .cost
            .unwrap()
            .total_cost_usd,
        Some(0.0046),
        "a lower mod cost changes nothing"
    );

    let later = ctx(at(30));
    write(&runtime, "claude", "s", &later, None).unwrap();
    let context = read_one(&runtime, "claude", "s").unwrap().context;
    assert_eq!(
        (
            context.tokens,
            context.fast,
            context.settle,
            context.turn_error
        ),
        (None, None, None, None),
        "a later statusline push still lands whole"
    );
}

#[test]
fn a_mod_reading_dates_its_windows_as_a_statusline_push_does() {
    let (_dir, runtime) = runtime();
    let owner = at(-60);
    merge_mod_reading(&runtime, "claude", "s", reading(at(0), 1, 0.0), Some(owner)).unwrap();
    let stamp = |runtime: &RuntimePaths| {
        read_one(runtime, "claude", "s")
            .unwrap()
            .context
            .rate_limits
            .unwrap()
            .windows[0]
            .observed_at
    };
    assert_eq!(
        stamp(&runtime),
        Some(owner),
        "the first windows take the owner's start"
    );
    merge_mod_reading(
        &runtime,
        "claude",
        "s",
        reading(at(10), 2, 0.0),
        Some(owner),
    )
    .unwrap();
    assert_eq!(
        stamp(&runtime),
        Some(owner),
        "unchanged content keeps its stamp"
    );
    let mut moved = reading(at(20), 3, 0.0);
    moved.rate_limits.as_mut().unwrap().windows[0].used_percentage = Some(46);
    moved.rate_limits.as_mut().unwrap().windows[0].observed_at = Some(at(20));
    merge_mod_reading(&runtime, "claude", "s", moved, Some(owner)).unwrap();
    assert_eq!(stamp(&runtime), Some(at(20)));

    let (_dir, unowned) = super::runtime();
    merge_mod_reading(&unowned, "claude", "s", reading(at(0), 1, 0.0), None).unwrap();
    assert_eq!(stamp(&unowned), Some(at(0)));
}

#[test]
fn a_mod_scan_never_moves_a_marker_backwards() {
    let (_dir, runtime) = runtime();
    let mut stored = ctx(at(0));
    stored.settle = Some(interrupted(100));
    stored.turn_error = Some(died(100));
    write(&runtime, "claude", "s", &stored, None).unwrap();
    let markers = |runtime: &RuntimePaths| {
        let context = read_one(runtime, "claude", "s").unwrap().context;
        (
            context.settle.map(|settle| settle.at),
            context.turn_error.map(|error| error.at),
        )
    };
    for second in [50, 100] {
        let mut stale = AgentContext::new("claude", at(200));
        stale.settle = Some(interrupted(second));
        stale.turn_error = Some(died(second));
        assert!(
            !merge_mod_reading(&runtime, "claude", "s", stale, None).unwrap(),
            "a marker at or before the stored one changes nothing ({second})"
        );
        assert_eq!(markers(&runtime), (Some(at(100)), Some(at(100))));
    }
    let mut newer = AgentContext::new("claude", at(300));
    newer.settle = Some(interrupted(150));
    merge_mod_reading(&runtime, "claude", "s", newer, None).unwrap();
    assert_eq!(markers(&runtime), (Some(at(150)), Some(at(100))));
    let mut newer = AgentContext::new("claude", at(300));
    newer.turn_error = Some(died(160));
    merge_mod_reading(&runtime, "claude", "s", newer, None).unwrap();
    assert_eq!(markers(&runtime), (Some(at(150)), Some(at(160))));

    let (_dir, empty) = super::runtime();
    let mut first = AgentContext::new("claude", at(10));
    first.settle = Some(interrupted(5));
    merge_mod_reading(&empty, "claude", "s", first, None).unwrap();
    assert_eq!(markers(&empty), (Some(at(5)), None));
}
