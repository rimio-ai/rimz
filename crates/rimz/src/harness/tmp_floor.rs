//! Machine-wide temp pressure policy, outside the sidebar's read-only graph.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

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

/// Whether a machine-wide pressure pass was skipped or completed.
#[derive(Debug)]
pub enum FloorOutcome {
    /// `gc.auto = false`: nothing was locked, probed, or removed.
    Paused,
    /// Another pass holds the machine pressure lock.
    Busy,
    Ran(FloorReport),
}

#[derive(Debug, Default, Serialize)]
pub struct TempUnits {
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub floor_bytes: u64,
    pub eviction_paused: bool,
    pub reclaimable_units: usize,
    pub past_grace_units: usize,
    pub earliest_reclaim: Option<jiff::Timestamp>,
    pub in_use_units: usize,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<TempUnitBytes>,
}

#[derive(Debug, Default, Serialize)]
pub struct TempUnitBytes {
    pub reclaimable_bytes: u64,
    pub in_use_bytes: u64,
}

pub fn inspect(config: &GcConfig, measure_bytes: bool) -> Result<TempUnits, TmpFloorErr> {
    let root = crate::disk::paths::workspaces_dir();
    let space = probe(&root)?;
    let mut report = TempUnits {
        free_bytes: space.free,
        total_bytes: space.total,
        floor_bytes: config
            .tmp_floor_bytes(space.total)
            .map_err(TmpFloorErr::Config)?,
        eviction_paused: !config.auto,
        bytes: measure_bytes.then(TempUnitBytes::default),
        ..TempUnits::default()
    };
    let now = SystemTime::now();
    for candidate in inventory(&root, false)? {
        let bytes = if measure_bytes {
            crate::disk::usage::dir_size(&candidate.unit.path)
        } else {
            0
        };
        report.tally(&candidate.unit, bytes, now, config.tmp_grace);
    }
    Ok(report)
}

impl TempUnits {
    /// Counts one unit. `earliest_reclaim` covers only units still inside the
    /// grace, so it never names a past time; a unit already past it waits for
    /// the next sweep and is counted in `past_grace_units` alone.
    fn tally(&mut self, unit: &TmpUnit, bytes: u64, now: SystemTime, grace: Duration) {
        let Some(order) = eviction_time(unit) else {
            self.in_use_units += 1;
            if let Some(measured) = &mut self.bytes {
                measured.in_use_bytes = measured.in_use_bytes.saturating_add(bytes);
            }
            return;
        };
        self.reclaimable_units += 1;
        if let Some(measured) = &mut self.bytes {
            measured.reclaimable_bytes = measured.reclaimable_bytes.saturating_add(bytes);
        }
        if unit.past_grace(now, grace) {
            self.past_grace_units += 1;
            return;
        }
        if let Some(reclaim) = unit
            .modified
            .max(order)
            .checked_add(grace)
            .and_then(|time| jiff::Timestamp::try_from(time).ok())
        {
            self.earliest_reclaim =
                Some(self.earliest_reclaim.map_or(reclaim, |at| at.min(reclaim)));
        }
    }
}

pub fn run(config: &GcConfig) -> Result<FloorOutcome, TmpFloorErr> {
    if !config.auto {
        return Ok(FloorOutcome::Paused);
    }
    let Some(_guard) = claim(&RuntimePaths::shared().shared_tmp_floor_lock())? else {
        return Ok(FloorOutcome::Busy);
    };
    let root = crate::disk::paths::workspaces_dir();
    let space = probe(&root)?;
    let floor_bytes = config
        .tmp_floor_bytes(space.total)
        .map_err(TmpFloorErr::Config)?;
    if space.free >= floor_bytes {
        return Ok(FloorOutcome::Ran(FloorReport {
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
    .map(FloorOutcome::Ran)
}

fn probe(root: &Path) -> io::Result<DiskSpace> {
    // Doctor must also work before the workspace directory has ever existed.
    let path = root.ancestors().find(|path| path.exists()).unwrap_or(root);
    crate::disk::usage::disk_space(path)
}

fn room_inventory(paths: &StatePaths) -> Result<Vec<TmpUnit>, TmpFloorErr> {
    let (_, agents, _) = crate::store::snapshot::RollupCursor::new().fold(paths)?;
    let owners = super::launch_plan::AgentTempOwners::new(agents.iter());
    Ok(crate::store::gc::tmp_inventory(
        paths,
        &agents,
        &|row, _| owners.owner(row).map(str::to_owned),
    )?)
}

fn inventory(root: &Path, lock: bool) -> Result<Vec<Candidate>, TmpFloorErr> {
    let mut candidates = Vec::new();
    for room in crate::workspace::known_workspaces_under(root)? {
        let mut paths = StatePaths::under_named(
            room.workspace_id,
            room.dir_name,
            &crate::disk::paths::rimz_home(),
        );
        let runtime = RuntimePaths::for_state_under(&paths, &crate::disk::paths::runtime_home());
        paths.bind_runtime_checkpoint(&runtime);
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
