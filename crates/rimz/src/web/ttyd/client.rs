//! Browser-client profile generation for the shared ttyd daemon.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::{InlinePalette, MachineConfig, parse_hex};
use crate::mux::CommandSpec;
use crate::store::{atomic, paths};

use super::{
    DaemonRecord, START_TIMEOUT, choose_ephemeral_port, random_secret, spawn_detached,
    terminate_record, version_at, wait_for_port,
};
use crate::sidebar_pane::pixel::{PLACEHOLDER, ROW_COLUMN_DIACRITICS};
use crate::web::WebWarning;

const STOCK_INDEX_TIMEOUT: Duration = Duration::from_secs(5);
const STOCK_INDEX_MAX_BYTES: u64 = 16 * 1024 * 1024;
const INDEX_CACHE_DIR: &str = "rimz/web-ttyd";
const CUSTOM_INDEX_SCHEMA: &str = "rimz.ttyd-index.v5";

const OFFLINE_ENV: &str = "RIMZ_WEB_FONTS_OFFLINE";
const FONT_CACHE_DIR: &str = "rimz/web-fonts";
const FONT_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FONT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(in crate::web) struct ClientProfile {
    pub(in crate::web) args: Vec<String>,
    pub(in crate::web) warnings: Vec<WebWarning>,
    pub(in crate::web) pixel_protocol: Option<u32>,
}

pub(in crate::web) fn profile(config: &MachineConfig, ttyd_program: &Path) -> ClientProfile {
    let mut profile = ClientProfile {
        args: vec![
            "-t".to_owned(),
            "macOptionIsMeta=true".to_owned(),
            "-t".to_owned(),
            "cursorBlink=false".to_owned(),
        ],
        warnings: Vec::new(),
        pixel_protocol: None,
    };
    if !config.web.enabled {
        return profile;
    }

    let mut font_family = None;
    let mut font_faces = Vec::new();
    if config.web.style_client {
        let family = &config.web.font;
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

        let resolution = resolve_font(family, config.web.font_source.as_deref());
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

    let index = version_at(ttyd_program)
        .map_err(|err| err.to_string())
        .and_then(|version| ensure_custom_index(ttyd_program, &version, font_family, &font_faces));
    apply_custom_index(&mut profile, index);
    profile
}

fn apply_custom_index(profile: &mut ClientProfile, index: Result<Option<PathBuf>, String>) {
    match index {
        Ok(Some(path)) => {
            profile
                .args
                .extend(["-I".to_owned(), path.display().to_string()]);
            profile.pixel_protocol = Some(crate::web::TTYD_PIXEL_PROTOCOL);
        }
        Ok(None) => profile.warnings.push(WebWarning::BrowserClientSkipped(
            "stock ttyd index has no </head> or </body> marker".to_owned(),
        )),
        Err(err) => profile.warnings.push(WebWarning::BrowserClientSkipped(err)),
    }
}

fn ensure_custom_index(
    program: &Path,
    ttyd_version: &str,
    family: Option<&str>,
    faces: &[FontFace],
) -> Result<Option<PathBuf>, String> {
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

fn fetch_stock_index(program: &Path) -> Result<String, String> {
    let port = choose_ephemeral_port().map_err(|err| err.to_string())?;
    let secret = random_secret();
    let spec = CommandSpec::new(program.display().to_string())
        .args(["-c", &format!("rimz:{secret}"), "-i", "127.0.0.1", "-p"])
        .arg(port.to_string())
        .arg("sh");
    let pid = spawn_detached(spec).map_err(|err| err.to_string())?;
    let record = DaemonRecord::basic_loopback(pid, port);
    if !wait_for_port(port, START_TIMEOUT) {
        terminate_record(&record);
        return Err(format!(
            "stock ttyd did not accept connections on 127.0.0.1:{port} within 5 seconds"
        ));
    }

    let fetched = get_stock_index(port, &secret);
    terminate_record(&record);
    fetched
}

fn get_stock_index(port: u16, secret: &str) -> Result<String, String> {
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
    custom_index_key_with_schema(CUSTOM_INDEX_SCHEMA, ttyd_version, family, faces)
}

fn custom_index_key_with_schema(
    schema: &str,
    ttyd_version: &str,
    family: Option<&str>,
    faces: &[FontFace],
) -> String {
    let mut hasher = Sha256::new();
    hash_index_part(&mut hasher, schema.as_bytes());
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
    let diacritics = ROW_COLUMN_DIACRITICS
        .iter()
        .map(char::to_string)
        .collect::<Vec<_>>();
    let diacritics = serde_json::to_string(&diacritics)
        .expect("serializing the static pixel diacritic table cannot fail");
    let pixel_layer = include_str!("pixel_layer.js");
    let pixel_protocol = crate::web::TTYD_PIXEL_PROTOCOL;
    let placeholder = u32::from(PLACEHOLDER);
    format!(
        r#"<script id="rimz-web-client">(()=>{{
"use strict";
const fontFamily={family};
const RIMZ_PIXEL_PROTOCOL={pixel_protocol};
const RIMZ_PIXEL_PLACEHOLDER={placeholder};
const RIMZ_PIXEL_DIACRITICS={diacritics};
{pixel_layer}
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
  installPixelLayer(term);
  const sendInput=data=>{{
    if(typeof term.input==="function"){{term.input(data,true);return true;}}
    const core=term._core&&term._core.coreService;
    if(core&&typeof core.triggerDataEvent==="function"){{core.triggerDataEvent(data,true);return true;}}
    return false;
  }};
  const writeClipboard=text=>{{
    if(!text)return;
    try{{navigator.clipboard.writeText(text).catch(()=>{{}});}}catch(_){{}}
  }};
  const altKeyChar=event=>{{
    if(/^Key[A-Z]$/.test(event.code)){{
      const ch=event.code.slice(3);
      return event.shiftKey?ch:ch.toLowerCase();
    }}
    if(/^Digit[0-9]$/.test(event.code)&&!event.shiftKey)return event.code.slice(5);
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
      writeClipboard(text);
    }}catch(_){{}}
    return true;
  }});
  const steadyStyles={{0:"block",1:"block",2:"block",3:"underline",4:"underline",5:"bar",6:"bar"}};
  term.parser.registerCsiHandler({{intermediates:" ",final:"q"}},params=>{{
    const style=steadyStyles[params[0]||0];
    if(!style)return true;
    if(term.options.cursorStyle!==style)term.options.cursorStyle=style;
    if(term.options.cursorBlink)term.options.cursorBlink=false;
    return true;
  }});
  const swallowBlinkMode=params=>params.length===1&&params[0]===12;
  term.parser.registerCsiHandler({{prefix:"?",final:"h"}},swallowBlinkMode);
  term.parser.registerCsiHandler({{prefix:"?",final:"l"}},swallowBlinkMode);
  term.onSelectionChange(()=>writeClipboard(term.getSelection()));
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct WebClientColors {
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
    cursor: (u8, u8, u8),
    cursor_accent: (u8, u8, u8),
    normal: [(u8, u8, u8); 8],
    bright: [(u8, u8, u8); 8],
    selection_background: Option<(u8, u8, u8)>,
    selection_foreground: Option<(u8, u8, u8)>,
}

impl WebClientColors {
    fn from_palette(palette: &InlinePalette) -> Option<Self> {
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
        let cursor_color = parse_optional_color(cursor.and_then(|color| color.cursor.as_deref()))?
            .unwrap_or(foreground);
        let cursor_accent = parse_optional_color(cursor.and_then(|color| color.text.as_deref()))?
            .unwrap_or(background);
        let selection = palette.selection.as_ref();
        Some(Self {
            background,
            foreground,
            cursor: cursor_color,
            cursor_accent,
            normal,
            bright,
            selection_background: parse_optional_color(
                selection.and_then(|color| color.background.as_deref()),
            )?,
            selection_foreground: parse_optional_color(
                selection.and_then(|color| color.text.as_deref()),
            )?,
        })
    }

    fn to_xterm_theme(&self) -> Value {
        let mut theme = Map::new();
        for (name, rgb) in [
            ("background", self.background),
            ("foreground", self.foreground),
            ("cursor", self.cursor),
            ("cursorAccent", self.cursor_accent),
        ] {
            theme.insert(name.to_owned(), Value::String(hex_color(rgb)));
        }
        if let Some(rgb) = self.selection_background {
            theme.insert(
                "selectionBackground".to_owned(),
                Value::String(hex_color(rgb)),
            );
        }
        if let Some(rgb) = self.selection_foreground {
            theme.insert(
                "selectionForeground".to_owned(),
                Value::String(hex_color(rgb)),
            );
        }
        for (name, rgb) in XTERM_NORMAL_COLOR_NAMES
            .iter()
            .copied()
            .zip(self.normal.iter().copied())
            .chain(
                XTERM_BRIGHT_COLOR_NAMES
                    .iter()
                    .copied()
                    .zip(self.bright.iter().copied()),
            )
        {
            theme.insert(name.to_owned(), Value::String(hex_color(rgb)));
        }
        Value::Object(theme)
    }
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

fn hex_color((red, green, blue): (u8, u8, u8)) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

const XTERM_NORMAL_COLOR_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

const XTERM_BRIGHT_COLOR_NAMES: [&str; 8] = [
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct FontFace {
    bytes: Vec<u8>,
    extension: String,
    weight: u16,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct FontResolution {
    faces: Vec<FontFace>,
    warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct PresetFace {
    url: &'static str,
    sha256: &'static str,
    weight: u16,
}

#[derive(Clone, Copy, Debug)]
struct FontPreset {
    family: &'static str,
    faces: &'static [PresetFace],
}

const JETBRAINS_MONO_FACES: &[PresetFace] = &[
    PresetFace {
        url: "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/JetBrainsMono/Ligatures/Regular/JetBrainsMonoNerdFontMono-Regular.ttf",
        sha256: "f01031f40e48dc29e1112e6b0b0450a2c6cd097f3f35cfff05c55cb311f8034c",
        weight: 400,
    },
    PresetFace {
        url: "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/JetBrainsMono/Ligatures/Bold/JetBrainsMonoNerdFontMono-Bold.ttf",
        sha256: "5bdd4a873f3cd32f882d2c55545089123926e27707d5880fc9eaf84eb01b6686",
        weight: 700,
    },
];

const CASKAYDIA_COVE_FACES: &[PresetFace] = &[
    PresetFace {
        url: "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/CascadiaCode/CaskaydiaCoveNerdFontMono-Regular.ttf",
        sha256: "32aa528c1d9be2240ceac90aa05f4e554679cabeb11b93684eb24ec4930bd0ea",
        weight: 400,
    },
    PresetFace {
        url: "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/CascadiaCode/CaskaydiaCoveNerdFontMono-Bold.ttf",
        sha256: "3b7960d16d56bc3e0fd109c3f0e18b0ef547c863144dbf79e2ec71ab6ff3dd1e",
        weight: 700,
    },
];

const FONT_PRESETS: &[FontPreset] = &[
    FontPreset {
        family: "JetBrainsMono Nerd Font Mono",
        faces: JETBRAINS_MONO_FACES,
    },
    FontPreset {
        family: "CaskaydiaCove Nerd Font Mono",
        faces: CASKAYDIA_COVE_FACES,
    },
];

fn resolve_font(family: &str, source: Option<&str>) -> FontResolution {
    match source.map(str::trim).filter(|source| !source.is_empty()) {
        Some(source) => match resolve_custom_font(source) {
            Ok(face) => FontResolution {
                faces: vec![face],
                warnings: Vec::new(),
            },
            Err(err) => FontResolution {
                faces: Vec::new(),
                warnings: vec![err],
            },
        },
        None => resolve_preset(family),
    }
}

fn resolve_custom_font(source: &str) -> Result<FontFace, String> {
    if source.starts_with("https://") {
        let url =
            Url::parse(source).map_err(|err| format!("invalid font URL `{source}`: {err}"))?;
        let extension = extension_from_path(Path::new(url.path()))?;
        let path = font_cache_dir().join(format!(
            "{}.{}",
            hex::encode(Sha256::digest(source.as_bytes())),
            extension
        ));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if offline() {
                    return Err(format!(
                        "font `{source}` is not cached and {OFFLINE_ENV} is set"
                    ));
                }
                let bytes = fetch_font(source)?;
                atomic::write_cache_bytes_atomically(&path, &bytes)
                    .map_err(|err| format!("could not cache font `{}`: {err}", path.display()))?;
                bytes
            }
            Err(err) => {
                return Err(format!(
                    "could not read font cache `{}`: {err}",
                    path.display()
                ));
            }
        };
        validate_size(source, bytes.len() as u64)?;
        return Ok(FontFace {
            bytes,
            extension,
            weight: 400,
        });
    }
    if source.contains("://") {
        return Err(format!("font URL must use https: `{source}`"));
    }
    let path = expand_home(Path::new(source));
    let extension = extension_from_path(&path)?;
    let size = fs::metadata(&path)
        .map_err(|err| format!("could not inspect font `{}`: {err}", path.display()))?
        .len();
    validate_size(source, size)?;
    let bytes = fs::read(&path)
        .map_err(|err| format!("could not read font `{}`: {err}", path.display()))?;
    Ok(FontFace {
        bytes,
        extension,
        weight: 400,
    })
}

fn resolve_preset(family: &str) -> FontResolution {
    let Some(preset) = FONT_PRESETS.iter().find(|preset| preset.family == family) else {
        return FontResolution::default();
    };
    let mut resolution = FontResolution::default();
    for face in preset.faces {
        match resolve_preset_face(*face) {
            Ok(face) => resolution.faces.push(face),
            Err(err) => resolution.warnings.push(err),
        }
    }
    resolution
}

fn resolve_preset_face(face: PresetFace) -> Result<FontFace, String> {
    let url = Url::parse(face.url).map_err(|err| format!("invalid built-in font URL: {err}"))?;
    let file = Path::new(url.path())
        .file_name()
        .ok_or_else(|| format!("built-in font URL has no filename: {}", face.url))?;
    let extension = extension_from_path(Path::new(file))?;
    let path = font_cache_dir().join(file);
    match fs::read(&path) {
        Ok(bytes) if sha256_hex(&bytes) == face.sha256 => {
            return Ok(FontFace {
                bytes,
                extension,
                weight: face.weight,
            });
        }
        Ok(_) if offline() => {
            return Err(format!(
                "cached font `{}` failed checksum verification and {OFFLINE_ENV} is set",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound && offline() => {
            return Err(format!(
                "font `{}` is not cached and {OFFLINE_ENV} is set",
                preset_filename(face.url)
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(format!(
                "could not read font cache `{}`: {err}",
                path.display()
            ));
        }
    }

    let bytes = fetch_font(face.url)?;
    let actual = sha256_hex(&bytes);
    if actual != face.sha256 {
        return Err(format!(
            "downloaded font `{}` failed checksum verification",
            preset_filename(face.url)
        ));
    }
    atomic::write_cache_bytes_atomically(&path, &bytes)
        .map_err(|err| format!("could not cache font `{}`: {err}", path.display()))?;
    Ok(FontFace {
        bytes,
        extension,
        weight: face.weight,
    })
}

fn fetch_font(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(FONT_FETCH_TIMEOUT))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|err| format!("could not download font `{url}`: {err}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "font download `{url}` returned HTTP {}",
            response.status().as_u16()
        ));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_FONT_BYTES)
        .read_to_vec()
        .map_err(|err| format!("could not read font download `{url}`: {err}"))
}

fn extension_from_path(path: &Path) -> Result<String, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            format!(
                "font source `{}` must end in .ttf, .otf, .woff, or .woff2",
                path.display()
            )
        })?;
    match extension.as_str() {
        "ttf" | "otf" | "woff" | "woff2" => Ok(extension),
        _ => Err(format!(
            "font source `{}` must end in .ttf, .otf, .woff, or .woff2",
            path.display()
        )),
    }
}

fn validate_size(source: &str, size: u64) -> Result<(), String> {
    if size <= MAX_FONT_BYTES {
        Ok(())
    } else {
        Err(format!(
            "font `{source}` exceeds the {} MiB browser-font limit",
            MAX_FONT_BYTES / 1024 / 1024
        ))
    }
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

fn font_cache_dir() -> PathBuf {
    paths::cache_home().join(FONT_CACHE_DIR)
}

fn offline() -> bool {
    std::env::var_os(OFFLINE_ENV).is_some()
}

fn preset_filename(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InlineAnsiColors, InlinePrimaryColors};

    #[test]
    fn browser_safety_options_survive_disabled_web() {
        let mut config = MachineConfig::default();
        config.web.enabled = false;
        let profile = profile(&config, Path::new("/missing-ttyd"));
        assert_eq!(
            profile.args,
            ["-t", "macOptionIsMeta=true", "-t", "cursorBlink=false"]
        );
        assert!(profile.warnings.is_empty());
        assert_eq!(profile.pixel_protocol, None);
    }

    #[test]
    fn compatibility_profile_does_not_require_client_styling() {
        let rendered = inject_client_profile("<html><head></head><body></body></html>", None, &[])
            .expect("document markers");
        assert!(rendered.contains("rimz-web-client"));
        assert!(rendered.contains("term.options.cursorBlink=false"));
        assert!(rendered.contains("registerCsiHandler({intermediates:\" \",final:\"q\"}"));
        assert!(rendered.contains("const installPixelLayer=term=>"));
        assert!(rendered.contains("installPixelLayer(term)"));
        assert!(rendered.contains("const RIMZ_PIXEL_PLACEHOLDER=1109742"));
        assert!(rendered.contains("const RIMZ_PIXEL_DIACRITICS=[\"̅\",\"̍\",\"̎\""));
        assert!(rendered.contains("const PLACEHOLDER_OVERHANG_COLS=3"));
        assert!(rendered.contains("const fittedImageRect="));
        assert!(rendered.contains("if(placement.rows>1)"));
        assert!(!rendered.contains("@font-face"));
    }

    #[test]
    fn pixel_protocol_requires_a_generated_custom_index() {
        let base = || ClientProfile {
            args: Vec::new(),
            warnings: Vec::new(),
            pixel_protocol: None,
        };

        let mut capable = base();
        apply_custom_index(&mut capable, Ok(Some(PathBuf::from("/cache/index.html"))));
        assert_eq!(
            capable.args,
            ["-I".to_owned(), "/cache/index.html".to_owned()]
        );
        assert_eq!(
            capable.pixel_protocol,
            Some(crate::web::TTYD_PIXEL_PROTOCOL)
        );

        let mut markerless = base();
        apply_custom_index(&mut markerless, Ok(None));
        assert_eq!(markerless.pixel_protocol, None);
        assert_eq!(markerless.warnings.len(), 1);

        let mut failed = base();
        apply_custom_index(&mut failed, Err("fetch failed".to_owned()));
        assert_eq!(failed.pixel_protocol, None);
        assert_eq!(failed.warnings.len(), 1);
    }

    #[test]
    fn custom_index_injects_fonts_and_browser_behavior_at_document_edges() {
        let faces = vec![FontFace {
            bytes: b"font bytes".to_vec(),
            extension: "woff2".to_owned(),
            weight: 400,
        }];
        let family = "RimZ \"Font\" </style></script>\n";
        assert_eq!(CUSTOM_INDEX_SCHEMA, "rimz.ttyd-index.v5");
        let key = custom_index_key("ttyd 1.7.7", Some(family), &faces);
        assert_eq!(key, custom_index_key("ttyd 1.7.7", Some(family), &faces));
        assert_ne!(key, custom_index_key("ttyd 1.7.8", Some(family), &faces));
        assert_ne!(key, custom_index_key("ttyd 1.7.7", None, &faces));
        assert_ne!(
            key,
            custom_index_key_with_schema("rimz.ttyd-index.v4", "ttyd 1.7.7", Some(family), &faces)
        );

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
        assert!(rendered.contains(
            "const steadyStyles={0:\"block\",1:\"block\",2:\"block\",3:\"underline\",4:\"underline\",5:\"bar\",6:\"bar\"}"
        ));
        assert!(rendered.contains("registerCsiHandler({intermediates:\" \",final:\"q\"}"));
        assert!(
            rendered.contains("if(term.options.cursorStyle!==style)term.options.cursorStyle=style")
        );
        assert!(rendered.contains("if(term.options.cursorBlink)term.options.cursorBlink=false"));
        assert!(
            rendered.contains("const swallowBlinkMode=params=>params.length===1&&params[0]===12")
        );
        assert!(rendered.contains("registerCsiHandler({prefix:\"?\",final:\"h\"}"));
        assert!(rendered.contains("registerCsiHandler({prefix:\"?\",final:\"l\"}"));
        assert!(rendered.contains("term.attachCustomKeyEventHandler(keyHandler)"));
        assert!(rendered.contains("registerOscHandler(52"));
        assert!(rendered.contains("onSelectionChange"));
        assert!(rendered.contains("event.altKey"));
        assert!(rendered.contains("/^Digit[0-9]$/.test(event.code)&&!event.shiftKey"));
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
        let default_style =
            inject_client_profile("<html><head></head><body></body></html>", None, &faces)
                .expect("document markers");
        assert!(default_style.contains("font:500 13px/1.4 monospace !important"));
        assert_eq!(
            inject_client_profile("<html></html>", Some("font"), &faces),
            None
        );
    }

    #[test]
    fn xterm_theme_uses_hex_keys_and_omits_absent_selection_colors() {
        let colors = WebClientColors {
            background: (1, 2, 3),
            foreground: (4, 5, 6),
            cursor: (7, 8, 9),
            cursor_accent: (10, 11, 12),
            normal: [(13, 14, 15); 8],
            bright: [(16, 17, 18); 8],
            selection_background: None,
            selection_foreground: None,
        };
        let theme = colors.to_xterm_theme();

        assert_eq!(theme["background"], "#010203");
        assert_eq!(theme["cursorAccent"], "#0a0b0c");
        assert_eq!(theme["black"], "#0d0e0f");
        assert_eq!(theme["brightWhite"], "#101112");
        assert!(theme.get("selectionBackground").is_none());
        assert!(theme.get("selectionForeground").is_none());
    }

    #[test]
    fn palette_projection_applies_fallbacks_and_rejects_malformed_colors() {
        let mut palette = InlinePalette {
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
        };
        let colors = WebClientColors::from_palette(&palette).expect("colors");
        assert_eq!(colors.normal[0], colors.background);
        assert_eq!(colors.normal[7], colors.foreground);
        assert_eq!(colors.bright, colors.normal);

        palette.normal.as_mut().expect("normal").green = Some("bad".to_owned());
        assert_eq!(WebClientColors::from_palette(&palette), None);
    }

    #[test]
    fn presets_are_https_sha256_pinned_regular_and_bold_fonts() {
        assert_eq!(FONT_PRESETS.len(), 2);
        for preset in FONT_PRESETS {
            assert!(preset.family.ends_with("Nerd Font Mono"));
            assert_eq!(
                preset
                    .faces
                    .iter()
                    .map(|face| face.weight)
                    .collect::<Vec<_>>(),
                [400, 700]
            );
            for face in preset.faces {
                assert!(face.url.starts_with("https://"));
                assert!(face.url.contains("/v3.4.0/patched-fonts/"));
                assert_eq!(face.sha256.len(), 64);
                assert!(face.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert_eq!(
                    extension_from_path(Path::new(face.url)),
                    Ok("ttf".to_owned())
                );
            }
        }
    }

    #[test]
    fn unsupported_font_extensions_are_rejected() {
        assert!(extension_from_path(Path::new("font.ttf")).is_ok());
        assert!(extension_from_path(Path::new("font.WOFF2")).is_ok());
        assert!(extension_from_path(Path::new("font.zip")).is_err());
        assert!(extension_from_path(Path::new("font")).is_err());
    }
}
