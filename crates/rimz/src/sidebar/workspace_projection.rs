//! Producer-published workspace enrichment and consumer adoption.
//!
//! The file is disposable runtime truth acceleration. Its source identity is
//! validated against the live rollup, pane-frame sections, and machine config
//! before a renderer applies its local projection.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::sidebar::enrich::WorkspaceSnapshot;
use crate::sidebar::frame::PaneFrame;
use crate::store::event_log::LogExtent;
use crate::store::parse_cache::{ParseCache, StampedPath};
use crate::{RuntimePaths, StatePaths};

pub const WORKSPACE_PROJECTION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectionSource {
    pub rollup_generation: u64,
    pub rollup_offset: u64,
    pub frame_topology_stamp: u64,
    pub frame_metrics_stamp: u64,
    pub config_generation: u64,
}

impl WorkspaceProjectionSource {
    pub fn from_fold(workspace: &WorkspaceSnapshot, frame: &PaneFrame) -> Option<Self> {
        let extent = workspace.snapshot().reflects_log?;
        Some(Self::new(extent, frame))
    }

    pub fn current(state: &StatePaths, frame: &PaneFrame) -> Option<Self> {
        let before = StampedPath::of(&state.events_log);
        let published_extent = read_latest_extent(&state.latest_snapshot)?;
        let after = StampedPath::of(&state.events_log);
        if before != after || published_extent.offset > after.stamp.len {
            return None;
        }
        // `latest.json` may trail an active log between debounced publishes;
        // its generation remains authoritative while the live log length is
        // the event-fresh offset. Rotation retracts latest before replacing
        // the log, and the before/after file identity rejects that race.
        Some(Self::new(
            LogExtent {
                generation: published_extent.generation,
                offset: after.stamp.len,
            },
            frame,
        ))
    }

    fn new(extent: LogExtent, frame: &PaneFrame) -> Self {
        Self {
            rollup_generation: extent.generation,
            rollup_offset: extent.offset,
            frame_topology_stamp: frame.topology_stamp_ms.unwrap_or_default(),
            frame_metrics_stamp: frame.metrics_stamp_ms.unwrap_or_default(),
            config_generation: crate::config::MachineConfig::load_stamp_generation(),
        }
    }

    pub fn is_matchable(self) -> bool {
        self.frame_topology_stamp != 0 && self.frame_metrics_stamp != 0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedWorkspaceProjection {
    pub schema_version: u32,
    pub session: String,
    pub source: WorkspaceProjectionSource,
    pub projection: WorkspaceSnapshot,
}

#[derive(Serialize)]
struct PublishedWorkspaceProjectionRef<'a> {
    schema_version: u32,
    session: &'a str,
    source: WorkspaceProjectionSource,
    projection: &'a WorkspaceSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceProjectionPublish {
    Published,
    Unchanged,
    Unmatchable,
}

#[derive(Debug, Default)]
pub struct WorkspaceProjectionPublisher {
    last_content: Option<(u64, Vec<u8>)>,
}

impl WorkspaceProjectionPublisher {
    pub fn publish(
        &mut self,
        runtime: &RuntimePaths,
        session: &str,
        workspace: &WorkspaceSnapshot,
        frame: &PaneFrame,
    ) -> crate::store::atomic::Result<WorkspaceProjectionPublish> {
        let Some(source) = WorkspaceProjectionSource::from_fold(workspace, frame)
            .filter(|source| source.is_matchable())
        else {
            return Ok(WorkspaceProjectionPublish::Unmatchable);
        };
        let bytes = serde_json::to_vec(&PublishedWorkspaceProjectionRef {
            schema_version: WORKSPACE_PROJECTION_SCHEMA_VERSION,
            session,
            source,
            projection: workspace,
        })?;
        let hash = content_hash(&bytes);
        let unchanged = self
            .last_content
            .as_ref()
            .is_some_and(|(last_hash, last)| *last_hash == hash && *last == bytes)
            || (self.last_content.is_none()
                && std::fs::read(workspace_projection_path(runtime))
                    .is_ok_and(|last| last == bytes));
        if unchanged {
            self.last_content = Some((hash, bytes));
            return Ok(WorkspaceProjectionPublish::Unchanged);
        }
        crate::store::atomic::write_cache_bytes_atomically(
            &workspace_projection_path(runtime),
            &bytes,
        )?;
        self.last_content = Some((hash, bytes));
        Ok(WorkspaceProjectionPublish::Published)
    }
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub fn workspace_projection_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("workspace-projection.json")
}

pub fn read_workspace_projection(
    runtime: &RuntimePaths,
) -> Option<Arc<PublishedWorkspaceProjection>> {
    WORKSPACE_PROJECTION_PARSE_CACHE
        .with(|cache| cache.read_stamped_json(&workspace_projection_path(runtime)))
}

#[derive(Deserialize)]
struct LatestExtent {
    #[serde(default)]
    snapshot_version: u32,
    #[serde(default)]
    reflects_log: Option<LogExtent>,
}

fn read_latest_extent(path: &Path) -> Option<LogExtent> {
    let latest = LATEST_EXTENT_PARSE_CACHE.with(|cache| cache.read_stamped_json(path))?;
    (latest.snapshot_version == crate::store::snapshot::SNAPSHOT_VERSION)
        .then_some(latest.reflects_log)
        .flatten()
}

thread_local! {
    static WORKSPACE_PROJECTION_PARSE_CACHE: ParseCache<PublishedWorkspaceProjection> = const { ParseCache::new() };
    static LATEST_EXTENT_PARSE_CACHE: ParseCache<LatestExtent> = const { ParseCache::new() };
}

#[cfg(test)]
mod tests;
