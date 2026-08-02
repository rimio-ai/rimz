//! Theme-resolved styles for human CLI output.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use rimz::config::{MachineConfig, ThemeConfig, ThemeProviderStyle};
use rimz::theme::{Palette, Tone, resolve_provider_brand, resolve_provider_identity};

struct CliTheme {
    palette: Palette,
    providers: BTreeMap<String, ThemeProviderStyle>,
}

impl CliTheme {
    fn resolve(theme: &ThemeConfig, truecolor: bool) -> Self {
        let depth = theme.effective_theme_mode().depth(truecolor);
        Self {
            palette: Palette::resolve(theme, depth),
            providers: theme.providers.clone(),
        }
    }

    fn load() -> Self {
        let config = MachineConfig::load_lenient();
        Self::resolve(&config.theme, rimz::tui::truecolor())
    }

    fn style(&self, tone: Tone) -> anstyle::Style {
        anstyle::Style::new().fg_color(Some(tone_color(tone)))
    }

    fn identity(&self, kind: &str) -> anstyle::Style {
        self.style(resolve_provider_brand(kind, &self.providers).tone(&self.palette))
    }
}

static THEME: LazyLock<CliTheme> = LazyLock::new(CliTheme::load);

fn tone_color(tone: Tone) -> anstyle::Color {
    match tone {
        Tone::Indexed(index) => anstyle::Color::Ansi256(anstyle::Ansi256Color(index)),
        Tone::Rgb(red, green, blue) => anstyle::Color::Rgb(anstyle::RgbColor(red, green, blue)),
    }
}

pub(crate) fn rgb_color(rgb: (u8, u8, u8)) -> anstyle::Color {
    tone_color(THEME.palette.rgb_tone(rgb))
}

pub(crate) fn rgb_bg(rgb: (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().bg_color(Some(rgb_color(rgb)))
}

pub(crate) fn good() -> anstyle::Style {
    THEME.style(THEME.palette.good())
}

pub(crate) fn warn() -> anstyle::Style {
    THEME.style(THEME.palette.warn())
}

pub(crate) fn alarm() -> anstyle::Style {
    THEME.style(THEME.palette.alarm())
}

pub(crate) fn accent() -> anstyle::Style {
    THEME.style(THEME.palette.accent())
}

pub(crate) fn cool() -> anstyle::Style {
    THEME.style(THEME.palette.cool())
}

pub(crate) fn meta() -> anstyle::Style {
    THEME.style(THEME.palette.meta())
}

pub(crate) fn body() -> anstyle::Style {
    THEME.style(THEME.palette.body())
}

pub(crate) fn muted() -> anstyle::Style {
    THEME.style(THEME.palette.muted())
}

pub(crate) fn faint() -> anstyle::Style {
    THEME.style(THEME.palette.faint())
}

pub(crate) fn rule() -> anstyle::Style {
    THEME.style(THEME.palette.rule())
}

pub(crate) fn header() -> anstyle::Style {
    muted().bold()
}

pub(crate) fn money() -> anstyle::Style {
    THEME.style(THEME.palette.identity(rimz::theme::Identity::Money))
}

pub(crate) fn human_chip() -> anstyle::Style {
    anstyle::Style::new()
        .bg_color(Some(tone_color(THEME.palette.cool())))
        .fg_color(Some(tone_color(THEME.palette.selection_bg())))
}

pub(crate) fn system_chip() -> anstyle::Style {
    anstyle::Style::new()
        .bg_color(Some(tone_color(THEME.palette.meta())))
        .fg_color(Some(tone_color(THEME.palette.selection_bg())))
}

pub(crate) fn identity(kind: &str) -> anstyle::Style {
    THEME.identity(kind)
}

pub(crate) fn identity_name(kind: &str) -> String {
    resolve_provider_identity(kind, &THEME.providers).product_name
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::config::{ColorDepth, ThemeColor, ThemeMode};

    #[test]
    fn scheme_and_slot_overrides_change_cli_tones() {
        let afterglow = CliTheme::resolve(
            &ThemeConfig {
                scheme: Some("Afterglow".to_owned()),
                ..Default::default()
            },
            true,
        );
        let default = CliTheme::resolve(&ThemeConfig::default(), true);
        assert_ne!(afterglow.palette.good(), default.palette.good());

        let overridden = CliTheme::resolve(
            &ThemeConfig {
                good: Some(ThemeColor::Rgb(1, 2, 3)),
                ..Default::default()
            },
            true,
        );
        assert_eq!(
            overridden.style(overridden.palette.good()).get_fg_color(),
            Some(anstyle::Color::Rgb(anstyle::RgbColor(1, 2, 3)))
        );
    }

    #[test]
    fn indexed_mode_quantizes_cli_tones() {
        let indexed = CliTheme::resolve(
            &ThemeConfig {
                mode: ThemeMode::Indexed,
                ..Default::default()
            },
            true,
        );
        assert_eq!(
            indexed.palette.good(),
            Tone::from_rgb((0x9e, 0xce, 0x6a), ColorDepth::Indexed)
        );
    }

    #[test]
    fn provider_override_drives_cli_identity() {
        let mut theme = ThemeConfig::default();
        theme.providers.insert(
            "claude".to_owned(),
            ThemeProviderStyle {
                product_name: Some("Anthropic".to_owned()),
                color: Some(ThemeColor::Rgb(1, 2, 3)),
                ..Default::default()
            },
        );
        let cli = CliTheme::resolve(&theme, true);
        assert_eq!(
            cli.identity("claude").get_fg_color(),
            Some(anstyle::Color::Rgb(anstyle::RgbColor(1, 2, 3)))
        );
        assert_eq!(
            resolve_provider_identity("claude", &cli.providers).product_name,
            "Anthropic"
        );
    }
}
