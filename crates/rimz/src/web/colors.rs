//! Shared browser-terminal palette projection.

use serde_json::{Map, Value};

use crate::config::{InlinePalette, parse_hex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WebClientColors {
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub cursor_accent: (u8, u8, u8),
    pub normal: [(u8, u8, u8); 8],
    pub bright: [(u8, u8, u8); 8],
    pub selection_background: Option<(u8, u8, u8)>,
    pub selection_foreground: Option<(u8, u8, u8)>,
}

impl WebClientColors {
    /// Build browser-client colors from an Alacritty palette. Missing optional
    /// colors fall back to terminal conventions; malformed provided colors
    /// return `None` so callers can skip theming without losing browser access.
    pub(super) fn from_palette(palette: &InlinePalette) -> Option<Self> {
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
        let cursor_color =
            parse_optional_color(cursor.and_then(|c| c.cursor.as_deref()))?.unwrap_or(foreground);
        let cursor_accent =
            parse_optional_color(cursor.and_then(|c| c.text.as_deref()))?.unwrap_or(background);
        let selection = palette.selection.as_ref();
        Some(Self {
            background,
            foreground,
            cursor: cursor_color,
            cursor_accent,
            normal,
            bright,
            selection_background: parse_optional_color(
                selection.and_then(|s| s.background.as_deref()),
            )?,
            selection_foreground: parse_optional_color(selection.and_then(|s| s.text.as_deref()))?,
        })
    }

    pub(super) fn to_xterm_theme(&self) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InlineAnsiColors, InlinePrimaryColors};

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
}
