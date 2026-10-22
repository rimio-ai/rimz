//! Single-flight leases whose probes and removal cannot defeat a claim.
//!
//! The directory gate covers only a lease attempt and its body I/O or unlink, never its lifetime hold.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use super::lock::{self, WorkspaceLock};

const GATE_NAME: &str = "lease-gate.lock";
const GATE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Lease {
    file: File,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) enum LeaseAttempt {
    Acquired(Lease),
    Held(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LeaseState {
    Free,
    Held(Vec<u8>),
}

/// Invoke the body writer only after claiming the lease, under the directory gate.
///
/// The writer must not take other locks; holder timestamps can be sampled here after acquisition.
pub(crate) fn claim(
    path: &Path,
    write_body: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<LeaseAttempt> {
    claim_with_timeout(path, write_body, GATE_TIMEOUT)
}

fn claim_with_timeout(
    path: &Path,
    write_body: impl FnOnce(&mut File) -> io::Result<()>,
    timeout: Duration,
) -> io::Result<LeaseAttempt> {
    let _gate = acquire_gate(path, timeout)?;
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match lock::try_lock_file(&mut file, path) {
        Ok(()) => {
            // Scratch stays on the locked inode: atomic replacement would detach the lease.
            file.set_len(0)?;
            write_body(&mut file)?;
            file.flush()?;
            Ok(LeaseAttempt::Acquired(Lease { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(LeaseAttempt::Held(read_body(&mut file)?)),
        Err(error) => Err(io::Error::from(error)),
    }
}

/// Missing leases create nothing; free leases retain their previous body.
pub(crate) fn probe(path: &Path) -> io::Result<LeaseState> {
    probe_with_timeout(path, GATE_TIMEOUT)
}

fn probe_with_timeout(path: &Path, timeout: Duration) -> io::Result<LeaseState> {
    if !path.try_exists()? {
        return Ok(LeaseState::Free);
    }
    let _gate = acquire_gate(path, timeout)?;
    let Some(mut file) = open_existing(path)? else {
        return Ok(LeaseState::Free);
    };
    match lock::try_lock_file(&mut file, path) {
        Ok(()) => {
            file.unlock()?;
            Ok(LeaseState::Free)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(LeaseState::Held(read_body(&mut file)?)),
        Err(error) => Err(io::Error::from(error)),
    }
}

/// Remove a free file under its own directory's gate, returning its removed byte count.
///
/// Gate files themselves are never removed.
pub(crate) fn remove_if_free(path: &Path) -> io::Result<Option<u64>> {
    if path.ends_with(GATE_NAME) || !path.try_exists()? {
        return Ok(None);
    }
    let _gate = acquire_gate(path, GATE_TIMEOUT)?;
    let Some(mut file) = open_existing(path)? else {
        return Ok(None);
    };
    match lock::try_lock_file(&mut file, path) {
        Ok(()) => {
            let bytes = file.metadata()?.len();
            match std::fs::remove_file(path) {
                Ok(()) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(io::Error::from(error)),
    }
}

fn acquire_gate(path: &Path, timeout: Duration) -> io::Result<WorkspaceLock> {
    WorkspaceLock::acquire_with_timeout(&path.with_file_name(GATE_NAME), timeout)
        .map_err(io::Error::other)
}

fn open_existing(path: &Path) -> io::Result<Option<File>> {
    match File::options().read(true).write(true).open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_body(file: &mut File) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    file.read_to_end(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = br#"{"pid":42,"started_at":"2026-10-22T00:00:00Z"}"#;

    #[test]
    fn claims_survive_probes_and_removals_with_complete_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        assert!(matches!(
            claim(&path, |file| file.write_all(BODY)).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
        let start = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                for _ in 0..10_000 {
                    if let LeaseState::Held(body) = probe(&path).unwrap() {
                        assert_eq!(body, BODY, "a held lease must have its complete body");
                    }
                }
            });
            scope.spawn(|| {
                start.wait();
                for _ in 0..1_000 {
                    remove_if_free(&path).unwrap();
                }
            });
            start.wait();
            for _ in 0..1_000 {
                let claim = claim(&path, |file| file.write_all(BODY)).unwrap();
                assert!(
                    matches!(claim, LeaseAttempt::Acquired(_)),
                    "only a claimant may defeat a lease claim"
                );
                drop(claim);
            }
        });
    }

    #[test]
    fn a_live_claimant_excludes_claims_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        let LeaseAttempt::Acquired(guard) = claim(&path, |file| file.write_all(BODY)).unwrap()
        else {
            panic!("a free lease must be claimed");
        };
        assert_eq!(probe(&path).unwrap(), LeaseState::Held(BODY.to_vec()));
        assert!(matches!(
            claim(&path, |_| panic!("a contender must not write a held lease")).unwrap(),
            LeaseAttempt::Held(body) if body == BODY
        ));
        assert_eq!(remove_if_free(&path).unwrap(), None);
        drop(guard);
        assert_eq!(probe(&path).unwrap(), LeaseState::Free);
    }

    #[test]
    fn missing_probes_create_no_directory_gate_or_lease() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("missing");
        let path = parent.join("lease.lock");
        assert_eq!(probe(&path).unwrap(), LeaseState::Free);
        assert_eq!(remove_if_free(&path).unwrap(), None);
        assert!(!parent.exists());
        std::fs::create_dir(&parent).unwrap();
        assert_eq!(probe(&path).unwrap(), LeaseState::Free);
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
    }

    #[test]
    fn free_probes_preserve_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        std::fs::write(&path, BODY).unwrap();
        assert_eq!(probe(&path).unwrap(), LeaseState::Free);
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
    }

    #[test]
    fn claim_gate_timeout_is_an_error_not_a_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        let gate = dir.path().join(GATE_NAME);
        let _held = WorkspaceLock::acquire(&gate).unwrap();
        let result = claim_with_timeout(
            &path,
            |file| file.write_all(BODY),
            Duration::from_millis(10),
        );
        assert!(matches!(result, Err(ref error) if error.to_string().contains("timed out")));
        let error = result.err().unwrap().to_string();
        assert!(error.contains(gate.to_str().unwrap()));
        assert!(error.contains("fuser"));
        assert!(!path.exists());
    }

    #[test]
    fn probe_gate_timeout_is_an_error_not_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        std::fs::write(&path, BODY).unwrap();
        let _held = WorkspaceLock::acquire(&dir.path().join(GATE_NAME)).unwrap();
        let result = probe_with_timeout(&path, Duration::from_millis(10));
        assert!(matches!(result, Err(ref error) if error.to_string().contains("timed out")));
    }

    #[test]
    fn raw_legacy_holders_stay_held_including_empty_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.lock");
        std::fs::write(&path, BODY).unwrap();
        let held = File::options().write(true).open(&path).unwrap();
        held.try_lock().unwrap();
        assert_eq!(probe(&path).unwrap(), LeaseState::Held(BODY.to_vec()));
        held.set_len(0).unwrap();
        assert_eq!(probe(&path).unwrap(), LeaseState::Held(Vec::new()));
        assert!(matches!(
            claim(&path, |file| file.write_all(BODY)).unwrap(),
            LeaseAttempt::Held(body) if body.is_empty()
        ));
        assert_eq!(remove_if_free(&path).unwrap(), None);
    }

    #[test]
    fn removal_keeps_the_gate_and_reports_removed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        std::fs::write(&path, BODY).unwrap();
        assert_eq!(remove_if_free(&path).unwrap(), Some(BODY.len() as u64));
        assert!(!path.exists());
        let gate = dir.path().join(GATE_NAME);
        assert!(gate.is_file());
        assert_eq!(remove_if_free(&gate).unwrap(), None);
        assert!(gate.is_file());
        assert!(matches!(
            claim(&path, |file| file.write_all(BODY)).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn failed_body_write_releases_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lease.lock");
        let result = claim(&path, |file| {
            file.write_all(b"partial body")?;
            Err(io::Error::other("body failed"))
        });
        assert!(matches!(result, Err(ref error) if error.to_string() == "body failed"));
        assert!(matches!(
            claim(&path, |file| file.write_all(BODY)).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }
}
