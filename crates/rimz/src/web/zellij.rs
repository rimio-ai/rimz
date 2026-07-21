//! Zellij web lifecycle, credential cache, status parsing, and client config.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::MachineConfig;
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::colors::WebClientColors;
use super::{
    CredentialCommand, CredentialOutcome, RawCommandOutput, Result, WebAccessOutcome,
    WebCredential, WebEngine, WebErr, WebOpenPayload, WebStartOptions, WebWarning,
    normalized_base_url,
};

pub(super) const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8082";
const DEFAULT_IP: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8082;
const WEB_LOGIN_TOKEN_CACHE_FILE: &str = "web-login-token.json";
const ZELLIJ_BIN_ENV: &str = "RIMZ_ZELLIJ_BIN";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WebLoginTokenCache {
    token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedWebStatus {
    Recognized(WebServerStatus),
    Unrecognized { raw: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WebServerStatus {
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ZellijWebEndpoint {
    pub(super) ip: String,
    pub(super) port: u16,
}

impl Default for ZellijWebEndpoint {
    fn default() -> Self {
        Self {
            ip: DEFAULT_IP.to_owned(),
            port: DEFAULT_PORT,
        }
    }
}

fn zellij_program() -> String {
    std::env::var(ZELLIJ_BIN_ENV).unwrap_or_else(|_| "zellij".to_owned())
}

pub(super) fn preflight() -> Result<()> {
    web_help_spec()
        .run()
        .map(|_| ())
        .map_err(|source| WebErr::ZellijUnavailable { source })
}

pub(super) fn available() -> bool {
    web_help_spec().run().is_ok()
}

pub(super) fn open_session(
    session: &str,
    config: &MachineConfig,
    may_start: bool,
) -> Result<WebAccessOutcome> {
    preflight()?;
    let mut web_status = status()?;
    let mut warnings = Vec::new();
    if !web_status.online {
        if may_start {
            let (config_file, config_warnings) = web_client_config_file(config);
            warnings = config_warnings;
            let output = web_start_spec(
                &WebStartOptions {
                    daemonize: true,
                    ..WebStartOptions::default()
                },
                config_file,
            )
            .run()
            .map_err(|source| WebErr::ZellijCommand {
                operation: "starting zellij web server",
                source,
            })?;
            if !output.stdout.is_empty() || !output.stderr.is_empty() {
                tracing::debug!(
                    stdout = %String::from_utf8_lossy(&output.stdout).trim(),
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "zellij web start output",
                );
            }
            web_status = status()?;
            if !web_status.online {
                return Err(WebErr::ZellijStartOffline);
            }
        } else {
            return Err(WebErr::ZellijOffline);
        }
    }
    Ok(WebAccessOutcome {
        payload: payload(session, config, &web_status)?,
        credential: None,
        warnings,
    })
}

pub(super) fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
    preflight()?;
    let status = status()?;
    payload(session, config, &status)
}

fn payload(
    session: &str,
    config: &MachineConfig,
    status: &WebServerStatus,
) -> Result<WebOpenPayload> {
    let base_url = normalized_base_url(
        config.web.zellij.base_url.as_deref(),
        status.base_url.as_deref(),
        DEFAULT_BASE_URL,
    );
    let endpoint = endpoint_from_status_base(status.base_url.as_deref());
    Ok(WebOpenPayload::for_session(
        WebEngine::Zellij,
        session,
        base_url,
        endpoint.ip,
        endpoint.port,
        token_count()?,
    ))
}

pub(super) fn status() -> Result<WebServerStatus> {
    let output = web_status_spec()
        .run()
        .map_err(|source| WebErr::ZellijCommand {
            operation: "reading web status",
            source,
        })?;
    match parse_status(&output.stdout) {
        ParsedWebStatus::Recognized(status) => Ok(status),
        ParsedWebStatus::Unrecognized { raw } => Err(WebErr::ZellijStatus { raw }),
    }
}

pub(super) fn token_count() -> Result<usize> {
    let output = web_token_spec(&CredentialCommand::List)
        .run()
        .map_err(|source| WebErr::ZellijCommand {
            operation: "listing tokens",
            source,
        })?;
    Ok(parse_token_count(&output.stdout))
}

pub(super) fn stop() -> Result<()> {
    web_stop_spec()
        .run()
        .map(|_| ())
        .map_err(|source| WebErr::ZellijCommand {
            operation: "stopping zellij web server",
            source,
        })
}

pub(super) fn credential(
    command: CredentialCommand,
    _config: &MachineConfig,
) -> Result<CredentialOutcome> {
    if command == CredentialCommand::Ensure {
        return Ok(CredentialOutcome::Ensured(WebCredential::ZellijLogin {
            secret: ensure_login_token()?,
        }));
    }
    preflight()?;
    let operation = match &command {
        CredentialCommand::Create { .. } => "creating token",
        CredentialCommand::List => "listing tokens",
        CredentialCommand::Revoke { .. } => "revoking token",
        CredentialCommand::RevokeAll => "revoking all tokens",
        // Ensure returns above before operation labeling.
        CredentialCommand::Ensure => unreachable!(),
    };
    let clears_cache = matches!(
        &command,
        CredentialCommand::Revoke { .. } | CredentialCommand::RevokeAll
    );
    let output = web_token_spec(&command)
        .run()
        .map_err(|source| WebErr::ZellijCommand { operation, source })?;
    if clears_cache {
        clear_cached_login_token()?;
    }
    Ok(CredentialOutcome::Raw(RawCommandOutput::from(output)))
}

fn ensure_login_token() -> Result<String> {
    if let Some(token) = read_cached_login_token()? {
        return Ok(token);
    }
    preflight()?;
    let output = web_token_spec(&CredentialCommand::Create { read_only: false })
        .run()
        .map_err(|source| WebErr::ZellijCommand {
            operation: "creating token",
            source,
        })?;
    let token = parse_created_login_token(&output.stdout)?;
    cache_login_token(&token)?;
    Ok(token)
}

fn read_cached_login_token() -> Result<Option<String>> {
    read_cached_login_token_at(&web_login_token_cache_path())
        .map(|record| record.map(|record| record.token))
}

fn cache_login_token(token: &str) -> Result<()> {
    cache_login_token_at(&web_login_token_cache_path(), token)
}

fn clear_cached_login_token() -> Result<()> {
    clear_cached_login_token_at(&web_login_token_cache_path())
}

fn parse_created_login_token(stdout: &[u8]) -> Result<String> {
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
        .ok_or(WebErr::MissingLoginToken)
}

fn active_zellij_config_path() -> Option<PathBuf> {
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

fn read_cached_login_token_at(path: &Path) -> Result<Option<WebLoginTokenCache>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(WebErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let record = serde_json::from_slice(&bytes).map_err(|source| WebErr::LoginTokenJson {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(record))
}

fn cache_login_token_at(path: &Path, token: &str) -> Result<()> {
    let record = WebLoginTokenCache {
        token: token.to_owned(),
    };
    atomic::write_private_temp_then_rename(path, &record)?;
    Ok(())
}

fn clear_cached_login_token_at(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                atomic::sync_dir(parent)?;
            }
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WebErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn merge_web_client_config(
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

fn web_help_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--help"])
}

fn web_status_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--status"])
}

pub(super) fn web_start_spec(opts: &WebStartOptions, config_file: Option<PathBuf>) -> CommandSpec {
    let mut spec = CommandSpec::new(zellij_program()).args(["web", "--start"]);
    if let Some(path) = config_file {
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

fn web_stop_spec() -> CommandSpec {
    CommandSpec::new(zellij_program()).args(["web", "--stop"])
}

fn web_token_spec(command: &CredentialCommand) -> CommandSpec {
    let mut spec = CommandSpec::new(zellij_program()).arg("web");
    match command {
        CredentialCommand::Create { read_only } => {
            spec = spec.arg(if *read_only {
                "--create-read-only-token"
            } else {
                "--create-token"
            });
        }
        CredentialCommand::List => spec = spec.arg("--list-tokens"),
        CredentialCommand::Revoke { name } => {
            spec = spec.args(["--revoke-token".to_owned(), name.clone()]);
        }
        CredentialCommand::RevokeAll => spec = spec.arg("--revoke-all-tokens"),
        // Ensure is handled by credential() and never becomes upstream argv.
        CredentialCommand::Ensure => unreachable!("ensure uses create-token after cache read"),
    }
    spec
}

fn parse_status(stdout: &[u8]) -> ParsedWebStatus {
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

fn parse_token_count(stdout: &[u8]) -> usize {
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

pub(super) fn endpoint_from_status_base(status_base_url: Option<&str>) -> ZellijWebEndpoint {
    status_base_url
        .and_then(endpoint_from_url)
        .unwrap_or_default()
}

fn checked_url(raw: &str) -> Option<String> {
    raw.rsplit_once("Checked:")
        .or_else(|| raw.rsplit_once("checked:"))
        .map(|(_, url)| url.trim().to_owned())
        .filter(|url| !url.is_empty())
}

fn endpoint_from_url(url: &str) -> Option<ZellijWebEndpoint> {
    if url.starts_with("http:///") || url.starts_with("https:///") {
        return None;
    }
    let parsed = Url::parse(url)
        .ok()
        .filter(|url| url.host_str().is_some())
        .or_else(|| {
            (!url.contains("://"))
                .then(|| Url::parse(&format!("http://{url}")).ok())
                .flatten()
                .filter(|url| url.host_str().is_some())
        })?;
    let host = parsed.host_str()?.trim_matches(['[', ']']);
    Some(ZellijWebEndpoint {
        ip: host.to_owned(),
        port: parsed.port_or_known_default().unwrap_or(DEFAULT_PORT),
    })
}

pub(super) fn web_client_config_file(config: &MachineConfig) -> (Option<PathBuf>, Vec<WebWarning>) {
    if !config.web.enabled || !config.web.zellij.style_client {
        return (None, Vec::new());
    }
    let colors = match WebClientColors::from_palette(&crate::config::resolve_inline_palette(
        &config.theme,
    )) {
        Some(colors) => colors,
        None => {
            return (
                None,
                vec![WebWarning::BrowserThemeSkipped(
                    "scheme palette is incomplete or malformed".to_owned(),
                )],
            );
        }
    };
    let existing = match active_zellij_config_path() {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(err) => {
                return (
                    None,
                    vec![WebWarning::BrowserThemeSkipped(format!(
                        "could not read Zellij config `{}`: {err}",
                        path.display()
                    ))],
                );
            }
        },
        None => None,
    };
    let kdl = match merge_web_client_config(existing.as_deref(), &config.web.zellij.font, &colors) {
        Ok(kdl) => kdl,
        Err(err) => return (None, vec![WebWarning::BrowserThemeSkipped(err)]),
    };
    let path = paths::state_home()
        .join("rimz")
        .join("zellij-web-config.kdl");
    if let Err(err) = atomic::write_bytes_atomically(&path, kdl.as_bytes()) {
        return (
            None,
            vec![WebWarning::BrowserThemeSkipped(format!(
                "could not write generated Zellij config `{}`: {err}",
                path.display()
            ))],
        );
    }
    (Some(path), Vec::new())
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
    use super::*;
    use crate::config::{InlineAnsiColors, InlinePalette, InlinePrimaryColors};

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
        assert!(matches!(
            parse_created_login_token(b"Created token successfully\n"),
            Err(WebErr::MissingLoginToken)
        ));
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
        let start = web_start_spec(
            &WebStartOptions {
                daemonize: true,
                ip: Some("127.0.0.1".to_owned()),
                port: Some(8082),
                cert: Some("/cert.pem".to_owned()),
                key: Some("/key.pem".to_owned()),
            },
            Some(PathBuf::from("/zellij-web.kdl")),
        );
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

        let token = web_token_spec(&CredentialCommand::Create { read_only: true });
        assert_eq!(token.args, ["web", "--create-read-only-token"]);
    }

    #[test]
    fn endpoint_parsing_keeps_url_and_fallback_contract() {
        let cases = [
            ("http://127.0.0.1:8082", "127.0.0.1", 8082),
            ("http://web.example", "web.example", 80),
            ("https://web.example", "web.example", 443),
            ("https://[::1]:9443/zellij", "::1", 9443),
            ("https://[::1]/zellij", "::1", 443),
            ("https://user:pass@web.example/path", "web.example", 443),
            ("web.example:8090/path", "web.example", 8090),
        ];
        for (url, ip, port) in cases {
            assert_eq!(
                endpoint_from_status_base(Some(url)),
                ZellijWebEndpoint {
                    ip: ip.to_owned(),
                    port,
                },
                "{url}"
            );
        }
        for malformed in ["http:///missing-host", "http://["] {
            assert_eq!(
                endpoint_from_status_base(Some(malformed)),
                ZellijWebEndpoint::default(),
                "{malformed}"
            );
        }
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
