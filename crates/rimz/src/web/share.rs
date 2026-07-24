//! Durable room allowlist and unauthenticated read-only ttyd daemon.

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{Result, WebErr, WebWarning, ttyd};

const ALLOWLIST_FILE: &str = "web-share.json";
const DAEMON_FILE: &str = "web-ttyd-share.json";
const DAEMON_LOCK_FILE: &str = "web-ttyd-share.lock";
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Allowlist {
    #[serde(default)]
    sessions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DaemonRecord {
    pub(super) pid: u32,
    pub(super) port: u16,
    #[serde(default = "default_interface")]
    pub(super) interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pixel_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) index_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DaemonSpec {
    port: u16,
    interface: String,
}

pub(super) struct RunningDaemon {
    pub(super) record: DaemonRecord,
    pub(super) warnings: Vec<WebWarning>,
}

pub(super) struct ShareMutation {
    pub(super) changed: bool,
    pub(super) sessions: Vec<String>,
    pub(super) daemon: Option<RunningDaemon>,
}

pub(super) fn add_session(session: &str, config: &MachineConfig) -> Result<(RunningDaemon, bool)> {
    let desired = desired_spec(config)?;
    let _guard = acquire_lock()?;
    let mut allowlist = read_allowlist()?;
    let previous = allowlist.clone();
    let changed = !allowlist.sessions.iter().any(|shared| shared == session);
    if changed {
        allowlist.sessions.push(session.to_owned());
        normalize(&mut allowlist.sessions);
        write_allowlist(&allowlist)?;
    }
    match ensure_daemon_locked(config, &desired) {
        Ok(daemon) => Ok((daemon, changed)),
        Err(err) => {
            if changed && let Err(rollback) = write_allowlist(&previous) {
                tracing::error!(
                    error = &rollback as &dyn std::error::Error,
                    "could not roll back broadcast allowlist after daemon start failed"
                );
            }
            Err(err)
        }
    }
}

pub(super) fn remove_session(session: &str, config: &MachineConfig) -> Result<ShareMutation> {
    let _guard = acquire_lock()?;
    let mut allowlist = read_allowlist()?;
    let before = allowlist.sessions.len();
    allowlist.sessions.retain(|shared| shared != session);
    if allowlist.sessions.len() == before {
        return Ok(ShareMutation {
            changed: false,
            sessions: allowlist.sessions,
            daemon: None,
        });
    }
    write_allowlist(&allowlist)?;
    let daemon = restart_or_stop_locked(config, &allowlist)?;
    Ok(ShareMutation {
        changed: true,
        sessions: allowlist.sessions,
        daemon,
    })
}

pub(super) fn remove_all(_config: &MachineConfig) -> Result<ShareMutation> {
    let _guard = acquire_lock()?;
    let allowlist = read_allowlist()?;
    let changed = !allowlist.sessions.is_empty();
    if changed {
        write_allowlist(&Allowlist::default())?;
    }
    stop_daemon_locked()?;
    Ok(ShareMutation {
        changed,
        sessions: Vec::new(),
        daemon: None,
    })
}

pub(super) fn sessions() -> Result<Vec<String>> {
    let _guard = acquire_lock()?;
    Ok(read_allowlist()?.sessions)
}

pub(super) fn contains(session: &str) -> Result<bool> {
    let _guard = acquire_lock()?;
    Ok(read_allowlist()?
        .sessions
        .iter()
        .any(|shared| shared == session))
}

pub(super) fn daemon_status() -> Result<Option<DaemonRecord>> {
    let _guard = acquire_lock()?;
    daemon_status_locked()
}

pub(crate) fn pixel_daemon_record() -> Option<(u32, u32)> {
    let bytes = std::fs::read(daemon_path()).ok()?;
    let record = serde_json::from_slice::<DaemonRecord>(&bytes).ok()?;
    record.pixel_protocol.map(|protocol| (record.pid, protocol))
}

pub(super) fn restart_if_shared(config: &MachineConfig) -> Result<Option<RunningDaemon>> {
    let _guard = acquire_lock()?;
    let allowlist = read_allowlist()?;
    if allowlist.sessions.is_empty() {
        stop_daemon_locked()?;
        return Ok(None);
    }
    let desired = desired_spec(config)?;
    restart_daemon_locked(config, &desired).map(Some)
}

pub(super) fn restart_if_online(config: &MachineConfig) -> Result<Option<RunningDaemon>> {
    let _guard = acquire_lock()?;
    if daemon_status_locked()?.is_none() {
        return Ok(None);
    }
    if read_allowlist()?.sessions.is_empty() {
        stop_daemon_locked()?;
        return Ok(None);
    }
    let desired = desired_spec(config)?;
    restart_daemon_locked(config, &desired).map(Some)
}

pub(super) fn stop_daemon() -> Result<bool> {
    let _guard = acquire_lock()?;
    stop_daemon_locked()
}

fn restart_or_stop_locked(
    config: &MachineConfig,
    allowlist: &Allowlist,
) -> Result<Option<RunningDaemon>> {
    if allowlist.sessions.is_empty() {
        stop_daemon_locked()?;
        Ok(None)
    } else {
        // Revocation wins over config or restart failures: disconnect the old
        // process before validating and starting the replacement.
        stop_daemon_locked()?;
        let desired = desired_spec(config)?;
        start_fresh_locked(config, &desired, None).map(Some)
    }
}

fn desired_spec(config: &MachineConfig) -> Result<DaemonSpec> {
    let interface = config
        .web
        .interface
        .parse::<IpAddr>()
        .map_err(|_| WebErr::InvalidInterface {
            value: config.web.interface.clone(),
        })?
        .to_string();
    Ok(DaemonSpec {
        port: config.web.share_port,
        interface,
    })
}

fn ensure_daemon_locked(config: &MachineConfig, desired: &DaemonSpec) -> Result<RunningDaemon> {
    let (program, version) = ttyd::required_program_with_version()?;
    let daemon = daemon_status_locked()?;
    let prepared = prepare_fresh_start(config, desired, daemon.as_ref(), program, &version)?;
    if let Some(record) = daemon.as_ref()
        && record_matches(record, desired, &prepared.2)
    {
        return Ok(running_daemon(
            record.clone(),
            desired,
            prepared.2.warnings.clone(),
        ));
    }
    start_fresh_locked_prepared(desired, daemon, prepared)
}

fn restart_daemon_locked(config: &MachineConfig, desired: &DaemonSpec) -> Result<RunningDaemon> {
    let (program, version) = ttyd::required_program_with_version()?;
    let daemon = daemon_status_locked()?;
    let prepared = prepare_fresh_start(config, desired, daemon.as_ref(), program, &version)?;
    start_fresh_locked_prepared(desired, daemon, prepared)
}

fn start_fresh_locked(
    config: &MachineConfig,
    desired: &DaemonSpec,
    daemon: Option<DaemonRecord>,
) -> Result<RunningDaemon> {
    let (program, version) = ttyd::required_program_with_version()?;
    let prepared = prepare_fresh_start(config, desired, daemon.as_ref(), program, &version)?;
    start_fresh_locked_prepared(desired, daemon, prepared)
}

type FreshStart = (PathBuf, SocketAddr, ttyd::client::ClientProfile);

fn prepare_fresh_start(
    config: &MachineConfig,
    desired: &DaemonSpec,
    daemon: Option<&DaemonRecord>,
    program: PathBuf,
    version: &str,
) -> Result<FreshStart> {
    let address = ttyd::socket_address(&desired.interface, desired.port)?;
    if daemon.is_none_or(|record| {
        ttyd::socket_address(&record.interface, record.port).ok() != Some(address)
    }) {
        ensure_share_port_available(address)?;
    }
    let profile = ttyd::client::profile(config, &program, version);
    Ok((program, address, profile))
}

fn start_fresh_locked_prepared(
    desired: &DaemonSpec,
    daemon: Option<DaemonRecord>,
    (program, address, profile): FreshStart,
) -> Result<RunningDaemon> {
    if let Some(record) = daemon {
        stop_record(&record)?;
    }
    ensure_share_port_available(address)?;
    let interface = desired
        .interface
        .parse::<IpAddr>()
        .map_err(|_| WebErr::InvalidInterface {
            value: desired.interface.clone(),
        })?;
    let spec = spawn_spec(&program, interface, desired.port, &profile.args)?;
    let pid = ttyd::spawn_detached(spec)?;
    let probe = ttyd::probe_address(address);
    if !ttyd::wait_for_address(probe, START_TIMEOUT) {
        ttyd::terminate_pids(&[pid]);
        return Err(WebErr::ShareStartTimeout { address: probe });
    }
    let record = DaemonRecord {
        pid,
        port: desired.port,
        interface: desired.interface.clone(),
        pixel_protocol: profile.pixel_protocol,
        index_key: profile.index_key,
    };
    if let Err(err) = write_daemon(&record) {
        let _ = stop_record(&record);
        return Err(err);
    }
    Ok(running_daemon(record, desired, profile.warnings))
}

fn running_daemon(
    record: DaemonRecord,
    desired: &DaemonSpec,
    mut warnings: Vec<WebWarning>,
) -> RunningDaemon {
    if desired
        .interface
        .parse::<IpAddr>()
        .is_ok_and(|interface| !interface.is_loopback())
    {
        warnings.push(WebWarning::BroadcastUnauthenticated(format!(
            "broadcast is unauthenticated; anyone reaching {}:{} can watch",
            desired.interface, desired.port
        )));
    }
    RunningDaemon { record, warnings }
}

fn record_matches(
    record: &DaemonRecord,
    desired: &DaemonSpec,
    profile: &ttyd::client::ClientProfile,
) -> bool {
    record.port == desired.port
        && record.interface == desired.interface
        && record.index_key == profile.index_key
}

fn daemon_status_locked() -> Result<Option<DaemonRecord>> {
    let path = daemon_path();
    let Some(record) = ttyd::read_json_optional::<DaemonRecord>(&path)? else {
        return Ok(None);
    };
    let processes = crate::proc::list_processes();
    let ttyd_live = processes
        .iter()
        .any(|process| process.pid == record.pid && ttyd::is_ttyd_process(process));
    let listening = ttyd::socket_address(&record.interface, record.port)
        .ok()
        .map(ttyd::probe_address)
        .is_some_and(|address| TcpStream::connect(address).is_ok());
    if ttyd_live && listening {
        return Ok(Some(record));
    }
    if ttyd_live {
        ttyd::terminate_pids(&[record.pid]);
    }
    ttyd::remove_optional(&path)?;
    Ok(None)
}

fn stop_daemon_locked() -> Result<bool> {
    let Some(record) = daemon_status_locked()? else {
        return Ok(false);
    };
    stop_record(&record)?;
    Ok(true)
}

fn stop_record(record: &DaemonRecord) -> Result<()> {
    ttyd::terminate_pids(&[record.pid]);
    #[cfg(unix)]
    if let Ok(address) = ttyd::socket_address(&record.interface, record.port) {
        ttyd::wait_for_address_close(ttyd::probe_address(address), Duration::from_secs(1));
    }
    ttyd::remove_optional(&daemon_path())?;
    Ok(())
}

fn spawn_spec(
    program: &Path,
    interface: IpAddr,
    port: u16,
    extra_args: &[String],
) -> Result<CommandSpec> {
    let rimz_exe = std::env::current_exe().map_err(|source| WebErr::Io {
        path: PathBuf::from("/proc/self/exe"),
        source,
    })?;
    Ok(spawn_spec_for(
        program, &rimz_exe, interface, port, extra_args,
    ))
}

fn spawn_spec_for(
    program: &Path,
    rimz_exe: &Path,
    interface: IpAddr,
    port: u16,
    extra_args: &[String],
) -> CommandSpec {
    CommandSpec::new(program.display().to_string())
        .args(["-O", "-a", "-i", &interface.to_string(), "-p"])
        .arg(port.to_string())
        .args(extra_args.iter().cloned())
        .arg(rimz_exe.display().to_string())
        .args(["web", "exec", "--share"])
}

fn ensure_share_port_available(address: SocketAddr) -> Result<()> {
    ttyd::ensure_port_available(address).map_err(|err| match err {
        WebErr::ConfiguredPortInUse { port } => WebErr::ConfiguredSharePortInUse { port },
        other => other,
    })
}

fn normalize(sessions: &mut Vec<String>) {
    sessions.sort();
    sessions.dedup();
}

fn read_allowlist() -> Result<Allowlist> {
    Ok(ttyd::read_json_optional(&allowlist_path())?.unwrap_or_default())
}

fn write_allowlist(allowlist: &Allowlist) -> Result<()> {
    write_allowlist_at(&allowlist_path(), allowlist)
}

fn write_allowlist_at(path: &Path, allowlist: &Allowlist) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, allowlist)?;
    Ok(())
}

fn write_daemon(record: &DaemonRecord) -> Result<()> {
    atomic::write_temp_then_rename_cache(&daemon_path(), record)?;
    Ok(())
}

fn allowlist_path() -> PathBuf {
    paths::state_home().join("rimz").join(ALLOWLIST_FILE)
}

fn daemon_path() -> PathBuf {
    paths::state_home().join("rimz").join(DAEMON_FILE)
}

fn acquire_lock() -> Result<crate::store::lock::WorkspaceLock> {
    let path = paths::state_home().join("rimz").join(DAEMON_LOCK_FILE);
    Ok(crate::store::lock::WorkspaceLock::acquire(&path)?)
}

fn default_interface() -> String {
    "127.0.0.1".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_with_index(index_key: Option<&str>) -> ttyd::client::ClientProfile {
        ttyd::client::ClientProfile {
            index_key: index_key.map(str::to_owned),
            ..ttyd::client::ClientProfile::default()
        }
    }

    #[test]
    fn broadcast_argv_has_no_write_or_auth_and_uses_share_shim() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8201,
            &["-t".to_owned(), "cursorBlink=false".to_owned()],
        );

        assert_eq!(
            spec.args,
            [
                "-O",
                "-a",
                "-i",
                "127.0.0.1",
                "-p",
                "8201",
                "-t",
                "cursorBlink=false",
                "/opt/rimz/bin/rimz",
                "web",
                "exec",
                "--share"
            ]
        );
        assert!(!spec.args.iter().any(|arg| arg == "-W"));
        assert!(!spec.args.iter().any(|arg| arg == "-c"));
    }

    #[test]
    fn allowlist_roundtrips_sorted_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("web-share.json");
        let mut allowlist = Allowlist {
            sessions: vec!["rimz-b".to_owned(), "rimz-a".to_owned()],
        };
        normalize(&mut allowlist.sessions);
        write_allowlist_at(&path, &allowlist).expect("write allowlist");

        assert_eq!(
            ttyd::read_json_optional(&path).expect("read allowlist"),
            Some(Allowlist {
                sessions: vec!["rimz-a".to_owned(), "rimz-b".to_owned()]
            })
        );
    }

    #[test]
    fn daemon_reuse_requires_the_generated_index_key() {
        let desired = DaemonSpec {
            port: 8201,
            interface: "127.0.0.1".to_owned(),
        };
        let mut daemon = DaemonRecord {
            pid: 42,
            port: desired.port,
            interface: desired.interface.clone(),
            pixel_protocol: None,
            index_key: None,
        };

        assert!(record_matches(&daemon, &desired, &profile_with_index(None)));
        assert!(!record_matches(
            &daemon,
            &desired,
            &profile_with_index(Some("current"))
        ));
        daemon.index_key = Some("current".to_owned());
        assert!(record_matches(
            &daemon,
            &desired,
            &profile_with_index(Some("current"))
        ));
        daemon.index_key = Some("stale".to_owned());
        assert!(!record_matches(
            &daemon,
            &desired,
            &profile_with_index(Some("current"))
        ));
    }
}
