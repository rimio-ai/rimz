//! Producer-owned publication of adapter wiring and launch defaults.
//!
//! The elected producer probes provider configuration behind an exact-stamp
//! process memo. Renderers consume only the normalized same-session cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::store::RuntimePaths;
use crate::store::parse_cache::ParseCache;

/// Agent kinds admitted for sessionless idle synthesis and their launch-model
/// fallbacks, in adapter display order.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WiredAgentProjection {
    pub kinds: Vec<String>,
    pub default_models: BTreeMap<String, String>,
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
    let mut paths = crate::agents::ADAPTERS
        .iter()
        .flat_map(|adapter| adapter.wiring_input_paths())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    probe_with(&WIRING_MEMO, &paths, probe_adapters)
}

fn probe_adapters() -> WiredAgentProjection {
    let mut projection = WiredAgentProjection::default();
    for agent in crate::agents::ADAPTERS {
        let descriptor = agent.descriptor();
        let wired = descriptor.capabilities.local_session_discovery
            || (descriptor.has_wired_hook_install() && agent.hooks_installed());
        if !wired {
            continue;
        }
        projection.kinds.push(descriptor.kind.to_owned());
        if let Some(model) = agent.default_launch_model() {
            projection
                .default_models
                .insert(descriptor.kind.to_owned(), model);
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
pub struct PublishedAgentWiring {
    pub session_name: String,
    pub projection: WiredAgentProjection,
}

thread_local! {
    static AGENT_WIRING_PARSE_CACHE: ParseCache<PublishedAgentWiring> = const { ParseCache::new() };
}

/// Refresh the stable process memo, publish semantic changes, and return the
/// fresh projection even when the disposable cache cannot be written.
pub fn refresh_published(runtime: &RuntimePaths, session_name: &str) -> WiredAgentProjection {
    let projection = probe_current();
    let published = PublishedAgentWiring {
        session_name: session_name.to_owned(),
        projection: projection.clone(),
    };
    match publish_if_changed(&runtime.agent_wiring_path(), &published, || {
        if let Err(err) = crate::store::wakeup::wake_sidebars(runtime) {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "agent-wiring publication could not wake sidebars",
            );
        }
    }) {
        Ok(_) => {}
        Err(err) => tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "agent-wiring publication failed",
        ),
    }
    projection
}

/// Read a normalized wiring publication for this exact mux session. Every
/// miss fails closed without invoking an adapter.
pub fn read_published(runtime: &RuntimePaths, session_name: &str) -> WiredAgentProjection {
    read_cache(&runtime.agent_wiring_path())
        .filter(|published| published.session_name == session_name)
        .map(|published| published.projection.clone())
        .unwrap_or_default()
}

fn publish_if_changed(
    path: &Path,
    published: &PublishedAgentWiring,
    on_changed: impl FnOnce(),
) -> crate::store::atomic::Result<bool> {
    if std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PublishedAgentWiring>(&bytes).ok())
        .as_ref()
        == Some(published)
    {
        return Ok(false);
    }
    crate::store::atomic::write_temp_then_rename_cache(path, published)?;
    on_changed();
    Ok(true)
}

fn read_cache(path: &Path) -> Option<Arc<PublishedAgentWiring>> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let len = metadata.len();
    if let Some(cached) = AGENT_WIRING_PARSE_CACHE.with(|cache| cache.get(path, modified, len)) {
        return Some(cached);
    }
    let parsed = Arc::new(serde_json::from_slice(&std::fs::read(path).ok()?).ok()?);
    AGENT_WIRING_PARSE_CACHE.with(|cache| {
        cache.store(path, modified, len, Arc::clone(&parsed));
    });
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::ids::WorkspaceId;

    fn projection(kind: &str) -> WiredAgentProjection {
        WiredAgentProjection {
            kinds: vec![kind.to_owned()],
            default_models: BTreeMap::new(),
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
    fn semantic_publication_wakes_after_visible_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agent-wiring.json");
        let published = PublishedAgentWiring {
            session_name: "room".to_owned(),
            projection: projection("codex"),
        };
        let wakes = Cell::new(0);
        publish_if_changed(&path, &published, || {
            let visible: PublishedAgentWiring =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(visible, published);
            wakes.set(wakes.get() + 1);
        })
        .unwrap();
        assert!(!publish_if_changed(&path, &published, || wakes.set(99)).unwrap());
        assert_eq!(wakes.get(), 1);
    }

    #[test]
    fn consumer_reads_fail_closed_for_missing_malformed_and_wrong_session_files() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(temp.path());
        let runtime = RuntimePaths::under(workspace, temp.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        assert_eq!(
            read_published(&runtime, "room"),
            WiredAgentProjection::default()
        );
        std::fs::write(runtime.agent_wiring_path(), "not json").unwrap();
        assert!(read_published(&runtime, "room").kinds.is_empty());
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.agent_wiring_path(),
            &PublishedAgentWiring {
                session_name: "other".to_owned(),
                projection: projection("codex"),
            },
        )
        .unwrap();
        assert!(read_published(&runtime, "room").kinds.is_empty());
        assert_eq!(read_published(&runtime, "other"), projection("codex"));
    }
}
