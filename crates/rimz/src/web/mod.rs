//! Browser access through one machine-wide ttyd daemon.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::ids::MuxName;
use crate::mux::CommandSpec;
use crate::room::session::{LiveSessions, workspace_record_for_session};
use crate::store::atomic;

mod ttyd;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v2";

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebOpenPayload {
    pub version: String,
    pub url: String,
    pub session: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<WebCredential>,
}

impl WebOpenPayload {
    pub fn for_session(
        session: impl Into<String>,
        base_url: impl Into<String>,
        port: u16,
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
    pub pid: u32,
    pub port: u16,
    pub warnings: Vec<WebWarning>,
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
        daemon.credential,
    ))
}

pub fn ensure_daemon(config: &MachineConfig) -> Result<WebDaemonOutcome> {
    let daemon = ttyd::ensure_daemon(config)?;
    Ok(WebDaemonOutcome {
        pid: daemon.pid,
        port: daemon.port,
        warnings: daemon.warnings,
    })
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
    Ok(WebStatusPayload {
        version: WEB_SCHEMA_VERSION.to_owned(),
        online: daemon.is_some(),
        pid: daemon.as_ref().map(|record| record.pid),
        port: daemon.map_or(config.web.port, |record| record.port),
    })
}

pub fn stop_daemon() -> Result<bool> {
    ttyd::stop_daemon()
}

const WEB_ADDRESSABLE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn ensure_session_addressable(mux: MuxName, session: &str) -> Result<()> {
    let backend = crate::mux::backend_for(mux);
    let deadline = Instant::now() + web_addressable_timeout();
    let mut last_error = None;
    loop {
        match backend.list_sessions() {
            Ok(sessions) if sessions.iter().any(|name| name == session) => return Ok(()),
            Ok(_) if Instant::now() >= deadline => {
                return Err(last_error.map_or_else(
                    || WebErr::SessionNotAddressable {
                        mux,
                        session: session.to_owned(),
                    },
                    |detail| WebErr::SessionAddressabilityProbe {
                        mux,
                        session: session.to_owned(),
                        detail,
                    },
                ));
            }
            Err(err) if Instant::now() >= deadline => {
                return Err(WebErr::SessionAddressabilityProbe {
                    mux,
                    session: session.to_owned(),
                    detail: err.to_string(),
                });
            }
            Err(err) => {
                last_error = Some(err.to_string());
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

pub fn existing_session_attach_command(session: Option<&str>) -> Result<CommandSpec> {
    let live = LiveSessions::probe();
    let target = session
        .filter(|session| !session.is_empty())
        .and_then(|session| {
            workspace_record_for_session(session)
                .ok()
                .flatten()
                .and_then(|_| live.mux_of(session).map(|mux| (session, mux)))
        });
    let Some((session, mux)) = target else {
        return Err(WebErr::InvalidSession(invalid_session_message(
            session, &live,
        )?));
    };
    Ok(crate::mux::backend_for(mux).attach_existing_command(session))
}

fn invalid_session_message(session: Option<&str>, live: &LiveSessions) -> Result<String> {
    let mut sessions = crate::workspace::known_workspaces()
        .map_err(|source| WebErr::WorkspaceRecords { source })?
        .into_iter()
        .filter_map(|workspace| {
            live.mux_of(&workspace.session_name)
                .map(|mux| format!("  {} ({mux})", workspace.session_name))
        })
        .collect::<Vec<_>>();
    sessions.sort();
    let requested = session.map_or_else(
        || "no session was provided".to_owned(),
        |session| format!("session `{session}` is not a live RimZ room"),
    );
    let listing = if sessions.is_empty() {
        "  (none)".to_owned()
    } else {
        sessions.join("\n")
    };
    Ok(format!("{requested}\n\nLive RimZ sessions:\n{listing}"))
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
    }
}
