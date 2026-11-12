//! Which checkouts and branches the room's condition tasks and active workflow runs ask the
//! forge about, read by the sidebar's heavy-lane refresh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;

fn probe_paths(
    entry: &crate::config::TaskEntry,
    name: &str,
    owned: &[std::path::PathBuf],
    workflows: &super::launch_ledger::WorkflowLedger,
) -> Vec<std::path::PathBuf> {
    if !entry.each_worktree {
        return vec![entry.run_dir()];
    }
    owned
        .iter()
        .map(|path| crate::utils::path::normalize_path_lexical(path))
        .filter(|path| super::launch_ledger::launched(entry, name, path, workflows).is_none())
        .collect()
}

/// Checkout scopes and branch selectors needed by live loops and Active workflows.
pub(crate) fn probe_demand(
    runtime: &crate::RuntimePaths,
    project_root: Option<&Path>,
) -> BTreeMap<String, BTreeSet<String>> {
    let arming = super::arming::load();
    let now = Timestamp::now();
    let mut tasks: Vec<_> = super::fire::runnable_tasks_for(runtime, project_root)
        .into_iter()
        .filter_map(|(name, task)| {
            if super::arming::ArmState::resolve(arming.get(&task.key(&name)), task.source(), now)
                != super::arming::ArmState::Live
            {
                return None;
            }
            let super::Trigger::Condition { expr, .. } = &task.trigger().as_ref().ok()?.trigger
            else {
                return None;
            };
            expr.reads_forge().then_some((name, task))
        })
        .collect();
    let remembers = |task: &super::catalog::LoadedTask| {
        task.entry().each_worktree && task.entry().dedupes_checkout()
    };
    let workflows = if tasks.iter().any(|(_, task)| remembers(task)) {
        match super::launch_ledger::load_workflows_room(runtime, project_root) {
            Ok(ledger) => ledger,
            Err(error) => {
                tracing::warn!(%error, "workflow loop ledger unavailable for probes");
                tasks.retain(|(_, task)| !remembers(task));
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let owned = tasks
        .iter()
        .find(|(_, task)| task.entry().each_worktree)
        .map(|(_, task)| {
            crate::worktree::discover_owned_cached(
                project_root.unwrap_or(&task.entry().resolved_root()),
            )
        })
        .transpose()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "enumerating loop probe worktrees");
            None
        })
        .unwrap_or_default()
        .into_iter()
        .map(|worktree| worktree.marker.worktree_path)
        .collect::<Vec<_>>();
    let mut demand = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, task) in tasks {
        let Ok(super::ParsedTrigger {
            trigger: super::Trigger::Condition { expr, .. },
            ..
        }) = task.trigger()
        else {
            continue;
        };
        let branches = expr.forge_branches().collect::<BTreeSet<_>>();
        for scope in probe_paths(task.entry(), &name, &owned, &workflows)
            .into_iter()
            .filter(|scope| scope.is_dir())
        {
            demand
                .entry(scope.to_string_lossy().into_owned())
                .or_default()
                .extend(branches.iter().map(|branch| (*branch).to_owned()));
        }
    }
    let Some(project_root) = project_root else {
        tracing::warn!("workflow probe demand has no project root");
        return demand;
    };
    let state = match crate::StatePaths::for_project_root(project_root) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "workflow probes could not resolve state paths");
            return demand;
        }
    };
    let records = match crate::store::workflow::live_runs(&state) {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(%error, "workflow probes could not read runs");
            return demand;
        }
    };
    for record in records
        .into_iter()
        .filter(|record| record.state == crate::store::workflow::RunState::Active)
    {
        let paths = state.workflow(&record.id);
        let branches = match crate::harness::workflow::load(&paths.home)
            .and_then(|spec| spec.forge_branches())
        {
            Ok(Some(branches)) => branches,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    path = %paths.spec.display(), %error, "workflow probe spec unavailable"
                );
                continue;
            }
        };
        demand
            .entry(record.scope.path.to_string_lossy().into_owned())
            .or_default()
            .extend(branches);
    }
    demand
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_probe_paths_exclude_completed_checkouts_without_canonicalizing() {
        let owned = vec![
            std::path::PathBuf::from("/alias/spare/../one"),
            std::path::PathBuf::from("/alias/spare/../two"),
        ];
        let mut ledger = super::super::launch_ledger::WorkflowLedger::default();
        ledger.tasks.insert(
            "fixer".to_owned(),
            BTreeMap::from([(
                std::path::PathBuf::from("/alias/one"),
                super::super::launch_ledger::WorkflowLaunchRecord {
                    at: Timestamp::now(),
                    run: None,
                    scope: crate::store::workflow::ScopeKind::Worktree,
                    leader: Some("otter".into()),
                    destination: None,
                },
            )]),
        );
        let entry = crate::config::TaskEntry {
            stay: true,
            each_worktree: true,
            when: Some(vec!["pr=merged".into()]),
            ..Default::default()
        };
        assert_eq!(
            probe_paths(&entry, "fixer", &owned, &ledger),
            vec![std::path::PathBuf::from("/alias/two")]
        );
    }

    #[test]
    fn workflow_probe_paths_exclude_workflow_launches() {
        use super::super::launch_ledger::{WorkflowLaunchRecord, WorkflowLedger};
        use crate::store::workflow::ScopeKind;
        let owned = vec!["/alias/spare/../one".into(), "/alias/spare/../two".into()];
        let mut workflows = WorkflowLedger::default();
        let record = WorkflowLaunchRecord {
            at: Timestamp::now(),
            run: Some(crate::ids::WorkflowRunId::new()),
            scope: ScopeKind::Worktree,
            leader: None,
            destination: None,
        };
        workflows.tasks.insert(
            "land".into(),
            BTreeMap::from([("/alias/one".into(), record.clone())]),
        );
        let entry = crate::config::TaskEntry {
            workflow: Some("review".into()),
            each_worktree: true,
            when: Some(vec!["pr=open".into()]),
            ..Default::default()
        };
        let worktree = probe_paths(&entry, "land", &owned, &workflows);
        workflows.tasks.get_mut("land").unwrap().insert(
            "/project".into(),
            WorkflowLaunchRecord {
                scope: ScopeKind::Project,
                ..record
            },
        );
        let project = probe_paths(&entry, "land", &owned, &workflows);
        assert_eq!(
            (worktree, project),
            (vec![std::path::PathBuf::from("/alias/two")], vec![])
        );
    }
}
