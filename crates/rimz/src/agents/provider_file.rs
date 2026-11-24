//! Atomic provider-file writes that preserve shared settings links.

use std::path::{Path, PathBuf};

use crate::disk::atomic;

pub(super) fn write_bytes_mode(path: &Path, bytes: &[u8], mode: u32) -> atomic::Result<()> {
    atomic::write_bytes_atomically_mode(&resolve_write_path(path)?, bytes, mode)
}

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) -> atomic::Result<()> {
    atomic::write_bytes_atomically(&resolve_write_path(path)?, bytes)
}

/// Ensure `path` exists as a regular file, creating an empty file at `mode`
/// (and its parent directories) only when absent. Leaves an existing regular
/// file untouched (never overwriting one created concurrently) and refuses any
/// non-regular entry (symlink, directory, etc.).
pub(crate) fn create_empty_if_absent(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(path)?;
            if meta.is_file() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    if meta.is_symlink() {
                        "expected a regular file, found a symlink; remove or replace it"
                    } else {
                        "expected a regular file; remove or replace it"
                    },
                ))
            }
        }
        Err(error) => Err(error),
    }
}

fn resolve_write_path(path: &Path) -> atomic::Result<PathBuf> {
    let mut target = path.to_path_buf();
    for _ in 0..40 {
        let io_err = |source| atomic::AtomicErr::Io {
            path: target.clone(),
            source,
        };
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_symlink() => {
                let link = std::fs::read_link(&target).map_err(io_err)?;
                // A symlink is a directory entry, so it always has a parent.
                target = target.parent().expect("symlink has a parent").join(link);
            }
            Ok(_) => return Ok(target),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(target);
            }
            Err(error) => return Err(io_err(error)),
        }
    }
    Err(atomic::AtomicErr::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(
            "provider file symlink chain exceeds 40 hops (possible cycle)",
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn writes_through_relative_two_hop_chain() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::write(&target, b"before").unwrap();
        symlink("second", &first).unwrap();
        symlink("target", &second).unwrap();
        write_bytes(&first, b"after").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"after");
        write_bytes_mode(&first, b"exact mode", 0o764).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"exact mode");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o764
        );
        assert_eq!(fs::read_link(first).unwrap(), Path::new("second"));
        assert_eq!(fs::read_link(second).unwrap(), Path::new("target"));
    }

    #[test]
    fn writes_through_dangling_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        write_bytes(&link, b"created").unwrap();
        assert!(target.exists());
        assert_eq!(fs::read(&target).unwrap(), b"created");
        assert_eq!(fs::read_link(link).unwrap(), target);
    }

    #[test]
    fn create_empty_if_absent_creates_0600_preserves_existing_and_refuses_non_regular() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("missing-parent/antigravity-oauth-token");
        create_empty_if_absent(&nested, 0o600).unwrap();
        let meta = fs::symlink_metadata(&nested).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.len(), 0);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);

        // Existing regular file keeps its bytes and mode (never overwritten).
        fs::write(&nested, b"keep-me").unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o640)).unwrap();
        create_empty_if_absent(&nested, 0o600).unwrap();
        assert_eq!(fs::read(&nested).unwrap(), b"keep-me");
        assert_eq!(
            fs::symlink_metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o640
        );

        // Symlink to an existing regular file is refused.
        let sym_existing = dir.path().join("sym-existing");
        symlink(&nested, &sym_existing).unwrap();
        let err = create_empty_if_absent(&sym_existing, 0o600).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");

        // Dangling symlink is refused.
        let sym_dangling = dir.path().join("sym-dangling");
        symlink(dir.path().join("does-not-exist"), &sym_dangling).unwrap();
        let err = create_empty_if_absent(&sym_dangling, 0o600).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");

        // Directory is refused.
        let as_dir = dir.path().join("as-dir");
        fs::create_dir(&as_dir).unwrap();
        let err = create_empty_if_absent(&as_dir, 0o600).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    }
}
