//! State predicates for clock-evaluated loop conditions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::forge::pr_state::PrStateCache;
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

pub fn evaluate(expr: &WhenExpr, scope: &Path, ci_source: Option<&CiSource>) -> Verdict {
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
                // WhenExpr can only be constructed through the key-validating parser.
                _ => unreachable!("the parser admits only team.stage and ci"),
            });
    }
    Verdict {
        ok: expr.0.matches(&readings),
        readings,
    }
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
    pub values: Vec<String>,
}

impl WhenTerm {
    pub fn matches(&self, reading: Option<&str>) -> bool {
        reading.is_some_and(|reading| self.values.iter().any(|value| value == reading))
    }
}

impl fmt::Display for WhenTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.key, self.values.join(","))
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
            if self.take("!=") {
                return Err(self.error(format!(
                    "!= is not supported; use !{key}={}",
                    self.rest().trim()
                )));
            }
            if key != "team.stage" && key != "ci" {
                return Err(self.error("unknown key; keys: team.stage, ci"));
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
            Node::Term(WhenTerm { key, values })
        };
        self.depth -= 1;
        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let verdict = evaluate(&expr, &entry.run_dir(), Some(&cache));
                assert!(verdict.ok, "{state:?} {ci:?}");
                assert_eq!(verdict.readings["ci"].as_deref(), Some(expected));
                assert!(!evaluate(&expr, root.path(), Some(&cache)).ok);
                assert!(!evaluate(&expr, &entry.run_dir(), None).ok);
            }
        }
        let expr = WhenExpr::parse(&["ci=passed".to_owned()]).unwrap();
        assert_eq!(
            evaluate(
                &expr,
                &entry.run_dir(),
                Some(&CiSource(PrStateCache::default()))
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
            assert_eq!(evaluate(&expr, scope.path(), None).ok, expected, "{input}");
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
                evaluate(&expr, scope.path(), Some(&cache)).ok,
                expected,
                "{input}"
            );
        }
        let expr = WhenExpr::parse(&["team.stage=Done && ci=pending".to_owned()]).unwrap();
        assert_eq!(
            evaluate(&expr, scope.path(), None).readings,
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
            ("agent=idle", "team.stage, ci"),
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
}
