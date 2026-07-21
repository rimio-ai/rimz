//! Browser-access lifecycle for Zellij and tmux rooms.

use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::ids::MuxName;
use crate::mux::{CommandSpec, MuxErr};
use crate::store::atomic;

mod colors;
mod fonts;
mod ttyd;
mod zellij;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v1";
pub const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;

#[derive(Debug, thiserror::Error)]
pub enum WebErr {
    #[error(
        "ttyd is required for tmux browser access; install it with `brew install ttyd` or `apt install ttyd`"
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
    #[error("could not parse cached web login token at {path}: {source}")]
    LoginTokenJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("ttyd is not serving tmux session `{0}`; run `rimz web open` or omit `--no-start`")]
    TtydOffline(String),
    #[error(
        "ttyd for tmux session `{session}` did not accept connections on 127.0.0.1:{port} within 5 seconds"
    )]
    TtydStartTimeout { session: String, port: u16 },
    #[error("no free ttyd port in 8200..8299")]
    NoFreeTtydPort,
    #[error(
        "checking zellij web support: `zellij web` is unavailable; install Zellij 0.44.3 or newer with web support: {source}"
    )]
    ZellijUnavailable {
        #[source]
        source: MuxErr,
    },
    #[error("{operation}: {source}")]
    ZellijCommand {
        operation: &'static str,
        #[source]
        source: MuxErr,
    },
    #[error(
        "could not parse `zellij web --status` output; upgrade RimZ or report this output: {raw:?}"
    )]
    ZellijStatus { raw: String },
    #[error("Zellij web server did not report online after start")]
    ZellijStartOffline,
    #[error("Zellij web server is offline; run `rimz web start --daemonize` or omit `--no-start`")]
    ZellijOffline,
    #[error("`zellij web --create-token` output did not contain a token line")]
    MissingLoginToken,
    #[error(
        "ttyd read-only access is per process, not per credential; tmux read-only credentials are not supported"
    )]
    TtydReadOnlyCredential,
    #[error("ttyd credential `{name}` does not exist (the single credential is `rimz`)")]
    TtydCredentialNotFound { name: String },
}

pub type Result<T> = std::result::Result<T, WebErr>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebEngine {
    #[default]
    Zellij,
    Ttyd,
}

impl From<MuxName> for WebEngine {
    fn from(value: MuxName) -> Self {
        match value {
            MuxName::Zellij => Self::Zellij,
            MuxName::Tmux => Self::Ttyd,
        }
    }
}

impl WebEngine {
    pub fn preflight(self) -> Result<()> {
        match self {
            Self::Zellij => zellij::preflight(),
            Self::Ttyd => ttyd::preflight(),
        }
    }

    pub fn open_session(
        self,
        session: &str,
        config: &MachineConfig,
        may_start: bool,
    ) -> Result<WebAccessOutcome> {
        match self {
            Self::Zellij => {
                zellij::open_session(session, config, may_start && config.web.zellij.auto_start)
            }
            Self::Ttyd => {
                ttyd::open_session(session, config, may_start && config.web.tmux.auto_start)
            }
        }
    }

    pub fn inspect_session(self, session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
        match self {
            Self::Zellij => zellij::inspect_session(session, config),
            Self::Ttyd => ttyd::inspect_session(session, config),
        }
    }

    pub fn credential(
        self,
        command: CredentialCommand,
        config: &MachineConfig,
    ) -> Result<CredentialOutcome> {
        match self {
            Self::Zellij => zellij::credential(command, config),
            Self::Ttyd => ttyd::credential(command, config),
        }
    }

    pub fn ensure_credential(self, config: &MachineConfig) -> Result<WebCredential> {
        match self.credential(CredentialCommand::Ensure, config)? {
            CredentialOutcome::Ensured(credential) => Ok(credential),
            // Concrete engine dispatch maps Ensure to this one outcome.
            _ => unreachable!("ensure command returns an ensured credential"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtydStatusInstance {
    pub session: String,
    pub pid: u32,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebOpenPayload {
    pub version: String,
    #[serde(default)]
    pub engine: WebEngine,
    pub url: String,
    pub session: String,
    pub base_url: String,
    pub ip: String,
    pub port: u16,
    pub token_count: usize,
}

impl WebOpenPayload {
    pub fn for_session(
        engine: WebEngine,
        session: impl Into<String>,
        base_url: impl Into<String>,
        ip: impl Into<String>,
        port: u16,
        token_count: usize,
    ) -> Self {
        let session = session.into();
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let url = join_session_url(&base_url, &session);
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            engine,
            url,
            session,
            base_url,
            ip: ip.into(),
            port,
            token_count,
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
    pub base_url: String,
    pub ip: String,
    pub port: u16,
    pub token_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tmux_instances: Vec<TtydStatusInstance>,
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
    pub credential: Option<WebCredential>,
    pub warnings: Vec<WebWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebStatusReport {
    pub zellij_available: bool,
    pub payload: WebStatusPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebStopOutcome {
    pub zellij_stopped: usize,
    pub ttyd_stopped: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebStartOptions {
    pub daemonize: bool,
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PreparedWebStart {
    pub command: CommandSpec,
    pub warnings: Vec<WebWarning>,
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
pub enum WebCredential {
    ZellijLogin { secret: String },
    BasicAuth { username: String, secret: String },
}

impl WebCredential {
    pub fn secret(&self) -> &str {
        match self {
            Self::ZellijLogin { secret } | Self::BasicAuth { secret, .. } => secret,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialSummary {
    pub name: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl From<std::process::Output> for RawCommandOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialOutcome {
    Raw(RawCommandOutput),
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

pub fn status() -> Result<WebStatusReport> {
    let zellij_available = zellij::available();
    let (online, status_base_url, token_count) = if zellij_available {
        let status = zellij::status()?;
        (status.online, status.base_url, zellij::token_count()?)
    } else {
        (false, None, 0)
    };
    let base_url = normalized_base_url(None, status_base_url.as_deref(), zellij::DEFAULT_BASE_URL);
    let endpoint = zellij::endpoint_from_status_base(status_base_url.as_deref());
    Ok(WebStatusReport {
        zellij_available,
        payload: WebStatusPayload {
            version: WEB_SCHEMA_VERSION.to_owned(),
            online,
            base_url,
            ip: endpoint.ip,
            port: endpoint.port,
            token_count,
            tmux_instances: ttyd::status_instances()?,
        },
    })
}

pub fn stop_all() -> Result<WebStopOutcome> {
    let zellij_stopped = if zellij::available() && zellij::status()?.online {
        zellij::stop()?;
        1
    } else {
        0
    };
    Ok(WebStopOutcome {
        zellij_stopped,
        ttyd_stopped: ttyd::stop_all()?,
    })
}

pub fn prepare_zellij_start(
    config: &MachineConfig,
    options: WebStartOptions,
) -> Result<PreparedWebStart> {
    zellij::preflight()?;
    let (config_file, warnings) = zellij::web_client_config_file(config);
    Ok(PreparedWebStart {
        command: zellij::web_start_spec(&options, config_file),
        warnings,
    })
}

pub fn ttyd_diagnostic() -> Result<TtydDiagnostic> {
    let path = ttyd::program()?;
    let version = ttyd::version_at(&path)?;
    Ok(TtydDiagnostic { path, version })
}

pub fn join_session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/{}", encode_path_segment(session))
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

fn normalized_base_url(configured: Option<&str>, fallback: Option<&str>, default: &str) -> String {
    configured
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .or(fallback)
        .unwrap_or(default)
        .trim_end_matches('/')
        .to_owned()
}

fn probe_local_port(port: u16) -> io::Result<()> {
    TcpListener::bind(("127.0.0.1", port)).map(|_| ())
}

fn encode_path_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
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
    fn joins_base_url_and_session_as_one_path_segment() {
        assert_eq!(
            join_session_url("http://127.0.0.1:8082", "rimz-terrain-a1b2c3"),
            "http://127.0.0.1:8082/rimz-terrain-a1b2c3"
        );
        assert_eq!(
            join_session_url("https://devbox.example/zellij/", "rimz-terrain-a1b2c3"),
            "https://devbox.example/zellij/rimz-terrain-a1b2c3"
        );
        assert_eq!(
            join_session_url("https://devbox.example/zellij", "rimz/a b"),
            "https://devbox.example/zellij/rimz%2Fa%20b"
        );
    }

    #[test]
    fn payload_constructor_owns_url_and_legacy_json_defaults_engine() {
        let payload = WebOpenPayload::for_session(
            WebEngine::Ttyd,
            "rimz-test-a1b2c3",
            "http://127.0.0.1:8201/",
            "127.0.0.1",
            8201,
            2,
        );
        assert_eq!(payload.engine, WebEngine::Ttyd);
        assert_eq!(payload.session, "rimz-test-a1b2c3");
        assert_eq!(payload.base_url, "http://127.0.0.1:8201");
        assert_eq!(payload.url, "http://127.0.0.1:8201/rimz-test-a1b2c3");
        assert_eq!(payload.ip, "127.0.0.1");
        assert_eq!(payload.port, 8201);
        assert_eq!(payload.token_count, 2);

        let json = serde_json::to_vec(&payload).expect("json");
        let parsed: WebOpenPayload = serde_json::from_slice(&json).expect("parse");
        assert_eq!(parsed, payload);
        assert!(parsed.version_ok());

        let legacy = serde_json::json!({
            "version": WEB_SCHEMA_VERSION,
            "url": "http://127.0.0.1:8082/rimz-test-a1b2c3",
            "session": "rimz-test-a1b2c3",
            "base_url": "http://127.0.0.1:8082",
            "ip": "127.0.0.1",
            "port": 8082,
            "token_count": 1
        });
        let parsed: WebOpenPayload = serde_json::from_value(legacy).expect("legacy payload");
        assert_eq!(parsed.engine, WebEngine::Zellij);
    }

    #[test]
    fn local_port_derivation_is_stable_and_in_range() {
        let first = derive_local_port("rimz-project-a1b2c3");
        assert_eq!(first, derive_local_port("rimz-project-a1b2c3"));
        assert!(LOCAL_PORT_RANGE.contains(&first));
    }
}
