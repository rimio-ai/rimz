//! The typed definition `spec::parse` compiles once the authored tables are cleaned and the
//! `@break` step is synthesized: each node's kind, its edges resolved once, its route targets,
//! and every definition string the engine parses, kept as the authored text beside the result
//! of the parser its consumer runs. Compiling never fails: a value admission would refuse loads
//! with its failure, and each consumer keeps the arm it has for that failure.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use super::graph::Edge;
use super::spec::{self, Check, SpecErr, Step, Wake, WorkflowSpec};
use crate::harness::budget::{BudgetParseError, BudgetSpec};
use crate::harness::signal::SignalSelector;
use crate::harness::when::{WhenError, WhenExpr};
use crate::store::workflow::StepLifecycle;
use crate::utils::time::TimeInputErr;

type Subscription = (SignalSelector, BTreeMap<String, String>);

/// The name of the step `workflow.on-break` synthesizes.
pub(super) const BREAK: &str = "@break";

/// A sealed definition as the engine and its readers consume it.
///
/// It dereferences to the authored tables it was compiled from, the bridge for the fields
/// and readers that do not read a typed node yet.
#[derive(Debug)]
pub struct Definition {
    authored: WorkflowSpec,
    checks: BTreeMap<String, Node>,
    steps: BTreeMap<String, Node>,
    stagger: Option<Parsed<Duration, TimeInputErr>>,
}

impl std::ops::Deref for Definition {
    type Target = WorkflowSpec;

    fn deref(&self) -> &WorkflowSpec {
        &self.authored
    }
}

/// Where a route sends the run: an engine outcome, or a node by name. A name that is no node
/// in the definition is still a node target, as the engine reads it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Wait,
    Done,
    Failed,
    User,
    Node(String),
}

impl Target {
    /// Read a target in the literal form the definition, the record and the journal store.
    pub fn parse(text: &str) -> Self {
        match text {
            "@wait" => Self::Wait,
            "@done" => Self::Done,
            "@failed" => Self::Failed,
            "@user" => Self::User,
            node => Self::Node(node.into()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Wait => "@wait",
            Self::Done => "@done",
            Self::Failed => "@failed",
            Self::User => "@user",
            Self::Node(node) => node,
        }
    }

    /// The node the target names, the synthesized `@break` step included.
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::Node(node) => Some(node),
            _ => None,
        }
    }

    /// The authored node the target names: a name the definition's author may write, so never
    /// the synthesized `@break` step.
    pub fn authored_node(&self) -> Option<&str> {
        self.node().filter(|node| !node.starts_with('@'))
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Target {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

impl PartialEq<str> for Target {
    fn eq(&self, text: &str) -> bool {
        self.as_str() == text
    }
}

impl PartialEq<&str> for Target {
    fn eq(&self, text: &&str) -> bool {
        self.as_str() == *text
    }
}

impl Serialize for Target {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A definition string parsed at load: the authored text, which every view prints, beside the
/// result of the parser its consumer runs, error included.
#[derive(Clone, Debug)]
pub struct Parsed<T, E> {
    text: String,
    value: Result<T, E>,
}

impl<T, E> Parsed<T, E> {
    fn new(text: &str, parse: impl FnOnce(&str) -> Result<T, E>) -> Self {
        Self {
            text: text.into(),
            value: parse(text),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn value(&self) -> Result<&T, &E> {
        self.value.as_ref()
    }
}

impl<T, E> Serialize for Parsed<T, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

/// A check or a step.
#[derive(Debug)]
pub struct Node {
    kind: Option<Kind>,
    edges: Vec<Edge>,
    then: Vec<Target>,
    lifecycle: StepLifecycle,
    timeout: Option<Parsed<Duration, TimeInputErr>>,
    window: Option<Parsed<WhenExpr, SpecErr>>,
    wakes: Vec<Subscribed>,
    stagger: Option<Parsed<Duration, TimeInputErr>>,
    budget: Option<Parsed<BudgetSpec, BudgetParseError>>,
}

/// What runs at a node, with the runner's own data. A node that sets no runner, or several,
/// has none.
#[derive(Debug)]
pub enum Kind {
    /// A command check or step: its argv, split as the definition writes it.
    Run(Command),
    Shell(String),
    /// An agent check, under this headless profile.
    Judge(String),
    Condition(Vec<Case>),
    Sleep(Sleep),
    /// An agent step, under this profile.
    Agent(String),
    /// A message step, to this address.
    Message(String),
}

/// A `run` command: its text and its `shlex` split, `None` when the text does not split into a
/// nonempty argv. The words resolve against the deployed directory at execution.
#[derive(Debug)]
pub struct Command {
    text: String,
    words: Option<Vec<String>>,
}

impl Command {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn words(&self) -> Option<&[String]> {
        self.words.as_deref()
    }
}

#[derive(Debug)]
pub struct Sleep {
    duration: Parsed<Duration, TimeInputErr>,
    cases: Vec<Case>,
}

impl Sleep {
    pub fn duration(&self) -> &Parsed<Duration, TimeInputErr> {
        &self.duration
    }
}

/// One condition case, in the order the definition lists it: case indices are durable.
#[derive(Debug)]
pub struct Case {
    pub outcome: String,
    when: Condition,
    hold: Option<Parsed<Duration, TimeInputErr>>,
}

impl Case {
    pub fn when(&self) -> &Condition {
        &self.when
    }

    pub fn hold(&self) -> Option<&Parsed<Duration, TimeInputErr>> {
        self.hold.as_ref()
    }
}

/// A condition's authored clauses beside the expression `WhenExpr::parse` makes of them.
#[derive(Debug)]
pub struct Condition {
    clauses: Vec<String>,
    expression: Result<WhenExpr, WhenError>,
}

impl Condition {
    fn new(clauses: &[String]) -> Self {
        Self {
            clauses: clauses.to_vec(),
            expression: WhenExpr::parse(clauses),
        }
    }

    pub fn clauses(&self) -> &[String] {
        &self.clauses
    }

    pub fn expression(&self) -> Result<&WhenExpr, &WhenError> {
        self.expression.as_ref()
    }
}

/// A check's wake or a step's subscription, parsed under that node's rule.
#[derive(Debug)]
pub struct Subscribed {
    subscription: Parsed<Subscription, SpecErr>,
    prompt: Option<String>,
}

impl Subscribed {
    pub fn subscription(&self) -> &Parsed<Subscription, SpecErr> {
        &self.subscription
    }

    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }
}

impl Node {
    pub fn kind(&self) -> Option<&Kind> {
        self.kind.as_ref()
    }

    pub fn is_sleep(&self) -> bool {
        matches!(self.kind, Some(Kind::Sleep(_)))
    }

    /// The node's condition cases: a condition check's own, or the one a sleep's `when` makes.
    pub fn cases(&self) -> &[Case] {
        match &self.kind {
            Some(Kind::Condition(cases)) => cases,
            Some(Kind::Sleep(sleep)) => &sleep.cases,
            _ => &[],
        }
    }

    /// A step's `then` targets in order, or a sleep's.
    pub fn then(&self) -> &[Target] {
        &self.then
    }

    /// A step's lifecycle, its kind's default when unset; a check's is `turn`.
    pub fn lifecycle(&self) -> StepLifecycle {
        self.lifecycle
    }

    pub fn timeout(&self) -> Option<&Parsed<Duration, TimeInputErr>> {
        self.timeout.as_ref()
    }

    pub fn window(&self) -> Option<&Parsed<WhenExpr, SpecErr>> {
        self.window.as_ref()
    }

    pub fn wakes(&self) -> &[Subscribed] {
        &self.wakes
    }

    /// A step's own start stagger.
    pub fn stagger(&self) -> Option<&Parsed<Duration, TimeInputErr>> {
        self.stagger.as_ref()
    }

    /// A step's per-launch budget.
    pub fn budget(&self) -> Option<&Parsed<BudgetSpec, BudgetParseError>> {
        self.budget.as_ref()
    }

    /// Every edge out of the node, conditional and never-firing ones included.
    pub(super) fn resolved_edges(&self) -> &[Edge] {
        &self.edges
    }
}

impl Definition {
    /// Compile cleaned authored tables, `@break` step included.
    pub(super) fn compile(authored: WorkflowSpec) -> Self {
        let checks = authored
            .checks
            .iter()
            .map(|(name, check)| (name.clone(), check_node(check)))
            .collect();
        let steps = authored
            .steps
            .iter()
            .map(|(name, step)| (name.clone(), step_node(step)))
            .collect();
        let stagger = stagger(authored.workflow.limit.as_ref());
        let mut definition = Self {
            authored,
            checks,
            steps,
            stagger,
        };
        let edges = definition
            .checks
            .keys()
            .chain(definition.steps.keys())
            .map(|name| {
                (
                    name.clone(),
                    definition.compile_edges(name).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        for (name, edges) in edges {
            // A name both tables carry gets the edges the check resolves, as every lookup
            // reads the check first.
            if let Some(node) = definition.checks.get_mut(&name) {
                node.edges = edges.clone();
            }
            if let Some(node) = definition.steps.get_mut(&name) {
                node.edges = edges;
            }
        }
        definition
    }

    pub fn check(&self, name: &str) -> Option<&Node> {
        self.checks.get(name)
    }

    pub fn step(&self, name: &str) -> Option<&Node> {
        self.steps.get(name)
    }

    /// The node by name, a check before a step.
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.check(name).or_else(|| self.step(name))
    }

    /// The step's typed node and its kind, for a step the engine reads only by kind.
    pub(super) fn step_kind(&self, name: &str) -> Option<&Kind> {
        self.step(name)?.kind()
    }

    pub(super) fn check_kind(&self, name: &str) -> Option<&Kind> {
        self.check(name)?.kind()
    }

    /// The workflow-wide start stagger.
    pub fn stagger(&self) -> Option<&Parsed<Duration, TimeInputErr>> {
        self.stagger.as_ref()
    }

    /// Compile authored tables a test built without `parse`, skipping cleanup, `@break`
    /// synthesis and validation as the fixture does.
    /// Change a test fixture's authored form and compile it again, as a load would.
    #[cfg(test)]
    pub(super) fn edit(&mut self, change: impl FnOnce(&mut WorkflowSpec)) {
        let mut authored = self.authored.clone();
        change(&mut authored);
        *self = Self::compile(authored);
    }
}

#[cfg(any(test, feature = "testkit"))]
impl<'de> serde::Deserialize<'de> for Definition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        WorkflowSpec::deserialize(deserializer).map(Self::compile)
    }
}

pub(super) fn duration(text: &str) -> Parsed<Duration, TimeInputErr> {
    Parsed::new(text, crate::harness::duration::duration)
}

fn stagger(
    limit: Option<&crate::harness::throttle::Limit>,
) -> Option<Parsed<Duration, TimeInputErr>> {
    limit?.stagger.as_deref().map(duration)
}

fn window(text: Option<&str>) -> Option<Parsed<WhenExpr, SpecErr>> {
    text.map(|text| Parsed::new(text, spec::parse_window))
}

fn cases(cases: &[spec::Case]) -> Vec<Case> {
    cases
        .iter()
        .map(|case| Case {
            outcome: case.outcome.clone(),
            when: Condition::new(&case.when),
            hold: case.hold.as_deref().map(duration),
        })
        .collect()
}

fn command(text: &str) -> Command {
    Command {
        text: text.into(),
        words: shlex::split(text).filter(|words| !words.is_empty()),
    }
}

fn subscribed(
    wakes: Option<&Vec<Wake>>,
    rule: fn(&Wake) -> Result<Subscription, SpecErr>,
) -> Vec<Subscribed> {
    wakes
        .into_iter()
        .flatten()
        .map(|wake| Subscribed {
            subscription: Parsed {
                text: wake.selector().into(),
                value: rule(wake),
            },
            prompt: wake.prompt().map(str::to_owned),
        })
        .collect()
}

fn check_node(check: &Check) -> Node {
    let kind = check.kind().map(|kind| match kind {
        spec::CheckKind::Run(run) => Kind::Run(command(run)),
        spec::CheckKind::Shell(shell) => Kind::Shell(shell.into()),
        spec::CheckKind::Agent(agent) => Kind::Judge(agent.into()),
        spec::CheckKind::Condition => Kind::Condition(cases(&check.cases)),
        spec::CheckKind::Sleep => Kind::Sleep(Sleep {
            // The kind is a sleep only when `sleep` is set.
            duration: duration(check.sleep.as_deref().unwrap_or_default()),
            cases: cases(&check.condition_cases()),
        }),
    });
    Node {
        kind,
        edges: Vec::new(),
        then: check
            .then
            .as_deref()
            .map(Target::parse)
            .into_iter()
            .collect(),
        lifecycle: StepLifecycle::Turn,
        timeout: check.timeout.as_deref().map(duration),
        window: window(check.window.as_deref()),
        wakes: subscribed(check.wake.as_ref(), Wake::subscription),
        stagger: None,
        budget: None,
    }
}

fn step_node(step: &Step) -> Node {
    let kind = step.kind().map(|kind| match kind {
        spec::StepKind::Agent(agent) => Kind::Agent(agent.into()),
        spec::StepKind::Run(run) => Kind::Run(command(run)),
        spec::StepKind::Shell(shell) => Kind::Shell(shell.into()),
        spec::StepKind::Message(target) => Kind::Message(target.into()),
    });
    Node {
        kind,
        edges: Vec::new(),
        then: step
            .then
            .iter()
            .map(|target| Target::parse(target))
            .collect(),
        lifecycle: step.lifecycle(),
        timeout: step.timeout.as_deref().map(duration),
        window: window(step.window.as_deref()),
        wakes: subscribed(step.subscribe.as_ref(), Wake::step_subscription),
        stagger: stagger(step.limit.as_ref()),
        budget: step
            .budget
            .as_deref()
            .map(|text| Parsed::new(text, str::parse)),
    }
}
