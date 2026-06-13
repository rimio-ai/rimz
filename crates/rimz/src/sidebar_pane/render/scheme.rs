use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Deserialize;

use crate::config::{Semantic, parse_hex};

use super::embedded_themes;
use super::oklab::Rgb;
use super::theme::RawPalette;

#[derive(Debug, Deserialize)]
struct AlacrittyTheme {
    colors: Option<AlacrittyColors>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittyColors {
    primary: Option<AlacrittyPrimaryColors>,
    normal: Option<AlacrittyAnsiColors>,
    bright: Option<AlacrittyAnsiColors>,
    selection: Option<AlacrittySelectionColors>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittyPrimaryColors {
    background: Option<String>,
    foreground: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittySelectionColors {
    background: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittyAnsiColors {
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
}

pub(crate) fn explicit_palette_tones(name_or_path: &str) -> Option<Semantic> {
    cached_explicit_palette_tones(name_or_path)
}

pub fn validate_explicit_scheme(name_or_path: &str) -> Result<(), String> {
    load_explicit_palette_tones(name_or_path).map(|_| ())
}

/// Every scheme name a user can select: the bundled Alacritty catalog, sorted.
pub fn available_scheme_names() -> Vec<String> {
    embedded_themes::theme_names().map(str::to_owned).collect()
}

fn cached_explicit_palette_tones(name_or_path: &str) -> Option<Semantic> {
    {
        let cache = lock_explicit_scheme_cache();
        if let Some(tones) = cache.get(name_or_path) {
            return *tones;
        }
    }

    let tones = load_explicit_palette_tones(name_or_path).ok();
    let mut cache = lock_explicit_scheme_cache();
    *cache.entry(name_or_path.to_owned()).or_insert(tones)
}

fn lock_explicit_scheme_cache() -> MutexGuard<'static, HashMap<String, Option<Semantic>>> {
    static CACHED: OnceLock<Mutex<HashMap<String, Option<Semantic>>>> = OnceLock::new();
    match CACHED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn load_explicit_palette_tones(name_or_path: &str) -> Result<Semantic, String> {
    if let Some(text) = embedded_themes::theme_toml(name_or_path) {
        return parse_palette_tones(text).map_err(|err| {
            format!("invalid bundled sidebar theme scheme `{name_or_path}`: {err}")
        });
    }
    load_external_palette_tones(name_or_path)
}

fn load_external_palette_tones(name_or_path: &str) -> Result<Semantic, String> {
    let path = resolve_external_scheme_path(name_or_path).ok_or_else(|| {
        format!(
            "unknown sidebar theme scheme `{name_or_path}`; {}",
            theme_lookup_hint()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading sidebar theme scheme `{}`: {err}", path.display()))?;
    parse_palette_tones(&text)
        .map_err(|err| format!("invalid sidebar theme scheme `{}`: {err}", path.display()))
}

pub fn theme_lookup_hint() -> String {
    format!(
        "{} bundled Alacritty themes in crates/rimz/themes/alacritty (see `rimz list-themes`); or a path to an Alacritty .toml",
        embedded_themes::theme_count()
    )
}

fn resolve_external_scheme_path(name_or_path: &str) -> Option<PathBuf> {
    let path = expand_home(Path::new(name_or_path));
    path.is_file().then_some(path)
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    path.to_path_buf()
}

pub(crate) fn parse_palette_tones(text: &str) -> Result<Semantic, String> {
    Ok(parse_raw_palette(text)?.derive_tones())
}

/// Parse an Alacritty theme into the [`RawPalette`] the semantic layer derives
/// from. Background, foreground, and the six normal ANSI hues are required;
/// `colors.bright.blue` is the selection accent, falling back to `normal.blue`.
fn parse_raw_palette(text: &str) -> Result<RawPalette, String> {
    let theme: AlacrittyTheme =
        toml::from_str(text).map_err(|err| format!("parsing Alacritty theme TOML: {err}"))?;
    let colors = theme.colors.unwrap_or_default();
    let primary = colors.primary.unwrap_or_default();
    let normal = colors.normal.unwrap_or_default();

    // Required keys are validated in a stable order — primary first, then the
    // normal hues — so a malformed scheme reports the earliest missing key.
    let background =
        parse_required_color("colors.primary.background", primary.background.as_deref())?;
    let foreground =
        parse_required_color("colors.primary.foreground", primary.foreground.as_deref())?;
    let red = parse_required_color("colors.normal.red", normal.red.as_deref())?;
    let green = parse_required_color("colors.normal.green", normal.green.as_deref())?;
    let yellow = parse_required_color("colors.normal.yellow", normal.yellow.as_deref())?;
    let blue = parse_required_color("colors.normal.blue", normal.blue.as_deref())?;
    let magenta = parse_required_color("colors.normal.magenta", normal.magenta.as_deref())?;
    let cyan = parse_required_color("colors.normal.cyan", normal.cyan.as_deref())?;
    let bright_blue = match colors
        .bright
        .as_ref()
        .and_then(|bright| bright.blue.as_deref())
    {
        Some(value) => parse_color("colors.bright.blue", value)?,
        None => blue,
    };
    // The selected-card band. Optional: a scheme without it derives a tint at
    // [`RawPalette::derive_tones`] time, so every theme still gets a band.
    let selection_background = match colors
        .selection
        .as_ref()
        .and_then(|selection| selection.background.as_deref())
    {
        Some(value) => Some(parse_color("colors.selection.background", value)?),
        None => None,
    };

    Ok(RawPalette {
        background,
        foreground,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        bright_blue,
        selection_background,
    })
}

fn parse_required_color(key: &str, value: Option<&str>) -> Result<Rgb, String> {
    let value = value.ok_or_else(|| format!("{key} is missing"))?;
    parse_color(key, value)
}

fn parse_color(key: &str, value: &str) -> Result<Rgb, String> {
    parse_hex(value).map_err(|err| format!("{key}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTERGLOW: &str = r#"
[colors.bright]
blue = '#6c99bb'

[colors.normal]
red = '#ac4142'
green = '#7e8e50'
yellow = '#e5b567'
blue = '#6c99bb'
magenta = '#9f4e85'
cyan = '#7dd6cf'

[colors.primary]
background = '#212121'
foreground = '#d0d0d0'
"#;

    const LIGHT_SCHEME: &str = r#"
[colors.normal]
red = '#dc322f'
green = '#859900'
yellow = '#b58900'
blue = '#268bd2'
magenta = '#d33682'
cyan = '#2aa198'

[colors.primary]
background = '#fdf6e3'
foreground = '#657b83'
"#;

    #[test]
    fn alacritty_scheme_parses_and_derives() {
        let tones = parse_palette_tones(AFTERGLOW).expect("parse scheme");
        assert_eq!(tones.good, (0x7e, 0x8e, 0x50));
        assert_eq!(tones.warn, (0xe5, 0xb5, 0x67));
        assert_eq!(tones.alarm, (0xac, 0x41, 0x42));
        assert_eq!(tones.accent, (0x7d, 0xd6, 0xcf));
        assert_eq!(tones.cool, (0x6c, 0x99, 0xbb));
        assert_eq!(tones.meta, (0x9f, 0x4e, 0x85));
        // Selection is its own bright cool tone, lifted off the data-cool slot —
        // never a raw copy of bright.blue — so the selected card never borrows a
        // token color.
        assert_ne!(tones.selection, tones.cool);
        // Caution warms the yellow into an amber distinct from both neighbors.
        assert_ne!(tones.caution, tones.warn);
        assert_ne!(tones.caution, tones.alarm);
    }

    #[test]
    fn selection_derives_a_distinct_bright_tone_and_a_band_when_scheme_omits_them() {
        // No bright.blue and no colors.selection: the selection source falls back
        // to normal blue, the selection tone still derives bright and distinct from
        // the data-cool slot, and the band derives a deep tint of the background.
        let tones = parse_palette_tones(LIGHT_SCHEME).expect("parse scheme");
        assert_eq!(tones.cool, (0x26, 0x8b, 0xd2));
        assert_ne!(tones.selection, tones.cool);
        assert_ne!(
            tones.selection_bg,
            (0xfd, 0xf6, 0xe3),
            "the band is a derived tint, not the raw background"
        );
    }

    #[test]
    fn alacritty_selection_background_feeds_the_band_when_present() {
        const WITH_SELECTION: &str = r#"
[colors.selection]
background = '#283457'

[colors.normal]
red = '#f7768e'
green = '#9ece6a'
yellow = '#e0af68'
blue = '#7aa2f7'
magenta = '#bb9af7'
cyan = '#7dcfff'

[colors.primary]
background = '#1a1b26'
foreground = '#c0caf5'
"#;
        let tones = parse_palette_tones(WITH_SELECTION).expect("parse scheme");
        // The band is the scheme's text-selection background pulled most of the
        // way back toward the background — subdued for a full-card fill, so it
        // equals neither the raw selection color nor the background, and its blue
        // sits between the two.
        assert_ne!(tones.selection_bg, (0x28, 0x34, 0x57));
        assert_ne!(tones.selection_bg, (0x1a, 0x1b, 0x26));
        assert!(
            tones.selection_bg.2 > 0x26 && tones.selection_bg.2 < 0x57,
            "the band's blue sits between background and the raw selection: {:?}",
            tones.selection_bg
        );
    }

    #[test]
    fn alacritty_missing_required_normal_entry_is_named_error() {
        let err = parse_palette_tones(
            r#"
[colors.normal]
red = '#ac4142'
yellow = '#e5b567'
blue = '#6c99bb'
magenta = '#9f4e85'
cyan = '#7dd6cf'

[colors.primary]
background = '#212121'
foreground = '#d0d0d0'
"#,
        )
        .expect_err("missing green should fail");
        assert_eq!(err, "colors.normal.green is missing");
    }

    #[test]
    fn light_alacritty_theme_ladder_darkens_toward_foreground() {
        let tones = parse_palette_tones(LIGHT_SCHEME).expect("parse scheme");
        assert!(
            tones.body.0 < tones.muted.0 && tones.muted.0 < tones.faint.0,
            "light scheme ladder darkens toward foreground"
        );
    }

    #[test]
    fn embedded_name_resolves_to_palette() {
        assert!(
            explicit_palette_tones("Afterglow").is_some(),
            "the vendored Alacritty catalog should include Afterglow"
        );
    }

    #[test]
    fn available_scheme_names_list_the_bundled_catalog() {
        let names = available_scheme_names();
        assert!(names.iter().any(|name| name == "TokyoNight Night"));
        assert!(names.iter().any(|name| name == "Catppuccin Mocha"));
        assert!(names.windows(2).all(|pair| pair[0] <= pair[1]), "sorted");
    }

    #[test]
    fn removed_builtin_names_no_longer_resolve() {
        for removed in ["clay", "slate", "classic"] {
            assert!(
                explicit_palette_tones(removed).is_none(),
                "`{removed}` was retired in favour of the bundled catalog"
            );
            assert!(validate_explicit_scheme(removed).is_err());
        }
    }
}
