use std::collections::BTreeMap;

use crate::config::{GlyphRole, SidebarGlyphsConfig};

use super::super::glyph_set;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlyphSetKind {
    Unicode,
    NerdFont,
}

impl GlyphSetKind {
    fn from_source(source: Option<&str>) -> Self {
        match source {
            Some("nerd-font") => Self::NerdFont,
            _ => Self::Unicode,
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
        Self::resolve(&SidebarGlyphsConfig::default())
    }
}

impl GlyphSet {
    pub(crate) fn resolve(config: &SidebarGlyphsConfig) -> Self {
        Self::resolve_with_set(config.set.as_deref(), config)
    }

    /// Resolve with an explicit set source, used by the `[sidebar] style` preset
    /// to select `nerd-font` without restating it under `[sidebar.glyphs]`. An
    /// explicit `glyphs.set` (the `config.set` the caller passes through) still
    /// wins; the per-namespace overrides in `config` always apply last.
    pub(crate) fn resolve_with_set(set: Option<&str>, config: &SidebarGlyphsConfig) -> Self {
        let file_config = set.and_then(glyph_set::explicit_glyph_config);
        let kind = file_config
            .as_ref()
            .and_then(|config| config.set.as_deref())
            .map(|source| GlyphSetKind::from_source(Some(source)))
            .unwrap_or_else(|| GlyphSetKind::from_source(set));
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

        if let Some(file_config) = file_config {
            apply_overrides(&mut glyphs, &file_config);
        }
        apply_overrides(&mut glyphs, config);

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

fn apply_overrides(glyphs: &mut BTreeMap<GlyphRole, String>, config: &SidebarGlyphsConfig) {
    for &role in GlyphRole::ALL {
        if let Some(glyph) = config.glyph(role) {
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
        GlyphRole::MeterBarCap => "╸",
        GlyphRole::MeterManaFilled => "▰",
        GlyphRole::MeterManaTrack => "▱",
        GlyphRole::MeterReset => "↻",
        GlyphRole::MeterScrollThumb => "▐",
        GlyphRole::MeterScrollTrack => "▕",
        GlyphRole::ClockQ1 => "◔",
        GlyphRole::ClockQ2 => "◑",
        GlyphRole::ClockQ3 => "◕",
        GlyphRole::ClockQ4 => "●",
        GlyphRole::ClockOver => "◉",
        GlyphRole::WorktreeBranch => "⑂",
        GlyphRole::WorktreeAhead => "⇡",
        GlyphRole::WorktreeBehind => "⇣",
        GlyphRole::WorktreeTrunkEqual => "≡",
        GlyphRole::WorktreeTrunkClear => "✓",
        GlyphRole::WorktreeDotted => "┄",
        GlyphRole::CardSubagents => "⧉",
        GlyphRole::CardTodoDone => "●",
        GlyphRole::CardTodoPending => "○",
        GlyphRole::CardParkedBg => "⋯",
        GlyphRole::ProcessCpu => "C",
        GlyphRole::ProcessMem => "M",
        GlyphRole::ProcessIo => "⇅",
        GlyphRole::ChromeAlert => "⚠",
        GlyphRole::ChromeRemoteLink => "⇄",
        GlyphRole::ChromeRemoteControl => "⇅",
        GlyphRole::ChromeHairline => "─",
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
/// selection layered over it (see `docs/reference/theme.md#glyphs`): the drawn
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
        // trailing space (see docs/reference/theme.md#glyphs).
        GlyphRole::CockpitWorkspace => "\u{efa0}", // nf-fa-git_alt
        GlyphRole::CockpitSessions => "\u{ef15}",  // nf-fa-wand_sparkles
        GlyphRole::CockpitAgents => "\u{ee9c}",    // nf-fa-brain
        // token-accounting markers.
        GlyphRole::TokensTotal => "\u{ed58}",  // nf-fa-ethereum
        GlyphRole::TokensInput => "\u{f103}",  // nf-fa-angle_double_down
        GlyphRole::TokensOutput => "\u{f102}", // nf-fa-angle_double_up
        GlyphRole::TokensCacheRead => "\u{f1978}", // nf-md-dots_circle
        GlyphRole::TokensCacheWrite => "\u{f1c0}", // nf-fa-database
        GlyphRole::TokensFilled => "\u{f0fe6}", // nf-md-texture_box (filled context)
        GlyphRole::TokensCompaction => "\u{f0e2}", // nf-fa-arrow_rotate_left
        // meter context tiles and the budget-reset marker.
        GlyphRole::MeterContextFull => "\u{f0570}", // nf-md-view_grid (full context)
        GlyphRole::MeterContextEmpty => "\u{f11d9}", // nf-md-view_grid_outline (empty context)
        GlyphRole::MeterReset => "\u{f0450}",       // nf-md-refresh (rate-limit reset)
        // the drawn bars, mana fill, and scrollbar keep their box-drawing shape.
        GlyphRole::MeterBarFilled
        | GlyphRole::MeterBarTrack
        | GlyphRole::MeterBarCap
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
        // worktree header: only the branch glyph iconifies.
        GlyphRole::WorktreeBranch => "\u{e725}", // nf-dev-git_branch
        GlyphRole::WorktreeAhead
        | GlyphRole::WorktreeBehind
        | GlyphRole::WorktreeTrunkEqual
        | GlyphRole::WorktreeTrunkClear
        | GlyphRole::WorktreeDotted => return None,
        // agent card.
        GlyphRole::CardSubagents => "\u{ef81}", // nf-fa-folder_tree
        GlyphRole::CardTodoDone | GlyphRole::CardTodoPending | GlyphRole::CardParkedBg => {
            return None;
        }
        // process resource grid.
        GlyphRole::ProcessCpu => "\u{ef8f}", // nf-fa-bars_progress
        GlyphRole::ProcessMem => "\u{efc5}", // nf-fa-memory
        GlyphRole::ProcessIo => "\u{f09f}",  // nf-fa-up_down
        // chrome: the network link and infinity badges iconify; framing stays drawn.
        GlyphRole::ChromeRemoteLink => "\u{ede3}", // nf-fa-tower_broadcast
        GlyphRole::ChromeInfinity => "\u{edfe}",   // nf-fa-infinity
        GlyphRole::ChromeAlert
        | GlyphRole::ChromeRemoteControl
        | GlyphRole::ChromeHairline
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
    use crate::config::validate_single_cell;
    use tempfile::tempdir;

    #[test]
    fn every_builtin_glyph_is_one_cell() {
        for &role in GlyphRole::ALL {
            // Every shipped glyph — the Unicode base and the Nerd Font overlay alike
            // — is a single cell. A face that draws an icon double-width is
            // reconciled by a per-glyph override that pads a trailing space, never by
            // the table.
            validate_single_cell(unicode_glyph(role))
                .unwrap_or_else(|err| panic!("unicode {}: {err}", role.namespaced_name()));
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
            GlyphRole::MeterManaFilled,
            GlyphRole::MeterManaTrack,
            GlyphRole::MeterScrollThumb,
            GlyphRole::MeterScrollTrack,
            GlyphRole::ClockQ1,
            GlyphRole::ClockQ2,
            GlyphRole::ClockQ3,
            GlyphRole::ClockQ4,
            GlyphRole::ClockOver,
            GlyphRole::WorktreeAhead,
            GlyphRole::WorktreeBehind,
            GlyphRole::WorktreeTrunkEqual,
            GlyphRole::WorktreeTrunkClear,
            GlyphRole::WorktreeDotted,
            GlyphRole::CardTodoDone,
            GlyphRole::CardTodoPending,
            GlyphRole::CardParkedBg,
            GlyphRole::ChromeAlert,
            GlyphRole::ChromeRemoteControl,
            GlyphRole::ChromeHairline,
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
            GlyphRole::MeterReset,
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
    fn resolves_named_set_and_explicit_overrides() {
        let config: SidebarGlyphsConfig = toml::from_str(
            "set = \"nerd-font\"\n\
             [status]\n\
             working = \"⢿\"\n",
        )
        .expect("glyph config");
        let glyphs = GlyphSet::resolve(&config);
        assert_eq!(glyphs.kind(), GlyphSetKind::NerdFont);
        assert_eq!(glyphs.glyph(GlyphRole::StatusWorking), "⢿");
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
    fn custom_file_set_picks_base_before_overrides() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("glyphs.toml");
        std::fs::write(
            &file,
            "set = \"nerd-font\"\n\
             [status]\n\
             working = \"⢿\"\n",
        )
        .expect("write glyph file");
        let config = SidebarGlyphsConfig {
            set: Some(file.display().to_string()),
            ..SidebarGlyphsConfig::default()
        };

        let glyphs = GlyphSet::resolve(&config);

        assert_eq!(glyphs.kind(), GlyphSetKind::NerdFont);
        assert_eq!(glyphs.glyph(GlyphRole::StatusWorking), "⢿");
        assert_eq!(
            glyphs.glyph(GlyphRole::WorktreeBranch),
            nerd_font_glyph(GlyphRole::WorktreeBranch).expect("branch icon")
        );
    }

    #[test]
    fn template_glyph_defaults_match_unicode_preset() {
        let config: crate::config::MachineConfig =
            toml::from_str(crate::config::MachineConfig::template()).expect("template parses");
        let from_template = GlyphSet::resolve(&config.sidebar.glyphs);
        let default = GlyphSet::default();
        for &role in GlyphRole::ALL {
            assert_eq!(
                from_template.glyph(role),
                default.glyph(role),
                "active template default for {} equals the Unicode preset",
                role.namespaced_name()
            );
        }
    }
}
