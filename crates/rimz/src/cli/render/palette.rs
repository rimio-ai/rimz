//! The CLI's semantic color palette: the [`Semantic`] slots emitted as
//! `anstyle` styles. Built from [`Semantic::DEFAULT`] — the same fixed
//! `TokyoNight Night` tones the default sidebar uses — so `rimz` command output
//! and the room share one look without dragging per-user theme resolution onto
//! the CLI path.

use rimz::config::Semantic;

const TONES: Semantic = Semantic::DEFAULT;

pub(crate) const fn rgb(rgb: (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().fg_color(Some(rgb_color(rgb)))
}

pub(crate) const fn rgb_color(rgb: (u8, u8, u8)) -> anstyle::Color {
    anstyle::Color::Rgb(anstyle::RgbColor(rgb.0, rgb.1, rgb.2))
}

pub(crate) const GOOD: anstyle::Style = rgb(TONES.good);
pub(crate) const WARN: anstyle::Style = rgb(TONES.warn);
pub(crate) const ALARM: anstyle::Style = rgb(TONES.alarm);
pub(crate) const ACCENT: anstyle::Style = rgb(TONES.accent);
pub(crate) const COOL: anstyle::Style = rgb(TONES.cool);
pub(crate) const META: anstyle::Style = rgb(TONES.meta);
pub(crate) const BODY: anstyle::Style = rgb(TONES.body);
pub(crate) const MUTED: anstyle::Style = rgb(TONES.muted);
pub(crate) const FAINT: anstyle::Style = rgb(TONES.faint);

/// Table and key/value headers: the `muted` tone, bolded — present but recessed.
pub(crate) const HEADER: anstyle::Style = rgb(TONES.muted).bold();
