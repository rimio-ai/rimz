//! Producer-side automatic garbage collection: sweep stale state once a day
//! from every open room.
//!
//! The elected producer reads the workspace's durable sweep stamp and spawns
//! the detached `rimz gc --unattended` helper when a sweep is due. The helper
//! owns the sweep, the assist record, and the stamp write.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::GcConfig;
use crate::{RuntimePaths, StatePaths};

pub(crate) fn sweep_runtime_claims(runtime: &RuntimePaths) {
    if let Err(err) = crate::store::gc::collect_runtime_claims(runtime) {
        tracing::debug!(error = %err, "runtime claim sweep failed");
    }
}

/// How often a workspace sweeps.
const AUTO_GC_INTERVAL: Duration = Duration::from_secs(24 * 3_600);
/// Producer age before its first sweep, so a reborn room finishes
/// materializing panes before the worktree area judges them.
const AUTO_GC_SETTLE: Duration = Duration::from_secs(5 * 60);
/// Bounds duplicate helper spawns while a sweep is still running.
const AUTO_GC_RESPAWN_THROTTLE: Duration = Duration::from_secs(10 * 60);

/// Process-local pacing owned by one long-lived producer.
#[derive(Debug, Default)]
pub(crate) struct AutoGcMemo {
    producer_since: Option<Timestamp>,
    last_spawn: Option<Timestamp>,
    /// Stamps only move forward, so one read inside the interval settles the
    /// rest of the day without touching disk.
    last_swept: Option<Timestamp>,
}

#[derive(Deserialize, Serialize)]
struct Stamp {
    swept_at: Timestamp,
}

/// Spawn the unattended gc helper when this workspace's daily sweep is due.
pub(crate) fn sweep_if_due(
    state_paths: &StatePaths,
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    config: &GcConfig,
    now: Timestamp,
    memo: &mut AutoGcMemo,
) {
    let producer_since = *memo.producer_since.get_or_insert(now);
    let last_spawn = memo.last_spawn;
    let cached_sweep = &mut memo.last_swept;
    let last_swept = || {
        if let Some(swept) = *cached_sweep
            && !interval_elapsed(swept, now)
        {
            return Some(swept);
        }
        *cached_sweep = read_stamp(state_paths);
        *cached_sweep
    };
    if !due(config.auto, last_swept, producer_since, last_spawn, now) {
        return;
    }
    sweep_runtime_claims(runtime);
    let Some(project_root) = project_root else {
        return;
    };
    let args = [
        "gc".as_ref(),
        "--unattended".as_ref(),
        "--root".as_ref(),
        project_root.as_os_str(),
    ];
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        "sidebar: starting automatic gc",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "auto-gc") {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "auto_gc.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn automatic gc",
        );
        return;
    }
    memo.last_spawn = Some(now);
}

/// The stamp is read last, so an idle or opted-out tick never touches disk.
fn due(
    auto: bool,
    last_swept: impl FnOnce() -> Option<Timestamp>,
    producer_since: Timestamp,
    last_spawn: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    auto && elapsed(producer_since, AUTO_GC_SETTLE, now)
        && last_spawn.is_none_or(|spawn| elapsed(spawn, AUTO_GC_RESPAWN_THROTTLE, now))
        && last_swept().is_none_or(|swept| interval_elapsed(swept, now))
}

fn interval_elapsed(swept: Timestamp, now: Timestamp) -> bool {
    elapsed(swept, AUTO_GC_INTERVAL, now)
}

fn elapsed(since: Timestamp, span: Duration, now: Timestamp) -> bool {
    now.as_second() - since.as_second() >= span.as_secs() as i64
}

/// When this workspace last ran an unattended sweep; unreadable reads as never.
fn read_stamp(paths: &StatePaths) -> Option<Timestamp> {
    let bytes = std::fs::read(&paths.auto_gc_stamp).ok()?;
    serde_json::from_slice::<Stamp>(&bytes)
        .ok()
        .map(|stamp| stamp.swept_at)
}

/// Record an unattended sweep attempt so the next one waits a full interval.
pub fn write_stamp(paths: &StatePaths, swept_at: Timestamp) -> Result<()> {
    crate::disk::atomic::write_temp_then_rename(&paths.auto_gc_stamp, &Stamp { swept_at })
        .with_context(|| format!("writing {}", paths.auto_gc_stamp.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn at(minutes: i64) -> Timestamp {
        Timestamp::from_second(1_000_000 + minutes * 60).expect("timestamp")
    }

    #[test]
    fn due_waits_for_settle_interval_and_throttle() {
        let day = 24 * 60;
        for (auto, swept, since, spawn, now, expected, case) in [
            (
                true,
                None,
                at(0),
                None,
                at(5),
                true,
                "first sweep after settle",
            ),
            (true, None, at(0), None, at(4), false, "still settling"),
            (false, None, at(0), None, at(60), false, "auto off"),
            (
                true,
                Some(at(0)),
                at(0),
                None,
                at(day - 1),
                false,
                "swept today",
            ),
            (
                true,
                Some(at(0)),
                at(0),
                None,
                at(day),
                true,
                "a day since the sweep",
            ),
            (
                true,
                None,
                at(0),
                Some(at(10)),
                at(19),
                false,
                "helper still running",
            ),
            (
                true,
                None,
                at(0),
                Some(at(10)),
                at(20),
                true,
                "throttle elapsed",
            ),
        ] {
            assert_eq!(due(auto, || swept, since, spawn, now), expected, "{case}");
        }
    }

    #[test]
    fn stamp_round_trips_and_unreadable_reads_as_never() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            dir.path(),
        )
        .expect("paths");
        std::fs::create_dir_all(&paths.root).expect("workspace dir");
        assert_eq!(read_stamp(&paths), None);

        write_stamp(&paths, at(3)).expect("write stamp");
        assert_eq!(read_stamp(&paths), Some(at(3)));

        std::fs::write(&paths.auto_gc_stamp, b"not json").expect("corrupt stamp");
        assert_eq!(read_stamp(&paths), None);
    }
}
