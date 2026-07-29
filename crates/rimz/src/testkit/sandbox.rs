//! Owner-death cleanup for integration-test process and filesystem namespaces.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const REAPER_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const HOME_PREFIX: &str = "rimz-test-home-";
const RUNTIME_PREFIX: &str = "rr";
const ZELLIJ_PREFIX: &str = "rz";
const RANDOM_BYTES: usize = 6;

/// Roots whose environment values identify every process owned by one fixture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxSpec {
    pub home_root: PathBuf,
    pub runtime_root: PathBuf,
}

/// Failure to allocate or arm a test sandbox.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("{action} `{path}`: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid test sandbox: {0}")]
    InvalidSpec(String),
    #[error("serializing test sandbox spec: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Owns one fixture's roots and the independent process that reaps them.
///
/// The reaper's stdin is a keepalive: orderly drop and abrupt owner death both
/// close it, so cleanup does not rely on this process reaching `Drop`.
pub struct TestSandbox {
    _roots: Vec<TempDir>,
    spec: SandboxSpec,
    reaper: Option<ReaperHandle>,
}

impl TestSandbox {
    /// Allocate the split HOME/runtime shape used by the integration `Env`.
    pub fn new() -> Result<Self, SandboxError> {
        let home = tempfile::Builder::new()
            .prefix(HOME_PREFIX)
            .rand_bytes(RANDOM_BYTES)
            .tempdir()
            .map_err(|source| SandboxError::Io {
                action: "creating test HOME",
                path: std::env::temp_dir(),
                source,
            })?;
        let runtime = tempfile::Builder::new()
            .prefix(RUNTIME_PREFIX)
            .rand_bytes(RANDOM_BYTES)
            .tempdir_in("/tmp")
            .map_err(|source| SandboxError::Io {
                action: "creating short test runtime",
                path: PathBuf::from("/tmp"),
                source,
            })?;
        let spec = SandboxSpec {
            home_root: canonical_or_owned(home.path()),
            // Keep `/tmp` lexical here: the managed tmux socket has the same
            // short path shape on macOS even though `/tmp` resolves through
            // `/private`.
            runtime_root: runtime.path().to_path_buf(),
        };
        validate(&spec)?;
        Ok(Self {
            _roots: vec![home, runtime],
            spec,
            reaper: None,
        })
    }

    /// Allocate the single-root namespace expected by the Zellij live fixtures.
    pub fn zellij() -> Result<Self, SandboxError> {
        let root = tempfile::Builder::new()
            .prefix(ZELLIJ_PREFIX)
            .rand_bytes(RANDOM_BYTES)
            .tempdir()
            .map_err(|source| SandboxError::Io {
                action: "creating Zellij test namespace",
                path: std::env::temp_dir(),
                source,
            })?;
        let root_path = canonical_or_owned(root.path());
        let spec = SandboxSpec {
            home_root: root_path.clone(),
            runtime_root: root_path,
        };
        validate(&spec)?;
        Ok(Self {
            _roots: vec![root],
            spec,
            reaper: None,
        })
    }

    /// Start the independent keepalive reaper before any fixture child exists.
    pub fn arm(mut self, reaper_bin: &Path) -> Result<Self, SandboxError> {
        self.reaper = Some(ReaperHandle::spawn(reaper_bin, &self.spec)?);
        Ok(self)
    }

    pub fn spec(&self) -> &SandboxSpec {
        &self.spec
    }

    pub fn home_root(&self) -> &Path {
        &self.spec.home_root
    }

    pub fn runtime_root(&self) -> &Path {
        &self.spec.runtime_root
    }

    /// Stamp the identity roots onto a child that does not use the full Env builder.
    pub fn pin_identity(&self, command: &mut Command) {
        command
            .env("HOME", &self.spec.home_root)
            .env("XDG_RUNTIME_DIR", &self.spec.runtime_root);
    }
}

impl Drop for TestSandbox {
    fn drop(&mut self) {
        if self.reaper.take().is_some_and(ReaperHandle::finish) {
            return;
        }
        cleanup(&self.spec);
    }
}

struct ReaperHandle {
    child: Child,
    keepalive: ChildStdin,
}

impl ReaperHandle {
    fn spawn(reaper_bin: &Path, spec: &SandboxSpec) -> Result<Self, SandboxError> {
        let encoded = serde_json::to_string(spec)?;
        let mut command = Command::new(reaper_bin);
        command
            .arg(encoded)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| SandboxError::Io {
            action: "starting test sandbox reaper",
            path: reaper_bin.to_path_buf(),
            source,
        })?;
        let keepalive = child.stdin.take().ok_or_else(|| {
            SandboxError::InvalidSpec("test sandbox reaper has no keepalive pipe".to_owned())
        })?;
        Ok(Self { child, keepalive })
    }

    fn finish(self) -> bool {
        let Self {
            mut child,
            keepalive,
        } = self;
        drop(keepalive);
        let deadline = Instant::now() + REAPER_WAIT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }
}

/// Reject cleanup requests that do not name roots allocated by this fixture.
pub fn validate(spec: &SandboxSpec) -> Result<(), SandboxError> {
    let temp_parent = canonical_or_owned(&std::env::temp_dir());
    validate_root(
        &spec.home_root,
        &[HOME_PREFIX, ZELLIJ_PREFIX],
        &temp_parent,
        "HOME",
    )?;
    if spec.runtime_root == spec.home_root {
        validate_root(
            &spec.runtime_root,
            &[ZELLIJ_PREFIX],
            &temp_parent,
            "runtime",
        )?;
    } else {
        validate_root(
            &spec.runtime_root,
            &[RUNTIME_PREFIX],
            Path::new("/tmp"),
            "runtime",
        )?;
    }
    Ok(())
}

fn validate_root(
    root: &Path,
    prefixes: &[&str],
    expected_parent: &Path,
    label: &str,
) -> Result<(), SandboxError> {
    let valid = root
        .parent()
        .filter(|parent| *parent == expected_parent)
        .and_then(|_| root.file_name())
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            prefixes.iter().any(|prefix| {
                name.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix.len() == RANDOM_BYTES
                        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            })
        });
    if valid {
        Ok(())
    } else {
        Err(SandboxError::InvalidSpec(format!(
            "{label} root `{}` is not a fixture root directly under `{}`",
            root.display(),
            expected_parent.display()
        )))
    }
}

/// Tear down graceful mux endpoints, every marker-carrying process, then roots.
pub fn cleanup(spec: &SandboxSpec) {
    if validate(spec).is_err() {
        return;
    }

    let socket = crate::mux::tmux::managed_server_socket_path_under(&spec.runtime_root);
    if socket.exists() {
        let mut command = crate::mux::tmux::tmux_cmd(&socket).to_command();
        scrub_session_env(&mut command);
        command.arg("kill-server");
        reap_bounded(command);
    }

    if spec.runtime_root.join("zellij").exists() {
        let mut command = Command::new("zellij");
        scrub_session_env(&mut command);
        pin_zellij_env(&mut command, spec);
        command.args(["kill-all-sessions", "--yes"]);
        reap_bounded(command);
    }

    reap_sandbox_processes(spec);
    remove_tree_bounded(&spec.home_root);
    if spec.runtime_root != spec.home_root {
        remove_tree_bounded(&spec.runtime_root);
    }
}

fn pin_zellij_env(command: &mut Command, spec: &SandboxSpec) {
    command
        .env("HOME", &spec.home_root)
        .env("XDG_RUNTIME_DIR", &spec.runtime_root);
    if spec.home_root == spec.runtime_root {
        command
            .env("XDG_STATE_HOME", &spec.home_root)
            .env("XDG_CONFIG_HOME", &spec.home_root)
            .env("XDG_CACHE_HOME", &spec.home_root)
            .env("XDG_DATA_HOME", &spec.home_root)
            .env("TMPDIR", &spec.home_root)
            .env("ZELLIJ_CONFIG_DIR", spec.home_root.join(".config/zellij"));
    } else {
        let config = spec.home_root.join("config");
        command
            .env("XDG_STATE_HOME", spec.home_root.join("state"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_CACHE_HOME", spec.home_root.join("cache"))
            .env("XDG_DATA_HOME", spec.home_root.join("data"))
            .env("TMPDIR", spec.home_root.join("tmp"))
            .env("ZELLIJ_CONFIG_DIR", config.join("zellij"));
    }
}

fn scrub_session_env(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("RIMZ_")
            || key_text.starts_with("TMUX")
            || key_text.starts_with("ZELLIJ")
        {
            command.env_remove(key);
        }
    }
}

fn reap_bounded(mut command: Command) {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn reap_sandbox_processes(spec: &SandboxSpec) {
    let pids = sandbox_processes(spec);
    if pids.is_empty() {
        return;
    }
    signal_processes("-TERM", &pids);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        let remaining = sandbox_processes(spec);
        if remaining.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            signal_processes("-KILL", &remaining);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_sandbox_processes(_spec: &SandboxSpec) {
    // Graceful endpoint teardown is portable; arbitrary descendant discovery
    // relies on Linux `/proc/<pid>/environ`.
}

/// Marker-carrying processes currently owned by this sandbox.
#[cfg(target_os = "linux")]
pub fn sandbox_processes(spec: &SandboxSpec) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environment| {
                environment_mentions_root(&environment, &spec.home_root)
                    || environment_mentions_root(&environment, &spec.runtime_root)
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn environment_mentions_root(environment: &[u8], root: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    let root = root.as_os_str().as_bytes();
    environment.split(|byte| *byte == 0).any(|entry| {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            return false;
        };
        let value = &entry[separator + 1..];
        value == root
            || value
                .strip_prefix(root)
                .is_some_and(|suffix| suffix.first() == Some(&b'/'))
    })
}

#[cfg(target_os = "linux")]
fn signal_processes(signal: &str, pids: &[u32]) {
    let mut command = Command::new("kill");
    command.arg(signal).arg("--");
    command.args(pids.iter().map(u32::to_string));
    reap_bounded(command);
}

fn remove_tree_bounded(root: &Path) {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        match std::fs::remove_dir_all(root) {
            Ok(()) => return,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(_) if Instant::now() >= deadline => return,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_marker_requires_a_path_boundary() {
        let root = Path::new("/tmp/rrABC123");
        assert!(environment_mentions_root(b"HOME=/tmp/rrABC123\0", root));
        assert!(environment_mentions_root(
            b"XDG_RUNTIME_DIR=/tmp/rrABC123/rimz\0",
            root
        ));
        assert!(!environment_mentions_root(
            b"HOME=/tmp/rrABC123-other\0",
            root
        ));
    }

    #[test]
    fn validation_rejects_unshaped_roots() {
        let spec = SandboxSpec {
            home_root: PathBuf::from("/tmp"),
            runtime_root: PathBuf::from("/tmp"),
        };
        assert!(matches!(validate(&spec), Err(SandboxError::InvalidSpec(_))));
    }
}
