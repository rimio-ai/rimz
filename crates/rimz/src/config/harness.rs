use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agents::BudgetWindow;
use crate::harness::budget::BudgetSpec;
use crate::utils::time::{DurationUnit, parse_duration_units};

/// Request anchoring excludes final generation; covers the producer tick, helper spawn, and submission.
pub(crate) const PROMPT_CACHE_MARGIN: Duration = Duration::from_secs(60);

const DEFAULT_COMPACT_INSTRUCTION: &str = "Summarize the transcript inside <summary></summary> tags. Include relevant information in the summary such that this conversation will be continued by a new context window without needing to redo work or be reprovided with relevant constraints or context. Be sure to preserve: (1) any difficulties or problems that came up, and how they were handled or resolved; (2) any possibilities, options, or approaches that were raised, tried, or set aside, and why; (3) anything that was asked for, decided, agreed, ruled out, or established as a preference, constraint, or boundary — stated exactly; (4) exactly where things stand now — what has been covered, settled, or completed so far; (5) anything still open, unresolved, promised, or expected to happen next; (6) specific details that would be hard to reconstruct — names, numbers, dates, exact wording, links or references — kept exactly. Be complete on these even at the cost of length; keep everything else concise. Weight the two voices differently: keep what the user said, asked for, shared, or established carefully and close to their own words; your own explanations and reasoning can be condensed much further, to what they concluded or produced — as long as nothing in the six items above is dropped.";
const TEAM_COMPACT_INSTRUCTION: &str = "Summarize the transcript inside <summary></summary> tags so a new context window continues without redoing work. This seat is a team member: on resume I reread the board, the stage files, and git, so point at them by path and section and copy nothing they hold. Keep only what this window alone knows: (1) facts, decisions, and drafts not yet filed, and the words of the user and teammates that shaped them, each tagged with the file and section it belongs in, so my first act on resume writes it there; (2) what is in flight: replies I wait on, messages I owe.";

/// The seat an agent holds, which selects RimZ's built-in compaction brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactSeat {
    /// A standalone agent: its summary is its only memory.
    Solo,
    /// A team member: the board, stage files, and git carry the run's memory.
    Team,
}

/// A local-calendar-day dollar cap stored as cents so machine config keeps
/// exact equality while reusing the public budget grammar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DayCap {
    cents: u64,
}

impl DayCap {
    pub(crate) fn as_usd(self) -> f64 {
        self.cents as f64 / 100.0
    }

    pub fn as_spec(self) -> BudgetSpec {
        BudgetSpec {
            cap_usd: self.as_usd(),
            window: BudgetWindow::Day,
        }
    }
}

impl FromStr for DayCap {
    type Err = DayCapParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let spec = raw
            .parse::<BudgetSpec>()
            .map_err(DayCapParseError::Budget)?;
        if spec.window != BudgetWindow::Day {
            return Err(DayCapParseError::DayRequired(raw.trim().to_owned()));
        }
        let cents = (spec.cap_usd * 100.0).round();
        if cents > u64::MAX as f64 {
            return Err(DayCapParseError::TooLarge(raw.trim().to_owned()));
        }
        Ok(Self {
            cents: cents as u64,
        })
    }
}

impl fmt::Display for DayCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollars = self.cents / 100;
        let cents = self.cents % 100;
        if cents == 0 {
            write!(f, "{dollars}/day")
        } else {
            write!(f, "{dollars}.{cents:02}/day")
        }
    }
}

impl Serialize for DayCap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DayCap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DayCapParseError {
    #[error(transparent)]
    Budget(#[from] crate::harness::budget::BudgetParseError),
    #[error("daily budget `{0}` must end in `/day`; use an amount such as `50/day`")]
    DayRequired(String),
    #[error("daily budget `{0}` is too large")]
    TooLarge(String),
}

/// A per-turn dollar cap stored as cents so machine config keeps exact
/// equality while reusing the public budget grammar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TurnCap {
    cents: u64,
}

impl TurnCap {
    pub fn as_usd(self) -> f64 {
        self.cents as f64 / 100.0
    }
}

impl FromStr for TurnCap {
    type Err = TurnCapParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let spec = raw
            .parse::<BudgetSpec>()
            .map_err(TurnCapParseError::Budget)?;
        if spec.window != BudgetWindow::Session {
            return Err(TurnCapParseError::PlainAmountRequired(
                raw.trim().to_owned(),
            ));
        }
        let cents = (spec.cap_usd * 100.0).round();
        if cents > u64::MAX as f64 {
            return Err(TurnCapParseError::TooLarge(raw.trim().to_owned()));
        }
        Ok(Self {
            cents: cents as u64,
        })
    }
}

impl fmt::Display for TurnCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollars = self.cents / 100;
        let cents = self.cents % 100;
        if cents == 0 {
            write!(f, "{dollars}")
        } else {
            write!(f, "{dollars}.{cents:02}")
        }
    }
}

impl Serialize for TurnCap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TurnCap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnCapParseError {
    #[error(transparent)]
    Budget(#[from] crate::harness::budget::BudgetParseError),
    #[error(
        "turn budget `{0}` must be a plain dollar amount; use an amount such as `3` or `$2.50`"
    )]
    PlainAmountRequired(String),
    #[error("turn budget `{0}` is too large")]
    TooLarge(String),
}

use super::FlipCompact;
use crate::store::message::AutoCompact;

const IDLE_COMPACT_DURATION_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// Automatic idle-compaction policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdleCompactMode {
    /// Leave idle team members untouched.
    Off,
    /// Compact before the provider prompt cache expires.
    #[default]
    On,
    /// Compact after an explicit idle span, regardless of provider cache facts.
    After(Duration),
}

impl fmt::Display for IdleCompactMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::On => f.write_str("on"),
            Self::After(duration) => {
                f.write_str(&crate::utils::time::format_duration_compact(*duration))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idle_compact takes off, on, or a duration such as 25m")]
pub struct IdleCompactParseError;

impl FromStr for IdleCompactMode {
    type Err = IdleCompactParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "off" => Ok(Self::Off),
            "on" => Ok(Self::On),
            _ => parse_duration_units(raw, IDLE_COMPACT_DURATION_UNITS)
                .map(Self::After)
                .map_err(|_| IdleCompactParseError),
        }
    }
}

impl Serialize for IdleCompactMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IdleCompactMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// How long an idle agent's prompt cache is held warm after its last real turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeepWarm {
    /// Let the cache expire on the provider's clock.
    #[default]
    Off,
    /// Ping before each cache expiry until this long after the last real turn ended.
    For(Duration),
}

impl fmt::Display for KeepWarm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::For(horizon) => {
                f.write_str(&crate::utils::time::format_duration_compact(*horizon))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("keep-warm takes off or a duration such as 2h")]
pub struct KeepWarmParseError;

impl FromStr for KeepWarm {
    type Err = KeepWarmParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw == "off" {
            return Ok(Self::Off);
        }
        parse_duration_units(raw, IDLE_COMPACT_DURATION_UNITS)
            .ok()
            .filter(|horizon| !horizon.is_zero())
            .map(Self::For)
            .ok_or(KeepWarmParseError)
    }
}

impl Serialize for KeepWarm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for KeepWarm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Harness behavior shared by immediate and parked message send paths.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HarnessConfig {
    /// Keep waiting agents' provider prompt caches warm.
    pub cache_keepalive: bool,
    /// Stop keepalive pings that would start this long after the agent's last request of its own; `None` (`off`) never stops them.
    #[serde(with = "cache_keepalive_max_serde")]
    pub cache_keepalive_max: Option<Duration>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        with = "prompt_cache_ttl_serde"
    )]
    prompt_cache_ttl: BTreeMap<String, Option<Duration>>,
    /// Default local-calendar-day cap for one room's whole agent fleet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<DayCap>,
    /// Default per-turn dollar cap for every agent in the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_budget: Option<TurnCap>,
    /// Native auto-compaction window for capable profiles without `auto-compact`; unset means 272k tokens, `off` passes no window.
    #[serde(with = "auto_compact_serde")]
    pub auto_compact: Option<u64>,
    /// Compact before messages and scheduled loop waits when context reaches this threshold; unset means 258,000 tokens, `off` disables it.
    #[serde(with = "smart_compact_serde")]
    pub smart_compact: Option<AutoCompact>,
    /// Free text appended to compact commands whose adapters accept it.
    /// Unset sends RimZ's brief for the agent's seat; a set value (an empty
    /// string sends the bare command) replaces the brief for every seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_instruction: Option<String>,
    /// Compact an idle team member before its provider prompt cache expires.
    #[serde(default)]
    pub idle_compact: IdleCompactMode,
    /// Compact a team member on hand-off: the role's `flip-compact` overrides this setting, then unset falls back to 120k for a Plan owner or 180k otherwise; `off` disables compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip_compact: Option<FlipCompact>,
    /// The shortest prompt-cache TTL keep-warm will hold; `None` (`off`) admits any known TTL.
    #[serde(with = "keep_warm_min_ttl_serde")]
    pub keep_warm_min_ttl: Option<Duration>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            cache_keepalive: true,
            cache_keepalive_max: Some(Duration::from_secs(6 * 3600)),
            prompt_cache_ttl: BTreeMap::new(),
            budget: None,
            turn_budget: None,
            auto_compact: Some(272_000),
            smart_compact: Some(AutoCompact::Tokens(258_000)),
            compact_instruction: None,
            idle_compact: IdleCompactMode::default(),
            flip_compact: None,
            keep_warm_min_ttl: Some(DEFAULT_KEEP_WARM_MIN_TTL),
        }
    }
}

impl HarnessConfig {
    /// Configured override, explicit off, or the provider definition's default.
    pub fn prompt_cache_ttl(&self, kind: &crate::ids::AgentKind) -> Option<Duration> {
        self.prompt_cache_ttl
            .get(kind.as_str())
            .copied()
            .unwrap_or_else(|| crate::agents::find_definition(kind.as_str())?.prompt_cache_ttl())
    }

    pub fn compact_instruction(&self, seat: CompactSeat) -> &str {
        self.compact_instruction.as_deref().unwrap_or(match seat {
            CompactSeat::Solo => DEFAULT_COMPACT_INSTRUCTION,
            CompactSeat::Team => TEAM_COMPACT_INSTRUCTION,
        })
    }
}

/// `off`, or a duration longer than [`PROMPT_CACHE_MARGIN`].
fn parse_off_or_duration(raw: &str) -> Option<Option<Duration>> {
    if raw == "off" {
        return Some(None);
    }
    parse_duration_units(raw, IDLE_COMPACT_DURATION_UNITS)
        .ok()
        .filter(|duration| *duration > PROMPT_CACHE_MARGIN)
        .map(Some)
}

fn format_off_or_duration(duration: Option<Duration>) -> String {
    duration.map_or_else(
        || "off".to_owned(),
        crate::utils::time::format_duration_compact,
    )
}

mod cache_keepalive_max_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_off_or_duration(&String::deserialize(deserializer)?).ok_or_else(|| {
            serde::de::Error::custom(
                "harness.cache_keepalive_max must be off or a duration longer than 1m, such as 6h",
            )
        })
    }

    pub fn serialize<S>(max: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_off_or_duration(*max))
    }
}

mod prompt_cache_ttl_serde {
    use super::*;

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Option<Duration>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        BTreeMap::<String, String>::deserialize(deserializer)?
            .into_iter()
            .map(|(kind, raw)| {
                let ttl = parse_entry(&kind, &raw).map_err(serde::de::Error::custom)?;
                Ok((kind, ttl))
            })
            .collect()
    }

    fn parse_entry(kind: &str, raw: &str) -> Result<Option<Duration>, String> {
        if !crate::agents::known_kinds().any(|known| known == kind) {
            return Err(format!(
                "harness.prompt_cache_ttl.{kind}: unknown provider kind; use a built-in or installed plugin kind"
            ));
        }
        parse_off_or_duration(raw).ok_or_else(|| {
            format!(
                "harness.prompt_cache_ttl.{kind} must be off or a duration longer than 1m, such as 5m"
            )
        })
    }

    pub fn serialize<S>(
        values: &BTreeMap<String, Option<Duration>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(|(kind, ttl)| (kind, format_off_or_duration(*ttl)))
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }
}

const DEFAULT_KEEP_WARM_MIN_TTL: Duration = Duration::from_secs(15 * 60);

mod keep_warm_min_ttl_serde {
    use super::*;

    pub(super) const FLOOR_ERROR: &str =
        "harness.keep_warm_min_ttl must be off or a duration such as 15m";

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.parse::<KeepWarm>() {
            Ok(KeepWarm::Off) => Ok(None),
            Ok(KeepWarm::For(floor)) => Ok(Some(floor)),
            Err(_) => Err(serde::de::Error::custom(FLOOR_ERROR)),
        }
    }

    pub fn serialize<S>(floor: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&floor.map_or(KeepWarm::Off, KeepWarm::For))
    }
}

mod auto_compact_serde {
    use super::*;

    const WINDOW_ERROR: &str =
        "harness.auto_compact must be off or a token count from 100k through 1M, such as 272k";

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom(WINDOW_ERROR))?;
        if raw == "off" {
            return Ok(None);
        }
        AutoCompact::parse_native_window(&raw)
            .map(Some)
            .map_err(|_| serde::de::Error::custom(WINDOW_ERROR))
    }

    pub fn serialize<S>(window: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match window {
            Some(tokens) if tokens.is_multiple_of(1_000) => {
                serializer.serialize_str(&format!("{}k", tokens / 1_000))
            }
            Some(tokens) => serializer.collect_str(tokens),
            None => serializer.serialize_str("off"),
        }
    }
}

mod smart_compact_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<AutoCompact>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if raw == "off" {
            return Ok(None);
        }
        AutoCompact::parse(&raw)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(
        smart_compact: &Option<AutoCompact>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match smart_compact {
            Some(AutoCompact::Percent(pct)) => serializer.serialize_str(&format!("{pct}%")),
            Some(AutoCompact::Tokens(tokens)) => serializer.serialize_str(&tokens.to_string()),
            None => serializer.serialize_str("off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cache_defaults_and_overrides() {
        let kind = crate::ids::AgentKind::new_unchecked;
        let defaults = HarnessConfig::default();
        assert!(defaults.cache_keepalive);
        assert_eq!(
            defaults.prompt_cache_ttl(&kind("claude")),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(
            defaults.prompt_cache_ttl(&kind("codex")),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(defaults.prompt_cache_ttl(&kind("amp")), None);
        let config: HarnessConfig = toml::from_str("cache_keepalive = false\n[prompt_cache_ttl]\nclaude = \"5m\"\ncodex = \"off\"\namp = \"10m\"").unwrap();
        assert!(!config.cache_keepalive);
        assert_eq!(
            config.prompt_cache_ttl(&kind("claude")),
            Some(Duration::from_secs(300))
        );
        assert_eq!(config.prompt_cache_ttl(&kind("codex")), None);
        assert_eq!(
            config.prompt_cache_ttl(&kind("amp")),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            toml::from_str::<HarnessConfig>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
    }

    #[test]
    fn prompt_cache_overrides_refuse_unknown_kinds_and_short_ttls() {
        for (kind, value) in [
            ("typo", "5m"),
            ("claude", "1m"),
            ("claude", "0s"),
            ("claude", "on"),
            ("claude", "nonsense"),
        ] {
            let raw = format!("[prompt_cache_ttl]\n{kind} = {value:?}");
            let err = toml::from_str::<HarnessConfig>(&raw)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("harness.prompt_cache_ttl.{kind}")),
                "{err}"
            );
        }
    }

    #[test]
    fn cache_keepalive_max_defaults_to_six_hours_and_takes_off_or_a_duration() {
        let six_hours = Some(Duration::from_secs(6 * 3600));
        assert_eq!(HarnessConfig::default().cache_keepalive_max, six_hours);
        let absent: HarnessConfig = toml::from_str("").unwrap();
        assert_eq!(absent.cache_keepalive_max, six_hours);
        for (raw, expected) in [("off", None), ("2h", Some(Duration::from_secs(7200)))] {
            let config: HarnessConfig =
                toml::from_str(&format!("cache_keepalive_max = {raw:?}")).unwrap();
            assert_eq!(config.cache_keepalive_max, expected);
            let rendered = toml::to_string(&config).unwrap();
            assert!(
                rendered.contains(&format!("cache_keepalive_max = {raw:?}")),
                "{rendered}"
            );
            assert_eq!(toml::from_str::<HarnessConfig>(&rendered).unwrap(), config);
        }
        for raw in ["30s", "1m", "on", "nonsense"] {
            let err = toml::from_str::<HarnessConfig>(&format!("cache_keepalive_max = {raw:?}"))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(
                    "harness.cache_keepalive_max must be off or a duration longer than 1m, such as 6h"
                ),
                "{raw}: {err}"
            );
        }
    }

    #[test]
    fn keep_warm_takes_off_or_a_positive_duration() {
        assert_eq!("off".parse::<KeepWarm>(), Ok(KeepWarm::Off));
        assert_eq!(
            "2h".parse::<KeepWarm>(),
            Ok(KeepWarm::For(Duration::from_secs(7200)))
        );
        assert_eq!(KeepWarm::For(Duration::from_secs(7200)).to_string(), "2h");
        for raw in ["true", "on", "0", "0s", "2x", ""] {
            assert_eq!(raw.parse::<KeepWarm>(), Err(KeepWarmParseError), "{raw}");
        }
    }

    #[test]
    fn keep_warm_floor_defaults_to_fifteen_minutes_and_takes_off() {
        assert_eq!(
            HarnessConfig::default().keep_warm_min_ttl,
            Some(Duration::from_secs(900))
        );
        for (raw, expected) in [("off", None), ("5m", Some(Duration::from_secs(300)))] {
            let config: HarnessConfig =
                toml::from_str(&format!("keep_warm_min_ttl = {raw:?}")).unwrap();
            assert_eq!(config.keep_warm_min_ttl, expected, "{raw}");
            assert_eq!(
                toml::from_str::<HarnessConfig>(&toml::to_string(&config).unwrap()).unwrap(),
                config
            );
        }
        let err = toml::from_str::<HarnessConfig>("keep_warm_min_ttl = \"5x\"")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("harness.keep_warm_min_ttl must be off or a duration such as 15m"),
            "{err}"
        );
    }

    #[test]
    fn smart_compact_deserializes_percent() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"70%\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Percent(70)));
    }

    #[test]
    fn compaction_defaults_apply_when_unset() {
        for config in [HarnessConfig::default(), toml::from_str("").unwrap()] {
            assert_eq!(config.auto_compact, Some(272_000));
            assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(258 * 1_000)));
        }
    }

    #[test]
    fn native_compaction_takes_off_or_a_window_and_round_trips() {
        for (raw, expected, rendered) in [
            ("off", None, "off"),
            ("100k", Some(100_000), "100k"),
            ("272k", Some(272_000), "272k"),
            ("272000", Some(272_000), "272k"),
            ("272001", Some(272_001), "272001"),
            ("1m", Some(1_000_000), "1000k"),
        ] {
            let parsed = toml::from_str::<HarnessConfig>(&format!("auto_compact = {raw:?}"));
            assert!(parsed.is_ok(), "{raw}: {parsed:?}");
            let config = parsed.unwrap();
            assert_eq!(config.auto_compact, expected);
            let encoded = toml::to_string(&config).unwrap();
            assert!(
                encoded.contains(&format!("auto_compact = {rendered:?}")),
                "{encoded}"
            );
            assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
        }
    }

    #[test]
    fn native_compaction_rejects_invalid_windows_with_the_key() {
        for raw in ["99k", "1000001", "70%", "abc"] {
            let error = toml::from_str::<HarnessConfig>(&format!("auto_compact = {raw:?}"))
                .unwrap_err()
                .to_string();
            assert!(error.contains("harness.auto_compact must be off or a token count from 100k through 1M, such as 272k"), "{raw}: {error}");
        }
    }

    #[test]
    fn smart_compaction_takes_off_and_round_trips() {
        let parsed = toml::from_str::<HarnessConfig>("smart_compact = \"off\"");
        assert!(parsed.is_ok(), "{parsed:?}");
        let config = parsed.unwrap();
        assert_eq!(config.smart_compact, None);
        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("smart_compact = \"off\""), "{encoded}");
        assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
    }

    #[test]
    fn day_cap_requires_day_and_round_trips() {
        let config: HarnessConfig =
            toml::from_str("budget = \"50.25/day\"").expect("parse day cap");
        assert_eq!(config.budget.map(DayCap::as_usd), Some(50.25));
        let rendered = toml::to_string(&config).expect("serialize harness");
        assert!(rendered.contains("budget = \"50.25/day\""), "{rendered}");
        assert!(
            toml::from_str::<HarnessConfig>("budget = \"50\"")
                .unwrap_err()
                .to_string()
                .contains("must end in `/day`")
        );
    }

    #[test]
    fn turn_cap_requires_plain_amount_and_round_trips() {
        let config: HarnessConfig =
            toml::from_str("turn_budget = \"$2.50\"").expect("parse turn cap");
        assert_eq!(config.turn_budget.map(TurnCap::as_usd), Some(2.5));
        let rendered = toml::to_string(&config).expect("serialize harness");
        assert!(rendered.contains("turn_budget = \"2.50\""), "{rendered}");
        assert!(
            toml::from_str::<HarnessConfig>("turn_budget = \"3/day\"")
                .unwrap_err()
                .to_string()
                .contains("must be a plain dollar amount")
        );
    }

    #[test]
    fn smart_compact_deserializes_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"120000\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(120_000)));
    }

    #[test]
    fn smart_compact_deserializes_suffixed_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"180k\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(180_000)));
    }

    #[test]
    fn smart_compact_round_trips() {
        let config = HarnessConfig {
            smart_compact: Some(AutoCompact::Percent(70)),
            ..Default::default()
        };

        let toml = toml::to_string(&config).expect("serialize harness config");
        let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");

        assert_eq!(back, config);
    }

    #[test]
    fn flip_compact_round_trips() {
        for (raw, expected) in [
            ("off", FlipCompact::Off),
            ("180k", FlipCompact::Threshold(AutoCompact::Tokens(180_000))),
            ("70%", FlipCompact::Threshold(AutoCompact::Percent(70))),
        ] {
            let config: HarnessConfig = toml::from_str(&format!("flip_compact = {raw:?}"))
                .expect("valid flip compaction policy");
            assert_eq!(config.flip_compact, Some(expected));
            let encoded = toml::to_string(&config).unwrap();
            assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
        }
        assert!(toml::from_str::<HarnessConfig>("flip_compact = \"OFF\"").is_err());
    }

    #[test]
    fn compact_instruction_defaults_to_the_brief_for_the_seat() {
        let config: HarnessConfig = toml::from_str("").expect("parse harness config");

        assert_eq!(
            config.compact_instruction(CompactSeat::Solo),
            DEFAULT_COMPACT_INSTRUCTION
        );
        assert_eq!(
            config.compact_instruction(CompactSeat::Team),
            TEAM_COMPACT_INSTRUCTION
        );
        assert!(
            !toml::to_string(&config)
                .expect("serialize harness config")
                .contains("compact_instruction")
        );
    }

    #[test]
    fn compact_instruction_empty_value_round_trips() {
        let config: HarnessConfig =
            toml::from_str("compact_instruction = \"\"").expect("parse harness config");

        assert_eq!(config.compact_instruction, Some(String::new()));
        assert_eq!(config.compact_instruction(CompactSeat::Solo), "");
        assert_eq!(config.compact_instruction(CompactSeat::Team), "");
        let rendered = toml::to_string(&config).expect("serialize harness config");
        assert!(
            rendered.contains("compact_instruction = \"\""),
            "{rendered}"
        );
        assert_eq!(
            toml::from_str::<HarnessConfig>(&rendered).expect("parse rendered harness config"),
            config
        );
    }

    #[test]
    fn idle_compact_defaults_on() {
        let config: HarnessConfig = toml::from_str("").expect("parse harness config");

        assert_eq!(serde_json::to_value(config).unwrap()["idle_compact"], "on");
    }

    #[test]
    fn idle_compact_modes_and_duration_round_trip() {
        for raw in ["off", "on", "25m", "2h"] {
            let config: HarnessConfig =
                toml::from_str(&format!("idle_compact = \"{raw}\"")).unwrap();
            let toml = toml::to_string(&config).expect("serialize harness config");
            assert!(toml.contains(&format!("idle_compact = \"{raw}\"")));
            let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");
            assert_eq!(back, config);
        }
    }

    #[test]
    fn idle_compact_rejects_bad_values() {
        for raw in ["auto", "always", "soon", "25", "25w"] {
            let err =
                toml::from_str::<HarnessConfig>(&format!("idle_compact = \"{raw}\"")).unwrap_err();
            assert!(
                err.to_string()
                    .contains("idle_compact takes off, on, or a duration such as 25m")
            );
        }
    }
}
