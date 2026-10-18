//! Zellij IPC socket path budgeting and denied-connect evidence.
//!
//! Zellij derives an AF_UNIX socket path from its socket base, protocol
//! contract directory, and session name. On macOS the path budget is especially
//! tight, so RimZ checks the path before asking Zellij to birth a room and
//! classifies the matching stderr if a different environment reaches Zellij
//! first.
//!
//! A socket omitted from Zellij's live listing gets one nonblocking connect
//! probe. Only a permission denial overrides the listing's classification.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::mux::command::CommandSpec;
use crate::mux::{MuxErr, Result};

const ZELLIJ_SOCKET_PATH_LIMIT: usize = crate::sock::AF_UNIX_PATH_LIMIT;

fn connect_socket(path: &Path) -> std::io::Result<()> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    let socket = socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )?;
    fcntl(&socket, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    let stream = UnixStream::from(socket);
    // A full accept backlog must not turn a diagnostic probe into a wait.
    stream.set_nonblocking(true)?;
    let address = UnixAddr::new(path)?;
    connect(stream.as_raw_fd(), &address).map_err(std::io::Error::from)
}

impl super::ZellijBackend {
    pub(super) fn check_socket_connect(&self, socket: PathBuf) -> Result<()> {
        use std::os::unix::fs::FileTypeExt;

        if !socket
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            return Ok(());
        }
        #[cfg(test)]
        let connect = self.socket_connect.unwrap_or(connect_socket);
        #[cfg(not(test))]
        let connect = connect_socket;
        if let Err(error) = connect(&socket)
            && error.kind() == std::io::ErrorKind::PermissionDenied
        {
            return Err(MuxErr::SocketConnectDenied {
                mux: crate::ids::MuxName::Zellij,
                socket,
                reason: error.to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn check_unlisted_sockets(&self, spec: &CommandSpec, live: &[String]) -> Result<()> {
        let directory = spec_socket_path(spec, "");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(MuxErr::SocketConnectDenied {
                    mux: crate::ids::MuxName::Zellij,
                    socket: directory,
                    reason: error.to_string(),
                });
            }
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            if live.iter().any(|name| entry.file_name() == name.as_str()) {
                continue;
            }
            self.check_socket_connect(entry.path())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijSocketHeadroom {
    pub path: PathBuf,
    pub len: usize,
    pub limit: usize,
}

pub fn socket_headroom(session_name: &str) -> ZellijSocketHeadroom {
    socket_headroom_with_xdg_override(session_name, None)
}

pub(super) fn socket_headroom_with_xdg_override(
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

pub(super) fn stderr_reports_socket_overflow(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("session name must be less than")
        || (lower.contains("socket") && lower.contains("too long"))
        || lower.contains("zellij_socket_dir")
}

/// Whether `output` is Zellij's client refusing a session whose socket is
/// still on disk. The client probes every socket before it dispatches
/// anything and counts a server too busy to answer as absent (`assert_socket`
/// in `zellij-utils`, observed on 0.45.1), so the refusal proves nothing ran.
/// A session that is really gone has no socket, and its refusal stands.
pub(super) fn refused_live_session(spec: &CommandSpec, output: &Output) -> bool {
    if output.status.success() || !super::parse::is_session_not_found(&output.stderr) {
        return false;
    }
    let session = spec
        .args
        .iter()
        .position(|arg| arg == "--session")
        .and_then(|flag| spec.args.get(flag + 1));
    let socket = spec_socket_path(spec, session.map_or("", String::as_str));
    if session.is_some() {
        return socket.symlink_metadata().is_ok();
    }
    // No `--session`: every action carries one, so this is a verb that names
    // its session another way. Any socket beside the `session_info` directory
    // may be the one it refused.
    std::fs::read_dir(socket).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
    })
}

/// Where the Zellij child `spec` starts looks for `session`'s socket.
pub(super) fn spec_socket_path(spec: &CommandSpec, session: &str) -> PathBuf {
    // The child's own environment: a key the spec sets or removes outranks
    // what this process exports.
    let child_env = |key: &str| match spec.env.get(key) {
        Some(value) => Some(PathBuf::from(value)).filter(|path| !path.as_os_str().is_empty()),
        None if spec.env_remove.contains(key) => None,
        None => env_path(key),
    };
    expected_socket_path_from(
        session,
        child_env("ZELLIJ_SOCKET_DIR").as_deref(),
        child_env("XDG_RUNTIME_DIR").as_deref(),
        &child_env("TMPDIR").unwrap_or_else(|| PathBuf::from("/tmp")),
        &current_uid(),
    )
}

fn socket_headroom_from(
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
        len: crate::sock::path_len(&path),
        path,
        limit: ZELLIJ_SOCKET_PATH_LIMIT,
    }
}

fn expected_socket_path_from(
    session_name: &str,
    zellij_socket_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: &str,
) -> PathBuf {
    socket_base_from(zellij_socket_dir, xdg_runtime_dir, temp_dir, uid)
        .join("contract_version_1")
        .join(session_name)
}

pub(in crate::mux) fn socket_base_from(
    zellij_socket_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: &str,
) -> PathBuf {
    if let Some(dir) = zellij_socket_dir {
        dir.to_path_buf()
    } else if cfg!(target_os = "linux") {
        xdg_runtime_dir
            .map(|dir| dir.join("zellij"))
            .unwrap_or_else(|| temp_dir.join(format!("zellij-{uid}")))
    } else {
        temp_dir.join(format!("zellij-{uid}"))
    }
}

fn validate_headroom(headroom: ZellijSocketHeadroom) -> Result<()> {
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
    fn socket_connect_probe_reaches_a_listener_and_preserves_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(connect_socket(&path).is_ok());
        drop(listener);
        assert_eq!(
            connect_socket(&path).unwrap_err().kind(),
            std::io::ErrorKind::ConnectionRefused
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn socket_connect_probe_never_waits_for_a_full_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        nix::sys::socket::listen(&listener, nix::sys::socket::Backlog::new(0).unwrap()).unwrap();
        let _pending = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(
            connect_socket(&path).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

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
        let fixed = crate::sock::path_len(&base.join("contract_version_1"));
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
