use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use super::{PaletteRole, parse_hex};

/// `[theme.animations]`: per-machine status-head animation overrides. Each
/// role is optional and each field inside a role is optional, so a user can
/// change one glyph run without copying the shipped color or cadence. `unread`
/// is the one cross-cutting knob: it picks how unread attention rows read,
/// shared across the lead glyph, the card name, the description, and the
/// make-up buckets.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ThemeAnimationsConfig {
    /// How an unread attention row carries its signal — a flowing `shimmer`
    /// (default), a constant `bright`, or the hard 2-pole `blink`. Unset keeps
    /// the default shimmer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread: Option<UnreadEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacting: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegating: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolving: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<AnimationSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<AnimationSpec>,
}

impl ThemeAnimationsConfig {
    /// Whether every role is unset — the serialized config omits the section.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    /// Whether calm status heads need a cosmetic animation tick.
    pub fn has_resting_motion(&self) -> bool {
        self.paused.as_ref().is_some_and(AnimationSpec::has_motion)
            || self.idle.as_ref().is_some_and(AnimationSpec::has_motion)
            || self.success.as_ref().is_some_and(AnimationSpec::has_motion)
    }
}

impl<'de> Deserialize<'de> for ThemeAnimationsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct RawThemeAnimationsConfig {
            unread: Option<UnreadEffect>,
            thinking: Option<AnimationSpec>,
            working: Option<AnimationSpec>,
            compacting: Option<AnimationSpec>,
            delegating: Option<AnimationSpec>,
            resolving: Option<AnimationSpec>,
            idle: Option<AnimationSpec>,
            success: Option<AnimationSpec>,
            paused: Option<AnimationSpec>,
            waiting: Option<AnimationSpec>,
            failed: Option<AnimationSpec>,
        }

        let raw = RawThemeAnimationsConfig::deserialize(deserializer)?;
        let config = Self {
            unread: raw.unread,
            thinking: raw.thinking,
            working: raw.working,
            compacting: raw.compacting,
            delegating: raw.delegating,
            resolving: raw.resolving,
            idle: raw.idle,
            success: raw.success,
            paused: raw.paused,
            waiting: raw.waiting,
            failed: raw.failed,
        };
        Ok(config)
    }
}

/// One role override under `[theme.animations.<role>]`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AnimationSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<AnimationFrames>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<AnimationColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<AnimationEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<AnimationSpeed>,
}

impl AnimationSpec {
    fn has_motion(&self) -> bool {
        self.has_frame_motion()
            || self
                .effect
                .is_some_and(|effect| effect != AnimationEffect::Static)
    }

    pub(crate) fn has_frame_motion(&self) -> bool {
        self.frames.as_ref().is_some_and(|frames| frames.len() > 1)
    }

    pub(crate) fn disables_effect_motion(&self) -> bool {
        self.effect == Some(AnimationEffect::Static)
    }
}

/// The glyph frames for one animation. A TOML string splits into Unicode
/// scalar values; an array keeps explicit multi-scalar glyphs intact.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AnimationFrames(Vec<String>);

impl AnimationFrames {
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn parse(frames: Vec<String>) -> Result<Self, String> {
        if frames.is_empty() {
            return Err("animation frames must not be empty".to_owned());
        }
        for frame in &frames {
            validate_single_cell(frame).map_err(|err| format!("animation frames {err}"))?;
        }
        Ok(Self(frames))
    }
}

pub fn validate_single_cell(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("must not contain empty glyphs".to_owned());
    }
    let width = ratatui::text::Span::raw(value).width();
    if width != 1 {
        return Err(format!(
            "must occupy exactly one terminal cell; `{value}` is {width} cells"
        ));
    }
    Ok(())
}

/// A glyph occupies one cell, or two when a double-width Nerd Font icon reserves
/// its second cell with a trailing space. Empty, zero-width, and over-wide
/// values are rejected so a glyph never bleeds past two columns.
pub fn validate_glyph_cells(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("must not contain empty glyphs".to_owned());
    }
    let width = ratatui::text::Span::raw(value).width();
    if !(1..=2).contains(&width) {
        return Err(format!(
            "must occupy one or two terminal cells; `{value}` is {width} cells"
        ));
    }
    Ok(())
}

impl<'de> Deserialize<'de> for AnimationFrames {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawFrames {
            String(String),
            Array(Vec<String>),
        }

        let frames = match RawFrames::deserialize(deserializer)? {
            RawFrames::String(value) => value.chars().map(|ch| ch.to_string()).collect(),
            RawFrames::Array(values) => values,
        };
        Self::parse(frames).map_err(de::Error::custom)
    }
}

/// A semantic animation tone or a raw 256-color index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationColor {
    Good,
    Warn,
    Caution,
    Alarm,
    Accent,
    Cool,
    Meta,
    Body,
    Muted,
    Faint,
    Clay,
    Role(PaletteRole),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl AnimationColor {
    fn name(self) -> Option<&'static str> {
        match self {
            Self::Good => Some("good"),
            Self::Warn => Some("warn"),
            Self::Caution => Some("caution"),
            Self::Alarm => Some("alarm"),
            Self::Accent => Some("accent"),
            Self::Cool => Some("cool"),
            Self::Meta => Some("meta"),
            Self::Body => Some("body"),
            Self::Muted => Some("muted"),
            Self::Faint => Some("faint"),
            Self::Clay => Some("clay"),
            Self::Role(role) => Some(role.name()),
            Self::Indexed(_) | Self::Rgb(_, _, _) => None,
        }
    }

    fn parse_name(value: &str) -> Option<Self> {
        match value {
            "good" => Some(Self::Good),
            "warn" => Some(Self::Warn),
            "caution" => Some(Self::Caution),
            "alarm" => Some(Self::Alarm),
            "accent" => Some(Self::Accent),
            "cool" => Some(Self::Cool),
            "meta" => Some(Self::Meta),
            "body" => Some(Self::Body),
            "muted" => Some(Self::Muted),
            "faint" => Some(Self::Faint),
            "clay" => Some(Self::Clay),
            _ => PaletteRole::parse(value).map(Self::Role),
        }
    }
}

impl Serialize for AnimationColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(name) = self.name() {
            serializer.serialize_str(name)
        } else {
            match self {
                Self::Indexed(index) => serializer.serialize_u8(*index),
                Self::Rgb(red, green, blue) => {
                    serializer.serialize_str(&format!("#{red:02x}{green:02x}{blue:02x}"))
                }
                _ => unreachable!("named colors are handled above"),
            }
        }
    }
}

impl<'de> Deserialize<'de> for AnimationColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = AnimationColor;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a lowercase color name, #rrggbb hex color, or 256-color index")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if let Some(color) = AnimationColor::parse_name(value) {
                    return Ok(color);
                }
                if value.starts_with('#') {
                    return parse_hex(value)
                        .map(|(red, green, blue)| AnimationColor::Rgb(red, green, blue))
                        .map_err(E::custom);
                }
                Err(E::custom(format!(
                    "unknown animation color `{value}`; expected semantic slot, palette role, clay, #rrggbb, or 0-255"
                )))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u8::try_from(value)
                    .map(AnimationColor::Indexed)
                    .map_err(|_| E::custom("animation color index must be in 0..=255"))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u8::try_from(value)
                    .map(AnimationColor::Indexed)
                    .map_err(|_| E::custom("animation color index must be in 0..=255"))
            }
        }

        deserializer.deserialize_any(ColorVisitor)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnimationEffect {
    Static,
    Breathe,
}

/// How an unread attention row carries its signal, shared by the lead glyph, the
/// card name, the description, and the cockpit make-up buckets. A per-role
/// `effect = "static"` quiet still wins over any of these, falling back to a
/// constant bold tone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UnreadEffect {
    /// A light beam flows left-to-right across each element on its own.
    #[default]
    Shimmer,
    /// The bright crest held constant — brighter and bold, no motion.
    Bright,
    /// The hard 2-pole brightness toggle between the resting tone and the crest.
    Blink,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnimationSpeed {
    Slow,
    Normal,
    Fast,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_frames_split_per_unicode_scalar() {
        let frames = toml::from_str::<FrameWrap>("value = \"⠁⠂⠄⡀\"")
            .expect("frame wrap")
            .value;
        assert_eq!(frames.as_slice(), ["⠁", "⠂", "⠄", "⡀"]);
    }

    #[test]
    fn array_frames_keep_multi_codepoint_glyphs() {
        let parsed: FrameWrap =
            toml::from_str("value = [\"⏸︎\", \"✓\"]").expect("array frames parse");
        assert_eq!(parsed.value.as_slice(), ["⏸︎", "✓"]);
    }

    #[test]
    fn frames_reject_empty_input() {
        assert!(toml::from_str::<FrameWrap>("value = \"\"").is_err());
        assert!(toml::from_str::<FrameWrap>("value = []").is_err());
        assert!(toml::from_str::<FrameWrap>("value = [\"\"]").is_err());
    }

    #[test]
    fn frames_reject_non_single_cell_glyphs() {
        assert!(toml::from_str::<FrameWrap>("value = [\"...\"]").is_err());
        assert!(toml::from_str::<FrameWrap>("value = [\"   \"]").is_err());
        assert!(toml::from_str::<FrameWrap>("value = [\"🚀\"]").is_err());
        assert!(toml::from_str::<FrameWrap>("value = [\"\\u0301\"]").is_err());
    }

    #[test]
    fn color_accepts_names_and_indexes() {
        let named: ColorWrap = toml::from_str("value = \"clay\"").expect("named color");
        assert_eq!(named.value, AnimationColor::Clay);
        let caution: ColorWrap = toml::from_str("value = \"caution\"").expect("caution color");
        assert_eq!(caution.value, AnimationColor::Caution);
        let indexed: ColorWrap = toml::from_str("value = 173").expect("indexed color");
        assert_eq!(indexed.value, AnimationColor::Indexed(173));
        let rgb: ColorWrap = toml::from_str("value = \"#2FB1D1\"").expect("rgb color");
        assert_eq!(rgb.value, AnimationColor::Rgb(0x2f, 0xb1, 0xd1));
    }

    #[test]
    fn color_rejects_unknown_or_out_of_range_values() {
        assert!(toml::from_str::<ColorWrap>("value = \"orange\"").is_err());
        assert!(toml::from_str::<ColorWrap>("value = 300").is_err());
        assert!(toml::from_str::<ColorWrap>("value = -1").is_err());
    }

    #[test]
    fn effect_and_speed_parse_lowercase_values() {
        let parsed: SpecWrap =
            toml::from_str("value = { effect = \"breathe\", speed = \"fast\" }").expect("spec");
        assert_eq!(parsed.value.effect, Some(AnimationEffect::Breathe));
        assert_eq!(parsed.value.speed, Some(AnimationSpeed::Fast));
        assert!(toml::from_str::<SpecWrap>("value = { effect = \"blink\" }").is_err());
        assert!(toml::from_str::<SpecWrap>("value = { effect = \"pulse\" }").is_err());
        assert!(toml::from_str::<SpecWrap>("value = { speed = \"instant\" }").is_err());
    }

    #[test]
    fn partial_specs_and_unset_config_round_trip() {
        let config: ThemeAnimationsConfig =
            toml::from_str("[thinking]\nframes = \"ab\"\n").expect("config");
        assert!(!config.is_unset());
        let thinking = config.thinking.expect("thinking override");
        assert_eq!(
            thinking.frames.expect("frames").as_slice(),
            ["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(thinking.color, None);
        assert!(ThemeAnimationsConfig::default().is_unset());
    }

    #[test]
    fn unread_effect_parses_each_mode_and_defaults_to_shimmer() {
        for (value, expected) in [
            ("shimmer", UnreadEffect::Shimmer),
            ("bright", UnreadEffect::Bright),
            ("blink", UnreadEffect::Blink),
        ] {
            let config: ThemeAnimationsConfig =
                toml::from_str(&format!("unread = \"{value}\"\n")).expect("config");
            assert_eq!(config.unread, Some(expected));
            assert!(!config.is_unset());
        }
        assert_eq!(UnreadEffect::default(), UnreadEffect::Shimmer);
        assert_eq!(ThemeAnimationsConfig::default().unread, None);
    }

    #[test]
    fn unread_effect_rejects_unknown_value() {
        assert!(toml::from_str::<ThemeAnimationsConfig>("unread = \"glow\"\n").is_err());
    }

    #[test]
    fn unset_unread_omits_the_key_on_serialize() {
        let toml = toml::to_string(&ThemeAnimationsConfig::default()).expect("serialize");
        assert!(
            !toml.contains("unread"),
            "unset unread is not written: {toml}"
        );
        let set = ThemeAnimationsConfig {
            unread: Some(UnreadEffect::Blink),
            ..ThemeAnimationsConfig::default()
        };
        assert!(
            toml::to_string(&set)
                .expect("serialize")
                .contains("unread = \"blink\"")
        );
    }

    #[test]
    fn attention_and_paused_roles_accept_multiple_frames() {
        assert!(toml::from_str::<ThemeAnimationsConfig>("[waiting]\nframes = \"?\"\n").is_ok());
        assert!(toml::from_str::<ThemeAnimationsConfig>("[failed]\nframes = \"!\"\n").is_ok());
        assert!(toml::from_str::<ThemeAnimationsConfig>("[paused]\nframes = [\"⏸︎\"]\n").is_ok());
        assert!(
            toml::from_str::<ThemeAnimationsConfig>("[failed]\nframes = [\"!\", \"?\"]\n").is_ok()
        );
        assert!(
            toml::from_str::<ThemeAnimationsConfig>("[paused]\nframes = [\"⏸︎\", \"○\"]\n").is_ok()
        );
    }

    #[derive(Deserialize)]
    struct FrameWrap {
        value: AnimationFrames,
    }

    #[derive(Deserialize)]
    struct ColorWrap {
        value: AnimationColor,
    }

    #[derive(Deserialize)]
    struct SpecWrap {
        value: AnimationSpec,
    }
}
