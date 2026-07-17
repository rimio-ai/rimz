//! Producer-owned publication of provider-local session discovery.
//!
//! Adapters validate provider stores once per represented kind. Every renderer
//! then folds the normalized observations covered by both the publication and
//! the current room's admitted `(kind, workspace)` input set.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::LocalSessionObservation;
use crate::ids::AgentKind;
use crate::pane::PaneRef;
use crate::store::parse_cache::ParseCache;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalSessionInputs {
    by_kind: BTreeMap<AgentKind, Vec<PathBuf>>,
}

impl LocalSessionInputs {
    pub fn from_panes(panes: &[PaneRef]) -> Self {
        let mut by_kind = BTreeMap::<AgentKind, BTreeSet<PathBuf>>::new();
        for pane in panes {
            let Some(kind) = crate::store::snapshot::pane_agent_kind(pane) else {
                continue;
            };
            let Some(adapter) = crate::agents::find_adapter(kind) else {
                continue;
            };
            if !adapter.descriptor().capabilities.local_session_discovery {
                continue;
            }
            let Some(workspace) = crate::store::snapshot::pane_worktree_path(pane) else {
                continue;
            };
            let workspace = crate::worktree::normalize_path_lexical(Path::new(workspace));
            if !workspace.is_absolute() {
                continue;
            }
            by_kind
                .entry(AgentKind::new_unchecked(kind))
                .or_default()
                .insert(workspace);
        }
        Self {
            by_kind: by_kind
                .into_iter()
                .map(|(kind, workspaces)| (kind, workspaces.into_iter().collect()))
                .collect(),
        }
    }

    fn discover_with(
        &self,
        mut discover: impl FnMut(&AgentKind, &[&Path]) -> Vec<LocalSessionObservation>,
    ) -> Vec<LocalSessionObservation> {
        let mut observations = self
            .by_kind
            .iter()
            .flat_map(|(kind, workspaces)| {
                let workspaces = workspaces.iter().map(PathBuf::as_path).collect::<Vec<_>>();
                discover(kind, &workspaces)
            })
            .collect::<Vec<_>>();
        normalize_observations(&mut observations);
        observations
    }

    fn normalized_keys(&self) -> BTreeSet<(AgentKind, PathBuf)> {
        self.by_kind
            .iter()
            .flat_map(|(kind, workspaces)| {
                workspaces.iter().map(|workspace| {
                    (
                        kind.clone(),
                        crate::worktree::normalize_path_lexical(workspace),
                    )
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublishedLocalSessions {
    pub session_name: String,
    pub inputs: LocalSessionInputs,
    pub observations: Vec<LocalSessionObservation>,
}

thread_local! {
    static LOCAL_SESSION_PARSE_CACHE: ParseCache<PublishedLocalSessions> = const { ParseCache::new() };
}

/// Discover once per represented provider kind, publish semantic changes, and
/// return the fresh observations directly to the producer fold.
pub fn refresh_published(
    runtime: &RuntimePaths,
    session_name: &str,
    inputs: LocalSessionInputs,
) -> Vec<LocalSessionObservation> {
    let observations = inputs.discover_with(|kind, workspaces| {
        crate::agents::find_adapter(kind.as_str())
            .map(|adapter| adapter.discover_local_sessions(workspaces))
            .unwrap_or_default()
    });
    let published = PublishedLocalSessions {
        session_name: session_name.to_owned(),
        inputs,
        observations: observations.clone(),
    };
    match publish_if_changed(&runtime.local_sessions_path(), &published, || {
        if let Err(err) = crate::store::wakeup::wake_sidebars(runtime) {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "local-session publication could not wake sidebars",
            );
        }
    }) {
        Ok(true) => {}
        Ok(false) => {}
        Err(err) => tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "local-session publication failed",
        ),
    }
    observations
}

/// Read the safe same-session intersection of published and current inputs.
/// Newly added inputs stay absent until their producer publishes them, while
/// removed inputs disappear immediately. Every miss fails closed and never
/// invokes an adapter.
pub fn read_published(
    runtime: &RuntimePaths,
    session_name: &str,
    inputs: &LocalSessionInputs,
) -> Vec<LocalSessionObservation> {
    let Some(published) = read_cache(&runtime.local_sessions_path())
        .filter(|published| published.session_name == session_name)
    else {
        return Vec::new();
    };
    let admitted = published
        .inputs
        .normalized_keys()
        .intersection(&inputs.normalized_keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    published
        .observations
        .iter()
        .filter(|observation| {
            admitted.contains(&(
                observation.kind.clone(),
                crate::worktree::normalize_path_lexical(&observation.workspace),
            ))
        })
        .cloned()
        .collect()
}

fn normalize_observations(observations: &mut Vec<LocalSessionObservation>) {
    for observation in observations.iter_mut() {
        observation.workspace = crate::worktree::normalize_path_lexical(&observation.workspace);
        observation.transcript_path =
            crate::worktree::normalize_path_lexical(&observation.transcript_path);
    }
    observations
        .sort_by_cached_key(|observation| serde_json::to_vec(observation).unwrap_or_default());
    observations.dedup();
}

fn publish_if_changed(
    path: &Path,
    published: &PublishedLocalSessions,
    on_changed: impl FnOnce(),
) -> crate::store::atomic::Result<bool> {
    if std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PublishedLocalSessions>(&bytes).ok())
        .as_ref()
        == Some(published)
    {
        return Ok(false);
    }
    crate::store::atomic::write_temp_then_rename_cache(path, published)?;
    on_changed();
    Ok(true)
}

fn read_cache(path: &Path) -> Option<Arc<PublishedLocalSessions>> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let len = metadata.len();
    if let Some(cached) = LOCAL_SESSION_PARSE_CACHE.with(|cache| cache.get(path, modified, len)) {
        return Some(cached);
    }
    let parsed = Arc::new(serde_json::from_slice(&std::fs::read(path).ok()?).ok()?);
    LOCAL_SESSION_PARSE_CACHE.with(|cache| {
        cache.store(path, modified, len, Arc::clone(&parsed));
    });
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use jiff::Timestamp;

    use super::*;
    use crate::agents::LocalSessionProjection;
    use crate::ids::{AgentSessionId, WorkspaceId};

    fn observation(workspace: &Path, session: &str) -> LocalSessionObservation {
        let now = Timestamp::from_second(1_750_000_000).expect("fixed timestamp");
        LocalSessionObservation {
            kind: AgentKind::new_unchecked("kiro"),
            session_id: AgentSessionId::from(session),
            workspace: workspace.to_path_buf(),
            transcript_path: workspace.join(format!("{session}.json")),
            created_at: now,
            fresh_binding_at: Some(now),
            first_event_at: Some(now),
            last_activity: now,
            projection: LocalSessionProjection::IdentityOnly,
        }
    }

    fn inputs(root: &Path) -> LocalSessionInputs {
        LocalSessionInputs {
            by_kind: BTreeMap::from([(
                AgentKind::new_unchecked("kiro"),
                vec![root.join("a"), root.join("b")],
            )]),
        }
    }

    #[test]
    fn discovery_batches_all_same_kind_workspaces_once() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = inputs(dir.path());
        let calls = Cell::new(0);
        let observed = inputs.discover_with(|kind, workspaces| {
            calls.set(calls.get() + 1);
            assert_eq!(kind.as_str(), "kiro");
            assert_eq!(workspaces.len(), 2);
            vec![
                observation(workspaces[1], "b"),
                observation(workspaces[0], "a"),
            ]
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(observed[0].session_id.as_str(), "a");
        assert_eq!(observed[1].session_id.as_str(), "b");
    }

    #[test]
    fn publication_is_semantic_and_intersection_reads_only() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let inputs = inputs(dir.path());
        let publication = PublishedLocalSessions {
            session_name: "room".to_owned(),
            inputs: inputs.clone(),
            observations: vec![observation(
                &inputs.by_kind[&AgentKind::new_unchecked("kiro")][0],
                "a",
            )],
        };
        let wakes = Cell::new(0);

        assert!(
            publish_if_changed(&runtime.local_sessions_path(), &publication, || {
                assert_eq!(
                    serde_json::from_slice::<PublishedLocalSessions>(
                        &std::fs::read(runtime.local_sessions_path()).unwrap()
                    )
                    .unwrap(),
                    publication,
                    "publication is visible before its wake"
                );
                wakes.set(wakes.get() + 1);
            })
            .unwrap()
        );
        assert!(
            !publish_if_changed(&runtime.local_sessions_path(), &publication, || {
                wakes.set(wakes.get() + 1);
            })
            .unwrap()
        );
        assert_eq!(wakes.get(), 1, "identical data neither rewrites nor wakes");
        assert_eq!(
            read_published(&runtime, "room", &inputs),
            publication.observations
        );
        assert!(read_published(&runtime, "other", &inputs).is_empty());
        let mut changed_inputs = inputs.clone();
        changed_inputs
            .by_kind
            .get_mut(&AgentKind::new_unchecked("kiro"))
            .unwrap()
            .push(dir.path().join("c"));
        assert_eq!(
            read_published(&runtime, "room", &changed_inputs),
            publication.observations,
            "new inputs wait for a matching producer publication"
        );

        let first = read_cache(&runtime.local_sessions_path()).unwrap();
        let second = read_cache(&runtime.local_sessions_path()).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged file reuses parse cache"
        );

        let mut changed = publication.clone();
        changed.observations.push(observation(dir.path(), "c"));
        assert!(publish_if_changed(&runtime.local_sessions_path(), &changed, || {}).unwrap());
        assert_eq!(
            read_published(&runtime, "room", &inputs),
            publication.observations,
            "observations outside the producer's admitted inputs stay hidden"
        );
    }

    #[test]
    fn publication_filters_removed_and_mixed_inputs_without_cross_session_leaks() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let inputs = inputs(dir.path());
        let publication = PublishedLocalSessions {
            session_name: "room".to_owned(),
            inputs: inputs.clone(),
            observations: vec![
                observation(&inputs.by_kind[&AgentKind::new_unchecked("kiro")][0], "a"),
                observation(&inputs.by_kind[&AgentKind::new_unchecked("kiro")][1], "b"),
            ],
        };
        publish_if_changed(&runtime.local_sessions_path(), &publication, || {}).unwrap();

        let mut current = inputs.clone();
        current
            .by_kind
            .get_mut(&AgentKind::new_unchecked("kiro"))
            .unwrap()
            .remove(0);
        current
            .by_kind
            .get_mut(&AgentKind::new_unchecked("kiro"))
            .unwrap()
            .push(dir.path().join("c"));
        assert_eq!(
            read_published(&runtime, "room", &current),
            vec![observation(
                &inputs.by_kind[&AgentKind::new_unchecked("kiro")][1],
                "b"
            )],
            "unchanged observations survive a mixed remove/add"
        );
        assert!(read_published(&runtime, "other", &current).is_empty());
    }

    #[test]
    fn absent_or_malformed_publication_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let inputs = inputs(dir.path());
        assert!(read_published(&runtime, "room", &inputs).is_empty());
        std::fs::write(runtime.local_sessions_path(), b"not json").unwrap();
        assert!(read_published(&runtime, "room", &inputs).is_empty());
    }
}
