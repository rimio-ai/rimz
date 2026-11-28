//! Which checkouts and branches the room's condition tasks, forge signal bindings and active
//! workflow runs ask the forge about, read by the sidebar's heavy-lane refresh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;

fn probe_paths(
    entry: &crate::config::TaskEntry,
    key: &str,
    owned: &[std::path::PathBuf],
    records: &super::task_record::TaskRecords,
) -> Vec<std::path::PathBuf> {
    if !entry.each_worktree {
        return vec![entry.run_dir()];
    }
    owned
        .iter()
        .map(|path| crate::utils::path::normalize_path_lexical(path))
        .filter(|path| records.launched(entry, key, path).is_none())
        .collect()
}

/// The branch a `ci` or `pr` subscription names by a literal `branch` match.
fn signal_branch(
    selector: &crate::harness::signal::SignalSelector,
    matches: &BTreeMap<String, String>,
) -> Option<String> {
    if !matches!(selector.family(), "ci" | "pr") {
        return None;
    }
    matches
        .get("branch")
        .filter(|branch| !crate::harness::signal::is_match_wildcard(branch))
        .cloned()
}

/// Checkout scopes and branch selectors needed by live loops and Active workflows. A live
/// forge signal binding holds the branch its `branch` match names under the project root.
pub(crate) fn probe_demand(
    runtime: &crate::RuntimePaths,
    project_root: Option<&Path>,
) -> BTreeMap<String, BTreeSet<String>> {
    let now = Timestamp::now();
    // An unreadable task record stops every fire, so no task asks the forge.
    let records = super::task_record::load()
        .inspect_err(|error| tracing::warn!(%error, "loop task record unavailable for probes"))
        .ok();
    let arming = records
        .as_ref()
        .map(|records| records.by_key())
        .unwrap_or_default();
    let mut signal_branches = BTreeSet::new();
    let tasks: Vec<_> = super::fire::runnable_tasks_for(runtime, project_root)
        .into_iter()
        .filter(|_| records.is_some())
        .filter_map(|(name, task)| {
            let armed = arming
                .get(&task.key(&name))
                .and_then(|(_, record)| record.arming.as_ref());
            if super::arming::ArmState::resolve(armed, task.source(), now)
                != super::arming::ArmState::Live
            {
                return None;
            }
            match &task.trigger().as_ref().ok()?.trigger {
                super::Trigger::Condition { expr, .. } => {
                    expr.reads_forge().then_some((name, task))
                }
                super::Trigger::Signal { selector, matches } => {
                    signal_branches.extend(signal_branch(selector, matches));
                    None
                }
                super::Trigger::Schedule(_) | super::Trigger::Watch(_) => None,
            }
        })
        .collect();
    let records = records.unwrap_or_default();
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
        for scope in probe_paths(task.entry(), &task.key(&name), &owned, &records)
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
    if !signal_branches.is_empty() && project_root.is_dir() {
        demand
            .entry(project_root.to_string_lossy().into_owned())
            .or_default()
            .extend(signal_branches);
    }
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
        let dir = record.definition_dir(&paths);
        let branches =
            match crate::harness::workflow::load(&dir).and_then(|spec| spec.forge_branches()) {
                Ok(Some(branches)) => branches,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        path = %dir.display(), %error, "workflow probe spec unavailable"
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

    fn binding(name: &str) -> super::super::task_record::Binding {
        super::super::task_record::Binding {
            scope: super::super::task_record::Scope::Machine,
            name: name.to_owned(),
        }
    }

    #[test]
    fn worktree_probe_paths_exclude_completed_checkouts_without_canonicalizing() {
        let owned = vec![
            std::path::PathBuf::from("/alias/spare/../one"),
            std::path::PathBuf::from("/alias/spare/../two"),
        ];
        let (records, _) = super::super::task_record::TaskRecords::marked(
            binding("fixer"),
            [(
                std::path::PathBuf::from("/alias/one"),
                crate::store::workflow::ScopeKind::Worktree,
            )],
        );
        let entry = crate::config::TaskEntry {
            once_per_checkout: Some(true),
            each_worktree: true,
            when: Some(vec!["pr=merged".into()]),
            ..Default::default()
        };
        assert_eq!(
            probe_paths(&entry, "machine::fixer", &owned, &records),
            vec![std::path::PathBuf::from("/alias/two")]
        );
    }

    #[test]
    fn workflow_probe_paths_exclude_workflow_launches() {
        use super::super::task_record::TaskRecords;
        use crate::store::workflow::ScopeKind;
        let owned = vec!["/alias/spare/../one".into(), "/alias/spare/../two".into()];
        let entry = crate::config::TaskEntry {
            once_per_checkout: Some(true),
            each_worktree: true,
            when: Some(vec!["pr=open".into()]),
            ..Default::default()
        };
        let (worktree, _) = TaskRecords::marked(
            binding("land"),
            [("/alias/one".into(), ScopeKind::Worktree)],
        );
        let (project, _) = TaskRecords::marked(
            binding("land"),
            [
                ("/alias/one".into(), ScopeKind::Worktree),
                ("/project".into(), ScopeKind::Project),
            ],
        );
        assert_eq!(
            (
                probe_paths(&entry, "machine::land", &owned, &worktree),
                probe_paths(&entry, "machine::land", &owned, &project)
            ),
            (vec![std::path::PathBuf::from("/alias/two")], vec![])
        );
    }
}
