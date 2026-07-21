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
const CUSTOM_INDEX_SCHEMA: &str = "rimz.ttyd-index.v3";

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
    let instances = inventory()?;
    if instances.is_empty() {
        return Ok((mint_credential()?, 0, Vec::new()));
    }
    let profile = client_profile(config);
    let credential = mint_credential()?;
    let instances = inventory()?;
    stop_instances(&instances)?;
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
        args: vec![
            "-t".to_owned(),
            "macOptionIsMeta=true".to_owned(),
            "-t".to_owned(),
            "cursorBlink=false".to_owned(),
        ],
        warnings: Vec::new(),
    };
    if !config.web.enabled {
        return profile;
    }

    let mut font_family = None;
    let mut font_faces = Vec::new();
    if config.web.tmux.style_client {
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
        if !resolution.faces.is_empty() {
            font_family = Some(family.as_str());
            font_faces = resolution.faces;
        }
    }

    let index = program()
        .and_then(|program| version_at(&program).map(|version| (program, version)))
        .map_err(|err| err.to_string())
        .and_then(|(program, version)| {
            ensure_custom_index(&program, &version, font_family, &font_faces)
        });
    match index {
        Ok(Some(path)) => profile
            .args
            .extend(["-I".to_owned(), path.display().to_string()]),
        Ok(None) => profile.warnings.push(WebWarning::BrowserClientSkipped(
            "stock ttyd index has no </head> or </body> marker".to_owned(),
        )),
        Err(err) => profile.warnings.push(WebWarning::BrowserClientSkipped(err)),
    }
    profile
}

fn ensure_custom_index(
    program: &Path,
    ttyd_version: &str,
    family: Option<&str>,
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
    let Some(rendered) = inject_client_profile(&stock, family, faces) else {
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

fn custom_index_key(ttyd_version: &str, family: Option<&str>, faces: &[FontFace]) -> String {
    let mut hasher = Sha256::new();
    hash_index_part(&mut hasher, CUSTOM_INDEX_SCHEMA.as_bytes());
    hash_index_part(&mut hasher, ttyd_version.as_bytes());
    hash_index_part(&mut hasher, family.unwrap_or_default().as_bytes());
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

fn inject_client_profile(stock: &str, family: Option<&str>, faces: &[FontFace]) -> Option<String> {
    let head_marker = stock.find("</head>")?;
    let body_marker = stock.rfind("</body>")?;
    if body_marker < head_marker {
        return None;
    }

    let mut style = String::from("<style id=\"rimz-web-style\">");
    let css_family = family.map(css_string);
    if let Some(family) = css_family.as_deref() {
        for face in faces {
            let payload = base64::engine::general_purpose::STANDARD.encode(&face.bytes);
            style.push_str(&format!(
                "@font-face{{font-family:\"{family}\";font-style:normal;font-weight:{};font-display:block;src:url(data:font/{};base64,{payload})}}",
                face.weight, face.extension
            ));
        }
    }
    let overlay_family = css_family.map_or_else(
        || "monospace".to_owned(),
        |family| format!("\"{family}\",monospace"),
    );
    style.push_str(&format!(
        ".xterm .rimz-overlay{{top:50% !important;left:50% !important;transform:translate(-50%,-50%);padding:10px 18px !important;border-radius:10px !important;background:rgba(13,15,20,.78) !important;color:#e6e8ee !important;font:500 13px/1.4 {overlay_family} !important;letter-spacing:.04em;border:1px solid rgba(255,255,255,.14);box-shadow:0 8px 32px rgba(0,0,0,.45);backdrop-filter:blur(10px);-webkit-backdrop-filter:blur(10px)}}"
    ));
    style.push_str("</style>");
    let bootstrap = client_bootstrap(family);

    let mut rendered = String::with_capacity(stock.len() + style.len() + bootstrap.len());
    rendered.push_str(&stock[..head_marker]);
    rendered.push_str(&style);
    rendered.push_str(&stock[head_marker..body_marker]);
    rendered.push_str(&bootstrap);
    rendered.push_str(&stock[body_marker..]);
    Some(rendered)
}

fn client_bootstrap(family: Option<&str>) -> String {
    let family = family.map_or_else(|| "null".to_owned(), js_string);
    format!(
        r#"<script id="rimz-web-client">(()=>{{
"use strict";
const fontFamily={family};
const waitForTerminal=()=>new Promise(resolve=>{{
  let attempts=0;
  const find=()=>{{
    if(window.term&&window.term.element)resolve(window.term);
    else if(attempts++<1200)window.setTimeout(find,25);
  }};
  find();
}});
const loadFont=fontFamily&&document.fonts
  ?Promise.all([400,700].map(weight=>document.fonts.load(`${{weight}} 13px ${{JSON.stringify(fontFamily)}}`))).catch(()=>{{}})
  :Promise.resolve();
waitForTerminal().then(term=>{{
  const sendInput=data=>{{
    if(typeof term.input==="function"){{term.input(data,true);return true;}}
    const core=term._core&&term._core.coreService;
    if(core&&typeof core.triggerDataEvent==="function"){{core.triggerDataEvent(data,true);return true;}}
    return false;
  }};
  const altKeyChar=event=>{{
    if(/^Key[A-Z]$/.test(event.code)){{
      const ch=event.code.slice(3);
      return event.shiftKey?ch:ch.toLowerCase();
    }}
    if(/^Digit[0-9]$/.test(event.code))return event.code.slice(5);
    return null;
  }};
  const keyHandler=event=>{{
    if(event.type==="keydown"&&event.altKey&&!event.ctrlKey&&!event.metaKey){{
      const ch=altKeyChar(event);
      if(ch&&sendInput("\u001b"+ch)){{event.preventDefault();event.stopPropagation();return false;}}
    }}
    if(event.type!=="keydown"||event.key!=="Enter"||!event.shiftKey||event.altKey||event.ctrlKey||event.metaKey)return true;
    if(!sendInput("\u001b[13;2u"))return true;
    event.preventDefault();
    event.stopPropagation();
    return false;
  }};
  term.parser.registerOscHandler(52,data=>{{
    const semi=data.indexOf(";");
    if(semi<0)return true;
    const payload=data.slice(semi+1);
    if(payload==="?")return true;
    try{{
      const bytes=Uint8Array.from(atob(payload),ch=>ch.charCodeAt(0));
      const text=new TextDecoder().decode(bytes);
      if(text)navigator.clipboard.writeText(text).catch(()=>{{}});
    }}catch(_){{}}
    return true;
  }});
  term.onSelectionChange(()=>{{
    const selection=term.getSelection();
    if(selection)navigator.clipboard.writeText(selection).catch(()=>{{}});
  }});
  const updateOverlay=node=>{{
    const element=node.nodeType===Node.ELEMENT_NODE?node:node.parentElement;
    if(!element)return;
    let overlay=element.closest(".rimz-overlay");
    if(element.tagName==="DIV"&&element.style.borderRadius==="15px"){{
      element.classList.add("rimz-overlay");
      overlay=element;
    }}
    if(overlay&&overlay.textContent==="Press ⏎ to Reconnect")overlay.textContent="Press Enter to reconnect";
  }};
  new MutationObserver(records=>{{
    for(const record of records){{
      updateOverlay(record.target);
      for(const node of record.addedNodes)updateOverlay(node);
    }}
  }}).observe(term.element,{{childList:true,subtree:true}});
  const install=()=>{{
    term.options.cursorBlink=false;
    term.attachCustomKeyEventHandler(keyHandler);
  }};
  install();
  const reset=term.reset.bind(term);
  term.reset=(...args)=>{{const result=reset(...args);install();return result;}};
  if(fontFamily)loadFont.then(()=>{{
    const configured=`${{fontFamily}},monospace`;
    term.options.fontFamily="monospace";
    window.requestAnimationFrame(()=>{{
      term.options.fontFamily=configured;
      term.clearTextureAtlas();
      term.refresh(0,term.rows-1);
      if(typeof term.fit==="function")term.fit();
    }});
  }});
}});
}})();</script>"#
    )
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

fn js_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '<' => escaped.push_str("\\u003c"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            ch if ch <= '\u{1f}' => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let value = ch as usize;
                escaped.push_str("\\u00");
                escaped.push(HEX[value >> 4] as char);
                escaped.push(HEX[value & 0x0f] as char);
            }
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
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
    fn browser_safety_options_survive_disabled_web() {
        let mut config = MachineConfig::default();
        config.web.enabled = false;
        let profile = client_profile(&config);
        assert_eq!(
            profile.args,
            ["-t", "macOptionIsMeta=true", "-t", "cursorBlink=false"]
        );
        assert!(profile.warnings.is_empty());
    }

    #[test]
    fn custom_index_injects_fonts_and_browser_behavior_at_document_edges() {
        let faces = vec![FontFace {
            bytes: b"font bytes".to_vec(),
            extension: "woff2".to_owned(),
            weight: 400,
        }];
        let family = "RimZ \"Font\" </style></script>\n";
        let key = custom_index_key("ttyd 1.7.7", Some(family), &faces);
        assert_eq!(key, custom_index_key("ttyd 1.7.7", Some(family), &faces));
        assert_ne!(key, custom_index_key("ttyd 1.7.8", Some(family), &faces));
        assert_ne!(key, custom_index_key("ttyd 1.7.7", None, &faces));

        let rendered = inject_client_profile(
            "<html><head><title>ttyd</title></head><body></body></html>",
            Some(family),
            &faces,
        )
        .expect("document markers");
        assert!(
            rendered.contains("font-family:\"RimZ \\\"Font\\\" \\3c /style>\\3c /script>\\a \"")
        );
        assert!(rendered.contains("data:font/woff2;base64,Zm9udCBieXRlcw=="));
        assert!(rendered.contains("font-display:block"));
        assert!(
            rendered.contains(
                "const fontFamily=\"RimZ \\\"Font\\\" \\u003c/style>\\u003c/script>\\n\""
            )
        );
        assert!(rendered.contains("term.options.cursorBlink=false"));
        assert!(rendered.contains("term.attachCustomKeyEventHandler(keyHandler)"));
        assert!(rendered.contains("registerOscHandler(52"));
        assert!(rendered.contains("onSelectionChange"));
        assert!(rendered.contains("event.altKey"));
        assert!(rendered.contains("element.classList.add(\"rimz-overlay\")"));
        assert!(rendered.contains("Press Enter to reconnect"));
        assert!(rendered.contains("sendInput(\"\\u001b[13;2u\")"));
        assert!(rendered.contains("term.clearTextureAtlas()"));
        assert!(
            rendered
                .contains("font:500 13px/1.4 \"RimZ \\\"Font\\\" \\3c /style>\\3c /script>\\a \"")
        );
        let style = rendered.find("rimz-web-style").unwrap();
        let overlay_rule = rendered.find(".xterm .rimz-overlay").unwrap();
        let head = rendered.find("</head>").unwrap();
        assert!(style < overlay_rule && overlay_rule < head);
        assert!(rendered.find("rimz-web-client").unwrap() < rendered.find("</body>").unwrap());
        assert_eq!(
            inject_client_profile("<html></html>", Some("font"), &faces),
            None
        );
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
