//! Per-key runtime records; the broker socket and process token establish liveness.

use super::{LspErr, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LaunchId(String);

#[cfg(test)]
impl From<String> for LaunchId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Lease {
    pub launch_id: Option<LaunchId>,
    pub pid: u32,
    pub start_token: String,
    pub since_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Indexing,
    Ready,
    Dormant {
        since_ms: u64,
        reason: Option<StopReason>,
    },
    Stopped {
        reason: StopReason,
        at_ms: u64,
    },
}

impl State {
    pub fn is_running(&self) -> bool {
        matches!(self, State::Starting | State::Indexing | State::Ready)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, thiserror::Error)]
pub enum StopReason {
    #[serde(rename = "released")]
    #[error("released")]
    Released,
    #[serde(rename = "checkout removed")]
    #[error("checkout removed")]
    CheckoutRemoved,
    #[serde(rename = "memory pressure")]
    #[error("memory pressure")]
    MemoryPressure,
    #[serde(rename = "crashed")]
    #[error("crashed")]
    Crashed,
    #[serde(rename = "stopped by hand")]
    #[error("stopped by hand")]
    StoppedByHand,
    #[serde(rename = "never leased")]
    #[error("never leased")]
    NeverLeased,
    #[serde(rename = "idle")]
    #[error("idle")]
    Idle,
    #[serde(rename = "evicted")]
    #[error("evicted")]
    Evicted,
    #[serde(rename = "team done")]
    #[error("team done")]
    TeamDone,
}

impl StopReason {
    pub(super) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CheckoutRemoved | Self::Released | Self::NeverLeased
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AttachedEditor {
    pub pid: u32,
    pub name: Option<String>,
    pub since_ms: u64,
    pub open: Vec<OpenDocument>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct OpenDocument {
    pub uri: String,
    pub owner: bool,
    #[serde(default)]
    pub dirty: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CrashCause {
    pub at_ms: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr_tail: String,
    pub error: Option<String>,
}

impl CrashCause {
    /// How the server ended: its exit code or signal, else the lifetime's error.
    pub fn exit_summary(&self) -> String {
        if let Some(code) = self.exit_code {
            return format!("exit code {code}");
        }
        if let Some(signal) = self.signal {
            return format!("signal {signal}");
        }
        self.error.clone().unwrap_or_else(|| "unknown cause".into())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Entry {
    #[serde(default)]
    pub kind: Option<crate::config::LspServerKind>,
    #[serde(default)]
    pub editor_check_on_save: Option<bool>,
    pub root: PathBuf,
    pub project: Option<PathBuf>,
    pub server: String,
    pub nonce: String,
    pub broker_pid: u32,
    pub broker_start_token: String,
    pub server_pid: Option<u32>,
    pub server_start_token: Option<String>,
    pub state: State,
    pub started_at_ms: u64,
    pub ready_at_ms: Option<u64>,
    pub estimate_bytes: u64,
    pub settings_hash: String,
    pub request_count: u64,
    pub last_request_at_ms: Option<u64>,
    pub peak_rss_kb: u64,
    #[serde(default)]
    pub restarts: u64,
    #[serde(default)]
    pub last_crash: Option<CrashCause>,
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub attached: Vec<AttachedEditor>,
}

impl Entry {
    pub(super) fn broker_is_alive(&self) -> bool {
        crate::proc::process_is_live(self.broker_pid, Some(&self.broker_start_token))
    }

    pub(super) fn has_live_lease_holder(&self) -> bool {
        self.leases
            .iter()
            .any(|lease| crate::proc::process_is_live(lease.pid, Some(&lease.start_token)))
    }
}

pub fn key(root: &Path, server: &str) -> Result<String> {
    if server.is_empty()
        || !server
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(LspErr::Configuration(
            "server names must contain only letters, numbers, - or _".into(),
        ));
    }
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    Ok(format!("{}-{server}", hex::encode(&digest[..8])))
}

pub fn enclosing_checkout<'a>(cwd: &Path, entries: &'a [Entry]) -> Option<&'a Path> {
    entries
        .iter()
        .filter(|entry| cwd.starts_with(&entry.root))
        .max_by_key(|entry| entry.root.components().count())
        .map(|entry| entry.root.as_path())
}

pub(super) fn kill_order(entries: &mut [Entry]) {
    entries.sort_by(|left, right| {
        let rank = |entry: &Entry| {
            (
                entry.last_request_at_ms.unwrap_or(entry.started_at_ms),
                entry.started_at_ms,
            )
        };
        rank(left)
            .cmp(&rank(right))
            .then_with(|| left.root.cmp(&right.root))
            .then_with(|| left.server.cmp(&right.server))
    });
}

pub fn directory(root: &Path, server: &str) -> Result<PathBuf> {
    directory_at(&crate::disk::paths::lsp_runtime_dir(), root, server)
}

fn directory_at(base: &Path, root: &Path, server: &str) -> Result<PathBuf> {
    let directory = base.join(key(root, server)?);
    crate::sock::validate_socket_path(&directory.join("sock"))?;
    Ok(directory)
}

pub(super) fn lock() -> Result<crate::disk::lock::WorkspaceLock> {
    ensure_runtime()?;
    Ok(crate::disk::lock::WorkspaceLock::acquire(
        &crate::disk::paths::lsp_runtime_dir().join("admission.lock"),
    )?)
}

pub(super) fn publish(entry: &Entry) -> Result<()> {
    ensure_runtime()?;
    let directory = directory(&entry.root, &entry.server)?;
    publish_at(&directory, entry)
}

fn publish_at(directory: &Path, entry: &Entry) -> Result<()> {
    crate::disk::paths::ensure_private_runtime_dir(directory)?;
    let _lock = crate::disk::lock::WorkspaceLock::acquire(&directory.join("entry.lock"))?;
    let path = directory.join("entry.json");
    crate::disk::atomic::write_temp_then_rename_cache(&path, entry)?;
    Ok(())
}

fn ensure_runtime() -> Result<()> {
    for path in [
        crate::disk::paths::runtime_home(),
        crate::disk::paths::runtime_rimz_root(),
        crate::disk::paths::lsp_runtime_dir(),
    ] {
        crate::disk::paths::ensure_private_runtime_dir(&path)?;
    }
    Ok(())
}

pub fn read_entries() -> Result<Vec<Entry>> {
    read_entries_at(&crate::disk::paths::lsp_runtime_dir())
}

fn read_entries_at(base: &Path) -> Result<Vec<Entry>> {
    let directories = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut entries = Vec::new();
    for directory in directories {
        let directory = directory?;
        match read_row(&directory) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(error) => record_unreadable_once(directory.path(), &error),
        }
    }
    Ok(entries)
}

/// One row's failure stays that row's: a newer build's entry must not break this build's reads.
fn read_row(directory: &std::fs::DirEntry) -> Result<Option<Entry>> {
    if !directory.file_type()?.is_dir() || directory.file_name() == "queue" {
        return Ok(None);
    }
    let bytes = match std::fs::read(directory.path().join("entry.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let entry: Entry = serde_json::from_slice(&bytes)?;
    if directory.file_name() != std::ffi::OsStr::new(&key(&entry.root, &entry.server)?) {
        return Err(LspErr::Protocol("registry key does not match entry".into()));
    }
    Ok(Some(entry))
}

/// Once per row per process: every query and the memory watchdog re-read the registry.
fn record_unreadable_once(row: PathBuf, error: &LspErr) {
    static RECORDED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let recorded = RECORDED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut recorded) = recorded.lock() else {
        return;
    };
    if !recorded.insert(row.clone()) {
        return;
    }
    crate::diag::lsp::append(&crate::diag::lsp::Record {
        at: jiff::Timestamp::now(),
        root: row,
        server: String::new(),
        event: "unreadable_entry".into(),
        details: serde_json::json!({"error": error.to_string()}),
    });
}

pub fn request(
    entry: &Entry,
    request: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    request_at(
        &crate::disk::paths::lsp_runtime_dir(),
        entry,
        request,
        timeout,
    )
}

fn request_at(
    base: &Path,
    entry: &Entry,
    request: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let mut stream =
        UnixStream::connect(directory_at(base, &entry.root, &entry.server)?.join("sock"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(64 * 1024 * 1024)
        .read_line(&mut response)?;
    if !response.ends_with('\n') {
        return Err(LspErr::Protocol("incomplete socket response".into()));
    }
    let response: serde_json::Value = serde_json::from_str(&response)?;
    if response.get("nonce").and_then(serde_json::Value::as_str) != Some(entry.nonce.as_str()) {
        return Err(LspErr::Protocol("broker nonce changed".into()));
    }
    Ok(response)
}

pub(super) fn is_live(entry: &Entry) -> bool {
    is_live_at(&crate::disk::paths::lsp_runtime_dir(), entry)
}

fn is_live_at(base: &Path, entry: &Entry) -> bool {
    entry.broker_is_alive()
        && request_at(
            base,
            entry,
            &serde_json::json!({"op": "hello"}),
            Duration::from_secs(2),
        )
        .is_ok()
}

pub fn sweep() -> Result<Vec<Entry>> {
    let _lock = lock()?;
    sweep_locked()
}

/// Call under the admission lock; retain lost rows while a lease holder lives.
pub(super) fn sweep_locked() -> Result<Vec<Entry>> {
    sweep_at(&crate::disk::paths::lsp_runtime_dir())
}

fn sweep_at(base: &Path) -> Result<Vec<Entry>> {
    let mut live = Vec::new();
    for entry in read_entries_at(base)? {
        if is_live_at(base, &entry) || entry.broker_is_alive() {
            live.push(entry);
            continue;
        }
        if entry.has_live_lease_holder() {
            continue;
        }
        match std::fs::remove_dir_all(directory_at(base, &entry.root, &entry.server)?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root,
            server: entry.server,
            event: "swept".into(),
            details: serde_json::json!({
                "broker_pid": entry.broker_pid,
                "state": entry.state,
                "leases": entry.leases.len(),
                "peak_rss_kb": entry.peak_rss_kb,
                "last_crash": entry.last_crash,
            }),
        });
    }
    Ok(live)
}

#[cfg(feature = "testkit")]
pub mod testkit {
    use super::*;

    pub fn publish(base: &Path, entry: &Entry) -> Result<()> {
        publish_at(&directory_at(base, &entry.root, &entry.server)?, entry)
    }

    pub fn sweep(base: &Path) -> Result<Vec<Entry>> {
        let _lock = crate::disk::lock::WorkspaceLock::acquire(&base.join("admission.lock"))?;
        sweep_at(base)
    }
}

/// Read-only: launch compilation must not create runtime files or sweep entries.
fn live_for_checkout(root: &Path) -> Result<Vec<Entry>> {
    let root = std::fs::canonicalize(root)?;
    Ok(read_entries()?
        .into_iter()
        .filter(|entry| {
            entry.root == root && !matches!(entry.state, State::Stopped { .. }) && is_live(entry)
        })
        .collect())
}

pub fn live_server_names(root: &Path) -> Result<Vec<String>> {
    Ok(live_for_checkout(root)?
        .into_iter()
        .map(|entry| entry.server)
        .collect())
}

pub fn stop_checkout(root: &Path, reason: StopReason) -> Result<()> {
    acknowledge_all(
        checkout_entries(root)?,
        &serde_json::json!({"op": "stop", "reason": reason}),
    )
}

pub fn register_lease(root: &Path, launch_id: Option<&str>, pid: u32) -> Result<()> {
    let start_token = crate::proc::process_start_token(pid)
        .ok_or_else(|| LspErr::Protocol(format!("cannot identify lease process {pid}")))?;
    acknowledge_all(
        live_for_checkout(root)?,
        &serde_json::json!({"op": "lease", "launch_id": launch_id, "pid": pid, "start_token": start_token}),
    )
}

pub fn release_lease(root: &Path, launch_id: Option<&str>, pid: u32) -> Result<()> {
    acknowledge_all(
        checkout_entries(root)?,
        &serde_json::json!({"op": "release", "launch_id": launch_id, "pid": pid}),
    )
}

fn checkout_entries(root: &Path) -> Result<impl Iterator<Item = Entry>> {
    // Worktree removal calls this after the directory has gone; its host path is already absolute.
    let root = std::fs::canonicalize(root)
        .unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(root));
    Ok(read_entries()?
        .into_iter()
        .filter(move |entry| entry.root == root))
}

fn acknowledge_all(
    entries: impl IntoIterator<Item = Entry>,
    operation: &serde_json::Value,
) -> Result<()> {
    let mut first_error = None;
    for entry in entries {
        let response = request(&entry, operation, Duration::from_secs(2)).and_then(|response| {
            if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                Ok(())
            } else {
                Err(LspErr::Protocol(format!(
                    "{} did not acknowledge {}",
                    entry.server, operation["op"]
                )))
            }
        });
        if let Err(error) = response {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_checkout_removal_and_lease_expiry_are_terminal() {
        for reason in [
            StopReason::Released,
            StopReason::NeverLeased,
            StopReason::CheckoutRemoved,
        ] {
            assert!(reason.is_terminal(), "{reason} ends the broker");
        }
        for reason in [
            StopReason::MemoryPressure,
            StopReason::Crashed,
            StopReason::StoppedByHand,
        ] {
            assert!(!reason.is_terminal(), "{reason} permits restart");
        }
    }

    #[test]
    fn stop_reasons_preserve_wire_strings() {
        for text in ["idle", "evicted", "team done"] {
            let reason = serde_json::from_value::<StopReason>(serde_json::json!(text));
            assert!(reason.is_ok(), "{text} must be a stop reason");
            assert_eq!(serde_json::to_value(reason.unwrap()).unwrap(), text);
        }
        for (reason, text) in [
            (StopReason::Released, "released"),
            (StopReason::CheckoutRemoved, "checkout removed"),
            (StopReason::MemoryPressure, "memory pressure"),
            (StopReason::Crashed, "crashed"),
            (StopReason::StoppedByHand, "stopped by hand"),
            (StopReason::NeverLeased, "never leased"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), text);
            assert_eq!(
                serde_json::from_value::<StopReason>(serde_json::json!(text)).unwrap(),
                reason
            );
            assert_eq!(reason.to_string(), text);
        }
    }

    #[test]
    fn registry_keys_are_stable_and_reject_path_components() {
        let key = key(Path::new("/checkout"), "rust").unwrap();
        assert_eq!(key.len(), 21);
        assert!(key.ends_with("-rust"));
        assert_eq!(key, super::key(Path::new("/checkout"), "rust").unwrap());
        assert!(super::key(Path::new("/checkout"), "../rust").is_err());
        assert!(
            directory_at(
                Path::new(&format!("/{}", "x".repeat(90))),
                Path::new("/checkout"),
                "rust"
            )
            .is_err()
        );
        assert!(
            directory_at(
                Path::new(&format!("/{}", "x".repeat(70))),
                Path::new("/checkout"),
                "rust"
            )
            .is_ok()
        );
    }

    fn sample_entry() -> Entry {
        Entry {
            kind: Some(crate::config::LspServerKind::RustAnalyzer),
            editor_check_on_save: Some(false),
            root: "/checkout".into(),
            project: Some("/project".into()),
            server: "rust".into(),
            nonce: "nonce".into(),
            broker_pid: 1,
            broker_start_token: "start".into(),
            server_pid: Some(2),
            server_start_token: Some("server-start".into()),
            state: State::Ready,
            started_at_ms: 100,
            ready_at_ms: Some(200),
            estimate_bytes: 8000,
            settings_hash: "hash".into(),
            request_count: 0,
            last_request_at_ms: None,
            peak_rss_kb: 6,
            restarts: 0,
            last_crash: None,
            attached: Vec::new(),
            leases: vec![Lease {
                launch_id: Some("launch-1".to_owned().into()),
                pid: 3,
                start_token: "lease-start".into(),
                since_ms: 200,
            }],
        }
    }

    #[test]
    fn unreadable_rows_are_skipped_recorded_once_and_never_swept() {
        let base = tempfile::tempdir().unwrap();
        let good = sample_entry();
        publish_at(
            &directory_at(base.path(), &good.root, &good.server).unwrap(),
            &good,
        )
        .unwrap();
        let garbage = base.path().join("garbage");
        std::fs::create_dir(&garbage).unwrap();
        std::fs::write(garbage.join("entry.json"), b"not json").unwrap();
        let mismatched = base.path().join("mismatched");
        std::fs::create_dir(&mismatched).unwrap();
        std::fs::write(
            mismatched.join("entry.json"),
            serde_json::to_vec(&good).unwrap(),
        )
        .unwrap();
        let newer = Entry {
            server: "newer".into(),
            ..sample_entry()
        };
        let newer_row = directory_at(base.path(), &newer.root, &newer.server).unwrap();
        std::fs::create_dir(&newer_row).unwrap();
        let state = serde_json::to_string(&State::Stopped {
            reason: StopReason::Released,
            at_ms: 1,
        })
        .unwrap()
        .replace("\"released\"", "\"from a newer build\"");
        let mut value = serde_json::to_value(&newer).unwrap();
        value["state"] = serde_json::from_str(&state).unwrap();
        std::fs::write(newer_row.join("entry.json"), value.to_string()).unwrap();
        let bad_rows = [garbage, mismatched, newer_row];

        let unreadable = || {
            crate::diag::lsp::recent()
                .into_iter()
                .filter(|record| {
                    record.event == "unreadable_entry" && record.root.starts_with(base.path())
                })
                .map(|record| record.root)
                .collect::<Vec<_>>()
        };
        assert_eq!(read_entries_at(base.path()).unwrap(), [good]);
        let mut recorded = unreadable();
        recorded.sort();
        let mut expected = bad_rows.to_vec();
        expected.sort();
        assert_eq!(recorded, expected);
        read_entries_at(base.path()).unwrap();
        assert_eq!(unreadable().len(), bad_rows.len());

        sweep_at(base.path()).unwrap();
        for row in &bad_rows {
            assert!(
                row.join("entry.json").exists(),
                "{} was swept",
                row.display()
            );
        }
    }

    #[test]
    fn registry_base_errors_stay_fatal() {
        let base = tempfile::tempdir().unwrap();
        assert!(
            read_entries_at(&base.path().join("absent"))
                .unwrap()
                .is_empty()
        );
        let file = base.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(read_entries_at(&file).is_err());
    }

    #[test]
    fn registry_round_trip_and_kill_order() {
        let entry = sample_entry();
        assert_eq!(
            serde_json::from_value::<Entry>(serde_json::to_value(&entry).unwrap()).unwrap(),
            entry
        );
        let mut legacy = serde_json::to_value(&entry).unwrap();
        assert!(legacy.as_object().unwrap().contains_key("last_crash"));
        legacy.as_object_mut().unwrap().remove("last_crash");
        legacy.as_object_mut().unwrap().remove("attached");
        legacy.as_object_mut().unwrap().remove("kind");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("editor_check_on_save");
        let parsed = serde_json::from_value::<Entry>(legacy);
        assert!(
            parsed.is_ok(),
            "legacy entries must remain readable: {parsed:?}"
        );
        let parsed = parsed.unwrap();
        assert!(parsed.attached.is_empty());
        assert!(parsed.kind.is_none());
        assert!(parsed.editor_check_on_save.is_none());
        let mut entries = vec![entry.clone(); 4];
        entries[0].server = "used-new".into();
        entries[0].request_count = 2;
        entries[0].last_request_at_ms = Some(400);
        entries[1].server = "unused-new".into();
        entries[1].started_at_ms = 300;
        entries[2].server = "used-old".into();
        entries[2].request_count = 1;
        entries[2].last_request_at_ms = Some(250);
        entries[3].server = "unused-old".into();
        kill_order(&mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.server.as_str())
                .collect::<Vec<_>>(),
            ["unused-old", "used-old", "unused-new", "used-new"]
        );
        entries[3].last_request_at_ms = entries[1].last_request_at_ms;
        entries[3].started_at_ms = 50;
        kill_order(&mut entries);
        assert_eq!(entries[1].server, "used-new");
    }

    #[test]
    fn dormant_state_preserves_stop_context() {
        for reason in [serde_json::Value::Null, serde_json::json!("idle")] {
            let value = serde_json::json!({"dormant": {"since_ms": 42, "reason": reason}});
            let state = serde_json::from_value::<State>(value.clone());
            assert!(
                state.is_ok(),
                "dormant must be addressable without a server"
            );
            assert_eq!(serde_json::to_value(state.unwrap()).unwrap(), value);
        }
    }
}
