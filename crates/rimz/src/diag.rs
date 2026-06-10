//! Durable anomaly diagnostics for sidebar state.
//!
//! The log is a human/debugging surface: producer and renderer code append
//! typed JSONL records, while correctness continues to read ledger/cache truth.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::ids::{PaneId, SidebarInstanceId, WorkspaceId};
use crate::schema::diag::{DiagEnvelope, DiagEvent, GroupIdentity};

const DIAG_LOG_NAME: &str = "diag.log.jsonl";
const DIAG_LOG_MAX_BYTES: u64 = 1_048_576;
const DIAG_FRAMES_DIR: &str = "diag-frames";
const DIAG_FRAME_RING: usize = 8;
const DIAG_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(5);
static DIAG_FRAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct DiagSink {
    state_root: PathBuf,
    workspace_id: WorkspaceId,
    session_name: String,
    instance_id: Option<SidebarInstanceId>,
    limiter: Arc<Mutex<HashMap<String, u64>>>,
}

impl DiagSink {
    pub fn for_workspace(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        instance_id: Option<SidebarInstanceId>,
    ) -> Option<Self> {
        let state = match crate::StatePaths::for_workspace(workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                tracing::debug!(error = %err, "diagnostic sink unavailable");
                return None;
            }
        };
        Some(Self::under(
            state.root,
            workspace_id,
            session_name,
            instance_id,
        ))
    }

    pub fn under(
        state_root: PathBuf,
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        instance_id: Option<SidebarInstanceId>,
    ) -> Self {
        Self {
            state_root,
            workspace_id,
            session_name: session_name.into(),
            instance_id,
            limiter: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn log_path(&self) -> PathBuf {
        self.state_root.join(DIAG_LOG_NAME)
    }

    pub fn frames_dir(&self) -> PathBuf {
        frames_dir_under(&self.state_root)
    }

    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    #[cfg(test)]
    pub(crate) fn frame_capture_path(&self, frames_ref: &str) -> PathBuf {
        self.state_root.join(DIAG_FRAMES_DIR).join(frames_ref)
    }

    pub fn emit(&self, event: DiagEvent) {
        self.emit_at_ms(event, crate::sidebar::cache::unix_now_ms());
    }

    pub fn emit_at_ms(&self, event: DiagEvent, at_ms: u64) {
        if !self.should_emit(&event, at_ms) {
            return;
        }
        self.append(event, at_ms);
    }

    pub fn emit_unlimited(&self, event: DiagEvent) {
        self.append(event, crate::sidebar::cache::unix_now_ms());
    }

    pub fn capture_frame_pair<T: Serialize>(
        &self,
        kind: &str,
        prior: &T,
        offending: &T,
        at_ms: u64,
    ) -> Option<String> {
        let dir = self.frames_dir();
        if let Err(err) = ensure_private_dir(&dir) {
            tracing::debug!(path = %dir.display(), error = %err, "diagnostic frame dir unavailable");
            return None;
        }
        let sequence = DIAG_FRAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let file_name = format!("frame.{at_ms}.{sequence}.{kind}.json");
        let path = dir.join(&file_name);
        let record = serde_json::json!({
            "prior": prior,
            "offending": offending,
        });
        if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(&path, &record) {
            tracing::debug!(path = %path.display(), error = %err, "diagnostic frame capture failed");
            return None;
        }
        prune_frame_ring(&dir);
        Some(file_name)
    }

    fn append(&self, event: DiagEvent, at_ms: u64) {
        let path = self.log_path();
        let envelope = DiagEnvelope::new(
            self.workspace_id.clone(),
            self.session_name.clone(),
            self.instance_id.clone(),
            at_ms,
            event,
        );
        if let Err(err) =
            crate::rotating_log::append_rotating_jsonl(&path, DIAG_LOG_MAX_BYTES, &envelope)
        {
            tracing::debug!(path = %path.display(), error = %err, "diagnostic append failed");
        }
    }

    fn should_emit(&self, event: &DiagEvent, at_ms: u64) -> bool {
        let key = event.identity_key();
        let Ok(mut limiter) = self.limiter.lock() else {
            return true;
        };
        let window_ms = DIAG_RATE_LIMIT_WINDOW.as_millis() as u64;
        limiter.retain(|_, last| at_ms.saturating_sub(*last) < window_ms);
        if limiter
            .get(&key)
            .is_some_and(|last| at_ms.saturating_sub(*last) < window_ms)
        {
            return false;
        }
        limiter.insert(key, at_ms);
        true
    }
}

pub fn path_for_workspace(workspace_id: WorkspaceId) -> Option<PathBuf> {
    crate::StatePaths::for_workspace(workspace_id)
        .ok()
        .map(|state| state.root.join(DIAG_LOG_NAME))
}

pub fn frames_dir_under(state_root: &Path) -> PathBuf {
    state_root.join(DIAG_FRAMES_DIR)
}

pub fn recent_records(
    workspace_id: WorkspaceId,
    limit: usize,
) -> Option<(PathBuf, Vec<DiagEnvelope>)> {
    let path = path_for_workspace(workspace_id)?;
    let mut records = Vec::new();
    for candidate in [rotated_path(&path), path.clone()] {
        let Ok(text) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<DiagEnvelope>(line) {
                Ok(record) if record.is_current_version() => records.push(record),
                Ok(_) => {}
                Err(err) => {
                    tracing::debug!(path = %candidate.display(), error = %err, "diagnostic record decode failed");
                }
            }
        }
    }
    records.sort_by_key(|record| record.at_ms);
    if records.len() > limit {
        records.drain(..records.len() - limit);
    }
    Some((path, records))
}

pub fn diff_group_migrations(
    prev: &crate::SidebarSnapshot,
    next: &crate::SidebarSnapshot,
) -> Vec<DiagEvent> {
    let prev_rows = rows_by_pane(prev);
    let next_rows = rows_by_pane(next);
    let mut events = Vec::new();
    for (pane_id, next_group) in next_rows {
        let Some(prev_group) = prev_rows.get(&pane_id) else {
            continue;
        };
        if prev_group.group == next_group.group && prev_group.cwd == next_group.cwd {
            continue;
        }
        events.push(DiagEvent::GroupMigration {
            pane_id,
            from: prev_group.group.clone(),
            to: next_group.group,
            cwd_before: prev_group.cwd.clone(),
            cwd_after: next_group.cwd,
        });
    }
    events
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowLocation {
    group: GroupIdentity,
    cwd: Option<String>,
}

fn rows_by_pane(snapshot: &crate::SidebarSnapshot) -> HashMap<PaneId, RowLocation> {
    let mut rows = HashMap::new();
    for group in &snapshot.worktree_groups {
        let identity = GroupIdentity {
            kind: worktree_kind_name(group.kind).to_owned(),
            key: group.key.clone(),
        };
        for row in &group.rows {
            let Some(pane) = row.pane.as_ref() else {
                continue;
            };
            rows.insert(
                pane.pane_id.clone(),
                RowLocation {
                    group: identity.clone(),
                    cwd: pane.cwd.clone(),
                },
            );
        }
    }
    rows
}

fn worktree_kind_name(kind: crate::SidebarWorktreeKind) -> &'static str {
    match kind {
        crate::SidebarWorktreeKind::Worktree => "worktree",
        crate::SidebarWorktreeKind::Root => "root",
        crate::SidebarWorktreeKind::External => "external",
    }
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn prune_frame_ring(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut frames = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("frame.") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    frames.sort_by_key(|frame| frame_capture_sort_key(frame));
    let remove_count = frames.len().saturating_sub(DIAG_FRAME_RING);
    for stale in frames.into_iter().take(remove_count) {
        let _ = std::fs::remove_file(stale);
    }
}

fn frame_capture_sort_key(path: &Path) -> (u64, u64, String) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return (0, 0, String::new());
    };
    let Some(rest) = name
        .strip_prefix("frame.")
        .and_then(|value| value.strip_suffix(".json"))
    else {
        return (0, 0, name.to_owned());
    };
    let mut parts = rest.splitn(3, '.');
    let at_ms = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (sequence, tail) = match (parts.next(), parts.next()) {
        (Some(sequence), Some(tail)) => (sequence.parse().unwrap_or(0), tail.to_owned()),
        (Some(tail), None) => (0, tail.to_owned()),
        _ => (0, String::new()),
    };
    (at_ms, sequence, tail)
}

fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name("diag.log.1.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, WorkspaceId};
    use crate::schema::diag::FrameRejectReason;

    fn sink(dir: &Path) -> DiagSink {
        DiagSink::under(
            dir.to_path_buf(),
            WorkspaceId::from_project_root(Path::new("/repo")),
            "s",
            None,
        )
    }

    #[test]
    fn rate_limit_suppresses_identical_events_but_allows_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        sink.emit_at_ms(
            DiagEvent::FrameRejected {
                reason: FrameRejectReason::Empty,
                prior_pane_count: 1,
                fresh_pane_count: 0,
                frames_ref: None,
            },
            1_000,
        );
        sink.emit_at_ms(
            DiagEvent::FrameRejected {
                reason: FrameRejectReason::Empty,
                prior_pane_count: 2,
                fresh_pane_count: 0,
                frames_ref: None,
            },
            1_001,
        );
        sink.emit_at_ms(
            DiagEvent::DuplicatePaneId {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            },
            1_002,
        );

        let text = std::fs::read_to_string(sink.log_path()).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn rate_limit_reopens_after_window() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        for at_ms in [1_000, 1_001, 6_000] {
            sink.emit_at_ms(
                DiagEvent::FrameRejected {
                    reason: FrameRejectReason::Empty,
                    prior_pane_count: 1,
                    fresh_pane_count: 0,
                    frames_ref: None,
                },
                at_ms,
            );
        }

        let text = std::fs::read_to_string(sink.log_path()).unwrap();
        assert_eq!(
            text.lines().count(),
            2,
            "the same identity emits again once the five-second window has elapsed"
        );
    }

    #[test]
    fn unlimited_bypasses_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        for _ in 0..2 {
            sink.emit_unlimited(DiagEvent::RendererPanic {
                message: "boom".to_owned(),
                backtrace: None,
            });
        }

        let text = std::fs::read_to_string(sink.log_path()).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn frame_ring_keeps_last_eight() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        for i in 0..10 {
            sink.capture_frame_pair("drop", &i, &(i + 1), i);
        }

        let count = std::fs::read_dir(dir.path().join(DIAG_FRAMES_DIR))
            .unwrap()
            .count();
        assert_eq!(count, DIAG_FRAME_RING);
    }

    #[test]
    fn frame_capture_names_do_not_collide_within_one_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        let first = sink.capture_frame_pair("drop", &1, &2, 42).unwrap();
        let second = sink.capture_frame_pair("drop", &3, &4, 42).unwrap();

        assert_ne!(first, second);
        assert!(dir.path().join(DIAG_FRAMES_DIR).join(first).exists());
        assert!(dir.path().join(DIAG_FRAMES_DIR).join(second).exists());
    }

    #[test]
    fn frame_ring_prunes_by_numeric_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        for i in 0..10 {
            sink.capture_frame_pair("drop", &i, &(i + 1), i);
        }

        let mut kept = std::fs::read_dir(dir.path().join(DIAG_FRAMES_DIR))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| frame_capture_sort_key(&entry.path()).0)
            .collect::<Vec<_>>();
        kept.sort_unstable();

        assert_eq!(kept, (2..10).collect::<Vec<_>>());
    }
}
