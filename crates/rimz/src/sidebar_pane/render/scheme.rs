use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::config::parse_hex;
use crate::ledger::paths::config_home;

use super::theme::{PaletteTones, builtin_palette_tones};

type Rgb = (u8, u8, u8);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GhosttyScheme {
    palette: [Option<Rgb>; 16],
    background: Option<Rgb>,
    foreground: Option<Rgb>,
}

pub(crate) fn auto_palette_tones() -> Option<PaletteTones> {
    static CACHED: OnceLock<Option<PaletteTones>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let config = std::fs::read_to_string(config_home().join("ghostty").join("config")).ok()?;
        let theme = active_theme_name(&config)?;
        explicit_palette_tones(&theme)
    })
}

pub(crate) fn explicit_palette_tones(name_or_path: &str) -> Option<PaletteTones> {
    if let Some(tones) = builtin_palette_tones(name_or_path) {
        return Some(tones);
    }
    cached_external_palette_tones(name_or_path)
}

pub fn validate_explicit_scheme(name_or_path: &str) -> Result<(), String> {
    load_explicit_palette_tones(name_or_path).map(|_| ())
}

fn cached_external_palette_tones(name_or_path: &str) -> Option<PaletteTones> {
    {
        let cache = lock_explicit_scheme_cache();
        if let Some(tones) = cache.get(name_or_path) {
            return *tones;
        }
    }

    let tones = load_external_palette_tones(name_or_path).ok();
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
    load_external_palette_tones(name_or_path)
}

fn load_external_palette_tones(name_or_path: &str) -> Result<PaletteTones, String> {
    let path = resolve_scheme_path(name_or_path).ok_or_else(|| {
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
    let user = config_home().join("ghostty").join("themes");
    match std::env::var_os("GHOSTTY_RESOURCES_DIR") {
        Some(resources) => format!(
            "builtins: clay, slate, classic; Ghostty themes: {}, {}",
            user.display(),
            PathBuf::from(resources).join("themes").display()
        ),
        None => format!(
            "builtins: clay, slate, classic; Ghostty themes: {} or $GHOSTTY_RESOURCES_DIR/themes",
            user.display()
        ),
    }
}

fn active_theme_name(config: &str) -> Option<String> {
    config
        .lines()
        .rev()
        .filter_map(line_key_value)
        .find_map(|(key, value)| {
            (key == "theme")
                .then(|| choose_dark_theme(value))
                .filter(|value| !value.is_empty())
        })
}

fn choose_dark_theme(value: &str) -> String {
    let value = strip_quotes(value.trim());
    for part in value.split(',') {
        let part = part.trim();
        if let Some(theme) = part.strip_prefix("dark:") {
            return strip_quotes(theme.trim()).to_owned();
        }
    }
    strip_quotes(value.split(',').next().unwrap_or(value).trim()).to_owned()
}

fn resolve_scheme_path(name_or_path: &str) -> Option<PathBuf> {
    scheme_candidates(name_or_path)
        .into_iter()
        .find(|path| path.is_file())
}

fn scheme_candidates(name_or_path: &str) -> Vec<PathBuf> {
    let mut paths = vec![
        config_home()
            .join("ghostty")
            .join("themes")
            .join(name_or_path),
    ];
    if let Some(resources) = std::env::var_os("GHOSTTY_RESOURCES_DIR") {
        paths.push(PathBuf::from(resources).join("themes").join(name_or_path));
    }
    paths.push(expand_home(Path::new(name_or_path)));
    paths
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
    derive_palette(&parse_ghostty_scheme(text)?)
}

fn parse_ghostty_scheme(text: &str) -> Result<GhosttyScheme, String> {
    let mut scheme = GhosttyScheme::default();
    for (key, value) in text.lines().filter_map(line_key_value) {
        match key {
            "palette" => {
                let (index, color) = value
                    .split_once('=')
                    .ok_or_else(|| format!("invalid palette entry `{value}`"))?;
                let index = index
                    .trim()
                    .parse::<usize>()
                    .map_err(|err| format!("invalid palette index `{index}`: {err}"))?;
                if index >= scheme.palette.len() {
                    return Err(format!("palette index {index} is outside 0..=15"));
                }
                scheme.palette[index] = Some(parse_hex(strip_quotes(color.trim()))?);
            }
            "background" => {
                scheme.background = Some(parse_hex(strip_quotes(value.trim()))?);
            }
            "foreground" => {
                scheme.foreground = Some(parse_hex(strip_quotes(value.trim()))?);
            }
            _ => {}
        }
    }
    Ok(scheme)
}

fn line_key_value(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

fn strip_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn derive_palette(scheme: &GhosttyScheme) -> Result<PaletteTones, String> {
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

    const DARK_SCHEME: &str = r#"
background = #1a1b26
foreground = #c0caf5
palette = 1=#f7768e
palette = 2=#9ece6a
palette = 3=#e0af68
palette = 4=#7aa2f7
palette = 5=#bb9af7
palette = 6=#7dcfff
palette = 12=#7aa2f7
"#;

    const LIGHT_SCHEME: &str = r#"
background = #fdf6e3
foreground = #657b83
palette = 1=#dc322f
palette = 2=#859900
palette = 3=#b58900
palette = 4=#268bd2
palette = 5=#d33682
palette = 6=#2aa198
palette = 12=#268bd2
"#;

    #[test]
    fn ghostty_scheme_derives_semantic_slots() {
        let tones = parse_palette_tones(DARK_SCHEME).expect("parse scheme");
        assert_eq!(tones.good, (0x9e, 0xce, 0x6a));
        assert_eq!(tones.warn, (0xe0, 0xaf, 0x68));
        assert_eq!(tones.alarm, (0xf7, 0x76, 0x8e));
        assert_eq!(tones.accent, (0x7d, 0xcf, 0xff));
        assert_eq!(tones.cool, (0x7a, 0xa2, 0xf7));
        assert_eq!(tones.meta, (0xbb, 0x9a, 0xf7));
        assert_eq!(tones.selection, (0x7a, 0xa2, 0xf7));
        assert_ne!(tones.caution, tones.warn);
        assert_ne!(tones.caution, tones.alarm);
    }

    #[test]
    fn gray_ladder_follows_background_to_foreground_direction() {
        let dark = parse_palette_tones(DARK_SCHEME).expect("dark");
        let light = parse_palette_tones(LIGHT_SCHEME).expect("light");
        assert!(
            dark.soft.0 > dark.dim.0 && dark.dim.0 > dark.faint.0,
            "dark scheme ladder lightens toward foreground"
        );
        assert!(
            light.soft.0 < light.dim.0 && light.dim.0 < light.faint.0,
            "light scheme ladder darkens toward foreground"
        );
    }

    #[test]
    fn ghostty_config_theme_prefers_dark_side() {
        assert_eq!(
            active_theme_name("theme = dark:TokyoNight,light:Solarized Light\n").as_deref(),
            Some("TokyoNight")
        );
        assert_eq!(
            active_theme_name("theme = \"Solarized Light\"\n").as_deref(),
            Some("Solarized Light")
        );
    }

    #[test]
    fn ghostty_config_theme_uses_last_assignment() {
        assert_eq!(
            active_theme_name("theme = Old\nfont-size = 14\ntheme = New\n").as_deref(),
            Some("New")
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
