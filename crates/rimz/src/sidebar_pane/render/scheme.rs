use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Deserialize;

use crate::config::parse_hex;

use super::embedded_themes;
use super::theme::{PaletteTones, builtin_palette_tones};

type Rgb = (u8, u8, u8);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AnsiScheme {
    palette: [Option<Rgb>; 16],
    background: Option<Rgb>,
    foreground: Option<Rgb>,
}

#[derive(Debug, Deserialize)]
struct AlacrittyTheme {
    colors: Option<AlacrittyColors>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittyColors {
    primary: Option<AlacrittyPrimaryColors>,
    normal: Option<AlacrittyAnsiColors>,
    bright: Option<AlacrittyAnsiColors>,
}

#[derive(Debug, Default, Deserialize)]
struct AlacrittyPrimaryColors {
    background: Option<String>,
    foreground: Option<String>,
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

pub(crate) fn explicit_palette_tones(name_or_path: &str) -> Option<PaletteTones> {
    if let Some(tones) = builtin_palette_tones(name_or_path) {
        return Some(tones);
    }
    cached_explicit_palette_tones(name_or_path)
}

pub fn validate_explicit_scheme(name_or_path: &str) -> Result<(), String> {
    load_explicit_palette_tones(name_or_path).map(|_| ())
}

fn cached_explicit_palette_tones(name_or_path: &str) -> Option<PaletteTones> {
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

fn lock_explicit_scheme_cache() -> MutexGuard<'static, HashMap<String, Option<PaletteTones>>> {
    static CACHED: OnceLock<Mutex<HashMap<String, Option<PaletteTones>>>> = OnceLock::new();
    match CACHED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn load_explicit_palette_tones(name_or_path: &str) -> Result<PaletteTones, String> {
    if let Some(tones) = builtin_palette_tones(name_or_path) {
        return Ok(tones);
    }
    if let Some(text) = embedded_themes::theme_toml(name_or_path) {
        return parse_palette_tones(text).map_err(|err| {
            format!("invalid bundled sidebar theme scheme `{name_or_path}`: {err}")
        });
    }
    load_external_palette_tones(name_or_path)
}

fn load_external_palette_tones(name_or_path: &str) -> Result<PaletteTones, String> {
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
        "builtins: clay, slate, classic; {} bundled Alacritty themes in crates/rimz/themes/alacritty; or a path to an Alacritty .toml",
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

pub(crate) fn parse_palette_tones(text: &str) -> Result<PaletteTones, String> {
    derive_palette(&parse_alacritty_scheme(text)?)
}

fn parse_alacritty_scheme(text: &str) -> Result<AnsiScheme, String> {
    let theme: AlacrittyTheme =
        toml::from_str(text).map_err(|err| format!("parsing Alacritty theme TOML: {err}"))?;
    let colors = theme.colors.unwrap_or_default();
    let mut scheme = AnsiScheme::default();

    let primary = colors.primary.unwrap_or_default();
    scheme.background = Some(parse_required_color(
        "colors.primary.background",
        primary.background.as_deref(),
    )?);
    scheme.foreground = Some(parse_required_color(
        "colors.primary.foreground",
        primary.foreground.as_deref(),
    )?);

    let normal = colors.normal.unwrap_or_default();
    set_required_palette_slot(&mut scheme, 1, "colors.normal.red", normal.red.as_deref())?;
    set_required_palette_slot(
        &mut scheme,
        2,
        "colors.normal.green",
        normal.green.as_deref(),
    )?;
    set_required_palette_slot(
        &mut scheme,
        3,
        "colors.normal.yellow",
        normal.yellow.as_deref(),
    )?;
    set_required_palette_slot(&mut scheme, 4, "colors.normal.blue", normal.blue.as_deref())?;
    set_required_palette_slot(
        &mut scheme,
        5,
        "colors.normal.magenta",
        normal.magenta.as_deref(),
    )?;
    set_required_palette_slot(&mut scheme, 6, "colors.normal.cyan", normal.cyan.as_deref())?;

    let bright_blue = colors
        .bright
        .as_ref()
        .and_then(|bright| bright.blue.as_deref())
        .or(normal.blue.as_deref());
    set_palette_slot(&mut scheme, 12, "colors.bright.blue", bright_blue)?;

    Ok(scheme)
}

fn parse_required_color(key: &str, value: Option<&str>) -> Result<Rgb, String> {
    let value = value.ok_or_else(|| format!("{key} is missing"))?;
    parse_color(key, value)
}

fn parse_optional_color(key: &str, value: Option<&str>) -> Result<Option<Rgb>, String> {
    value.map(|value| parse_color(key, value)).transpose()
}

fn set_required_palette_slot(
    scheme: &mut AnsiScheme,
    index: usize,
    key: &str,
    value: Option<&str>,
) -> Result<(), String> {
    scheme.palette[index] = Some(parse_required_color(key, value)?);
    Ok(())
}

fn set_palette_slot(
    scheme: &mut AnsiScheme,
    index: usize,
    key: &str,
    value: Option<&str>,
) -> Result<(), String> {
    scheme.palette[index] = parse_optional_color(key, value)?;
    Ok(())
}

fn parse_color(key: &str, value: &str) -> Result<Rgb, String> {
    parse_hex(value).map_err(|err| format!("{key}: {err}"))
}

fn derive_palette(scheme: &AnsiScheme) -> Result<PaletteTones, String> {
    let color = |index: usize| {
        scheme.palette[index].ok_or_else(|| format!("palette index {index} is missing"))
    };
    let background = scheme
        .background
        .ok_or_else(|| "background is missing".to_owned())?;
    let foreground = scheme
        .foreground
        .ok_or_else(|| "foreground is missing".to_owned())?;
    let red = color(1)?;
    let green = color(2)?;
    let yellow = color(3)?;
    let blue = color(4)?;
    let magenta = color(5)?;
    let cyan = color(6)?;
    let bright_blue = color(12)?;
    Ok(PaletteTones {
        good: green,
        warn: yellow,
        caution: blend_oklab(yellow, red, 0.5),
        alarm: red,
        accent: cyan,
        cool: blue,
        meta: magenta,
        soft: blend_oklab(background, foreground, 0.65),
        dim: blend_oklab(background, foreground, 0.45),
        faint: blend_oklab(background, foreground, 0.25),
        rule: blend_oklab(background, foreground, 0.18),
        selection: bright_blue,
    })
}

pub(super) fn blend_oklab(left: Rgb, right: Rgb, amount: f32) -> Rgb {
    let left = Oklab::from_rgb(left);
    let right = Oklab::from_rgb(right);
    Oklab {
        l: lerp(left.l, right.l, amount),
        a: lerp(left.a, right.a, amount),
        b: lerp(left.b, right.b, amount),
    }
    .to_rgb()
}

pub(super) fn lift_lightness(rgb: Rgb, delta: f32) -> Rgb {
    let mut color = Oklab::from_rgb(rgb);
    color.l = (color.l + delta).clamp(0.0, 1.0);
    color.to_rgb()
}

fn lerp(left: f32, right: f32, amount: f32) -> f32 {
    left + (right - left) * amount
}

#[derive(Clone, Copy, Debug)]
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

impl Oklab {
    fn from_rgb((red, green, blue): Rgb) -> Self {
        let r = srgb_to_linear(red);
        let g = srgb_to_linear(green);
        let b = srgb_to_linear(blue);

        let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
        let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
        let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;

        let l_ = l.cbrt();
        let m_ = m.cbrt();
        let s_ = s.cbrt();

        Self {
            l: 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
            a: 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
            b: 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
        }
    }

    fn to_rgb(self) -> Rgb {
        let l_ = self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b;
        let m_ = self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b;
        let s_ = self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        let red = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
        let green = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s;
        let blue = -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s;

        (
            linear_to_srgb(red),
            linear_to_srgb(green),
            linear_to_srgb(blue),
        )
    }
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let value = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
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
        assert_eq!(tones.selection, (0x6c, 0x99, 0xbb));
        assert_ne!(tones.caution, tones.warn);
        assert_ne!(tones.caution, tones.alarm);
    }

    #[test]
    fn alacritty_selection_falls_back_to_normal_blue_when_bright_missing() {
        let tones = parse_palette_tones(LIGHT_SCHEME).expect("parse scheme");
        assert_eq!(tones.cool, (0x26, 0x8b, 0xd2));
        assert_eq!(tones.selection, tones.cool);
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
            tones.soft.0 < tones.dim.0 && tones.dim.0 < tones.faint.0,
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
    fn lift_lightness_preserves_oklab_hue_axes() {
        let base = Oklab::from_rgb((0xdf, 0xb6, 0x6d));
        let lifted = Oklab::from_rgb(lift_lightness((0xdf, 0xb6, 0x6d), 0.05));
        assert!(
            lifted.l > base.l,
            "lightness should move upward through OKLab"
        );
        assert!((lifted.a - base.a).abs() < 0.01);
        assert!((lifted.b - base.b).abs() < 0.01);
    }
}
