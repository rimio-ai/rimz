//! Zellij IPC socket path budgeting.
//!
//! Zellij derives an AF_UNIX socket path from its socket base, protocol
//! contract directory, and session name. On macOS the path budget is especially
//! tight, so Rimz checks the path before asking Zellij to birth a room and
//! classifies the matching stderr if a different environment reaches Zellij
//! first.

use std::path::{Path, PathBuf};

use crate::mux::{MuxErr, Result};

#[cfg(target_os = "macos")]
pub const ZELLIJ_SOCKET_PATH_LIMIT: usize = 104;
#[cfg(not(target_os = "macos"))]
pub const ZELLIJ_SOCKET_PATH_LIMIT: usize = 108;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijSocketHeadroom {
    pub path: PathBuf,
    pub len: usize,
    pub limit: usize,
}

pub fn socket_headroom(session_name: &str) -> ZellijSocketHeadroom {
    socket_headroom_with_xdg_override(session_name, None)
}

pub(crate) fn socket_headroom_with_xdg_override(
    session_name: &str,
    xdg_override: Option<&Path>,
) -> ZellijSocketHeadroom {
    let zellij_socket_dir = env_path("ZELLIJ_SOCKET_DIR");
    let xdg_runtime_dir = env_path("XDG_RUNTIME_DIR");
    socket_headroom_from(
        session_name,
        zellij_socket_dir.as_deref(),
        xdg_override.or(xdg_runtime_dir.as_deref()),
        &std::env::temp_dir(),
        &current_uid(),
    )
}

pub fn socket_preflight(session_name: &str) -> Result<()> {
    validate_headroom(socket_headroom(session_name))
}

pub fn stderr_reports_socket_overflow(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("session name must be less than")
        || (lower.contains("socket") && lower.contains("too long"))
        || lower.contains("zellij_socket_dir")
}

pub(crate) fn socket_headroom_from(
    session_name: &str,
    zellij_socket_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: &str,
) -> ZellijSocketHeadroom {
    let path = expected_socket_path_from(
        session_name,
        zellij_socket_dir,
        xdg_runtime_dir,
        temp_dir,
        uid,
    );
    ZellijSocketHeadroom {
        len: path_len(&path),
        path,
        limit: ZELLIJ_SOCKET_PATH_LIMIT,
    }
}

pub(crate) fn expected_socket_path_from(
    session_name: &str,
    zellij_socket_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: &str,
) -> PathBuf {
    let base = if let Some(dir) = zellij_socket_dir {
        dir.to_path_buf()
    } else if cfg!(target_os = "linux") {
        xdg_runtime_dir
            .map(|dir| dir.join("zellij"))
            .unwrap_or_else(|| temp_dir.join(format!("zellij-{uid}")))
    } else {
        temp_dir.join(format!("zellij-{uid}"))
    };
    base.join("contract_version_1").join(session_name)
}

pub(crate) fn validate_headroom(headroom: ZellijSocketHeadroom) -> Result<()> {
    if headroom.len >= headroom.limit {
        return Err(MuxErr::SocketPathTooLong {
            path: headroom.path,
            len: headroom.len,
            limit: headroom.limit,
        });
    }
    Ok(())
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn path_len(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    path.as_os_str().as_bytes().len()
}

#[cfg(not(unix))]
fn path_len(path: &Path) -> usize {
    path.to_string_lossy().len()
}

#[cfg(unix)]
fn current_uid() -> String {
    nix::unistd::Uid::current().as_raw().to_string()
}

#[cfg(not(unix))]
fn current_uid() -> String {
    "0".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_socket_dir_wins_verbatim() {
        let path = expected_socket_path_from(
            "rimz-room",
            Some(Path::new("/short/socket")),
            Some(Path::new("/runtime")),
            Path::new("/tmp"),
            "1000",
        );

        assert_eq!(
            path,
            Path::new("/short/socket/contract_version_1/rimz-room")
        );
    }

    #[test]
    fn xdg_runtime_dir_is_used_on_linux_only() {
        let path = expected_socket_path_from(
            "rimz-room",
            None,
            Some(Path::new("/run/user/1000")),
            Path::new("/tmp"),
            "1000",
        );

        if cfg!(target_os = "linux") {
            assert_eq!(
                path,
                Path::new("/run/user/1000/zellij/contract_version_1/rimz-room")
            );
        } else {
            assert_eq!(
                path,
                Path::new("/tmp/zellij-1000/contract_version_1/rimz-room")
            );
        }
    }

    #[test]
    fn temp_dir_fallback_includes_uid() {
        let path =
            expected_socket_path_from("rimz-room", None, None, Path::new("/tmp/base"), "501");

        assert_eq!(
            path,
            Path::new("/tmp/base/zellij-501/contract_version_1/rimz-room")
        );
    }

    #[test]
    fn path_equal_to_limit_is_rejected() {
        let base = Path::new("/tmp/z");
        let fixed = path_len(&base.join("contract_version_1"));
        let session_len = ZELLIJ_SOCKET_PATH_LIMIT - fixed - 1;
        let headroom = socket_headroom_from(
            &"s".repeat(session_len),
            Some(base),
            None,
            Path::new("/tmp"),
            "1000",
        );

        assert_eq!(headroom.len, ZELLIJ_SOCKET_PATH_LIMIT);
        assert!(matches!(
            validate_headroom(headroom),
            Err(MuxErr::SocketPathTooLong { .. })
        ));
    }

    #[test]
    fn socket_overflow_stderr_matcher_covers_known_shapes() {
        assert!(stderr_reports_socket_overflow(
            "session name must be less than 20 characters"
        ));
        assert!(stderr_reports_socket_overflow(
            "failed to bind socket: File name too long"
        ));
        assert!(stderr_reports_socket_overflow(
            "try setting ZELLIJ_SOCKET_DIR to a shorter path"
        ));
        assert!(!stderr_reports_socket_overflow("session already exists"));
        assert!(!stderr_reports_socket_overflow("session not found"));
    }
}
