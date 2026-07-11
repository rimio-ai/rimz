//! ttyd-backed browser access for tmux rooms.

use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{TtydStatusInstance, derive_port, port_scan};

pub const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
pub const TTYD_PORT_RANGE: RangeInclusive<u16> = 8200..=8299;
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const INSTANCE_DIR: &str = "web-ttyd";
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum TtydErr {
    #[error(
        "ttyd is required for tmux browser access; install it with `brew install ttyd` or `apt install ttyd`"
    )]
    MissingBinary,
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse ttyd state at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("ttyd is not serving tmux session `{0}`; run `rimz web open` or omit `--no-start`")]
    Offline(String),
    #[error(
        "ttyd for tmux session `{session}` did not accept connections on 127.0.0.1:{port} within 5 seconds"
    )]
    StartTimeout { session: String, port: u16 },
    #[error("no free ttyd port in 8200..8299")]
    NoFreePort,
}

pub type Result<T> = std::result::Result<T, TtydErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtydCredential {
    pub name: String,
    pub created_at: Timestamp,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtydInstance {
    pub session: String,
    pub pid: u32,
    pub port: u16,
    pub started_at: Timestamp,
}

pub fn ttyd_program() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TTYD_BIN_ENV) {
        return Ok(PathBuf::from(path));
    }
    which::which("ttyd").map_err(|_| TtydErr::MissingBinary)
}

pub fn version() -> Result<String> {
    let program = ttyd_program()?;
    let output = std::process::Command::new(&program)
        .arg("--version")
        .output()
        .map_err(|source| TtydErr::Io {
            path: program.clone(),
            source,
        })?;
    let text = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    Ok(String::from_utf8_lossy(text).trim().to_owned())
}

pub fn spawn_spec(session: &str, port: u16, secret: &str) -> Result<CommandSpec> {
    Ok(spawn_spec_for(&ttyd_program()?, session, port, secret))
}

fn spawn_spec_for(program: &Path, session: &str, port: u16, secret: &str) -> CommandSpec {
    CommandSpec::new(program.display().to_string())
        .args(["-W", "-O", "-c"])
        .arg(format!("rimz:{secret}"))
        .args(["-i", "127.0.0.1", "-p"])
        .arg(port.to_string())
        .args(["-b"])
        .arg(format!("/{session}"))
        .args(["tmux", "attach", "-t"])
        .arg(session.to_owned())
}

pub fn derive_instance_port(session: &str) -> u16 {
    derive_port(session, &TTYD_PORT_RANGE)
}

pub fn mint_credential() -> Result<TtydCredential> {
    let record = TtydCredential {
        name: "rimz".to_owned(),
        created_at: Timestamp::now(),
        secret: random_secret(),
    };
    write_credential_at(&credential_path(), &record)?;
    Ok(record)
}

pub fn read_credential() -> Result<Option<TtydCredential>> {
    read_json_optional(&credential_path())
}

pub fn ensure_credential() -> Result<TtydCredential> {
    read_credential()?.map_or_else(mint_credential, Ok)
}

pub fn clear_credential() -> Result<bool> {
    remove_optional(&credential_path())
}

pub fn live_instance(session: &str) -> Result<Option<TtydInstance>> {
    let path = instance_path(session);
    let Some(instance) = read_json_optional::<TtydInstance>(&path)? else {
        return Ok(None);
    };
    if instance_live(&instance) {
        Ok(Some(instance))
    } else {
        let _ = fs::remove_file(path);
        Ok(None)
    }
}

pub fn live_instances() -> Result<Vec<TtydInstance>> {
    let dir = instance_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(TtydErr::Io { path: dir, source }),
    };
    let mut live = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| TtydErr::Io {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(instance) = read_json_optional::<TtydInstance>(&path)? else {
            continue;
        };
        if instance_live(&instance) {
            live.push(instance);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    live.sort_by(|a, b| a.session.cmp(&b.session));
    Ok(live)
}

pub fn status_instances() -> Result<Vec<TtydStatusInstance>> {
    Ok(live_instances()?
        .into_iter()
        .map(|instance| TtydStatusInstance {
            session: instance.session,
            pid: instance.pid,
            port: instance.port,
        })
        .collect())
}

pub fn ensure_instance(session: &str, may_start: bool) -> Result<(TtydInstance, TtydCredential)> {
    let credential = ensure_credential()?;
    if let Some(instance) = live_instance(session)? {
        return Ok((instance, credential));
    }
    if !may_start {
        return Err(TtydErr::Offline(session.to_owned()));
    }
    let port = choose_instance_port(session)?;
    let spec = spawn_spec(session, port, &credential.secret)?;
    let pid = spawn_detached(spec)?;
    if !wait_for_port(port, START_TIMEOUT) {
        terminate_pid(pid);
        return Err(TtydErr::StartTimeout {
            session: session.to_owned(),
            port,
        });
    }
    let instance = TtydInstance {
        session: session.to_owned(),
        pid,
        port,
        started_at: Timestamp::now(),
    };
    write_instance(&instance)?;
    Ok((instance, credential))
}

pub fn stop_all() -> Result<usize> {
    let instances = live_instances()?;
    for instance in &instances {
        terminate_pid(instance.pid);
        let _ = fs::remove_file(instance_path(&instance.session));
    }
    Ok(instances.len())
}

pub fn restart_all() -> Result<usize> {
    let sessions = live_instances()?
        .into_iter()
        .map(|instance| instance.session)
        .collect::<Vec<_>>();
    stop_all()?;
    for session in &sessions {
        ensure_instance(session, true)?;
    }
    Ok(sessions.len())
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
        TtydErr::Io {
            path: PathBuf::from(&spec.program),
            source,
        }
    })
}

fn choose_instance_port(session: &str) -> Result<u16> {
    let preferred = derive_instance_port(session);
    for port in port_scan(preferred, &TTYD_PORT_RANGE) {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(TtydErr::NoFreePort)
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

fn instance_live(instance: &TtydInstance) -> bool {
    crate::proc::list_processes()
        .iter()
        .any(|process| process.pid == instance.pid)
        && TcpStream::connect(("127.0.0.1", instance.port)).is_ok()
}

fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    if let Ok(raw) = i32::try_from(pid) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(raw), Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if !crate::proc::list_processes()
                .iter()
                .any(|process| process.pid == pid)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
    }
}

fn credential_path() -> PathBuf {
    paths::state_home().join("rimz").join(CREDENTIAL_FILE)
}

fn instance_dir() -> PathBuf {
    paths::state_home().join("rimz").join(INSTANCE_DIR)
}

fn instance_path(session: &str) -> PathBuf {
    let encoded = session
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    instance_dir().join(format!("{encoded}.json"))
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| TtydErr::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    atomic::write_private_temp_then_rename(path, credential)?;
    Ok(())
}

fn write_instance(instance: &TtydInstance) -> Result<()> {
    let path = instance_path(&instance.session);
    write_instance_at(&path, instance)
}

fn write_instance_at(path: &Path, instance: &TtydInstance) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| TtydErr::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    atomic::write_temp_then_rename_cache(path, instance)?;
    Ok(())
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(TtydErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| TtydErr::Json {
            path: path.to_path_buf(),
            source,
        })
}

fn remove_optional(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(TtydErr::Io {
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
    fn argv_uses_loopback_auth_origin_and_tmux_attach() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            "rimz-project-a1b2c3",
            8201,
            "secret",
        );
        assert_eq!(
            spec.args,
            [
                "-W",
                "-O",
                "-c",
                "rimz:secret",
                "-i",
                "127.0.0.1",
                "-p",
                "8201",
                "-b",
                "/rimz-project-a1b2c3",
                "tmux",
                "attach",
                "-t",
                "rimz-project-a1b2c3"
            ]
        );
    }

    #[test]
    fn port_derivation_is_stable_and_in_range() {
        let first = derive_instance_port("rimz-project-a1b2c3");
        assert_eq!(first, derive_instance_port("rimz-project-a1b2c3"));
        assert!(TTYD_PORT_RANGE.contains(&first));
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
    fn instance_state_roundtrips_and_stale_pid_is_not_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("instance.json");
        let instance = TtydInstance {
            session: "rimz-project-a1b2c3".to_owned(),
            pid: u32::MAX,
            port: 8299,
            started_at: Timestamp::from_second(1_700_000_000).expect("timestamp"),
        };
        write_instance_at(&path, &instance).expect("write state");
        assert_eq!(
            read_json_optional(&path).expect("read state"),
            Some(instance.clone())
        );
        assert!(!instance_live(&instance));
    }
}
