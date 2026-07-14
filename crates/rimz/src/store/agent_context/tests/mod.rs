use super::*;
use crate::ids::WorkspaceId;

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/ctx-test"));
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn ctx(observed_at: Timestamp) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: Some("claude-opus-4-8".to_owned()),
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at,
    }
}

#[test]
fn write_then_read_round_trips() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let all = read_all(&runtime);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, "claude");
    assert_eq!(all[0].agent_id, "sess-1");
    assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn turn_openers_replace_and_survive_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    let first = crate::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    let second = crate::ids::MessageId::parse("msg_123456789abcdef0").unwrap();

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![first.clone()]).unwrap());
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![first]
    );

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![second.clone()]).unwrap());
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![second]
    );
    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", Vec::new()).unwrap());
    assert!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by
            .is_empty()
    );
}

#[test]
fn locally_priced_cost_deduplicates_accumulates_and_survives_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let first = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-1".to_owned(),
        cost_usd: 0.25,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap());
    assert!(!merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap());

    let mut context = ctx(Timestamp::now());
    context.source = "cursor".to_owned();
    context.model_id = Some("auto".to_owned());
    write(&runtime, "cursor", "sess-1", &context).unwrap();
    let second = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-2".to_owned(),
        cost_usd: 0.5,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &second).unwrap());

    let record = read_one(&runtime, "cursor", "sess-1").unwrap();
    assert_eq!(record.context.model_id.as_deref(), Some("auto"));
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.75)
    );
    assert_eq!(
        record.context.cost.as_ref().map(|cost| cost.basis),
        Some(crate::agents::CostBasis::LocallyPriced)
    );
}

#[test]
fn provider_reported_cost_wins_over_later_estimates() {
    let (_dir, runtime) = runtime();
    let first = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-1".to_owned(),
        cost_usd: 0.25,
    };
    merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap();

    let mut context = ctx(Timestamp::now());
    context.source = "cursor".to_owned();
    context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(10.0),
        ..Default::default()
    });
    write(&runtime, "cursor", "sess-1", &context).unwrap();
    let second = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-2".to_owned(),
        cost_usd: 0.5,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &second).unwrap());

    let record = read_one(&runtime, "cursor", "sess-1").unwrap();
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(10.0)
    );
    assert_eq!(
        record.context.cost.as_ref().map(|cost| cost.basis),
        Some(crate::agents::CostBasis::ProviderReported)
    );
}

#[test]
fn statusline_and_locally_priced_cost_writers_serialize_without_lost_fields() {
    let (_dir, runtime) = runtime();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let statusline_runtime = runtime.clone();
    let statusline_barrier = barrier.clone();
    let statusline = std::thread::spawn(move || {
        let mut context = ctx(Timestamp::now());
        context.source = "cursor".to_owned();
        context.model_display_name = Some("Auto".to_owned());
        statusline_barrier.wait();
        write(&statusline_runtime, "cursor", "sess-race", &context).unwrap();
    });
    let cost_runtime = runtime.clone();
    let cost_barrier = barrier.clone();
    let cost = std::thread::spawn(move || {
        cost_barrier.wait();
        merge_locally_priced_cost(
            &cost_runtime,
            "cursor",
            "sess-race",
            &crate::agents::LocallyPricedTurnCost {
                turn_id: "gen-1".to_owned(),
                cost_usd: 0.25,
            },
        )
        .unwrap();
    });
    barrier.wait();
    statusline.join().unwrap();
    cost.join().unwrap();

    let record = read_one(&runtime, "cursor", "sess-race").unwrap();
    assert_eq!(record.context.model_display_name.as_deref(), Some("Auto"));
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.25)
    );
}

#[test]
fn old_record_is_read_liveness_gating_is_the_rollups_job() {
    let (_dir, runtime) = runtime();
    let old = Timestamp::from_second(0).unwrap();
    write(&runtime, "claude", "sess-old", &ctx(old)).unwrap();

    let all = read_all(&runtime);

    assert_eq!(all.len(), 1);
    assert_eq!(all[0].agent_id, "sess-old");
    assert_eq!(all[0].context.observed_at, old);
}

mod merge;
