use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::validate_single_cell;

/// A configurable sidebar glyph role. The namespace groups match the TOML
/// shape under `[sidebar.glyphs.<namespace>]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphRole {
    MarkerTokenTotal,
    MarkerTokenIn,
    MarkerTokenOut,
    MarkerCacheRead,
    MarkerCacheWrite,
    MarkerSessions,
    MarkerCompaction,
    MarkerSubagents,
    MarkerActiveAgents,
    MarkerTodoDone,
    MarkerTodoPending,
    MarkerRemoteControl,
    MarkerProcCpu,
    MarkerProcMem,
    MarkerProcIo,
    MarkerInfinity,
    MeterBarFilled,
    MeterBarTrack,
    MeterBarCap,
    MeterManaFilled,
    MeterManaTrack,
    MeterScrollThumb,
    MeterScrollTrack,
    MeterContextFull,
    MeterContextEmpty,
    MeterContextFilled,
    ClockQ1,
    ClockQ2,
    ClockQ3,
    ClockQ4,
    ClockOver,
    StructureCardSpineLeft,
    StructureCardSpineRight,
    StructureLaneSpineLeft,
    StructureLaneSpineRight,
    StructureTabCapLeft,
    StructureTabCapRight,
    StructureBranch,
    StructureAhead,
    StructureBehind,
    StructureTrunkEqual,
    StructureTrunkClear,
    StructureDotted,
    ChromeWorkspace,
    ChromeAlert,
    ChromeRemoteLink,
    ChromeHairline,
}

impl GlyphRole {
    pub const ALL: &'static [Self] = &[
        Self::MarkerTokenTotal,
        Self::MarkerTokenIn,
        Self::MarkerTokenOut,
        Self::MarkerCacheRead,
        Self::MarkerCacheWrite,
        Self::MarkerSessions,
        Self::MarkerCompaction,
        Self::MarkerSubagents,
        Self::MarkerActiveAgents,
        Self::MarkerTodoDone,
        Self::MarkerTodoPending,
        Self::MarkerRemoteControl,
        Self::MarkerProcCpu,
        Self::MarkerProcMem,
        Self::MarkerProcIo,
        Self::MarkerInfinity,
        Self::MeterBarFilled,
        Self::MeterBarTrack,
        Self::MeterBarCap,
        Self::MeterManaFilled,
        Self::MeterManaTrack,
        Self::MeterScrollThumb,
        Self::MeterScrollTrack,
        Self::MeterContextFull,
        Self::MeterContextEmpty,
        Self::MeterContextFilled,
        Self::ClockQ1,
        Self::ClockQ2,
        Self::ClockQ3,
        Self::ClockQ4,
        Self::ClockOver,
        Self::StructureCardSpineLeft,
        Self::StructureCardSpineRight,
        Self::StructureLaneSpineLeft,
        Self::StructureLaneSpineRight,
        Self::StructureTabCapLeft,
        Self::StructureTabCapRight,
        Self::StructureBranch,
        Self::StructureAhead,
        Self::StructureBehind,
        Self::StructureTrunkEqual,
        Self::StructureTrunkClear,
        Self::StructureDotted,
        Self::ChromeWorkspace,
        Self::ChromeAlert,
        Self::ChromeRemoteLink,
        Self::ChromeHairline,
    ];

    pub fn namespace(self) -> &'static str {
        match self {
            Self::MarkerTokenTotal
            | Self::MarkerTokenIn
            | Self::MarkerTokenOut
            | Self::MarkerCacheRead
            | Self::MarkerCacheWrite
            | Self::MarkerSessions
            | Self::MarkerCompaction
            | Self::MarkerSubagents
            | Self::MarkerActiveAgents
            | Self::MarkerTodoDone
            | Self::MarkerTodoPending
            | Self::MarkerRemoteControl
            | Self::MarkerProcCpu
            | Self::MarkerProcMem
            | Self::MarkerProcIo
            | Self::MarkerInfinity => "marker",
            Self::MeterBarFilled
            | Self::MeterBarTrack
            | Self::MeterBarCap
            | Self::MeterManaFilled
            | Self::MeterManaTrack
            | Self::MeterScrollThumb
            | Self::MeterScrollTrack
            | Self::MeterContextFull
            | Self::MeterContextEmpty
            | Self::MeterContextFilled => "meter",
            Self::ClockQ1 | Self::ClockQ2 | Self::ClockQ3 | Self::ClockQ4 | Self::ClockOver => {
                "clock"
            }
            Self::StructureCardSpineLeft
            | Self::StructureCardSpineRight
            | Self::StructureLaneSpineLeft
            | Self::StructureLaneSpineRight
            | Self::StructureTabCapLeft
            | Self::StructureTabCapRight
            | Self::StructureBranch
            | Self::StructureAhead
            | Self::StructureBehind
            | Self::StructureTrunkEqual
            | Self::StructureTrunkClear
            | Self::StructureDotted => "structure",
            Self::ChromeWorkspace
            | Self::ChromeAlert
            | Self::ChromeRemoteLink
            | Self::ChromeHairline => "chrome",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::MarkerTokenTotal => "token_total",
            Self::MarkerTokenIn => "token_in",
            Self::MarkerTokenOut => "token_out",
            Self::MarkerCacheRead => "cache_read",
            Self::MarkerCacheWrite => "cache_write",
            Self::MarkerSessions => "sessions",
            Self::MarkerCompaction => "compaction",
            Self::MarkerSubagents => "subagents",
            Self::MarkerActiveAgents => "active_agents",
            Self::MarkerTodoDone => "todo_done",
            Self::MarkerTodoPending => "todo_pending",
            Self::MarkerRemoteControl => "remote_control",
            Self::MarkerProcCpu => "proc_cpu",
            Self::MarkerProcMem => "proc_mem",
            Self::MarkerProcIo => "proc_io",
            Self::MarkerInfinity => "infinity",
            Self::MeterBarFilled => "bar_filled",
            Self::MeterBarTrack => "bar_track",
            Self::MeterBarCap => "bar_cap",
            Self::MeterManaFilled => "mana_filled",
            Self::MeterManaTrack => "mana_track",
            Self::MeterScrollThumb => "scroll_thumb",
            Self::MeterScrollTrack => "scroll_track",
            Self::MeterContextFull => "context_full",
            Self::MeterContextEmpty => "context_empty",
            Self::MeterContextFilled => "context_filled",
            Self::ClockQ1 => "q1",
            Self::ClockQ2 => "q2",
            Self::ClockQ3 => "q3",
            Self::ClockQ4 => "q4",
            Self::ClockOver => "over",
            Self::StructureCardSpineLeft => "card_spine_left",
            Self::StructureCardSpineRight => "card_spine_right",
            Self::StructureLaneSpineLeft => "lane_spine_left",
            Self::StructureLaneSpineRight => "lane_spine_right",
            Self::StructureTabCapLeft => "tab_cap_left",
            Self::StructureTabCapRight => "tab_cap_right",
            Self::StructureBranch => "branch",
            Self::StructureAhead => "ahead",
            Self::StructureBehind => "behind",
            Self::StructureTrunkEqual => "trunk_equal",
            Self::StructureTrunkClear => "trunk_clear",
            Self::StructureDotted => "dotted",
            Self::ChromeWorkspace => "workspace",
            Self::ChromeAlert => "alert",
            Self::ChromeRemoteLink => "remote_link",
            Self::ChromeHairline => "hairline",
        }
    }

    pub fn namespaced_name(self) -> String {
        format!("{}.{}", self.namespace(), self.name())
    }

    pub fn from_namespaced(namespace: &str, name: &str) -> Option<Self> {
        match (namespace, name) {
            ("marker", "token_total") => Some(Self::MarkerTokenTotal),
            ("marker", "token_in") => Some(Self::MarkerTokenIn),
            ("marker", "token_out") => Some(Self::MarkerTokenOut),
            ("marker", "cache_read") => Some(Self::MarkerCacheRead),
            ("marker", "cache_write") => Some(Self::MarkerCacheWrite),
            ("marker", "sessions") => Some(Self::MarkerSessions),
            ("marker", "compaction") => Some(Self::MarkerCompaction),
            ("marker", "subagents") => Some(Self::MarkerSubagents),
            ("marker", "active_agents") => Some(Self::MarkerActiveAgents),
            ("marker", "todo_done") => Some(Self::MarkerTodoDone),
            ("marker", "todo_pending") => Some(Self::MarkerTodoPending),
            ("marker", "remote_control") => Some(Self::MarkerRemoteControl),
            ("marker", "proc_cpu") => Some(Self::MarkerProcCpu),
            ("marker", "proc_mem") => Some(Self::MarkerProcMem),
            ("marker", "proc_io") => Some(Self::MarkerProcIo),
            ("marker", "infinity") => Some(Self::MarkerInfinity),
            ("meter", "bar_filled") => Some(Self::MeterBarFilled),
            ("meter", "bar_track") => Some(Self::MeterBarTrack),
            ("meter", "bar_cap") => Some(Self::MeterBarCap),
            ("meter", "mana_filled") => Some(Self::MeterManaFilled),
            ("meter", "mana_track") => Some(Self::MeterManaTrack),
            ("meter", "scroll_thumb") => Some(Self::MeterScrollThumb),
            ("meter", "scroll_track") => Some(Self::MeterScrollTrack),
            ("meter", "context_full") => Some(Self::MeterContextFull),
            ("meter", "context_empty") => Some(Self::MeterContextEmpty),
            ("meter", "context_filled") => Some(Self::MeterContextFilled),
            ("clock", "q1") => Some(Self::ClockQ1),
            ("clock", "q2") => Some(Self::ClockQ2),
            ("clock", "q3") => Some(Self::ClockQ3),
            ("clock", "q4") => Some(Self::ClockQ4),
            ("clock", "over") => Some(Self::ClockOver),
            ("structure", "card_spine_left") => Some(Self::StructureCardSpineLeft),
            ("structure", "card_spine_right") => Some(Self::StructureCardSpineRight),
            ("structure", "lane_spine_left") => Some(Self::StructureLaneSpineLeft),
            ("structure", "lane_spine_right") => Some(Self::StructureLaneSpineRight),
            ("structure", "tab_cap_left") => Some(Self::StructureTabCapLeft),
            ("structure", "tab_cap_right") => Some(Self::StructureTabCapRight),
            ("structure", "branch") => Some(Self::StructureBranch),
            ("structure", "ahead") => Some(Self::StructureAhead),
            ("structure", "behind") => Some(Self::StructureBehind),
            ("structure", "trunk_equal") => Some(Self::StructureTrunkEqual),
            ("structure", "trunk_clear") => Some(Self::StructureTrunkClear),
            ("structure", "dotted") => Some(Self::StructureDotted),
            ("chrome", "workspace") => Some(Self::ChromeWorkspace),
            ("chrome", "alert") => Some(Self::ChromeAlert),
            ("chrome", "remote_link") => Some(Self::ChromeRemoteLink),
            ("chrome", "hairline") => Some(Self::ChromeHairline),
            _ => None,
        }
    }

    pub fn is_drawn_unicode_in_nerd_font(self) -> bool {
        matches!(
            self,
            Self::MeterBarFilled
                | Self::MeterBarTrack
                | Self::MeterBarCap
                | Self::MeterManaFilled
                | Self::MeterManaTrack
                | Self::MeterScrollThumb
                | Self::MeterScrollTrack
                | Self::MeterContextFull
                | Self::MeterContextEmpty
                | Self::MeterContextFilled
                | Self::StructureCardSpineLeft
                | Self::StructureCardSpineRight
                | Self::StructureLaneSpineLeft
                | Self::StructureLaneSpineRight
                | Self::StructureTabCapLeft
                | Self::StructureTabCapRight
                | Self::StructureDotted
                | Self::ChromeHairline
        )
    }
}

/// Sparse per-namespace glyph overrides.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct GlyphGroup(BTreeMap<String, String>);

impl GlyphGroup {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn validate(namespace: &str, values: BTreeMap<String, String>) -> Result<Self, String> {
        for (name, value) in &values {
            if GlyphRole::from_namespaced(namespace, name).is_none() {
                return Err(format!("unknown sidebar glyph role `{namespace}.{name}`"));
            }
            validate_single_cell(value)
                .map_err(|err| format!("sidebar glyph `{namespace}.{name}` {err}"))?;
        }
        Ok(Self(values))
    }
}

/// `[sidebar.glyphs]`: the selected glyph preset and sparse user overrides.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarGlyphsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub marker: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub meter: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub clock: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub structure: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub chrome: GlyphGroup,
}

impl SidebarGlyphsConfig {
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    pub fn glyph(&self, role: GlyphRole) -> Option<&str> {
        let group = match role.namespace() {
            "marker" => &self.marker,
            "meter" => &self.meter,
            "clock" => &self.clock,
            "structure" => &self.structure,
            "chrome" => &self.chrome,
            _ => return None,
        };
        group.get(role.name())
    }
}

impl<'de> Deserialize<'de> for SidebarGlyphsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct RawSidebarGlyphsConfig {
            set: Option<String>,
            marker: BTreeMap<String, String>,
            meter: BTreeMap<String, String>,
            clock: BTreeMap<String, String>,
            structure: BTreeMap<String, String>,
            chrome: BTreeMap<String, String>,
        }

        let raw = RawSidebarGlyphsConfig::deserialize(deserializer)?;
        Ok(Self {
            set: raw.set,
            marker: GlyphGroup::validate("marker", raw.marker).map_err(serde::de::Error::custom)?,
            meter: GlyphGroup::validate("meter", raw.meter).map_err(serde::de::Error::custom)?,
            clock: GlyphGroup::validate("clock", raw.clock).map_err(serde::de::Error::custom)?,
            structure: GlyphGroup::validate("structure", raw.structure)
                .map_err(serde::de::Error::custom)?,
            chrome: GlyphGroup::validate("chrome", raw.chrome).map_err(serde::de::Error::custom)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sparse_overrides_and_defaults_to_unset() {
        let config: SidebarGlyphsConfig = toml::from_str(
            "[marker]\n\
             token_total = \"◇\"\n\
             [clock]\n\
             over = \"◉\"\n",
        )
        .expect("glyphs config");
        assert_eq!(config.set, None);
        assert_eq!(config.glyph(GlyphRole::MarkerTokenTotal), Some("◇"));
        assert_eq!(config.glyph(GlyphRole::ClockOver), Some("◉"));
        assert!(SidebarGlyphsConfig::default().is_unset());
    }

    #[test]
    fn validates_known_roles_and_one_cell_values() {
        let err = toml::from_str::<SidebarGlyphsConfig>("[marker]\nnope = \"x\"\n")
            .expect_err("unknown role")
            .to_string();
        assert!(err.contains("unknown sidebar glyph role `marker.nope`"));

        let err = toml::from_str::<SidebarGlyphsConfig>("[makr]\ntoken_total = \"Σ\"\n")
            .expect_err("unknown namespace")
            .to_string();
        assert!(err.contains("unknown field `makr`"));

        let err = toml::from_str::<SidebarGlyphsConfig>("[marker]\ntoken_total = \"ab\"\n")
            .expect_err("wide glyph")
            .to_string();
        assert!(err.contains("must occupy exactly one terminal cell"));

        let err = toml::from_str::<SidebarGlyphsConfig>("[marker]\ntoken_total = \"\"\n")
            .expect_err("empty glyph")
            .to_string();
        assert!(err.contains("must not contain empty glyphs"));
    }

    #[test]
    fn serializes_only_changed_keys() {
        let config: SidebarGlyphsConfig =
            toml::from_str("[marker]\ntoken_total = \"◇\"\n").expect("glyphs config");
        let serialized = toml::to_string(&config).expect("serialize");
        assert!(serialized.contains("[marker]"));
        assert!(serialized.contains("token_total = \"◇\""));
        assert_eq!(
            toml::to_string(&SidebarGlyphsConfig::default()).expect("serialize"),
            ""
        );
    }
}
