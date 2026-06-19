use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::validate_glyph_cells;

/// A configurable sidebar glyph role. The namespaces mirror the on-screen
/// reading order, so `[theme.glyphs.<set>.<namespace>]` groups the glyphs the
/// way the sidebar lays them out: `status` heads, the `cockpit` identity row,
/// `tokens`, `meter` bars, the age `clock`, the `worktree` header, the agent
/// `card`, `process` rows, and `chrome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphRole {
    // status — the leading cell of every agent row.
    StatusWaiting,
    StatusAttention,
    StatusPaused,
    StatusDone,
    StatusIdle,
    StatusWorking,
    StatusThinking,
    StatusDelegating,
    StatusResolving,
    StatusCompacting,
    // cockpit — the identity and summary row.
    CockpitWorkspace,
    CockpitSessions,
    CockpitAgents,
    // tokens — the token-accounting markers.
    TokensTotal,
    TokensInput,
    TokensOutput,
    TokensCacheRead,
    TokensCacheWrite,
    TokensFilled,
    TokensCompaction,
    // meter — the drawn gauges and bars.
    MeterContextFull,
    MeterContextEmpty,
    MeterBarFilled,
    MeterBarTrack,
    MeterBarCap,
    MeterManaFilled,
    MeterManaTrack,
    MeterReset,
    MeterScrollThumb,
    MeterScrollTrack,
    // clock — the last-activity age face.
    ClockQ1,
    ClockQ2,
    ClockQ3,
    ClockQ4,
    ClockOver,
    // worktree — the group header's git story.
    WorktreeBranch,
    WorktreeAhead,
    WorktreeBehind,
    WorktreeTrunkEqual,
    WorktreeTrunkClear,
    WorktreeDotted,
    // card — the agent card body.
    CardSubagents,
    CardTodoDone,
    CardTodoPending,
    CardParkedBg,
    // process — the process-row resource grid.
    ProcessCpu,
    ProcessMem,
    ProcessIo,
    // chrome — framing, spines, tabs, and badges.
    ChromeAlert,
    ChromeRemoteLink,
    ChromeRemoteControl,
    ChromeHairline,
    ChromeTabCapLeft,
    ChromeTabCapRight,
    ChromeSpineCardLeft,
    ChromeSpineCardRight,
    ChromeSpineLaneLeft,
    ChromeSpineLaneRight,
    ChromeInfinity,
}

impl GlyphRole {
    pub const ALL: &'static [Self] = &[
        Self::StatusWaiting,
        Self::StatusAttention,
        Self::StatusPaused,
        Self::StatusDone,
        Self::StatusIdle,
        Self::StatusWorking,
        Self::StatusThinking,
        Self::StatusDelegating,
        Self::StatusResolving,
        Self::StatusCompacting,
        Self::CockpitWorkspace,
        Self::CockpitSessions,
        Self::CockpitAgents,
        Self::TokensTotal,
        Self::TokensInput,
        Self::TokensOutput,
        Self::TokensCacheRead,
        Self::TokensCacheWrite,
        Self::TokensFilled,
        Self::TokensCompaction,
        Self::MeterContextFull,
        Self::MeterContextEmpty,
        Self::MeterBarFilled,
        Self::MeterBarTrack,
        Self::MeterBarCap,
        Self::MeterManaFilled,
        Self::MeterManaTrack,
        Self::MeterReset,
        Self::MeterScrollThumb,
        Self::MeterScrollTrack,
        Self::ClockQ1,
        Self::ClockQ2,
        Self::ClockQ3,
        Self::ClockQ4,
        Self::ClockOver,
        Self::WorktreeBranch,
        Self::WorktreeAhead,
        Self::WorktreeBehind,
        Self::WorktreeTrunkEqual,
        Self::WorktreeTrunkClear,
        Self::WorktreeDotted,
        Self::CardSubagents,
        Self::CardTodoDone,
        Self::CardTodoPending,
        Self::CardParkedBg,
        Self::ProcessCpu,
        Self::ProcessMem,
        Self::ProcessIo,
        Self::ChromeAlert,
        Self::ChromeRemoteLink,
        Self::ChromeRemoteControl,
        Self::ChromeHairline,
        Self::ChromeTabCapLeft,
        Self::ChromeTabCapRight,
        Self::ChromeSpineCardLeft,
        Self::ChromeSpineCardRight,
        Self::ChromeSpineLaneLeft,
        Self::ChromeSpineLaneRight,
        Self::ChromeInfinity,
    ];

    pub fn namespace(self) -> &'static str {
        match self {
            Self::StatusWaiting
            | Self::StatusAttention
            | Self::StatusPaused
            | Self::StatusDone
            | Self::StatusIdle
            | Self::StatusWorking
            | Self::StatusThinking
            | Self::StatusDelegating
            | Self::StatusResolving
            | Self::StatusCompacting => "status",
            Self::CockpitWorkspace | Self::CockpitSessions | Self::CockpitAgents => "cockpit",
            Self::TokensTotal
            | Self::TokensInput
            | Self::TokensOutput
            | Self::TokensCacheRead
            | Self::TokensCacheWrite
            | Self::TokensFilled
            | Self::TokensCompaction => "tokens",
            Self::MeterContextFull
            | Self::MeterContextEmpty
            | Self::MeterBarFilled
            | Self::MeterBarTrack
            | Self::MeterBarCap
            | Self::MeterManaFilled
            | Self::MeterManaTrack
            | Self::MeterReset
            | Self::MeterScrollThumb
            | Self::MeterScrollTrack => "meter",
            Self::ClockQ1 | Self::ClockQ2 | Self::ClockQ3 | Self::ClockQ4 | Self::ClockOver => {
                "clock"
            }
            Self::WorktreeBranch
            | Self::WorktreeAhead
            | Self::WorktreeBehind
            | Self::WorktreeTrunkEqual
            | Self::WorktreeTrunkClear
            | Self::WorktreeDotted => "worktree",
            Self::CardSubagents
            | Self::CardTodoDone
            | Self::CardTodoPending
            | Self::CardParkedBg => "card",
            Self::ProcessCpu | Self::ProcessMem | Self::ProcessIo => "process",
            Self::ChromeAlert
            | Self::ChromeRemoteLink
            | Self::ChromeRemoteControl
            | Self::ChromeHairline
            | Self::ChromeTabCapLeft
            | Self::ChromeTabCapRight
            | Self::ChromeSpineCardLeft
            | Self::ChromeSpineCardRight
            | Self::ChromeSpineLaneLeft
            | Self::ChromeSpineLaneRight
            | Self::ChromeInfinity => "chrome",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::StatusWaiting => "waiting",
            Self::StatusAttention => "attention",
            Self::StatusPaused => "paused",
            Self::StatusDone => "done",
            Self::StatusIdle => "idle",
            Self::StatusWorking => "working",
            Self::StatusThinking => "thinking",
            Self::StatusDelegating => "delegating",
            Self::StatusResolving => "resolving",
            Self::StatusCompacting => "compacting",
            Self::CockpitWorkspace => "workspace",
            Self::CockpitSessions => "sessions",
            Self::CockpitAgents => "agents",
            Self::TokensTotal => "total",
            Self::TokensInput => "input",
            Self::TokensOutput => "output",
            Self::TokensCacheRead => "cache_read",
            Self::TokensCacheWrite => "cache_write",
            Self::TokensFilled => "filled",
            Self::TokensCompaction => "compaction",
            Self::MeterContextFull => "context_full",
            Self::MeterContextEmpty => "context_empty",
            Self::MeterBarFilled => "bar_filled",
            Self::MeterBarTrack => "bar_track",
            Self::MeterBarCap => "bar_cap",
            Self::MeterManaFilled => "mana_filled",
            Self::MeterManaTrack => "mana_track",
            Self::MeterReset => "reset",
            Self::MeterScrollThumb => "scroll_thumb",
            Self::MeterScrollTrack => "scroll_track",
            Self::ClockQ1 => "q1",
            Self::ClockQ2 => "q2",
            Self::ClockQ3 => "q3",
            Self::ClockQ4 => "q4",
            Self::ClockOver => "over",
            Self::WorktreeBranch => "branch",
            Self::WorktreeAhead => "ahead",
            Self::WorktreeBehind => "behind",
            Self::WorktreeTrunkEqual => "trunk_equal",
            Self::WorktreeTrunkClear => "trunk_clear",
            Self::WorktreeDotted => "dotted",
            Self::CardSubagents => "subagents",
            Self::CardTodoDone => "todo_done",
            Self::CardTodoPending => "todo_pending",
            Self::CardParkedBg => "parked_bg",
            Self::ProcessCpu => "cpu",
            Self::ProcessMem => "mem",
            Self::ProcessIo => "io",
            Self::ChromeAlert => "alert",
            Self::ChromeRemoteLink => "remote_link",
            Self::ChromeRemoteControl => "remote_control",
            Self::ChromeHairline => "hairline",
            Self::ChromeTabCapLeft => "tab_cap_left",
            Self::ChromeTabCapRight => "tab_cap_right",
            Self::ChromeSpineCardLeft => "spine_card_left",
            Self::ChromeSpineCardRight => "spine_card_right",
            Self::ChromeSpineLaneLeft => "spine_lane_left",
            Self::ChromeSpineLaneRight => "spine_lane_right",
            Self::ChromeInfinity => "infinity",
        }
    }

    pub fn namespaced_name(self) -> String {
        format!("{}.{}", self.namespace(), self.name())
    }

    pub fn from_namespaced(namespace: &str, name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.namespace() == namespace && role.name() == name)
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
            validate_glyph_cells(value)
                .map_err(|err| format!("sidebar glyph `{namespace}.{name}` {err}"))?;
        }
        Ok(Self(values))
    }
}

/// Sparse glyph overrides for one named glyph set, grouped by the sidebar's
/// on-screen zones.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct GlyphNamespaces {
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub status: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub cockpit: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub tokens: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub meter: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub clock: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub worktree: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub card: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub process: GlyphGroup,
    #[serde(skip_serializing_if = "GlyphGroup::is_empty")]
    pub chrome: GlyphGroup,
}

impl GlyphNamespaces {
    pub fn glyph(&self, role: GlyphRole) -> Option<&str> {
        let group = match role.namespace() {
            "status" => &self.status,
            "cockpit" => &self.cockpit,
            "tokens" => &self.tokens,
            "meter" => &self.meter,
            "clock" => &self.clock,
            "worktree" => &self.worktree,
            "card" => &self.card,
            "process" => &self.process,
            "chrome" => &self.chrome,
            _ => return None,
        };
        group.get(role.name())
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl<'de> Deserialize<'de> for GlyphNamespaces {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct RawGlyphNamespaces {
            status: BTreeMap<String, String>,
            cockpit: BTreeMap<String, String>,
            tokens: BTreeMap<String, String>,
            meter: BTreeMap<String, String>,
            clock: BTreeMap<String, String>,
            worktree: BTreeMap<String, String>,
            card: BTreeMap<String, String>,
            process: BTreeMap<String, String>,
            chrome: BTreeMap<String, String>,
        }

        let raw = RawGlyphNamespaces::deserialize(deserializer)?;
        let group = |namespace, values| {
            GlyphGroup::validate(namespace, values).map_err(serde::de::Error::custom)
        };
        Ok(Self {
            status: group("status", raw.status)?,
            cockpit: group("cockpit", raw.cockpit)?,
            tokens: group("tokens", raw.tokens)?,
            meter: group("meter", raw.meter)?,
            clock: group("clock", raw.clock)?,
            worktree: group("worktree", raw.worktree)?,
            card: group("card", raw.card)?,
            process: group("process", raw.process)?,
            chrome: group("chrome", raw.chrome)?,
        })
    }
}

/// `[theme.glyphs]`: the selected glyph preset and both first-class glyph sets.
/// The selected set starts from the built-in preset, then overlays the matching
/// inline namespace table.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ThemeGlyphsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    #[serde(skip_serializing_if = "GlyphNamespaces::is_empty")]
    pub unicode: GlyphNamespaces,
    #[serde(skip_serializing_if = "GlyphNamespaces::is_empty")]
    pub nerd_font: GlyphNamespaces,
}

impl ThemeGlyphsConfig {
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    pub fn glyph(&self, set: &str, role: GlyphRole) -> Option<&str> {
        match set {
            "unicode" => self.unicode.glyph(role),
            "nerd_font" => self.nerd_font.glyph(role),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ThemeGlyphsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct RawThemeGlyphsConfig {
            set: Option<String>,
            unicode: GlyphNamespaces,
            nerd_font: GlyphNamespaces,
        }

        let raw = RawThemeGlyphsConfig::deserialize(deserializer)?;
        if let Some(set) = raw.set.as_deref()
            && !is_named_glyph_set(set)
        {
            return Err(serde::de::Error::custom(format!(
                "unknown theme glyph set `{set}`; expected unicode or nerd_font"
            )));
        }
        Ok(Self {
            set: raw.set,
            unicode: raw.unicode,
            nerd_font: raw.nerd_font,
        })
    }
}

pub fn is_named_glyph_set(name: &str) -> bool {
    matches!(name, "unicode" | "nerd_font")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sparse_overrides_and_defaults_to_unset() {
        let config: ThemeGlyphsConfig = toml::from_str(
            "[unicode.status]\n\
             working = \"⢿\"\n\
             [unicode.clock]\n\
             over = \"◉\"\n",
        )
        .expect("glyphs config");
        assert_eq!(config.set, None);
        assert_eq!(config.glyph("unicode", GlyphRole::StatusWorking), Some("⢿"));
        assert_eq!(config.glyph("unicode", GlyphRole::ClockOver), Some("◉"));
        assert!(ThemeGlyphsConfig::default().is_unset());
    }

    #[test]
    fn status_group_sets_the_whole_make_up_row_at_once() {
        let config: ThemeGlyphsConfig = toml::from_str(
            "[unicode.status]\n\
             waiting = \"?\"\n\
             attention = \"!\"\n\
             paused = \"⏸\"\n\
             done = \"✓\"\n\
             working = \"⢿\"\n\
             idle = \"○\"\n",
        )
        .expect("glyphs config");
        assert_eq!(config.glyph("unicode", GlyphRole::StatusWaiting), Some("?"));
        assert_eq!(
            config.glyph("unicode", GlyphRole::StatusAttention),
            Some("!")
        );
        assert_eq!(config.glyph("unicode", GlyphRole::StatusPaused), Some("⏸"));
        assert_eq!(config.glyph("unicode", GlyphRole::StatusDone), Some("✓"));
        assert_eq!(config.glyph("unicode", GlyphRole::StatusWorking), Some("⢿"));
        assert_eq!(config.glyph("unicode", GlyphRole::StatusIdle), Some("○"));
    }

    #[test]
    fn validates_known_roles_and_one_cell_values() {
        let err = toml::from_str::<ThemeGlyphsConfig>("[unicode.tokens]\nnope = \"x\"\n")
            .expect_err("unknown role")
            .to_string();
        assert!(err.contains("unknown sidebar glyph role `tokens.nope`"));

        let err = toml::from_str::<ThemeGlyphsConfig>("[unicode.makr]\ntotal = \"Σ\"\n")
            .expect_err("unknown namespace")
            .to_string();
        assert!(err.contains("unknown field `makr`"));

        let err = toml::from_str::<ThemeGlyphsConfig>("[unicode.tokens]\ntotal = \"abc\"\n")
            .expect_err("over-wide glyph")
            .to_string();
        assert!(err.contains("must occupy one or two terminal cells"));

        // A double-width glyph padded to two cells is accepted.
        toml::from_str::<ThemeGlyphsConfig>("[unicode.tokens]\ntotal = \"\u{efa0} \"\n")
            .expect("double-width glyph");

        let err = toml::from_str::<ThemeGlyphsConfig>("[unicode.tokens]\ntotal = \"\"\n")
            .expect_err("empty glyph")
            .to_string();
        assert!(err.contains("must not contain empty glyphs"));

        let err = toml::from_str::<ThemeGlyphsConfig>("set = \"nerd-font\"\n")
            .expect_err("old set spelling")
            .to_string();
        assert!(err.contains("expected unicode or nerd_font"));
    }

    #[test]
    fn serializes_only_changed_keys() {
        let config: ThemeGlyphsConfig =
            toml::from_str("[unicode.tokens]\ntotal = \"◇\"\n").expect("glyphs config");
        let serialized = toml::to_string(&config).expect("serialize");
        assert!(serialized.contains("[unicode.tokens]"));
        assert!(serialized.contains("total = \"◇\""));
        assert_eq!(
            toml::to_string(&ThemeGlyphsConfig::default()).expect("serialize"),
            ""
        );
    }
}
