//! State predicates for clock-evaluated loop conditions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::account::read_rate_limits_cache;
use crate::agents::{ProviderCapacity, RateLimitWindow, RoomLoginSet, WindowSpan};
use crate::forge::pr_state::{PrQueueFact, PrStateCache};
use crate::ids::{AgentKind, LoginKey, LoginName};
use crate::store::snapshot::{WorktreeCi, WorktreePrState};

fn probe_paths(
    entry: &crate::config::TaskEntry,
    name: &str,
    owned: &[std::path::PathBuf],
    ledger: &super::launch_ledger::Ledger,
) -> Vec<std::path::PathBuf> {
    if !entry.each_worktree {
        return vec![entry.run_dir()];
    }
    owned
        .iter()
        .map(|path| crate::utils::path::normalize_path_lexical(path))
        .filter(|path| {
            !ledger
                .get(name)
                .is_some_and(|launches| launches.contains_key(path))
        })
        .collect()
}

/// Checkout scopes and branch selectors whose live conditions need forge probes.
pub(crate) fn probe_demand(
    runtime: &crate::RuntimePaths,
    project_root: Option<&Path>,
) -> BTreeMap<String, BTreeSet<String>> {
    let arming = super::arming::load();
    let now = Timestamp::now();
    let mut tasks: Vec<_> = super::fire::runnable_tasks_for(runtime, project_root)
        .into_iter()
        .filter_map(|(name, task)| {
            if super::arming::ArmState::resolve(arming.get(&task.key(&name)), task.source(), now)
                != super::arming::ArmState::Live
            {
                return None;
            }
            let super::Trigger::Condition { expr, .. } = &task.trigger().as_ref().ok()?.trigger
            else {
                return None;
            };
            expr.reads_forge().then_some((name, task))
        })
        .collect();
    let ledger = if tasks.iter().any(|(_, task)| task.entry().each_worktree) {
        match super::launch_ledger::load_room(runtime, project_root) {
            Ok(ledger) => ledger,
            Err(error) => {
                tracing::warn!(%error, "resident loop ledger unavailable for probes");
                tasks.retain(|(_, task)| !task.entry().each_worktree);
                BTreeMap::new()
            }
        }
    } else {
        BTreeMap::new()
    };
    let owned = tasks
        .iter()
        .find(|(_, task)| task.entry().each_worktree)
        .map(|(_, task)| {
            crate::worktree::discover_owned_cached(
                project_root.unwrap_or(&task.entry().resolved_root()),
            )
        })
        .transpose()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "enumerating loop probe worktrees");
            None
        })
        .unwrap_or_default()
        .into_iter()
        .map(|worktree| worktree.marker.worktree_path)
        .collect::<Vec<_>>();
    let mut demand = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, task) in tasks {
        let Ok(super::ParsedTrigger {
            trigger: super::Trigger::Condition { expr, .. },
            ..
        }) = task.trigger()
        else {
            continue;
        };
        let branches = expr
            .terms()
            .filter_map(|term| ForgeKey::parse(&term.key)?.branch())
            .collect::<BTreeSet<_>>();
        for scope in probe_paths(task.entry(), &name, &owned, &ledger)
            .into_iter()
            .filter(|scope| scope.is_dir())
        {
            demand
                .entry(scope.to_string_lossy().into_owned())
                .or_default()
                .extend(branches.iter().map(|branch| (*branch).to_owned()));
        }
    }
    demand
}

/// A room's last-known CI and PR readings. Absence of this source means unknown.
pub struct CiSource(PrStateCache);

/// The condition keys read from the room's forge cache. This is the one place
/// that names them: the parser, the probe scopes, and the no-room hint ask it.
#[derive(Clone, Copy)]
enum ForgeKey<'a> {
    Ci(Option<&'a str>),
    Pr(Option<&'a str>),
    PrQueue(Option<&'a str>),
}

impl<'a> ForgeKey<'a> {
    fn parse(key: &'a str) -> Option<Self> {
        let (key, branch) = match key.split_once('[') {
            Some((key, branch)) => (key, Some(branch.strip_suffix(']')?)),
            None => (key, None),
        };
        match key {
            "ci" => Some(Self::Ci(branch)),
            "pr" => Some(Self::Pr(branch)),
            "pr.queue" => Some(Self::PrQueue(branch)),
            _ => None,
        }
    }

    fn branch(self) -> Option<&'a str> {
        match self {
            Self::Ci(branch) | Self::Pr(branch) | Self::PrQueue(branch) => branch,
        }
    }

    fn values(self) -> &'static [&'static str] {
        match self {
            Self::Ci(_) => &["passed", "failed", "pending", "unknown"],
            Self::Pr(_) => &["open", "merged", "closed", "unknown"],
            Self::PrQueue(_) => &["queued", "dequeued", "none", "unknown"],
        }
    }

    fn read(self, cache: &PrStateCache, scope: &Path) -> Option<&'static str> {
        let scope = scope.to_string_lossy();
        let (link, head_ci) = if let Some(branch) = self.branch() {
            let repo = cache.path_repos.get(scope.as_ref())?;
            let row = cache.branches.get(repo)?.get(branch)?;
            (row.pr.as_ref(), row.ci)
        } else {
            (
                cache.states.get(scope.as_ref()),
                cache.branch_ci.get(scope.as_ref()).copied(),
            )
        };
        match self {
            Self::Ci(_) => link
                .filter(|link| {
                    matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged)
                })
                .and_then(|link| link.ci)
                .or(head_ci)
                .map(|ci| match ci {
                    WorktreeCi::Passing => "passed",
                    WorktreeCi::Failing => "failed",
                    WorktreeCi::Pending => "pending",
                }),
            Self::Pr(_) => link.map(|link| match link.state {
                WorktreePrState::Open => "open",
                WorktreePrState::Merged => "merged",
                WorktreePrState::Closed => "closed",
            }),
            Self::PrQueue(_) => link.map(|link| match link.queue() {
                Some(PrQueueFact::Queued { .. }) => "queued",
                Some(PrQueueFact::Dequeued { .. }) => "dequeued",
                None => "none",
            }),
        }
    }
}

impl CiSource {
    pub fn read(runtime: &crate::RuntimePaths) -> Self {
        Self(crate::forge::pr_state::read_pr_state_cache(
            &runtime.pr_state_path(),
        ))
    }
}

/// The stored kind-wide provider windows of every account, with the room's
/// account selection, read at most once and only when a `window.*` term asks
/// for one.
pub struct WindowReadings<'a> {
    runtime: Option<&'a crate::RuntimePaths>,
    now: Timestamp,
    capacities: std::cell::OnceCell<(RoomLoginSet, BTreeMap<LoginKey, ProviderCapacity>)>,
}

impl<'a> WindowReadings<'a> {
    pub fn new(runtime: Option<&'a crate::RuntimePaths>, now: Timestamp) -> Self {
        Self {
            runtime,
            now,
            capacities: std::cell::OnceCell::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(now: Timestamp, capacities: BTreeMap<LoginKey, ProviderCapacity>) -> Self {
        Self {
            runtime: None,
            now,
            capacities: std::cell::OnceCell::from((RoomLoginSet::native(), capacities)),
        }
    }

    /// Percent of the `span` window left, projected to now, on `account` of
    /// `kind`, else on the room's account for it; a lifted window has no limit.
    fn left(
        &self,
        kind: &AgentKind,
        account: Option<&LoginName>,
        span: WindowSpan,
    ) -> Option<String> {
        let (room, capacities) = self.capacities.get_or_init(|| self.read());
        let key = match account {
            Some(name) => LoginKey::new(kind.clone(), name.clone()),
            None => room.default_key(kind)?,
        };
        let window = capacities.get(&key)?.window_of_span(span, self.now)?;
        Some(percent_left(&window)?.to_string())
    }

    fn read(&self) -> (RoomLoginSet, BTreeMap<LoginKey, ProviderCapacity>) {
        let Some(runtime) = self.runtime else {
            return (RoomLoginSet::native(), BTreeMap::new());
        };
        let capacities = read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .into_iter()
            .filter(|(_, entry)| entry.scope.is_kind_wide())
            .map(|(key, entry)| (key, ProviderCapacity::from_windows(entry.limits.windows)))
            .collect();
        (RoomLoginSet::for_runtime(runtime), capacities)
    }
}

/// Percent of a window left; a lifted window has no limit.
pub(super) fn percent_left(window: &RateLimitWindow) -> Option<u8> {
    if window.lifted {
        return Some(100);
    }
    Some(100 - window.used_percentage?.min(100))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub ok: bool,
    pub readings: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhenState {
    #[serde(default)]
    pub(super) fingerprint: Option<(String, Option<std::time::Duration>, std::path::PathBuf)>,
    pub since: Timestamp,
    pub fired: bool,
}

impl WhenState {
    pub(super) fn matches_condition(
        &self,
        expr: &WhenExpr,
        hold: Option<std::time::Duration>,
        run_dir: &Path,
    ) -> bool {
        self.fingerprint
            .as_ref()
            .is_some_and(|(when, duration, scope)| {
                when == &expr.to_string() && *duration == hold && scope == run_dir
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionEvidence {
    #[serde(default)]
    pub since: Option<Timestamp>,
    pub when: String,
    pub hold: Option<String>,
    pub held_ms: u64,
    pub readings: BTreeMap<String, Option<String>>,
}

pub fn evaluate(
    expr: &WhenExpr,
    scope: &Path,
    ci_source: Option<&CiSource>,
    provider: Option<&AgentKind>,
    account: Option<&LoginName>,
    windows: &WindowReadings<'_>,
) -> Verdict {
    let mut readings = BTreeMap::new();
    for term in expr.terms() {
        readings.entry(term.key.clone()).or_insert_with(|| {
            if term.key == "team.stage" {
                return crate::harness::scratch::board_stage(scope).map(|stage| stage.name);
            }
            if let Some(key) = ForgeKey::parse(&term.key) {
                return ci_source
                    .and_then(|source| key.read(&source.0, scope))
                    .map(str::to_owned);
            }
            // WhenExpr can only be constructed through the key-validating parser.
            let span = window_span(&term.key)
                .expect("the parser admits only team.stage, forge, and window keys");
            provider.and_then(|kind| windows.left(kind, account, span))
        });
    }
    Verdict {
        ok: expr.0.matches(&readings),
        readings,
    }
}

/// The span a `window.<span>.left` key reads.
fn window_span(key: &str) -> Option<WindowSpan> {
    key.strip_prefix("window.")?
        .strip_suffix(".left")?
        .parse()
        .ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenExpr(Node);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Term(WhenTerm),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenTerm {
    pub key: String,
    op: Op,
    pub values: Vec<String>,
}

/// How a term compares its reading: membership for named states, order for window percents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    In,
    NotIn,
    Ge,
    Le,
    Gt,
    Lt,
}

impl Op {
    /// Comparisons in lexing order: a two-byte token before its one-byte prefix.
    const COMPARISONS: [Self; 4] = [Self::Ge, Self::Le, Self::Gt, Self::Lt];

    fn token(self) -> &'static str {
        match self {
            Self::In => "=",
            Self::NotIn => "!=",
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Lt => "<",
        }
    }
}

impl WhenTerm {
    fn comparison_evidence(&self, reading: Option<&str>) -> String {
        let op = if reading.is_none() || self.matches(reading) {
            self.op.token()
        } else {
            match self.op {
                Op::In => "!=",
                Op::NotIn => "=",
                Op::Ge => "<",
                Op::Le => ">",
                Op::Gt => "<=",
                Op::Lt => ">=",
            }
        };
        format!(
            "{}={}{op}{}",
            self.key,
            reading.unwrap_or("unknown"),
            self.values.join(",")
        )
    }

    pub fn matches(&self, reading: Option<&str>) -> bool {
        if self.op == Op::In {
            return self
                .values
                .iter()
                .any(|value| value == reading.unwrap_or("unknown"));
        }
        let Some(reading) = reading else {
            return false;
        };
        if self.op == Op::NotIn {
            return !self.values.iter().any(|value| value == reading);
        }
        let (Ok(reading), Some(Ok(bound))) = (
            reading.parse::<u8>(),
            self.values.first().map(|value| value.parse::<u8>()),
        ) else {
            return false;
        };
        match self.op {
            Op::Ge => reading >= bound,
            Op::Le => reading <= bound,
            Op::Gt => reading > bound,
            Op::Lt => reading < bound,
            Op::In | Op::NotIn => unreachable!("membership returned above"),
        }
    }
}

impl fmt::Display for WhenTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.key,
            self.op.token(),
            self.values.join(",")
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("clause {clause}, position {position}: {fix}")]
pub struct WhenError {
    clause: usize,
    position: usize,
    fix: String,
}

impl WhenExpr {
    pub(crate) fn blocking_evidence(&self, verdict: &Verdict) -> String {
        let (term, negated) = self.0.blocking_term(&verdict.readings, true);
        let evidence = term.comparison_evidence(verdict.readings[&term.key].as_deref());
        format!("{}{evidence}", if negated { "!" } else { "" })
    }

    pub fn parse(clauses: &[String]) -> Result<Self, WhenError> {
        Self::parse_with_stages(clauses, None)
    }

    /// Validate stage names against the effective configuration at admission.
    pub fn parse_with_stages(
        clauses: &[String],
        stages: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Self, WhenError> {
        let mut joined = None;
        let mut nodes = 0;
        for (index, clause) in clauses.iter().enumerate() {
            let mut parser = Parser {
                input: clause,
                pos: 0,
                clause: index + 1,
                depth: 0,
                nodes,
                stages,
            };
            if joined.is_some() {
                parser.node()?;
            }
            let node = parser.or()?;
            parser.space();
            if parser.pos != clause.len() {
                return Err(parser.error(if parser.rest().starts_with(')') {
                    "unbalanced parentheses; remove the extra ')'"
                } else {
                    "expected && or || between terms"
                }));
            }
            nodes = parser.nodes;
            joined = Some(match joined {
                None => node,
                Some(previous) => Node::And(Box::new(previous), Box::new(node)),
            });
        }
        joined.map(Self).ok_or_else(|| WhenError {
            clause: 1,
            position: 1,
            fix: "empty term; use key=value".to_owned(),
        })
    }

    /// Leaf terms in expression order, including repeated keys.
    pub fn terms(&self) -> impl Iterator<Item = &WhenTerm> {
        let mut terms = Vec::new();
        self.0.terms(&mut terms);
        terms.into_iter()
    }

    /// Whether any term reads the room's forge cache.
    pub fn reads_forge(&self) -> bool {
        self.terms()
            .any(|term| ForgeKey::parse(&term.key).is_some())
    }

    /// The spans the expression's `window.*` terms read, in expression order.
    pub(super) fn window_spans(&self) -> impl Iterator<Item = WindowSpan> {
        self.terms().filter_map(|term| window_span(&term.key))
    }

    /// Unique readings in expression order rather than the map's key order.
    pub fn readings<'a>(
        &'a self,
        verdict: &'a Verdict,
    ) -> impl Iterator<Item = (&'a str, Option<&'a str>)> {
        let mut seen = std::collections::BTreeSet::new();
        self.terms()
            .filter(move |term| seen.insert(term.key.as_str()))
            .map(|term| {
                (
                    term.key.as_str(),
                    verdict
                        .readings
                        .get(&term.key)
                        .and_then(|value| value.as_deref()),
                )
            })
    }
}

impl fmt::Display for WhenExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display(f, 0)
    }
}

impl Node {
    fn blocking_term(
        &self,
        readings: &BTreeMap<String, Option<String>>,
        expected: bool,
    ) -> (&WhenTerm, bool) {
        match self {
            Self::Term(term) => (term, !expected),
            Self::Not(node) => node.blocking_term(readings, !expected),
            Self::And(left, right) | Self::Or(left, right) => {
                if left.matches(readings) != expected {
                    left.blocking_term(readings, expected)
                } else {
                    right.blocking_term(readings, expected)
                }
            }
        }
    }

    fn terms<'a>(&'a self, terms: &mut Vec<&'a WhenTerm>) {
        match self {
            Self::Term(term) => terms.push(term),
            Self::Not(node) => node.terms(terms),
            Self::And(left, right) | Self::Or(left, right) => {
                left.terms(terms);
                right.terms(terms);
            }
        }
    }

    fn matches(&self, readings: &BTreeMap<String, Option<String>>) -> bool {
        match self {
            Self::Term(term) => term.matches(readings[&term.key].as_deref()),
            Self::Not(node) => !node.matches(readings),
            Self::And(left, right) => left.matches(readings) && right.matches(readings),
            Self::Or(left, right) => left.matches(readings) || right.matches(readings),
        }
    }

    fn display(&self, f: &mut fmt::Formatter<'_>, parent: u8) -> fmt::Result {
        let precedence = match self {
            Self::Or(..) => 1,
            Self::And(..) => 2,
            Self::Not(_) => 3,
            Self::Term(_) => 4,
        };
        if precedence < parent {
            f.write_str("(")?;
        }
        match self {
            Self::Term(term) => write!(f, "{term}")?,
            Self::Not(node) => {
                f.write_str("!")?;
                node.display(f, precedence)?;
            }
            Self::And(left, right) | Self::Or(left, right) => {
                left.display(f, precedence)?;
                f.write_str(if matches!(self, Self::And(..)) {
                    " && "
                } else {
                    " || "
                })?;
                right.display(f, precedence)?;
            }
        }
        if precedence < parent {
            f.write_str(")")?;
        }
        Ok(())
    }
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    clause: usize,
    depth: usize,
    nodes: usize,
    stages: Option<&'a std::collections::BTreeSet<String>>,
}

impl Parser<'_> {
    fn node(&mut self) -> Result<(), WhenError> {
        if self.nodes >= 128 {
            return Err(self.error("expression exceeds 128 nodes; simplify the condition"));
        }
        self.nodes += 1;
        Ok(())
    }

    fn rest(&self) -> &str {
        &self.input[self.pos..]
    }
    fn space(&mut self) {
        self.pos += self.rest().len() - self.rest().trim_start().len();
    }
    fn take(&mut self, token: &str) -> bool {
        self.space();
        if self.rest().starts_with(token) {
            self.pos += token.len();
            true
        } else {
            false
        }
    }
    fn error(&self, fix: impl Into<String>) -> WhenError {
        WhenError {
            clause: self.clause,
            position: self.input[..self.pos].chars().count() + 1,
            fix: fix.into(),
        }
    }
    fn word(&mut self) -> Result<String, WhenError> {
        self.space();
        let len = self
            .rest()
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
            .count();
        if len == 0 {
            return Err(self.error("empty term; use key=value"));
        }
        let word = self.rest()[..len].to_owned();
        self.pos += len;
        Ok(word)
    }

    fn key(&mut self) -> Result<String, WhenError> {
        let mut key = self.word()?;
        if key != "team.stage" && window_span(&key).is_none() && ForgeKey::parse(&key).is_none() {
            let keys = [
                "team.stage",
                "ci",
                "pr",
                "pr.queue",
                "window.5h.left",
                "window.7d.left",
            ];
            let suggestion = crate::utils::suggest::nearest_name(&key, keys)
                .map(|name| format!("did you mean {name}? "))
                .unwrap_or_default();
            return Err(self.error(format!(
                "{suggestion}unknown key; keys: {}; ci, pr, pr.queue also accept [branch]",
                keys.join(", ")
            )));
        }
        if !self.take("[") {
            return Ok(key);
        }
        if ForgeKey::parse(&key).is_none() {
            let fix = if window_span(&key).is_some() {
                format!("{key}>=40")
            } else {
                "team.stage=unknown".to_owned()
            };
            return Err(self.error(format!(
                "{key} cannot take a branch selector; only ci, pr, pr.queue take one; use {fix}"
            )));
        }
        let fix = format!("{key}[branch]=unknown");
        let Some(end) = self.rest().find(']') else {
            return Err(self.error(format!("missing ']' in branch selector; use {fix}")));
        };
        let branch = &self.rest()[..end];
        if branch.is_empty() || branch.chars().any(|ch| ch.is_whitespace() || ch == '[') {
            return Err(self.error(format!(
                "branch selector must be nonempty, without whitespace, '[' or ']'; use {fix}"
            )));
        }
        key.push('[');
        key.push_str(branch);
        key.push(']');
        self.pos += end + 1;
        Ok(key)
    }
    fn or(&mut self) -> Result<Node, WhenError> {
        let mut left = self.and()?;
        while self.take("||") {
            self.node()?;
            self.operand()?;
            left = Node::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }
    fn and(&mut self) -> Result<Node, WhenError> {
        let mut left = self.not()?;
        while self.take("&&") {
            self.node()?;
            self.operand()?;
            left = Node::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }
    fn operand(&mut self) -> Result<(), WhenError> {
        self.space();
        if self.rest().is_empty() || self.rest().starts_with(')') {
            return Err(self.error("dangling operator; add a term"));
        }
        Ok(())
    }
    fn not(&mut self) -> Result<Node, WhenError> {
        if self.depth >= 128 {
            return Err(self.error("expression nesting is too deep; simplify the condition"));
        }
        self.depth += 1;
        let node = if self.take("!") {
            self.node()?;
            self.operand()?;
            Node::Not(Box::new(self.not()?))
        } else if self.take("(") {
            let node = self.or()?;
            if !self.take(")") {
                return Err(self.error("unbalanced parentheses; add ')'"));
            }
            node
        } else {
            self.node()?;
            let key = self.key()?;
            if window_span(&key).is_some() {
                return self.window_term(key);
            }
            let forge_key = ForgeKey::parse(&key);
            if Op::COMPARISONS.iter().any(|op| self.take(op.token())) {
                let value = self.word()?;
                return Err(self.error(format!(
                    "comparisons apply only to window keys; use {key}={value}"
                )));
            }
            let op = if self.take("!=") {
                Op::NotIn
            } else if self.take("=") {
                Op::In
            } else {
                return Err(self.error("expected '=' or '!='; use key=value"));
            };
            let mut values = vec![self.word()?];
            while self.take(",") {
                values.push(self.word()?);
            }
            if key == "team.stage"
                && let Some(stages) = self.stages
            {
                for value in &values {
                    if value != "unknown" && !stages.contains(value) {
                        let suggestion = crate::utils::suggest::nearest_name(
                            value,
                            stages
                                .iter()
                                .map(String::as_str)
                                .chain(std::iter::once("unknown")),
                        )
                        .map(|name| format!("did you mean {name}? "))
                        .unwrap_or_default();
                        return Err(self.error(format!(
                            "{suggestion}no team defines stage {value}; stages: {}",
                            stages.iter().cloned().collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
            }
            if let Some(allowed) = forge_key.map(ForgeKey::values)
                && let Some(value) = values
                    .iter()
                    .find(|value| !allowed.contains(&value.as_str()))
            {
                let suggestion =
                    crate::utils::suggest::nearest_name(value, allowed.iter().copied())
                        .map(|name| format!("did you mean {name}? "))
                        .unwrap_or_default();
                return Err(self.error(format!(
                    "{suggestion}unknown {key} value; values: {}",
                    allowed.join(", ")
                )));
            }
            Node::Term(WhenTerm { key, op, values })
        };
        self.depth -= 1;
        Ok(node)
    }

    /// A `window.<span>.left` comparison against a whole percent.
    fn window_term(&mut self, key: String) -> Result<Node, WhenError> {
        if self.take("!=") {
            let bound = self.percent(&key, Op::Lt)?;
            return Err(self.error(format!(
                "!= is not supported; use {key}<{bound} || {key}>{bound}"
            )));
        }
        if let Some(op) = Op::COMPARISONS.into_iter().find(|op| self.take(op.token())) {
            let bound = self.percent(&key, op)?;
            return Ok(Node::Term(WhenTerm {
                key,
                op,
                values: vec![bound.to_string()],
            }));
        }
        if self.take("=") {
            let bound = self.percent(&key, Op::Ge)?;
            return Err(self.error(format!(
                "= is not supported on window keys; use {key}>={bound} && {key}<={bound}"
            )));
        }
        Err(self.error(format!(
            "expected a comparison (>=, <=, >, <); use {key}>=40"
        )))
    }

    fn percent(&mut self, key: &str, op: Op) -> Result<u8, WhenError> {
        let op = op.token();
        let Ok(word) = self.word() else {
            return Err(self.error(format!(
                "expected a whole percent from 0 to 100; use {key}{op}40"
            )));
        };
        match word.parse::<i64>() {
            Ok(value) => u8::try_from(value)
                .ok()
                .filter(|percent| *percent <= 100)
                .ok_or_else(|| {
                    self.error(format!(
                        "percent left runs from 0 to 100; use {key}{op}{}",
                        value.clamp(0, 100)
                    ))
                }),
            Err(_) => Err(self.error(format!(
                "expected a whole percent from 0 to 100; use {key}{op}40"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "when_tests.rs"]
mod tests;
