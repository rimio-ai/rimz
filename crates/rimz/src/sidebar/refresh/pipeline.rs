//! Producer-published team pipeline state from each cohort's blackboard.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::config::{MachineConfig, TeamsConfig};
use crate::store::snapshot::{SidebarPipeline, SidebarSnapshot, SidebarWorktreeGroup};
use crate::utils::path::normalize_path_lexical;

const PIPELINE_CACHE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct PipelineCache {
    pub version: u32,
    pub groups: BTreeMap<String, SidebarPipeline>,
}

pub(in crate::sidebar) fn read_pipeline_cache(path: &Path) -> PipelineCache {
    let cache: PipelineCache = crate::disk::atomic::read_json_cache(path);
    if cache.version == PIPELINE_CACHE_VERSION {
        cache
    } else {
        PipelineCache::default()
    }
}

pub(super) fn refresh_pipeline_for(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
) {
    if !snapshot
        .worktree_groups
        .iter()
        .any(|group| group.team.is_some())
    {
        return;
    }
    let local_teams = snapshot.project_root.as_deref().and_then(|root| {
        crate::config::effective::load(config, root)
            .ok()
            .map(|agents| agents.teams)
    });
    let teams = local_teams.as_ref().unwrap_or(&config.agents.teams);
    let refreshed = PipelineCache {
        version: PIPELINE_CACHE_VERSION,
        groups: compute_pipelines(&snapshot.worktree_groups, teams, &config.time_zone()),
    };
    let path = runtime.pipeline_path();
    if refreshed == read_pipeline_cache(&path) {
        return;
    }
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refreshed) {
        tracing::debug!(path = %path.display(), %error, "sidebar pipeline cache write failed");
    }
}

fn compute_pipelines(
    groups: &[SidebarWorktreeGroup],
    teams: &TeamsConfig,
    zone: &jiff::tz::TimeZone,
) -> BTreeMap<String, SidebarPipeline> {
    let mut computed = BTreeMap::new();
    for group in groups {
        let Some(name) = group.team.as_deref() else {
            continue;
        };
        let Some(team) = teams.0.get(name).filter(|team| team.staged()) else {
            continue;
        };
        let paths = group
            .rows
            .iter()
            .filter(|row| row.team() == Some(name))
            .filter_map(|row| row.worktree_path.as_deref())
            .filter(|path| !path.is_empty())
            .map(|path| normalize_path_lexical(Path::new(path)))
            .collect::<BTreeSet<_>>();
        if paths.len() != 1 {
            continue;
        }
        let Some(run) = paths
            .first()
            .and_then(|path| crate::harness::scratch::board_run(path, zone))
        else {
            continue;
        };
        computed.insert(
            group.key.clone(),
            SidebarPipeline {
                stages: team.pipeline_stages(),
                stage: run.stage.name,
                owner: run.stage.owner,
                started_at: run.started_at,
                done_at: run.done_at,
            },
        );
    }
    computed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Team;
    use crate::sidebar::test_support::{activity_row, worktree_group};

    fn group(path: &Path) -> SidebarWorktreeGroup {
        let mut row = activity_row(true, None, jiff::Timestamp::UNIX_EPOCH, path);
        if let crate::store::snapshot::RowCard::Agent(card) = &mut row.card {
            card.team = Some("forge".to_owned());
        }
        let mut group = worktree_group(path, vec![row]);
        group.team = Some("forge".to_owned());
        group
    }

    fn teams() -> TeamsConfig {
        TeamsConfig(BTreeMap::from([(
            "forge".to_owned(),
            Team {
                stages: vec!["Plan".to_owned(), "Build".to_owned()],
                ..Team::default()
            },
        )]))
    }

    fn board(path: &Path) {
        std::fs::write(path.join("blackboard.md"), "Stage: Build (@coder)\n\n## Progress\n- 2026-09-19 12:00:01 @user: opened Plan — start\n").unwrap();
    }

    #[test]
    fn pipeline_requires_staged_team_and_unique_board_root() {
        let dir = tempfile::tempdir().unwrap();
        board(dir.path());
        let group = group(dir.path());
        let teams = teams();
        let compute = |group: SidebarWorktreeGroup, teams: &TeamsConfig| {
            compute_pipelines(&[group], teams, &jiff::tz::TimeZone::UTC)
        };
        let result = compute(group.clone(), &teams);
        let pipeline = &result[&group.key];
        assert_eq!(pipeline.stages, ["Plan", "Build"]);
        assert_eq!(pipeline.stage, "Build");
        assert_eq!(pipeline.owner.as_deref(), Some("coder"));
        assert_eq!(
            pipeline.started_at,
            Some("2026-09-19T12:00:01Z".parse().unwrap())
        );
        assert_eq!(pipeline.done_at, None);

        let mut normalized = group.clone();
        let mut duplicate = normalized.rows[0].clone();
        duplicate.worktree_path = Some(dir.path().join("sub/..").display().to_string());
        normalized.rows.push(duplicate);
        assert_eq!(compute(normalized, &teams), result);

        assert!(compute(group.clone(), &TeamsConfig::default()).is_empty());
        let unstaged = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
        assert!(compute(group.clone(), &unstaged).is_empty());
        let mut ambiguous = group.clone();
        let mut other = ambiguous.rows[0].clone();
        other.worktree_path = Some(dir.path().join("other").display().to_string());
        ambiguous.rows.push(other);
        assert!(compute(ambiguous, &teams).is_empty());
        let mut unteamed = group.clone();
        unteamed.team = None;
        assert!(compute(unteamed, &teams).is_empty());
        let mut empty = group.clone();
        empty.rows[0].worktree_path = Some(String::new());
        assert!(compute(empty, &teams).is_empty());
        std::fs::remove_file(dir.path().join("blackboard.md")).unwrap();
        assert!(compute(group, &teams).is_empty());
    }

    #[test]
    fn refresh_preserves_unchanged_publication_and_gates_versions() {
        let dir = tempfile::tempdir().unwrap();
        board(dir.path());
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), jiff::Timestamp::UNIX_EPOCH);
        snapshot.project_root = None;
        snapshot.worktree_groups = vec![group(dir.path())];
        let mut config = MachineConfig::default();
        config.agents.teams = teams();
        refresh_pipeline_for(&snapshot, &runtime, &config);
        let path = runtime.pipeline_path();
        let cache = read_pipeline_cache(&path);
        assert_eq!(cache.groups.len(), 1);
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        refresh_pipeline_for(&snapshot, &runtime, &config);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), old);
        let stale = PipelineCache {
            version: PIPELINE_CACHE_VERSION + 1,
            ..cache
        };
        crate::disk::atomic::write_temp_then_rename_cache(&path, &stale).unwrap();
        assert_eq!(read_pipeline_cache(&path), PipelineCache::default());
        snapshot.worktree_groups[0].team = None;
        refresh_pipeline_for(&snapshot, &runtime, &config);
        assert_eq!(
            crate::disk::atomic::read_json_cache::<PipelineCache>(&path),
            stale
        );
    }
}
