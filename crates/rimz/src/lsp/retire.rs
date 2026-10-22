//! Retire outdated brokers without interrupting their servers or dropping leases.

use super::registry::{self, Entry, State};
use super::{Result, installed};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Outcome {
    pub(crate) retired: usize,
    pub(crate) marked: usize,
    pub(crate) kept: usize,
    pub(crate) unchecked: usize,
}

pub(crate) fn run() -> Outcome {
    let mut outcome = Outcome::default();
    let mut entries = match registry::read_entries() {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, "language-server broker retirement scan failed");
            return outcome;
        }
    };
    entries.retain(Entry::broker_is_alive);
    if entries.is_empty() {
        return outcome;
    }
    let Some(installed) = installed::build() else {
        outcome.unchecked = entries.len();
        return outcome;
    };
    for entry in entries {
        let Ok(build) = crate::build_id::of_file(std::path::Path::new(&format!(
            "/proc/{}/exe",
            entry.broker_pid
        ))) else {
            continue;
        };
        if build == installed || !entry.broker_is_alive() {
            continue;
        }
        let exiting = registry::request(
            &entry,
            &serde_json::json!({"op": "retire"}),
            Duration::from_secs(2),
        )
        .ok()
        .filter(|reply| reply["ok"] == true)
        .and_then(|reply| reply["exiting"].as_bool());
        let decision = match exiting {
            Some(true) => {
                outcome.retired += 1;
                "exiting"
            }
            Some(false) => {
                outcome.marked += 1;
                "marked"
            }
            None => match kill_legacy(&entry).unwrap_or_else(|error| {
                tracing::warn!(broker_pid = entry.broker_pid, %error, "legacy broker retirement failed");
                Fallback::Kept
            }) {
                Fallback::Killed => {
                    outcome.retired += 1;
                    "killed"
                }
                Fallback::Kept => {
                    outcome.kept += 1;
                    "kept"
                }
                Fallback::Gone => "gone",
            },
        };
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root,
            server: entry.server,
            event: "retired".into(),
            details: serde_json::json!({"broker_pid": entry.broker_pid, "build": build, "installed": installed, "outcome": decision}),
        });
    }
    outcome
}

fn kill_legacy(previous: &Entry) -> Result<Fallback> {
    let _lock = registry::lock()?;
    let directory = registry::directory(&previous.root, &previous.server)?;
    let bytes = match std::fs::read(directory.join("entry.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Fallback::Gone),
        Err(error) => return Err(error.into()),
    };
    let current: Entry = serde_json::from_slice(&bytes)?;
    let children = crate::proc::children(current.broker_pid);
    let decision = fallback_decision(previous, &current, previous.broker_is_alive(), &children);
    if decision != Fallback::Killed {
        return Ok(decision);
    }
    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(current.broker_pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) => {}
        Err(nix::errno::Errno::ESRCH) => return Ok(Fallback::Gone),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) && current.broker_is_alive() {
        std::thread::sleep(Duration::from_millis(10));
    }
    if current.broker_is_alive() {
        return Ok(Fallback::Kept);
    }
    match std::fs::remove_file(directory.join("sock")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(Fallback::Killed)
}

#[derive(Debug, PartialEq, Eq)]
enum Fallback {
    Killed,
    Kept,
    Gone,
}

fn fallback_decision(previous: &Entry, current: &Entry, alive: bool, children: &[u32]) -> Fallback {
    if !alive {
        return Fallback::Gone;
    }
    if previous.nonce == current.nonce
        && previous.broker_pid == current.broker_pid
        && previous.broker_start_token == current.broker_start_token
        && matches!(current.state, State::Dormant { .. })
        && current.server_pid.is_none()
        && current.attached.is_empty()
        && children.is_empty()
    {
        Fallback::Killed
    } else {
        Fallback::Kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::registry::{AttachedEditor, State};

    #[test]
    fn fallback_requires_the_same_live_dormant_childless_unattached_broker() {
        let previous: Entry = serde_json::from_value(serde_json::json!({"root": "/checkout", "server": "rust", "nonce": "n", "broker_pid": 7, "broker_start_token": "t", "server_pid": null, "server_start_token": null, "state": {"dormant": {"since_ms": 0, "reason": null}}, "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "s", "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 0, "leases": []})).unwrap();
        let decisions: Vec<_> = (0..10)
            .map(|case| {
                let mut current = previous.clone();
                let mut alive = true;
                let mut children = Vec::new();
                match case {
                    0 => {}
                    1 => current.nonce = "other".into(),
                    2 => current.broker_pid = 8,
                    3 => current.broker_start_token = "reused".into(),
                    4 => alive = false,
                    5 => current.state = State::Ready,
                    6 => current.server_pid = Some(9),
                    7 => current.attached.push(AttachedEditor {
                        pid: 10,
                        name: None,
                        since_ms: 0,
                        open: Vec::new(),
                    }),
                    8 => children.push(9),
                    9 => {
                        current.broker_pid = 8;
                        alive = false;
                    }
                    _ => unreachable!(),
                }
                fallback_decision(&previous, &current, alive, &children)
            })
            .collect();
        assert_eq!(
            decisions,
            [
                Fallback::Killed,
                Fallback::Kept,
                Fallback::Kept,
                Fallback::Kept,
                Fallback::Gone,
                Fallback::Kept,
                Fallback::Kept,
                Fallback::Kept,
                Fallback::Kept,
                Fallback::Gone
            ]
        );
    }
}
