//! Durable anomaly diagnostics for sidebar state.
//!
//! The log is a human/debugging surface: producer and renderer code append
//! typed JSONL records, while correctness continues to read store/cache truth.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::diag::notify::{NotifyTraceEnvelope, NotifyTraceEvent};
use crate::diag::record::{DiagEnvelope, DiagEvent};
use crate::ids::{SidebarInstanceId, WorkspaceId};

pub mod binding;
pub mod notify;
pub mod plugin_presence;
pub mod record;
pub(crate) mod rotating;

pub use rotating::JsonlLog;

const DIAG_LOG_NAME: &str = "diag.log.jsonl";
const DIAG_LOG_MAX_BYTES: u64 = 1_048_576;
const DIAG_FRAMES_DIR: &str = "diag-frames";
const DIAG_FRAME_RING: usize = 8;
/// The one diagnostics rate-limit window, applied per [`DiagEvent::identity_key`]
/// so per-tick repeats on one subject collapse into periodic records carrying
/// their suppressed count while a fault on a different subject reports now.
/// `DIAG_KIND_CEILING` bounds each kind's total within the same window.
const DIAG_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);
const DIAG_KIND_CEILING: u32 = 120;
static DIAG_FRAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct DiagSink {
    inner: Option<Arc<Inner>>,
}

#[derive(Debug)]
struct Inner {
    state_root: PathBuf,
    workspace_id: WorkspaceId,
    session_name: String,
    instance_id: Option<SidebarInstanceId>,
    limiter: Mutex<Limiter>,
}

#[derive(Clone, Debug)]
struct Limiter {
    window: Duration,
    entries: HashMap<String, LimiterEntry>,
    kind_windows: HashMap<String, KindWindow>,
}

#[derive(Clone, Debug, Default)]
struct LimiterEntry {
    last_emit_ms: Option<u64>,
    suppressed: u32,
}

#[derive(Clone, Debug)]
struct KindWindow {
    window_start_ms: u64,
    emitted: u32,
    dropped: u32,
}

impl Limiter {
    fn new(window: Duration) -> Self {
        Self {
            window,
            entries: HashMap::new(),
            kind_windows: HashMap::new(),
        }
    }

    /// Returns `Some(suppressed_since_last)` when `key` may emit now; `None`
    /// when the emission is suppressed.
    fn allow(&mut self, key: &str, kind: &str, at_ms: u64) -> Option<u32> {
        let window_ms = self.window.as_millis() as u64;
        self.gc(at_ms, window_ms, key);
        let suppressed = {
            let entry = self.entries.entry(key.to_owned()).or_default();
            if entry
                .last_emit_ms
                .is_some_and(|last| at_ms.saturating_sub(last) < window_ms)
            {
                entry.suppressed = entry.suppressed.saturating_add(1);
                return None;
            }
            entry.suppressed
        };
        let kind_dropped = self.allow_kind_emit(kind, at_ms, window_ms)?;
        let entry = self.entries.entry(key.to_owned()).or_default();
        entry.suppressed = 0;
        entry.last_emit_ms = Some(at_ms);
        Some(suppressed.saturating_add(kind_dropped))
    }

    fn allow_kind_only(&mut self, kind: &str, at_ms: u64) -> Option<u32> {
        let window_ms = self.window.as_millis() as u64;
        self.gc(at_ms, window_ms, "");
        self.allow_kind_emit(kind, at_ms, window_ms)
    }

    fn gc(&mut self, at_ms: u64, window_ms: u64, key: &str) {
        self.entries.retain(|entry_key, entry| {
            entry_key == key
                || entry.suppressed > 0
                || entry
                    .last_emit_ms
                    .is_some_and(|last| at_ms.saturating_sub(last) < window_ms)
        });
        self.kind_windows.retain(|_, window| {
            window.dropped > 0 || at_ms.saturating_sub(window.window_start_ms) < window_ms
        });
    }

    fn allow_kind_emit(&mut self, kind: &str, at_ms: u64, window_ms: u64) -> Option<u32> {
        let window = self
            .kind_windows
            .entry(kind.to_owned())
            .or_insert_with(|| KindWindow {
                window_start_ms: at_ms,
                emitted: 0,
                dropped: 0,
            });
        if at_ms.saturating_sub(window.window_start_ms) >= window_ms {
            window.window_start_ms = at_ms;
            window.emitted = 0;
        }
        if window.emitted >= DIAG_KIND_CEILING {
            window.dropped = window.dropped.saturating_add(1);
            return None;
        }
        window.emitted = window.emitted.saturating_add(1);
        Some(std::mem::take(&mut window.dropped))
    }
}

impl DiagSink {
    pub fn for_workspace(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        instance_id: Option<SidebarInstanceId>,
    ) -> Self {
        let state = match crate::StatePaths::for_workspace(workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                tracing::debug!(error = %err, "diagnostic sink unavailable");
                return Self::disabled();
            }
        };
        Self::under(state.root, workspace_id, session_name, instance_id)
    }

    pub fn under(
        state_root: PathBuf,
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        instance_id: Option<SidebarInstanceId>,
    ) -> Self {
        Self {
            inner: Some(Arc::new(Inner {
                state_root,
                workspace_id,
                session_name: session_name.into(),
                instance_id,
                limiter: Mutex::new(Limiter::new(DIAG_RATE_LIMIT_WINDOW)),
            })),
        }
    }

    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn log_path(&self) -> Option<PathBuf> {
        Some(self.inner.as_ref()?.log_path())
    }

    pub fn frames_dir(&self) -> Option<PathBuf> {
        Some(self.inner.as_ref()?.frames_dir())
    }

    pub fn session_name(&self) -> &str {
        self.inner
            .as_ref()
            .map_or("", |inner| inner.session_name.as_str())
    }

    #[cfg(test)]
    pub(crate) fn frame_capture_path(&self, frames_ref: &str) -> Option<PathBuf> {
        Some(self.inner.as_ref()?.frames_dir().join(frames_ref))
    }

    pub fn emit(&self, event: DiagEvent) {
        self.emit_at_ms(event, crate::sidebar::timing::unix_now_ms());
    }

    pub fn emit_at_ms(&self, event: DiagEvent, at_ms: u64) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let Some(suppressed_since_last) = inner.suppression(&event, at_ms) else {
            return;
        };
        inner.append(event, at_ms, suppressed_since_last);
    }

    pub fn emit_unlimited(&self, event: DiagEvent) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let at_ms = crate::sidebar::timing::unix_now_ms();
        let kind = event.kind_name();
        let Ok(mut limiter) = inner.limiter.lock() else {
            inner.append(event, at_ms, 0);
            return;
        };
        let Some(suppressed_since_last) = limiter.allow_kind_only(kind, at_ms) else {
            return;
        };
        drop(limiter);
        inner.append(event, at_ms, suppressed_since_last);
    }

    /// Append a notification trace record to the sibling `notify.log.jsonl`.
    /// Reuses this sink's workspace identity and plumbing; the trace stream is
    /// never rate-limited, so every notification, bell decision, and unread
    /// transition lands.
    pub fn trace_notify(&self, event: NotifyTraceEvent) {
        self.trace_notify_at_ms(event, crate::sidebar::timing::unix_now_ms());
    }

    pub fn trace_notify_at_ms(&self, event: NotifyTraceEvent, at_ms: u64) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let envelope = NotifyTraceEnvelope::new(
            inner.workspace_id.clone(),
            inner.session_name.clone(),
            inner.instance_id.clone(),
            at_ms,
            event,
        );
        notify::log(&inner.state_root).append(&envelope);
    }

    pub fn capture_frame_pair<T: Serialize>(
        &self,
        kind: &str,
        prior: &T,
        offending: &T,
        at_ms: u64,
    ) -> Option<String> {
        let inner = self.inner.as_ref()?;
        let dir = inner.frames_dir();
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
        if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(&path, &record) {
            tracing::debug!(path = %path.display(), error = %err, "diagnostic frame capture failed");
            return None;
        }
        prune_frame_ring(&dir);
        Some(file_name)
    }
}

impl Inner {
    fn log_path(&self) -> PathBuf {
        self.state_root.join(DIAG_LOG_NAME)
    }

    fn frames_dir(&self) -> PathBuf {
        frames_dir_under(&self.state_root)
    }

    fn append(&self, event: DiagEvent, at_ms: u64, suppressed_since_last: u32) {
        let envelope = DiagEnvelope::new(
            self.workspace_id.clone(),
            self.session_name.clone(),
            self.instance_id.clone(),
            at_ms,
            event,
        )
        .with_suppressed(suppressed_since_last);
        rotating::JsonlLog::new(self.log_path(), DIAG_LOG_MAX_BYTES).append(&envelope);
    }

    fn suppression(&self, event: &DiagEvent, at_ms: u64) -> Option<u32> {
        let key = event.identity_key();
        let kind = event.kind_name();
        let Ok(mut limiter) = self.limiter.lock() else {
            return Some(0);
        };
        limiter.allow(&key, kind, at_ms)
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
    use crate::diag::record::FrameRejectReason;
    use crate::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::from_project_root(Path::new("/repo"))
    }
    fn sink(dir: &Path) -> DiagSink {
        DiagSink::under(dir.to_path_buf(), workspace_id(), "s", None)
    }
    fn frame_rejected(prior_pane_count: usize) -> DiagEvent {
        DiagEvent::FrameRejected {
            reason: FrameRejectReason::Empty,
            prior_pane_count,
            fresh_pane_count: 0,
            frames_ref: None,
        }
    }
    fn duplicate_pane(raw: impl std::fmt::Display) -> DiagEvent {
        DiagEvent::DuplicatePaneId {
            pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw}")),
        }
    }
    fn diag_records(sink: &DiagSink) -> Vec<DiagEnvelope> {
        std::fs::read_to_string(sink.log_path().unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn notify_records(dir: &Path) -> Vec<NotifyTraceEnvelope> {
        std::fs::read_to_string(notify::log(dir).path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn rate_limit_uses_identity_key_and_flushes_suppressed_count() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());

        sink.emit_at_ms(frame_rejected(1), 1_000);
        sink.emit_at_ms(frame_rejected(2), 15_000);
        sink.emit_at_ms(duplicate_pane(1), 32_000);
        sink.emit_at_ms(frame_rejected(1), 33_000);

        let records = diag_records(&sink);
        assert_eq!(
            records
                .into_iter()
                .map(|record| (record.event, record.suppressed_since_last))
                .collect::<Vec<_>>(),
            vec![
                (frame_rejected(1), 0),
                (duplicate_pane(1), 0),
                (frame_rejected(1), 1),
            ]
        );
    }

    #[test]
    fn kind_ceiling_bounds_keyed_and_unlimited_storms() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink(dir.path());

        for i in 0..(DIAG_KIND_CEILING + 3) {
            sink.emit_at_ms(duplicate_pane(i), 1_000);
        }
        sink.emit_at_ms(duplicate_pane("window"), 32_000);
        sink.emit_at_ms(duplicate_pane("window"), 63_000);
        for _ in 0..(DIAG_KIND_CEILING + 1) {
            sink.emit_unlimited(DiagEvent::RendererPanic {
                message: "boom".to_owned(),
                backtrace: None,
            });
        }

        let records = diag_records(&sink);
        let keyed_len = DIAG_KIND_CEILING as usize + 2;
        assert_eq!(records.len(), keyed_len + DIAG_KIND_CEILING as usize);
        assert_eq!(records[DIAG_KIND_CEILING as usize].suppressed_since_last, 3);
        assert_eq!(
            records[DIAG_KIND_CEILING as usize + 1].suppressed_since_last,
            0
        );
        assert!(
            records[..keyed_len]
                .iter()
                .all(|record| matches!(record.event, DiagEvent::DuplicatePaneId { .. }))
        );
        assert!(
            records[keyed_len..]
                .iter()
                .all(|record| matches!(record.event, DiagEvent::RendererPanic { .. }))
        );
    }

    #[test]
    fn trace_notify_writes_envelope_with_sink_identity() {
        let dir = tempfile::tempdir().unwrap();
        let instance_id = SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").unwrap();
        let sink = DiagSink::under(
            dir.path().to_path_buf(),
            workspace_id(),
            "s",
            Some(instance_id.clone()),
        );
        let event = NotifyTraceEvent::BellRing {
            notification_kind: "success".to_owned(),
            fired: true,
            recheck_unread: true,
            panes: Vec::new(),
            suppressed: None,
        };

        sink.trace_notify_at_ms(event.clone(), 1_000);
        sink.trace_notify_at_ms(event.clone(), 1_001);

        let records = notify_records(dir.path());
        assert_eq!(records.len(), 2);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.v, notify::NOTIFY_TRACE_SCHEMA_VERSION);
            assert_eq!(record.build.as_deref(), crate::build_id::current());
            assert!(record.build.is_some());
            assert_eq!(record.workspace_id, workspace_id());
            assert_eq!(record.session_name, "s");
            assert_eq!(record.instance_id.as_ref(), Some(&instance_id));
            assert_eq!(record.at_ms, 1_000 + index as u64);
            assert_eq!(record.event, event);
        }
    }

    #[test]
    fn recent_records_merges_generations_filters_versions_and_caps_results() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/diag-recent-records"));
        let state = crate::StatePaths::for_workspace(workspace_id.clone()).unwrap();
        let _ = std::fs::remove_dir_all(&state.root);
        std::fs::create_dir_all(&state.root).unwrap();
        let live_path = state.root.join(DIAG_LOG_NAME);
        let record = |at_ms| {
            DiagEnvelope::new(
                workspace_id.clone(),
                "s".to_owned(),
                None,
                at_ms,
                frame_rejected(at_ms as usize),
            )
        };
        let mut stale = record(90);
        stale.v = "rimz.diag.v0".to_owned();
        let encode = |record: DiagEnvelope| serde_json::to_string(&record).unwrap();

        std::fs::write(
            rotated_path(&live_path),
            [
                encode(record(40)),
                "not-json".to_owned(),
                encode(stale),
                encode(record(10)),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &live_path,
            [encode(record(50)), encode(record(20)), encode(record(30))].join("\n"),
        )
        .unwrap();

        let (returned_path, records) = recent_records(workspace_id, 3).unwrap();
        assert_eq!(returned_path, live_path);
        assert_eq!(
            records
                .iter()
                .map(|record| record.at_ms)
                .collect::<Vec<_>>(),
            vec![30, 40, 50]
        );
        std::fs::remove_dir_all(state.root).unwrap();
    }

    #[test]
    fn frame_capture_writes_unique_private_ring() {
        let dir = tempfile::tempdir().unwrap();
        let capture_sink = sink(dir.path());
        let first = capture_sink.capture_frame_pair("drop", &1, &2, 42).unwrap();
        let second = capture_sink.capture_frame_pair("drop", &3, &4, 42).unwrap();
        let frames_dir = dir.path().join(DIAG_FRAMES_DIR);

        assert_ne!(first, second);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(frames_dir.join(&first)).unwrap()
            )
            .unwrap(),
            serde_json::json!({ "prior": 1, "offending": 2 })
        );
        assert!(frames_dir.join(second).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&frames_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        let ring_dir = tempfile::tempdir().unwrap();
        let ring_sink = sink(ring_dir.path());
        for i in 0..10 {
            ring_sink.capture_frame_pair("drop", &i, &(i + 1), i);
        }

        let frames = std::fs::read_dir(ring_dir.path().join(DIAG_FRAMES_DIR))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), DIAG_FRAME_RING);

        let mut kept = frames
            .iter()
            .map(|path| frame_capture_sort_key(path).0)
            .collect::<Vec<_>>();
        kept.sort_unstable();

        assert_eq!(kept, (2..10).collect::<Vec<_>>());
        for path in frames {
            let at_ms = frame_capture_sort_key(&path).0;
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

            assert_eq!(value["prior"], at_ms);
            assert_eq!(value["offending"], at_ms + 1);
        }
    }
}
