//! Browser-access domain logic shared by the Zellij and tmux web engines.

use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::ids::MuxName;

pub mod ttyd;
mod zellij;

pub use zellij::*;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v1";
pub const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;

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
    pub fn new(
        url: String,
        session: String,
        base_url: String,
        endpoint: ZellijWebEndpoint,
        token_count: usize,
    ) -> Self {
        Self::for_engine(
            WebEngine::Zellij,
            url,
            session,
            base_url,
            endpoint.ip,
            endpoint.port,
            token_count,
        )
    }

    pub fn for_engine(
        engine: WebEngine,
        url: String,
        session: String,
        base_url: String,
        ip: String,
        port: u16,
        token_count: usize,
    ) -> Self {
        Self {
            version: WEB_SCHEMA_VERSION.to_owned(),
            engine,
            url,
            session,
            base_url,
            ip,
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
            tmux_instances: Vec::new(),
        }
    }
}

pub fn join_session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/{}", encode_path_segment(session))
}

pub fn parse_web_open_payload(stdout: &[u8]) -> Result<WebOpenPayload, serde_json::Error> {
    serde_json::from_slice(stdout)
}

pub fn derive_port(session: &str, range: &RangeInclusive<u16>) -> u16 {
    let span = u32::from(*range.end()) - u32::from(*range.start()) + 1;
    let offset = crc32fast::hash(session.as_bytes()) % span;
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
    fn web_json_round_trips_and_defaults_legacy_engine() {
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
