//! Machine-wide temp pressure policy, outside the sidebar's read-only graph.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use crate::config::GcConfig;
use crate::disk::usage::DiskSpace;
use crate::store::gc::{TmpHold, TmpUnit};
use crate::{RuntimePaths, StatePaths};

use super::assist_log::{self, Assist, AssistRecord};

#[derive(Debug, thiserror::Error)]
pub enum TmpFloorErr {
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Gc(#[from] crate::store::gc::GcErr),
    #[error(transparent)]
    Snapshot(#[from] crate::store::snapshot::SnapshotErr),
}

struct Candidate {
    paths: StatePaths,
    unit: TmpUnit,
}

#[derive(Debug, Serialize)]
pub struct FloorReport {
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub floor_bytes: u64,
    pub evictions: Vec<AssistRecord>,
}

#[derive(Debug, Default, Serialize)]
pub struct TempUnits {
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub floor_bytes: u64,
    pub reclaimable_units: usize,
    pub reclaimable_bytes: u64,
    pub past_grace_units: usize,
    pub earliest_reclaim: Option<jiff::Timestamp>,
    pub in_use_units: usize,
    pub in_use_bytes: u64,
}

pub fn inspect(config: &GcConfig) -> Result<TempUnits, TmpFloorErr> {
    let root = crate::disk::paths::workspaces_dir();
    let space = probe(&root)?;
    let mut report = TempUnits {
        free_bytes: space.free,
        total_bytes: space.total,
        floor_bytes: config
            .tmp_floor_bytes(space.total)
            .map_err(TmpFloorErr::Config)?,
        ..TempUnits::default()
    };
    let now = SystemTime::now();
    for candidate in inventory(&root, false)? {
        let unit = candidate.unit;
        let bytes = crate::disk::usage::dir_size(&unit.path);
        let Some(order) = eviction_time(&unit) else {
            report.in_use_units += 1;
            report.in_use_bytes = report.in_use_bytes.saturating_add(bytes);
            continue;
        };
        report.reclaimable_units += 1;
        report.reclaimable_bytes = report.reclaimable_bytes.saturating_add(bytes);
        report.past_grace_units += usize::from(unit.past_grace(now, config.tmp_grace));
        if let Some(reclaim) = unit
            .modified
            .max(order)
            .checked_add(config.tmp_grace)
            .and_then(|time| jiff::Timestamp::try_from(time).ok())
        {
            report.earliest_reclaim = Some(
                report
                    .earliest_reclaim
                    .map_or(reclaim, |at| at.min(reclaim)),
            );
        }
    }
    Ok(report)
}

pub fn run(config: &GcConfig) -> Result<Option<FloorReport>, TmpFloorErr> {
    if !config.auto {
        return Ok(None);
    }
    let Some(_guard) = claim(&RuntimePaths::shared().shared_tmp_floor_lock())? else {
        return Ok(None);
    };
    let root = crate::disk::paths::workspaces_dir();
    let space = probe(&root)?;
    let floor_bytes = config
        .tmp_floor_bytes(space.total)
        .map_err(TmpFloorErr::Config)?;
    if space.free >= floor_bytes {
        return Ok(Some(FloorReport {
            free_bytes: space.free,
            total_bytes: space.total,
            floor_bytes,
            evictions: Vec::new(),
        }));
    }
    enforce(
        inventory(&root, true)?,
        config,
        || probe(&root),
        rejudge,
        |record| {
            if let Err(error) = assist_log::try_append(record) {
                tracing::warn!(%error, "tmp eviction assist append failed");
            }
        },
    )
    .map(Some)
}

fn probe(root: &Path) -> io::Result<DiskSpace> {
    // Doctor must also work before the workspace directory has ever existed.
    let path = root.ancestors().find(|path| path.exists()).unwrap_or(root);
    crate::disk::usage::disk_space(path)
}

fn room_inventory(paths: &StatePaths) -> Result<Vec<TmpUnit>, TmpFloorErr> {
    let (_, agents, _) = crate::store::snapshot::RollupCursor::new().fold(paths)?;
    Ok(crate::store::gc::tmp_inventory(
        paths,
        &agents,
        &|row, agents| super::launch_plan::agent_temp_owner(row, agents.iter()).map(str::to_owned),
    )?)
}

fn inventory(root: &Path, lock: bool) -> Result<Vec<Candidate>, TmpFloorErr> {
    let mut candidates = Vec::new();
    for room in crate::workspace::known_workspaces_under(root)? {
        let paths = StatePaths::under_named(
            room.workspace_id,
            room.dir_name,
            &crate::disk::paths::rimz_home(),
        );
        let units = (|| {
            let _guard = if lock {
                Some(crate::disk::lock::WorkspaceLock::acquire(
                    &paths.workspace_lock,
                )?)
            } else {
                None
            };
            room_inventory(&paths)
        })();
        match units {
            Ok(units) => candidates.extend(units.into_iter().map(|unit| Candidate {
                paths: paths.clone(),
                unit,
            })),
            Err(error) => {
                tracing::warn!(workspace = %paths.dir_name, %error, "tmp inventory skipped unreadable room")
            }
        }
    }
    Ok(candidates)
}

fn rejudge(candidate: &Candidate) -> Result<Option<TmpUnit>, TmpFloorErr> {
    if !candidate.paths.root.is_dir() {
        return Ok(None);
    }
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&candidate.paths.workspace_lock)?;
    Ok(room_inventory(&candidate.paths)?
        .into_iter()
        .find(|unit| unit.path == candidate.unit.path))
}

fn eviction_time(unit: &TmpUnit) -> Option<SystemTime> {
    match unit.hold {
        TmpHold::Ended(at) => Some(at.into()),
        TmpHold::Unowned => Some(unit.modified),
        TmpHold::Live | TmpHold::Open => None,
    }
}

fn enforce(
    mut candidates: Vec<Candidate>,
    config: &GcConfig,
    mut probe: impl FnMut() -> io::Result<DiskSpace>,
    mut rejudge: impl FnMut(&Candidate) -> Result<Option<TmpUnit>, TmpFloorErr>,
    mut record: impl FnMut(&AssistRecord),
) -> Result<FloorReport, TmpFloorErr> {
    let space = probe()?;
    let mut report = FloorReport {
        free_bytes: space.free,
        total_bytes: space.total,
        floor_bytes: config
            .tmp_floor_bytes(space.total)
            .map_err(TmpFloorErr::Config)?,
        evictions: Vec::new(),
    };
    candidates.retain(|candidate| eviction_time(&candidate.unit).is_some());
    candidates.sort_by_key(|candidate| eviction_time(&candidate.unit));
    let mut estimated_free = space.free;
    for candidate in candidates {
        if report.free_bytes.max(estimated_free) >= report.floor_bytes {
            break;
        }
        // Measuring a large tree may take seconds; do it before the final decisions.
        let bytes = crate::disk::usage::dir_allocated_size(&candidate.unit.path);
        let space = probe()?;
        report.free_bytes = space.free;
        report.total_bytes = space.total;
        report.floor_bytes = config
            .tmp_floor_bytes(space.total)
            .map_err(TmpFloorErr::Config)?;
        if space.free.max(estimated_free) >= report.floor_bytes {
            break;
        }
        let unit = match rejudge(&candidate) {
            Ok(Some(unit)) => unit,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(path = %candidate.unit.path.display(), %error, "tmp eviction skipped failed rejudgment");
                continue;
            }
        };
        if eviction_time(&unit).is_none() || unit.modified > candidate.unit.modified {
            continue;
        }
        // The room lock has dropped: recursive removal must not stall hook writers.
        let error = std::fs::remove_dir_all(&unit.path)
            .err()
            .map(|error| error.to_string());
        if let Some(error) = &error {
            tracing::warn!(path = %unit.path.display(), %error, "tmp eviction failed");
        } else {
            // Copy-on-write filesystems may not publish reclaimed space until later.
            estimated_free = estimated_free.saturating_add(bytes);
        }
        match probe() {
            Ok(space) => {
                report.free_bytes = space.free;
                report.total_bytes = space.total;
                report.floor_bytes = config
                    .tmp_floor_bytes(space.total)
                    .map_err(TmpFloorErr::Config)?;
            }
            Err(error) => {
                tracing::warn!(%error, "tmp eviction space probe failed; retaining last reading")
            }
        }
        let eviction = AssistRecord {
            at: jiff::Timestamp::now(),
            assist: Assist::TmpEvict {
                workspace_id: candidate.paths.workspace_id,
                handle: unit.handle,
                reclaimed_bytes: if error.is_none() { bytes } else { 0 },
                ended_at: match unit.hold {
                    TmpHold::Ended(at) => Some(at),
                    _ => None,
                },
                free_bytes_after: report.free_bytes,
                floor_bytes: report.floor_bytes,
                error,
            },
        };
        record(&eviction);
        report.evictions.push(eviction);
    }
    Ok(report)
}

fn claim(path: &Path) -> Result<Option<crate::disk::lock::WorkspaceLock>, TmpFloorErr> {
    Ok(crate::disk::lock::WorkspaceLock::try_acquire(path)?)
}

#[cfg(test)]
#[path = "tmp_floor_tests.rs"]
mod tests;
