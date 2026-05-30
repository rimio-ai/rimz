//! Capability-aware styling. Picks the color depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default tier is "stock Unicode + a tuned 256-color palette"; `NO_COLOR`
//! strips color but keeps Unicode and modifiers, so every gauge still reads by
//! shape and fill (the bar's `█`/`░` count carries the meter without the
//! green→red ramp). Powerline / truecolor are reserved as a future "garnish"
//! tier that only swaps chrome characters and color depth — never the grammar.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// Muted 256-color palette. The renderer's callers speak in semantic ANSI
/// names (`Color::Green` for "good", `Color::Red` for "alarm"); [`Theme::style`]
/// resolves each to one of these indexed tones, so the whole palette lives in
/// one place and stays easy on the eyes on a dark terminal.
const GREEN: Color = Color::Indexed(108); // sage — running tally / low gauge / additions
const AMBER: Color = Color::Indexed(179); // gold — waiting / mid gauge
const RED: Color = Color::Indexed(167); // balanced red — failed / high gauge
const CYAN: Color = Color::Indexed(73); // teal — worktree headers / cache writes
const BLUE: Color = Color::Indexed(75); // sky — cache reads in the context bar
const VIOLET: Color = Color::Indexed(141); // soft purple — the weekly "mana" bar
const DIM: Color = Color::Indexed(244); // mid gray — separators, ages, labels

/// Claude clay — the running agent's animated working/thinking head, so the
/// live cell reads in the agent's own brand orange. Closest muted 256-color
/// tone to Claude's `#D97757`.
pub(super) const ORANGE: Color = Color::Indexed(173);

/// Accent for the selected-row left bar. Brighter than the chrome so the `▎`
/// reads as "here you are" without inverting the whole row.
const ACCENT: Color = Color::Indexed(110); // soft blue

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Theme {
    no_color: bool,
}

impl Theme {
    /// Read the environment once. The shell sets `NO_COLOR` when the user
    /// opts out of ANSI color (the [no-color.org](https://no-color.org/)
    /// convention); the renderer honors any non-empty value.
    ///
    /// `NO_COLOR` cannot change mid-process, so the result is cached — the
    /// render path asks for it on every frame (≈8×/s while a spinner animates),
    /// and an env lookup per frame is pure waste.
    pub(crate) fn from_env() -> Self {
        static CACHED: OnceLock<Theme> = OnceLock::new();
        *CACHED.get_or_init(|| {
            let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
            Self { no_color }
        })
    }

    /// Build a constant theme — used by tests to assert the NO_COLOR shape
    /// without poking at the process environment.
    #[cfg(test)]
    pub(crate) const fn fixed(no_color: bool) -> Self {
        Self { no_color }
    }

    /// Style with `fg` color and `modifier`, suppressing the color when
    /// `NO_COLOR` is in effect. Modifiers (BOLD/DIM) survive — they shape the
    /// glyph itself, not its color. The semantic `fg` is mapped through the
    /// 256-color palette so the whole renderer paints one tuned set of tones.
    pub(crate) fn style(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(resolve(fg))
        }
    }

    /// Shared dim-chrome style — for separators, ages, and labels that sit
    /// alongside the active vocabulary glyphs.
    pub(crate) fn dim(&self) -> Style {
        self.style(Color::DarkGray, Modifier::DIM)
    }

    /// Accent style for the selected-row left bar (`▎`). Under `NO_COLOR` the
    /// bar glyph alone marks selection, so no style is needed.
    pub(crate) fn selection(&self) -> Style {
        self.style(ACCENT, Modifier::BOLD)
    }
}

/// Map a semantic ANSI color to its tuned 256-color tone. Anything outside the
/// renderer's vocabulary passes through unchanged.
fn resolve(color: Color) -> Color {
    match color {
        Color::Green => GREEN,
        Color::Yellow => AMBER,
        Color::Red => RED,
        Color::Cyan => CYAN,
        Color::Blue => BLUE,
        Color::Magenta => VIOLET,
        Color::DarkGray | Color::Gray => DIM,
        other => other,
    }
}
