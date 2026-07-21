//! Browser access through one machine-wide ttyd daemon.

use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::store::atomic;

mod colors;
mod fonts;
mod ttyd;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v2";
pub const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;

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
        "the shared ttyd daemon did not accept connections on 127.0.0.1:{port} within 5 seconds"
    )]
    TtydStartTimeout { port: u16 },
    #[error(
        "[web] port {port} is already in use by another process; choose a free port in `rimz config path`"
    )]
    ConfiguredPortInUse { port: u16 },
    #[error(
        "ttyd read-only access is per process, not per credential; read-only credentials are not supported"
    )]
    TtydReadOnlyCredential,
    #[error("ttyd credential `{name}` does not exist (the single credential is `rimz`)")]
    TtydCredentialNotFound { name: String },
}

pub type Result<T> = std::result::Result<T, WebErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCredential {
    pub username: String,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebOpenPayload {
    pub version: String,
    pub url: String,
    pub session: String,
    pub port: u16,
    pub credential: WebCredential,
}

impl WebOpenPayload {
    pub fn for_session(
        session: impl Into<String>,
        base_url: impl Into<String>,
        port: u16,
        credential: WebCredential,
    ) -> Self {
        let session = session.into();
        let base_url = base_url.into();
        let url = join_session_url(&base_url, &session);
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            url,
            session,
            port,
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
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebWarning {
    BrowserClientSkipped(String),
    BrowserFontSkipped(String),
    BrowserThemeSkipped(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebAccessOutcome {
    pub payload: WebOpenPayload,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebDaemonOutcome {
    pub record: DaemonRecord,
    pub credential: WebCredential,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRecord {
    pub pid: u32,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebStopOutcome {
    pub stopped: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialCommand {
    Create { read_only: bool },
    List,
    Revoke { name: String },
    RevokeAll,
    Ensure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialSummary {
    pub name: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialOutcome {
    Ensured(WebCredential),
    Rotated {
        credential: WebCredential,
        restarted_instances: usize,
        warnings: Vec<WebWarning>,
    },
    Listed(Vec<CredentialSummary>),
    Revoked {
        stopped_instances: usize,
    },
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
    ttyd::open_session(session, config, may_start)
}

pub fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
    ttyd::inspect_session(session, config)
}

pub fn ensure_daemon(config: &MachineConfig) -> Result<WebDaemonOutcome> {
    let (record, credential, warnings) = ttyd::ensure_daemon(config)?;
    Ok(WebDaemonOutcome {
        record,
        credential,
        warnings,
    })
}

pub fn credential(command: CredentialCommand, config: &MachineConfig) -> Result<CredentialOutcome> {
    ttyd::credential(command, config)
}

pub fn status(config: &MachineConfig) -> Result<WebStatusPayload> {
    let daemon = ttyd::daemon_status()?;
    Ok(WebStatusPayload {
        version: WEB_SCHEMA_VERSION.to_owned(),
        online: daemon.is_some(),
        pid: daemon.as_ref().map(|record| record.pid),
        port: daemon.map_or(config.web.port, |record| record.port),
    })
}

pub fn stop_all() -> Result<WebStopOutcome> {
    Ok(WebStopOutcome {
        stopped: usize::from(ttyd::stop_daemon()?),
    })
}

pub fn ttyd_diagnostic() -> Result<TtydDiagnostic> {
    let path = ttyd::program()?;
    let version = ttyd::version_at(&path)?;
    Ok(TtydDiagnostic { path, version })
}

pub fn join_session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/?arg={}", encode_query_value(session))
}

pub fn local_tunnel_url(remote: &WebOpenPayload, local_port: u16) -> String {
    join_session_url(&format!("http://127.0.0.1:{local_port}"), &remote.session)
}

pub fn derive_port(session: &str, range: &RangeInclusive<u16>) -> u16 {
    let span = u32::from(*range.end()) - u32::from(*range.start()) + 1;
    let offset = crc32fast::hash(session.as_bytes()) % span;
    // Modulo span bounds offset to the inclusive u16 port range.
    *range.start() + u16::try_from(offset).expect("offset fits in port range")
}

pub fn derive_local_port(session: &str) -> u16 {
    derive_port(session, &LOCAL_PORT_RANGE)
}

pub fn choose_local_port(session: &str, override_port: Option<u16>) -> io::Result<u16> {
    if let Some(port) = override_port {
        probe_local_port(port)?;
        return Ok(port);
    }
    let preferred = derive_local_port(session);
    for port in port_scan(preferred, &LOCAL_PORT_RANGE) {
        if probe_local_port(port).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free local web tunnel port in 8300..8399",
    ))
}

pub fn port_scan(preferred: u16, range: &RangeInclusive<u16>) -> impl Iterator<Item = u16> + use<> {
    let start = *range.start();
    let end = *range.end();
    (preferred..=end).chain(start..preferred)
}

fn normalized_base_url(configured: Option<&str>, port: u16) -> String {
    configured
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| url.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| format!("http://127.0.0.1:{port}"))
}

fn probe_local_port(port: u16) -> io::Result<()> {
    TcpListener::bind(("127.0.0.1", port)).map(|_| ())
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
            WebCredential {
                username: "rimz".to_owned(),
                secret: "secret".to_owned(),
            },
        );
        assert_eq!(payload.url, "http://127.0.0.1:8200/?arg=rimz-test-a1b2c3");
        assert_eq!(payload.credential.username, "rimz");

        let json = serde_json::to_vec(&payload).expect("json");
        let parsed: WebOpenPayload = serde_json::from_slice(&json).expect("parse");
        assert_eq!(parsed, payload);
        assert!(parsed.version_ok());
    }

    #[test]
    fn local_port_derivation_is_stable_and_in_range() {
        let first = derive_local_port("rimz-project-a1b2c3");
        assert_eq!(first, derive_local_port("rimz-project-a1b2c3"));
        assert!(LOCAL_PORT_RANGE.contains(&first));
    }
}
