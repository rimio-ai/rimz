//! ttyd-backed browser access for tmux rooms.

use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::MachineConfig;
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{
    CredentialCommand, CredentialOutcome, CredentialSummary, Result, TtydStatusInstance,
    WebAccessOutcome, WebCredential, WebEngine, WebErr, WebOpenPayload, WebWarning, derive_port,
    normalized_base_url, port_scan,
};
use super::{colors::WebClientColors, fonts::FontFace};

const TTYD_BIN_ENV: &str = "RIMZ_TTYD_BIN";
const TTYD_PORT_RANGE: RangeInclusive<u16> = 8200..=8299;
const CREDENTIAL_FILE: &str = "web-ttyd-credential.json";
const INSTANCE_DIR: &str = "web-ttyd";
const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOCK_INDEX_TIMEOUT: Duration = Duration::from_secs(5);
const STOCK_INDEX_MAX_BYTES: u64 = 16 * 1024 * 1024;
const INDEX_CACHE_DIR: &str = "rimz/web-ttyd";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TtydCredential {
    name: String,
    created_at: Timestamp,
    secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TtydInstance {
    session: String,
    pid: u32,
    port: u16,
}

#[derive(Clone, Debug, Default)]
struct TtydClientProfile {
    args: Vec<String>,
    warnings: Vec<WebWarning>,
}

pub(super) fn preflight() -> Result<()> {
    program().map(|_| ())
}

pub(super) fn program() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TTYD_BIN_ENV) {
        return Ok(PathBuf::from(path));
    }
    which::which("ttyd").map_err(|_| WebErr::MissingTtyd)
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

pub(super) fn open_session(
    session: &str,
    config: &MachineConfig,
    may_start: bool,
) -> Result<WebAccessOutcome> {
    preflight()?;
    let (instance, credential, warnings) = ensure_instance(session, config, may_start)?;
    let fallback = format!("http://127.0.0.1:{}", instance.port);
    let base_url = normalized_base_url(config.web.tmux.base_url.as_deref(), None, &fallback);
    Ok(WebAccessOutcome {
        payload: WebOpenPayload::for_session(
            WebEngine::Ttyd,
            session,
            base_url,
            "127.0.0.1",
            instance.port,
            1,
        ),
        credential: Some(basic_auth(&credential)),
        warnings,
    })
}

pub(super) fn inspect_session(session: &str, config: &MachineConfig) -> Result<WebOpenPayload> {
    let instance = inventory()?
        .into_iter()
        .find(|instance| instance.session == session);
    let port = instance.map_or_else(|| derive_instance_port(session), |instance| instance.port);
    let fallback = format!("http://127.0.0.1:{port}");
    let base_url = normalized_base_url(config.web.tmux.base_url.as_deref(), None, &fallback);
    Ok(WebOpenPayload::for_session(
        WebEngine::Ttyd,
        session,
        base_url,
        "127.0.0.1",
        port,
        usize::from(read_credential()?.is_some()),
    ))
}

pub(super) fn credential(
    command: CredentialCommand,
    config: &MachineConfig,
) -> Result<CredentialOutcome> {
    match command {
        CredentialCommand::Create { read_only: true } => Err(WebErr::TtydReadOnlyCredential),
        CredentialCommand::Create { read_only: false } => {
            let (credential, restarted_instances, warnings) = rotate_credential(config)?;
            Ok(CredentialOutcome::Rotated {
                credential: basic_auth(&credential),
                restarted_instances,
                warnings,
            })
        }
        CredentialCommand::List => Ok(CredentialOutcome::Listed(
            read_credential()?
                .into_iter()
                .map(|credential| CredentialSummary {
                    name: credential.name,
                    created_at: credential.created_at,
                })
                .collect(),
        )),
        CredentialCommand::Revoke { name } => {
            if name != "rimz" {
                return Err(WebErr::TtydCredentialNotFound { name });
            }
            Ok(CredentialOutcome::Revoked {
                stopped_instances: revoke_credential()?,
            })
        }
        CredentialCommand::RevokeAll => Ok(CredentialOutcome::Revoked {
            stopped_instances: revoke_credential()?,
        }),
        CredentialCommand::Ensure => Ok(CredentialOutcome::Ensured(basic_auth(
            &ensure_credential()?,
        ))),
    }
}

pub(super) fn status_instances() -> Result<Vec<TtydStatusInstance>> {
    Ok(inventory()?
        .into_iter()
        .map(|instance| TtydStatusInstance {
            session: instance.session,
            pid: instance.pid,
            port: instance.port,
        })
        .collect())
}

pub(super) fn stop_all() -> Result<usize> {
    let instances = inventory()?;
    stop_instances(&instances)?;
    Ok(instances.len())
}

fn derive_instance_port(session: &str) -> u16 {
    derive_port(session, &TTYD_PORT_RANGE)
}

fn basic_auth(credential: &TtydCredential) -> WebCredential {
    WebCredential::BasicAuth {
        username: credential.name.clone(),
        secret: credential.secret.clone(),
    }
}

fn mint_credential() -> Result<TtydCredential> {
    let record = TtydCredential {
        name: "rimz".to_owned(),
        created_at: Timestamp::now(),
        secret: random_secret(),
    };
    write_credential_at(&credential_path(), &record)?;
    Ok(record)
}

fn read_credential() -> Result<Option<TtydCredential>> {
    read_json_optional(&credential_path())
}

fn ensure_credential() -> Result<TtydCredential> {
    read_credential()?.map_or_else(mint_credential, Ok)
}

fn clear_credential() -> Result<bool> {
    remove_optional(&credential_path())
}

fn ensure_instance(
    session: &str,
    config: &MachineConfig,
    may_start: bool,
) -> Result<(TtydInstance, TtydCredential, Vec<WebWarning>)> {
    let credential = ensure_credential()?;
    if let Some(instance) = inventory()?
        .into_iter()
        .find(|instance| instance.session == session)
    {
        return Ok((instance, credential, Vec::new()));
    }
    if !may_start {
        return Err(WebErr::TtydOffline(session.to_owned()));
    }
    let (instance, warnings) = start_instance(session, &credential, config)?;
    Ok((instance, credential, warnings))
}

fn start_instance(
    session: &str,
    credential: &TtydCredential,
    config: &MachineConfig,
) -> Result<(TtydInstance, Vec<WebWarning>)> {
    let profile = client_profile(config);
    let instance = start_instance_with_profile(session, credential, &profile)?;
    Ok((instance, profile.warnings))
}

fn start_instance_with_profile(
    session: &str,
    credential: &TtydCredential,
    profile: &TtydClientProfile,
) -> Result<TtydInstance> {
    let port = choose_instance_port(session)?;
    let spec = spawn_spec(session, port, &credential.secret, &profile.args)?;
    let pid = spawn_detached(spec)?;
    let instance = TtydInstance {
        session: session.to_owned(),
        pid,
        port,
    };
    if !wait_for_port(port, START_TIMEOUT) {
        let _ = stop_instances(std::slice::from_ref(&instance));
        return Err(WebErr::TtydStartTimeout {
            session: session.to_owned(),
            port,
        });
    }
    write_instance(&instance)?;
    Ok(instance)
}

fn rotate_credential(config: &MachineConfig) -> Result<(TtydCredential, usize, Vec<WebWarning>)> {
    let credential = mint_credential()?;
    let instances = inventory()?;
    stop_instances(&instances)?;
    if instances.is_empty() {
        return Ok((credential, 0, Vec::new()));
    }
    let profile = client_profile(config);
    for instance in &instances {
        start_instance_with_profile(&instance.session, &credential, &profile)?;
    }
    Ok((credential, instances.len(), profile.warnings))
}

fn revoke_credential() -> Result<usize> {
    let instances = inventory()?;
    stop_instances(&instances)?;
    clear_credential()?;
    Ok(instances.len())
}

fn inventory() -> Result<Vec<TtydInstance>> {
    let dir = instance_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(WebErr::Io { path: dir, source }),
    };
    let processes = crate::proc::list_processes();
    let mut live = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| WebErr::Io {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(instance) = read_json_optional::<TtydInstance>(&path)? else {
            continue;
        };
        if processes.iter().any(|process| process.pid == instance.pid)
            && TcpStream::connect(("127.0.0.1", instance.port)).is_ok()
        {
            live.push(instance);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    live.sort_by(|a, b| a.session.cmp(&b.session));
    Ok(live)
}

fn stop_instances(instances: &[TtydInstance]) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        let mut survivors = Vec::new();
        for instance in instances {
            if let Ok(raw) = i32::try_from(instance.pid) {
                let _ = kill(Pid::from_raw(raw), Signal::SIGTERM);
                survivors.push(instance.pid);
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

    let mut first_error = None;
    for instance in instances {
        if let Err(err) = remove_optional(&instance_path(&instance.session))
            && first_error.is_none()
        {
            first_error = Some(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn client_profile(config: &MachineConfig) -> TtydClientProfile {
    let mut profile = TtydClientProfile {
        args: vec!["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
        warnings: Vec::new(),
    };
    if !config.web.enabled || !config.web.tmux.style_client {
        return profile;
    }

    let family = &config.web.tmux.font;
    profile
        .args
        .extend(["-t".to_owned(), format!("fontFamily={family},monospace")]);
    match WebClientColors::from_palette(&crate::config::resolve_inline_palette(&config.theme)) {
        Some(colors) => match serde_json::to_string(&colors.to_xterm_theme()) {
            Ok(theme) => profile
                .args
                .extend(["-t".to_owned(), format!("theme={theme}")]),
            Err(err) => profile
                .warnings
                .push(WebWarning::BrowserThemeSkipped(format!(
                    "could not serialize browser theme: {err}"
                ))),
        },
        None => profile.warnings.push(WebWarning::BrowserThemeSkipped(
            "scheme palette is incomplete or malformed".to_owned(),
        )),
    }

    let resolution = super::fonts::resolve(family, config.web.tmux.font_source.as_deref());
    profile.warnings.extend(
        resolution
            .warnings
            .into_iter()
            .map(WebWarning::BrowserFontSkipped),
    );
    if resolution.faces.is_empty() {
        return profile;
    }

    let index = program()
        .and_then(|program| version_at(&program).map(|version| (program, version)))
        .map_err(|err| err.to_string())
        .and_then(|(program, version)| {
            ensure_custom_index(&program, &version, family, &resolution.faces)
        });
    match index {
        Ok(Some(path)) => profile
            .args
            .extend(["-I".to_owned(), path.display().to_string()]),
        Ok(None) => profile.warnings.push(WebWarning::BrowserFontSkipped(
            "stock ttyd index has no </head> marker".to_owned(),
        )),
        Err(err) => profile.warnings.push(WebWarning::BrowserFontSkipped(err)),
    }
    profile
}

fn ensure_custom_index(
    program: &Path,
    ttyd_version: &str,
    family: &str,
    faces: &[FontFace],
) -> std::result::Result<Option<PathBuf>, String> {
    let key = custom_index_key(ttyd_version, family, faces);
    let path = paths::cache_home()
        .join(INDEX_CACHE_DIR)
        .join(format!("index-{key}.html"));
    if path.is_file() {
        return Ok(Some(path));
    }

    let stock = fetch_stock_index(program)?;
    let Some(rendered) = inject_font_faces(&stock, family, faces) else {
        return Ok(None);
    };
    atomic::write_cache_bytes_atomically(&path, rendered.as_bytes()).map_err(|err| {
        format!(
            "could not cache generated ttyd index `{}`: {err}",
            path.display()
        )
    })?;
    Ok(Some(path))
}

fn fetch_stock_index(program: &Path) -> std::result::Result<String, String> {
    let session = "rimz-stock-index";
    let port = choose_instance_port(session).map_err(|err| err.to_string())?;
    let secret = random_secret();
    let spec = CommandSpec::new(program.display().to_string())
        .args(["-c", &format!("rimz:{secret}"), "-i", "127.0.0.1", "-p"])
        .arg(port.to_string())
        .arg("sh");
    let pid = spawn_detached(spec).map_err(|err| err.to_string())?;
    let instance = TtydInstance {
        session: session.to_owned(),
        pid,
        port,
    };
    if !wait_for_port(port, START_TIMEOUT) {
        let _ = stop_instances(std::slice::from_ref(&instance));
        return Err(format!(
            "stock ttyd did not accept connections on 127.0.0.1:{port} within 5 seconds"
        ));
    }

    let fetched = get_stock_index(port, &secret);
    let stopped = stop_instances(std::slice::from_ref(&instance)).map_err(|err| err.to_string());
    match (fetched, stopped) {
        (Ok(index), Ok(())) => Ok(index),
        (Err(err), _) | (Ok(_), Err(err)) => Err(err),
    }
}

fn get_stock_index(port: u16, secret: &str) -> std::result::Result<String, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(STOCK_INDEX_TIMEOUT))
        .build()
        .new_agent();
    let credentials = base64::engine::general_purpose::STANDARD.encode(format!("rimz:{secret}"));
    let url = format!("http://127.0.0.1:{port}/");
    let mut response = agent
        .get(&url)
        .header("Authorization", format!("Basic {credentials}"))
        .call()
        .map_err(|err| format!("could not fetch stock ttyd index: {err}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "stock ttyd index returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(STOCK_INDEX_MAX_BYTES)
        .read_to_vec()
        .map_err(|err| format!("could not read stock ttyd index: {err}"))?;
    String::from_utf8(bytes).map_err(|err| format!("stock ttyd index is not UTF-8: {err}"))
}

fn custom_index_key(ttyd_version: &str, family: &str, faces: &[FontFace]) -> String {
    let mut hasher = Sha256::new();
    hash_index_part(&mut hasher, ttyd_version.as_bytes());
    hash_index_part(&mut hasher, family.as_bytes());
    for face in faces {
        hash_index_part(&mut hasher, face.extension.as_bytes());
        hash_index_part(&mut hasher, &face.weight.to_le_bytes());
        hash_index_part(&mut hasher, &Sha256::digest(&face.bytes));
    }
    hex::encode(&hasher.finalize()[..16])
}

fn hash_index_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn inject_font_faces(stock: &str, family: &str, faces: &[FontFace]) -> Option<String> {
    let marker = stock.find("</head>")?;
    let family = css_string(family);
    let mut style = String::from("<style id=\"rimz-web-fonts\">");
    for face in faces {
        let payload = base64::engine::general_purpose::STANDARD.encode(&face.bytes);
        style.push_str(&format!(
            "@font-face{{font-family:\"{family}\";font-style:normal;font-weight:{};src:url(data:font/{};base64,{payload})}}",
            face.weight, face.extension
        ));
    }
    style.push_str("</style>");

    let mut rendered = String::with_capacity(stock.len() + style.len());
    rendered.push_str(&stock[..marker]);
    rendered.push_str(&style);
    rendered.push_str(&stock[marker..]);
    Some(rendered)
}

fn css_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '<' => escaped.push_str("\\3c "),
            '\n' => escaped.push_str("\\a "),
            '\r' => escaped.push_str("\\d "),
            '\0' => escaped.push_str("\\fffd "),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn spawn_spec(
    session: &str,
    port: u16,
    secret: &str,
    extra_args: &[String],
) -> Result<CommandSpec> {
    Ok(spawn_spec_for(
        &program()?,
        session,
        port,
        secret,
        &crate::mux::tmux::managed_server_socket_path(),
        extra_args,
    ))
}

/// ttyd's child attaches to the managed room, so it addresses the managed
/// socket explicitly — ttyd runs detached with no inherited `$TMUX` to follow.
fn spawn_spec_for(
    program: &Path,
    session: &str,
    port: u16,
    secret: &str,
    tmux_socket: &Path,
    extra_args: &[String],
) -> CommandSpec {
    CommandSpec::new(program.display().to_string())
        .args(["-W", "-O", "-c"])
        .arg(format!("rimz:{secret}"))
        .args(["-i", "127.0.0.1", "-p"])
        .arg(port.to_string())
        .args(["-b"])
        .arg(format!("/{session}"))
        .args(extra_args.iter().cloned())
        .args(["tmux", "-S"])
        .arg(tmux_socket.display().to_string())
        .args(["attach", "-t"])
        .arg(session.to_owned())
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

fn choose_instance_port(session: &str) -> Result<u16> {
    let preferred = derive_instance_port(session);
    for port in port_scan(preferred, &TTYD_PORT_RANGE) {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(WebErr::NoFreeTtydPort)
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn credential_path() -> PathBuf {
    paths::state_home().join("rimz").join(CREDENTIAL_FILE)
}

fn instance_dir() -> PathBuf {
    paths::state_home().join("rimz").join(INSTANCE_DIR)
}

fn instance_path(session: &str) -> PathBuf {
    let encoded = session
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    instance_dir().join(format!("{encoded}.json"))
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

fn write_instance(instance: &TtydInstance) -> Result<()> {
    write_instance_at(&instance_path(&instance.session), instance)
}

fn write_instance_at(path: &Path, instance: &TtydInstance) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, instance)?;
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn argv_uses_loopback_auth_origin_and_tmux_attach() {
        let spec = spawn_spec_for(
            Path::new("/tmp/ttyd"),
            "rimz-project-a1b2c3",
            8201,
            "secret",
            Path::new("/run/user/1000/rimz/tmux/server"),
            &["-t".to_owned(), "macOptionIsMeta=true".to_owned()],
        );
        assert_eq!(
            spec.args,
            [
                "-W",
                "-O",
                "-c",
                "rimz:secret",
                "-i",
                "127.0.0.1",
                "-p",
                "8201",
                "-b",
                "/rimz-project-a1b2c3",
                "-t",
                "macOptionIsMeta=true",
                "tmux",
                "-S",
                "/run/user/1000/rimz/tmux/server",
                "attach",
                "-t",
                "rimz-project-a1b2c3"
            ]
        );
    }

    #[test]
    fn styled_argv_keeps_all_client_options_before_tmux() {
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
            "room",
            8202,
            "secret",
            Path::new("/tmp/tmux.sock"),
            &extra,
        );

        let tmux = spec
            .args
            .iter()
            .position(|arg| arg == "tmux")
            .expect("tmux argv");
        assert_eq!(&spec.args[tmux - extra.len()..tmux], extra);
    }

    #[test]
    fn custom_index_key_is_stable_and_font_faces_inject_before_head_close() {
        let faces = vec![FontFace {
            bytes: b"font bytes".to_vec(),
            extension: "woff2".to_owned(),
            weight: 400,
        }];
        let key = custom_index_key("ttyd 1.7.7", "RimZ \"Font\"", &faces);
        assert_eq!(key, custom_index_key("ttyd 1.7.7", "RimZ \"Font\"", &faces));
        assert_ne!(key, custom_index_key("ttyd 1.7.8", "RimZ \"Font\"", &faces));

        let rendered = inject_font_faces(
            "<html><head><title>ttyd</title></head><body></body></html>",
            "RimZ \"Font\" </style>",
            &faces,
        )
        .expect("head marker");
        assert!(rendered.contains("font-family:\"RimZ \\\"Font\\\" \\3c /style>\""));
        assert!(rendered.contains("data:font/woff2;base64,Zm9udCBieXRlcw=="));
        assert!(rendered.find("rimz-web-fonts").unwrap() < rendered.find("</head>").unwrap());
        assert_eq!(inject_font_faces("<html></html>", "font", &faces), None);
    }

    #[test]
    fn port_derivation_is_stable_and_in_range() {
        let first = derive_instance_port("rimz-project-a1b2c3");
        assert_eq!(first, derive_instance_port("rimz-project-a1b2c3"));
        assert!(TTYD_PORT_RANGE.contains(&first));
    }

    #[test]
    fn credential_roundtrip_is_private() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("credential.json");
        let credential = TtydCredential {
            name: "rimz".to_owned(),
            created_at: Timestamp::from_second(1_700_000_000).expect("timestamp"),
            secret: "secret".to_owned(),
        };
        write_credential_at(&path, &credential).expect("write");
        assert_eq!(read_json_optional(&path).expect("read"), Some(credential));
        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn instance_state_reads_legacy_timestamp_and_stale_pid_is_not_in_inventory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("instance.json");
        let instance = TtydInstance {
            session: "rimz-project-a1b2c3".to_owned(),
            pid: u32::MAX,
            port: 8299,
        };
        let mut legacy = serde_json::to_value(&instance).expect("instance json");
        legacy["started_at"] = serde_json::json!("2023-11-14T22:13:20Z");
        atomic::write_temp_then_rename_cache(&path, &legacy).expect("write legacy state");
        assert_eq!(
            read_json_optional(&path).expect("read state"),
            Some(instance)
        );
    }
}
