use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Palette depth the sidebar renderer may emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    Indexed,
    Truecolor,
}

/// The twelve semantic palette tones, as raw RGB. One neutral home shared by
/// the sidebar renderer (which quantizes these to the active [`ColorDepth`])
/// and the CLI presentation layer (which emits them as ANSI). Pure data: no
/// renderer or terminal dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteTones {
    pub good: (u8, u8, u8),
    pub warn: (u8, u8, u8),
    pub caution: (u8, u8, u8),
    pub alarm: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    pub cool: (u8, u8, u8),
    pub meta: (u8, u8, u8),
    pub soft: (u8, u8, u8),
    pub dim: (u8, u8, u8),
    pub faint: (u8, u8, u8),
    pub rule: (u8, u8, u8),
    pub selection: (u8, u8, u8),
}

impl PaletteTones {
    /// The derived `TokyoNight Night` tones, baked in as the infallible default
    /// so sidebar resolution never fails even when the bundled catalog is
    /// unreadable. `default_const_matches_bundled_default` keeps these in
    /// lockstep with the catalog; the CLI palette (`cli::render`) styles its
    /// output from these same tones.
    pub const DEFAULT: Self = Self {
        good: (0x9e, 0xce, 0x6a),
        warn: (0xe0, 0xaf, 0x68),
        caution: (0xed, 0x95, 0x7d),
        alarm: (0xf7, 0x76, 0x8e),
        accent: (0x7d, 0xcf, 0xff),
        cool: (0x7a, 0xa2, 0xf7),
        meta: (0xbb, 0x9a, 0xf7),
        soft: (0x80, 0x87, 0xa6),
        dim: (0x5e, 0x63, 0x7b),
        faint: (0x3e, 0x41, 0x53),
        rule: (0x34, 0x36, 0x46),
        selection: (0x7a, 0xa2, 0xf7),
    };
}

/// `[sidebar.theme] mode`: how the renderer chooses palette color depth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    Auto,
    Truecolor,
    Indexed,
}

impl ThemeMode {
    pub fn depth(self, truecolor_advertised: bool) -> ColorDepth {
        match self {
            Self::Auto if truecolor_advertised => ColorDepth::Truecolor,
            Self::Auto | Self::Indexed => ColorDepth::Indexed,
            Self::Truecolor => ColorDepth::Truecolor,
        }
    }
}

impl Serialize for ThemeMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Auto => "auto",
            Self::Truecolor => "truecolor",
            Self::Indexed => "256",
        })
    }
}

impl<'de> Deserialize<'de> for ThemeMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ThemeModeVisitor;

        impl Visitor<'_> for ThemeModeVisitor {
            type Value = ThemeMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(r#""auto", "truecolor", "256", or integer 256"#)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "auto" => Ok(ThemeMode::Auto),
                    "truecolor" => Ok(ThemeMode::Truecolor),
                    "256" => Ok(ThemeMode::Indexed),
                    _ => Err(E::custom(format!(
                        "unknown theme mode `{value}`; expected auto, truecolor, or 256"
                    ))),
                }
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 256 {
                    Ok(ThemeMode::Indexed)
                } else {
                    Err(E::custom("theme mode integer must be 256"))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 256 {
                    Ok(ThemeMode::Indexed)
                } else {
                    Err(E::custom("theme mode integer must be 256"))
                }
            }
        }

        deserializer.deserialize_any(ThemeModeVisitor)
    }
}

/// A user-provided display color: either a 256-color index or an RGB hex tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl ThemeColor {
    pub fn indexed(self) -> u8 {
        match self {
            Self::Indexed(index) => index,
            Self::Rgb(red, green, blue) => nearest_xterm_index(red, green, blue),
        }
    }
}

impl Serialize for ThemeColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Indexed(index) => serializer.serialize_u8(*index),
            Self::Rgb(red, green, blue) => {
                serializer.serialize_str(&format!("#{red:02x}{green:02x}{blue:02x}"))
            }
        }
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = ThemeColor;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a 256-color index or #rrggbb hex color")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                parse_hex(value)
                    .map(|(red, green, blue)| ThemeColor::Rgb(red, green, blue))
                    .map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u8::try_from(value)
                    .map(ThemeColor::Indexed)
                    .map_err(|_| E::custom("color index must be in 0..=255"))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u8::try_from(value)
                    .map(ThemeColor::Indexed)
                    .map_err(|_| E::custom("color index must be in 0..=255"))
            }
        }

        deserializer.deserialize_any(ColorVisitor)
    }
}

pub fn parse_hex(value: &str) -> Result<(u8, u8, u8), String> {
    let Some(hex) = value.strip_prefix('#') else {
        return Err("hex colors must start with #".to_owned());
    };
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("hex colors must use #rrggbb".to_owned());
    }
    let red = u8::from_str_radix(&hex[0..2], 16).map_err(|err| err.to_string())?;
    let green = u8::from_str_radix(&hex[2..4], 16).map_err(|err| err.to_string())?;
    let blue = u8::from_str_radix(&hex[4..6], 16).map_err(|err| err.to_string())?;
    Ok((red, green, blue))
}

pub fn nearest_xterm_index(red: u8, green: u8, blue: u8) -> u8 {
    let mut best = (u32::MAX, 16_u8);
    for index in 16..=231 {
        let (r, g, b) = xterm_rgb(index);
        let distance = distance_squared((red, green, blue), (r, g, b));
        if distance < best.0 {
            best = (distance, index);
        }
    }
    for index in 232..=255 {
        let (r, g, b) = xterm_rgb(index);
        let distance = distance_squared((red, green, blue), (r, g, b));
        if distance < best.0 {
            best = (distance, index);
        }
    }
    best.1
}

pub(crate) fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    debug_assert!(index >= 16);
    if index >= 232 {
        let value = 8 + (index - 232) * 10;
        return (value, value, value);
    }
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let offset = index - 16;
    let red = LEVELS[(offset / 36) as usize];
    let green = LEVELS[((offset % 36) / 6) as usize];
    let blue = LEVELS[(offset % 6) as usize];
    (red, green, blue)
}

fn distance_squared(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let dr = i32::from(left.0) - i32::from(right.0);
    let dg = i32::from(left.1) - i32::from(right.1);
    let db = i32::from(left.2) - i32::from(right.2);
    (dr * dr + dg * dg + db * db) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Serialize)]
    struct ColorWrap {
        value: ThemeColor,
    }

    #[derive(Deserialize, Serialize)]
    struct ModeWrap {
        value: ThemeMode,
    }

    #[test]
    fn theme_color_accepts_indexes_and_hex() {
        let indexed: ColorWrap = toml::from_str("value = 173").expect("indexed");
        assert_eq!(indexed.value, ThemeColor::Indexed(173));

        let rgb: ColorWrap = toml::from_str("value = \"#D97757\"").expect("rgb");
        assert_eq!(rgb.value, ThemeColor::Rgb(0xd9, 0x77, 0x57));
        assert_eq!(
            toml::to_string(&rgb).expect("serialize"),
            "value = \"#d97757\"\n"
        );
    }

    #[test]
    fn theme_color_rejects_bad_values() {
        assert!(toml::from_str::<ColorWrap>("value = \"d97757\"").is_err());
        assert!(toml::from_str::<ColorWrap>("value = \"#12345\"").is_err());
        assert!(toml::from_str::<ColorWrap>("value = 300").is_err());
        assert!(toml::from_str::<ColorWrap>("value = -1").is_err());
    }

    #[test]
    fn theme_mode_accepts_strings_and_integer_256() {
        let auto: ModeWrap = toml::from_str("value = \"auto\"").expect("auto");
        assert_eq!(auto.value, ThemeMode::Auto);
        let truecolor: ModeWrap = toml::from_str("value = \"truecolor\"").expect("truecolor");
        assert_eq!(truecolor.value, ThemeMode::Truecolor);
        let indexed: ModeWrap = toml::from_str("value = \"256\"").expect("indexed string");
        assert_eq!(indexed.value, ThemeMode::Indexed);
        let indexed: ModeWrap = toml::from_str("value = 256").expect("indexed integer");
        assert_eq!(indexed.value, ThemeMode::Indexed);
        assert!(toml::from_str::<ModeWrap>("value = 16").is_err());
    }

    #[test]
    fn theme_mode_depth_truth_table() {
        assert_eq!(ThemeMode::Auto.depth(false), ColorDepth::Indexed);
        assert_eq!(ThemeMode::Auto.depth(true), ColorDepth::Truecolor);
        assert_eq!(ThemeMode::Indexed.depth(true), ColorDepth::Indexed);
        assert_eq!(ThemeMode::Truecolor.depth(false), ColorDepth::Truecolor);
    }

    #[test]
    fn quantizer_uses_xterm_cube_and_gray_ramp() {
        assert_eq!(nearest_xterm_index(0, 0, 0), 16);
        assert_eq!(nearest_xterm_index(255, 255, 255), 231);
        assert_eq!(nearest_xterm_index(0, 95, 135), 24);
        assert_eq!(nearest_xterm_index(8, 8, 8), 232);
        assert_eq!(nearest_xterm_index(238, 238, 238), 255);
    }
}
