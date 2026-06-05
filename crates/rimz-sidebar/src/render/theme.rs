//! Capability-aware styling. Picks the color depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default tier is "stock Unicode + a tuned 256-color palette"; `NO_COLOR`
//! strips color but keeps Unicode and modifiers, so every gauge still reads by
//! shape and fill (the bar's `█`/`░` count carries the meter without the
//! green→red ramp). Truecolor is the "garnish" tier: when `COLORTERM`
//! advertises 24-bit color (and `NO_COLOR` is off) the post-render effects
//! pass ([`super::effects`]) layers smooth color motion over the composed
//! frame — color depth only, never the grammar. The tier is also a choice:
//! `[sidebar] glow = false` pins the plain 256-color render on any terminal,
//! riding the snapshot like the palette overrides.
//!
//! The palette is data, not constants: every semantic slot carries a built-in
//! tone ([`Palette::BUILTIN`]) and an optional per-machine override from
//! `[sidebar.theme]` ([`SidebarThemeConfig`]), resolved producer-side onto the
//! snapshot exactly like the `[sidebar.providers]` brand styling — so every
//! renderer of the same workspace paints the same tones with zero config
//! knowledge of its own.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use rimz::config::{SidebarConfig, SidebarThemeConfig};

/// Claude clay — the running agent's animated working/thinking head, so the
/// live cell reads in the agent's own brand orange. Closest muted 256-color
/// tone to Claude's `#D97757`. Deliberately not a palette slot: it is a brand
/// tone (like the per-provider dashboard colors), not chrome, and a generic
/// `Indexed(173)` remap would wrongly absorb unrelated 173s.
pub(super) const ORANGE: Color = Color::Indexed(173);

/// The muted 256-color palette, one named slot per semantic tone. The
/// renderer's callers speak in semantic ANSI names (`Color::Green` for "good",
/// `Color::Red` for "alarm"); [`Theme::style`] resolves each through the active
/// palette, so the whole tone set lives in one place, stays easy on the eyes on
/// a dark terminal, and re-tunes from `[sidebar.theme]` without touching a
/// callsite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    /// sage — running tally / low gauge / additions / cache reads (`Color::Green`).
    good: Color,
    /// gold — waiting / mid gauge / cache writes (`Color::Yellow`).
    warn: Color,
    /// balanced red — failed / high gauge / fresh input (`Color::Red`).
    alarm: Color,
    /// teal — worktree headers and the lane spine (`Color::Cyan`).
    accent: Color,
    /// sky — the cautious `plan` posture pill (`Color::Blue`).
    cool: Color,
    /// soft purple — the provider `⇅ rc` flag and delegation family (`Color::Magenta`).
    meta: Color,
    /// mid gray — labels, ages, values (`Color::DarkGray`/`Color::Gray`).
    dim: Color,
    /// deep gray — recedes below `dim`: bar tracks, `·` separators, dividers.
    faint: Color,
    /// darkest chrome — the borderless section hairlines, quieter than a dotted divider.
    rule: Color,
    /// soft blue — the selected-row left bar, brighter than the chrome so the
    /// `▎` reads as "here you are" without inverting the whole row.
    selection: Color,
}

impl Palette {
    /// The shipped tones. Every `[sidebar.theme]` slot a user leaves unset
    /// falls back here, so an absent section renders the built-ins.
    pub(crate) const BUILTIN: Palette = Palette {
        good: Color::Indexed(108),
        warn: Color::Indexed(179),
        alarm: Color::Indexed(167),
        accent: Color::Indexed(73),
        cool: Color::Indexed(75),
        meta: Color::Indexed(141),
        dim: Color::Indexed(246),
        faint: Color::Indexed(242),
        rule: Color::Indexed(238),
        selection: Color::Indexed(110),
    };

    /// Resolve the active palette: each configured `[sidebar.theme]` slot (a
    /// 256-color index) overrides its built-in; an unset slot keeps it.
    pub(crate) fn resolve(theme: &SidebarThemeConfig) -> Palette {
        let slot = |over: Option<u8>, builtin: Color| over.map(Color::Indexed).unwrap_or(builtin);
        Palette {
            good: slot(theme.good, Self::BUILTIN.good),
            warn: slot(theme.warn, Self::BUILTIN.warn),
            alarm: slot(theme.alarm, Self::BUILTIN.alarm),
            accent: slot(theme.accent, Self::BUILTIN.accent),
            cool: slot(theme.cool, Self::BUILTIN.cool),
            meta: slot(theme.meta, Self::BUILTIN.meta),
            dim: slot(theme.dim, Self::BUILTIN.dim),
            faint: slot(theme.faint, Self::BUILTIN.faint),
            rule: slot(theme.rule, Self::BUILTIN.rule),
            selection: slot(theme.selection, Self::BUILTIN.selection),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Theme {
    no_color: bool,
    /// The terminal advertises 24-bit color (`COLORTERM`). Gates the
    /// post-render effects pass; the composed grammar never reads it.
    truecolor: bool,
    /// The `[sidebar] glow` flag, riding the snapshot. On by default; an
    /// opt-out pins the plain 256-color render whatever the terminal speaks.
    glow: bool,
    palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            no_color: false,
            truecolor: false,
            glow: true,
            palette: Palette::BUILTIN,
        }
    }
}

impl Theme {
    /// The active theme for a frame: the cached `NO_COLOR` and `COLORTERM`
    /// readings plus the palette and glow flag resolved from the snapshot's
    /// `[sidebar]` config. Called per compose — the resolve is ten copies of a
    /// `Color`, far below the frame budget — so a config change lands with the
    /// next produced snapshot, no renderer restart.
    pub(crate) fn for_sidebar(sidebar: &SidebarConfig) -> Self {
        Self {
            no_color: no_color_env(),
            truecolor: truecolor_env(),
            glow: sidebar.glow,
            palette: Palette::resolve(&sidebar.theme),
        }
    }

    /// Build a constant theme — used by tests to assert the NO_COLOR shape
    /// without poking at the process environment. Always the built-in palette;
    /// override tests go through [`Theme::for_sidebar`].
    #[cfg(test)]
    pub(crate) const fn fixed(no_color: bool) -> Self {
        Self {
            no_color,
            truecolor: false,
            glow: true,
            palette: Palette::BUILTIN,
        }
    }

    /// Whether the post-render effects pass runs: the `[sidebar] glow` flag
    /// must be on (its default), the terminal must speak 24-bit color (smooth
    /// lightness interpolation quantizes into visible banding on a 256-color
    /// palette), and the user must not have opted out of color entirely. With
    /// the pass off the modifier-based attention breath alone carries the cue,
    /// exactly as before the glow tier existed.
    pub(crate) fn effects_enabled(&self) -> bool {
        self.glow && self.truecolor && !self.no_color
    }

    /// Style with `fg` color and `modifier`, suppressing the color when
    /// `NO_COLOR` is in effect. Modifiers (BOLD/DIM) survive — they shape the
    /// glyph itself, not its color. The semantic `fg` is mapped through the
    /// active palette so the whole renderer paints one tuned set of tones.
    pub(crate) fn style(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(self.resolve(fg))
        }
    }

    /// Style a chip — `fg` ink on a `bg` fill plus `modifier` — the provider
    /// tab rail's active pick. Both colors are suppressed under `NO_COLOR`
    /// (the `┤ ├` caps then carry the pick by shape); otherwise each maps
    /// through the palette, so an explicit `Indexed` brand fill and the dark
    /// ink pass through unchanged like every other indexed tone. Modifiers
    /// always survive — they shape the glyph, not its color.
    pub(crate) fn chip(&self, fg: Color, bg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(self.resolve(fg)).bg(self.resolve(bg))
        }
    }

    /// Shared dim-chrome style — for ages, labels, and values that sit
    /// alongside the active vocabulary glyphs.
    pub(crate) fn dim(&self) -> Style {
        self.style(Color::DarkGray, Modifier::DIM)
    }

    /// Full-strength value text — the terminal's default foreground, no
    /// modifier. The colored marker beside it carries the semantics; the
    /// figure reads at normal weight (the fleet token lines, the cockpit
    /// counts, the W/M ledger figures).
    pub(crate) fn value(&self) -> Style {
        Style::default()
    }

    /// The faintest chrome — a step below [`dim`](Self::dim) for the pure
    /// scaffolding that should recede furthest: bar tracks, `·` separators, and
    /// dividers. Under `NO_COLOR` it collapses to the same dim modifier as
    /// [`dim`](Self::dim); the shape (a light `─` track, a thin `·`) carries
    /// the reading without the tone.
    pub(crate) fn faint(&self) -> Style {
        self.style(self.palette.faint, Modifier::DIM)
    }

    /// The faintest *solid* chrome — the borderless section hairline rules (`─`).
    /// A step below [`faint`](Self::faint) so a full-width solid rule recedes to
    /// about the apparent weight of the dotted `┄ external` divider instead of
    /// reading as a bright bar. Under `NO_COLOR` it collapses to the dim
    /// modifier like the rest.
    pub(crate) fn rule(&self) -> Style {
        self.style(self.palette.rule, Modifier::DIM)
    }

    /// Accent style for the selected-row left bar (`▎`). Under `NO_COLOR` the
    /// bar glyph alone marks selection, so no style is needed.
    pub(crate) fn selection(&self) -> Style {
        self.style(self.palette.selection, Modifier::BOLD)
    }

    /// The resolved palette tone for a semantic color, as a bare `Color` — the
    /// effects pass feeds these to its shaders, which interpolate the color
    /// itself rather than build a `Style`. Only meaningful when
    /// [`effects_enabled`](Self::effects_enabled) holds, which already implies
    /// `NO_COLOR` is off.
    pub(super) fn tone(&self, color: Color) -> Color {
        self.resolve(color)
    }

    /// Map a semantic ANSI color to its tuned palette tone. Anything outside
    /// the renderer's vocabulary — an explicit `Indexed` brand color, the
    /// palette tones the dedicated methods pass back through — goes out
    /// unchanged.
    fn resolve(&self, color: Color) -> Color {
        match color {
            Color::Green => self.palette.good,
            Color::Yellow => self.palette.warn,
            Color::Red => self.palette.alarm,
            Color::Cyan => self.palette.accent,
            Color::Blue => self.palette.cool,
            Color::Magenta => self.palette.meta,
            Color::DarkGray | Color::Gray => self.palette.dim,
            other => other,
        }
    }
}

/// Read the environment once. The shell sets `NO_COLOR` when the user opts out
/// of ANSI color (the [no-color.org](https://no-color.org/) convention); the
/// renderer honors any non-empty value.
///
/// `NO_COLOR` cannot change mid-process, so the result is cached — the render
/// path asks for it on every frame (≈8×/s while a spinner animates), and an
/// env lookup per frame is pure waste.
fn no_color_env() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

/// Read the terminal's 24-bit color capability once. `COLORTERM=truecolor` (or
/// `24bit`) is the de-facto convention emulators use to advertise it; like
/// `NO_COLOR` it cannot change mid-process, so the reading is cached for the
/// same per-frame reason.
fn truecolor_env() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("COLORTERM").is_ok_and(|v| matches!(v.as_str(), "truecolor" | "24bit"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden-safety contract behind every existing snapshot: an absent
    /// `[sidebar.theme]` resolves to exactly the built-in palette, so the
    /// default render is byte-identical to the pre-config era.
    #[test]
    fn unset_theme_resolves_to_the_builtin_palette() {
        assert_eq!(
            Palette::resolve(&SidebarThemeConfig::default()),
            Palette::BUILTIN
        );
    }

    #[test]
    fn configured_slot_overrides_only_its_semantic_color() {
        let theme = Theme {
            palette: Palette::resolve(&SidebarThemeConfig {
                good: Some(34),
                ..SidebarThemeConfig::default()
            }),
            ..Theme::default()
        };
        // The overridden slot re-tones its semantic ANSI name…
        assert_eq!(
            theme.style(Color::Green, Modifier::empty()).fg,
            Some(Color::Indexed(34))
        );
        // …while an untouched slot keeps the built-in tone, and an explicit
        // indexed brand color passes through unmapped.
        assert_eq!(
            theme.style(Color::Red, Modifier::empty()).fg,
            Some(Color::Indexed(167))
        );
        assert_eq!(
            theme.style(ORANGE, Modifier::empty()).fg,
            Some(Color::Indexed(173))
        );
    }

    #[test]
    fn chip_suppresses_both_fg_and_bg_under_no_color() {
        let lit = Theme::fixed(false).chip(Color::Indexed(16), Color::Indexed(173), Modifier::BOLD);
        assert_eq!(lit.fg, Some(Color::Indexed(16)));
        assert_eq!(
            lit.bg,
            Some(Color::Indexed(173)),
            "brand fill passes through unmapped"
        );
        assert!(lit.add_modifier.contains(Modifier::BOLD));

        let dark = Theme::fixed(true).chip(Color::Indexed(16), Color::Indexed(173), Modifier::BOLD);
        assert_eq!(dark.fg, None);
        assert_eq!(dark.bg, None, "NO_COLOR suppresses the chip fill too");
        assert!(dark.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn no_color_strips_the_palette_but_keeps_modifiers() {
        let theme = Theme {
            no_color: true,
            palette: Palette::resolve(&SidebarThemeConfig {
                alarm: Some(196),
                ..SidebarThemeConfig::default()
            }),
            ..Theme::default()
        };
        let style = theme.style(Color::Red, Modifier::BOLD);
        assert_eq!(style.fg, None, "NO_COLOR suppresses even a themed tone");
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    /// The effects gate: the pass needs the terminal capability (truecolor),
    /// color left on (`NO_COLOR` beats everything), and the `[sidebar] glow`
    /// flag — on by default, so the opt-out is the only lever a user has to
    /// pull.
    #[test]
    fn effects_run_only_under_truecolor_color_and_the_glow_flag() {
        let theme = |no_color, truecolor, glow| Theme {
            no_color,
            truecolor,
            glow,
            palette: Palette::BUILTIN,
        };
        assert!(theme(false, true, true).effects_enabled());
        assert!(
            !theme(false, false, true).effects_enabled(),
            "a 256-color terminal never runs the pass"
        );
        assert!(
            !theme(true, true, true).effects_enabled(),
            "NO_COLOR beats both the capability and the flag"
        );
        assert!(
            !theme(false, true, false).effects_enabled(),
            "the glow opt-out pins the plain render on a truecolor terminal"
        );
    }

    /// The opt-out travels: `[sidebar] glow = false` resolves producer-side
    /// onto the snapshot and lands in the theme, so every renderer of the
    /// workspace falls back to the plain 256-color render together.
    #[test]
    fn the_glow_opt_out_rides_the_snapshot_into_the_theme() {
        assert!(Theme::for_sidebar(&SidebarConfig::default()).glow);
        let opted_out = SidebarConfig {
            glow: false,
            ..SidebarConfig::default()
        };
        let theme = Theme::for_sidebar(&opted_out);
        assert!(!theme.glow);
        assert!(!theme.effects_enabled());
    }
}
