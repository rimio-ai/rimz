//! Shared ttyd daemon for browser access to every RimZ room.

use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{CredentialSummary, Result, WebCredential, WebErr, WebWarning};

mod client;

use client::ClientProfile;

const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const DAEMON_FILE: &str = "web-ttyd.json";
const DAEMON_LOCK_FILE: &str = "web-ttyd.lock";
const LEGACY_INSTANCE_DIR: &str = "web-ttyd";
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TtydCredential {
    name: String,
    created_at: Timestamp,
    secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DaemonRecord {
    pub(super) pid: u32,
    pub(super) port: u16,
}

pub(super) struct RunningDaemon {
    pub(super) pid: u32,
    pub(super) port: u16,
    pub(super) credential: WebCredential,
    pub(super) warnings: Vec<WebWarning>,
}

pub(super) struct DaemonInspection {
    pub(super) port: u16,
    pub(super) credential: Option<WebCredential>,
}

pub(super) struct CredentialRotation {
    pub(super) credential: WebCredential,
    pub(super) restarted: bool,
    pub(super) warnings: Vec<WebWarning>,
}

#[derive(Debug, Deserialize)]
struct LegacyTtydInstance {
    session: String,
    pid: u32,
    port: u16,
}

pub(super) fn preflight() -> Result<()> {
    program().map(|_| ())
}

pub(super) fn program() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TTYD_BIN_ENV) {
        return Ok(PathBuf::from(path));
    }
    which::which("ttyd").map_err(|_| WebErr::MissingTtyd)
}

pub(super) fn version_at(program: &Path) -> Result<String> {
    let output = std::process::Command::new(program)
        .arg("--version")
        .output()
        .map_err(|source| WebErr::Io {
            path: program.to_path_buf(),
            source,
        })?;
    let text = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    Ok(String::from_utf8_lossy(text).trim().to_owned())
}

pub(super) fn open_daemon(config: &MachineConfig, may_start: bool) -> Result<RunningDaemon> {
    if may_start {
        ensure_daemon(config)
    } else {
        let _guard = acquire_daemon_lock()?;
        let Some(record) = daemon_status_locked()? else {
            return Err(WebErr::TtydOffline);
        };
        let Some(credential) = read_credential()? else {
            return Err(WebErr::TtydCredentialMissing);
        };
        Ok(running_daemon(record, credential, Vec::new()))
    }
}

pub(super) fn inspect_daemon(config: &MachineConfig) -> Result<DaemonInspection> {
    let _guard = acquire_daemon_lock()?;
    let daemon = daemon_status_locked()?;
    let port = daemon.map_or(config.web.port, |record| record.port);
    let credential = read_credential()?.map(|credential| basic_auth(&credential));
    Ok(DaemonInspection { port, credential })
}

pub(super) fn credential_summary() -> Result<Option<CredentialSummary>> {
    let _guard = acquire_daemon_lock()?;
    Ok(read_credential()?.map(|credential| CredentialSummary {
        name: credential.name,
        created_at: credential.created_at,
    }))
}

fn basic_auth(credential: &TtydCredential) -> WebCredential {
    WebCredential {
        username: credential.name.clone(),
        secret: credential.secret.clone(),
    }
}

fn mint_credential() -> Result<TtydCredential> {
    let record = TtydCredential {
        name: "rimz".to_owned(),
        created_at: Timestamp::now(),
        secret: random_secret(),
    };
    write_credential_at(&credential_path(), &record)?;
    Ok(record)
}

fn read_credential() -> Result<Option<TtydCredential>> {
    read_json_optional(&credential_path())
}

fn ensure_credential() -> Result<TtydCredential> {
    read_credential()?.map_or_else(mint_credential, Ok)
}

fn clear_credential() -> Result<bool> {
    remove_optional(&credential_path())
}

pub(super) fn ensure_daemon(config: &MachineConfig) -> Result<RunningDaemon> {
    let _guard = acquire_daemon_lock()?;
    reap_legacy_instances();
    let daemon = daemon_status_locked()?;
    if let Some(record) = &daemon
        && record.port == config.web.port
        && let Some(credential) = read_credential()?
    {
        return Ok(running_daemon(record.clone(), credential, Vec::new()));
    }
    let program = program()?;
    if daemon
        .as_ref()
        .is_none_or(|record| record.port != config.web.port)
    {
        ensure_port_available(config.web.port)?;
    }
    if let Some(record) = daemon {
        stop_record(&record)?;
    }
    let credential = ensure_credential()?;
    let profile = client::profile(config, &program);
    let record = start_daemon_with_profile(&program, config.web.port, &credential, &profile)?;
    Ok(running_daemon(record, credential, profile.warnings))
}

fn running_daemon(
    record: DaemonRecord,
    credential: TtydCredential,
    warnings: Vec<WebWarning>,
) -> RunningDaemon {
    RunningDaemon {
        pid: record.pid,
        port: record.port,
        credential: basic_auth(&credential),
        warnings,
    }
}

fn reap_legacy_instances() {
    let dir = legacy_instance_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!(path = %dir.display(), error = %err, "legacy ttyd state directory unreadable");
            remove_legacy_instance_dir(&dir);
            return;
        }
    };
    let processes = crate::proc::list_processes();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(path = %dir.display(), error = %err, "legacy ttyd state entry unreadable");
                continue;
            }
        };
        let path = entry.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "legacy ttyd state record unreadable");
                continue;
            }
        };
        let instance = match serde_json::from_slice::<LegacyTtydInstance>(&bytes) {
            Ok(instance) => instance,
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "legacy ttyd state record invalid");
                continue;
            }
        };
        let owned = processes
            .iter()
            .any(|process| process.pid == instance.pid && is_ttyd_process(process));
        if owned {
            terminate_legacy_instance(&instance);
        } else {
            tracing::debug!(
                session = %instance.session,
                pid = instance.pid,
                port = instance.port,
                "legacy ttyd process absent or pid belongs to another program"
            );
        }
    }
    remove_legacy_instance_dir(&dir);
}

fn is_ttyd_process(process: &crate::proc::ProcInfo) -> bool {
    crate::proc::command::program_label(&process.cmdline) == "ttyd"
}

fn terminate_legacy_instance(instance: &LegacyTtydInstance) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        let result = i32::try_from(instance.pid)
            .map(Pid::from_raw)
            .map_err(|err| err.to_string())
            .and_then(|pid| kill(pid, Signal::SIGTERM).map_err(|err| err.to_string()));
        match result {
            Ok(()) => {
                wait_for_port_close(instance.port, Duration::from_secs(1));
                tracing::debug!(
                    session = %instance.session,
                    pid = instance.pid,
                    port = instance.port,
                    "signalled legacy ttyd process"
                );
            }
            Err(err) => tracing::debug!(
                session = %instance.session,
                pid = instance.pid,
                port = instance.port,
                error = %err,
                "signalling legacy ttyd process failed"
            ),
        }
    }
}

fn remove_legacy_instance_dir(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => tracing::debug!(path = %dir.display(), "removed legacy ttyd state directory"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(path = %dir.display(), error = %err, "removing legacy ttyd state directory failed")
        }
    }
}

fn start_daemon_with_profile(
    program: &Path,
    port: u16,
    credential: &TtydCredential,
    profile: &ClientProfile,
) -> Result<DaemonRecord> {
    let spec = spawn_spec(program, port, &credential.secret, &profile.args)?;
    let pid = spawn_detached(spec)?;
    let record = DaemonRecord { pid, port };
    if !wait_for_port(port, START_TIMEOUT) {
        let _ = stop_record(&record);
        return Err(WebErr::TtydStartTimeout { port });
    }
    write_daemon(&record)?;
    Ok(record)
}

pub(super) fn rotate_credential(config: &MachineConfig) -> Result<CredentialRotation> {
    let _guard = acquire_daemon_lock()?;
    rotate_credential_locked(config)
}

fn rotate_credential_locked(config: &MachineConfig) -> Result<CredentialRotation> {
    let Some(daemon) = daemon_status_locked()? else {
        return Ok(CredentialRotation {
            credential: basic_auth(&mint_credential()?),
            restarted: false,
            warnings: Vec::new(),
        });
    };
    if daemon.port != config.web.port {
        ensure_port_available(config.web.port)?;
    }
    let program = program()?;
    let profile = client::profile(config, &program);
    let credential = mint_credential()?;
    stop_record(&daemon)?;
    start_daemon_with_profile(&program, config.web.port, &credential, &profile)?;
    Ok(CredentialRotation {
        credential: basic_auth(&credential),
        restarted: true,
        warnings: profile.warnings,
    })
}

pub(super) fn revoke_credential() -> Result<bool> {
    let _guard = acquire_daemon_lock()?;
    let stopped = stop_daemon_locked()?;
    clear_credential()?;
    Ok(stopped)
}

pub(super) fn daemon_status() -> Result<Option<DaemonRecord>> {
    let _guard = acquire_daemon_lock()?;
    daemon_status_locked()
}

fn daemon_status_locked() -> Result<Option<DaemonRecord>> {
    let path = daemon_path();
    let Some(record) = read_json_optional::<DaemonRecord>(&path)? else {
        return Ok(None);
    };
    let processes = crate::proc::list_processes();
    if processes
        .iter()
        .any(|process| process.pid == record.pid && is_ttyd_process(process))
        && TcpStream::connect(("127.0.0.1", record.port)).is_ok()
    {
        return Ok(Some(record));
    }
    remove_optional(&path)?;
    Ok(None)
}

pub(super) fn stop_daemon() -> Result<bool> {
    let _guard = acquire_daemon_lock()?;
    stop_daemon_locked()
}

fn stop_daemon_locked() -> Result<bool> {
    let Some(record) = daemon_status_locked()? else {
        return Ok(false);
    };
    stop_record(&record)?;
    Ok(true)
}

fn stop_record(record: &DaemonRecord) -> Result<()> {
    terminate_record(record);
    remove_optional(&daemon_path())?;
    Ok(())
}

fn terminate_record(record: &DaemonRecord) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        let mut survivors = Vec::new();
        if let Ok(raw) = i32::try_from(record.pid) {
            let _ = kill(Pid::from_raw(raw), Signal::SIGTERM);
            survivors.push(record.pid);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while !survivors.is_empty() && Instant::now() < deadline {
            let processes = crate::proc::list_processes();
            survivors.retain(|pid| processes.iter().any(|process| process.pid == *pid));
            if !survivors.is_empty() {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        for pid in survivors {
            if let Ok(raw) = i32::try_from(pid) {
                let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
            }
        }
        wait_for_port_close(record.port, Duration::from_secs(1));
    }
}

fn spawn_spec(
    program: &Path,
    port: u16,
    secret: &str,
    extra_args: &[String],
) -> Result<CommandSpec> {
    Ok(spawn_spec_for(
        program,
        &std::env::current_exe().map_err(|source| WebErr::Io {
            path: PathBuf::from("/proc/self/exe"),
            source,
        })?,
        port,
        secret,
        extra_args,
    ))
}

fn spawn_spec_for(
    program: &Path,
    rimz_exe: &Path,
    port: u16,
    secret: &str,
    extra_args: &[String],
) -> CommandSpec {
    CommandSpec::new(program.display().to_string())
        .args(["-W", "-O", "-a", "-c"])
        .arg(format!("rimz:{secret}"))
        .args(["-i", "127.0.0.1", "-p"])
        .arg(port.to_string())
        .args(extra_args.iter().cloned())
        .arg(rimz_exe.display().to_string())
        .args(["web", "exec"])
}

fn spawn_detached(spec: CommandSpec) -> Result<u32> {
    let mut command = spec.to_command();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    crate::child_process::spawn_detached_reaped(&mut command, "ttyd-web").map_err(|source| {
        WebErr::Io {
            path: PathBuf::from(&spec.program),
            source,
        }
    })
}

fn ensure_port_available(port: u16) -> Result<()> {
    TcpListener::bind(("127.0.0.1", port))
        .map(|_| ())
        .map_err(|_| WebErr::ConfiguredPortInUse { port })
}

fn choose_ephemeral_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.local_addr().map(|address| address.port())
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
fn wait_for_port_close(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && TcpStream::connect(("127.0.0.1", port)).is_ok() {
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn credential_path() -> PathBuf {
    paths::state_home().join("rimz").join(CREDENTIAL_FILE)
}

fn daemon_path() -> PathBuf {
    paths::state_home().join("rimz").join(DAEMON_FILE)
}

fn legacy_instance_dir() -> PathBuf {
    paths::state_home().join("rimz").join(LEGACY_INSTANCE_DIR)
}

fn acquire_daemon_lock() -> Result<crate::store::lock::WorkspaceLock> {
    let path = paths::state_home().join("rimz").join(DAEMON_LOCK_FILE);
    Ok(crate::store::lock::WorkspaceLock::acquire(&path)?)
}

fn random_secret() -> String {
    let first = uuid::Uuid::now_v7().simple().to_string();
    let second = uuid::Uuid::now_v7().simple().to_string();
    format!("{}{}", &first[12..], &second[12..])
        .chars()
        .take(24)
        .collect()
}

fn write_credential_at(path: &Path, credential: &TtydCredential) -> Result<()> {
    atomic::write_private_temp_then_rename(path, credential)?;
    Ok(())
}

fn write_daemon(record: &DaemonRecord) -> Result<()> {
    write_daemon_at(&daemon_path(), record)
}

fn write_daemon_at(path: &Path, record: &DaemonRecord) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, record)?;
    Ok(())
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(WebErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| WebErr::TtydJson {
            path: path.to_path_buf(),
            source,
        })
}

fn remove_optional(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(WebErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn argv_uses_loopback_auth_url_args_and_rimz_shim() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            8201,
            "secret",
            &["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
        );
        assert_eq!(
            spec.args,
            [
                "-W",
                "-O",
                "-a",
                "-c",
                "rimz:secret",
                "-i",
                "127.0.0.1",
                "-p",
                "8201",
                "-t",
                "macOptionIsMeta=true",
                "/opt/rimz/bin/rimz",
                "web",
                "exec"
            ]
        );
    }

    #[test]
    fn styled_argv_keeps_all_client_options_before_rimz_shim() {
        let extra = vec![
            "-t".to_owned(),
            "macOptionIsMeta=true".to_owned(),
            "-t".to_owned(),
            "fontFamily=RimZ Font,monospace".to_owned(),
            "-t".to_owned(),
            "theme={\"background\":\"#010203\"}".to_owned(),
            "-I".to_owned(),
            "/cache/index.html".to_owned(),
        ];
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            8202,
            "secret",
            &extra,
        );

        let shim = spec
            .args
            .iter()
            .position(|arg| arg == "/opt/rimz/bin/rimz")
            .expect("shim argv");
        assert_eq!(&spec.args[shim - extra.len()..shim], extra);
    }

    #[test]
    fn ephemeral_port_is_bindable() {
        let port = choose_ephemeral_port().expect("ephemeral port");
        TcpListener::bind(("127.0.0.1", port)).expect("port released after selection");
    }

    #[test]
    fn credential_roundtrip_is_private() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("credential.json");
        let credential = TtydCredential {
            name: "rimz".to_owned(),
            created_at: Timestamp::from_second(1_700_000_000).expect("timestamp"),
            secret: "secret".to_owned(),
        };
        write_credential_at(&path, &credential).expect("write");
        assert_eq!(read_json_optional(&path).expect("read"), Some(credential));
        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn daemon_state_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.json");
        let daemon = DaemonRecord {
            pid: u32::MAX,
            port: 8200,
        };
        write_daemon_at(&path, &daemon).expect("write daemon state");
        assert_eq!(read_json_optional(&path).expect("read state"), Some(daemon));
    }

    #[test]
    fn legacy_pid_guard_requires_ttyd_as_the_program() {
        let process = |cmdline: &str| crate::proc::ProcInfo {
            pid: 42,
            ppid: 1,
            real_uid: 1000,
            cmdline: cmdline.to_owned(),
        };
        assert!(is_ttyd_process(&process("/usr/bin/ttyd -p 8200 sh")));
        assert!(!is_ttyd_process(&process("/usr/bin/ttyd-trace -p 8200")));
        assert!(!is_ttyd_process(&process("sh -c ttyd -p 8200")));
    }
}
