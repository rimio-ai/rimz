//! Atomic provider-file writes that preserve shared settings links.

use std::path::Path;

use crate::disk::atomic;

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) -> atomic::Result<()> {
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
            Ok(_) => return atomic::write_bytes_atomically(&target, bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return atomic::write_bytes_atomically(&target, bytes);
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
    use std::os::unix::fs::symlink;

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
}
