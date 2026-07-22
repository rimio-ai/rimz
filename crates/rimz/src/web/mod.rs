//! Browser access through one machine-wide ttyd daemon.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::ids::MuxName;
use crate::mux::CommandSpec;
use crate::room::session::{LiveSessions, workspace_record_for_session};
use crate::store::atomic;

mod gate;
mod sessions;
mod share;
mod ttyd;

pub use gate::GateAuth;
pub use sessions::{LiveRoom, live_rooms};

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v2";
pub const TTYD_SESSION_OSC: u16 = 7717;
pub(crate) const TTYD_PIXEL_PROTOCOL: u32 = 3;

pub(crate) fn pixel_daemon_records() -> Vec<(u32, u32)> {
    [ttyd::pixel_daemon_record(), share::pixel_daemon_record()]
        .into_iter()
        .flatten()
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum WebErr {
    #[error(
        "ttyd is required for browser access; install it with `brew install ttyd` or `apt install ttyd`"
    )]
    MissingTtyd,
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse ttyd state at {path}: {source}")]
    TtydJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    DaemonLock(#[from] crate::store::lock::LockErr),
    #[error("the shared ttyd daemon is offline; run `rimz web start` or omit `--no-start`")]
    TtydOffline,
    #[error(
        "the shared ttyd daemon credential is missing; run `rimz web token create`, then retry"
    )]
    TtydCredentialMissing,
    #[error("the shared ttyd daemon did not accept connections on {address} within 5 seconds")]
    TtydStartTimeout { address: SocketAddr },
    #[error(
        "[web] port {port} is already in use by another process; choose a free port in `rimz config path`"
    )]
    ConfiguredPortInUse { port: u16 },
    #[error(
        "[web] share_port {port} is already in use by another process; choose a free port in `rimz config path`"
    )]
    ConfiguredSharePortInUse { port: u16 },
    #[error(
        "the read-only broadcast ttyd daemon did not accept connections on {address} within 5 seconds"
    )]
    ShareStartTimeout { address: SocketAddr },
    #[error(
        "ttyd read-only access is per process, not per credential; use `rimz web share` for a read-only broadcast"
    )]
    TtydReadOnlyCredential,
    #[error(
        "[web] interface `{value}` is not an IP address; set it to an IPv4 or IPv6 address in `rimz config path`"
    )]
    InvalidInterface { value: String },
    #[error(
        "[web] auth_users requires trusted-header authentication; set [web] auth_header or remove [web] auth_users"
    )]
    AuthUsersRequireHeader,
    #[error(
        "[web] auth_users contains an empty username; remove it or set it to the IdP's canonical username"
    )]
    EmptyAuthUser,
    #[error(
        "[web] trusted proxy `{value}` is invalid: {reason}; use an IP address or CIDR in `rimz config path`"
    )]
    InvalidTrustedProxy { value: String, reason: String },
    #[error("web proxy gate failed while {action}: {source}")]
    GateIo {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("ttyd credential `{name}` does not exist (the single credential is `rimz`)")]
    TtydCredentialNotFound { name: String },
    #[error(
        "{mux} session `{session}` is not addressable after web preparation. Run `rimz reset` from the workspace, then retry `rimz web open`."
    )]
    SessionNotAddressable { mux: MuxName, session: String },
    #[error(
        "{mux} session `{session}` is not addressable after web preparation: {detail}. Run `rimz reset` from the workspace, then retry `rimz web open`."
    )]
    SessionAddressabilityProbe {
        mux: MuxName,
        session: String,
        detail: String,
    },
    #[error("{0}")]
    InvalidSession(String),
    #[error("reading RimZ workspace records: {source}")]
    WorkspaceRecords {
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, WebErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCredential {
    pub username: String,
    pub secret: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WebAuth {
    #[default]
    Basic,
    TrustedHeader {
        header: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebOpenPayload {
    pub version: String,
    pub url: String,
    pub session: String,
    pub port: u16,
    #[serde(default)]
    pub tunnel_port: Option<u16>,
    #[serde(default)]
    pub auth: WebAuth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<WebCredential>,
}

impl WebOpenPayload {
    pub fn for_session(
        session: impl Into<String>,
        base_url: impl Into<String>,
        port: u16,
        tunnel_port: Option<u16>,
        auth: WebAuth,
        credential: Option<WebCredential>,
    ) -> Self {
        let session = session.into();
        let base_url = base_url.into();
        let url = join_session_url(&base_url, &session);
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            url,
            session,
            port,
            tunnel_port,
            auth,
            credential,
        }
    }

    pub fn version_ok(&self) -> bool {
        self.version == WEB_SCHEMA_VERSION
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebStatusPayload {
    pub version: String,
    pub online: bool,
    pub pid: Option<u32>,
    pub interface: String,
    pub port: u16,
    #[serde(default)]
    pub share: WebShareStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebShareStatus {
    pub online: bool,
    pub pid: Option<u32>,
    pub interface: String,
    pub port: u16,
    #[serde(default)]
    pub sessions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSharePayload {
    pub version: String,
    pub url: String,
    pub session: String,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebWarning {
    BrowserClientSkipped(String),
    BrowserFontSkipped(String),
    BrowserThemeSkipped(String),
    HeaderAuthUnprotected(String),
    BroadcastUnauthenticated(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebAccessOutcome {
    pub payload: WebOpenPayload,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebDaemonOutcome {
    pub pid: u32,
    pub interface: String,
    pub port: u16,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebShareOutcome {
    pub payload: WebSharePayload,
    pub changed: bool,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebUnshareOutcome {
    pub changed: bool,
    pub sessions: Vec<String>,
    pub daemon: Option<WebDaemonOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRestartOutcome {
    pub pid: u32,
    pub interface: String,
    pub port: u16,
    pub was_online: bool,
    pub warnings: Vec<WebWarning>,
    pub share: Option<WebDaemonOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebReloadOutcome {
    pub writable: Option<WebDaemonOutcome>,
    pub share: Option<WebDaemonOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialSummary {
    pub name: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRotation {
    pub credential: WebCredential,
    pub restarted: bool,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtydDiagnostic {
    pub path: PathBuf,
    pub version: String,
}

pub fn preflight() -> Result<()> {
    ttyd::preflight()
}

pub fn open_session(
    session: &str,
    config: &MachineConfig,
    may_start: bool,
) -> Result<WebAccessOutcome> {
    let daemon = ttyd::open_daemon(config, may_start)?;
    let base_url = normalized_base_url(config.web.base_url.as_deref(), daemon.port);
    Ok(WebAccessOutcome {
        payload: WebOpenPayload::for_session(
            session,
            base_url,
            daemon.port,
            Some(daemon.tunnel_port),
            daemon.auth,
            Some(daemon.credential),
        ),
        warnings: daemon.warnings,
    })
}

pub fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
    let daemon = ttyd::inspect_daemon(config)?;
    let base_url = normalized_base_url(config.web.base_url.as_deref(), daemon.port);
    Ok(WebOpenPayload::for_session(
        session,
        base_url,
        daemon.port,
        daemon.tunnel_port,
        daemon.auth,
        daemon.credential,
    ))
}

pub fn ensure_daemon(config: &MachineConfig) -> Result<WebDaemonOutcome> {
    let daemon = ttyd::ensure_daemon(config)?;
    Ok(WebDaemonOutcome {
        pid: daemon.pid,
        interface: daemon.interface,
        port: daemon.port,
        warnings: daemon.warnings,
    })
}

pub fn restart_daemon(config: &MachineConfig) -> Result<WebRestartOutcome> {
    let (daemon, was_online) = ttyd::restart_daemon(config)?;
    let share = share::restart_if_shared(config)?.map(share_daemon_outcome);
    Ok(WebRestartOutcome {
        pid: daemon.pid,
        interface: daemon.interface,
        port: daemon.port,
        was_online,
        warnings: daemon.warnings,
        share,
    })
}

pub fn restart_if_online(config: &MachineConfig) -> Result<WebReloadOutcome> {
    let writable = ttyd::restart_if_online(config)?.map(|daemon| WebDaemonOutcome {
        pid: daemon.pid,
        interface: daemon.interface,
        port: daemon.port,
        warnings: daemon.warnings,
    });
    let share = share::restart_if_online(config)?.map(share_daemon_outcome);
    Ok(WebReloadOutcome { writable, share })
}

pub fn credential_summary() -> Result<Option<CredentialSummary>> {
    ttyd::credential_summary()
}

pub fn rotate_credential(config: &MachineConfig, read_only: bool) -> Result<CredentialRotation> {
    if read_only {
        return Err(WebErr::TtydReadOnlyCredential);
    }
    let rotation = ttyd::rotate_credential(config)?;
    Ok(CredentialRotation {
        credential: rotation.credential,
        restarted: rotation.restarted,
        warnings: rotation.warnings,
    })
}

pub fn gate_authorization() -> Result<String> {
    ttyd::authorization_header()
}

pub fn serve_gate(
    listen: SocketAddr,
    upstream: SocketAddr,
    allow: &[String],
    auth: Option<GateAuth>,
) -> Result<()> {
    let allow = allow
        .iter()
        .map(|value| gate::Cidr::parse(value))
        .collect::<Result<Vec<_>>>()?;
    gate::serve(listen, upstream, allow, auth)
}

pub fn revoke_credential(name: Option<&str>) -> Result<bool> {
    if let Some(name) = name
        && name != "rimz"
    {
        return Err(WebErr::TtydCredentialNotFound {
            name: name.to_owned(),
        });
    }
    ttyd::revoke_credential()
}

pub fn status(config: &MachineConfig) -> Result<WebStatusPayload> {
    let daemon = ttyd::daemon_status()?;
    let share_daemon = share::daemon_status()?;
    let shared_sessions = share::sessions()?;
    Ok(WebStatusPayload {
        version: WEB_SCHEMA_VERSION.to_owned(),
        online: daemon.is_some(),
        pid: daemon.as_ref().map(|record| record.pid),
        interface: daemon.as_ref().map_or_else(
            || config.web.interface.clone(),
            |record| record.interface.clone(),
        ),
        port: daemon.map_or(config.web.port, |record| record.port),
        share: WebShareStatus {
            online: share_daemon.is_some(),
            pid: share_daemon.as_ref().map(|record| record.pid),
            interface: share_daemon.as_ref().map_or_else(
                || config.web.interface.clone(),
                |record| record.interface.clone(),
            ),
            port: share_daemon.map_or(config.web.share_port, |record| record.port),
            sessions: shared_sessions,
        },
    })
}

pub fn stop_daemons() -> Result<usize> {
    Ok(usize::from(ttyd::stop_daemon()?) + usize::from(share::stop_daemon()?))
}

pub fn share_session(session: &str, config: &MachineConfig) -> Result<WebShareOutcome> {
    if live_session_target(Some(session)).is_none() {
        return Err(WebErr::InvalidSession("this room is not shared".to_owned()));
    }
    let (daemon, changed) = share::add_session(session, config)?;
    let base_url = normalized_base_url(config.web.share_base_url.as_deref(), daemon.record.port);
    Ok(WebShareOutcome {
        payload: WebSharePayload {
            version: "rimz.web.share.v1".to_owned(),
            url: join_session_url(&base_url, session),
            session: session.to_owned(),
            port: daemon.record.port,
        },
        changed,
        warnings: daemon.warnings,
    })
}

pub fn unshare_session(session: &str, config: &MachineConfig) -> Result<WebUnshareOutcome> {
    let mutation = share::remove_session(session, config)?;
    Ok(WebUnshareOutcome {
        changed: mutation.changed,
        sessions: mutation.sessions,
        daemon: mutation.daemon.map(share_daemon_outcome),
    })
}

pub fn unshare_all(config: &MachineConfig) -> Result<WebUnshareOutcome> {
    let mutation = share::remove_all(config)?;
    Ok(WebUnshareOutcome {
        changed: mutation.changed,
        sessions: mutation.sessions,
        daemon: mutation.daemon.map(share_daemon_outcome),
    })
}

pub fn shared_sessions() -> Result<Vec<String>> {
    share::sessions()
}

pub fn share_attach_command(session: Option<&str>) -> Result<CommandSpec> {
    let Some(session) = session.filter(|session| !session.is_empty()) else {
        return Err(WebErr::InvalidSession("this room is not shared".to_owned()));
    };
    if !share::contains(session)? {
        return Err(WebErr::InvalidSession("this room is not shared".to_owned()));
    }
    let Some((session, mux)) = live_session_target(Some(session)) else {
        return Err(WebErr::InvalidSession("this room is not shared".to_owned()));
    };
    Ok(crate::mux::backend_for(mux).attach_readonly_command(session))
}

fn share_daemon_outcome(daemon: share::RunningDaemon) -> WebDaemonOutcome {
    WebDaemonOutcome {
        pid: daemon.record.pid,
        interface: daemon.record.interface,
        port: daemon.record.port,
        warnings: daemon.warnings,
    }
}

const WEB_ADDRESSABLE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn ensure_session_addressable(mux: MuxName, session: &str) -> Result<()> {
    let backend = crate::mux::backend_for(mux);
    let deadline = Instant::now() + web_addressable_timeout();
    loop {
        match backend.list_sessions() {
            Ok(sessions) if sessions.iter().any(|name| name == session) => return Ok(()),
            Ok(_) if Instant::now() >= deadline => {
                return Err(WebErr::SessionNotAddressable {
                    mux,
                    session: session.to_owned(),
                });
            }
            Err(err) if Instant::now() >= deadline => {
                return Err(WebErr::SessionAddressabilityProbe {
                    mux,
                    session: session.to_owned(),
                    detail: err.to_string(),
                });
            }
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

pub fn existing_session_attach_command(session: Option<&str>) -> Result<CommandSpec> {
    let live = LiveSessions::probe();
    let target = live_session_target_with_probe(session, &live);
    let Some((session, mux)) = target else {
        let rooms = sessions::live_rooms_with(&live)?;
        return Err(WebErr::InvalidSession(invalid_session_message(
            session, &rooms,
        )));
    };
    Ok(crate::mux::backend_for(mux).attach_existing_command(session))
}

fn live_session_target(session: Option<&str>) -> Option<(&str, MuxName)> {
    let live = LiveSessions::probe();
    live_session_target_with_probe(session, &live)
}

fn live_session_target_with_probe<'a>(
    session: Option<&'a str>,
    live: &LiveSessions,
) -> Option<(&'a str, MuxName)> {
    session
        .filter(|session| !session.is_empty())
        .and_then(|session| {
            workspace_record_for_session(session)
                .ok()
                .flatten()
                .and_then(|_| live.mux_of(session).map(|mux| (session, mux)))
        })
}

fn invalid_session_message(session: Option<&str>, rooms: &[LiveRoom]) -> String {
    let sessions = rooms
        .iter()
        .map(|room| format!("  {} ({})", room.session_name, room.mux))
        .collect::<Vec<_>>();
    let requested = session.map_or_else(
        || "no session was provided".to_owned(),
        |session| format!("session `{session}` is not a live RimZ room"),
    );
    let listing = if sessions.is_empty() {
        "  (none)".to_owned()
    } else {
        sessions.join("\n")
    };
    format!("{requested}\n\nLive RimZ sessions:\n{listing}")
}

fn web_addressable_timeout() -> Duration {
    let Some(value) =
        std::env::var_os("RIMZ_TEST_WEB_ADDRESSABLE_MS").filter(|value| !value.is_empty())
    else {
        return WEB_ADDRESSABLE_TIMEOUT;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(WEB_ADDRESSABLE_TIMEOUT)
}

pub fn ttyd_diagnostic() -> Result<TtydDiagnostic> {
    let path = ttyd::program()?;
    let version = ttyd::version_at(&path)?;
    Ok(TtydDiagnostic { path, version })
}

fn join_session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/?arg={}", encode_query_value(session))
}

fn normalized_base_url(configured: Option<&str>, port: u16) -> String {
    configured
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| url.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| format!("http://127.0.0.1:{port}"))
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_base_url_and_session_as_ttyd_argument() {
        assert_eq!(
            join_session_url("http://127.0.0.1:8200", "rimz-terrain-a1b2c3"),
            "http://127.0.0.1:8200/?arg=rimz-terrain-a1b2c3"
        );
        assert_eq!(
            join_session_url("https://devbox.example/rimz/", "rimz/a b"),
            "https://devbox.example/rimz/?arg=rimz%2Fa%20b"
        );
    }

    #[test]
    fn payload_constructor_roundtrips_v2_credential() {
        let payload = WebOpenPayload::for_session(
            "rimz-test-a1b2c3",
            "http://127.0.0.1:8200/",
            8200,
            Some(8200),
            WebAuth::Basic,
            Some(WebCredential {
                username: "rimz".to_owned(),
                secret: "secret".to_owned(),
            }),
        );
        assert_eq!(payload.url, "http://127.0.0.1:8200/?arg=rimz-test-a1b2c3");
        assert_eq!(
            payload
                .credential
                .as_ref()
                .map(|credential| credential.username.as_str()),
            Some("rimz")
        );

        let json = serde_json::to_vec(&payload).expect("json");
        let parsed: WebOpenPayload = serde_json::from_slice(&json).expect("parse");
        assert_eq!(parsed, payload);
        assert!(parsed.version_ok());
        assert_eq!(parsed.auth, WebAuth::Basic);
        assert_eq!(parsed.tunnel_port, Some(8200));

        let basic: WebOpenPayload = serde_json::from_str(
            r#"{"version":"rimz.web.v2","url":"http://localhost","session":"rimz-test","port":8200}"#,
        )
        .expect("old v2 payload");
        assert_eq!(basic.auth, WebAuth::Basic);
        assert_eq!(basic.tunnel_port, None);
    }

    #[test]
    fn status_share_block_is_additive_to_v2() {
        let old: WebStatusPayload = serde_json::from_str(
            r#"{"version":"rimz.web.v2","online":false,"pid":null,"interface":"127.0.0.1","port":8200}"#,
        )
        .expect("old status payload");
        assert_eq!(old.share, WebShareStatus::default());

        let status = WebStatusPayload {
            version: WEB_SCHEMA_VERSION.to_owned(),
            online: true,
            pid: Some(10),
            interface: "127.0.0.1".to_owned(),
            port: 8200,
            share: WebShareStatus {
                online: true,
                pid: Some(20),
                interface: "127.0.0.1".to_owned(),
                port: 8201,
                sessions: vec!["rimz-test".to_owned()],
            },
        };
        let roundtrip = serde_json::from_slice::<WebStatusPayload>(
            &serde_json::to_vec(&status).expect("serialize status"),
        )
        .expect("parse status");
        assert_eq!(roundtrip, status);
    }
}
