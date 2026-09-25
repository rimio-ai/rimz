use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agents::BudgetWindow;
use crate::harness::budget::BudgetSpec;
use crate::utils::time::{DurationUnit, parse_duration_units};

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

/// Harness behavior shared by immediate and parked message send paths.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HarnessConfig {
    /// Default local-calendar-day cap for one room's whole agent fleet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<DayCap>,
    /// Default per-turn dollar cap for every agent in the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_budget: Option<TurnCap>,
    /// Compact before messages and scheduled loop waits when the agent's
    /// context window has reached this threshold. Unset keeps compaction opt-in.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "smart_compact_serde"
    )]
    pub smart_compact: Option<AutoCompact>,
    /// Free text appended to compact commands whose adapters accept it.
    /// Unset sends RimZ's brief for the agent's seat; a set value (an empty
    /// string sends the bare command) replaces the brief for every seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_instruction: Option<String>,
    /// Compact an idle team member before its provider prompt cache expires.
    #[serde(default)]
    pub idle_compact: IdleCompactMode,
    /// Compact a team member at the flip that hands its own stage to another role, once its context is at least this full. Unset keeps flips uncompacted; a role's `flip-compact` overrides it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "smart_compact_serde"
    )]
    pub flip_compact: Option<AutoCompact>,
}

impl HarnessConfig {
    pub fn compact_instruction(&self, seat: CompactSeat) -> &str {
        self.compact_instruction.as_deref().unwrap_or(match seat {
            CompactSeat::Solo => DEFAULT_COMPACT_INSTRUCTION,
            CompactSeat::Team => TEAM_COMPACT_INSTRUCTION,
        })
    }
}

mod smart_compact_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<AutoCompact>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|raw| AutoCompact::parse(&raw).map_err(serde::de::Error::custom))
            .transpose()
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
            None => serializer.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_compact_deserializes_percent() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"70%\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Percent(70)));
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
