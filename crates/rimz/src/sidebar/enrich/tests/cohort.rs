use super::*;
use crate::sidebar::refresh::cohort_spend::{COHORT_SPEND_CACHE_VERSION, CohortSpendCache};

#[test]
fn published_pipeline_projects_stuck_attention_and_clears_on_fresh_member_activity() {
    let (dir, runtime, mut snapshot) = runtime();
    let path = dir.path().join("lane");
    let stale = snapshot.now - SignedDuration::from_secs(1_801);
    let rows = ["coder", "reviewer"].map(|handle| {
        let mut row = activity_row(true, Some(AgentStatus::Success), stale, &path);
        row.id = handle.into();
        let card = row.as_agent_mut().unwrap();
        card.handle = Some(handle.into());
        card.team = Some("crew".into());
        row
    });
    let mut group = worktree_group(&path, rows.into());
    group.team = Some("crew".into());
    atomic::write_temp_then_rename_cache(
        &runtime.pipeline_path(),
        &serde_json::json!({
            "version": 2,
            "groups": { group.key.clone(): {
                "stages": ["Implement"], "stage": "Implement", "owner": "coder",
                "started_at": stale, "stage_started_at": stale, "done_at": null
            }}
        }),
    )
    .unwrap();
    snapshot.worktree_groups = vec![group].into();
    let projected = fold_producing(snapshot.clone(), None, &runtime);
    let owner = projected.rows().find(|row| row.id == "coder").unwrap();
    assert_eq!(owner.status(), Some(AgentStatus::Failed));
    assert_eq!(
        owner.turn_error_label(),
        Some("stage Implement: nothing running 30m")
    );
    let mut episodes = crate::sidebar::unread::UnreadEpisodes::default();
    let mut projected = projected;
    projected.panes_produced_at_ms = Some(projected.now.as_millisecond() as u64);
    episodes.reconcile(&mut projected, &Default::default(), false);
    assert!(
        projected
            .rows()
            .find(|row| row.id == "coder")
            .unwrap()
            .unread
    );

    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[1]
    .last_activity = snapshot.now;
    let cleared = fold_producing(snapshot, None, &runtime);
    let owner = cleared.rows().find(|row| row.id == "coder").unwrap();
    assert_eq!(owner.status(), Some(AgentStatus::Success));
    assert_eq!(owner.turn_error_label(), None);
}

#[test]
fn cohort_effort_projects_by_group_key_and_clears_misses() {
    let (dir, _runtime, mut snapshot) = runtime();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    snapshot.worktree_groups = vec![
        worktree_group(&first, Vec::new()),
        worktree_group(&second, Vec::new()),
    ]
    .into();
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].cohort_effort =
        Some(crate::store::snapshot::SidebarCohortEffort {
            cost_usd: Some(99.0),
            ..crate::store::snapshot::SidebarCohortEffort::default()
        });
    let expected = crate::store::snapshot::SidebarCohortEffort {
        cost_usd: Some(1.25),
        tokens: crate::agents::spending::EffortTokens {
            input: 10,
            output: 2,
            cache_write: 3,
            cache_read: 4,
        },
        active_secs: Some(60),
        ..crate::store::snapshot::SidebarCohortEffort::default()
    };
    let cache = CohortSpendCache {
        version: COHORT_SPEND_CACHE_VERSION,
        refreshed_at_ms: 1,
        groups: BTreeMap::from([(first.display().to_string(), expected.clone())]),
    };

    project_cohort_effort(&mut snapshot, &cache);

    assert_eq!(snapshot.worktree_groups[0].cohort_effort, Some(expected));
    assert_eq!(snapshot.worktree_groups[1].cohort_effort, None);
}
