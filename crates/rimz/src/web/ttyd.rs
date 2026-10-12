//! Writable and broadcast browser daemons, token storage, and process lifecycle.

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
use crate::disk::{atomic, paths};
use crate::mux::CommandSpec;

use super::{
    CredentialSummary, Result, TokenCreated, TokenDaemonChange, TokenRevoked, WebAuth,
    WebCredential, WebDaemonOutcome, WebErr, WebShareOutcome, WebSharePayload, WebUnshareOutcome,
    WebUrlPayload, WebWarning, WebWritableStatus, gate::Cidr,
};

mod client;

use client::ClientProfile;

const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const TOKENS_FILE: &str = "web-tokens.json";
const DAEMON_FILE: &str = "web-ttyd.json";
const DAEMON_LOCK_FILE: &str = "web-ttyd.lock";
const SHARE_ALLOWLIST_FILE: &str = "web-share.json";
const SHARE_DAEMON_FILE: &str = "web-ttyd-share.json";
const SHARE_DAEMON_LOCK_FILE: &str = "web-ttyd-share.lock";
const START_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_TTYD_VERSION: TtydVersion = TtydVersion::new(1, 7, 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TtydVersion {
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
#[serde(deny_unknown_fields)]
struct TtydCredential {
    name: String,
    created_at: Timestamp,
    secret: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenStore {
    version: String,
    upstream: WebCredential,
    tokens: Vec<TtydCredential>,
}

impl TokenStore {
    fn new(tokens: Vec<TtydCredential>) -> Self {
        Self {
            version: super::WEB_TOKENS_VERSION.to_owned(),
            upstream: WebCredential {
                username: "rimz-upstream".to_owned(),
                secret: random_secret(),
            },
            tokens,
        }
    }
}

fn read_tokens() -> Result<Option<TokenStore>> {
    let path = state_path(TOKENS_FILE);
    if let Some(store) = read_json_optional::<TokenStore>(&path)? {
        if store.version != super::WEB_TOKENS_VERSION
            || store.upstream.username != "rimz-upstream"
            || store.upstream.secret.is_empty()
            || store
                .tokens
                .iter()
                .any(|token| !valid_token_name(&token.name) || token.secret.is_empty())
            || store
                .tokens
                .windows(2)
                .any(|pair| pair[0].name >= pair[1].name)
        {
            return Err(WebErr::TtydJson {
                path,
                source: <serde_json::Error as serde::de::Error>::custom("invalid token store"),
            });
        }
        return Ok(Some(store));
    }
    let legacy_path = state_path(CREDENTIAL_FILE);
    let Some(legacy) = read_json_optional::<TtydCredential>(&legacy_path)? else {
        return Ok(None);
    };
    if legacy.name != "rimz" || legacy.secret.is_empty() {
        return Err(WebErr::TtydJson {
            path: legacy_path,
            source: <serde_json::Error as serde::de::Error>::custom("invalid legacy credential"),
        });
    }
    let store = TokenStore::new(vec![legacy]);
    write_tokens(&store)?;
    remove_optional(&legacy_path)?;
    Ok(Some(store))
}

fn valid_token_name(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn write_tokens(store: &TokenStore) -> Result<()> {
    atomic::write_private_temp_then_rename(&state_path(TOKENS_FILE), store)?;
    Ok(())
}

pub(super) fn gate_credentials() -> Result<(WebCredential, Vec<WebCredential>)> {
    let store = read_tokens()?.ok_or(WebErr::TtydCredentialMissing)?;
    Ok((
        store.upstream,
        store.tokens.iter().map(basic_auth).collect(),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TtydProcessRecord {
    pub(super) pid: u32,
    pub(super) port: u16,
    #[serde(default = "default_interface")]
    pub(super) interface: String,
    #[serde(default)]
    launch_context_scrubbed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pixel_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_key: Option<String>,
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
    auth: WebAuth,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    auth_users: Vec<String>,
    #[serde(default)]
    trusted_proxies: Vec<String>,
    #[serde(default)]
    gate: Option<GateRecord>,
    #[serde(default)]
    token_auth: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GateRecord {
    pid: u32,
    upstream_port: u16,
}

impl WritableDaemonRecord {
    fn basic_loopback(pid: u32, port: u16) -> Self {
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
            token_auth: false,
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

fn required_program_with_version() -> Result<(PathBuf, String)> {
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
        if !record.token_auth {
            return Err(WebErr::TtydOffline);
        }
        let credential = required_credential()?;
        Ok(running_daemon(config, record, credential, Vec::new()))
    }
}

pub(super) fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebUrlPayload> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let daemon = daemon_status_locked()?;
    let port = daemon
        .as_ref()
        .map_or(config.web.port, |record| record.process.port);
    let auth = daemon
        .as_ref()
        .map_or_else(|| auth_from_config(config), |record| record.auth.clone());
    let tunnel_port = daemon.as_ref().map(tunnel_port);
    let base_url = super::normalized_base_url(config.web.base_url.as_deref(), port);
    Ok(WebUrlPayload {
        version: super::WEB_URL_VERSION.to_owned(),
        session: session.to_owned(),
        url: super::join_session_url(&base_url, session),
        port,
        tunnel_port,
        auth,
    })
}

pub(super) fn token_summaries() -> Result<Vec<CredentialSummary>> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    Ok(read_tokens()?
        .into_iter()
        .flat_map(|store| store.tokens)
        .map(|credential| CredentialSummary {
            name: credential.name,
            created_at: credential.created_at,
        })
        .collect())
}

pub(super) fn token_credential(name: &str) -> Result<WebCredential> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    token_credential_locked(name)
}

fn token_credential_locked(name: &str) -> Result<WebCredential> {
    let tokens = read_tokens()?.map_or_else(Vec::new, |store| store.tokens);
    tokens
        .iter()
        .find(|token| token.name == name)
        .map(basic_auth)
        .ok_or_else(|| token_not_found(name, &tokens))
}

fn token_not_found(name: &str, tokens: &[TtydCredential]) -> WebErr {
    WebErr::TokenNotFound {
        name: name.to_owned(),
        existing: tokens.iter().map(|token| token.name.clone()).collect(),
    }
}

fn basic_auth(credential: &TtydCredential) -> WebCredential {
    WebCredential {
        username: credential.name.clone(),
        secret: credential.secret.clone(),
    }
}

pub(super) fn authorization_header() -> Result<String> {
    let credential = required_credential()?;
    Ok(credential.authorization())
}

fn mint_credential() -> Result<WebCredential> {
    let record = TtydCredential {
        name: "rimz".to_owned(),
        created_at: Timestamp::now(),
        secret: random_secret(),
    };
    let mut store = read_tokens()?.unwrap_or_else(|| TokenStore::new(Vec::new()));
    store.tokens = vec![record];
    write_tokens(&store)?;
    Ok(store.upstream)
}

fn read_credential() -> Result<Option<WebCredential>> {
    Ok(read_tokens()?.map(|store| store.upstream))
}

fn ensure_credential() -> Result<WebCredential> {
    match read_tokens()? {
        Some(store) if !store.tokens.is_empty() => Ok(store.upstream),
        _ => mint_credential(),
    }
}

pub(super) fn ensure_daemon(config: &MachineConfig) -> Result<WritableDaemon> {
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let daemon = daemon_status_locked()?;
    let prepared = prepare_writable_start(config, &desired, daemon.as_ref(), program, &version)?;
    if let Some(record) = &daemon
        && record_matches(record, &desired, &prepared.2)
        && let Some(store) = read_tokens()?.filter(|store| !store.tokens.is_empty())
    {
        let mut warnings = auth_warnings(&desired);
        warnings.extend(prepared.2.warnings.clone());
        return Ok(running_daemon(
            config,
            record.clone(),
            store.upstream,
            warnings,
        ));
    }
    let credential = ensure_credential()?;
    start_fresh_locked(config, &desired, daemon, credential, prepared)
}

fn running_daemon(
    config: &MachineConfig,
    record: WritableDaemonRecord,
    credential: WebCredential,
    warnings: Vec<WebWarning>,
) -> WritableDaemon {
    let tunnel_port = tunnel_port(&record);
    WritableDaemon {
        outcome: WebDaemonOutcome {
            url: super::normalized_base_url(config.web.base_url.as_deref(), record.process.port),
            pid: record.process.pid,
            gate_pid: record.gate.as_ref().map(|gate| gate.pid),
            port: record.process.port,
            interface: record.process.interface,
            warnings,
        },
        auth: record.auth,
        credential,
        tunnel_port,
    }
}

fn required_credential() -> Result<WebCredential> {
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
    record.token_auth
        && record.process.matches(&desired.listener, profile)
        && record.auth == desired.auth
        && record.auth_users == desired.auth_users
        && record.trusted_proxies == desired.trusted_proxies
        && record.gate.is_some()
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

fn is_ttyd_process(process: &crate::proc::ProcInfo) -> bool {
    crate::proc::command::argv0_label(&process.cmdline) == "ttyd"
}

fn start_daemon_with_profile(
    program: &Path,
    desired: &WritableDaemonSpec,
    credential: &WebCredential,
    profile: &ClientProfile,
) -> Result<WritableDaemonRecord> {
    let ttyd_port = choose_ephemeral_port().map_err(|source| WebErr::GateIo {
        action: "choosing the ttyd upstream port",
        source,
    })?;
    let ttyd_interface = IpAddr::V4(Ipv4Addr::LOCALHOST);
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
    let record = WritableDaemonRecord {
        process,
        auth: desired.auth.clone(),
        auth_users: desired.auth_users.clone(),
        trusted_proxies: desired.trusted_proxies.clone(),
        gate: Some(GateRecord {
            pid: gate_pid,
            upstream_port: ttyd_port,
        }),
        token_auth: true,
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

pub(super) fn create_token(config: &MachineConfig, name: &str) -> Result<TokenCreated> {
    if !valid_token_name(name) {
        return Err(WebErr::InvalidTokenName {
            name: name.to_owned(),
        });
    }
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let mut store = read_tokens()?.unwrap_or_else(|| TokenStore::new(Vec::new()));
    if store.tokens.iter().any(|token| token.name == name) {
        return Err(WebErr::TokenExists {
            name: name.to_owned(),
        });
    }
    let token = TtydCredential {
        name: name.to_owned(),
        created_at: Timestamp::now(),
        secret: random_secret(),
    };
    let credential = basic_auth(&token);
    store.tokens.push(token);
    store.tokens.sort_by(|a, b| a.name.cmp(&b.name));
    let (daemon, warnings) = apply_tokens_locked(config, &mut store)?;
    Ok(TokenCreated {
        credential,
        restarted: daemon == TokenDaemonChange::Restarted,
        warnings,
    })
}

fn apply_tokens_locked(
    config: &MachineConfig,
    store: &mut TokenStore,
) -> Result<(TokenDaemonChange, Vec<WebWarning>)> {
    store.upstream.secret = random_secret();
    let Some(daemon) = daemon_status_locked()? else {
        write_tokens(store)?;
        return Ok((TokenDaemonChange::Offline, Vec::new()));
    };
    stop_record(&daemon)?;
    write_tokens(store)?;
    if store.tokens.is_empty() {
        return Ok((TokenDaemonChange::Stopped, Vec::new()));
    }
    let restart = (|| {
        let desired = desired_spec(config)?;
        let (program, version) = required_program_with_version()?;
        let prepared = prepare_writable_start(config, &desired, None, program, &version)?;
        start_fresh_locked(config, &desired, None, store.upstream.clone(), prepared)
    })();
    match restart {
        Ok(running) => Ok((TokenDaemonChange::Restarted, running.outcome.warnings)),
        Err(err) => Ok((
            TokenDaemonChange::RestartFailed,
            vec![WebWarning::DaemonRestartFailed(format!(
                "tokens saved; the writable daemon is stopped because restart failed: {err}"
            ))],
        )),
    }
}

pub(super) fn restart_daemon(config: &MachineConfig) -> Result<(WritableDaemon, bool)> {
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let daemon = daemon_status_locked()?;
    let was_online = daemon.is_some();
    let prepared = prepare_writable_start(config, &desired, daemon.as_ref(), program, &version)?;
    let credential = ensure_credential()?;
    start_fresh_locked(config, &desired, daemon, credential, prepared)
        .map(|daemon| (daemon, was_online))
}

pub(super) fn restart_if_online(config: &MachineConfig) -> Result<Option<WritableDaemon>> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let Some(daemon) = daemon_status_locked()? else {
        return Ok(None);
    };
    let desired = desired_spec(config)?;
    let (program, version) = required_program_with_version()?;
    let prepared = prepare_writable_start(config, &desired, Some(&daemon), program, &version)?;
    let credential = ensure_credential()?;
    start_fresh_locked(config, &desired, Some(daemon), credential, prepared).map(Some)
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
    config: &MachineConfig,
    desired: &WritableDaemonSpec,
    daemon: Option<WritableDaemonRecord>,
    credential: WebCredential,
    (program, public_address, profile): FreshStart,
) -> Result<WritableDaemon> {
    if let Some(record) = daemon {
        stop_record(&record)?;
    }
    ensure_port_available(public_address, ListenerKind::Writable)?;
    let record = start_daemon_with_profile(&program, desired, &credential, &profile)?;
    let mut warnings = auth_warnings(desired);
    warnings.extend(profile.warnings);
    Ok(running_daemon(config, record, credential, warnings))
}

pub(super) fn revoke_token(config: &MachineConfig, name: Option<&str>) -> Result<TokenRevoked> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let Some(mut store) = read_tokens()? else {
        if let Some(name) = name {
            return Err(token_not_found(name, &[]));
        }
        let daemon = if stop_daemon_locked()? {
            TokenDaemonChange::Stopped
        } else {
            TokenDaemonChange::Offline
        };
        return Ok(TokenRevoked {
            count: 0,
            daemon,
            warnings: Vec::new(),
        });
    };
    let count = if let Some(name) = name {
        let Some(index) = store.tokens.iter().position(|token| token.name == name) else {
            return Err(token_not_found(name, &store.tokens));
        };
        store.tokens.remove(index);
        1
    } else {
        let count = store.tokens.len();
        store.tokens.clear();
        count
    };
    let (daemon, warnings) = apply_tokens_locked(config, &mut store)?;
    Ok(TokenRevoked {
        count,
        daemon,
        warnings,
    })
}

pub(super) fn writable_status(config: &MachineConfig) -> Result<WebWritableStatus> {
    let _guard = acquire_lock(DAEMON_LOCK_FILE)?;
    let daemon = daemon_status_locked()?;
    let port = daemon
        .as_ref()
        .map_or(config.web.port, |record| record.process.port);
    Ok(WebWritableStatus {
        online: daemon.is_some(),
        url: super::normalized_base_url(config.web.base_url.as_deref(), port),
        auth: daemon
            .as_ref()
            .map_or_else(|| auth_from_config(config), |record| record.auth.clone()),
        pid: daemon.as_ref().map(|record| record.process.pid),
        gate_pid: daemon
            .as_ref()
            .and_then(|record| record.gate.as_ref().map(|gate| gate.pid)),
        interface: daemon.as_ref().map_or_else(
            || config.web.interface.clone(),
            |record| record.process.interface.clone(),
        ),
        port,
        tokens: read_tokens()?
            .into_iter()
            .flat_map(|store| store.tokens.into_iter().map(|token| token.name))
            .collect(),
    })
}

pub(super) fn pixel_daemon_record() -> Option<(u32, u32)> {
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
    crate::proc::command::argv0_label(&process.cmdline) == "rimz"
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
fn terminate_pids(pids: &[u32]) {
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
fn terminate_pids(_pids: &[u32]) {}

#[derive(Clone, Copy)]
enum TtydMode<'a> {
    Writable(&'a WebCredential),
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
        spec = spec
            .arg("-c")
            .arg(format!("{}:{}", credential.username, credential.secret));
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

fn wait_for_address(address: SocketAddr, timeout: Duration) -> bool {
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
fn wait_for_address_close(address: SocketAddr, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && TcpStream::connect(address).is_ok() {
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn socket_address(interface: &str, port: u16) -> Result<SocketAddr> {
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

fn probe_address(mut address: SocketAddr) -> SocketAddr {
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
    paths::web_dir().join(file)
}

fn acquire_lock(file: &str) -> Result<crate::disk::lock::WorkspaceLock> {
    Ok(crate::disk::lock::WorkspaceLock::acquire(&state_path(
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

fn write_daemon(record: &WritableDaemonRecord) -> Result<()> {
    write_cache_json(&state_path(DAEMON_FILE), record)
}

fn write_cache_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, value)?;
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
            version: super::WEB_SHARE_VERSION.to_owned(),
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

pub(super) fn pixel_broadcast_record() -> Option<(u32, u32)> {
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
            config,
            record.clone(),
            desired,
            prepared.2.warnings.clone(),
        ));
    }
    start_broadcast_prepared(config, desired, daemon, prepared)
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
    start_broadcast_prepared(config, desired, daemon, prepared)
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
    config: &MachineConfig,
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
    Ok(running_broadcast(config, record, desired, profile.warnings))
}

fn running_broadcast(
    config: &MachineConfig,
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
        url: super::normalized_base_url(config.web.share_base_url.as_deref(), record.port),
        pid: record.pid,
        gate_pid: None,
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
mod tests;
