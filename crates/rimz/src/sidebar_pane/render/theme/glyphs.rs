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
        let file_config = config
            .set
            .as_deref()
            .and_then(glyph_set::explicit_glyph_config);
        let kind = file_config
            .as_ref()
            .and_then(|config| config.set.as_deref())
            .map(|source| GlyphSetKind::from_source(Some(source)))
            .unwrap_or_else(|| GlyphSetKind::from_source(config.set.as_deref()));
        let mut glyphs = GlyphRole::ALL
            .iter()
            .map(|&role| {
                let glyph = match kind {
                    GlyphSetKind::Unicode => unicode_glyph(role),
                    GlyphSetKind::NerdFont => nerd_font_glyph(role),
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
        GlyphRole::MarkerTokenTotal => "◇",
        GlyphRole::MarkerTokenIn => "↘",
        GlyphRole::MarkerTokenOut => "↗",
        GlyphRole::MarkerCacheRead => "◌",
        GlyphRole::MarkerCacheWrite => "◍",
        GlyphRole::MarkerSessions => "◎",
        GlyphRole::MarkerCompaction => "↻",
        GlyphRole::MarkerSubagents => "⧉",
        GlyphRole::MarkerActiveAgents => "¤",
        GlyphRole::MarkerTodoDone => "●",
        GlyphRole::MarkerTodoPending => "○",
        GlyphRole::MarkerRemoteControl => "⇅",
        GlyphRole::MarkerProcCpu => "C",
        GlyphRole::MarkerProcMem => "M",
        GlyphRole::MarkerProcIo => "⇅",
        GlyphRole::MarkerInfinity => "∞",
        GlyphRole::MeterBarFilled => "━",
        GlyphRole::MeterBarTrack => "─",
        GlyphRole::MeterBarCap => "╸",
        GlyphRole::MeterManaFilled => "▰",
        GlyphRole::MeterManaTrack => "▱",
        GlyphRole::MeterScrollThumb => "▐",
        GlyphRole::MeterScrollTrack => "▕",
        GlyphRole::MeterContextFull => "▣",
        GlyphRole::MeterContextEmpty => "▢",
        GlyphRole::MeterContextFilled => "▤",
        GlyphRole::ClockQ1 => "◔",
        GlyphRole::ClockQ2 => "◑",
        GlyphRole::ClockQ3 => "◕",
        GlyphRole::ClockQ4 => "●",
        GlyphRole::ClockOver => "◉",
        GlyphRole::StructureCardSpineLeft => "▌",
        GlyphRole::StructureCardSpineRight => "▐",
        GlyphRole::StructureLaneSpineLeft => "▎",
        GlyphRole::StructureLaneSpineRight => "🮇",
        GlyphRole::StructureTabCapLeft => "┤",
        GlyphRole::StructureTabCapRight => "├",
        GlyphRole::StructureBranch => "⑂",
        GlyphRole::StructureAhead => "⇡",
        GlyphRole::StructureBehind => "⇣",
        GlyphRole::StructureTrunkEqual => "≡",
        GlyphRole::StructureTrunkClear => "✓",
        GlyphRole::StructureDotted => "┄",
        GlyphRole::ChromeWorkspace => "⌘",
        GlyphRole::ChromeAlert => "⚠",
        GlyphRole::ChromeRemoteLink => "⇄",
        GlyphRole::ChromeHairline => "─",
    }
}

pub(crate) fn nerd_font_glyph(role: GlyphRole) -> &'static str {
    match role {
        GlyphRole::MarkerTokenTotal => "\u{f04a0}", // nf-md-sigma
        GlyphRole::MarkerTokenIn => "\u{f0120}",    // nf-md-tray_arrow_down
        GlyphRole::MarkerTokenOut => "\u{f011d}",   // nf-md-tray_arrow_up
        GlyphRole::MarkerCacheRead => "\u{f163b}",  // nf-md-database_arrow_down
        GlyphRole::MarkerCacheWrite => "\u{f163e}", // nf-md-database_arrow_up
        GlyphRole::MarkerSessions => "\u{f02da}",   // nf-md-history
        GlyphRole::MarkerCompaction => "\u{f0450}", // nf-md-refresh
        GlyphRole::MarkerSubagents => "\u{f0e8}",   // nf-fa-sitemap
        GlyphRole::MarkerActiveAgents => "\u{eb31}", // nf-cod-pulse
        GlyphRole::MarkerTodoDone => "\u{f05e0}",   // nf-md-check_circle
        GlyphRole::MarkerTodoPending => "\u{f0130}", // nf-md-checkbox_blank_circle_outline
        GlyphRole::MarkerRemoteControl => "\u{f04e2}", // nf-md-swap_vertical
        GlyphRole::MarkerProcCpu => "C",
        GlyphRole::MarkerProcMem => "M",
        GlyphRole::MarkerProcIo => "\u{f0bce}", // nf-md-swap_vertical_bold
        GlyphRole::MarkerInfinity => "\u{f06e4}", // nf-md-infinity
        GlyphRole::MeterBarFilled => "━",
        GlyphRole::MeterBarTrack => "─",
        GlyphRole::MeterBarCap => "╸",
        GlyphRole::MeterManaFilled => "▰",
        GlyphRole::MeterManaTrack => "▱",
        GlyphRole::MeterScrollThumb => "▐",
        GlyphRole::MeterScrollTrack => "▕",
        GlyphRole::MeterContextFull => "▣",
        GlyphRole::MeterContextEmpty => "▢",
        GlyphRole::MeterContextFilled => "▤",
        GlyphRole::ClockQ1 => "\u{f0a9f}",  // nf-md-circle_slice_2
        GlyphRole::ClockQ2 => "\u{f0aa1}",  // nf-md-circle_slice_4
        GlyphRole::ClockQ3 => "\u{f0aa3}",  // nf-md-circle_slice_6
        GlyphRole::ClockQ4 => "\u{f0aa5}",  // nf-md-circle_slice_8
        GlyphRole::ClockOver => "\u{ea71}", // nf-cod-circle_filled
        GlyphRole::StructureCardSpineLeft => "▌",
        GlyphRole::StructureCardSpineRight => "▐",
        GlyphRole::StructureLaneSpineLeft => "▎",
        GlyphRole::StructureLaneSpineRight => "🮇",
        GlyphRole::StructureTabCapLeft => "┤",
        GlyphRole::StructureTabCapRight => "├",
        GlyphRole::StructureBranch => "\u{e725}", // nf-dev-git_branch
        GlyphRole::StructureAhead => "\u{f0737}", // nf-md-arrow_up_bold
        GlyphRole::StructureBehind => "\u{f072e}", // nf-md-arrow_down_bold
        GlyphRole::StructureTrunkEqual => "\u{f01fc}", // nf-md-equal
        GlyphRole::StructureTrunkClear => "\u{f012c}", // nf-md-check
        GlyphRole::StructureDotted => "┄",
        GlyphRole::ChromeWorkspace => "\u{ea62}", // nf-cod-repo
        GlyphRole::ChromeAlert => "\u{f0026}",    // nf-md-alert
        GlyphRole::ChromeRemoteLink => "\u{f04e1}", // nf-md-swap_horizontal
        GlyphRole::ChromeHairline => "─",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate_single_cell;
    use tempfile::tempdir;

    #[test]
    fn every_builtin_glyph_is_one_cell() {
        for &role in GlyphRole::ALL {
            validate_single_cell(unicode_glyph(role))
                .unwrap_or_else(|err| panic!("unicode {}: {err}", role.namespaced_name()));
            validate_single_cell(nerd_font_glyph(role))
                .unwrap_or_else(|err| panic!("nerd-font {}: {err}", role.namespaced_name()));
        }
    }

    #[test]
    fn nerd_font_keeps_drawn_glyph_roles_unicode() {
        for &role in GlyphRole::ALL {
            if role.is_drawn_unicode_in_nerd_font() {
                assert_eq!(
                    nerd_font_glyph(role),
                    unicode_glyph(role),
                    "{} keeps its drawn Unicode glyph",
                    role.namespaced_name()
                );
            }
        }
    }

    #[test]
    fn resolves_named_set_and_explicit_overrides() {
        let config: SidebarGlyphsConfig = toml::from_str(
            "set = \"nerd-font\"\n\
             [marker]\n\
             token_total = \"◇\"\n",
        )
        .expect("glyph config");
        let glyphs = GlyphSet::resolve(&config);
        assert_eq!(glyphs.kind(), GlyphSetKind::NerdFont);
        assert_eq!(glyphs.glyph(GlyphRole::MarkerTokenTotal), "◇");
        assert_eq!(
            glyphs.glyph(GlyphRole::StructureBranch),
            nerd_font_glyph(GlyphRole::StructureBranch)
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
             [marker]\n\
             token_total = \"◇\"\n",
        )
        .expect("write glyph file");
        let config = SidebarGlyphsConfig {
            set: Some(file.display().to_string()),
            ..SidebarGlyphsConfig::default()
        };

        let glyphs = GlyphSet::resolve(&config);

        assert_eq!(glyphs.kind(), GlyphSetKind::NerdFont);
        assert_eq!(glyphs.glyph(GlyphRole::MarkerTokenTotal), "◇");
        assert_eq!(
            glyphs.glyph(GlyphRole::StructureBranch),
            nerd_font_glyph(GlyphRole::StructureBranch)
        );
    }
}
