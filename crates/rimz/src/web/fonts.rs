//! Browser-font resolution, verified downloads, and cache reuse.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use url::Url;

use crate::store::{atomic, paths};

const OFFLINE_ENV: &str = "RIMZ_WEB_FONTS_OFFLINE";
const FONT_CACHE_DIR: &str = "rimz/web-fonts";
const FONT_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FONT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FontFace {
    pub bytes: Vec<u8>,
    pub extension: String,
    pub weight: u16,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FontResolution {
    pub faces: Vec<FontFace>,
    pub warnings: Vec<String>,
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

pub(super) fn resolve(family: &str, source: Option<&str>) -> FontResolution {
    match source.map(str::trim).filter(|source| !source.is_empty()) {
        Some(source) => match resolve_custom(source) {
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

fn resolve_custom(source: &str) -> Result<FontFace, String> {
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
                let bytes = fetch(source)?;
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
        validate_size(source, &bytes)?;
        return Ok(FontFace {
            bytes,
            extension,
            weight: 400,
        });
    }
    if source.contains("://") {
        return Err(format!("font URL must use https: `{source}`"));
    }
    let path = PathBuf::from(source);
    let extension = extension_from_path(&path)?;
    let bytes = fs::read(&path)
        .map_err(|err| format!("could not read font `{}`: {err}", path.display()))?;
    validate_size(source, &bytes)?;
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

    let bytes = fetch(face.url)?;
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

fn fetch(url: &str) -> Result<Vec<u8>, String> {
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

fn validate_size(source: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 <= MAX_FONT_BYTES {
        Ok(())
    } else {
        Err(format!(
            "font `{source}` exceeds the {} MiB browser-font limit",
            MAX_FONT_BYTES / 1024 / 1024
        ))
    }
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
