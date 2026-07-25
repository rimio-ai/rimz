//! Disposable host-state and multiplexer roots for tests and manual smoke runs.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const REAPER_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const SANDBOX_PREFIX: &str = "rimz-sandbox-";
const REAPER_ARG: &str = "__sandbox-reaper";

/// One short-lived filesystem namespace for host-facing test processes.
///
/// XDG roots keep Zellij and RimZ state off the host, while `TMUX_TMPDIR`
/// keeps even a forgotten default `TmuxBackend` away from the user's server.
/// Every sandbox also replaces `HOME`, covering agent configs whose upstream
/// location does not follow XDG.
///
/// An independent keepalive reaper applies the same cleanup when the owner dies before `Drop`.
pub(crate) struct HostSandbox {
    _root: TempDir,
    env: BTreeMap<&'static str, PathBuf>,
    reaper: Option<SandboxReaper>,
}

impl HostSandbox {
    pub(crate) fn for_tests(workspace_root: &Path) -> Result<Self> {
        let mut sandbox = Self::new()?;
        sandbox.trust_workspace_for_git(workspace_root)?;
        let tee_dir = workspace_root.join("target").join("xtask").join("rtk");
        std::fs::create_dir_all(&tee_dir).with_context(|| {
            format!("creating retained rtk log directory {}", tee_dir.display())
        })?;
        sandbox.env.insert("RTK_TEE_DIR", tee_dir);
        Ok(sandbox)
    }

    fn for_manual_command() -> Result<Self> {
        Self::new()
    }

    fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix(SANDBOX_PREFIX)
            .tempdir_in("/tmp")
            .context("creating short test sandbox")?;
        let env = sandbox_env(root.path());
        for path in env.values() {
            std::fs::create_dir_all(path)
                .with_context(|| format!("creating sandbox directory {}", path.display()))?;
        }
        std::fs::write(
            env["ZELLIJ_CONFIG_DIR"].join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\n",
        )
        .context("writing sandbox Zellij config")?;
        std::fs::write(env["HOME"].join(".zshrc"), "").context("writing sandbox zsh config")?;
        let reaper = SandboxReaper::spawn(root.path())?;
        Ok(Self {
            _root: root,
            env,
            reaper,
        })
    }

    fn trust_workspace_for_git(&self, workspace_root: &Path) -> Result<()> {
        let config = self.env["HOME"].join(".gitconfig");
        let output = Command::new("git")
            .arg("config")
            .arg("--file")
            .arg(&config)
            .args(["--add", "safe.directory"])
            .arg(workspace_root)
            .output()
            .context("creating sandbox Git configuration")?;
        if !output.status.success() {
            bail!(
                "creating sandbox Git configuration failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub(crate) fn command_env(&self) -> Vec<(&'static str, PathBuf)> {
        self.env
            .iter()
            .map(|(key, value)| (*key, value.clone()))
            .collect()
    }

    fn apply_to(&self, command: &mut Command, scrub_session: bool) {
        apply_env(command, &self.env, scrub_session);
    }

    #[cfg(test)]
    fn root(&self) -> &Path {
        self._root.path()
    }
}

fn sandbox_env(root: &Path) -> BTreeMap<&'static str, PathBuf> {
    BTreeMap::from([
        ("HOME", root.join("home")),
        ("TMUX_TMPDIR", root.join("tmux")),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_RUNTIME_DIR", root.join("runtime")),
        ("XDG_STATE_HOME", root.join("state")),
        ("TMPDIR", root.join("tmp")),
        ("ZELLIJ_CONFIG_DIR", root.join("config").join("zellij")),
    ])
}

impl Drop for HostSandbox {
    fn drop(&mut self) {
        if self.reaper.take().is_some_and(SandboxReaper::finish) {
            return;
        }
        cleanup_sandbox(self._root.path(), &self.env);
    }
}

struct SandboxReaper {
    child: Child,
    keepalive: ChildStdin,
}

impl SandboxReaper {
    #[cfg(not(test))]
    fn spawn(root: &Path) -> Result<Option<Self>> {
        let executable = std::env::current_exe().context("resolving xtask reaper executable")?;
        let mut child = Command::new(executable)
            .arg(REAPER_ARG)
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting sandbox cleanup reaper")?;
        let keepalive = child
            .stdin
            .take()
            .context("sandbox cleanup reaper has no keepalive pipe")?;
        Ok(Some(Self { child, keepalive }))
    }

    #[cfg(test)]
    fn spawn(root: &Path) -> Result<Option<Self>> {
        validate_sandbox_root(root)?;
        Ok(None)
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

pub(crate) fn run_reaper_mode(args: &[String]) -> Result<bool> {
    if args.first().is_none_or(|arg| arg != REAPER_ARG) {
        return Ok(false);
    }
    let [_, root] = args else {
        bail!("{REAPER_ARG} requires exactly one sandbox root");
    };
    let root = Path::new(root);
    validate_sandbox_root(root)?;
    let mut buffer = [0_u8; 64];
    let mut input = std::io::stdin().lock();
    while input
        .read(&mut buffer)
        .context("reading reaper keepalive")?
        != 0
    {}
    cleanup_sandbox(root, &sandbox_env(root));
    Ok(true)
}

fn validate_sandbox_root(root: &Path) -> Result<()> {
    let suffix = root
        .parent()
        .filter(|parent| *parent == Path::new("/tmp"))
        .and_then(|_| root.file_name())
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix(SANDBOX_PREFIX))
        .filter(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    if suffix.is_none() {
        bail!(
            "sandbox reaper refuses root outside /tmp/{SANDBOX_PREFIX}<six alphanumeric characters>"
        );
    }
    Ok(())
}

fn apply_env(command: &mut Command, env: &BTreeMap<&'static str, PathBuf>, scrub_session: bool) {
    if scrub_session {
        for (key, _) in std::env::vars_os() {
            if session_key(&key) {
                command.env_remove(key);
            }
        }
    }
    command.envs(env);
}

fn cleanup_sandbox(root: &Path, env: &BTreeMap<&'static str, PathBuf>) {
    let tmux_started =
        std::fs::read_dir(&env["TMUX_TMPDIR"]).is_ok_and(|mut entries| entries.next().is_some());
    if tmux_started {
        let mut command = Command::new("tmux");
        apply_env(&mut command, env, true);
        command.arg("kill-server");
        reap_bounded(command);
    }

    if env["XDG_RUNTIME_DIR"].join("zellij").exists() {
        let mut command = Command::new("zellij");
        apply_env(&mut command, env, true);
        command.args(["kill-all-sessions", "--yes"]);
        reap_bounded(command);
    }

    reap_sandbox_processes(root);
    remove_tree_bounded(root);
}

fn session_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("RIMZ_") || key.starts_with("TMUX") || key.starts_with("ZELLIJ")
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
fn reap_sandbox_processes(root: &Path) {
    let pids = sandbox_processes(root);
    if pids.is_empty() {
        return;
    }
    signal_processes("-TERM", &pids);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        let remaining = sandbox_processes(root);
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
fn reap_sandbox_processes(_root: &Path) {
    // Explicit tmux and Zellij cleanup remains cross-platform; sweeping an
    // arbitrary leaked child by its environment relies on Linux `/proc`.
}

#[cfg(target_os = "linux")]
fn sandbox_processes(root: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|environment| environment_mentions_root(&environment, root))
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
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if Instant::now() >= deadline => return,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Run an arbitrary contributor command with disposable HOME, XDG, tmux, and
/// Zellij roots. The command inherits terminal I/O and runs from the workspace
/// root, so it is suitable for interactive `target/debug/rimz` smoke runs.
pub(crate) fn run(root: &Path, args: &[String]) -> Result<()> {
    let args = if args.first().is_some_and(|arg| arg == "--") {
        &args[1..]
    } else {
        args
    };
    let Some((program, program_args)) = args.split_first() else {
        bail!(
            "sandbox requires a command; for example: cargo xtask sandbox -- target/debug/rimz --zellij doctor"
        );
    };
    let sandbox = HostSandbox::for_manual_command()?;
    let program_path = Path::new(program);
    let resolved_program = if program_path.is_relative() && program_path.components().count() > 1 {
        root.join(program_path)
    } else {
        program_path.to_path_buf()
    };
    let mut command = Command::new(&resolved_program);
    command.args(program_args).current_dir(root);
    sandbox.apply_to(&mut command, true);
    let status = command
        .status()
        .with_context(|| format!("running sandboxed command `{}`", resolved_program.display()))?;
    if !status.success() {
        bail!("sandboxed command `{program}` exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_replaces_home_and_pins_both_muxes() {
        let workspace = TempDir::new().unwrap();
        let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
        for key in [
            "HOME",
            "TMUX_TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "ZELLIJ_CONFIG_DIR",
        ] {
            assert!(sandbox.env[key].starts_with(sandbox.root()), "{key}");
        }
        assert_eq!(
            sandbox.env["RTK_TEE_DIR"],
            workspace.path().join("target/xtask/rtk"),
        );
        let trusted = Command::new("git")
            .arg("config")
            .arg("--file")
            .arg(sandbox.env["HOME"].join(".gitconfig"))
            .args(["--get-all", "safe.directory"])
            .output()
            .unwrap();
        assert!(trusted.status.success());
        assert_eq!(
            String::from_utf8(trusted.stdout).unwrap().trim(),
            workspace.path().to_string_lossy(),
        );
    }

    #[test]
    fn manual_sandbox_replaces_home_and_every_persistent_xdg_root() {
        let sandbox = HostSandbox::for_manual_command().unwrap();
        for key in [
            "HOME",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "XDG_STATE_HOME",
        ] {
            assert!(sandbox.env[key].starts_with(sandbox.root()), "{key}");
        }
    }

    #[test]
    fn session_key_covers_identity_and_mux_routing() {
        for key in ["RIMZ_WORKSPACE_ID", "TMUX", "TMUX_TMPDIR", "ZELLIJ_PANE_ID"] {
            assert!(session_key(OsStr::new(key)), "{key}");
        }
        assert!(!session_key(OsStr::new("PATH")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_process_match_requires_a_path_boundary() {
        let environment = b"HOME=/tmp/rimz-sandbox-a/home\0PATH=/usr/bin\0";
        assert!(environment_mentions_root(
            environment,
            Path::new("/tmp/rimz-sandbox-a")
        ));
        assert!(!environment_mentions_root(
            environment,
            Path::new("/tmp/rimz-sandbox")
        ));
    }

    #[test]
    fn reaper_accepts_only_generated_sandbox_roots() {
        assert!(validate_sandbox_root(Path::new("/tmp/rimz-sandbox-aB123z")).is_ok());
        for root in [
            "/tmp/rimz-sandbox-short",
            "/tmp/rimz-sandbox-aB_23z",
            "/var/tmp/rimz-sandbox-aB123z",
            "/tmp/other-aB123z",
            "/tmp/rimz-sandbox-aB123z/child",
        ] {
            assert!(validate_sandbox_root(Path::new(root)).is_err(), "{root}");
        }
    }
}
