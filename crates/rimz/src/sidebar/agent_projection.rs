//! Producer-owned publication of adapter wiring and provider-local sessions.
//!
//! The elected producer probes provider configuration behind an exact-stamp
//! process memo. Renderers consume only the normalized same-session cache.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::agents::LocalSessionObservation;
use crate::ids::AgentKind;
use crate::pane::PaneRef;
use crate::store::RuntimePaths;
use crate::store::parse_cache::ParseCache;

/// Agent kinds admitted for sessionless idle synthesis and their launch-model
/// fallbacks, in adapter display order.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WiredAgentProjection {
    pub kinds: Vec<String>,
    pub default_models: BTreeMap<String, String>,
}

/// Projections consumed together by one sidebar fold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentProjection {
    pub wiring: WiredAgentProjection,
    pub local_sessions: Vec<LocalSessionObservation>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalSessionInputs {
    by_kind: BTreeMap<AgentKind, Vec<PathBuf>>,
}

impl LocalSessionInputs {
    pub fn from_panes(panes: &[PaneRef]) -> Self {
        let mut by_kind = BTreeMap::<AgentKind, BTreeSet<PathBuf>>::new();
        for pane in panes {
            let Some(kind) = candidate_pane_agent_kind(pane) else {
                continue;
            };
            let Some(adapter) = crate::agents::find_definition(kind) else {
                continue;
            };
            if !adapter.spec().capabilities.local_session_discovery {
                continue;
            }
            let Some(workspace) = crate::store::snapshot::pane_worktree_path(pane) else {
                continue;
            };
            let workspace = crate::worktree::normalize_path_lexical(Path::new(workspace));
            if workspace.is_absolute() {
                by_kind
                    .entry(AgentKind::new_unchecked(kind))
                    .or_default()
                    .insert(workspace);
            }
        }
        Self {
            by_kind: by_kind
                .into_iter()
                .map(|(kind, workspaces)| (kind, workspaces.into_iter().collect()))
                .collect(),
        }
    }

    fn discover(&self) -> Vec<LocalSessionObservation> {
        self.discover_with(|kind, workspaces| {
            crate::agents::find_definition(kind.as_str())
                .map(|adapter| adapter.discover_local_sessions(workspaces))
                .unwrap_or_default()
        })
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

/// Candidate identity feeds only provider-local discovery. Its observations
/// still require an exact durable or resume binding before they can affect a
/// card, so admitting an ambiguous basename here preserves hook-bound Cursor
/// enrichment without turning that basename into pane presence or routing.
fn candidate_pane_agent_kind(pane: &PaneRef) -> Option<&'static str> {
    pane.spawn_command
        .as_deref()
        .and_then(crate::agents::registry::command_agent_kind_candidate)
        .or_else(|| {
            pane.command
                .as_deref()
                .and_then(crate::agents::registry::command_agent_kind_candidate)
        })
        .or_else(|| {
            pane.hosted_agent_kind
                .as_ref()
                .and_then(|kind| crate::agents::spec_by_kind(kind.as_str()))
                .map(|definition| definition.kind)
        })
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct InputStamp {
    path: PathBuf,
    present: bool,
    modified_secs: u64,
    modified_nanos: u32,
    len: u64,
}

#[derive(Clone, Debug)]
struct MemoizedProjection {
    inputs: Vec<InputStamp>,
    projection: WiredAgentProjection,
}

static WIRING_MEMO: Mutex<Option<MemoizedProjection>> = Mutex::new(None);

/// Probe the current wiring projection. Unchanged provider inputs pay only
/// metadata checks; a raced edit keeps the last stable projection and retries
/// on the next call.
pub fn probe_current() -> WiredAgentProjection {
    let mut paths = crate::agents::all_definitions()
        .flat_map(|adapter| adapter.wiring_input_paths())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    probe_with(&WIRING_MEMO, &paths, probe_adapters)
}

fn probe_adapters() -> WiredAgentProjection {
    let mut projection = WiredAgentProjection::default();
    for agent in crate::agents::all_definitions() {
        let definition = agent.spec();
        let wired = definition.capabilities.local_session_discovery
            || (definition.has_wired_hook_install() && agent.hooks_installed());
        if !wired {
            continue;
        }
        projection.kinds.push(definition.kind.to_owned());
        if let Some(model) = agent.default_launch_model() {
            projection
                .default_models
                .insert(definition.kind.to_owned(), model);
        }
    }
    projection
}

fn probe_with(
    memo: &Mutex<Option<MemoizedProjection>>,
    paths: &[PathBuf],
    probe: impl FnOnce() -> WiredAgentProjection,
) -> WiredAgentProjection {
    let mut memo = memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = stamp_inputs(paths);
    if let Some(cached) = memo.as_ref().filter(|cached| cached.inputs == before) {
        return cached.projection.clone();
    }
    let projection = probe();
    let after = stamp_inputs(paths);
    if before == after {
        *memo = Some(MemoizedProjection {
            inputs: after,
            projection: projection.clone(),
        });
        return projection;
    }
    memo.as_ref()
        .map(|cached| cached.projection.clone())
        .unwrap_or_default()
}

fn stamp_inputs(paths: &[PathBuf]) -> Vec<InputStamp> {
    paths
        .iter()
        .map(|path| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return InputStamp {
                    path: path.clone(),
                    present: false,
                    modified_secs: 0,
                    modified_nanos: 0,
                    len: 0,
                };
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
            InputStamp {
                path: path.clone(),
                present: true,
                modified_secs: modified.as_ref().map_or(0, |time| time.as_secs()),
                modified_nanos: modified.map_or(0, |time| time.subsec_nanos()),
                len: metadata.len(),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentProjectionPublication {
    pub session_name: String,
    pub wiring: WiredAgentProjection,
    pub inputs: LocalSessionInputs,
    pub observations: Vec<LocalSessionObservation>,
}

thread_local! {
    static AGENT_PROJECTION_PARSE_CACHE: ParseCache<AgentProjectionPublication> = const { ParseCache::new() };
}

/// Probe wiring and provider sessions once, publish one semantic cache, and
/// return fresh values even when its disposable write fails.
pub fn refresh_published(
    runtime: &RuntimePaths,
    session_name: &str,
    panes: &[PaneRef],
) -> AgentProjection {
    let wiring = probe_current();
    let inputs = LocalSessionInputs::from_panes(panes);
    let observations = inputs.discover();
    let published = AgentProjectionPublication {
        session_name: session_name.to_owned(),
        wiring: wiring.clone(),
        inputs,
        observations: observations.clone(),
    };
    match publish_if_changed(&runtime.agent_projection_path(), &published, || {
        if let Err(err) = crate::store::wakeup::wake_sidebars(runtime) {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "agent-projection publication could not wake sidebars",
            );
        }
    }) {
        Ok(_) => {}
        Err(err) => tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "agent-projection publication failed",
        ),
    }
    AgentProjection {
        wiring,
        local_sessions: observations,
    }
}

/// Read one same-session publication and filter observations through current
/// normalized inputs. Every miss fails closed without invoking an adapter.
pub fn read_published(
    runtime: &RuntimePaths,
    session_name: &str,
    panes: &[PaneRef],
) -> AgentProjection {
    let inputs = LocalSessionInputs::from_panes(panes);
    let Some(published) = AGENT_PROJECTION_PARSE_CACHE
        .with(|cache| cache.read_stamped_json(&runtime.agent_projection_path()))
        .filter(|published| published.session_name == session_name)
    else {
        return AgentProjection::default();
    };
    project_published(&published, &inputs)
}

fn project_published(
    published: &AgentProjectionPublication,
    inputs: &LocalSessionInputs,
) -> AgentProjection {
    let admitted = published
        .inputs
        .normalized_keys()
        .intersection(&inputs.normalized_keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let local_sessions = published
        .observations
        .iter()
        .filter(|observation| {
            admitted.contains(&(
                observation.kind.clone(),
                crate::worktree::normalize_path_lexical(&observation.workspace),
            ))
        })
        .cloned()
        .collect();
    AgentProjection {
        wiring: published.wiring.clone(),
        local_sessions,
    }
}

fn publish_if_changed(
    path: &Path,
    published: &AgentProjectionPublication,
    on_changed: impl FnOnce(),
) -> crate::store::atomic::Result<bool> {
    if std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<AgentProjectionPublication>(&bytes).ok())
        .as_ref()
        == Some(published)
    {
        return Ok(false);
    }
    crate::store::atomic::write_temp_then_rename_cache(path, published)?;
    on_changed();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use jiff::Timestamp;

    use super::*;
    use crate::agents::LocalSessionProjection;
    use crate::ids::{AgentSessionId, WorkspaceId};

    fn projection(kind: &str) -> WiredAgentProjection {
        WiredAgentProjection {
            kinds: vec![kind.to_owned()],
            default_models: BTreeMap::new(),
        }
    }

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

    fn local_inputs(root: &Path) -> LocalSessionInputs {
        LocalSessionInputs {
            by_kind: BTreeMap::from([(
                AgentKind::new_unchecked("kiro"),
                vec![root.join("a"), root.join("b")],
            )]),
        }
    }

    #[test]
    fn unchanged_and_missing_to_present_inputs_control_probes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let memo = Mutex::new(None);
        let calls = Cell::new(0);
        let run = || {
            probe_with(&memo, std::slice::from_ref(&path), || {
                calls.set(calls.get() + 1);
                projection("codex")
            })
        };
        assert_eq!(run(), projection("codex"));
        assert_eq!(run(), projection("codex"));
        assert_eq!(calls.get(), 1);
        std::fs::write(&path, "wired").unwrap();
        assert_eq!(run(), projection("codex"));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn raced_probe_keeps_prior_projection_and_retries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, "old").unwrap();
        let memo = Mutex::new(None);
        assert_eq!(
            probe_with(&memo, std::slice::from_ref(&path), || projection("old")),
            projection("old")
        );
        std::fs::write(&path, "changed-before").unwrap();
        let raced = probe_with(&memo, std::slice::from_ref(&path), || {
            std::fs::write(&path, "changed-during-probe-and-longer").unwrap();
            projection("mixed")
        });
        assert_eq!(raced, projection("old"));
        assert_eq!(
            probe_with(&memo, std::slice::from_ref(&path), || projection("new")),
            projection("new")
        );
    }

    #[test]
    fn local_session_discovery_batches_each_kind_once() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = local_inputs(dir.path());
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
    fn semantic_publication_wakes_after_visible_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agent-projection.json");
        let published = AgentProjectionPublication {
            session_name: "room".to_owned(),
            wiring: projection("codex"),
            inputs: LocalSessionInputs::default(),
            observations: Vec::new(),
        };
        let wakes = Cell::new(0);
        publish_if_changed(&path, &published, || {
            let visible: AgentProjectionPublication =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(visible, published);
            wakes.set(wakes.get() + 1);
        })
        .unwrap();
        assert!(!publish_if_changed(&path, &published, || wakes.set(99)).unwrap());
        assert_eq!(wakes.get(), 1);
    }

    #[test]
    fn local_session_projection_uses_published_current_intersection() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = local_inputs(dir.path());
        let kind = AgentKind::new_unchecked("kiro");
        let publication = AgentProjectionPublication {
            session_name: "room".to_owned(),
            wiring: projection("kiro"),
            inputs: inputs.clone(),
            observations: vec![
                observation(&inputs.by_kind[&kind][0], "a"),
                observation(&inputs.by_kind[&kind][1], "b"),
            ],
        };
        let mut current = inputs.clone();
        current.by_kind.get_mut(&kind).unwrap().remove(0);
        current
            .by_kind
            .get_mut(&kind)
            .unwrap()
            .push(dir.path().join("c"));

        assert_eq!(
            project_published(&publication, &current).local_sessions,
            vec![observation(&inputs.by_kind[&kind][1], "b")],
            "removed inputs disappear and new inputs wait for publication",
        );
    }

    #[test]
    fn unchanged_projection_file_reuses_one_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-projection.json");
        let publication = AgentProjectionPublication {
            session_name: "room".to_owned(),
            wiring: projection("kiro"),
            inputs: local_inputs(dir.path()),
            observations: Vec::new(),
        };
        publish_if_changed(&path, &publication, || {}).unwrap();

        let first = AGENT_PROJECTION_PARSE_CACHE
            .with(|cache| cache.read_stamped_json(&path))
            .unwrap();
        let second = AGENT_PROJECTION_PARSE_CACHE
            .with(|cache| cache.read_stamped_json(&path))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn consumer_reads_fail_closed_for_missing_malformed_and_wrong_session_files() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(temp.path());
        let runtime = RuntimePaths::under(workspace, temp.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        assert_eq!(
            read_published(&runtime, "room", &[]),
            AgentProjection::default()
        );
        std::fs::write(runtime.agent_projection_path(), "not json").unwrap();
        assert!(
            read_published(&runtime, "room", &[])
                .wiring
                .kinds
                .is_empty()
        );
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.agent_projection_path(),
            &AgentProjectionPublication {
                session_name: "other".to_owned(),
                wiring: projection("codex"),
                inputs: LocalSessionInputs::default(),
                observations: Vec::new(),
            },
        )
        .unwrap();
        assert!(
            read_published(&runtime, "room", &[])
                .wiring
                .kinds
                .is_empty()
        );
        assert_eq!(
            read_published(&runtime, "other", &[]).wiring,
            projection("codex")
        );
    }
}
