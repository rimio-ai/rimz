//! The agent selector: one flat row per addressable agent, the field table
//! naming its columns, the `--filter` query grammar over those fields, and the
//! default set (live top-level agents) a query steps aside from.
//!
//! The row is the one source of the field vocabulary: its serialized keys, the
//! filter's field names, `--filter help`, and completion all read
//! [`Field::ALL`].

use jiff::Timestamp;
use serde::Serialize;

use super::{
    AddressContext, LaneSelector, Roster, agent_in_lane, lane_label, launched_parent,
    parse_selector, resolve_many,
};
use crate::agents::{AgentState, AgentStatus, ParkDemotion, TurnErrorClass, TurnPhase};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::{
    AgentCard, SidebarRow, SidebarSnapshot, SidebarSubAgent, agent_idle_since, card_status,
};
use crate::utils::filter::{self, Op, TermError, Word, glob};
use crate::utils::suggest::nearest_name;
use crate::utils::time::parse_compact_duration;

/// One column of the flat agent row, by the name the JSON key and filter share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Handle,
    Profile,
    Kind,
    Account,
    Model,
    Lane,
    Team,
    Role,
    Parent,
    Status,
    Error,
    Live,
    Age,
    Ctx,
    Tokens,
    Cost,
}

impl Field {
    pub const ALL: [Self; 17] = [
        Self::Name,
        Self::Handle,
        Self::Profile,
        Self::Kind,
        Self::Account,
        Self::Model,
        Self::Lane,
        Self::Team,
        Self::Role,
        Self::Parent,
        Self::Status,
        Self::Error,
        Self::Live,
        Self::Age,
        Self::Ctx,
        Self::Tokens,
        Self::Cost,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Handle => "handle",
            Self::Profile => "profile",
            Self::Kind => "kind",
            Self::Account => "account",
            Self::Model => "model",
            Self::Lane => "lane",
            Self::Team => "team",
            Self::Role => "role",
            Self::Parent => "parent",
            Self::Status => "status",
            Self::Error => "error",
            Self::Live => "live",
            Self::Age => "age",
            Self::Ctx => "ctx",
            Self::Tokens => "tokens",
            Self::Cost => "cost",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.name() == name)
    }

    pub const fn kind(self) -> FieldKind {
        match self {
            Self::Name
            | Self::Handle
            | Self::Profile
            | Self::Kind
            | Self::Account
            | Self::Model
            | Self::Role => FieldKind::Text,
            Self::Lane | Self::Team | Self::Parent => FieldKind::Address,
            Self::Status | Self::Error | Self::Live => FieldKind::Enum,
            Self::Age => FieldKind::Duration,
            Self::Ctx => FieldKind::Percent,
            Self::Tokens => FieldKind::Size,
            Self::Cost => FieldKind::Dollars,
        }
    }

    /// The words an enum field accepts; empty for every other kind.
    pub fn values(self) -> Vec<&'static str> {
        match self {
            Self::Status => STATUSES.iter().map(|status| status.as_str()).collect(),
            Self::Error => ERRORS
                .iter()
                .map(|error| error.as_str())
                .chain(["none"])
                .collect(),
            Self::Live => vec!["yes", "no"],
            _ => Vec::new(),
        }
    }
}

/// How a field's values are written and compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Address,
    Enum,
    Duration,
    Percent,
    Size,
    Dollars,
}

impl FieldKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Address => "address",
            Self::Enum => "enum",
            Self::Duration => "duration",
            Self::Percent => "percent",
            Self::Size => "size",
            Self::Dollars => "dollars",
        }
    }

    /// How a value of this kind is written, for help and completion.
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Text => "words, * wildcard",
            Self::Address => "@handle, #lane, * wildcard",
            Self::Enum => "",
            Self::Duration => "30s, 5m, 1h, 2d",
            Self::Percent => "80%",
            Self::Size => "500, 100k, 2M",
            Self::Dollars => "$1.50",
        }
    }

    const fn ordered(self) -> bool {
        matches!(
            self,
            Self::Duration | Self::Percent | Self::Size | Self::Dollars
        )
    }
}

/// Every displayed status the `status` field accepts, in [`status_slot`] order.
const STATUSES: [AgentStatus; 7] = [
    AgentStatus::Running,
    AgentStatus::Waiting,
    AgentStatus::Idle,
    AgentStatus::Success,
    AgentStatus::Failed,
    AgentStatus::Paused,
    AgentStatus::Sleeping,
];

/// A new `AgentStatus` variant fails to compile here until `STATUSES` names it.
const fn status_slot(status: AgentStatus) -> usize {
    match status {
        AgentStatus::Running => 0,
        AgentStatus::Waiting => 1,
        AgentStatus::Idle => 2,
        AgentStatus::Success => 3,
        AgentStatus::Failed => 4,
        AgentStatus::Paused => 5,
        AgentStatus::Sleeping => 6,
    }
}

const _: () = {
    let mut slot = 0;
    while slot < STATUSES.len() {
        assert!(status_slot(STATUSES[slot]) == slot);
        slot += 1;
    }
};

/// The effective turn-error class in filter spelling, `paused_` dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ErrorClass {
    RateLimit,
    SpendLimit,
    Overloaded,
    NotEntitled,
    Failed,
    Unknown,
}

const ERRORS: [ErrorClass; 6] = [
    ErrorClass::RateLimit,
    ErrorClass::SpendLimit,
    ErrorClass::Overloaded,
    ErrorClass::NotEntitled,
    ErrorClass::Failed,
    ErrorClass::Unknown,
];

impl ErrorClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::SpendLimit => "spend-limit",
            Self::Overloaded => "overloaded",
            Self::NotEntitled => "not-entitled",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// A new `TurnErrorClass` variant fails to compile here until the filter names it.
impl From<TurnErrorClass> for ErrorClass {
    fn from(class: TurnErrorClass) -> Self {
        match class {
            TurnErrorClass::PausedRateLimit => Self::RateLimit,
            TurnErrorClass::PausedSpendLimit => Self::SpendLimit,
            TurnErrorClass::PausedOverloaded => Self::Overloaded,
            TurnErrorClass::NotEntitled => Self::NotEntitled,
            TurnErrorClass::Failed => Self::Failed,
            TurnErrorClass::Unknown => Self::Unknown,
        }
    }
}

/// One addressable agent as `agents list` prints and filters it. A missing
/// value serializes as `null` and is the filter's `none`.
#[derive(Clone, Debug, Serialize)]
pub struct AgentRow<'a> {
    #[serde(skip)]
    agent: &'a AgentState,
    #[serde(skip)]
    parent_agent: Option<&'a AgentState>,
    #[serde(skip)]
    source: &'a SidebarSnapshot,
    name: Option<String>,
    handle: String,
    profile: Option<String>,
    kind: AgentKind,
    account: String,
    model: Option<String>,
    lane: Option<String>,
    team: Option<String>,
    role: Option<String>,
    parent: Option<String>,
    status: AgentStatus,
    #[serde(skip)]
    phase: TurnPhase,
    error: Option<ErrorClass>,
    live: bool,
    age: u64,
    ctx: Option<u8>,
    tokens: Option<u64>,
    cost: Option<f64>,
}

impl<'a> AgentRow<'a> {
    /// The row for one agent of `source`, addressed and judged live as
    /// [`live_rows`] does.
    pub fn of(
        agent: &'a AgentState,
        source: &'a SidebarSnapshot,
        demotion: &ParkDemotion,
        reader: &AddressContext,
        now: Timestamp,
    ) -> Self {
        Self::live_in(agent, &Roster::of(source), demotion, reader, now)
    }

    fn live_in(
        agent: &'a AgentState,
        roster: &Roster<'a>,
        demotion: &ParkDemotion,
        reader: &AddressContext,
        now: Timestamp,
    ) -> Self {
        let live = agent.ended_at.is_none() && pane_bound(roster.snapshot, agent);
        let handle = roster.paste_address(agent, reader);
        Self::new(agent, roster, handle, live, demotion, reader, now)
    }

    pub fn agent(&self) -> &'a AgentState {
        self.agent
    }

    /// The snapshot the row was projected from: the live room, or Audit for a
    /// closed session.
    pub fn source(&self) -> &'a SidebarSnapshot {
        self.source
    }

    /// The displayed status and phase, as `card_status` projects them.
    pub fn card(&self) -> (AgentStatus, TurnPhase) {
        (self.status, self.phase)
    }

    /// The shortest address that resolves back to this one agent.
    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// The launched parent's address, as the `parent` column prints it.
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    fn new(
        agent: &'a AgentState,
        roster: &Roster<'a>,
        handle: String,
        live: bool,
        demotion: &ParkDemotion,
        reader: &AddressContext,
        now: Timestamp,
    ) -> Self {
        let source = roster.snapshot;
        let card_row = card_row(source, agent);
        let (status, phase) =
            card_status(agent, card_row, child_card(source, agent), demotion, now);
        let parent_agent = launched_parent(source.agents.iter(), agent);
        let idle = now.duration_since(agent_idle_since(agent, card_row));
        Self {
            agent,
            parent_agent,
            source,
            name: agent.name.clone(),
            handle,
            profile: agent.profile.clone(),
            kind: agent.kind.clone(),
            account: agent.login.clone().unwrap_or_default().to_string(),
            model: agent
                .context
                .as_ref()
                .and_then(|context| context.model_id.clone())
                .or_else(|| agent.model.clone()),
            lane: lane_label(agent),
            team: agent.team.clone(),
            role: agent.role.clone(),
            parent: parent_agent.map(|parent| roster.paste_address(parent, reader)),
            status,
            phase,
            error: agent
                .displayed_turn_error(now)
                .map(|(class, _)| class.into()),
            live,
            age: u64::try_from(idle.as_secs()).unwrap_or(0),
            ctx: agent
                .context_fill_pct()
                .map(|pct| pct.round().clamp(0.0, 100.0) as u8),
            tokens: agent.context_used_tokens(),
            cost: card_row
                .and_then(SidebarRow::as_agent)
                .and_then(AgentCard::cost_usd),
        }
    }

    /// Whether the agent is unended and bound to a pane.
    pub fn live(&self) -> bool {
        self.live
    }

    fn missing(&self, field: Field) -> bool {
        match field {
            Field::Handle
            | Field::Kind
            | Field::Account
            | Field::Status
            | Field::Live
            | Field::Age => false,
            Field::Parent => self.agent.parent_agent_id.is_none(),
            Field::Error => self.error.is_none(),
            _ => self.text(field).is_none() && self.number(field).is_none(),
        }
    }

    fn text(&self, field: Field) -> Option<&str> {
        match field {
            Field::Name => self.name.as_deref(),
            Field::Handle => Some(&self.handle),
            Field::Profile => self.profile.as_deref(),
            Field::Kind => Some(self.kind.as_str()),
            Field::Account => Some(&self.account),
            Field::Model => self.model.as_deref(),
            Field::Lane => self.lane.as_deref(),
            Field::Team => self.team.as_deref(),
            Field::Role => self.role.as_deref(),
            Field::Parent => self.parent.as_deref(),
            _ => None,
        }
    }

    fn number(&self, field: Field) -> Option<f64> {
        match field {
            Field::Age => Some(self.age as f64),
            Field::Ctx => self.ctx.map(f64::from),
            Field::Tokens => self.tokens.map(|tokens| tokens as f64),
            Field::Cost => self.cost,
            _ => None,
        }
    }
}

/// The agent's own sidebar card row; a launched child has none and nests
/// under its parent's.
pub fn card_row<'s>(snapshot: &'s SidebarSnapshot, agent: &AgentState) -> Option<&'s SidebarRow> {
    snapshot
        .rows()
        .find(|row| row.is_agent() && row.id == agent.agent_id.as_str())
}

/// The nested child card a child displays through, under the parent its launch
/// stamp names (a launch id or a prior one resolves to the parent's row).
pub fn child_card<'s>(
    snapshot: &'s SidebarSnapshot,
    agent: &AgentState,
) -> Option<&'s SidebarSubAgent> {
    let parent_id = launched_parent(snapshot.agents.iter(), agent)
        .map(|parent| &parent.agent_id)
        .or(agent.parent_agent_id.as_ref())?;
    snapshot
        .rows()
        .find(|row| row.is_agent() && row.id == parent_id.as_str())?
        .sub_agents()
        .iter()
        .find(|child| child.id == agent.agent_id.as_str())
}

/// Rows for every addressable agent the live snapshot carries: pane-bound
/// roots and launched children, ended children included.
pub fn live_rows<'a>(
    snapshot: &'a SidebarSnapshot,
    demotion: &ParkDemotion,
    reader: &AddressContext,
    now: Timestamp,
) -> Vec<AgentRow<'a>> {
    let roster = Roster::of(snapshot);
    roster
        .candidates
        .iter()
        .copied()
        .filter(|agent| agent.is_launched_child() || pane_bound(snapshot, agent))
        .filter(|agent| agent.parent_agent_id.is_none() || agent.is_launched_child())
        .map(|agent| AgentRow::live_in(agent, &roster, demotion, reader, now))
        .collect()
}

/// Rows for the Audit projection's closed sessions: ended, or gone from the
/// live snapshot, and not already `listed`. Each is addressed so that live
/// resolution misses it and Audit resolution finds it where such an address
/// exists; otherwise it keeps its short Audit address.
pub fn closed_rows<'a>(
    audit: &'a SidebarSnapshot,
    live: &SidebarSnapshot,
    listed: &[AgentRow<'_>],
    demotion: &ParkDemotion,
    reader: &AddressContext,
    now: Timestamp,
) -> Vec<AgentRow<'a>> {
    let roster = Roster::of(audit);
    let live_roster = Roster::of(live);
    roster
        .candidates
        .iter()
        .copied()
        .filter(|agent| agent.parent_agent_id.is_none() || agent.is_launched_child())
        .filter(|agent| {
            let same =
                |other: &AgentState| other.kind == agent.kind && other.agent_id == agent.agent_id;
            let carried_unended = live
                .agents
                .iter()
                .any(|other| same(other) && other.ended_at.is_none());
            (agent.ended_at.is_some() || !carried_unended)
                && !listed.iter().any(|row| same(row.agent))
        })
        .map(|agent| {
            let handle = closed_address(agent, &roster, &live_roster, reader);
            AgentRow::new(agent, &roster, handle, false, demotion, reader, now)
        })
        .collect()
}

/// Which lanes a selection reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneScope<L> {
    /// Every lane of the room.
    Room,
    /// The caller's current lane, which a query naming `lane` replaces.
    Current(L),
    /// A lane the caller named, which holds beside any lane the query names.
    Named(L),
}

impl<L> LaneScope<L> {
    /// The one lane `query` reads under this scope, or `None` for every lane.
    pub fn lane(self, query: &Query) -> Option<L> {
        match self {
            Self::Current(_) if query.names(Field::Lane) => None,
            Self::Current(lane) | Self::Named(lane) => Some(lane),
            Self::Room => None,
        }
    }

    pub fn as_ref(&self) -> LaneScope<&L> {
        match self {
            Self::Room => LaneScope::Room,
            Self::Current(lane) => LaneScope::Current(lane),
            Self::Named(lane) => LaneScope::Named(lane),
        }
    }

    pub fn map<M>(self, f: impl FnOnce(L) -> M) -> LaneScope<M> {
        match self {
            Self::Room => LaneScope::Room,
            Self::Current(lane) => LaneScope::Current(f(lane)),
            Self::Named(lane) => LaneScope::Named(f(lane)),
        }
    }
}

/// The rows `query` admits within `scope`: [`live_rows`], then `audit`'s
/// [`closed_rows`] when the query reads closed sessions, kept to the lane the
/// scope reads and to the query's default set and terms.
pub fn rows_in_scope<'a>(
    live: &'a SidebarSnapshot,
    audit: Option<&'a SidebarSnapshot>,
    scope: LaneScope<&LaneSelector>,
    query: &Query,
    demotion: &ParkDemotion,
    reader: &AddressContext,
    now: Timestamp,
) -> Vec<AgentRow<'a>> {
    let mut rows = live_rows(live, demotion, reader, now);
    if let Some(audit) = audit.filter(|_| query.reads_closed()) {
        let closed = closed_rows(audit, live, &rows, demotion, reader, now);
        rows.extend(closed);
    }
    let lane = scope.lane(query);
    rows.retain(|row| {
        lane.is_none_or(|lane| agent_in_lane(row.agent, lane)) && query.admits(row, reader)
    });
    rows
}

/// The short Audit address, or `@<agent_id>` when the short one also names a
/// live agent. A provisional id resolves nowhere, live or Audit, so such a
/// row's line names no agent rather than the live holder of its name: an
/// action misses and `agents show` finds nothing.
fn closed_address(
    agent: &AgentState,
    audit: &Roster<'_>,
    live: &Roster<'_>,
    reader: &AddressContext,
) -> String {
    let short = audit.paste_address(agent, reader);
    if live.resolve_many(&short, None, reader).is_err() {
        return short;
    }
    format!("@{}", agent.agent_id)
}

fn pane_bound(snapshot: &SidebarSnapshot, agent: &AgentState) -> bool {
    snapshot
        .agent_panes
        .iter()
        .any(|pane| pane.agent_id.as_ref() == Some(&agent.agent_id))
}

/// A parsed `--filter` query: every term must match.
#[derive(Clone, Debug, Default)]
pub struct Query {
    terms: Vec<Term>,
}

#[derive(Clone, Debug)]
struct Term {
    field: Field,
    op: Op,
    values: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    None,
    Any,
    Text(String),
    Status(AgentStatus),
    Error(ErrorClass),
    Live(bool),
    Number(f64),
}

/// A filter the grammar or the field table refuses; each names its term.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("empty filter: write <field><op><value>, such as status=idle; see `--filter help`")]
    Empty,
    #[error(transparent)]
    Term(#[from] TermError),
    #[error("filter term `{term}`: unknown field `{field}`{suggestion}")]
    UnknownField {
        term: String,
        field: String,
        suggestion: String,
    },
    #[error("filter term `{term}`: `{value}` is not a {field} value; use one of {valid}")]
    UnknownValue {
        term: String,
        field: &'static str,
        value: String,
        valid: String,
    },
    #[error("filter term `{term}`: {field} is not ordered; use = or !=")]
    Unordered { term: String, field: &'static str },
    #[error("filter term `{term}`: `{value}` is not {expected}")]
    InvalidValue {
        term: String,
        value: String,
        expected: &'static str,
    },
    #[error("filter term `{term}`: a comparison takes one value")]
    OneValue { term: String },
}

impl Query {
    /// Parse every `--filter` flag; flags and space-separated terms AND.
    pub fn parse<S: AsRef<str>>(filters: &[S]) -> Result<Self, QueryError> {
        let mut terms = Vec::new();
        for filter in filters {
            let mut rest = filter.as_ref();
            if rest.trim().is_empty() {
                return Err(QueryError::Empty);
            }
            while let Some(term) = filter::next_term(&mut rest) {
                terms.push(Term::parse(term?)?);
            }
        }
        Ok(Self { terms })
    }

    /// Whether a term names `field`, which replaces that field's default.
    pub fn names(&self, field: Field) -> bool {
        self.terms.iter().any(|term| term.field == field)
    }

    /// Whether a closed session can match: `live` is named and every `live`
    /// term admits `no`.
    pub fn reads_closed(&self) -> bool {
        self.names(Field::Live)
            && self
                .terms
                .iter()
                .filter(|term| term.field == Field::Live)
                .all(|term| term.holds(|value| matches!(value, Value::Live(false) | Value::Any)))
    }

    /// The default set (live, top-level) where its field is unnamed, then every term.
    pub fn admits(&self, row: &AgentRow<'_>, reader: &AddressContext) -> bool {
        (self.names(Field::Live) || row.live)
            && (self.names(Field::Parent) || row.missing(Field::Parent))
            && self.matches(row, reader)
    }

    /// Every term, without the default set: for a caller that chooses its own.
    pub fn matches(&self, row: &AgentRow<'_>, reader: &AddressContext) -> bool {
        self.terms.iter().all(|term| term.matches(row, reader))
    }
}

impl Term {
    fn parse(lexed: filter::Term<'_>) -> Result<Self, QueryError> {
        let filter::Term {
            text: term,
            field,
            op,
            values: raw,
        } = lexed;
        let Some(field) = Field::from_name(&field.to_ascii_lowercase()) else {
            let suggestion = nearest_name(field, Field::ALL.map(Field::name)).map_or_else(
                || "; see `--filter help`".to_owned(),
                |name| format!("; did you mean `{name}`?"),
            );
            return Err(QueryError::UnknownField {
                term: term.into(),
                field: field.into(),
                suggestion,
            });
        };
        if op.ordered() && !field.kind().ordered() {
            return Err(QueryError::Unordered {
                term: term.into(),
                field: field.name(),
            });
        }
        let values = raw
            .iter()
            .map(|word| parse_value(field, word, term))
            .collect::<Result<Vec<_>, _>>()?;
        if op.ordered() && !matches!(values.as_slice(), [Value::Number(_)]) {
            return Err(QueryError::OneValue { term: term.into() });
        }
        Ok(Self { field, op, values })
    }

    /// `=` holds when any value does, `!=` when none does.
    fn holds(&self, test: impl Fn(&Value) -> bool) -> bool {
        self.op.holds(&self.values, test)
    }

    fn matches(&self, row: &AgentRow<'_>, reader: &AddressContext) -> bool {
        if self.op.ordered() {
            return match (row.number(self.field), self.values.as_slice()) {
                (Some(have), [Value::Number(want)]) => self.op.compares(have, *want),
                _ => false,
            };
        }
        self.holds(|value| value_matches(self.field, value, row, reader))
    }
}

fn parse_value(field: Field, word: &Word, term: &str) -> Result<Value, QueryError> {
    let raw = match word {
        Word::Any => return Ok(Value::Any),
        Word::None => return Ok(Value::None),
        Word::Text(raw) => raw.as_str(),
    };
    let invalid = |expected| QueryError::InvalidValue {
        term: term.into(),
        value: raw.into(),
        expected,
    };
    let number = |text: &str| {
        text.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
    };
    match field.kind() {
        FieldKind::Text => Ok(Value::Text(raw.to_owned())),
        FieldKind::Address => address_value(field, raw)
            .then(|| Value::Text(raw.to_owned()))
            .ok_or_else(|| {
                invalid(match field {
                    Field::Lane => "a lane such as #feat",
                    Field::Team => "a team such as forge or forge#feat",
                    _ => "an address such as @coder or @coder#feat",
                })
            }),
        FieldKind::Enum => enum_value(field, raw).ok_or_else(|| QueryError::UnknownValue {
            term: term.into(),
            field: field.name(),
            value: raw.into(),
            valid: field.values().join(", "),
        }),
        FieldKind::Duration => parse_compact_duration(raw)
            .map(|duration| Value::Number(duration.as_secs() as f64))
            .map_err(|_| invalid("a duration such as 30m or 2h")),
        FieldKind::Percent => number(raw.strip_suffix('%').unwrap_or(raw))
            .filter(|pct| *pct <= 100.0)
            .map(Value::Number)
            .ok_or_else(|| invalid("a percent such as 80%")),
        FieldKind::Size => {
            let (digits, factor) = match raw.char_indices().last() {
                Some((at, 'k' | 'K')) => (&raw[..at], 1e3),
                Some((at, 'm' | 'M')) => (&raw[..at], 1e6),
                Some((at, 'b' | 'B')) => (&raw[..at], 1e9),
                _ => (raw, 1.0),
            };
            number(digits)
                .map(|count| Value::Number(count * factor))
                .ok_or_else(|| invalid("a size such as 100k or 2M"))
        }
        FieldKind::Dollars => number(raw.strip_prefix('$').unwrap_or(raw))
            .map(Value::Number)
            .ok_or_else(|| invalid("a dollar amount such as $1.50")),
    }
}

/// Whether an address value can name something: a wildcard always can, a
/// lane or team needs a name after its sigil, and a parent must parse as an
/// address whose selector holds no second sigil (`parse_selector` reads
/// `@@x` as the selector `@x`).
fn address_value(field: Field, raw: &str) -> bool {
    if raw.contains('*') {
        return true;
    }
    match field {
        Field::Lane => !raw.trim_start_matches('#').is_empty(),
        Field::Team => {
            let team = raw.trim_start_matches('@');
            team.split_once('#')
                .map_or(!team.is_empty(), |(team, lane)| {
                    !team.is_empty() && !lane.is_empty()
                })
        }
        _ => parse_selector(&as_address(raw))
            .is_ok_and(|(selector, _)| !selector.contains(['@', '#'])),
    }
}

/// A bare word is a handle: `coder` reads as `@coder`.
fn as_address(text: &str) -> String {
    let bare_word = text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if bare_word {
        format!("@{text}")
    } else {
        text.to_owned()
    }
}

fn enum_value(field: Field, raw: &str) -> Option<Value> {
    match field {
        Field::Status => STATUSES
            .into_iter()
            .find(|status| status.as_str().eq_ignore_ascii_case(raw))
            .map(Value::Status),
        Field::Error => ERRORS
            .into_iter()
            .find(|error| error.as_str().eq_ignore_ascii_case(raw))
            .map(Value::Error),
        Field::Live if raw.eq_ignore_ascii_case("yes") => Some(Value::Live(true)),
        Field::Live if raw.eq_ignore_ascii_case("no") => Some(Value::Live(false)),
        _ => None,
    }
}

fn value_matches(field: Field, value: &Value, row: &AgentRow<'_>, reader: &AddressContext) -> bool {
    let text = match value {
        Value::None => return row.missing(field),
        Value::Any => return !row.missing(field),
        Value::Status(status) => return row.status == *status,
        Value::Error(error) => return row.error == Some(*error),
        Value::Live(live) => return row.live == *live,
        Value::Number(number) => return row.number(field) == Some(*number),
        Value::Text(text) => text.as_str(),
    };
    match field {
        Field::Handle => handle_matches(&row.handle, text),
        Field::Lane => lane_matches(row, text),
        Field::Team => {
            let text = text.trim_start_matches('@');
            let (team, lane) = text
                .split_once('#')
                .map_or((text, None), |(team, lane)| (team, Some(lane)));
            row.team.as_deref().is_some_and(|have| glob(team, have))
                && lane.is_none_or(|lane| lane_matches(row, lane))
        }
        Field::Parent => parent_matches(row, text, reader),
        _ => row.text(field).is_some_and(|have| glob(text, have)),
    }
}

/// A wildcard compares the lane label; a name takes the scope rule `#lane` uses.
fn lane_matches(row: &AgentRow<'_>, text: &str) -> bool {
    let text = text.trim_start_matches('#');
    if text.contains('*') {
        return row.lane.as_deref().is_some_and(|lane| glob(text, lane));
    }
    agent_in_lane(row.agent, &LaneSelector::parse(Some(text)))
}

/// `@` is optional, and a pattern without `#lane` ignores the address's lane.
fn handle_matches(handle: &str, pattern: &str) -> bool {
    let handle = handle.trim_start_matches('@');
    let pattern = pattern.trim_start_matches('@');
    glob(pattern, handle)
        || !pattern.contains('#')
            && handle
                .split_once('#')
                .is_some_and(|(base, _)| glob(pattern, base))
}

/// A wildcard compares the parent's address; anything else resolves as an
/// address in the row's own snapshot and must name its parent.
fn parent_matches(row: &AgentRow<'_>, text: &str, reader: &AddressContext) -> bool {
    let Some(parent) = row.parent_agent else {
        return false;
    };
    if text.contains('*') {
        return row
            .parent
            .as_deref()
            .is_some_and(|have| handle_matches(have, text));
    }
    resolve_many(row.source, &as_address(text), None, reader).is_ok_and(|matches| {
        matches
            .iter()
            .any(|agent| agent.kind == parent.kind && agent.agent_id == parent.agent_id)
    })
}

/// One selected agent: the address an action resolves at act time and, for a
/// filter row or a pinned stdin line, the agent that address named at
/// selection, so the act-time read can tell that agent apart from one that has
/// since taken the address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub address: String,
    pub agent: Option<(AgentKind, AgentSessionId)>,
}

/// Why a selected address no longer names the agent selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SelectionMoved {
    #[error("now names another agent than the one selected")]
    Another,
    #[error("has ended")]
    Ended,
}

impl Selected {
    /// Whether `agent`, what the address resolves to now, is still the agent
    /// selected and still live. `None` is a target with no agent, such as a
    /// bare pane: an unpinned address may name one, while for a pinned one it
    /// means the agent selected has gone from behind the address.
    pub fn check(&self, agent: Option<&AgentState>) -> Result<(), SelectionMoved> {
        let Some((kind, id)) = &self.agent else {
            return match agent {
                Some(agent) if agent.ended_at.is_some() => Err(SelectionMoved::Ended),
                _ => Ok(()),
            };
        };
        let Some(agent) = agent else {
            return Err(SelectionMoved::Ended);
        };
        if (kind, id) != (&agent.kind, &agent.agent_id) {
            return Err(SelectionMoved::Another);
        }
        if agent.ended_at.is_some() {
            return Err(SelectionMoved::Ended);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
