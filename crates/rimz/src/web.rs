//! Zellij web domain logic: URL construction, command argv, status parsing,
//! JSON payloads, token cache, and deterministic local tunnel ports.
//!
//! Process execution and human presentation live in `cli/`; this module owns
//! the structured web data and the machine-local token cache.

use std::env;
use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use kdl::{KdlDocument, KdlEntry, KdlNode};
use serde::{Deserialize, Serialize};

use crate::config::{InlinePalette, parse_hex};
use crate::ledger::{atomic, paths};
use crate::mux::CommandSpec;

pub const WEB_SCHEMA_VERSION: &str = "rimz.web.v1";
pub const DEFAULT_ZELLIJ_WEB_BASE_URL: &str = "http://127.0.0.1:8082";
pub const DEFAULT_ZELLIJ_WEB_IP: &str = "127.0.0.1";
pub const DEFAULT_ZELLIJ_WEB_PORT: u16 = 8082;
pub const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;
const WEB_LOGIN_TOKEN_CACHE_FILE: &str = "web-login-token.json";

/// Binary override for tests, mirroring the Zellij backend.
pub const ZELLIJ_BIN_ENV: &str = "RIMZ_ZELLIJ_BIN";

#[derive(Debug, thiserror::Error)]
pub enum WebLoginTokenCacheErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse cached web login token at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

pub type WebLoginTokenCacheResult<T> = std::result::Result<T, WebLoginTokenCacheErr>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebLoginTokenParseErr {
    #[error("`zellij web --create-token` output did not contain a token line")]
    MissingToken,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WebLoginTokenCache {
    token: String,
    created: Timestamp,
}

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
    pub config_file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebClientColors {
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub cursor_accent: (u8, u8, u8),
    pub normal: [(u8, u8, u8); 8],
    pub bright: [(u8, u8, u8); 8],
    pub selection_background: Option<(u8, u8, u8)>,
    pub selection_foreground: Option<(u8, u8, u8)>,
}

impl WebClientColors {
    /// Build Zellij web-client colors from an Alacritty palette. Missing
    /// optional colors fall back to terminal conventions; malformed provided
    /// colors return `None` so callers can skip browser theming without
    /// discarding the user's Zellij config.
    pub fn from_palette(palette: &InlinePalette) -> Option<Self> {
        let primary = palette.primary.as_ref()?;
        let normal = palette.normal.as_ref()?;
        let background = parse_required_color(primary.background.as_deref())?;
        let foreground = parse_required_color(primary.foreground.as_deref())?;
        let normal = [
            parse_optional_color(normal.black.as_deref())?.unwrap_or(background),
            parse_required_color(normal.red.as_deref())?,
            parse_required_color(normal.green.as_deref())?,
            parse_required_color(normal.yellow.as_deref())?,
            parse_required_color(normal.blue.as_deref())?,
            parse_required_color(normal.magenta.as_deref())?,
            parse_required_color(normal.cyan.as_deref())?,
            parse_optional_color(normal.white.as_deref())?.unwrap_or(foreground),
        ];
        let bright = match palette.bright.as_ref() {
            Some(bright) => [
                parse_optional_color(bright.black.as_deref())?.unwrap_or(normal[0]),
                parse_optional_color(bright.red.as_deref())?.unwrap_or(normal[1]),
                parse_optional_color(bright.green.as_deref())?.unwrap_or(normal[2]),
                parse_optional_color(bright.yellow.as_deref())?.unwrap_or(normal[3]),
                parse_optional_color(bright.blue.as_deref())?.unwrap_or(normal[4]),
                parse_optional_color(bright.magenta.as_deref())?.unwrap_or(normal[5]),
                parse_optional_color(bright.cyan.as_deref())?.unwrap_or(normal[6]),
                parse_optional_color(bright.white.as_deref())?.unwrap_or(normal[7]),
            ],
            None => normal,
        };
        let cursor = palette.cursor.as_ref();
        let cursor_color =
            parse_optional_color(cursor.and_then(|c| c.cursor.as_deref()))?.unwrap_or(foreground);
        let cursor_accent =
            parse_optional_color(cursor.and_then(|c| c.text.as_deref()))?.unwrap_or(background);
        let selection = palette.selection.as_ref();
        Some(Self {
            background,
            foreground,
            cursor: cursor_color,
            cursor_accent,
            normal,
            bright,
            selection_background: parse_optional_color(
                selection.and_then(|s| s.background.as_deref()),
            )?,
            selection_foreground: parse_optional_color(selection.and_then(|s| s.text.as_deref()))?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebTokenCommand {
    Create { read_only: bool },
    List,
    Revoke { name: String },
    RevokeAll,
}

pub fn zellij_program() -> String {
    std::env::var(ZELLIJ_BIN_ENV).unwrap_or_else(|_| "zellij".to_owned())
}

pub fn read_cached_login_token() -> WebLoginTokenCacheResult<Option<String>> {
    read_cached_login_token_at(&web_login_token_cache_path())
        .map(|record| record.map(|record| record.token))
}

pub fn cache_login_token(token: &str) -> WebLoginTokenCacheResult<()> {
    cache_login_token_at(&web_login_token_cache_path(), token)
}

pub fn clear_cached_login_token() -> WebLoginTokenCacheResult<()> {
    clear_cached_login_token_at(&web_login_token_cache_path())
}

pub fn parse_created_login_token(
    stdout: &[u8],
) -> std::result::Result<String, WebLoginTokenParseErr> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .find_map(|line| {
            let (name, token) = line.split_once(':')?;
            let suffix = name.trim().strip_prefix("token_")?;
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let token = token.trim();
            (!token.is_empty()).then(|| token.to_owned())
        })
        .ok_or(WebLoginTokenParseErr::MissingToken)
}

pub fn active_zellij_config_path() -> Option<PathBuf> {
    config_file_from_env()
        .or_else(config_dir_from_env)
        .or_else(home_zellij_config)
        .or_else(platform_zellij_config)
        .or_else(system_zellij_config)
}

fn web_login_token_cache_path() -> PathBuf {
    paths::state_home()
        .join("rimz")
        .join(WEB_LOGIN_TOKEN_CACHE_FILE)
}

fn read_cached_login_token_at(path: &Path) -> WebLoginTokenCacheResult<Option<WebLoginTokenCache>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(WebLoginTokenCacheErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let record = serde_json::from_slice(&bytes).map_err(|source| WebLoginTokenCacheErr::Json {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(record))
}

fn cache_login_token_at(path: &Path, token: &str) -> WebLoginTokenCacheResult<()> {
    let record = WebLoginTokenCache {
        token: token.to_owned(),
        created: Timestamp::now(),
    };
    atomic::write_private_temp_then_rename(path, &record)?;
    Ok(())
}

fn clear_cached_login_token_at(path: &Path) -> WebLoginTokenCacheResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                atomic::sync_dir(parent)?;
            }
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WebLoginTokenCacheErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn merge_web_client_config(
    existing: Option<&str>,
    font: &str,
    colors: &WebClientColors,
) -> std::result::Result<String, String> {
    let mut document = match existing {
        Some(text) => text
            .parse::<KdlDocument>()
            .map_err(|err| format!("parsing Zellij config KDL: {err}"))?,
        None => KdlDocument::new(),
    };
    document
        .nodes_mut()
        .retain(|node| node.name().value() != "web_client");
    document.nodes_mut().push(web_client_node(font, colors));
    document.fmt();
    Ok(document.to_string())
}

pub fn web_help_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--help"])
}

pub fn web_status_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--status"])
}

pub fn web_start_spec(opts: &WebStartOptions) -> CommandSpec {
    let mut spec = CommandSpec::new(zellij_program()).args(["web", "--start"]);
    if let Some(path) = &opts.config_file {
        spec = spec.env("ZELLIJ_CONFIG_FILE", path.display().to_string());
    }
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
        WebTokenCommand::Create { read_only } => {
            spec = spec.arg(if *read_only {
                "--create-read-only-token"
            } else {
                "--create-token"
            });
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
        .filter_map(token_name_from_line)
        .count()
}

fn token_name_from_line(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    Some(line.split_once(':').map_or(line, |(name, _)| name).trim())
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

fn web_client_node(font: &str, colors: &WebClientColors) -> KdlNode {
    let mut node = KdlNode::new("web_client");
    let children = node.ensure_children();
    let mut font_node = KdlNode::new("font");
    font_node.push(KdlEntry::new(font.to_owned()));
    children.nodes_mut().push(font_node);

    let mut theme_node = KdlNode::new("theme");
    let theme = theme_node.ensure_children();
    push_color_node(theme, "background", colors.background);
    push_color_node(theme, "foreground", colors.foreground);
    for (name, rgb) in NORMAL_WEB_COLOR_NAMES
        .iter()
        .copied()
        .zip(colors.normal.iter().copied())
    {
        push_color_node(theme, name, rgb);
    }
    for (name, rgb) in BRIGHT_WEB_COLOR_NAMES
        .iter()
        .copied()
        .zip(colors.bright.iter().copied())
    {
        push_color_node(theme, name, rgb);
    }
    push_color_node(theme, "cursor", colors.cursor);
    push_color_node(theme, "cursor_accent", colors.cursor_accent);
    if let Some(rgb) = colors.selection_background {
        push_color_node(theme, "selection_background", rgb);
    }
    if let Some(rgb) = colors.selection_foreground {
        push_color_node(theme, "selection_foreground", rgb);
    }
    children.nodes_mut().push(theme_node);
    node
}

fn push_color_node(document: &mut KdlDocument, name: &str, (red, green, blue): (u8, u8, u8)) {
    let mut node = KdlNode::new(name);
    node.push(KdlEntry::new(i64::from(red)));
    node.push(KdlEntry::new(i64::from(green)));
    node.push(KdlEntry::new(i64::from(blue)));
    document.nodes_mut().push(node);
}

fn parse_required_color(value: Option<&str>) -> Option<(u8, u8, u8)> {
    parse_hex(value?).ok()
}

fn parse_optional_color(value: Option<&str>) -> Option<Option<(u8, u8, u8)>> {
    match value {
        Some(value) => parse_hex(value).ok().map(Some),
        None => Some(None),
    }
}

const NORMAL_WEB_COLOR_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

const BRIGHT_WEB_COLOR_NAMES: [&str; 8] = [
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

fn config_file_from_env() -> Option<PathBuf> {
    env_path("ZELLIJ_CONFIG_FILE").and_then(existing_file)
}

fn config_dir_from_env() -> Option<PathBuf> {
    env_path("ZELLIJ_CONFIG_DIR")
        .map(|dir| dir.join("config.kdl"))
        .and_then(existing_file)
}

fn home_zellij_config() -> Option<PathBuf> {
    env_path("HOME")
        .map(|home| home.join(".config").join("zellij").join("config.kdl"))
        .and_then(existing_file)
}

fn platform_zellij_config() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        macos_zellij_config()
    } else {
        xdg_zellij_config()
    }
}

fn xdg_zellij_config() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME")
        .map(|home| home.join("zellij").join("config.kdl"))
        .and_then(existing_file)
}

fn macos_zellij_config() -> Option<PathBuf> {
    env_path("HOME")
        .map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("org.Zellij-Contributors.Zellij")
                .join("config.kdl")
        })
        .and_then(existing_file)
}

fn system_zellij_config() -> Option<PathBuf> {
    existing_file(PathBuf::from("/etc/zellij/config.kdl"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    let value = env::var_os(key)?;
    (!value.as_os_str().is_empty()).then(|| PathBuf::from(value))
}

fn existing_file(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use crate::config::{InlineAnsiColors, InlinePalette, InlinePrimaryColors};

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
    fn counts_token_lines_from_zellij_list_output() {
        let stdout = b"rimz-project-a1b2c3: created at 2026-07-05 09:00:00\nwatch: created at 2026-07-05 10:11:12\nlegacy-token\n\n";

        assert_eq!(parse_token_count(stdout), 3);
    }

    #[test]
    fn parses_created_login_token_from_zellij_output() {
        let stdout =
            b"Created token successfully\n\ntoken_1: d2d9a2b9-9861-43b3-960b-b5292ac0407b\n";

        assert_eq!(
            parse_created_login_token(stdout).expect("token parses"),
            "d2d9a2b9-9861-43b3-960b-b5292ac0407b"
        );
        assert_eq!(
            parse_created_login_token(b"Created token successfully\n\ntoken_1: rimz-tok-123\nnote: still use the token line\n").expect("token parses"),
            "rimz-tok-123"
        );
        assert_eq!(
            parse_created_login_token(b"Created token successfully\n").unwrap_err(),
            WebLoginTokenParseErr::MissingToken
        );
    }

    #[test]
    fn login_token_cache_round_trips_and_clears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rimz/web-login-token.json");

        assert!(
            read_cached_login_token_at(&path)
                .expect("missing cache reads")
                .is_none()
        );

        cache_login_token_at(&path, "rimz-tok-123").expect("cache token");
        let record = read_cached_login_token_at(&path)
            .expect("read cache")
            .expect("cache exists");
        assert_eq!(record.token, "rimz-tok-123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        clear_cached_login_token_at(&path).expect("clear cache");
        assert!(!path.exists());
        clear_cached_login_token_at(&path).expect("clearing a missing cache is ok");
    }

    #[test]
    fn builds_zellij_web_argv() {
        let start = web_start_spec(&WebStartOptions {
            daemonize: true,
            ip: Some("127.0.0.1".to_owned()),
            port: Some(8082),
            cert: Some("/cert.pem".to_owned()),
            key: Some("/key.pem".to_owned()),
            config_file: Some(PathBuf::from("/zellij-web.kdl")),
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
        assert_eq!(
            start.env.get("ZELLIJ_CONFIG_FILE").map(String::as_str),
            Some("/zellij-web.kdl")
        );

        let token = web_token_spec(&WebTokenCommand::Create { read_only: true });
        assert_eq!(token.args, ["web", "--create-read-only-token"]);
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

    #[test]
    fn web_client_colors_apply_terminal_fallbacks() {
        let colors = WebClientColors::from_palette(&minimal_palette()).expect("colors");
        assert_eq!(colors.background, (0x01, 0x02, 0x03));
        assert_eq!(colors.foreground, (0xfa, 0xfb, 0xfc));
        assert_eq!(colors.normal[0], colors.background);
        assert_eq!(colors.normal[7], colors.foreground);
        assert_eq!(colors.bright, colors.normal);
        assert_eq!(colors.cursor, colors.foreground);
        assert_eq!(colors.cursor_accent, colors.background);
        assert_eq!(colors.selection_background, None);
        assert_eq!(colors.selection_foreground, None);
    }

    #[test]
    fn web_client_colors_reject_missing_or_malformed_required_colors() {
        let mut missing = minimal_palette();
        missing.normal.as_mut().expect("normal").green = None;
        assert_eq!(WebClientColors::from_palette(&missing), None);

        let mut malformed = minimal_palette();
        malformed.bright = Some(InlineAnsiColors {
            red: Some("not-hex".to_owned()),
            ..InlineAnsiColors::default()
        });
        assert_eq!(WebClientColors::from_palette(&malformed), None);
    }

    #[test]
    fn merge_web_client_config_builds_standalone_theme() {
        let rendered = merge_web_client_config(None, "JetBrains \"Mono\"", &sample_web_colors())
            .expect("merge");
        assert!(
            rendered.contains("font \"JetBrains \\\"Mono\\\"\""),
            "font is emitted as escaped KDL string: {rendered}"
        );
        let doc: KdlDocument = rendered.parse().expect("parse rendered");
        assert_eq!(doc.nodes().len(), 1);
        let web_client = doc.get("web_client").expect("web_client");
        let children = web_client.children().expect("web_client children");
        assert_eq!(
            children.get_arg("font").and_then(|value| value.as_string()),
            Some("JetBrains \"Mono\"")
        );
        let theme = children
            .get("theme")
            .and_then(KdlNode::children)
            .expect("theme children");
        assert_eq!(color_args(theme, "background"), [1, 2, 3]);
        assert_eq!(color_args(theme, "black"), [10, 11, 12]);
        assert_eq!(color_args(theme, "bright_white"), [87, 88, 89]);
        assert_eq!(color_args(theme, "cursor_accent"), [7, 8, 9]);
        assert!(theme.get("selection_background").is_none());
        assert!(theme.get("selection_foreground").is_none());
    }

    #[test]
    fn merge_web_client_config_preserves_foreign_nodes_and_replaces_stale_web_client() {
        let mut colors = sample_web_colors();
        colors.selection_background = Some((90, 91, 92));
        colors.selection_foreground = Some((93, 94, 95));
        let rendered = merge_web_client_config(
            Some("web_server true\nweb_client {\n  font \"old\"\n}\n"),
            "JetBrainsMono Nerd Font Mono",
            &colors,
        )
        .expect("merge");
        let doc: KdlDocument = rendered.parse().expect("parse rendered");
        assert_eq!(
            doc.nodes()
                .iter()
                .filter(|node| node.name().value() == "web_client")
                .count(),
            1
        );
        assert_eq!(
            doc.get_arg("web_server").and_then(|value| value.as_bool()),
            Some(true)
        );
        let theme = doc
            .get("web_client")
            .and_then(KdlNode::children)
            .and_then(|children| children.get("theme"))
            .and_then(KdlNode::children)
            .expect("theme children");
        assert_eq!(color_args(theme, "selection_background"), [90, 91, 92]);
        assert_eq!(color_args(theme, "selection_foreground"), [93, 94, 95]);
        assert!(!rendered.contains("old"));
    }

    #[test]
    fn merge_web_client_config_rejects_invalid_existing_kdl() {
        assert!(merge_web_client_config(Some("{"), "font", &sample_web_colors()).is_err());
    }

    fn minimal_palette() -> InlinePalette {
        InlinePalette {
            primary: Some(InlinePrimaryColors {
                background: Some("#010203".to_owned()),
                foreground: Some("#fafbfc".to_owned()),
            }),
            normal: Some(InlineAnsiColors {
                red: Some("#111213".to_owned()),
                green: Some("#212223".to_owned()),
                yellow: Some("#313233".to_owned()),
                blue: Some("#414243".to_owned()),
                magenta: Some("#515253".to_owned()),
                cyan: Some("#616263".to_owned()),
                ..InlineAnsiColors::default()
            }),
            ..InlinePalette::default()
        }
    }

    fn sample_web_colors() -> WebClientColors {
        WebClientColors {
            background: (1, 2, 3),
            foreground: (4, 5, 6),
            cursor: (5, 6, 7),
            cursor_accent: (7, 8, 9),
            normal: [
                (10, 11, 12),
                (20, 21, 22),
                (30, 31, 32),
                (40, 41, 42),
                (50, 51, 52),
                (60, 61, 62),
                (70, 71, 72),
                (80, 81, 82),
            ],
            bright: [
                (17, 18, 19),
                (27, 28, 29),
                (37, 38, 39),
                (47, 48, 49),
                (57, 58, 59),
                (67, 68, 69),
                (77, 78, 79),
                (87, 88, 89),
            ],
            selection_background: None,
            selection_foreground: None,
        }
    }

    fn color_args(document: &KdlDocument, name: &str) -> [i64; 3] {
        let values: Vec<i64> = document
            .get_args(name)
            .into_iter()
            .map(|value| value.as_i64().expect("integer color entry"))
            .collect();
        values.try_into().expect("three color entries")
    }
}
