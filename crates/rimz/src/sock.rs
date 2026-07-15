//! AF_UNIX socket path budgeting shared by every RimZ socket surface.
//!
//! The OS limit includes the trailing NUL byte. Public headroom values in this
//! module therefore report bytes-used including that terminator, while
//! [`path_len`] reports the path bytes themselves for callers that need to match
//! backend diagnostics.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
pub const AF_UNIX_PATH_LIMIT: usize = 104;
#[cfg(not(target_os = "macos"))]
pub const AF_UNIX_PATH_LIMIT: usize = 108;

pub const LONGEST_SOCKET_FILENAME: &str = "sidebar.123456789012.sock";
pub const LONGEST_SOCKET_TAIL_LEN: usize = LONGEST_SOCKET_FILENAME.len() + 1;
pub const XDG_REMEDY: &str = "export XDG_RUNTIME_DIR=/tmp/rimz-$(id -u)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SockBudget {
    pub sock_dir: PathBuf,
    pub used: usize,
    pub limit: usize,
}

impl SockBudget {
    pub fn for_sock_dir(sock_dir: &Path) -> Self {
        let longest_path = sock_dir.join(LONGEST_SOCKET_FILENAME);
        Self {
            sock_dir: sock_dir.to_path_buf(),
            used: path_len(&longest_path) + 1,
            limit: AF_UNIX_PATH_LIMIT,
        }
    }

    pub fn fits(&self) -> bool {
        self.used <= self.limit
    }

    pub fn longest_path(&self) -> PathBuf {
        self.sock_dir.join(LONGEST_SOCKET_FILENAME)
    }

    pub fn validate(&self) -> Result<(), SocketPathTooLong> {
        validate_socket_path(&self.longest_path())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "socket path {path} needs {used} bytes; AF_UNIX allows {limit} including the terminator.\nPoint RimZ at a shorter runtime directory and re-run rimz:\n\n    {remedy}\n\nAdd the export to your shell profile to make it permanent. `rimz doctor` reports the socket headroom.",
    remedy = XDG_REMEDY
)]
pub struct SocketPathTooLong {
    pub path: PathBuf,
    pub used: usize,
    pub limit: usize,
}

pub fn validate_socket_path(path: &Path) -> Result<(), SocketPathTooLong> {
    let used = path_len(path) + 1;
    if used > AF_UNIX_PATH_LIMIT {
        return Err(SocketPathTooLong {
            path: path.to_path_buf(),
            used,
            limit: AF_UNIX_PATH_LIMIT,
        });
    }
    Ok(())
}

#[cfg(unix)]
pub fn path_len(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    path.as_os_str().as_bytes().len()
}

#[cfg(not(unix))]
pub fn path_len(path: &Path) -> usize {
    path.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_with_len(len: usize) -> PathBuf {
        let base = Path::new("/tmp");
        let component_len = len - path_len(base) - 1;
        let path = base.join("s".repeat(component_len));
        assert_eq!(path_len(&path), len);
        path
    }

    #[test]
    fn budget_accepts_limit_including_terminator_and_rejects_one_more() {
        let max_sock_dir_len = AF_UNIX_PATH_LIMIT - LONGEST_SOCKET_TAIL_LEN - 1;
        let at_limit = SockBudget::for_sock_dir(&path_with_len(max_sock_dir_len));
        let overflow = SockBudget::for_sock_dir(&path_with_len(max_sock_dir_len + 1));

        assert_eq!(at_limit.used, AF_UNIX_PATH_LIMIT);
        assert!(at_limit.fits());
        assert_eq!(overflow.used, AF_UNIX_PATH_LIMIT + 1);
        assert!(!overflow.fits());
    }

    #[test]
    fn socket_path_equal_to_limit_without_terminator_overflows() {
        let path = path_with_len(AF_UNIX_PATH_LIMIT);
        let err = validate_socket_path(&path).expect_err("terminator must overflow");

        assert_eq!(err.used, AF_UNIX_PATH_LIMIT + 1);
        assert_eq!(err.limit, AF_UNIX_PATH_LIMIT);
        assert!(err.to_string().contains(XDG_REMEDY));
    }

    #[test]
    fn short_fallback_root_fits_with_headroom() {
        let sock_dir = Path::new("/tmp/rimz-501/rimz/ws_0123456789abcdef01234567/sock");
        let budget = SockBudget::for_sock_dir(sock_dir);

        assert!(budget.fits(), "{budget:?}");
    }
}
