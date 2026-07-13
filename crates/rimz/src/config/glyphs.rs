use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::validate_glyph_cells;

/// A configurable sidebar glyph role. The namespaces mirror the on-screen
/// reading order, so `[theme.glyphs.<set>.<namespace>]` groups the glyphs the
/// way the sidebar lays them out: `status` heads, the `cockpit` identity row,
/// `tokens`, `meter` bars, the age `clock`, the `worktree` header, the agent
/// `card`, `process` rows, help `keys`, and `chrome`.
macro_rules! glyph_roles {
    ($($namespace:literal { $($variant:ident => $name:literal,)+ })+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        #[repr(usize)]
        pub enum GlyphRole {
            $($($variant,)+)+
        }

        const ROLE_TABLE: &[(GlyphRole, &str, &str)] = &[
            $($( (GlyphRole::$variant, $namespace, $name), )+)+
        ];

        impl GlyphRole {
            pub const ALL: &'static [Self] = &[
                $($( Self::$variant, )+)+
            ];

            const NAMESPACES: &'static [&'static str] = &[
                $($namespace,)+
            ];

            fn role_entry(self) -> (&'static str, &'static str) {
                let (role, namespace, name) = ROLE_TABLE[self as usize];
                debug_assert_eq!(role, self);
                (namespace, name)
            }
        }
    };
}

glyph_roles! {
    "status" {
        StatusWaiting => "waiting",
        StatusAttention => "attention",
        StatusPaused => "paused",
        StatusDone => "done",
        StatusIdle => "idle",
        StatusWorking => "working",
        StatusThinking => "thinking",
        StatusDelegating => "delegating",
        StatusResolving => "resolving",
        StatusCompacting => "compacting",
    }
    "cockpit" {
        CockpitWorkspace => "workspace",
        CockpitSessions => "sessions",
        CockpitAgents => "agents",
    }
    "tokens" {
        TokensTotal => "total",
        TokensInput => "input",
        TokensOutput => "output",
        TokensCacheRead => "cache_read",
        TokensCacheWrite => "cache_write",
        TokensFilled => "filled",
        TokensCompaction => "compaction",
    }
    "meter" {
        MeterContextFull => "context_full",
        MeterContextEmpty => "context_empty",
        MeterBarFilled => "bar_filled",
        MeterBarTrack => "bar_track",
        MeterBarCap => "bar_cap",
        MeterBarHalf => "bar_half",
        MeterManaFilled => "mana_filled",
        MeterManaTrack => "mana_track",
        MeterReset => "reset",
        MeterScrollThumb => "scroll_thumb",
        MeterScrollTrack => "scroll_track",
    }
    "clock" {
        ClockQ1 => "q1",
        ClockQ2 => "q2",
        ClockQ3 => "q3",
        ClockQ4 => "q4",
        ClockOver => "over",
    }
    "worktree" {
        WorktreeBranch => "branch",
        WorktreeMerge => "merge",
        WorktreeAhead => "ahead",
        WorktreeBehind => "behind",
        WorktreeTrunkEqual => "trunk_equal",
        WorktreeTrunkBranch => "trunk_branch",
        WorktreeTrunkMerge => "trunk_merge",
        WorktreePrOpen => "pr_open",
        WorktreePrClosed => "pr_closed",
        WorktreeReconciling => "reconciling",
        WorktreeDotted => "dotted",
        ChannelHash => "channel_hash",
    }
    "card" {
        CardSubagents => "subagents",
        CardParkedBg => "parked_bg",
    }
    "process" {
        ProcessCpu => "cpu",
        ProcessMem => "mem",
        ProcessIo => "io",
    }
    "keys" {
        KeysMove => "move",
        KeysFocus => "focus",
        KeysInbox => "inbox",
        KeysRead => "read",
        KeysUnread => "unread",
        KeysAll => "all",
        KeysAccounts => "accounts",
        KeysReload => "reload",
        KeysDismiss => "dismiss",
        KeysSidebar => "sidebar",
    }
    "chrome" {
        ChromeAlert => "alert",
        ChromePresenceAway => "presence_away",
        ChromeRemoteLink => "remote_link",
        ChromeRemoteControl => "remote_control",
        ChromeHairline => "hairline",
        ChromeBoxTopLeft => "box_top_left",
        ChromeBoxTopRight => "box_top_right",
        ChromeBoxBottomLeft => "box_bottom_left",
        ChromeBoxBottomRight => "box_bottom_right",
        ChromeBoxVertical => "box_vertical",
        ChromeTabCapLeft => "tab_cap_left",
        ChromeTabCapRight => "tab_cap_right",
        ChromeSpineCardLeft => "spine_card_left",
        ChromeSpineCardRight => "spine_card_right",
        ChromeSpineLaneLeft => "spine_lane_left",
        ChromeSpineLaneRight => "spine_lane_right",
        ChromeInfinity => "infinity",
    }
}

impl GlyphRole {
    pub fn namespace(self) -> &'static str {
        self.role_entry().0
    }

    pub fn name(self) -> &'static str {
        self.role_entry().1
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

/// Sparse glyph overrides for one named glyph set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphOverrides(BTreeMap<GlyphRole, String>);

impl GlyphOverrides {
    pub fn glyph(&self, role: GlyphRole) -> Option<&str> {
        self.0.get(&role).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for GlyphOverrides {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let groups = self.ordered_groups();
        let mut map = serializer.serialize_map(Some(groups.len()))?;
        for (namespace, values) in groups {
            map.serialize_entry(namespace, &values)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for GlyphOverrides {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = BTreeMap::<String, BTreeMap<String, String>>::deserialize(deserializer)?;
        let mut overrides = BTreeMap::new();
        for (namespace, values) in raw {
            if !GlyphRole::NAMESPACES.contains(&namespace.as_str()) {
                return Err(serde::de::Error::unknown_field(
                    &namespace,
                    GlyphRole::NAMESPACES,
                ));
            }
            for (name, value) in values {
                let Some(role) = GlyphRole::from_namespaced(&namespace, &name) else {
                    return Err(serde::de::Error::custom(format!(
                        "unknown sidebar glyph role `{namespace}.{name}`"
                    )));
                };
                validate_glyph_cells(&value).map_err(|err| {
                    serde::de::Error::custom(format!("sidebar glyph `{namespace}.{name}` {err}"))
                })?;
                overrides.insert(role, value);
            }
        }
        Ok(Self(overrides))
    }
}

impl GlyphOverrides {
    fn ordered_groups(&self) -> Vec<(&'static str, BTreeMap<&'static str, &str>)> {
        let mut groups = Vec::new();
        for &namespace in GlyphRole::NAMESPACES {
            let mut values = BTreeMap::new();
            for (&role, glyph) in &self.0 {
                if role.namespace() == namespace {
                    values.insert(role.name(), glyph.as_str());
                }
            }
            if !values.is_empty() {
                groups.push((namespace, values));
            }
        }
        groups
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
    #[serde(skip_serializing_if = "GlyphOverrides::is_empty")]
    pub unicode: GlyphOverrides,
    #[serde(skip_serializing_if = "GlyphOverrides::is_empty")]
    pub nerd_font: GlyphOverrides,
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
            unicode: GlyphOverrides,
            nerd_font: GlyphOverrides,
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

pub fn validate_glyph_source(name: &str) -> Result<(), String> {
    if is_named_glyph_set(name) {
        Ok(())
    } else {
        Err(format!(
            "unknown theme glyph set `{name}`; expected unicode or nerd_font"
        ))
    }
}

pub fn glyph_lookup_hint() -> String {
    "named sets: unicode, nerd_font".to_owned()
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
             over = \"◉\"\n\
             [unicode.keys]\n\
             focus = \"F\"\n\
             [unicode.chrome]\n\
             box_vertical = \"|\"\n",
        )
        .expect("glyphs config");
        assert_eq!(config.set, None);
        assert_eq!(config.glyph("unicode", GlyphRole::StatusWorking), Some("⢿"));
        assert_eq!(config.glyph("unicode", GlyphRole::ClockOver), Some("◉"));
        assert_eq!(config.glyph("unicode", GlyphRole::KeysFocus), Some("F"));
        assert_eq!(
            config.glyph("unicode", GlyphRole::ChromeBoxVertical),
            Some("|")
        );
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
