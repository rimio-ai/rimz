//! Zellij web domain logic: URL construction, command argv, status parsing,
//! JSON payloads, and deterministic local tunnel ports.
//!
//! Process I/O and human presentation live in `cli/`; this module is pure
//! except for the local bind probe used to find an available tunnel port.

use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::mux::CommandSpec;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v1";
pub const DEFAULT_ZELLIJ_WEB_BASE_URL: &str = "http://127.0.0.1:8082";
pub const DEFAULT_ZELLIJ_WEB_IP: &str = "127.0.0.1";
pub const DEFAULT_ZELLIJ_WEB_PORT: u16 = 8082;
pub const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;

/// Binary override for tests, mirroring the Zellij backend.
pub const ZELLIJ_BIN_ENV: &str = "RIMZ_ZELLIJ_BIN";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedWebStatus {
    Recognized(WebServerStatus),
    Unrecognized { raw: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebServerStatus {
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebOpenPayload {
    pub version: String,
    pub url: String,
    pub session: String,
    pub base_url: String,
    pub ip: String,
    pub port: u16,
    pub token_count: usize,
}

impl WebOpenPayload {
    pub fn new(
        url: String,
        session: String,
        base_url: String,
        endpoint: ZellijWebEndpoint,
        token_count: usize,
    ) -> Self {
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            url,
            session,
            base_url,
            ip: endpoint.ip,
            port: endpoint.port,
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
}

impl WebStatusPayload {
    pub fn new(
        online: bool,
        base_url: String,
        endpoint: ZellijWebEndpoint,
        token_count: usize,
    ) -> Self {
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            online,
            base_url,
            ip: endpoint.ip,
            port: endpoint.port,
            token_count,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijWebEndpoint {
    pub ip: String,
    pub port: u16,
}

impl Default for ZellijWebEndpoint {
    fn default() -> Self {
        Self {
            ip: DEFAULT_ZELLIJ_WEB_IP.to_owned(),
            port: DEFAULT_ZELLIJ_WEB_PORT,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebStartOptions {
    pub daemonize: bool,
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebTokenCommand {
    Create {
        read_only: bool,
        name: Option<String>,
    },
    List,
    Revoke {
        name: String,
    },
    RevokeAll,
}

pub fn zellij_program() -> String {
    std::env::var(ZELLIJ_BIN_ENV).unwrap_or_else(|_| "zellij".to_owned())
}

pub fn web_help_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--help"])
}

pub fn web_status_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--status"])
}

pub fn web_start_spec(opts: &WebStartOptions) -> CommandSpec {
    let mut spec = CommandSpec::new(zellij_program()).args(["web", "--start"]);
    if opts.daemonize {
        spec = spec.arg("--daemonize");
    }
    if let Some(ip) = &opts.ip {
        spec = spec.args(["--ip".to_owned(), ip.clone()]);
    }
    if let Some(port) = opts.port {
        spec = spec.args(["--port".to_owned(), port.to_string()]);
    }
    if let Some(cert) = &opts.cert {
        spec = spec.args(["--cert".to_owned(), cert.clone()]);
    }
    if let Some(key) = &opts.key {
        spec = spec.args(["--key".to_owned(), key.clone()]);
    }
    spec
}

pub fn web_stop_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--stop"])
}

pub fn web_token_spec(command: &WebTokenCommand) -> CommandSpec {
    let mut spec = CommandSpec::new(zellij_program()).arg("web");
    match command {
        WebTokenCommand::Create { read_only, name } => {
            spec = spec.arg(if *read_only {
                "--create-read-only-token"
            } else {
                "--create-token"
            });
            if let Some(name) = name {
                spec = spec.args(["--token-name".to_owned(), name.clone()]);
            }
        }
        WebTokenCommand::List => spec = spec.arg("--list-tokens"),
        WebTokenCommand::Revoke { name } => {
            spec = spec.args(["--revoke-token".to_owned(), name.clone()]);
        }
        WebTokenCommand::RevokeAll => spec = spec.arg("--revoke-all-tokens"),
    }
    spec
}

pub fn join_session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/{}", encode_path_segment(session))
}

pub fn effective_base_url(configured: Option<&str>, status: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .or(status)
        .unwrap_or(DEFAULT_ZELLIJ_WEB_BASE_URL)
        .trim_end_matches('/')
        .to_owned()
}

pub fn parse_status(stdout: &[u8]) -> ParsedWebStatus {
    let raw = String::from_utf8_lossy(stdout).trim().to_owned();
    let lower = raw.to_ascii_lowercase();
    let base_url = checked_url(&raw);
    if lower.contains("web server is offline") {
        return ParsedWebStatus::Recognized(WebServerStatus {
            online: false,
            base_url,
        });
    }
    if lower.contains("web server online") || lower.contains("web server is online") {
        return ParsedWebStatus::Recognized(WebServerStatus {
            online: true,
            base_url,
        });
    }
    ParsedWebStatus::Unrecognized { raw }
}

pub fn parse_token_count(stdout: &[u8]) -> usize {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .count()
}

pub fn parse_web_open_payload(stdout: &[u8]) -> Result<WebOpenPayload, serde_json::Error> {
    serde_json::from_slice(stdout)
}

pub fn endpoint_from_status_base(status_base_url: Option<&str>) -> ZellijWebEndpoint {
    status_base_url
        .and_then(endpoint_from_url)
        .unwrap_or_default()
}

pub fn derive_local_port(session: &str) -> u16 {
    let span = u32::from(*LOCAL_PORT_RANGE.end()) - u32::from(*LOCAL_PORT_RANGE.start()) + 1;
    let offset = crc32fast::hash(session.as_bytes()) % span;
    *LOCAL_PORT_RANGE.start() + u16::try_from(offset).expect("offset fits in port range")
}

pub fn choose_local_port(session: &str, override_port: Option<u16>) -> io::Result<u16> {
    if let Some(port) = override_port {
        probe_local_port(port)?;
        return Ok(port);
    }
    let preferred = derive_local_port(session);
    for port in port_scan(preferred) {
        if probe_local_port(port).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free local Zellij web tunnel port in 8300..8399",
    ))
}

fn probe_local_port(port: u16) -> io::Result<()> {
    TcpListener::bind(("127.0.0.1", port)).map(|_| ())
}

fn port_scan(preferred: u16) -> impl Iterator<Item = u16> {
    let start = *LOCAL_PORT_RANGE.start();
    let end = *LOCAL_PORT_RANGE.end();
    (preferred..=end).chain(start..preferred)
}

fn checked_url(raw: &str) -> Option<String> {
    raw.rsplit_once("Checked:")
        .or_else(|| raw.rsplit_once("checked:"))
        .map(|(_, url)| url.trim().to_owned())
        .filter(|url| !url.is_empty())
}

fn endpoint_from_url(url: &str) -> Option<ZellijWebEndpoint> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some(after_open) = authority.strip_prefix('[') {
        let (host, rest) = after_open.split_once(']')?;
        let port = rest.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()),
            None => (authority, None),
        }
    };
    Some(ZellijWebEndpoint {
        ip: host.to_owned(),
        port: port.unwrap_or_else(|| default_port_for_scheme(scheme)),
    })
}

fn default_port_for_scheme(scheme: &str) -> u16 {
    match scheme {
        "https" => 443,
        "http" => 80,
        _ => DEFAULT_ZELLIJ_WEB_PORT,
    }
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
    fn parses_zellij_status_output() {
        assert_eq!(
            parse_status(b"Web server is offline, checked: http://127.0.0.1:8082\n"),
            ParsedWebStatus::Recognized(WebServerStatus {
                online: false,
                base_url: Some("http://127.0.0.1:8082".to_owned()),
            })
        );
        assert_eq!(
            parse_status(
                b"Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n"
            ),
            ParsedWebStatus::Recognized(WebServerStatus {
                online: true,
                base_url: Some("http://127.0.0.1:8082".to_owned()),
            })
        );
        assert!(matches!(
            parse_status(b"something else\n"),
            ParsedWebStatus::Unrecognized { .. }
        ));
    }

    #[test]
    fn builds_zellij_web_argv() {
        let start = web_start_spec(&WebStartOptions {
            daemonize: true,
            ip: Some("127.0.0.1".to_owned()),
            port: Some(8082),
            cert: Some("/cert.pem".to_owned()),
            key: Some("/key.pem".to_owned()),
        });
        assert_eq!(
            start.args,
            [
                "web",
                "--start",
                "--daemonize",
                "--ip",
                "127.0.0.1",
                "--port",
                "8082",
                "--cert",
                "/cert.pem",
                "--key",
                "/key.pem"
            ]
        );

        let token = web_token_spec(&WebTokenCommand::Create {
            read_only: true,
            name: Some("watch".to_owned()),
        });
        assert_eq!(
            token.args,
            ["web", "--create-read-only-token", "--token-name", "watch"]
        );
    }

    #[test]
    fn web_json_round_trips_and_version_checks() {
        let payload = WebOpenPayload::new(
            "http://127.0.0.1:8082/rimz-test-a1b2c3".to_owned(),
            "rimz-test-a1b2c3".to_owned(),
            "http://127.0.0.1:8082".to_owned(),
            ZellijWebEndpoint::default(),
            2,
        );
        let json = serde_json::to_vec(&payload).expect("json");
        let parsed = parse_web_open_payload(&json).expect("parse");
        assert_eq!(parsed, payload);
        assert!(parsed.version_ok());
    }

    #[test]
    fn local_port_derivation_is_stable_and_in_range() {
        let a = derive_local_port("rimz-project-a1b2c3");
        let b = derive_local_port("rimz-project-a1b2c3");
        assert_eq!(a, b);
        assert!(LOCAL_PORT_RANGE.contains(&a));
    }

    #[test]
    fn endpoint_parsing_keeps_zellij_defaults() {
        assert_eq!(
            endpoint_from_status_base(Some("http://127.0.0.1:8082")),
            ZellijWebEndpoint {
                ip: "127.0.0.1".to_owned(),
                port: 8082,
            }
        );
        assert_eq!(
            endpoint_from_status_base(Some("https://[::1]:9443/zellij")),
            ZellijWebEndpoint {
                ip: "::1".to_owned(),
                port: 9443,
            }
        );
    }
}
