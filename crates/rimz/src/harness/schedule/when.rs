//! State predicates for clock-evaluated loop conditions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::account::read_rate_limits_cache;
use crate::agents::{ProviderCapacity, RateLimitWindow, RoomLoginSet, WindowSpan};
use crate::forge::pr_state::PrStateCache;
use crate::ids::{AgentKind, LoginKey, LoginName};
use crate::store::snapshot::{WorktreeCi, WorktreePrState};

/// Checkout scopes whose live loop conditions need sidebar CI probes.
pub(crate) fn ci_scopes(
    runtime: &crate::RuntimePaths,
    project_root: Option<&Path>,
) -> BTreeSet<String> {
    let arming = super::arming::load();
    let now = Timestamp::now();
    super::fire::runnable_tasks_for(runtime, project_root)
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
            let scope = task.entry().run_dir();
            (expr.terms().any(|term| term.key == "ci") && scope.is_dir())
                .then(|| scope.to_string_lossy().into_owned())
        })
        .collect()
}

/// A room's last-known CI reading. Absence of this source means unknown CI.
pub struct CiSource(PrStateCache);

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionEvidence {
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
        readings
            .entry(term.key.clone())
            .or_insert_with(|| match term.key.as_str() {
                "team.stage" => crate::harness::scratch::board_stage(scope).map(|stage| stage.name),
                "ci" => ci_source.and_then(|source| {
                    let key = scope.to_string_lossy();
                    source
                        .0
                        .states
                        .get(key.as_ref())
                        .filter(|link| {
                            matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged)
                        })
                        .and_then(|link| link.ci)
                        .or_else(|| source.0.branch_ci.get(key.as_ref()).copied())
                        .map(|ci| {
                            match ci {
                                WorktreeCi::Passing => "passed",
                                WorktreeCi::Failing => "failed",
                                WorktreeCi::Pending => "pending",
                            }
                            .to_owned()
                        })
                }),
                key => {
                    // WhenExpr can only be constructed through the key-validating parser.
                    let span = window_span(key)
                        .expect("the parser admits only team.stage, ci, and window keys");
                    provider.and_then(|kind| windows.left(kind, account, span))
                }
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
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Lt => "<",
        }
    }
}

impl WhenTerm {
    pub fn matches(&self, reading: Option<&str>) -> bool {
        let Some(reading) = reading else {
            return false;
        };
        if self.op == Op::In {
            return self.values.iter().any(|value| value == reading);
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
            Op::In => unreachable!("membership returned above"),
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
            let key = self.word()?;
            if window_span(&key).is_some() {
                return self.window_term(key);
            }
            if self.take("!=") {
                return Err(self.error(format!(
                    "!= is not supported; use !{key}={}",
                    self.rest().trim()
                )));
            }
            if key != "team.stage" && key != "ci" {
                return Err(
                    self.error("unknown key; keys: team.stage, ci, window.5h.left, window.7d.left")
                );
            }
            if Op::COMPARISONS.iter().any(|op| self.take(op.token())) {
                let value = self.word()?;
                return Err(self.error(format!(
                    "comparisons apply only to window keys; use {key}={value}"
                )));
            }
            if !self.take("=") {
                return Err(self.error("expected '='; use key=value"));
            }
            let mut values = vec![self.word()?];
            while self.take(",") {
                values.push(self.word()?);
            }
            if key == "team.stage"
                && let Some(stages) = self.stages
            {
                for value in &values {
                    if !stages.contains(value) {
                        return Err(self.error(format!(
                            "no team defines stage {value}; stages: {}",
                            stages.iter().cloned().collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
            }
            if key == "ci"
                && values
                    .iter()
                    .any(|value| !matches!(value.as_str(), "passed" | "failed" | "pending"))
            {
                return Err(self.error("unknown ci value; values: passed, failed, pending"));
            }
            Node::Term(WhenTerm {
                key,
                op: Op::In,
                values,
            })
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
mod tests {
    use super::*;

    fn no_windows() -> WindowReadings<'static> {
        WindowReadings::fixed(Timestamp::now(), BTreeMap::new())
    }

    #[test]
    fn whole_expression_node_limit_includes_flat_chains_and_clauses() {
        for separator in [" && ", " || "] {
            assert!(WhenExpr::parse(&[vec!["ci=passed"; 64].join(separator)]).is_ok());
            let error = WhenExpr::parse(&[vec!["ci=passed"; 65].join(separator)]).unwrap_err();
            assert!(error.to_string().contains("simplify the condition"));
        }
        assert!(WhenExpr::parse(&vec!["ci=passed".to_owned(); 64]).is_ok());
        assert!(WhenExpr::parse(&vec!["ci=passed".to_owned(); 65]).is_err());
        assert!(WhenExpr::parse(&[vec!["!ci=passed"; 43].join(" && ")]).is_ok());
        assert!(WhenExpr::parse(&[vec!["!ci=passed"; 44].join(" && ")]).is_err());
    }

    #[test]
    fn ci_link_fallback_mapping_and_run_dir_key() {
        use crate::config::TaskEntry;
        use crate::forge::pr_state::PrLink;
        use crate::store::snapshot::{WorktreeCi, WorktreePrState};

        let root = tempfile::tempdir().unwrap();
        let scope = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(root.path()),
            root.path(),
        )
        .unwrap();
        runtime.ensure_dirs().unwrap();
        let entry = TaskEntry {
            root: root.path().to_owned(),
            dir: Some(scope.path().to_owned()),
            ..TaskEntry::default()
        };
        // The producer copies the worktree's absolute path into Target::path unchanged.
        let key = scope.path().to_string_lossy().into_owned();
        for (ci, label) in [
            (WorktreeCi::Passing, "passed"),
            (WorktreeCi::Failing, "failed"),
            (WorktreeCi::Pending, "pending"),
        ] {
            for state in [
                WorktreePrState::Open,
                WorktreePrState::Merged,
                WorktreePrState::Closed,
            ] {
                let link: PrLink =
                    serde_json::from_value(serde_json::json!({"state": state, "ci": ci})).unwrap();
                let cache = CiSource(PrStateCache {
                    states: BTreeMap::from([(key.clone(), link)]),
                    branch_ci: BTreeMap::from([(key.clone(), WorktreeCi::Pending)]),
                    ..PrStateCache::default()
                });
                crate::disk::atomic::write_temp_then_rename_cache(
                    &runtime.pr_state_path(),
                    &cache.0,
                )
                .unwrap();
                let cache = CiSource::read(&runtime);
                let expected = if state == WorktreePrState::Closed {
                    "pending"
                } else {
                    label
                };
                let expr = WhenExpr::parse(&[format!("ci={expected}")]).unwrap();
                let verdict = evaluate(
                    &expr,
                    &entry.run_dir(),
                    Some(&cache),
                    None,
                    None,
                    &no_windows(),
                );
                assert!(verdict.ok, "{state:?} {ci:?}");
                assert_eq!(verdict.readings["ci"].as_deref(), Some(expected));
                assert!(!evaluate(&expr, root.path(), Some(&cache), None, None, &no_windows()).ok);
                assert!(!evaluate(&expr, &entry.run_dir(), None, None, None, &no_windows()).ok);
            }
        }
        let expr = WhenExpr::parse(&["ci=passed".to_owned()]).unwrap();
        assert_eq!(
            evaluate(
                &expr,
                &entry.run_dir(),
                Some(&CiSource(PrStateCache::default())),
                None,
                None,
                &no_windows(),
            )
            .readings["ci"],
            None
        );
    }

    #[test]
    fn readings_and_boolean_truth_include_unknown_negation() {
        let scope = tempfile::tempdir().unwrap();
        for (input, expected) in [
            ("ci=failed", false),
            ("!ci=failed", true),
            ("team.stage=Done", false),
        ] {
            let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
            assert_eq!(
                evaluate(&expr, scope.path(), None, None, None, &no_windows()).ok,
                expected,
                "{input}"
            );
        }
        std::fs::write(scope.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        let cache = CiSource(PrStateCache {
            branch_ci: BTreeMap::from([(
                scope.path().to_string_lossy().into_owned(),
                crate::store::snapshot::WorktreeCi::Pending,
            )]),
            ..PrStateCache::default()
        });
        for (input, expected) in [
            ("ci=passed,pending", true),
            ("team.stage=Done && ci=passed", false),
            ("team.stage=Done || ci=passed", true),
            ("!team.stage=Done && ci=pending", false),
            ("ci=passed && ci=failed || team.stage=Done", true),
            ("ci=passed && (ci=failed || team.stage=Done)", false),
        ] {
            let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
            assert_eq!(
                evaluate(&expr, scope.path(), Some(&cache), None, None, &no_windows()).ok,
                expected,
                "{input}"
            );
        }
        let expr = WhenExpr::parse(&["team.stage=Done && ci=pending".to_owned()]).unwrap();
        assert_eq!(
            evaluate(&expr, scope.path(), None, None, None, &no_windows()).readings,
            BTreeMap::from([
                ("team.stage".to_owned(), Some("Done".to_owned())),
                ("ci".to_owned(), None),
            ])
        );
    }

    #[test]
    fn grammar_and_canonical_display() {
        for (clauses, expected) in [
            (vec!["ci=passed"], "ci=passed"),
            (vec![" ci = passed , pending "], "ci=passed,pending"),
            (
                vec!["team.stage=Done && ci=passed || ci=pending"],
                "team.stage=Done && ci=passed || ci=pending",
            ),
            (
                vec!["!ci=failed && team.stage=Done"],
                "!ci=failed && team.stage=Done",
            ),
            (
                vec!["!(ci=failed || ci=pending)"],
                "!(ci=failed || ci=pending)",
            ),
            (vec!["((!!ci=passed))"], "!!ci=passed"),
            (
                vec!["ci=passed || ci=pending", "team.stage=Done"],
                "(ci=passed || ci=pending) && team.stage=Done",
            ),
            (vec![" window.5h.left >= 40 "], "window.5h.left>=40"),
            (
                vec!["window.7d.left<10 || !window.5h.left>90"],
                "window.7d.left<10 || !window.5h.left>90",
            ),
            (
                vec!["window.5h.left<=0 && ci=passed", "window.7d.left>100"],
                "window.5h.left<=0 && ci=passed && window.7d.left>100",
            ),
        ] {
            let clauses = clauses.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let expr = WhenExpr::parse(&clauses).unwrap();
            assert_eq!(expr.to_string(), expected);
            assert_eq!(
                WhenExpr::parse(&[expected.to_owned()]).unwrap().to_string(),
                expected
            );
        }
    }

    #[test]
    fn refusals_name_clause_position_and_fix() {
        for (input, hint) in [
            (
                "agent=idle",
                "team.stage, ci, window.5h.left, window.7d.left",
            ),
            (
                "window.1h.left>=40",
                "team.stage, ci, window.5h.left, window.7d.left",
            ),
            ("ci=green", "passed, failed, pending"),
            ("ci!=failed", "!ci=failed"),
            ("", "empty term"),
            ("ci=", "empty term"),
            ("ci=passed,", "empty term"),
            ("(ci=passed", "parenthes"),
            ("ci=passed)", "parenthes"),
            ("ci=passed &&", "dangling operator"),
            ("ci=passed ||", "dangling operator"),
            ("!", "dangling operator"),
        ] {
            let error = WhenExpr::parse(&["ci=passed".to_owned(), input.to_owned()])
                .unwrap_err()
                .to_string();
            assert!(error.contains("clause 2"), "{error}");
            assert!(error.contains("position"), "{error}");
            assert!(error.contains(hint), "{input}: {error}");
        }
        assert!(WhenExpr::parse(&[]).is_err());
    }

    #[test]
    fn window_refusals_suggest_a_form_that_parses() {
        for (input, hint) in [
            (
                "window.5h.left=40",
                "window.5h.left>=40 && window.5h.left<=40",
            ),
            (
                "window.5h.left!=40",
                "window.5h.left<40 || window.5h.left>40",
            ),
            ("window.7d.left>=101", "window.7d.left>=100"),
            ("window.7d.left<-5", "window.7d.left<0"),
            ("window.5h.left>=lots", "window.5h.left>=40"),
            ("window.5h.left", "window.5h.left>=40"),
            ("ci>=passed", "ci=passed"),
            ("team.stage<Done", "team.stage=Done"),
        ] {
            let error = WhenExpr::parse(&["ci=passed".to_owned(), input.to_owned()])
                .unwrap_err()
                .to_string();
            assert!(error.contains("clause 2, position"), "{input}: {error}");
            assert!(error.ends_with(&format!("use {hint}")), "{input}: {error}");
            assert!(WhenExpr::parse(&[hint.to_owned()]).is_ok(), "{hint}");
        }
        let error = WhenExpr::parse(&["window.5h.left!=40".to_owned()])
            .unwrap_err()
            .to_string();
        assert!(!error.contains("!window"), "{error}");
    }

    #[test]
    fn window_terms_read_percent_left_at_the_boundaries() {
        let scope = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let window = |used, resets_in: i64, span: WindowSpan| RateLimitWindow {
            used_percentage: used,
            resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
            duration_mins: Some(span.minutes()),
            ..RateLimitWindow::default()
        };
        let claude = AgentKind::new_unchecked("claude");
        let codex = AgentKind::new_unchecked("codex");
        let windows = WindowReadings::fixed(
            now,
            BTreeMap::from([
                (
                    LoginKey::default_for(claude.clone()),
                    ProviderCapacity::from_windows(vec![
                        window(Some(60), 3_600, WindowSpan::FiveHour),
                        window(None, 86_400, WindowSpan::SevenDay),
                    ]),
                ),
                (
                    LoginKey::new(claude.clone(), "work".parse().unwrap()),
                    ProviderCapacity::from_windows(vec![window(
                        Some(10),
                        3_600,
                        WindowSpan::FiveHour,
                    )]),
                ),
                (
                    LoginKey::default_for(codex.clone()),
                    ProviderCapacity::from_windows(vec![
                        RateLimitWindow {
                            lifted: true,
                            resets_at: None,
                            ..window(None, 0, WindowSpan::FiveHour)
                        },
                        window(Some(70), -60, WindowSpan::SevenDay),
                    ]),
                ),
            ]),
        );
        let read = |input: &str, provider: Option<&AgentKind>, account: Option<&str>| {
            let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
            let account = account.map(|name| name.parse().unwrap());
            evaluate(
                &expr,
                scope.path(),
                None,
                provider,
                account.as_ref(),
                &windows,
            )
        };
        let check = |input: &str, provider: Option<&AgentKind>| read(input, provider, None);
        for (input, expected) in [
            ("window.5h.left>=40", true),
            ("window.5h.left>40", false),
            ("window.5h.left<=40", true),
            ("window.5h.left<40", false),
            ("window.5h.left>=41", false),
            ("window.7d.left>=0", false),
            ("!window.7d.left>=0", true),
            ("window.7d.left<=100 || window.5h.left>39", true),
        ] {
            assert_eq!(check(input, Some(&claude)).ok, expected, "claude {input}");
        }
        let verdict = check("window.5h.left>=40 && window.7d.left<10", Some(&claude));
        assert_eq!(
            verdict.readings,
            BTreeMap::from([
                ("window.5h.left".to_owned(), Some("40".to_owned())),
                ("window.7d.left".to_owned(), None),
            ])
        );
        let verdict = check("window.5h.left>=100 && window.7d.left>=100", Some(&codex));
        assert!(verdict.ok, "a lifted window and a rolled window are full");
        assert!(!check("window.5h.left>=0", None).ok);
        assert!(!check("window.5h.left>=0", Some(&AgentKind::new_unchecked("pi"))).ok);
        // A row's account reads its own stored window, never the room's.
        for (pin, left) in [
            (Some("default"), Some("40")),
            (Some("work"), Some("90")),
            (Some("idle"), None),
        ] {
            assert_eq!(
                read("window.5h.left>=0", Some(&claude), pin).readings["window.5h.left"].as_deref(),
                left,
                "{pin:?}"
            );
        }
    }

    #[test]
    fn window_readings_read_nothing_until_a_window_term_asks() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(root.path()),
            root.path(),
        )
        .unwrap();
        let windows = WindowReadings::new(Some(&runtime), Timestamp::now());
        let claude = AgentKind::new_unchecked("claude");
        let expr = WhenExpr::parse(&["ci=passed".to_owned()]).unwrap();
        evaluate(&expr, root.path(), None, Some(&claude), None, &windows);
        assert!(windows.capacities.get().is_none());
        let expr = WhenExpr::parse(&["window.5h.left>=40".to_owned()]).unwrap();
        let verdict = evaluate(&expr, root.path(), None, Some(&claude), None, &windows);
        assert_eq!(verdict.readings["window.5h.left"], None);
        assert!(windows.capacities.get().is_some());
    }
}
