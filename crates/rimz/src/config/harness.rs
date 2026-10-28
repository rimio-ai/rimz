use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agents::AgentState;
use crate::agents::BudgetWindow;
use crate::agents::context::{CacheWriteRetention, SignIn};
use crate::harness::budget::BudgetSpec;
use crate::utils::time::{DurationUnit, parse_duration_units};

const DEFAULT_PROMPT_CACHE_MARGIN: Duration = Duration::from_secs(60);

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

const DURATION_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// The context size at which an idle seat is compacted before its prompt
/// cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleCompact {
    /// Leave the seat's context alone however large it grows.
    Off,
    /// Compact once at least this many tokens occupy the context.
    Tokens(u64),
}

impl Default for IdleCompact {
    fn default() -> Self {
        Self::Tokens(100_000)
    }
}

impl fmt::Display for IdleCompact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Tokens(tokens) if tokens.is_multiple_of(1_000) => {
                write!(f, "{}k", tokens / 1_000)
            }
            Self::Tokens(tokens) => write!(f, "{tokens}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idle_compact takes off or a positive token floor in digits or k, such as 100k or 1000k")]
pub struct IdleCompactParseError;

impl FromStr for IdleCompact {
    type Err = IdleCompactParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw == "off" {
            return Ok(Self::Off);
        }
        // `m` reads as a million there; refusing it keeps a legacy minute
        // duration such as `59m` from loading as a floor no context reaches.
        if raw.ends_with(['m', 'M']) {
            return Err(IdleCompactParseError);
        }
        match AutoCompact::parse(raw) {
            Ok(AutoCompact::Tokens(tokens)) if tokens > 0 => Ok(Self::Tokens(tokens)),
            _ => Err(IdleCompactParseError),
        }
    }
}

impl Serialize for IdleCompact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IdleCompact {
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
        parse_duration_units(raw, DURATION_UNITS)
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
        struct Setting;
        impl<'de> serde::de::Visitor<'de> for Setting {
            type Value = KeepWarm;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("off or a duration such as 2h")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(E::custom)
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                if value {
                    Err(E::custom(KeepWarmParseError))
                } else {
                    Ok(KeepWarm::Off)
                }
            }
        }
        deserializer.deserialize_any(Setting)
    }
}

/// A declared keep-warm shorthand or block, retaining shorthand `off` so solo profiles can distinguish it from an explicit `while`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum KeepWarmPolicy {
    Shorthand(KeepWarm),
    Block(KeepWarmFields),
}

/// Independent keep-warm fields; resolved output uses `None` for cancelled or unset fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepWarmFields {
    #[serde(default)]
    pub max: Option<KeepWarm>,
    #[serde(default, rename = "while", deserialize_with = "keep_warm_while")]
    pub while_clause: Option<String>,
}

fn keep_warm_while<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Setting {
        Text(String),
        Boolean(bool),
    }
    match Setting::deserialize(deserializer)? {
        Setting::Text(value) => Ok(Some(value)),
        Setting::Boolean(false) => Ok(Some("off".into())),
        Setting::Boolean(true) => Err(serde::de::Error::custom(
            "keep-warm while takes a condition or off",
        )),
    }
}

impl From<KeepWarm> for KeepWarmPolicy {
    fn from(value: KeepWarm) -> Self {
        Self::Shorthand(value)
    }
}

impl KeepWarmPolicy {
    pub(crate) fn fields(&self) -> KeepWarmFields {
        match self {
            Self::Shorthand(max) => KeepWarmFields {
                max: Some(*max),
                while_clause: (*max == KeepWarm::Off).then(|| "off".into()),
            },
            Self::Block(fields) => fields.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for KeepWarmPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Policy;
        impl<'de> serde::de::Visitor<'de> for Policy {
            type Value = KeepWarmPolicy;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("keep-warm takes off or a duration such as 2h, or a max/while block")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse::<KeepWarm>().map(Into::into).map_err(E::custom)
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                if value {
                    Err(E::custom(KeepWarmParseError))
                } else {
                    Ok(KeepWarm::Off.into())
                }
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                KeepWarmFields::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(KeepWarmPolicy::Block)
            }
        }
        deserializer.deserialize_any(Policy)
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
    prompt_cache_ttl: BTreeMap<String, LifetimeSetting>,
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
    /// Compact an idle seat whose context holds at least this many tokens
    /// before its resolved prompt cache expires; a profile or team role
    /// overrides it, and `off` disables it.
    #[serde(default)]
    pub idle_compact: IdleCompact,
    /// Compact a team member on hand-off: the role's `flip-compact` overrides this setting, then unset falls back to 120k for a Plan owner or 180k otherwise; `off` disables compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip_compact: Option<FlipCompact>,
    /// The shortest lifetime admitted for any cache ping; `None` (`off`) removes the floor.
    #[serde(with = "keep_warm_min_ttl_serde")]
    pub keep_warm_min_ttl: Option<Duration>,
    /// How long before a prompt cache expires cache pings and idle
    /// compaction fire. Request anchoring excludes final generation, so this
    /// covers the producer tick, helper spawn, and submission.
    #[serde(with = "prompt_cache_margin_serde")]
    pub prompt_cache_margin: Duration,
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
            idle_compact: IdleCompact::default(),
            flip_compact: None,
            keep_warm_min_ttl: Some(DEFAULT_KEEP_WARM_MIN_TTL),
            prompt_cache_margin: DEFAULT_PROMPT_CACHE_MARGIN,
        }
    }
}

impl HarnessConfig {
    /// Read the smart-compaction default from the typed core file alone.
    /// Missing or broken files use the full lenient load's defaults and stable-file guard.
    pub(crate) fn load_smart_compact_lenient() -> Option<AutoCompact> {
        Self::load_smart_compact_lenient_from(&super::MachineConfig::config_path())
    }

    fn load_smart_compact_lenient_from(path: &std::path::Path) -> Option<AutoCompact> {
        super::read_core_lenient(path).harness.smart_compact
    }

    /// Settings, provider defaults, then the fallback, without live cache-write evidence.
    pub fn configured_lifetime(
        &self,
        kind: &crate::ids::AgentKind,
        sign_in: Option<SignIn>,
    ) -> ConfiguredLifetime {
        let setting = self
            .prompt_cache_ttl
            .get(kind.as_str())
            .and_then(|setting| match setting {
                LifetimeSetting::Plain(ttl) => Some(*ttl),
                LifetimeSetting::Split(values) => values.get(&sign_in?).copied(),
            });
        if let Some(ttl) = setting {
            return ConfiguredLifetime {
                ttl,
                source: LifetimeSource::Setting,
            };
        }
        if let Some(ttl) = crate::agents::find_definition(kind.as_str())
            .and_then(|adapter| adapter.prompt_cache_ttl(sign_in))
        {
            return ConfiguredLifetime {
                ttl: Some(ttl),
                source: LifetimeSource::Default,
            };
        }
        ConfiguredLifetime {
            ttl: self.fallback_lifetime(),
            source: LifetimeSource::Fallback,
        }
    }

    /// The newest split cache write wins, even over `off`; unsplit writes use static policy.
    pub fn prompt_cache_lifetime(&self, agent: &AgentState) -> Option<Duration> {
        match agent
            .context
            .as_ref()
            .and_then(|context| context.last_cache_write)
        {
            Some(CacheWriteRetention::FiveMinutes) => Some(Duration::from_secs(300)),
            Some(CacheWriteRetention::OneHour) => Some(Duration::from_secs(3600)),
            Some(CacheWriteRetention::Unsplit) | None => {
                self.configured_lifetime(&agent.kind, agent.sign_in).ttl
            }
        }
    }

    /// Every ping needs room for the margin and admission by the optional lifetime floor.
    pub fn ping_lifetime(&self, agent: &AgentState) -> Option<Duration> {
        self.prompt_cache_lifetime(agent).filter(|ttl| {
            *ttl > self.prompt_cache_margin
                && self.keep_warm_min_ttl.is_none_or(|floor| *ttl >= floor)
        })
    }

    /// The configured fallback lifetime, or one hour when unset.
    pub fn fallback_lifetime(&self) -> Option<Duration> {
        match self.prompt_cache_ttl.get("fallback") {
            Some(LifetimeSetting::Plain(ttl)) => *ttl,
            _ => Some(Duration::from_secs(3600)),
        }
    }

    pub fn compact_instruction(&self, seat: CompactSeat) -> &str {
        self.compact_instruction.as_deref().unwrap_or(match seat {
            CompactSeat::Solo => DEFAULT_COMPACT_INSTRUCTION,
            CompactSeat::Team => TEAM_COMPACT_INSTRUCTION,
        })
    }

    /// Static lifetimes must leave room for the margin. Runtime writes may override them.
    fn check_prompt_cache_margin(&self) -> Result<(), String> {
        let margin = self.prompt_cache_margin;
        let shown = crate::utils::time::format_duration_compact;
        if let Some(max) = self.cache_keepalive_max.filter(|max| *max <= margin) {
            return Err(format!(
                "harness.prompt_cache_margin {} must be shorter than harness.cache_keepalive_max {}; lower the margin or raise cache_keepalive_max",
                shown(margin),
                shown(max)
            ));
        }
        let fallback = self
            .fallback_lifetime()
            .map(|ttl| ("fallback".to_owned(), ttl));
        let lifetimes = crate::agents::known_kinds().flat_map(|kind| {
            [None, Some(SignIn::Subscription), Some(SignIn::ApiKey)]
                .into_iter()
                .filter_map(move |sign_in| {
                    let label = match sign_in {
                        None => kind.to_owned(),
                        Some(sign_in) => format!("{kind} {}", sign_in.as_str()),
                    };
                    self.configured_lifetime(&crate::ids::AgentKind::new_unchecked(kind), sign_in)
                        .ttl
                        .map(|ttl| (label, ttl))
                })
        });
        for (label, ttl) in fallback.into_iter().chain(lifetimes) {
            if ttl <= margin {
                return Err(format!(
                    "harness.prompt_cache_margin {} must be shorter than the {label} prompt-cache TTL {}; lower the margin or change harness.prompt_cache_ttl (settings and defaults only; an agent's own cache writes can change its lifetime)",
                    shown(margin),
                    shown(ttl)
                ));
            }
        }
        Ok(())
    }
}

/// The static policy layer that supplied a configured prompt-cache lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifetimeSource {
    Setting,
    Default,
    Fallback,
}

/// A prompt-cache lifetime resolved without live cache-write evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfiguredLifetime {
    /// The resolved duration, or `None` when the static policy disables it.
    pub ttl: Option<Duration>,
    /// The setting, provider default, or fallback that supplied the lifetime.
    pub source: LifetimeSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LifetimeSetting {
    Plain(Option<Duration>),
    Split(BTreeMap<SignIn, Option<Duration>>),
}

/// Deserializes `[harness]` and refuses a prompt-cache margin its TTLs or
/// keepalive cap cannot fit, so the refusal reads like any other bad value.
pub(super) fn deserialize_checked<'de, D>(deserializer: D) -> Result<HarnessConfig, D::Error>
where
    D: Deserializer<'de>,
{
    let harness = HarnessConfig::deserialize(deserializer)?;
    harness
        .check_prompt_cache_margin()
        .map_err(serde::de::Error::custom)?;
    Ok(harness)
}

/// `off`, or a non-zero duration.
fn parse_off_or_duration(raw: &str) -> Option<Option<Duration>> {
    if raw == "off" {
        return Some(None);
    }
    parse_nonzero_duration(raw).map(Some)
}

fn parse_nonzero_duration(raw: &str) -> Option<Duration> {
    parse_duration_units(raw, DURATION_UNITS)
        .ok()
        .filter(|duration| !duration.is_zero())
}

fn format_off_or_duration(duration: Option<Duration>) -> String {
    duration.map_or_else(
        || "off".to_owned(),
        crate::utils::time::format_duration_compact,
    )
}

mod prompt_cache_margin_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_nonzero_duration(&String::deserialize(deserializer)?).ok_or_else(|| {
            serde::de::Error::custom(
                "harness.prompt_cache_margin must be a non-zero duration such as 1m",
            )
        })
    }

    pub fn serialize<S>(margin: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&crate::utils::time::format_duration_compact(*margin))
    }
}

mod cache_keepalive_max_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_off_or_duration(&String::deserialize(deserializer)?).ok_or_else(|| {
            serde::de::Error::custom(
                "harness.cache_keepalive_max must be off or a non-zero duration such as 6h",
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
    ) -> Result<BTreeMap<String, LifetimeSetting>, D::Error>
    where
        D: Deserializer<'de>,
    {
        BTreeMap::<String, toml::Value>::deserialize(deserializer)?
            .into_iter()
            .map(|(kind, raw)| {
                let ttl = parse_entry(&kind, raw).map_err(serde::de::Error::custom)?;
                Ok((kind, ttl))
            })
            .collect()
    }

    fn parse_entry(kind: &str, raw: toml::Value) -> Result<LifetimeSetting, String> {
        if kind != "fallback" && !crate::agents::known_kinds().any(|known| known == kind) {
            return Err(format!(
                "harness.prompt_cache_ttl.{kind}: unknown provider kind; use a built-in or installed plugin kind"
            ));
        }
        if kind != "fallback"
            && let toml::Value::Table(values) = raw
        {
            return values
                .into_iter()
                .map(|(sign_in, raw)| parse_split_entry(kind, sign_in, raw))
                .collect::<Result<_, _>>()
                .map(LifetimeSetting::Split);
        }
        parse_ttl(kind, &raw).map(LifetimeSetting::Plain)
    }

    fn parse_split_entry(
        kind: &str,
        sign_in: String,
        raw: toml::Value,
    ) -> Result<(SignIn, Option<Duration>), String> {
        let sign_in = [SignIn::Subscription, SignIn::ApiKey]
            .into_iter()
            .find(|mode| mode.as_str() == sign_in)
            .ok_or_else(|| {
                format!(
                    "harness.prompt_cache_ttl.{kind}.{sign_in}: unknown sign-in; use subscription or api-key"
                )
            })?;
        let ttl = parse_ttl(&format!("{kind}.{}", sign_in.as_str()), &raw)?;
        Ok((sign_in, ttl))
    }

    fn parse_ttl(key: &str, raw: &toml::Value) -> Result<Option<Duration>, String> {
        raw.as_str().and_then(parse_off_or_duration).ok_or_else(|| {
            format!("harness.prompt_cache_ttl.{key} must be off or a non-zero duration such as 5m")
        })
    }

    pub fn serialize<S>(
        values: &BTreeMap<String, LifetimeSetting>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(|(kind, setting)| {
                let value = match setting {
                    LifetimeSetting::Plain(ttl) => {
                        toml::Value::String(format_off_or_duration(*ttl))
                    }
                    LifetimeSetting::Split(values) => toml::Value::Table(
                        values
                            .iter()
                            .map(|(sign_in, ttl)| {
                                (
                                    sign_in.as_str().to_owned(),
                                    toml::Value::String(format_off_or_duration(*ttl)),
                                )
                            })
                            .collect(),
                    ),
                };
                (kind, value)
            })
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
mod tests;
