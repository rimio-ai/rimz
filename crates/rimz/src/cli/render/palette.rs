//! The CLI's semantic color palette: the [`PaletteTones`] slots emitted as
//! `anstyle` styles. Built from [`PaletteTones::DEFAULT`] — the same fixed
//! `TokyoNight Night` tones the default sidebar uses — so `rimz` command output
//! and the room share one look without dragging per-user theme resolution onto
//! the CLI path.

use rimz::config::PaletteTones;

const TONES: PaletteTones = PaletteTones::DEFAULT;

const fn fg(rgb: (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().fg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(
        rgb.0, rgb.1, rgb.2,
    ))))
}

pub(crate) const GOOD: anstyle::Style = fg(TONES.good);
pub(crate) const WARN: anstyle::Style = fg(TONES.warn);
pub(crate) const ALARM: anstyle::Style = fg(TONES.alarm);
pub(crate) const ACCENT: anstyle::Style = fg(TONES.accent);
pub(crate) const COOL: anstyle::Style = fg(TONES.cool);
pub(crate) const META: anstyle::Style = fg(TONES.meta);
pub(crate) const BODY: anstyle::Style = fg(TONES.body);
pub(crate) const MUTED: anstyle::Style = fg(TONES.muted);
pub(crate) const FAINT: anstyle::Style = fg(TONES.faint);

/// Table and key/value headers: the `muted` tone, bolded — present but recessed.
pub(crate) const HEADER: anstyle::Style = fg(TONES.muted).bold();
