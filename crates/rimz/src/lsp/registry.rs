//! Per-key runtime records; the broker socket and process token establish liveness.

use super::{LspErr, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LaunchId(String);

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
    pub fn is_terminal(self) -> bool {
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
pub struct Entry {
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
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub attached: Vec<AttachedEditor>,
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

pub fn kill_order(entries: &mut [Entry]) {
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

pub fn lock() -> Result<crate::disk::lock::WorkspaceLock> {
    ensure_runtime()?;
    Ok(crate::disk::lock::WorkspaceLock::acquire(
        &crate::disk::paths::lsp_runtime_dir().join("admission.lock"),
    )?)
}

pub fn publish(entry: &Entry) -> Result<()> {
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
        if !directory.file_type()?.is_dir() || directory.file_name() == "queue" {
            continue;
        }
        let bytes = match std::fs::read(directory.path().join("entry.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let entry: Entry = serde_json::from_slice(&bytes)?;
        if directory.file_name() != std::ffi::OsStr::new(&key(&entry.root, &entry.server)?) {
            return Err(LspErr::Protocol("registry key does not match entry".into()));
        }
        entries.push(entry);
    }
    Ok(entries)
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

pub fn is_live(entry: &Entry) -> bool {
    is_live_at(&crate::disk::paths::lsp_runtime_dir(), entry)
}

fn is_live_at(base: &Path, entry: &Entry) -> bool {
    crate::proc::process_is_live(entry.broker_pid, Some(&entry.broker_start_token))
        && request_at(
            base,
            entry,
            &serde_json::json!({"op": "hello"}),
            Duration::from_secs(2),
        )
        .is_ok()
}

/// Call under the admission lock; only dead brokers are swept.
pub fn sweep_locked() -> Result<Vec<Entry>> {
    sweep_at(&crate::disk::paths::lsp_runtime_dir())
}

fn sweep_at(base: &Path) -> Result<Vec<Entry>> {
    let mut live = Vec::new();
    for entry in read_entries_at(base)? {
        if is_live_at(base, &entry)
            || crate::proc::process_is_live(entry.broker_pid, Some(&entry.broker_start_token))
        {
            live.push(entry);
            continue;
        }
        match std::fs::remove_dir_all(directory_at(base, &entry.root, &entry.server)?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
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
pub fn live_for_checkout(root: &Path) -> Result<Vec<Entry>> {
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
    // Worktree removal calls this after the directory has gone; its host path is already absolute.
    let root = std::fs::canonicalize(root)
        .unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(root));
    acknowledge_all(
        read_entries()?
            .into_iter()
            .filter(|entry| entry.root == root),
        &serde_json::json!({"op": "stop", "reason": reason}),
    )
}

pub(super) fn acknowledge_all(
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

    #[test]
    fn registry_round_trip_and_kill_order() {
        let entry = Entry {
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
            attached: Vec::new(),
            leases: vec![Lease {
                launch_id: Some("launch-1".to_owned().into()),
                pid: 3,
                start_token: "lease-start".into(),
                since_ms: 200,
            }],
        };
        assert_eq!(
            serde_json::from_value::<Entry>(serde_json::to_value(&entry).unwrap()).unwrap(),
            entry
        );
        let mut legacy = serde_json::to_value(&entry).unwrap();
        legacy.as_object_mut().unwrap().remove("attached");
        let parsed = serde_json::from_value::<Entry>(legacy);
        assert!(
            parsed.is_ok(),
            "legacy entries must remain readable: {parsed:?}"
        );
        assert!(parsed.unwrap().attached.is_empty());
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
