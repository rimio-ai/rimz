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

use super::{
    CredentialRotation, CredentialSummary, Result, WebAuth, WebCredential, WebDaemonOutcome,
    WebErr, WebOpenPayload, WebShareOutcome, WebSharePayload, WebUnshareOutcome, WebWarning,
    gate::Cidr,
};

pub(super) mod client;

use client::ClientProfile;

const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const DAEMON_FILE: &str = "web-ttyd.json";
const DAEMON_LOCK_FILE: &str = "web-ttyd.lock";
const SHARE_ALLOWLIST_FILE: &str = "web-share.json";
const SHARE_DAEMON_FILE: &str = "web-ttyd-share.json";
const SHARE_DAEMON_LOCK_FILE: &str = "web-ttyd-share.lock";
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
pub(super) struct TtydProcessRecord {
    pub(super) pid: u32,
    pub(super) port: u16,
    #[serde(default = "default_interface")]
    pub(super) interface: String,
    #[serde(default)]
    pub(super) launch_context_scrubbed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pixel_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) index_key: Option<String>,
}

impl TtydProcessRecord {
    fn matches(&self, desired: &ListenerSpec, profile: &ClientProfile) -> bool {
        self.launch_context_scrubbed
            && self.port == desired.port
            && self.interface == desired.interface
            && self.index_key == profile.index_key
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WritableDaemonRecord {
    #[serde(flatten)]
    pub(super) process: TtydProcessRecord,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct GateRecord {
    pub(super) pid: u32,
    pub(super) upstream_port: u16,
}

impl WritableDaemonRecord {
    pub(super) fn basic_loopback(pid: u32, port: u16) -> Self {
        Self {
            process: TtydProcessRecord {
                pid,
                port,
                interface: default_interface(),
                launch_context_scrubbed: true,
                pixel_protocol: None,
                index_key: None,
            },
            auth: WebAuth::Basic,
            auth_users: Vec::new(),
            trusted_proxies: Vec::new(),
            gate: None,
            basic_upstream: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListenerSpec {
    port: u16,
    interface: String,
}

#[derive(Clone, Copy)]
enum ListenerKind {
    Writable,
    Broadcast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WritableDaemonSpec {
    listener: ListenerSpec,
    auth: WebAuth,
    auth_users: Vec<String>,
    trusted_proxies: Vec<String>,
}

pub(super) struct WritableDaemon {
    pub(super) outcome: WebDaemonOutcome,
    pub(super) auth: WebAuth,
    pub(super) credential: WebCredential,
    pub(super) tunnel_port: u16,
}

#[derive(Debug, Deserialize)]
struct LegacyTtydInstance {
    session: String,
    pid: u32,
    port: u16,
}

pub(super) fn preflight() -> Result<()> {
    required_program_with_version().map(|_| ())
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

pub(super) fn required_program_with_version() -> Result<(PathBuf, String)> {
    let program = program()?;
    let reported = version_at(&program)?;
    require_supported_version(&reported)?;
    Ok((program, reported))
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

pub(super) fn open_daemon(config: &MachineConfig, may_start: bool) -> Result<WritableDaemon> {
    if may_start {
        ensure_daemon(config)
    } else {
        let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
        let Some(record) = daemon_status_locked()? else {
            return Err(WebErr::TtydOffline);
        };
        let credential = required_credential()?;
        Ok(running_daemon(record, credential, Vec::new()))
    }
}

pub(super) fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let daemon = daemon_status_locked()?;
    let port = daemon
        .as_ref()
        .map_or(config.web.port, |record| record.process.port);
    let auth = daemon
        .as_ref()
        .map_or_else(|| auth_from_config(config), |record| record.auth.clone());
    let credential = read_credential()?.map(|credential| basic_auth(&credential));
    let tunnel_port = daemon.as_ref().map(tunnel_port);
    let base_url = super::normalized_base_url(config.web.base_url.as_deref(), port);
    Ok(WebOpenPayload::for_session(
        session,
        base_url,
        port,
        tunnel_port,
        auth,
        credential,
    ))
}

pub(super) fn credential_summary() -> Result<Option<CredentialSummary>> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
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
    write_credential_at(&state_path(CREDENTIAL_FILE), &record)?;
    Ok(record)
}

fn read_credential() -> Result<Option<TtydCredential>> {
    read_json_optional(&state_path(CREDENTIAL_FILE))
}

fn ensure_credential() -> Result<TtydCredential> {
    read_credential()?.map_or_else(mint_credential, Ok)
}

fn clear_credential() -> Result<bool> {
    remove_optional(&state_path(CREDENTIAL_FILE))
}

pub(super) fn ensure_daemon(config: &MachineConfig) -> Result<WritableDaemon> {
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    reap_legacy_instances();
    let daemon = daemon_status_locked()?;
    let prepared = prepare_writable_start(config, &desired, daemon.as_ref(), program, &version)?;
    if let Some(record) = &daemon
        && record_matches(record, &desired, &prepared.2)
    {
        let credential = required_credential()?;
        let mut warnings = auth_warnings(&desired);
        warnings.extend(prepared.2.warnings.clone());
        return Ok(running_daemon(record.clone(), credential, warnings));
    }
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, daemon, credential, prepared)
}

fn running_daemon(
    record: WritableDaemonRecord,
    credential: TtydCredential,
    warnings: Vec<WebWarning>,
) -> WritableDaemon {
    let tunnel_port = tunnel_port(&record);
    WritableDaemon {
        outcome: WebDaemonOutcome {
            pid: record.process.pid,
            port: record.process.port,
            interface: record.process.interface,
            warnings,
        },
        auth: record.auth,
        credential: basic_auth(&credential),
        tunnel_port,
    }
}

fn required_credential() -> Result<TtydCredential> {
    read_credential()?.ok_or(WebErr::TtydCredentialMissing)
}

fn tunnel_port(record: &WritableDaemonRecord) -> u16 {
    record
        .gate
        .as_ref()
        .map_or(record.process.port, |gate| gate.upstream_port)
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

fn desired_spec(config: &MachineConfig) -> Result<WritableDaemonSpec> {
    let listener = desired_listener(config, config.web.port)?;
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
    Ok(WritableDaemonSpec {
        listener,
        auth,
        auth_users,
        trusted_proxies: config.web.trusted_proxies.clone(),
    })
}

fn desired_listener(config: &MachineConfig, port: u16) -> Result<ListenerSpec> {
    let interface = config
        .web
        .interface
        .parse::<IpAddr>()
        .map_err(|_| WebErr::InvalidInterface {
            value: config.web.interface.clone(),
        })?
        .to_string();
    Ok(ListenerSpec { port, interface })
}

fn record_matches(
    record: &WritableDaemonRecord,
    desired: &WritableDaemonSpec,
    profile: &ClientProfile,
) -> bool {
    record.basic_upstream
        && record.process.matches(&desired.listener, profile)
        && record.auth == desired.auth
        && record.auth_users == desired.auth_users
        && record.trusted_proxies == desired.trusted_proxies
        && record.gate.is_some() == gated(desired)
}

fn auth_warnings(desired: &WritableDaemonSpec) -> Vec<WebWarning> {
    let Ok(interface) = desired.listener.interface.parse::<IpAddr>() else {
        return Vec::new();
    };
    if matches!(desired.auth, WebAuth::TrustedHeader { .. })
        && !interface.is_loopback()
        && desired.trusted_proxies.is_empty()
    {
        vec![WebWarning::HeaderAuthUnprotected(format!(
            "trusted-header auth on {interface}:{} accepts only loopback proxies; add the authenticating proxy network to `[web] trusted_proxies` before connecting from another host",
            desired.listener.port
        ))]
    } else {
        Vec::new()
    }
}

fn gated(desired: &WritableDaemonSpec) -> bool {
    !desired.trusted_proxies.is_empty() || matches!(desired.auth, WebAuth::TrustedHeader { .. })
}

fn reap_legacy_instances() {
    let dir = state_path(LEGACY_INSTANCE_DIR);
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
    desired: &WritableDaemonSpec,
    credential: &TtydCredential,
    profile: &ClientProfile,
) -> Result<WritableDaemonRecord> {
    let is_gated = gated(desired);
    let ttyd_port = if is_gated {
        choose_ephemeral_port().map_err(|source| WebErr::GateIo {
            action: "choosing the ttyd upstream port",
            source,
        })?
    } else {
        desired.listener.port
    };
    let ttyd_interface = if is_gated {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        desired
            .listener
            .interface
            .parse()
            .map_err(|_| WebErr::InvalidInterface {
                value: desired.listener.interface.clone(),
            })?
    };
    let process = spawn_ttyd_process(
        program,
        ttyd_interface,
        ttyd_port,
        &desired.listener,
        TtydMode::Writable(credential),
        profile,
        ListenerKind::Writable,
    )?;
    let pid = process.pid;
    let ttyd_address = SocketAddr::new(ttyd_interface, ttyd_port);
    let gate = if is_gated {
        let public_address = socket_address(&desired.listener.interface, desired.listener.port)?;
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
    let record = WritableDaemonRecord {
        process,
        auth: desired.auth.clone(),
        auth_users: desired.auth_users.clone(),
        trusted_proxies: desired.trusted_proxies.clone(),
        gate,
        basic_upstream: true,
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
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
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
    let (program, version) = required_program_with_version()?;
    let prepared = prepare_writable_start(config, &desired, Some(&daemon), program, &version)?;
    let credential = mint_credential()?;
    let running = start_fresh_locked(&desired, Some(daemon), credential.clone(), prepared)?;
    Ok(CredentialRotation {
        credential: basic_auth(&credential),
        restarted: true,
        warnings: running.outcome.warnings,
    })
}

pub(super) fn restart_daemon(config: &MachineConfig) -> Result<(WritableDaemon, bool)> {
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    reap_legacy_instances();
    let daemon = daemon_status_locked()?;
    let was_online = daemon.is_some();
    let prepared = prepare_writable_start(config, &desired, daemon.as_ref(), program, &version)?;
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, daemon, credential, prepared).map(|daemon| (daemon, was_online))
}

pub(super) fn restart_if_online(config: &MachineConfig) -> Result<Option<WritableDaemon>> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    reap_legacy_instances();
    let Some(daemon) = daemon_status_locked()? else {
        return Ok(None);
    };
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let prepared = prepare_writable_start(config, &desired, Some(&daemon), program, &version)?;
    let credential = ensure_credential()?;
    start_fresh_locked(&desired, Some(daemon), credential, prepared).map(Some)
}

type FreshStart = (PathBuf, SocketAddr, ClientProfile);

fn prepare_writable_start(
    config: &MachineConfig,
    desired: &WritableDaemonSpec,
    daemon: Option<&WritableDaemonRecord>,
    program: PathBuf,
    version: &str,
) -> Result<FreshStart> {
    prepare_fresh_start(
        config,
        &desired.listener,
        daemon.map(|record| &record.process),
        program,
        version,
        ListenerKind::Writable,
    )
}

fn prepare_fresh_start(
    config: &MachineConfig,
    desired: &ListenerSpec,
    daemon: Option<&TtydProcessRecord>,
    program: PathBuf,
    version: &str,
    kind: ListenerKind,
) -> Result<FreshStart> {
    let public_address = socket_address(&desired.interface, desired.port)?;
    if daemon.is_none_or(|record| {
        socket_address(&record.interface, record.port).ok() != Some(public_address)
    }) {
        ensure_port_available(public_address, kind)?;
    }
    let profile = client::profile(config, &program, version);
    Ok((program, public_address, profile))
}

fn start_fresh_locked(
    desired: &WritableDaemonSpec,
    daemon: Option<WritableDaemonRecord>,
    credential: TtydCredential,
    (program, public_address, profile): FreshStart,
) -> Result<WritableDaemon> {
    if let Some(record) = daemon {
        stop_record(&record)?;
    }
    ensure_port_available(public_address, ListenerKind::Writable)?;
    let record = start_daemon_with_profile(&program, desired, &credential, &profile)?;
    let mut warnings = auth_warnings(desired);
    warnings.extend(profile.warnings);
    Ok(running_daemon(record, credential, warnings))
}

pub(super) fn revoke_credential() -> Result<bool> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let stopped = stop_daemon_locked()?;
    clear_credential()?;
    Ok(stopped)
}

pub(super) fn daemon_status() -> Result<Option<WritableDaemonRecord>> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    daemon_status_locked()
}

pub(crate) fn pixel_daemon_record() -> Option<(u32, u32)> {
    let bytes = fs::read(state_path(DAEMON_FILE)).ok()?;
    let record = serde_json::from_slice::<WritableDaemonRecord>(&bytes).ok()?;
    record
        .process
        .pixel_protocol
        .map(|protocol| (record.process.pid, protocol))
}

fn daemon_status_locked() -> Result<Option<WritableDaemonRecord>> {
    let path = state_path(DAEMON_FILE);
    let Some(record) = read_json_optional::<WritableDaemonRecord>(&path)? else {
        return Ok(None);
    };
    let processes = crate::proc::list_processes();
    let (ttyd_live, listening) = ttyd_process_status(&record.process, &processes);
    let gate_live = record.gate.as_ref().is_none_or(|gate| {
        processes
            .iter()
            .any(|process| process.pid == gate.pid && is_gate_process(process))
    });
    if ttyd_live && gate_live && listening {
        return Ok(Some(record));
    }
    terminate_live_record(&record, ttyd_live, gate_live);
    remove_optional(&path)?;
    Ok(None)
}

fn ttyd_process_status(
    record: &TtydProcessRecord,
    processes: &[crate::proc::ProcInfo],
) -> (bool, bool) {
    let live = processes
        .iter()
        .any(|process| process.pid == record.pid && is_ttyd_process(process));
    let listening = process_address(record)
        .ok()
        .is_some_and(|address| TcpStream::connect(address).is_ok());
    (live, listening)
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
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    stop_daemon_locked()
}

fn stop_daemon_locked() -> Result<bool> {
    let Some(record) = daemon_status_locked()? else {
        return Ok(false);
    };
    stop_record(&record)?;
    Ok(true)
}

fn stop_record(record: &WritableDaemonRecord) -> Result<()> {
    terminate_record(record);
    remove_optional(&state_path(DAEMON_FILE))?;
    Ok(())
}

fn terminate_record(record: &WritableDaemonRecord) {
    terminate_live_record(record, true, record.gate.is_some());
}

fn terminate_ttyd_process(record: &TtydProcessRecord) {
    terminate_pids(&[record.pid]);
    #[cfg(unix)]
    if let Ok(address) = process_address(record) {
        wait_for_address_close(address, Duration::from_secs(1));
    }
}

fn terminate_live_record(record: &WritableDaemonRecord, ttyd_live: bool, gate_live: bool) {
    #[cfg(unix)]
    {
        let mut pids = Vec::new();
        if gate_live && let Some(gate) = &record.gate {
            pids.push(gate.pid);
        }
        if ttyd_live {
            pids.push(record.process.pid);
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

#[derive(Clone, Copy)]
enum TtydMode<'a> {
    Writable(&'a TtydCredential),
    Broadcast,
}

fn spawn_spec(
    program: &Path,
    interface: IpAddr,
    port: u16,
    mode: TtydMode<'_>,
    extra_args: &[String],
) -> Result<CommandSpec> {
    Ok(spawn_spec_for(
        program,
        &std::env::current_exe().map_err(|source| WebErr::Io {
            path: PathBuf::from("/proc/self/exe"),
            source,
        })?,
        interface,
        port,
        mode,
        extra_args,
    ))
}

fn spawn_spec_for(
    program: &Path,
    rimz_exe: &Path,
    interface: IpAddr,
    port: u16,
    mode: TtydMode<'_>,
    extra_args: &[String],
) -> CommandSpec {
    let mut spec = CommandSpec::new(program.display().to_string());
    if matches!(mode, TtydMode::Writable(_)) {
        spec = spec.arg("-W");
    }
    spec = spec.args(["-O", "-a", "-P", "3600"]);
    if let TtydMode::Writable(credential) = mode {
        spec = spec.arg("-c").arg(format!("rimz:{}", credential.secret));
    }
    spec = spec
        .args(["-i", &interface.to_string(), "-p"])
        .arg(port.to_string())
        .args(extra_args.iter().cloned())
        .arg(rimz_exe.display().to_string())
        .args(["web", "exec"]);
    if matches!(mode, TtydMode::Broadcast) {
        spec = spec.arg("--share");
    }
    super::without_ttyd_launch_context(spec)
}

fn spawn_ttyd_process(
    program: &Path,
    interface: IpAddr,
    port: u16,
    public_listener: &ListenerSpec,
    mode: TtydMode<'_>,
    profile: &ClientProfile,
    kind: ListenerKind,
) -> Result<TtydProcessRecord> {
    let pid = spawn_detached(spawn_spec(program, interface, port, mode, &profile.args)?)?;
    let address = probe_address(SocketAddr::new(interface, port));
    if !wait_for_address(address, START_TIMEOUT) {
        terminate_pids(&[pid]);
        if matches!(kind, ListenerKind::Writable) {
            wait_for_address_close(address, Duration::from_secs(1));
        }
        return Err(match kind {
            ListenerKind::Writable => WebErr::TtydStartTimeout { address },
            ListenerKind::Broadcast => WebErr::ShareStartTimeout { address },
        });
    }
    Ok(TtydProcessRecord {
        pid,
        port: public_listener.port,
        interface: public_listener.interface.clone(),
        launch_context_scrubbed: true,
        pixel_protocol: profile.pixel_protocol,
        index_key: profile.index_key.clone(),
    })
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

fn ensure_port_available(address: SocketAddr, kind: ListenerKind) -> Result<()> {
    TcpListener::bind(address)
        .map(|_| ())
        .map_err(|_| match kind {
            ListenerKind::Writable => WebErr::ConfiguredPortInUse {
                port: address.port(),
            },
            ListenerKind::Broadcast => WebErr::ConfiguredSharePortInUse {
                port: address.port(),
            },
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

fn record_public_address(record: &WritableDaemonRecord) -> Result<SocketAddr> {
    process_address(&record.process)
}

fn process_address(record: &TtydProcessRecord) -> Result<SocketAddr> {
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

fn state_path(file: &str) -> PathBuf {
    paths::state_home().join("rimz").join(file)
}

fn acquire_lock(file: &str) -> Result<crate::store::lock::WorkspaceLock> {
    Ok(crate::store::lock::WorkspaceLock::acquire(&state_path(
        file,
    ))?)
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

fn write_daemon(record: &WritableDaemonRecord) -> Result<()> {
    write_cache_json(&state_path(DAEMON_FILE), record)
}

fn write_cache_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, value)?;
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Allowlist {
    #[serde(default)]
    sessions: Vec<String>,
}

pub(super) fn add_shared_session(session: &str, config: &MachineConfig) -> Result<WebShareOutcome> {
    let desired = desired_listener(config, config.web.share_port)?;
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    let mut allowlist = read_allowlist()?;
    let previous = allowlist.clone();
    let changed = !allowlist.sessions.iter().any(|shared| shared == session);
    if changed {
        allowlist.sessions.push(session.to_owned());
        normalize_sessions(&mut allowlist.sessions);
        write_allowlist(&allowlist)?;
    }
    let daemon = match ensure_broadcast_locked(config, &desired) {
        Ok(daemon) => daemon,
        Err(err) => {
            if changed && let Err(rollback) = write_allowlist(&previous) {
                tracing::error!(
                    error = &rollback as &dyn std::error::Error,
                    "could not roll back broadcast allowlist after daemon start failed"
                );
            }
            return Err(err);
        }
    };
    let base_url = super::normalized_base_url(config.web.share_base_url.as_deref(), daemon.port);
    Ok(WebShareOutcome {
        payload: WebSharePayload {
            version: "rimz.web.share.v1".to_owned(),
            url: super::join_session_url(&base_url, session),
            session: session.to_owned(),
            port: daemon.port,
        },
        changed,
        warnings: daemon.warnings,
    })
}

pub(super) fn remove_shared_session(
    session: &str,
    config: &MachineConfig,
) -> Result<WebUnshareOutcome> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    let mut allowlist = read_allowlist()?;
    let before = allowlist.sessions.len();
    allowlist.sessions.retain(|shared| shared != session);
    if allowlist.sessions.len() == before {
        return Ok(WebUnshareOutcome {
            changed: false,
            sessions: allowlist.sessions,
            daemon: None,
        });
    }
    write_allowlist(&allowlist)?;
    let daemon = restart_or_stop_broadcast_locked(config, &allowlist)?;
    Ok(WebUnshareOutcome {
        changed: true,
        sessions: allowlist.sessions,
        daemon,
    })
}

pub(super) fn remove_all_shared_sessions() -> Result<WebUnshareOutcome> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    let allowlist = read_allowlist()?;
    let changed = !allowlist.sessions.is_empty();
    if changed {
        write_allowlist(&Allowlist::default())?;
    }
    stop_broadcast_locked()?;
    Ok(WebUnshareOutcome {
        changed,
        sessions: Vec::new(),
        daemon: None,
    })
}

pub(super) fn shared_sessions() -> Result<Vec<String>> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    Ok(read_allowlist()?.sessions)
}

pub(super) fn broadcast_status() -> Result<Option<TtydProcessRecord>> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    broadcast_status_locked()
}

pub(crate) fn pixel_broadcast_record() -> Option<(u32, u32)> {
    let bytes = fs::read(state_path(SHARE_DAEMON_FILE)).ok()?;
    let record = serde_json::from_slice::<TtydProcessRecord>(&bytes).ok()?;
    record.pixel_protocol.map(|protocol| (record.pid, protocol))
}

pub(super) fn restart_broadcast_if_shared(
    config: &MachineConfig,
) -> Result<Option<WebDaemonOutcome>> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    let allowlist = read_allowlist()?;
    if allowlist.sessions.is_empty() {
        stop_broadcast_locked()?;
        return Ok(None);
    }
    let desired = desired_listener(config, config.web.share_port)?;
    restart_broadcast_locked(config, &desired).map(Some)
}

pub(super) fn restart_broadcast_if_online(
    config: &MachineConfig,
) -> Result<Option<WebDaemonOutcome>> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    if broadcast_status_locked()?.is_none() {
        return Ok(None);
    }
    if read_allowlist()?.sessions.is_empty() {
        stop_broadcast_locked()?;
        return Ok(None);
    }
    let desired = desired_listener(config, config.web.share_port)?;
    restart_broadcast_locked(config, &desired).map(Some)
}

pub(super) fn stop_broadcast() -> Result<bool> {
    let _guard = acquire_lock(SHARE_DAEMON_LOCK_FILE)?;
    stop_broadcast_locked()
}

fn restart_or_stop_broadcast_locked(
    config: &MachineConfig,
    allowlist: &Allowlist,
) -> Result<Option<WebDaemonOutcome>> {
    if allowlist.sessions.is_empty() {
        stop_broadcast_locked()?;
        Ok(None)
    } else {
        // Revocation wins over config or restart failures: disconnect the old
        // process before validating and starting the replacement.
        stop_broadcast_locked()?;
        let desired = desired_listener(config, config.web.share_port)?;
        replace_broadcast_locked(config, &desired, None).map(Some)
    }
}

fn ensure_broadcast_locked(
    config: &MachineConfig,
    desired: &ListenerSpec,
) -> Result<WebDaemonOutcome> {
    let daemon = broadcast_status_locked()?;
    let prepared = prepare_broadcast_start(config, desired, daemon.as_ref())?;
    if let Some(record) = daemon.as_ref()
        && record.matches(desired, &prepared.2)
    {
        return Ok(running_broadcast(
            record.clone(),
            desired,
            prepared.2.warnings.clone(),
        ));
    }
    start_broadcast_prepared(desired, daemon, prepared)
}

fn restart_broadcast_locked(
    config: &MachineConfig,
    desired: &ListenerSpec,
) -> Result<WebDaemonOutcome> {
    let daemon = broadcast_status_locked()?;
    replace_broadcast_locked(config, desired, daemon)
}

fn replace_broadcast_locked(
    config: &MachineConfig,
    desired: &ListenerSpec,
    daemon: Option<TtydProcessRecord>,
) -> Result<WebDaemonOutcome> {
    let prepared = prepare_broadcast_start(config, desired, daemon.as_ref())?;
    start_broadcast_prepared(desired, daemon, prepared)
}

fn prepare_broadcast_start(
    config: &MachineConfig,
    desired: &ListenerSpec,
    daemon: Option<&TtydProcessRecord>,
) -> Result<FreshStart> {
    let (program, version) = required_program_with_version()?;
    prepare_fresh_start(
        config,
        desired,
        daemon,
        program,
        &version,
        ListenerKind::Broadcast,
    )
}

fn start_broadcast_prepared(
    desired: &ListenerSpec,
    daemon: Option<TtydProcessRecord>,
    (program, address, profile): FreshStart,
) -> Result<WebDaemonOutcome> {
    if let Some(record) = daemon {
        stop_broadcast_record(&record)?;
    }
    ensure_port_available(address, ListenerKind::Broadcast)?;
    let interface = desired
        .interface
        .parse::<IpAddr>()
        .map_err(|_| WebErr::InvalidInterface {
            value: desired.interface.clone(),
        })?;
    let record = spawn_ttyd_process(
        &program,
        interface,
        desired.port,
        desired,
        TtydMode::Broadcast,
        &profile,
        ListenerKind::Broadcast,
    )?;
    if let Err(err) = write_cache_json(&state_path(SHARE_DAEMON_FILE), &record) {
        let _ = stop_broadcast_record(&record);
        return Err(err);
    }
    Ok(running_broadcast(record, desired, profile.warnings))
}

fn running_broadcast(
    record: TtydProcessRecord,
    desired: &ListenerSpec,
    mut warnings: Vec<WebWarning>,
) -> WebDaemonOutcome {
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
    WebDaemonOutcome {
        pid: record.pid,
        port: record.port,
        interface: record.interface,
        warnings,
    }
}

fn broadcast_status_locked() -> Result<Option<TtydProcessRecord>> {
    let path = state_path(SHARE_DAEMON_FILE);
    let Some(record) = read_json_optional::<TtydProcessRecord>(&path)? else {
        return Ok(None);
    };
    let processes = crate::proc::list_processes();
    let (live, listening) = ttyd_process_status(&record, &processes);
    if live && listening {
        return Ok(Some(record));
    }
    if live {
        terminate_ttyd_process(&record);
    }
    remove_optional(&path)?;
    Ok(None)
}

fn stop_broadcast_locked() -> Result<bool> {
    let Some(record) = broadcast_status_locked()? else {
        return Ok(false);
    };
    stop_broadcast_record(&record)?;
    Ok(true)
}

fn stop_broadcast_record(record: &TtydProcessRecord) -> Result<()> {
    terminate_ttyd_process(record);
    remove_optional(&state_path(SHARE_DAEMON_FILE))?;
    Ok(())
}

fn normalize_sessions(sessions: &mut Vec<String>) {
    sessions.sort();
    sessions.dedup();
}

fn read_allowlist() -> Result<Allowlist> {
    Ok(read_json_optional(&state_path(SHARE_ALLOWLIST_FILE))?.unwrap_or_default())
}

fn write_allowlist(allowlist: &Allowlist) -> Result<()> {
    write_cache_json(&state_path(SHARE_ALLOWLIST_FILE), allowlist)
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

    fn profile_with_index(index_key: Option<&str>) -> ClientProfile {
        ClientProfile {
            index_key: index_key.map(str::to_owned),
            ..ClientProfile::default()
        }
    }

    #[test]
    fn broadcast_argv_has_no_write_or_auth_and_uses_share_shim() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8201,
            TtydMode::Broadcast,
            &["-t".to_owned(), "cursorBlink=false".to_owned()],
        );

        assert_eq!(
            spec.args,
            [
                "-O",
                "-a",
                "-P",
                "3600",
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
        assert!(spec.args.windows(2).any(|args| args == ["-P", "3600"]));
        assert!(!spec.args.iter().any(|arg| arg == "-W"));
        assert!(!spec.args.iter().any(|arg| arg == "-c"));
        assert!(
            super::super::TTYD_AMBIENT_CONTEXT_ENV
                .iter()
                .all(|key| spec.env_remove.contains(*key))
        );
        assert!(spec.env_remove.contains("ZELLIJ_SESSION_NAME"));
        assert!(spec.env_remove.contains("RIMZ_AGENT_KIND"));
    }

    #[test]
    fn allowlist_roundtrips_sorted_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("web-share.json");
        let mut allowlist = Allowlist {
            sessions: vec!["rimz-b".to_owned(), "rimz-a".to_owned()],
        };
        normalize_sessions(&mut allowlist.sessions);
        write_cache_json(&path, &allowlist).expect("write allowlist");

        assert_eq!(
            read_json_optional(&path).expect("read allowlist"),
            Some(Allowlist {
                sessions: vec!["rimz-a".to_owned(), "rimz-b".to_owned()]
            })
        );
    }

    #[test]
    fn broadcast_state_roundtrips_the_launch_context_marker() {
        let daemon = TtydProcessRecord {
            pid: 42,
            port: 8201,
            interface: "0.0.0.0".to_owned(),
            launch_context_scrubbed: true,
            pixel_protocol: Some(crate::web::TTYD_PIXEL_PROTOCOL),
            index_key: Some("generated-index-key".to_owned()),
        };
        let bytes = serde_json::to_vec(&daemon).expect("serialize daemon state");

        assert_eq!(
            serde_json::from_slice::<TtydProcessRecord>(&bytes).expect("parse daemon state"),
            daemon
        );
    }

    #[test]
    fn old_broadcast_state_defaults_to_unscrubbed_launch_context() {
        let daemon: TtydProcessRecord =
            serde_json::from_str(r#"{"pid":42,"port":8201}"#).expect("old record");

        assert!(!daemon.launch_context_scrubbed);
        assert_eq!(daemon.pixel_protocol, None);
        assert_eq!(daemon.index_key, None);
    }

    #[test]
    fn broadcast_reuse_requires_context_marker_and_generated_index_key() {
        let desired = ListenerSpec {
            port: 8201,
            interface: "127.0.0.1".to_owned(),
        };
        let mut daemon = TtydProcessRecord {
            pid: 42,
            port: desired.port,
            interface: desired.interface.clone(),
            launch_context_scrubbed: true,
            pixel_protocol: None,
            index_key: None,
        };

        assert!(daemon.matches(&desired, &profile_with_index(None)));
        daemon.launch_context_scrubbed = false;
        assert!(!daemon.matches(&desired, &profile_with_index(None)));
        daemon.launch_context_scrubbed = true;
        assert!(!daemon.matches(&desired, &profile_with_index(Some("current"))));
        daemon.index_key = Some("current".to_owned());
        assert!(daemon.matches(&desired, &profile_with_index(Some("current"))));
        daemon.index_key = Some("stale".to_owned());
        assert!(!daemon.matches(&desired, &profile_with_index(Some("current"))));
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
            TtydMode::Writable(&credential()),
            &["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
        );
        assert_eq!(
            spec.args,
            [
                "-W",
                "-O",
                "-a",
                "-P",
                "3600",
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
        assert!(spec.args.windows(2).any(|args| args == ["-P", "3600"]));
        assert!(
            super::super::TTYD_AMBIENT_CONTEXT_ENV
                .iter()
                .all(|key| spec.env_remove.contains(*key))
        );
        assert!(spec.env_remove.contains("ZELLIJ_SESSION_NAME"));
        assert!(spec.env_remove.contains("RIMZ_AGENT_KIND"));
    }

    #[test]
    fn argv_uses_basic_auth_for_a_trusted_header_daemon() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            Path::new("/opt/rimz/bin/rimz"),
            "127.0.0.1".parse().expect("IP"),
            8399,
            TtydMode::Writable(&credential()),
            &[],
        );

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
            TtydMode::Writable(&credential()),
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
        let daemon = WritableDaemonRecord {
            process: TtydProcessRecord {
                pid: u32::MAX,
                port: 8200,
                interface: "0.0.0.0".to_owned(),
                launch_context_scrubbed: true,
                pixel_protocol: Some(crate::web::TTYD_PIXEL_PROTOCOL),
                index_key: Some("generated-index-key".to_owned()),
            },
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
        };
        write_cache_json(&path, &daemon).expect("write daemon state");
        assert_eq!(read_json_optional(&path).expect("read state"), Some(daemon));
    }

    #[test]
    fn old_daemon_state_defaults_to_non_reusable_markers() {
        let daemon: WritableDaemonRecord =
            serde_json::from_str(r#"{"pid":42,"port":8200}"#).expect("old record");
        assert_eq!(daemon.process.pid, 42);
        assert_eq!(daemon.auth, WebAuth::Basic);
        assert!(daemon.auth_users.is_empty());
        assert!(daemon.gate.is_none());
        assert!(!daemon.basic_upstream);
        assert!(!daemon.process.launch_context_scrubbed);
        assert_eq!(daemon.process.index_key, None);
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
        let spec = WritableDaemonSpec {
            listener: ListenerSpec {
                port: 8200,
                interface: "0.0.0.0".to_owned(),
            },
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
        let daemon = serde_json::from_str::<WritableDaemonRecord>(r#"{"pid":42,"port":8200}"#)
            .expect("legacy daemon state");

        assert_eq!(daemon.process.pid, 42);
        assert_eq!(daemon.process.port, 8200);
        assert_eq!(daemon.process.pixel_protocol, None);
        assert_eq!(daemon.process.index_key, None);
    }

    #[test]
    fn daemon_reuse_requires_context_marker_and_generated_index_key() {
        let config = MachineConfig::default();
        let desired = desired_spec(&config).expect("desired daemon");
        let mut daemon = WritableDaemonRecord::basic_loopback(42, config.web.port);
        let mut profile = ClientProfile::default();

        assert!(record_matches(&daemon, &desired, &profile));
        daemon.process.launch_context_scrubbed = false;
        assert!(!record_matches(&daemon, &desired, &profile));
        daemon.process.launch_context_scrubbed = true;
        daemon.basic_upstream = false;
        assert!(!record_matches(&daemon, &desired, &profile));
        daemon.basic_upstream = true;
        profile.index_key = Some("current".to_owned());
        assert!(!record_matches(&daemon, &desired, &profile));
        daemon.process.index_key = profile.index_key.clone();
        assert!(record_matches(&daemon, &desired, &profile));
        daemon.process.index_key = Some("stale".to_owned());
        assert!(!record_matches(&daemon, &desired, &profile));
        daemon.process.index_key = profile.index_key.clone();
        daemon.process.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL - 1);
        assert!(record_matches(&daemon, &desired, &profile));
        daemon.process.index_key = None;
        daemon.process.pixel_protocol = None;
        profile.index_key = None;
        assert!(record_matches(&daemon, &desired, &profile));
        daemon.auth_users.push("alice".to_owned());
        assert!(!record_matches(&daemon, &desired, &profile));
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
