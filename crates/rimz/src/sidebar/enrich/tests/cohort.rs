use super::*;
use crate::sidebar::refresh::cohort_spend::{COHORT_SPEND_CACHE_VERSION, CohortSpendCache};

#[test]
fn cohort_effort_projects_by_group_key_and_clears_misses() {
    let (dir, _runtime, mut snapshot) = runtime();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    snapshot.worktree_groups = vec![
        worktree_group(&first, Vec::new()),
        worktree_group(&second, Vec::new()),
    ];
    snapshot.worktree_groups[1].cohort_effort = Some(crate::store::snapshot::SidebarCohortEffort {
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
