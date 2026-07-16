use std::collections::BTreeMap;

use crate::config::{GlyphRole, ThemeConfig, ThemeGlyphsConfig};

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
    glyphs: BTreeMap<GlyphRole, String>,
}

impl Default for GlyphSet {
    fn default() -> Self {
        let config = ThemeGlyphsConfig::default();
        Self::resolve(config.set.as_deref(), &config)
    }
}

impl GlyphSet {
    pub(crate) fn from_theme(theme: &ThemeConfig) -> Self {
        Self::resolve(theme.glyph_set_source().as_deref(), &theme.glyphs)
    }

    /// Resolve with an explicit set source, used by the `[theme] style` preset
    /// to select `nerd_font` without restating it under `[theme.glyphs]`. An
    /// explicit `glyphs.set` (the `config.set` the caller passes through) still
    /// wins; the matching inline set overrides apply last.
    pub(crate) fn resolve(set: Option<&str>, config: &ThemeGlyphsConfig) -> Self {
        let kind = GlyphSetKind::from_source(set);
        let mut glyphs = GlyphRole::ALL
            .iter()
            .map(|&role| {
                let glyph = match kind {
                    GlyphSetKind::Unicode => unicode_glyph(role),
                    GlyphSetKind::NerdFont => {
                        nerd_font_glyph(role).unwrap_or_else(|| unicode_glyph(role))
                    }
                };
                (role, glyph.to_owned())
            })
            .collect::<BTreeMap<_, _>>();

        apply_overrides(&mut glyphs, config, kind);

        Self { kind, glyphs }
    }

    pub(crate) fn glyph(&self, role: GlyphRole) -> &str {
        self.glyphs
            .get(&role)
            .map(String::as_str)
            .unwrap_or_else(|| unicode_glyph(role))
    }

    pub(crate) fn kind(&self) -> GlyphSetKind {
        self.kind
    }
}

fn apply_overrides(
    glyphs: &mut BTreeMap<GlyphRole, String>,
    config: &ThemeGlyphsConfig,
    kind: GlyphSetKind,
) {
    for &role in GlyphRole::ALL {
        if let Some(glyph) = config.glyph(kind.name(), role) {
            glyphs.insert(role, glyph.to_owned());
        }
    }
}

pub(crate) fn unicode_glyph(role: GlyphRole) -> &'static str {
    match role {
        GlyphRole::StatusWaiting => "?",
        GlyphRole::StatusAttention => "!",
        GlyphRole::StatusPaused => "⏸\u{FE0E}",
        GlyphRole::StatusDone => "✓",
        GlyphRole::StatusIdle => "○",
        GlyphRole::StatusWorking => "⢿",
        GlyphRole::StatusThinking => "⠁",
        GlyphRole::StatusDelegating => "⢄",
        GlyphRole::StatusResolving => "⠙",
        GlyphRole::StatusCompacting => "▇",
        GlyphRole::CockpitWorkspace => "⌘",
        GlyphRole::CockpitSessions => "◎",
        GlyphRole::CockpitAgents => "¤",
        GlyphRole::TokensTotal => "◇",
        GlyphRole::TokensInput => "↘",
        GlyphRole::TokensOutput => "↗",
        GlyphRole::TokensCacheRead => "◌",
        GlyphRole::TokensCacheWrite => "◍",
        GlyphRole::TokensFilled => "▤",
        GlyphRole::TokensCompaction => "↻",
        GlyphRole::MeterContextFull => "▣",
        GlyphRole::MeterContextEmpty => "▢",
        GlyphRole::MeterBarFilled => "━",
        GlyphRole::MeterBarTrack => "─",
        GlyphRole::MeterBarCap => "╺",
        GlyphRole::MeterBarHalf => "╸",
        GlyphRole::MeterManaFilled => "▰",
        GlyphRole::MeterManaTrack => "▱",
        GlyphRole::MeterReset => "↻",
        GlyphRole::MeterUnlimited => "∞",
        GlyphRole::MeterScrollThumb => "▐",
        GlyphRole::MeterScrollTrack => "▕",
        GlyphRole::ClockQ1 => "◔",
        GlyphRole::ClockQ2 => "◑",
        GlyphRole::ClockQ3 => "◕",
        GlyphRole::ClockQ4 => "●",
        GlyphRole::ClockOver => "◉",
        GlyphRole::WorktreeBranch => "⑂",
        GlyphRole::WorktreeMerge => "⮌",
        GlyphRole::WorktreeAhead => "⇡",
        GlyphRole::WorktreeBehind => "⇣",
        GlyphRole::WorktreeTrunkEqual => "≡",
        GlyphRole::WorktreeTrunkBranch => "⑂",
        GlyphRole::WorktreeTrunkMerge => "✓",
        GlyphRole::WorktreePrOpen => "⊙",
        GlyphRole::WorktreePrClosed => "✕",
        GlyphRole::WorktreeReconciling => "⟳",
        GlyphRole::WorktreeExpand => "▸",
        GlyphRole::WorktreeDotted => "┄",
        GlyphRole::ChannelHash => "#",
        GlyphRole::CardSubagents => "⧉",
        GlyphRole::CardParkedBg => "⋯",
        GlyphRole::ProcessCpu => "C",
        GlyphRole::ProcessMem => "M",
        GlyphRole::ProcessIo => "⇅",
        GlyphRole::KeysMove => "↕",
        GlyphRole::KeysFocus => "⏎",
        GlyphRole::KeysInbox => "␣",
        GlyphRole::KeysRead => "✉",
        GlyphRole::KeysUnread => "●",
        GlyphRole::KeysAll => "≡",
        GlyphRole::KeysAccounts => "↔",
        GlyphRole::KeysReload => "⟳",
        GlyphRole::KeysDismiss => "✕",
        GlyphRole::KeysSidebar => "▐",
        GlyphRole::ChromeAlert => "⚠",
        GlyphRole::ChromePresenceAway => "zᶻ",
        GlyphRole::ChromeRemoteLink => "⇄",
        GlyphRole::ChromeRemoteControl => "⇅",
        GlyphRole::ChromeHairline => "─",
        GlyphRole::ChromeBoxTopLeft => "╭",
        GlyphRole::ChromeBoxTopRight => "╮",
        GlyphRole::ChromeBoxBottomLeft => "╰",
        GlyphRole::ChromeBoxBottomRight => "╯",
        GlyphRole::ChromeBoxVertical => "│",
        GlyphRole::ChromeTabCapLeft => "┤",
        GlyphRole::ChromeTabCapRight => "├",
        GlyphRole::ChromeSpineCardLeft => "▌",
        GlyphRole::ChromeSpineCardRight => "▐",
        GlyphRole::ChromeSpineLaneLeft => "▎",
        GlyphRole::ChromeSpineLaneRight => "🮇",
        GlyphRole::ChromeInfinity => "∞",
    }
}

/// The Nerd Font glyph for a role, or `None` when the preset keeps the role's
/// Unicode default. The Nerd Font set is the Unicode base with this curated icon
/// selection layered over it (see `docs/guide/theme.md#glyphs`): the drawn
/// gauges, spines, caps, hairline, the dotted seal, the compacting wave, and the
/// status spinner/clock heads (which animate through frame sequences in
/// `animation.rs`) all return `None` and keep their box-drawing or Unicode shape,
/// while the mana bar swaps to the `nf-extra` progress segments in `meters.rs`.
pub(crate) fn nerd_font_glyph(role: GlyphRole) -> Option<&'static str> {
    Some(match role {
        // status heads — the resting glyph for each agent state.
        GlyphRole::StatusWaiting => "\u{f128}", // nf-fa-question
        GlyphRole::StatusAttention => "\u{f12a}", // nf-fa-exclamation
        GlyphRole::StatusPaused => "\u{f04c}",  // nf-fa-pause
        GlyphRole::StatusDone => "\u{f00c}",    // nf-fa-check
        GlyphRole::StatusIdle => "\u{f2dd}",    // nf-fa-superpowers
        // the working head and the animated spinners keep their Unicode braille so
        // the cockpit's running representative matches the spinner; the wave too.
        GlyphRole::StatusWorking
        | GlyphRole::StatusThinking
        | GlyphRole::StatusDelegating
        | GlyphRole::StatusResolving
        | GlyphRole::StatusCompacting => return None,
        // cockpit identity row. Every icon ships single-cell to match the "Mono"
        // Nerd Font builds; a face that draws them double-width pads per-glyph with a
        // trailing space (see docs/guide/theme.md#glyphs).
        GlyphRole::CockpitWorkspace => "\u{eda7}", // nf-fa-seedling
        GlyphRole::CockpitSessions => "\u{ee83}",  // nf-fa-splotch
        GlyphRole::CockpitAgents => "\u{ee9c}",    // nf-fa-brain
        // token-accounting markers.
        GlyphRole::TokensTotal => "\u{ed58}",  // nf-fa-ethereum
        GlyphRole::TokensInput => "\u{f103}",  // nf-fa-angle_double_down
        GlyphRole::TokensOutput => "\u{f102}", // nf-fa-angle_double_up
        GlyphRole::TokensCacheRead => "\u{f1978}", // nf-md-dots_circle
        GlyphRole::TokensCacheWrite => "\u{f1c0}", // nf-fa-database
        GlyphRole::TokensFilled => "\u{f0fe6}", // nf-md-texture_box (filled context)
        GlyphRole::TokensCompaction => "\u{f0e2}", // nf-fa-arrow_rotate_left
        // meter context tiles and the budget-reset marker. The unlimited marker
        // stays Unicode so it shares the reset marker's visual scale.
        GlyphRole::MeterContextFull => "\u{f0570}", // nf-md-view_grid (full context)
        GlyphRole::MeterContextEmpty => "\u{f11d9}", // nf-md-view_grid_outline (empty context)
        GlyphRole::MeterReset => "\u{f0450}",       // nf-md-refresh (rate-limit reset)
        GlyphRole::MeterUnlimited => return None,
        // the drawn bars, mana fill, and scrollbar keep their box-drawing shape.
        GlyphRole::MeterBarFilled
        | GlyphRole::MeterBarTrack
        | GlyphRole::MeterBarCap
        | GlyphRole::MeterBarHalf
        | GlyphRole::MeterManaFilled
        | GlyphRole::MeterManaTrack
        | GlyphRole::MeterScrollThumb
        | GlyphRole::MeterScrollTrack => return None,
        // age clock: the Nerd Font preset fills the circle-slice series by elapsed
        // time in `labels::glyphs`, so the per-quarter roles stay Unicode here.
        GlyphRole::ClockQ1
        | GlyphRole::ClockQ2
        | GlyphRole::ClockQ3
        | GlyphRole::ClockQ4
        | GlyphRole::ClockOver => return None,
        // worktree header: branch/merge and trunk state markers iconify.
        GlyphRole::WorktreeBranch => "\u{f126}", // nf-fa-code_branch
        GlyphRole::WorktreeMerge => "\u{f17f}",  // nf-fa-code_merge
        GlyphRole::ChannelHash => "\u{f292}",    // nf-fa-hashtag
        GlyphRole::WorktreeTrunkBranch => "\u{f418}", // nf-oct-git_branch
        GlyphRole::WorktreeTrunkMerge => "\u{f419}", // nf-oct-git_merge
        GlyphRole::WorktreePrOpen => "\u{f407}", // nf-oct-git_pull_request
        GlyphRole::WorktreePrClosed => "\u{f4dc}", // nf-oct-git_pull_request_closed
        GlyphRole::WorktreeReconciling => "\u{f4db}", // nf-oct-git_merge_queue
        GlyphRole::WorktreeAhead
        | GlyphRole::WorktreeBehind
        | GlyphRole::WorktreeTrunkEqual
        | GlyphRole::WorktreeExpand
        | GlyphRole::WorktreeDotted => return None,
        // agent card.
        GlyphRole::CardSubagents => "\u{ed50}", // nf-fa-gitter
        GlyphRole::CardParkedBg => return None,
        // process resource grid.
        GlyphRole::ProcessCpu => "\u{ef8f}", // nf-fa-bars_progress
        GlyphRole::ProcessMem => "\u{efc5}", // nf-fa-memory
        GlyphRole::ProcessIo => "\u{f09f}",  // nf-fa-up_down
        // help-overlay action keys.
        GlyphRole::KeysMove => "\u{f07d}",     // nf-fa-arrows_v
        GlyphRole::KeysFocus => "\u{f05b}",    // nf-fa-crosshairs
        GlyphRole::KeysInbox => "\u{f01c}",    // nf-fa-inbox
        GlyphRole::KeysRead => "\u{f0e0}",     // nf-fa-envelope
        GlyphRole::KeysUnread => "\u{f111}",   // nf-fa-circle
        GlyphRole::KeysAll => "\u{f03a}",      // nf-fa-list
        GlyphRole::KeysAccounts => "\u{f07e}", // nf-fa-arrows_h
        GlyphRole::KeysReload => "\u{f021}",   // nf-fa-refresh
        GlyphRole::KeysDismiss => "\u{f00d}",  // nf-fa-times
        GlyphRole::KeysSidebar => "\u{f0db}",  // nf-fa-columns
        // chrome: the presence, network link, and infinity badges iconify; framing stays drawn.
        GlyphRole::ChromePresenceAway => "\u{f186}", // nf-fa-moon_o
        GlyphRole::ChromeRemoteLink => "\u{ede3}",   // nf-fa-tower_broadcast
        GlyphRole::ChromeInfinity => "\u{edfe}",     // nf-fa-infinity
        GlyphRole::ChromeAlert
        | GlyphRole::ChromeRemoteControl
        | GlyphRole::ChromeHairline
        | GlyphRole::ChromeBoxTopLeft
        | GlyphRole::ChromeBoxTopRight
        | GlyphRole::ChromeBoxBottomLeft
        | GlyphRole::ChromeBoxBottomRight
        | GlyphRole::ChromeBoxVertical
        | GlyphRole::ChromeTabCapLeft
        | GlyphRole::ChromeTabCapRight
        | GlyphRole::ChromeSpineCardLeft
        | GlyphRole::ChromeSpineCardRight
        | GlyphRole::ChromeSpineLaneLeft
        | GlyphRole::ChromeSpineLaneRight => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{validate_glyph_cells, validate_single_cell};

    #[test]
    fn every_builtin_glyph_fits_its_slot() {
        for &role in GlyphRole::ALL {
            // Most shipped glyphs are one cell. The presence badge's Unicode
            // sleep cue deliberately uses a two-cell "zᶻ" cluster in footer
            // chrome, where the layout measures the whole badge span.
            if role == GlyphRole::ChromePresenceAway {
                validate_glyph_cells(unicode_glyph(role))
                    .unwrap_or_else(|err| panic!("unicode {}: {err}", role.namespaced_name()));
            } else {
                validate_single_cell(unicode_glyph(role))
                    .unwrap_or_else(|err| panic!("unicode {}: {err}", role.namespaced_name()));
            }
            if let Some(nerd) = nerd_font_glyph(role) {
                validate_single_cell(nerd)
                    .unwrap_or_else(|err| panic!("nerd-font {}: {err}", role.namespaced_name()));
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

        // The curated icons are real Nerd Font codepoints, distinct from Unicode.
        for role in [
            GlyphRole::CockpitWorkspace,
            GlyphRole::TokensTotal,
            GlyphRole::StatusIdle,
            GlyphRole::WorktreeBranch,
            GlyphRole::WorktreeMerge,
            GlyphRole::ChannelHash,
            GlyphRole::WorktreeTrunkBranch,
            GlyphRole::WorktreeTrunkMerge,
            GlyphRole::WorktreePrOpen,
            GlyphRole::WorktreePrClosed,
            GlyphRole::WorktreeReconciling,
            GlyphRole::MeterReset,
            GlyphRole::KeysFocus,
            GlyphRole::KeysUnread,
            GlyphRole::KeysAll,
            GlyphRole::KeysSidebar,
            GlyphRole::ChromePresenceAway,
            GlyphRole::ChromeInfinity,
        ] {
            let nerd = nerd_font_glyph(role).expect("curated icon");
            assert_ne!(
                nerd,
                unicode_glyph(role),
                "{} carries a Nerd Font icon",
                role.namespaced_name()
            );
        }
    }

    #[test]
    fn role_names_round_trip() {
        for &role in GlyphRole::ALL {
            assert_eq!(
                GlyphRole::from_namespaced(role.namespace(), role.name()),
                Some(role),
                "{} maps back to the same role",
                role.namespaced_name()
            );
        }
    }

    #[test]
    fn channel_hash_has_unicode_and_nerd_font_glyphs() {
        assert_eq!(unicode_glyph(GlyphRole::ChannelHash), "#");
        assert_eq!(nerd_font_glyph(GlyphRole::ChannelHash), Some("\u{f292}"));
    }

    #[test]
    fn worktree_expand_keeps_its_unicode_chevron_in_both_sets() {
        assert_eq!(unicode_glyph(GlyphRole::WorktreeExpand), "▸");
        assert_eq!(nerd_font_glyph(GlyphRole::WorktreeExpand), None);
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
        let glyphs = GlyphSet::resolve(config.set.as_deref(), &config);
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
    fn template_glyph_defaults_match_presets() {
        #[derive(serde::Deserialize)]
        struct ThemeFile {
            theme: crate::config::ThemeConfig,
        }

        let parsed: ThemeFile = toml::from_str(crate::config::MachineConfig::template_theme())
            .expect("theme template parses");
        let mut unicode_config = parsed.theme.glyphs.clone();
        unicode_config.set = Some("unicode".to_owned());
        let from_template = GlyphSet::resolve(unicode_config.set.as_deref(), &unicode_config);
        let default = GlyphSet::default();
        for &role in GlyphRole::ALL {
            assert_eq!(
                from_template.glyph(role),
                default.glyph(role),
                "active template default for {} equals the Unicode preset",
                role.namespaced_name()
            );
        }

        let mut nerd_config = parsed.theme.glyphs;
        nerd_config.set = Some("nerd_font".to_owned());
        let from_template = GlyphSet::resolve(nerd_config.set.as_deref(), &nerd_config);
        let expected_config = ThemeGlyphsConfig {
            set: Some("nerd_font".to_owned()),
            ..ThemeGlyphsConfig::default()
        };
        let expected = GlyphSet::resolve(expected_config.set.as_deref(), &expected_config);
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
