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
use crate::schema::notify_trace::{NotifyTraceEnvelope, NotifyTraceEvent};

pub mod binding;
mod notify;
pub mod plugin_presence;
pub(crate) mod rotating;

const DIAG_LOG_NAME: &str = "diag.log.jsonl";
const DIAG_LOG_MAX_BYTES: u64 = 1_048_576;
const DIAG_FRAMES_DIR: &str = "diag-frames";
const DIAG_FRAME_RING: usize = 8;
/// Matches the observer diagnostics cadence (`OBSERVE_COOLDOWN`) so per-tick
/// repeats collapse into periodic records carrying their suppressed count.
const DIAG_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);
static DIAG_FRAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct DiagSink {
    state_root: PathBuf,
    workspace_id: WorkspaceId,
    session_name: String,
    instance_id: Option<SidebarInstanceId>,
    limiter: Arc<Mutex<HashMap<String, LimiterEntry>>>,
}

#[derive(Clone, Debug, Default)]
struct LimiterEntry {
    last_emit_ms: Option<u64>,
    suppressed: u32,
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
        self.emit_at_ms(event, crate::sidebar::timing::unix_now_ms());
    }

    pub fn emit_at_ms(&self, event: DiagEvent, at_ms: u64) {
        let Some(suppressed_since_last) = self.suppression(&event, at_ms) else {
            return;
        };
        self.append(event, at_ms, suppressed_since_last);
    }

    pub fn emit_unlimited(&self, event: DiagEvent) {
        self.append(event, crate::sidebar::timing::unix_now_ms(), 0);
    }

    /// Append a notification trace record to the sibling `notify.log.jsonl`.
    /// Reuses this sink's workspace identity and plumbing; the trace stream is
    /// never rate-limited, so every notification, bell decision, and unread
    /// transition lands.
    pub fn trace_notify(&self, event: NotifyTraceEvent) {
        self.trace_notify_at_ms(event, crate::sidebar::timing::unix_now_ms());
    }

    pub fn trace_notify_at_ms(&self, event: NotifyTraceEvent, at_ms: u64) {
        let envelope = NotifyTraceEnvelope::new(
            self.workspace_id.clone(),
            self.session_name.clone(),
            self.instance_id.clone(),
            at_ms,
            event,
        );
        notify::append(&self.state_root, &envelope);
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

    fn append(&self, event: DiagEvent, at_ms: u64, suppressed_since_last: u32) {
        let path = self.log_path();
        let envelope = DiagEnvelope::new(
            self.workspace_id.clone(),
            self.session_name.clone(),
            self.instance_id.clone(),
            at_ms,
            event,
        )
        .with_suppressed(suppressed_since_last);
        if let Err(err) = rotating::append_rotating_jsonl(&path, DIAG_LOG_MAX_BYTES, &envelope) {
            tracing::debug!(path = %path.display(), error = %err, "diagnostic append failed");
        }
    }

    fn suppression(&self, event: &DiagEvent, at_ms: u64) -> Option<u32> {
        let key = event.identity_key();
        let Ok(mut limiter) = self.limiter.lock() else {
            return Some(0);
        };
        let window_ms = DIAG_RATE_LIMIT_WINDOW.as_millis() as u64;
        limiter.retain(|entry_key, entry| {
            entry_key == &key
                || entry.suppressed > 0
                || entry
                    .last_emit_ms
                    .is_some_and(|last| at_ms.saturating_sub(last) < window_ms)
        });
        let entry = limiter.entry(key).or_default();
        if entry
            .last_emit_ms
            .is_some_and(|last| at_ms.saturating_sub(last) < window_ms)
        {
            entry.suppressed = entry.suppressed.saturating_add(1);
            return None;
        }
        let suppressed = std::mem::take(&mut entry.suppressed);
        entry.last_emit_ms = Some(at_ms);
        Some(suppressed)
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
        // A group migration is a row moving between groups. A cwd that changes
        // while the group identity holds (e.g. a worktree pane whose cwd flaps
        // between two paths that both fold to `external`) is not a migration —
        // gating on cwd here recorded spurious `external -> external` self-moves.
        if prev_group.group == next_group.group {
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
        crate::SidebarWorktreeKind::Channel => "channel",
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
        for at_ms in [1_000, 1_001, 31_001] {
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
            "the same identity emits again once the thirty-second window has elapsed"
        );
    }

    #[test]
    fn rate_limit_flushes_suppressed_count_after_window() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());
        for at_ms in [1_000, 1_001, 1_002, 31_003] {
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
        let records = text
            .lines()
            .map(|line| serde_json::from_str::<DiagEnvelope>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].suppressed_since_last, 0);
        assert_eq!(records[1].suppressed_since_last, 2);
    }

    #[test]
    fn rate_limit_keeps_suppressed_count_across_foreign_emit() {
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
                prior_pane_count: 1,
                fresh_pane_count: 0,
                frames_ref: None,
            },
            15_000,
        );
        sink.emit_at_ms(
            DiagEvent::DuplicatePaneId {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            },
            32_000,
        );
        sink.emit_at_ms(
            DiagEvent::FrameRejected {
                reason: FrameRejectReason::Empty,
                prior_pane_count: 1,
                fresh_pane_count: 0,
                frames_ref: None,
            },
            33_000,
        );

        let text = std::fs::read_to_string(sink.log_path()).unwrap();
        let records = text
            .lines()
            .map(|line| serde_json::from_str::<DiagEnvelope>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(records.len(), 3);
        assert_eq!(records[2].suppressed_since_last, 1);
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

    fn snapshot_in_group(
        kind: crate::SidebarWorktreeKind,
        key: &str,
        pane: &str,
        cwd: Option<&str>,
    ) -> crate::SidebarSnapshot {
        let mut pane_ref = crate::pane::PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, pane));
        pane_ref.cwd = cwd.map(ToOwned::to_owned);
        let row = crate::SidebarRow {
            id: pane.to_owned(),
            name: pane.to_owned(),
            pane: Some(pane_ref),
            worktree_path: None,
            worktree_branch: None,
            channel: None,
            unread: false,
            inactive: false,
            last_activity: jiff::Timestamp::from_second(1_000).unwrap(),
            card: crate::RowCard::Process(crate::ProcessCard::default()),
        };
        let group = crate::SidebarWorktreeGroup {
            key: key.to_owned(),
            label: key.to_owned(),
            kind,
            status_counts: Vec::new(),
            rows: vec![row],
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
        };
        let mut snapshot = crate::SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/repo")),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::from_second(1_000).unwrap(),
        );
        snapshot.worktree_groups = vec![group];
        snapshot
    }

    #[test]
    fn cwd_flap_within_one_group_is_not_a_migration() {
        use crate::SidebarWorktreeKind::External;
        // The pane stays in the `external` group while its cwd flaps between two
        // out-of-project paths — a cwd change, not a row moving between groups.
        let prev = snapshot_in_group(External, "external", "terminal_1", Some("/tmp/a"));
        let next = snapshot_in_group(External, "external", "terminal_1", Some("/tmp/b"));

        assert!(diff_group_migrations(&prev, &next).is_empty());
    }

    #[test]
    fn moving_between_groups_records_one_migration() {
        let prev = snapshot_in_group(
            crate::SidebarWorktreeKind::External,
            "external",
            "terminal_1",
            Some("/tmp/a"),
        );
        let next = snapshot_in_group(
            crate::SidebarWorktreeKind::Worktree,
            "/repo/feature",
            "terminal_1",
            Some("/repo/feature"),
        );

        assert!(matches!(
            diff_group_migrations(&prev, &next).as_slice(),
            [DiagEvent::GroupMigration { pane_id, .. }] if pane_id.raw() == "terminal_1"
        ));
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
