//! Renderer-neutral glyph catalog, configured-set resolution, and setup probes.

use std::borrow::Cow;

use crate::config::{GlyphRole, ThemeConfig, ThemeStyle};

use super::ramp_tone;

#[derive(Clone, Copy, Debug)]
struct GlyphCatalogRow {
    role: GlyphRole,
    unicode: &'static str,
    nerd_font: Option<&'static str>,
}

macro_rules! glyph {
    ($role:ident, $unicode:literal, $nerd_font:expr) => {
        GlyphCatalogRow {
            role: GlyphRole::$role,
            unicode: $unicode,
            nerd_font: $nerd_font,
        }
    };
}

/// One row per [`GlyphRole`], in exact discriminant order. `None` keeps the
/// Unicode glyph in the Nerd Font set: drawn gauges, spines, caps, hairlines,
/// spinner/clock heads, and the compacting wave stay on the terminal grid.
const GLYPH_CATALOG: &[GlyphCatalogRow] = &[
    glyph!(StatusWaiting, "?", Some("\u{f128}")),
    glyph!(StatusAttention, "!", Some("\u{f12a}")),
    glyph!(StatusPaused, "⏸\u{FE0E}", Some("\u{f04c}")),
    glyph!(StatusDone, "✓", Some("\u{f00c}")),
    glyph!(StatusIdle, "○", Some("\u{f2dd}")),
    glyph!(StatusWorking, "⢿", None),
    glyph!(StatusThinking, "⠁", None),
    glyph!(StatusDelegating, "⢄", None),
    glyph!(StatusResolving, "⠙", None),
    glyph!(StatusCompacting, "▇", None),
    glyph!(CockpitWorkspace, "⌘", Some("\u{eda7}")),
    glyph!(CockpitSessions, "◎", Some("\u{ee83}")),
    glyph!(CockpitAgents, "¤", Some("\u{ee9c}")),
    glyph!(CockpitPrOpen, "⑃", Some("\u{efa0}")),
    glyph!(TokensTotal, "◇", Some("\u{ed58}")),
    glyph!(TokensInput, "↘", Some("\u{f103}")),
    glyph!(TokensOutput, "↗", Some("\u{f102}")),
    glyph!(TokensCacheRead, "◌", Some("\u{f1978}")),
    glyph!(TokensCacheWrite, "◍", Some("\u{f1c0}")),
    glyph!(TokensFilled, "▤", Some("\u{f0fe6}")),
    glyph!(TokensCompaction, "↻", Some("\u{f0e2}")),
    glyph!(MeterContextFull, "▣", Some("\u{f0570}")),
    glyph!(MeterContextEmpty, "▢", Some("\u{f11d9}")),
    glyph!(MeterBarFilled, "━", None),
    glyph!(MeterBarTrack, "─", None),
    glyph!(MeterBarCap, "╺", None),
    glyph!(MeterBarHalf, "╸", None),
    glyph!(MeterManaFilled, "▰", None),
    glyph!(MeterManaTrack, "▱", None),
    glyph!(MeterReset, "↻", Some("\u{f0450}")),
    glyph!(MeterUnlimited, "∞", None),
    glyph!(MeterScrollThumb, "▐", None),
    glyph!(MeterScrollTrack, "▕", None),
    glyph!(ClockQ1, "◔", None),
    glyph!(ClockQ2, "◑", None),
    glyph!(ClockQ3, "◕", None),
    glyph!(ClockQ4, "●", None),
    glyph!(ClockOver, "◉", None),
    glyph!(ValueApprox, "≈", None),
    glyph!(WorktreeBranch, "⑂", Some("\u{e0a0}")),
    glyph!(WorktreeMerge, "⮌", Some("\u{f17f}")),
    glyph!(WorktreeAhead, "⇡", None),
    glyph!(WorktreeBehind, "⇣", None),
    glyph!(WorktreeTrunkEqual, "≡", None),
    glyph!(WorktreeTrunkBranch, "⑂", Some("\u{f418}")),
    glyph!(WorktreeTrunkMerge, "✓", Some("\u{f419}")),
    glyph!(WorktreePrOpen, "⑃", Some("\u{f407}")),
    glyph!(WorktreePrClosed, "✕", Some("\u{f4dc}")),
    glyph!(WorktreeCiPassing, "✓", Some("\u{f058}")),
    glyph!(WorktreeCiFailing, "✕", Some("\u{f057}")),
    glyph!(WorktreeCiPending, "◌", Some("\u{f192}")),
    glyph!(WorktreeReconciling, "⟳", Some("\u{f4db}")),
    glyph!(WorktreeExpand, "▸", None),
    glyph!(WorktreeDotted, "┄", None),
    glyph!(ChannelHash, "#", Some("\u{f292}")),
    glyph!(CardSubagents, "⧉", Some("\u{ed50}")),
    glyph!(CardParkedBg, "⋯", None),
    glyph!(ProcessCpu, "C", Some("\u{ef8f}")),
    glyph!(ProcessMem, "M", Some("\u{efc5}")),
    glyph!(ProcessIo, "⇅", Some("\u{f09f}")),
    glyph!(KeysMove, "↕", Some("\u{f07d}")),
    glyph!(KeysFocus, "⏎", Some("\u{f05b}")),
    glyph!(KeysInbox, "␣", Some("\u{f01c}")),
    glyph!(KeysRead, "✉", Some("\u{f0e0}")),
    glyph!(KeysUnread, "●", Some("\u{f111}")),
    glyph!(KeysAll, "≡", Some("\u{f03a}")),
    glyph!(KeysAccounts, "↔", Some("\u{f07e}")),
    glyph!(KeysReload, "⟳", Some("\u{f021}")),
    glyph!(KeysDismiss, "✕", Some("\u{f00d}")),
    glyph!(KeysSidebar, "▐", Some("\u{f0db}")),
    glyph!(ChromeAlert, "⚠", None),
    glyph!(ChromePresenceAway, "zᶻ", Some("\u{f186}")),
    glyph!(ChromeRemoteLink, "⇄", Some("\u{ede3}")),
    glyph!(ChromeRemoteControl, "⇅", None),
    glyph!(ChromeHairline, "─", None),
    glyph!(ChromeBoxTopLeft, "╭", None),
    glyph!(ChromeBoxTopRight, "╮", None),
    glyph!(ChromeBoxBottomLeft, "╰", None),
    glyph!(ChromeBoxBottomRight, "╯", None),
    glyph!(ChromeBoxVertical, "│", None),
    glyph!(ChromeTabCapLeft, "┤", None),
    glyph!(ChromeTabCapRight, "├", None),
    glyph!(ChromeSpineCardLeft, "▌", None),
    glyph!(ChromeSpineCardRight, "▐", None),
    glyph!(ChromeSpineLaneLeft, "▎", None),
    glyph!(ChromeSpineLaneRight, "🮇", None),
    glyph!(ChromeInfinity, "∞", Some("\u{edfe}")),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlyphSetKind {
    Unicode,
    NerdFont,
}

impl GlyphSetKind {
    fn from_source(source: Option<&str>) -> Self {
        match source {
            Some("nerd_font") => Self::NerdFont,
            _ => Self::Unicode,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::NerdFont => "nerd_font",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GlyphSet {
    kind: GlyphSetKind,
    glyphs: Vec<Cow<'static, str>>,
}

impl Default for GlyphSet {
    fn default() -> Self {
        Self::resolve(&ThemeConfig::default())
    }
}

impl GlyphSet {
    /// Resolve style-derived set selection, explicit `glyphs.set` precedence,
    /// preset fallback, and matching inline overrides in one pass.
    pub(crate) fn resolve(theme: &ThemeConfig) -> Self {
        let source = theme.glyphs.set.as_deref().or(match theme.style {
            Some(ThemeStyle::Modern) => Some("nerd_font"),
            _ => None,
        });
        let kind = GlyphSetKind::from_source(source);
        let mut glyphs = GlyphRole::ALL
            .iter()
            .copied()
            .map(|role| match kind {
                GlyphSetKind::Unicode => Cow::Borrowed(unicode_glyph(role)),
                GlyphSetKind::NerdFont => {
                    Cow::Borrowed(nerd_font_glyph(role).unwrap_or_else(|| unicode_glyph(role)))
                }
            })
            .collect::<Vec<_>>();

        for &role in GlyphRole::ALL {
            if let Some(glyph) = theme.glyphs.glyph(kind.name(), role) {
                glyphs[role as usize] = Cow::Owned(glyph.to_owned());
            }
        }

        Self { kind, glyphs }
    }

    pub(crate) fn glyph(&self, role: GlyphRole) -> &str {
        &self.glyphs[role as usize]
    }

    pub(crate) fn kind(&self) -> GlyphSetKind {
        self.kind
    }
}

fn catalog_row(role: GlyphRole) -> &'static GlyphCatalogRow {
    let row = &GLYPH_CATALOG[role as usize];
    debug_assert_eq!(row.role, role);
    row
}

pub(crate) fn unicode_glyph(role: GlyphRole) -> &'static str {
    catalog_row(role).unicode
}

/// The Nerd Font glyph for a role, or `None` when the preset keeps the role's
/// Unicode default. The Nerd Font set is the Unicode base with this curated icon
/// selection layered over it (see `docs/guide/theme.md#glyphs`): the drawn
/// gauges, spines, caps, hairline, the dotted seal, the compacting wave, and the
/// status spinner/clock heads (which animate through frame sequences in
/// `animation.rs`) all return `None` and keep their box-drawing or Unicode shape,
/// while the mana bar swaps to the `nf-extra` progress segments in `meters.rs`.
pub(crate) fn nerd_font_glyph(role: GlyphRole) -> Option<&'static str> {
    catalog_row(role).nerd_font
}

/// Resolve the glyph set under `theme`, so every human surface honors the same
/// `[theme] style` / `[theme.glyphs]` configuration.
pub fn theme_glyphs(theme: &ThemeConfig) -> impl Fn(GlyphRole) -> String {
    let glyphs = GlyphSet::resolve(theme);
    move |role| glyphs.glyph(role).to_owned()
}

pub fn nerd_font_probe_glyphs() -> [&'static str; 8] {
    [
        nerd_font_glyph(GlyphRole::CockpitWorkspace).expect("workspace icon"),
        nerd_font_glyph(GlyphRole::CockpitAgents).expect("agents icon"),
        nerd_font_glyph(GlyphRole::TokensTotal).expect("tokens icon"),
        nerd_font_glyph(GlyphRole::WorktreeBranch).expect("branch icon"),
        nerd_font_glyph(GlyphRole::ChannelHash).expect("channel icon"),
        nerd_font_glyph(GlyphRole::KeysFocus).expect("focus icon"),
        nerd_font_glyph(GlyphRole::KeysUnread).expect("unread icon"),
        nerd_font_glyph(GlyphRole::ChromeInfinity).expect("infinity icon"),
    ]
}

/// The setup probe's color sweep: the sidebar's identity hues resampled to
/// `width` cells with perceptually even OKLab steps between anchors.
pub fn nerd_font_probe_gradient(width: usize) -> Vec<(u8, u8, u8)> {
    const ANCHORS: &[(u8, u8, u8)] = &[
        (125, 207, 255),
        (105, 192, 255),
        (122, 162, 247),
        (146, 138, 255),
        (187, 154, 247),
        (247, 118, 142),
        (255, 158, 100),
        (224, 175, 104),
        (158, 206, 106),
        (115, 218, 202),
        (42, 195, 222),
        (125, 207, 255),
    ];
    (0..width)
        .map(|cell| {
            let amount = if width <= 1 {
                0.0
            } else {
                cell as f32 / (width - 1) as f32
            };
            ramp_tone(ANCHORS, amount)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ThemeGlyphsConfig, validate_glyph_cells, validate_single_cell};

    /// One walk of the catalog pinning every per-row invariant: the table is
    /// indexed by [`GlyphRole`] discriminant, both presets fit their cell
    /// budget, role names round-trip, and a curated Nerd Font icon is a real
    /// override rather than a copy of the Unicode glyph it replaces.
    #[test]
    fn catalog_rows_are_ordered_complete_and_renderable() {
        assert_eq!(
            GLYPH_CATALOG.len(),
            GlyphRole::ALL.len(),
            "catalog contains a row without a GlyphRole"
        );
        for (index, &role) in GlyphRole::ALL.iter().enumerate() {
            let name = role.namespaced_name();
            assert_eq!(
                GLYPH_CATALOG[index].role, role,
                "catalog row for {name} is out of discriminant order"
            );
            assert_eq!(
                GlyphRole::from_namespaced(role.namespace(), role.name()),
                Some(role),
                "{name} maps back to the same role"
            );

            // Most shipped glyphs are one cell. The presence badge's Unicode
            // sleep cue deliberately uses a two-cell "zᶻ" cluster in footer
            // chrome, where the layout measures the whole badge span.
            let unicode = unicode_glyph(role);
            if role == GlyphRole::ChromePresenceAway {
                validate_glyph_cells(unicode).unwrap_or_else(|err| panic!("unicode {name}: {err}"));
            } else {
                validate_single_cell(unicode).unwrap_or_else(|err| panic!("unicode {name}: {err}"));
            }

            if let Some(nerd) = nerd_font_glyph(role) {
                validate_single_cell(nerd).unwrap_or_else(|err| panic!("nerd-font {name}: {err}"));
                assert_ne!(nerd, unicode, "{name} carries a real Nerd Font icon");
            }
        }
    }

    #[test]
    fn nerd_font_falls_back_to_unicode_for_drawn_and_spinner_roles() {
        // The Nerd Font preset is the Unicode base plus a curated icon overlay;
        // every other role returns `None` and keeps its Unicode shape. The drawn
        // gauges/spines, the dotted seal, and the wave must keep falling back so
        // they render on the terminal grid, not as a stray tile. The spinner heads
        // keep their Unicode frames in `animation.rs`, and the per-quarter clock
        // roles defer to the elapsed-time slice series in `labels::glyphs`.
        const FALLBACK_ROLES: &[GlyphRole] = &[
            GlyphRole::StatusWorking,
            GlyphRole::StatusThinking,
            GlyphRole::StatusDelegating,
            GlyphRole::StatusResolving,
            GlyphRole::StatusCompacting,
            GlyphRole::MeterBarFilled,
            GlyphRole::MeterBarTrack,
            GlyphRole::MeterBarCap,
            GlyphRole::MeterBarHalf,
            GlyphRole::MeterManaFilled,
            GlyphRole::MeterManaTrack,
            GlyphRole::MeterScrollThumb,
            GlyphRole::MeterScrollTrack,
            GlyphRole::MeterUnlimited,
            GlyphRole::ClockQ1,
            GlyphRole::ClockQ2,
            GlyphRole::ClockQ3,
            GlyphRole::ClockQ4,
            GlyphRole::ClockOver,
            GlyphRole::WorktreeAhead,
            GlyphRole::WorktreeBehind,
            GlyphRole::WorktreeTrunkEqual,
            GlyphRole::WorktreeExpand,
            GlyphRole::WorktreeDotted,
            GlyphRole::CardParkedBg,
            GlyphRole::ChromeAlert,
            GlyphRole::ChromeRemoteControl,
            GlyphRole::ChromeHairline,
            GlyphRole::ChromeBoxTopLeft,
            GlyphRole::ChromeBoxTopRight,
            GlyphRole::ChromeBoxBottomLeft,
            GlyphRole::ChromeBoxBottomRight,
            GlyphRole::ChromeBoxVertical,
            GlyphRole::ChromeTabCapLeft,
            GlyphRole::ChromeTabCapRight,
            GlyphRole::ChromeSpineCardLeft,
            GlyphRole::ChromeSpineCardRight,
            GlyphRole::ChromeSpineLaneLeft,
            GlyphRole::ChromeSpineLaneRight,
        ];

        for &role in FALLBACK_ROLES {
            assert_eq!(
                nerd_font_glyph(role),
                None,
                "{} keeps its Unicode default in the Nerd Font preset",
                role.namespaced_name()
            );
        }
    }

    #[test]
    fn resolves_named_set_and_explicit_overrides() {
        let config: ThemeGlyphsConfig = toml::from_str(
            "set = \"nerd_font\"\n\
             [nerd_font.status]\n\
             working = \"⢿\"\n\
             [nerd_font.meter]\n\
             bar_half = \"H\"\n\
             [nerd_font.keys]\n\
             focus = \"F\"\n\
             [nerd_font.chrome]\n\
             box_vertical = \"|\"\n",
        )
        .expect("glyph config");
        let glyphs = GlyphSet::resolve(&ThemeConfig {
            glyphs: config,
            ..ThemeConfig::default()
        });
        assert_eq!(glyphs.kind(), GlyphSetKind::NerdFont);
        assert_eq!(glyphs.glyph(GlyphRole::StatusWorking), "⢿");
        assert_eq!(glyphs.glyph(GlyphRole::MeterBarHalf), "H");
        assert_eq!(glyphs.glyph(GlyphRole::KeysFocus), "F");
        assert_eq!(glyphs.glyph(GlyphRole::ChromeBoxVertical), "|");
        assert_eq!(
            glyphs.glyph(GlyphRole::WorktreeBranch),
            nerd_font_glyph(GlyphRole::WorktreeBranch).expect("branch icon")
        );
        assert_eq!(
            glyphs.glyph(GlyphRole::MeterBarFilled),
            unicode_glyph(GlyphRole::MeterBarFilled)
        );
    }

    #[test]
    fn explicit_set_wins_over_style_preset() {
        let modern = GlyphSet::resolve(&ThemeConfig {
            style: Some(ThemeStyle::Modern),
            ..ThemeConfig::default()
        });
        assert_eq!(modern.kind(), GlyphSetKind::NerdFont);

        let explicit = GlyphSet::resolve(&ThemeConfig {
            style: Some(ThemeStyle::Modern),
            glyphs: ThemeGlyphsConfig {
                set: Some("unicode".to_owned()),
                ..ThemeGlyphsConfig::default()
            },
            ..ThemeConfig::default()
        });
        assert_eq!(explicit.kind(), GlyphSetKind::Unicode);
        assert_eq!(
            explicit.glyph(GlyphRole::CockpitWorkspace),
            unicode_glyph(GlyphRole::CockpitWorkspace)
        );
    }

    #[test]
    fn template_glyph_defaults_match_presets() {
        #[derive(serde::Deserialize)]
        struct ThemeFile {
            theme: crate::config::ThemeConfig,
        }

        let parsed: ThemeFile = toml::from_str(crate::config::MachineConfig::template_theme())
            .expect("theme template parses");
        let mut unicode_theme = parsed.theme.clone();
        unicode_theme.glyphs.set = Some("unicode".to_owned());
        let from_template = GlyphSet::resolve(&unicode_theme);
        let default = GlyphSet::default();
        for &role in GlyphRole::ALL {
            assert_eq!(
                from_template.glyph(role),
                default.glyph(role),
                "active template default for {} equals the Unicode preset",
                role.namespaced_name()
            );
        }

        let mut nerd_theme = parsed.theme;
        nerd_theme.glyphs.set = Some("nerd_font".to_owned());
        let from_template = GlyphSet::resolve(&nerd_theme);
        let expected_config = ThemeGlyphsConfig {
            set: Some("nerd_font".to_owned()),
            ..ThemeGlyphsConfig::default()
        };
        let expected = GlyphSet::resolve(&ThemeConfig {
            glyphs: expected_config,
            ..ThemeConfig::default()
        });
        for &role in GlyphRole::ALL {
            assert_eq!(
                from_template.glyph(role),
                expected.glyph(role),
                "active template default for {} equals the Nerd Font preset",
                role.namespaced_name()
            );
        }
    }
}
