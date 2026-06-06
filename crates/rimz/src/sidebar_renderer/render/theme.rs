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
//! the `[sidebar] glow` mode rides the snapshot like the palette overrides —
//! `never` pins the plain 256-color render on any terminal, and `always`
//! forces the pass where a truecolor terminal's advertisement went missing
//! (an SSH hop forwards `TERM` but drops `COLORTERM`).
//!
//! The palette is data, not constants: every semantic slot carries a built-in
//! tone ([`Palette::BUILTIN`]) and an optional per-machine override from
//! `[sidebar.theme]` ([`SidebarThemeConfig`]), resolved producer-side onto the
//! snapshot exactly like the `[sidebar.providers]` brand styling — so every
//! renderer of the same workspace paints the same tones with zero config
//! knowledge of its own.

use std::sync::OnceLock;

use crate::config::{GlowMode, SidebarConfig, SidebarThemeConfig};
use ratatui::style::{Color, Modifier, Style};

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
    /// mid gray — the soft content tier: capability tokens, card figures,
    /// subagent lines. A step above `dim`, below the default-fg `value`.
    soft: Color,
    /// deep gray — labels, ages, seams (`Color::DarkGray`/`Color::Gray`).
    dim: Color,
    /// deeper gray — recedes below `dim`: bar tracks, `·` separators, dividers.
    faint: Color,
    /// the darkest chrome — `faint`'s gray dropped a further step by the `DIM`
    /// attenuation: the scrollbar's resting track.
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
        soft: Color::Indexed(246),
        dim: Color::Indexed(242),
        faint: Color::Indexed(238),
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
            soft: slot(theme.soft, Self::BUILTIN.soft),
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
    /// The `[sidebar] glow` mode, riding the snapshot. `auto` follows the
    /// terminal capability; `always` forces the pass past a missing
    /// `COLORTERM`; `never` pins the plain 256-color render.
    glow: GlowMode,
    palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            no_color: false,
            truecolor: false,
            glow: GlowMode::Auto,
            palette: Palette::BUILTIN,
        }
    }
}

impl Theme {
    /// The active theme for a frame: the cached `NO_COLOR` and `COLORTERM`
    /// readings plus the palette and glow mode resolved from the snapshot's
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
            glow: GlowMode::Auto,
            palette: Palette::BUILTIN,
        }
    }

    /// Whether the post-render effects pass runs. `NO_COLOR` beats everything
    /// — the pass is color-only, so it has nothing to say on a colorless
    /// frame. Under `auto` the terminal must advertise 24-bit color (smooth
    /// lightness interpolation quantizes into visible banding on a 256-color
    /// palette); `always` trusts the user's word over a missing advertisement;
    /// `never` pins the plain render. With the pass off the modifier-based
    /// attention breath alone carries the cue, exactly as before the glow
    /// tier existed.
    pub(crate) fn effects_enabled(&self) -> bool {
        !self.no_color
            && match self.glow {
                GlowMode::Never => false,
                GlowMode::Always => true,
                GlowMode::Auto => self.truecolor,
            }
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

    /// A plain palette gray at normal weight — the shared shape of the lit
    /// gray ladder. The ladder steps by color index alone; a `DIM` modifier
    /// would hand each tone back to the terminal's attenuation and collapse
    /// the steps. Under `NO_COLOR` every rung falls to the bare `DIM`
    /// modifier, so "a step below `value`" still carries as weight.
    fn gray(&self, color: Color) -> Style {
        if self.no_color {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(color)
        }
    }

    /// Shared dim-chrome style — for ages, labels, and seams that sit
    /// alongside the active vocabulary glyphs. A step below
    /// [`soft`](Self::soft) on the gray ladder.
    pub(crate) fn dim(&self) -> Style {
        self.gray(self.palette.dim)
    }

    /// The soft middle tier — between the default-fg full-strength text and
    /// the gray [`dim`](Self::dim) chrome, for content a reader actually
    /// reads: capability tokens, stat figures, subagent lines, the process
    /// rows' program names.
    pub(crate) fn soft(&self) -> Style {
        self.gray(self.palette.soft)
    }

    /// The faintest chrome — a step below [`dim`](Self::dim) for the pure
    /// scaffolding that should recede furthest: bar tracks, `·` separators, and
    /// dividers. Under `NO_COLOR` it collapses to the same dim modifier as the
    /// rest of the ladder; the shape (a light `─` track, a thin `·`) carries
    /// the reading without the tone.
    pub(crate) fn faint(&self) -> Style {
        self.gray(self.palette.faint)
    }

    /// The darkest chrome — [`faint`](Self::faint)'s gray under the `DIM`
    /// attenuation, a step below it: the scrollbar's resting track (`▕`),
    /// receding beside its `dim` thumb so the position reads without the rail
    /// shouting.
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

    /// The gray ladder steps by color index alone when lit — soft, dim, and
    /// faint paint plain palette grays at *normal* weight (a `DIM` modifier
    /// would hand each tone back to the terminal's attenuation and collapse
    /// the steps); only the darkest rung `rule` keeps `DIM`, dropping the
    /// faint gray one further step. Under `NO_COLOR` every rung collapses to
    /// the bare `DIM` modifier, so the de-emphasis still carries as weight.
    /// Each slot re-tunes from its `[sidebar.theme]` key.
    #[test]
    fn gray_ladder_is_plain_when_lit_and_a_dim_weight_under_no_color() {
        let lit = Theme::fixed(false);
        for (style, index) in [(lit.soft(), 246), (lit.dim(), 242), (lit.faint(), 238)] {
            assert_eq!(style.fg, Some(Color::Indexed(index)));
            assert!(style.add_modifier.is_empty(), "no DIM attenuation when lit");
        }
        assert_eq!(lit.rule().fg, Some(Color::Indexed(238)));
        assert!(
            lit.rule().add_modifier.contains(Modifier::DIM),
            "rule rides faint's gray under the DIM attenuation"
        );

        let dark = Theme::fixed(true);
        for style in [dark.soft(), dark.dim(), dark.faint(), dark.rule()] {
            assert_eq!(style.fg, None);
            assert!(style.add_modifier.contains(Modifier::DIM));
        }

        let themed = Theme {
            palette: Palette::resolve(&SidebarThemeConfig {
                soft: Some(252),
                ..SidebarThemeConfig::default()
            }),
            ..Theme::default()
        };
        assert_eq!(themed.soft().fg, Some(Color::Indexed(252)));
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

    /// The effects gate, mode by mode: `auto` follows the terminal's
    /// truecolor advertisement, `always` overrides a missing one, `never`
    /// pins the pass off — and `NO_COLOR` beats every mode, since the pass
    /// is color-only.
    #[test]
    fn effects_follow_the_glow_mode_and_no_color_beats_it() {
        let theme = |no_color, truecolor, glow| Theme {
            no_color,
            truecolor,
            glow,
            palette: Palette::BUILTIN,
        };
        assert!(theme(false, true, GlowMode::Auto).effects_enabled());
        assert!(
            !theme(false, false, GlowMode::Auto).effects_enabled(),
            "auto on a terminal that advertises no truecolor stays plain"
        );
        assert!(
            theme(false, false, GlowMode::Always).effects_enabled(),
            "always forces the pass past a missing COLORTERM (the SSH hop)"
        );
        assert!(
            !theme(false, true, GlowMode::Never).effects_enabled(),
            "never pins the plain render on a truecolor terminal"
        );
        assert!(
            !theme(true, true, GlowMode::Always).effects_enabled(),
            "NO_COLOR beats every mode, the forced one included"
        );
    }

    /// The mode travels: `[sidebar] glow` resolves producer-side onto the
    /// snapshot and lands in the theme, so every renderer of the workspace
    /// switches tiers together.
    #[test]
    fn the_glow_mode_rides_the_snapshot_into_the_theme() {
        assert_eq!(
            Theme::for_sidebar(&SidebarConfig::default()).glow,
            GlowMode::Auto
        );
        let pinned_off = SidebarConfig {
            glow: GlowMode::Never,
            ..SidebarConfig::default()
        };
        let theme = Theme::for_sidebar(&pinned_off);
        assert_eq!(theme.glow, GlowMode::Never);
        assert!(!theme.effects_enabled());
    }
}
