//! Shared ttyd daemon for browser access to every RimZ room.

use std::fmt;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{CredentialSummary, Result, WebAuth, WebCredential, WebErr, WebWarning, gate::Cidr};

pub(super) mod client;

use client::ClientProfile;

const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const DAEMON_FILE: &str = "web-ttyd.json";
const DAEMON_LOCK_FILE: &str = "web-ttyd.lock";
const LEGACY_INSTANCE_DIR: &str = "web-ttyd";
const START_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const MIN_TTYD_VERSION: TtydVersion = TtydVersion::new(1, 7, 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TtydVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl TtydVersion {
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for TtydVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

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
    #[serde(default = "default_interface")]
    pub(super) interface: String,
    #[serde(default)]
    pub(super) auth: WebAuth,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) auth_users: Vec<String>,
    #[serde(default)]
    pub(super) trusted_proxies: Vec<String>,
    #[serde(default)]
    pub(super) gate: Option<GateRecord>,
    #[serde(default)]
    pub(super) basic_upstream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pixel_protocol: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct GateRecord {
    pub(super) pid: u32,
    pub(super) upstream_port: u16,
}

impl DaemonRecord {
    pub(super) fn basic_loopback(pid: u32, port: u16) -> Self {
        Self {
            pid,
            port,
            interface: default_interface(),
            auth: WebAuth::Basic,
            auth_users: Vec::new(),
            trusted_proxies: Vec::new(),
            gate: None,
            basic_upstream: true,
            pixel_protocol: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DaemonSpec {
    port: u16,
    interface: String,
    auth: WebAuth,
    auth_users: Vec<String>,
    trusted_proxies: Vec<String>,
}

pub(super) struct RunningDaemon {
    pub(super) pid: u32,
    pub(super) port: u16,
    pub(super) interface: String,
    pub(super) auth: WebAuth,
    pub(super) credential: WebCredential,
    pub(super) tunnel_port: u16,
    pub(super) warnings: Vec<WebWarning>,
}

pub(super) struct DaemonInspection {
    pub(super) port: u16,
    pub(super) auth: WebAuth,
    pub(super) credential: Option<WebCredential>,
    pub(super) tunnel_port: Option<u16>,
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
    required_program().map(|_| ())
}

pub(super) fn program() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TTYD_BIN_ENV) {
        return Ok(PathBuf::from(path));
    }
    which::which("ttyd").map_err(|_| WebErr::MissingTtyd {
        minimum: MIN_TTYD_VERSION.to_string(),
    })
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

pub(super) fn required_program() -> Result<PathBuf> {
    let program = program()?;
    let reported = version_at(&program)?;
    require_supported_version(&reported)?;
    Ok(program)
}

fn require_supported_version(reported: &str) -> Result<TtydVersion> {
    let Some(found) = parse_version(reported) else {
        return Err(WebErr::TtydTooOld {
            found: reported.to_owned(),
            minimum: MIN_TTYD_VERSION.to_string(),
        });
    };
    if found < MIN_TTYD_VERSION {
        return Err(WebErr::TtydTooOld {
            found: found.to_string(),
            minimum: MIN_TTYD_VERSION.to_string(),
        });
    }
    Ok(found)
}

fn parse_version(reported: &str) -> Option<TtydVersion> {
    let version = reported
        .trim()
        .strip_prefix("ttyd version ")?
        .split_whitespace()
        .next()?;
    let mut components = version.splitn(3, '.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch_and_suffix = components.next()?;
    let patch_len = patch_and_suffix
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if patch_len == 0 {
        return None;
    }
    let (patch, suffix) = patch_and_suffix.split_at(patch_len);
    if !suffix.is_empty() && !matches!(suffix.as_bytes().first(), Some(b'-' | b'+')) {
        return None;
    }
    Some(TtydVersion::new(major, minor, patch.parse().ok()?))
}

pub(super) fn open_daemon(config: &MachineConfig, may_start: bool) -> Result<RunningDaemon> {
    if may_start {
        ensure_daemon(config)
    } else {
        let _guard = acquire_daemon_lock()?;
        let Some(record) = daemon_status_locked()? else {
            return Err(WebErr::TtydOffline);
        };
        let credential = required_credential()?;
        Ok(running_daemon(record, credential, Vec::new()))
    }
}

pub(super) fn inspect_daemon(config: &MachineConfig) -> Result<DaemonInspection> {
    let _guard = acquire_daemon_lock()?;
    let daemon = daemon_status_locked()?;
    let port = daemon
        .as_ref()
        .map_or(config.web.port, |record| record.port);
    let auth = daemon
        .as_ref()
        .map_or_else(|| auth_from_config(config), |record| record.auth.clone());
    let credential = read_credential()?.map(|credential| basic_auth(&credential));
    let tunnel_port = daemon.as_ref().map(tunnel_port);
    Ok(DaemonInspection {
        port,
        auth,
        credential,
        tunnel_port,
    })
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

pub(super) fn authorization_header() -> Result<String> {
    let credential = required_credential()?;
    Ok(basic_auth(&credential).authorization())
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
    let desired = desired_spec(config)?;
    let program = required_program()?;
    let _guard = acquire_daemon_lock()?;
    reap_legacy_instances();
    let daemon = daemon_status_locked()?;
    if let Some(record) = &daemon
        && record_matches(record, &desired)
    {
        let credential = required_credential()?;
        return Ok(running_daemon(
            record.clone(),
            credential,
            auth_warnings(&desired),
        ));
    }
    let prepared = prepare_fresh_start(config, &desired, daemon.as_ref(), program)?;
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, daemon, credential, prepared)
}

fn running_daemon(
    record: DaemonRecord,
    credential: TtydCredential,
    warnings: Vec<WebWarning>,
) -> RunningDaemon {
    let tunnel_port = tunnel_port(&record);
    RunningDaemon {
        pid: record.pid,
        port: record.port,
        interface: record.interface,
        auth: record.auth,
        credential: basic_auth(&credential),
        tunnel_port,
        warnings,
    }
}

fn required_credential() -> Result<TtydCredential> {
    read_credential()?.ok_or(WebErr::TtydCredentialMissing)
}

fn tunnel_port(record: &DaemonRecord) -> u16 {
    record
        .gate
        .as_ref()
        .map_or(record.port, |gate| gate.upstream_port)
}

fn auth_from_config(config: &MachineConfig) -> WebAuth {
    config
        .web
        .auth_header
        .as_deref()
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .map_or(WebAuth::Basic, |header| WebAuth::TrustedHeader {
            header: header.to_owned(),
        })
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
    for value in &config.web.trusted_proxies {
        Cidr::parse(value)?;
    }
    let auth = auth_from_config(config);
    if !config.web.auth_users.is_empty() && !matches!(auth, WebAuth::TrustedHeader { .. }) {
        return Err(WebErr::AuthUsersRequireHeader);
    }
    let auth_users = config
        .web
        .auth_users
        .iter()
        .map(|user| user.trim().to_owned())
        .collect::<Vec<_>>();
    if auth_users.iter().any(String::is_empty) {
        return Err(WebErr::EmptyAuthUser);
    }
    Ok(DaemonSpec {
        port: config.web.port,
        interface,
        auth,
        auth_users,
        trusted_proxies: config.web.trusted_proxies.clone(),
    })
}

fn record_matches(record: &DaemonRecord, desired: &DaemonSpec) -> bool {
    record.basic_upstream
        && record.port == desired.port
        && record.interface == desired.interface
        && record.auth == desired.auth
        && record.auth_users == desired.auth_users
        && record.trusted_proxies == desired.trusted_proxies
        && record.gate.is_some() == gated(desired)
        && record
            .pixel_protocol
            .is_none_or(|protocol| protocol == crate::web::TTYD_PIXEL_PROTOCOL)
}

fn auth_warnings(desired: &DaemonSpec) -> Vec<WebWarning> {
    let Ok(interface) = desired.interface.parse::<IpAddr>() else {
        return Vec::new();
    };
    if matches!(desired.auth, WebAuth::TrustedHeader { .. })
        && !interface.is_loopback()
        && desired.trusted_proxies.is_empty()
    {
        vec![WebWarning::HeaderAuthUnprotected(format!(
            "trusted-header auth on {interface}:{} accepts only loopback proxies; add the authenticating proxy network to `[web] trusted_proxies` before connecting from another host",
            desired.port
        ))]
    } else {
        Vec::new()
    }
}

fn gated(desired: &DaemonSpec) -> bool {
    !desired.trusted_proxies.is_empty() || matches!(desired.auth, WebAuth::TrustedHeader { .. })
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

pub(super) fn is_ttyd_process(process: &crate::proc::ProcInfo) -> bool {
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
                wait_for_address_close(
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), instance.port),
                    Duration::from_secs(1),
                );
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
    desired: &DaemonSpec,
    credential: &TtydCredential,
    profile: &ClientProfile,
) -> Result<DaemonRecord> {
    let is_gated = gated(desired);
    let ttyd_port = if is_gated {
        choose_ephemeral_port().map_err(|source| WebErr::GateIo {
            action: "choosing the ttyd upstream port",
            source,
        })?
    } else {
        desired.port
    };
    let ttyd_interface = if is_gated {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        desired
            .interface
            .parse()
            .map_err(|_| WebErr::InvalidInterface {
                value: desired.interface.clone(),
            })?
    };
    let ttyd_address = SocketAddr::new(ttyd_interface, ttyd_port);
    let spec = spawn_spec(
        program,
        ttyd_interface,
        ttyd_port,
        credential,
        &profile.args,
    )?;
    let pid = spawn_detached(spec)?;
    let ttyd_probe_address = probe_address(ttyd_address);
    if !wait_for_address(ttyd_probe_address, START_TIMEOUT) {
        terminate_pids(&[pid]);
        wait_for_address_close(ttyd_probe_address, Duration::from_secs(1));
        return Err(WebErr::TtydStartTimeout {
            address: ttyd_probe_address,
        });
    }
    let gate = if is_gated {
        let public_address = socket_address(&desired.interface, desired.port)?;
        let gate_pid = match spawn_gate(
            public_address,
            ttyd_address,
            &desired.trusted_proxies,
            &desired.auth,
            &desired.auth_users,
        ) {
            Ok(pid) => pid,
            Err(err) => {
                terminate_pids(&[pid]);
                return Err(err);
            }
        };
        Some(GateRecord {
            pid: gate_pid,
            upstream_port: ttyd_port,
        })
    } else {
        None
    };
    let record = DaemonRecord {
        pid,
        port: desired.port,
        interface: desired.interface.clone(),
        auth: desired.auth.clone(),
        auth_users: desired.auth_users.clone(),
        trusted_proxies: desired.trusted_proxies.clone(),
        gate,
        basic_upstream: true,
        pixel_protocol: profile.pixel_protocol,
    };
    let public_address = record_public_address(&record)?;
    if !wait_for_address(public_address, START_TIMEOUT) {
        let _ = stop_record(&record);
        return Err(WebErr::TtydStartTimeout {
            address: public_address,
        });
    }
    if let Err(err) = write_daemon(&record) {
        let _ = stop_record(&record);
        return Err(err);
    }
    Ok(record)
}

fn spawn_gate(
    listen: SocketAddr,
    upstream: SocketAddr,
    allow: &[String],
    auth: &WebAuth,
    auth_users: &[String],
) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|source| WebErr::Io {
        path: PathBuf::from("/proc/self/exe"),
        source,
    })?;
    let mut spec = CommandSpec::new(exe.display().to_string())
        .args(["web", "gate", "--listen"])
        .arg(listen.to_string())
        .arg("--upstream")
        .arg(upstream.to_string());
    for cidr in allow {
        spec = spec.arg("--allow").arg(cidr.clone());
    }
    if let WebAuth::TrustedHeader { header } = auth {
        spec = spec.arg("--auth-header").arg(header.clone());
    }
    for user in auth_users {
        spec = spec.arg("--auth-user").arg(user.clone());
    }
    spawn_detached(spec)
}

pub(super) fn rotate_credential(config: &MachineConfig) -> Result<CredentialRotation> {
    let _guard = acquire_daemon_lock()?;
    rotate_credential_locked(config)
}

fn rotate_credential_locked(config: &MachineConfig) -> Result<CredentialRotation> {
    let desired = desired_spec(config)?;
    let Some(daemon) = daemon_status_locked()? else {
        return Ok(CredentialRotation {
            credential: basic_auth(&mint_credential()?),
            restarted: false,
            warnings: Vec::new(),
        });
    };
    let program = required_program()?;
    let prepared = prepare_fresh_start(config, &desired, Some(&daemon), program)?;
    let credential = mint_credential()?;
    let running = start_fresh_locked(&desired, Some(daemon), credential.clone(), prepared)?;
    Ok(CredentialRotation {
        credential: basic_auth(&credential),
        restarted: true,
        warnings: running.warnings,
    })
}

pub(super) fn restart_daemon(config: &MachineConfig) -> Result<(RunningDaemon, bool)> {
    let desired = desired_spec(config)?;
    let program = required_program()?;
    let _guard = acquire_daemon_lock()?;
    reap_legacy_instances();
    let daemon = daemon_status_locked()?;
    let was_online = daemon.is_some();
    let prepared = prepare_fresh_start(config, &desired, daemon.as_ref(), program)?;
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, daemon, credential, prepared).map(|daemon| (daemon, was_online))
}

pub(super) fn restart_if_online(config: &MachineConfig) -> Result<Option<RunningDaemon>> {
    let _guard = acquire_daemon_lock()?;
    reap_legacy_instances();
    let Some(daemon) = daemon_status_locked()? else {
        return Ok(None);
    };
    let desired = desired_spec(config)?;
    let program = required_program()?;
    let prepared = prepare_fresh_start(config, &desired, Some(&daemon), program)?;
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, Some(daemon), credential, prepared).map(Some)
}

type FreshStart = (PathBuf, SocketAddr, ClientProfile);

fn prepare_fresh_start(
    config: &MachineConfig,
    desired: &DaemonSpec,
    daemon: Option<&DaemonRecord>,
    program: PathBuf,
) -> Result<FreshStart> {
    let public_address = socket_address(&desired.interface, desired.port)?;
    if daemon.is_none_or(|record| {
        socket_address(&record.interface, record.port).ok() != Some(public_address)
    }) {
        ensure_port_available(public_address)?;
    }
    let profile = client::profile(config, &program);
    Ok((program, public_address, profile))
}

fn start_fresh_locked(
    desired: &DaemonSpec,
    daemon: Option<DaemonRecord>,
    credential: TtydCredential,
    (program, public_address, profile): FreshStart,
) -> Result<RunningDaemon> {
    if let Some(record) = daemon {
        stop_record(&record)?;
    }
    ensure_port_available(public_address)?;
    let record = start_daemon_with_profile(&program, desired, &credential, &profile)?;
    let mut warnings = auth_warnings(desired);
    warnings.extend(profile.warnings);
    Ok(running_daemon(record, credential, warnings))
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

pub(crate) fn pixel_daemon_record() -> Option<(u32, u32)> {
    let bytes = fs::read(daemon_path()).ok()?;
    let record = serde_json::from_slice::<DaemonRecord>(&bytes).ok()?;
    record.pixel_protocol.map(|protocol| (record.pid, protocol))
}

fn daemon_status_locked() -> Result<Option<DaemonRecord>> {
    let path = daemon_path();
    let Some(record) = read_json_optional::<DaemonRecord>(&path)? else {
        return Ok(None);
    };
    let processes = crate::proc::list_processes();
    let ttyd_live = processes
        .iter()
        .any(|process| process.pid == record.pid && is_ttyd_process(process));
    let gate_live = record.gate.as_ref().is_none_or(|gate| {
        processes
            .iter()
            .any(|process| process.pid == gate.pid && is_gate_process(process))
    });
    let listening = record_public_address(&record)
        .ok()
        .is_some_and(|address| TcpStream::connect(address).is_ok());
    if ttyd_live && gate_live && listening {
        return Ok(Some(record));
    }
    terminate_live_record(&record, ttyd_live, gate_live);
    remove_optional(&path)?;
    Ok(None)
}

fn is_gate_process(process: &crate::proc::ProcInfo) -> bool {
    crate::proc::command::program_label(&process.cmdline) == "rimz"
        && process
            .cmdline
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|args| args == ["web", "gate"])
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
    terminate_live_record(record, true, record.gate.is_some());
}

fn terminate_live_record(record: &DaemonRecord, ttyd_live: bool, gate_live: bool) {
    #[cfg(unix)]
    {
        let mut pids = Vec::new();
        if gate_live && let Some(gate) = &record.gate {
            pids.push(gate.pid);
        }
        if ttyd_live {
            pids.push(record.pid);
        }
        let terminated = !pids.is_empty();
        terminate_pids(&pids);
        if terminated && let Ok(address) = record_public_address(record) {
            wait_for_address_close(address, Duration::from_secs(1));
        }
    }
    #[cfg(not(unix))]
    let _ = (record, ttyd_live, gate_live);
}

#[cfg(unix)]
pub(super) fn terminate_pids(pids: &[u32]) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let mut survivors = Vec::new();
    for pid in pids {
        if let Ok(raw) = i32::try_from(*pid) {
            let _ = kill(Pid::from_raw(raw), Signal::SIGTERM);
            survivors.push(*pid);
        }
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
}

#[cfg(not(unix))]
pub(super) fn terminate_pids(_pids: &[u32]) {}

fn spawn_spec(
    program: &Path,
    interface: IpAddr,
    port: u16,
    credential: &TtydCredential,
    extra_args: &[String],
) -> Result<CommandSpec> {
    spawn_spec_for(
        program,
        &std::env::current_exe().map_err(|source| WebErr::Io {
            path: PathBuf::from("/proc/self/exe"),
            source,
        })?,
        interface,
        port,
        credential,
        extra_args,
    )
}

fn spawn_spec_for(
    program: &Path,
    rimz_exe: &Path,
    interface: IpAddr,
    port: u16,
    credential: &TtydCredential,
    extra_args: &[String],
) -> Result<CommandSpec> {
    let mut spec = CommandSpec::new(program.display().to_string()).args(["-W", "-O", "-a"]);
    spec = spec.arg("-c").arg(format!("rimz:{}", credential.secret));
    Ok(spec
        .args(["-i", &interface.to_string(), "-p"])
        .arg(port.to_string())
        .args(extra_args.iter().cloned())
        .arg(rimz_exe.display().to_string())
        .args(["web", "exec"]))
}

pub(super) fn spawn_detached(spec: CommandSpec) -> Result<u32> {
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

pub(super) fn ensure_port_available(address: SocketAddr) -> Result<()> {
    TcpListener::bind(address)
        .map(|_| ())
        .map_err(|_| WebErr::ConfiguredPortInUse {
            port: address.port(),
        })
}

fn choose_ephemeral_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.local_addr().map(|address| address.port())
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    wait_for_address(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        timeout,
    )
}

pub(super) fn wait_for_address(address: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(address).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
pub(super) fn wait_for_address_close(address: SocketAddr, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && TcpStream::connect(address).is_ok() {
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn socket_address(interface: &str, port: u16) -> Result<SocketAddr> {
    let interface = interface
        .parse::<IpAddr>()
        .map_err(|_| WebErr::InvalidInterface {
            value: interface.to_owned(),
        })?;
    Ok(SocketAddr::new(interface, port))
}

fn record_public_address(record: &DaemonRecord) -> Result<SocketAddr> {
    Ok(probe_address(socket_address(
        &record.interface,
        record.port,
    )?))
}

pub(super) fn probe_address(mut address: SocketAddr) -> SocketAddr {
    if address.ip().is_unspecified() {
        address.set_ip(match address.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    address
}

fn default_interface() -> String {
    "127.0.0.1".to_owned()
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

pub(super) fn read_json_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
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

pub(super) fn remove_optional(path: &Path) -> Result<bool> {
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

    fn credential() -> TtydCredential {
        TtydCredential {
            name: "rimz".to_owned(),
            created_at: Timestamp::from_second(1_700_000_000).expect("timestamp"),
            secret: "secret".to_owned(),
        }
    }

    #[test]
    fn ttyd_version_gate_accepts_the_minimum_and_rejects_the_previous_patch() {
        assert!(matches!(
            require_supported_version("ttyd version 1.7.4"),
            Err(WebErr::TtydTooOld { found, minimum })
                if found == "1.7.4" && minimum == "1.7.5"
        ));
        assert_eq!(
            require_supported_version("ttyd version 1.7.5").expect("minimum ttyd version"),
            MIN_TTYD_VERSION
        );
        assert_eq!(
            require_supported_version("ttyd version 1.7.7-1+deb13u1")
                .expect("packaged ttyd version"),
            TtydVersion::new(1, 7, 7)
        );
    }

    #[test]
    fn malformed_ttyd_versions_fail_with_the_reported_value() {
        for reported in ["", "ttyd 1.7.7", "ttyd version 1.7", "ttyd version current"] {
            assert!(matches!(
                require_supported_version(reported),
                Err(WebErr::TtydTooOld { found, minimum })
                    if found == reported && minimum == "1.7.5"
            ));
        }
    }

    #[test]
    fn argv_uses_loopback_auth_url_args_and_rimz_shim() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            8201,
            &credential(),
            &["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
        )
        .expect("spawn spec");
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
    fn argv_uses_basic_auth_for_a_trusted_header_daemon() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8399,
            &credential(),
            &[],
        )
        .expect("spawn spec");

        assert!(
            spec.args
                .windows(2)
                .any(|args| args == ["-c", "rimz:secret"])
        );
        assert!(!spec.args.iter().any(|arg| arg == "-H"));
        assert!(spec.args.windows(2).any(|args| args == ["-i", "127.0.0.1"]));
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
            "127.0.0.1".parse().expect("IP"),
            8202,
            &credential(),
            &extra,
        )
        .expect("spawn spec");

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
    fn unspecified_listener_probes_use_the_matching_loopback_family() {
        assert_eq!(
            probe_address("0.0.0.0:8200".parse().expect("IPv4 socket")),
            "127.0.0.1:8200".parse().expect("IPv4 loopback socket")
        );
        assert_eq!(
            probe_address("[::]:8200".parse().expect("IPv6 socket")),
            "[::1]:8200".parse().expect("IPv6 loopback socket")
        );
    }

    #[test]
    fn credential_roundtrip_is_private() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("credential.json");
        let credential = credential();
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
            interface: "0.0.0.0".to_owned(),
            auth: WebAuth::TrustedHeader {
                header: "X-Forwarded-User".to_owned(),
            },
            auth_users: vec!["alice".to_owned()],
            trusted_proxies: vec!["10.0.0.0/8".to_owned()],
            gate: Some(GateRecord {
                pid: u32::MAX - 1,
                upstream_port: 41820,
            }),
            basic_upstream: true,
            pixel_protocol: Some(crate::web::TTYD_PIXEL_PROTOCOL),
        };
        write_daemon_at(&path, &daemon).expect("write daemon state");
        assert_eq!(read_json_optional(&path).expect("read state"), Some(daemon));
    }

    #[test]
    fn old_daemon_state_defaults_to_a_non_reusable_basic_upstream() {
        let daemon: DaemonRecord =
            serde_json::from_str(r#"{"pid":42,"port":8200}"#).expect("old record");
        assert_eq!(daemon.pid, 42);
        assert_eq!(daemon.auth, WebAuth::Basic);
        assert!(daemon.auth_users.is_empty());
        assert!(daemon.gate.is_none());
        assert!(!daemon.basic_upstream);
    }

    #[test]
    fn desired_spec_validates_listener_and_proxy_cidrs() {
        let mut config = MachineConfig::default();
        config.web.interface = "localhost".to_owned();
        assert!(matches!(
            desired_spec(&config),
            Err(WebErr::InvalidInterface { value }) if value == "localhost"
        ));

        config.web.interface = "127.0.0.1".to_owned();
        config.web.trusted_proxies = vec!["10.0.0.0/33".to_owned()];
        assert!(matches!(
            desired_spec(&config),
            Err(WebErr::InvalidTrustedProxy { value, .. }) if value == "10.0.0.0/33"
        ));
    }

    #[test]
    fn desired_spec_validates_and_trims_auth_users() {
        let mut config = MachineConfig::default();
        config.web.auth_users = vec!["alice".to_owned()];
        assert!(matches!(
            desired_spec(&config),
            Err(WebErr::AuthUsersRequireHeader)
        ));

        config.web.auth_header = Some(" \t ".to_owned());
        assert!(matches!(
            desired_spec(&config),
            Err(WebErr::AuthUsersRequireHeader)
        ));

        config.web.auth_header = Some("X-Forwarded-User".to_owned());
        config.web.auth_users = vec![" \t ".to_owned()];
        assert!(matches!(desired_spec(&config), Err(WebErr::EmptyAuthUser)));

        config.web.auth_users = vec![" alice ".to_owned(), "bob".to_owned()];
        let desired = desired_spec(&config).expect("valid auth users");
        assert_eq!(desired.auth_users, ["alice", "bob"]);
    }

    #[test]
    fn non_loopback_trusted_header_auth_warns_without_a_proxy_allowlist() {
        let spec = DaemonSpec {
            port: 8200,
            interface: "0.0.0.0".to_owned(),
            auth: WebAuth::TrustedHeader {
                header: "X-Forwarded-User".to_owned(),
            },
            auth_users: Vec::new(),
            trusted_proxies: Vec::new(),
        };
        assert!(matches!(
            auth_warnings(&spec).as_slice(),
            [WebWarning::HeaderAuthUnprotected(message)] if message.contains("trusted_proxies")
        ));
    }

    #[test]
    fn pre_pixel_daemon_state_deserializes_without_protocol() {
        let daemon = serde_json::from_str::<DaemonRecord>(r#"{"pid":42,"port":8200}"#)
            .expect("legacy daemon state");

        assert_eq!(daemon.pid, 42);
        assert_eq!(daemon.port, 8200);
        assert_eq!(daemon.pixel_protocol, None);
    }

    #[test]
    fn daemon_reuse_requires_the_current_pixel_protocol() {
        let config = MachineConfig::default();
        let desired = desired_spec(&config).expect("desired daemon");
        let mut daemon = DaemonRecord::basic_loopback(42, config.web.port);

        assert!(record_matches(&daemon, &desired));
        daemon.basic_upstream = false;
        assert!(!record_matches(&daemon, &desired));
        daemon.basic_upstream = true;
        daemon.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL);
        assert!(record_matches(&daemon, &desired));
        daemon.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL - 1);
        assert!(!record_matches(&daemon, &desired));
        daemon.pixel_protocol = None;
        daemon.auth_users.push("alice".to_owned());
        assert!(!record_matches(&daemon, &desired));
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
        assert!(is_gate_process(&process(
            "/opt/rimz/bin/rimz web gate --listen 0.0.0.0:8200"
        )));
        assert!(!is_gate_process(&process("/opt/rimz/bin/rimz web start")));
    }
}
