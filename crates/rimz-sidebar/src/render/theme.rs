//! Capability-aware styling. Picks the color depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default tier is "stock Unicode + color"; `NO_COLOR` strips color but
//! keeps Unicode and modifiers, so every gauge still reads by shape and fill
//! (the segmented bar's `▰` count carries the meter without the green→red
//! ramp). Powerline / truecolor are reserved as a future "garnish" tier that
//! only swaps chrome characters and color depth — never the grammar itself.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Theme {
    no_color: bool,
}

impl Theme {
    /// Read the environment once. The shell sets `NO_COLOR` when the user
    /// opts out of ANSI color (the [no-color.org](https://no-color.org/)
    /// convention); the renderer honors any non-empty value.
    pub(crate) fn from_env() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        Self { no_color }
    }

    /// Build a constant theme — used by tests to assert the NO_COLOR shape
    /// without poking at the process environment.
    #[cfg(test)]
    pub(crate) const fn fixed(no_color: bool) -> Self {
        Self { no_color }
    }

    /// Style with `fg` color and `modifier`, suppressing the color when
    /// `NO_COLOR` is in effect. Modifiers (BOLD/DIM) survive — they shape the
    /// glyph itself, not its color.
    pub(crate) fn style(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color { style } else { style.fg(fg) }
    }

    /// Shared dim-chrome style — for separators, ages, and labels that sit
    /// alongside the active vocabulary glyphs.
    pub(crate) fn dim(&self) -> Style {
        self.style(Color::DarkGray, Modifier::DIM)
    }
}
