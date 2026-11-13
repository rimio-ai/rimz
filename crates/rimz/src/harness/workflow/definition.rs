//! The typed definition `spec::parse` compiles once the authored tables are cleaned and the
//! `@break` step is synthesized: each node's kind, its edges resolved once, its route targets,
//! and every definition string the engine parses, kept as the authored text beside the result
//! of the parser its consumer runs. Compiling never fails: a value admission would refuse loads
//! with its failure, and each consumer keeps the arm it has for that failure.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;

use super::graph::Edge;
use super::spec::{self, Check, Issue, LaunchOverrides, SpecErr, Step, Wake, WorkflowSpec};
use crate::harness::budget::{BudgetParseError, BudgetSpec};
use crate::harness::signal::SignalSelector;
use crate::harness::signal::default_signal_match_key;
use crate::harness::when::{WhenError, WhenExpr};
use crate::store::workflow::{RunRecord, ScopeKind, StepLifecycle};
use crate::utils::time::TimeInputErr;

type Subscription = (SignalSelector, BTreeMap<String, String>);

/// The name of the step `workflow.on-break` synthesizes.
pub(super) const BREAK: &str = "@break";

/// A sealed definition as the engine and its readers consume it. The authored tables it was
/// compiled from stay inside the workflow module, for admission validation alone.
#[derive(Debug)]
pub struct Definition {
    authored: WorkflowSpec,
    checks: BTreeMap<String, Node>,
    steps: BTreeMap<String, Node>,
    limit: Option<Limit>,
    on_deadline: Option<Target>,
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

impl<T: Clone, E: Clone> Parsed<T, E> {
    /// The parse result, owned, for a reader outside the workflow module.
    pub(in crate::harness) fn result(&self) -> Result<T, E> {
        self.value.clone()
    }
}

impl<T, E> Serialize for Parsed<T, E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

/// A workflow or step `limit` table, its stagger parsed.
#[derive(Clone, Debug)]
pub struct Limit {
    pub max_parallel: Option<u32>,
    pub stagger: Option<Parsed<Duration, TimeInputErr>>,
}

/// A check or a step.
#[derive(Debug)]
pub struct Node {
    kind: Option<Kind>,
    authored: Authored,
    edges: Vec<Edge>,
    then: Vec<Target>,
    lifecycle: StepLifecycle,
    timeout: Option<Parsed<Duration, TimeInputErr>>,
    window: Option<Parsed<WhenExpr, SpecErr>>,
    wakes: Vec<Subscribed>,
    limit: Option<Limit>,
    budget: Option<Parsed<BudgetSpec, BudgetParseError>>,
    fallback: Option<String>,
    /// A check's cases and `when` that no condition or sleep kind took, kept for the views
    /// that print a node with no single runner.
    stray_cases: Vec<Case>,
    stray_when: Option<Condition>,
}

/// The authored table a node was compiled from, read through the node's accessors.
#[derive(Debug)]
enum Authored {
    Check(Box<Check>),
    Step(Box<Step>),
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
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
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

    /// A step's own start limit.
    pub fn limit(&self) -> Option<&Limit> {
        self.limit.as_ref()
    }

    /// A step's per-launch budget.
    pub fn budget(&self) -> Option<&Parsed<BudgetSpec, BudgetParseError>> {
        self.budget.as_ref()
    }

    /// Whether a step's own `on` table writes an edge for `outcome`; a check writes none.
    pub(super) fn routes(&self, outcome: &str) -> bool {
        let Some(step) = self.step_table() else {
            return false;
        };
        match outcome {
            "asked" => step.on.asked.is_some(),
            "cap" => step.on.cap.is_some(),
            "capacity" => step.on.capacity.is_some(),
            "invalid" => step.on.invalid.is_some(),
            "undelivered" => step.on.undelivered.is_some(),
            "failed" => step.on.failed.is_some(),
            _ => false,
        }
    }

    /// Every edge out of the node, conditional and never-firing ones included.
    pub(super) fn resolved_edges(&self) -> &[Edge] {
        &self.edges
    }

    /// A condition answer's outcome when no case holds: a sleep's `sleeping`, else `else`.
    pub(super) fn fallback_outcome(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    fn check_table(&self) -> Option<&Check> {
        match &self.authored {
            Authored::Check(check) => Some(check),
            Authored::Step(_) => None,
        }
    }

    fn step_table(&self) -> Option<&Step> {
        match &self.authored {
            Authored::Step(step) => Some(step),
            Authored::Check(_) => None,
        }
    }

    pub fn summary(&self) -> Option<&str> {
        match &self.authored {
            Authored::Check(check) => check.summary.as_deref(),
            Authored::Step(step) => step.summary.as_deref(),
        }
    }

    pub fn prompt_file(&self) -> Option<&str> {
        match &self.authored {
            Authored::Check(check) => check.prompt_file.as_deref(),
            Authored::Step(step) => step.prompt_file.as_deref(),
        }
    }

    /// The authored `run`, set whether or not it is the node's one runner; so are `shell`,
    /// `agent`, `message` and `sleep`.
    pub fn run(&self) -> Option<&str> {
        match &self.authored {
            Authored::Check(check) => check.run.as_deref(),
            Authored::Step(step) => step.run.as_deref(),
        }
    }

    pub fn shell(&self) -> Option<&str> {
        match &self.authored {
            Authored::Check(check) => check.shell.as_deref(),
            Authored::Step(step) => step.shell.as_deref(),
        }
    }

    pub fn agent(&self) -> Option<&str> {
        match &self.authored {
            Authored::Check(check) => check.agent.as_deref(),
            Authored::Step(step) => step.agent.as_deref(),
        }
    }

    pub fn message(&self) -> Option<&str> {
        self.step_table()?.message.as_deref()
    }

    pub fn sleep(&self) -> Option<&str> {
        self.check_table()?.sleep.as_deref()
    }

    /// A check's `when`, a sleep's own or one no sleep took.
    pub fn when(&self) -> Option<&Condition> {
        match &self.kind {
            Some(Kind::Sleep(sleep)) => sleep.cases.first().map(Case::when),
            _ => self.stray_when.as_ref(),
        }
    }

    /// A check's `for`.
    pub fn hold(&self) -> Option<&str> {
        self.check_table()?.hold.as_deref()
    }

    /// A check's listed cases, a condition's own or ones no condition took.
    pub fn listed_cases(&self) -> &[Case] {
        match &self.kind {
            Some(Kind::Condition(cases)) => cases,
            _ => &self.stray_cases,
        }
    }

    pub fn else_outcome(&self) -> Option<&str> {
        self.check_table()?.else_outcome.as_deref()
    }

    /// A check's provider for window readings.
    pub fn provider(&self) -> Option<&crate::ids::AgentKind> {
        self.check_table()?.provider.as_ref()
    }

    /// The account a check reads windows on, or the one an agent step's launch pins.
    pub fn account(&self) -> Option<&crate::ids::LoginName> {
        match &self.authored {
            Authored::Check(check) => check.account.as_ref(),
            Authored::Step(step) => step.account.as_ref(),
        }
    }

    pub fn keep(&self) -> Option<bool> {
        self.step_table()?.keep
    }

    pub fn notes(&self) -> Option<bool> {
        self.step_table()?.notes
    }

    pub fn takeover(&self) -> Option<bool> {
        self.step_table()?.takeover
    }

    pub fn share(&self) -> Option<bool> {
        self.step_table()?.share
    }

    pub fn throttle(&self) -> Option<crate::config::ThrottleSwitch> {
        self.step_table()?.throttle
    }

    pub fn capacity_wait(&self) -> Option<crate::config::CapacityWait> {
        self.step_table()?.capacity_wait
    }

    pub fn max_visits(&self) -> Option<u64> {
        self.step_table()?.max_visits
    }

    pub fn mode(&self) -> Option<crate::agents::PermissionMode> {
        self.step_table()?.mode
    }

    pub fn effort(&self) -> Option<&str> {
        self.step_table()?.effort.as_deref()
    }

    pub fn fast(&self) -> Option<bool> {
        self.step_table()?.fast
    }

    pub fn system_prompt_file(&self) -> Option<&str> {
        self.step_table()?.system_prompt_file.as_deref()
    }

    pub fn worktree(&self) -> Option<&str> {
        self.step_table()?.worktree.as_deref()
    }

    /// The agent session a message step delivers to.
    pub fn session(&self) -> Option<&crate::ids::AgentSessionId> {
        self.step_table()?.session.as_ref()
    }

    /// A step's `max-visits` raised by the run's grant; a check, or a step without a cap, has
    /// none.
    pub fn visit_cap(&self, grant: u64) -> Option<u64> {
        self.step_table()?.visit_cap(grant)
    }

    /// The visits used and the cap, when the record's current step has spent its cap.
    pub fn tripped_visit_bound(&self, record: &RunRecord) -> Option<(u64, u64)> {
        self.step_table()?.tripped_visit_bound(record)
    }

    /// A step's launch overrides, its system-prompt file resolved inside `dir`, with the budget
    /// the definition parsed; a budget that failed to parse is no budget.
    pub(super) fn launch_overrides(
        &self,
        dir: &std::path::Path,
    ) -> Result<LaunchOverrides, SpecErr> {
        let budget = self.budget().and_then(|budget| budget.value().ok());
        match self.step_table() {
            Some(step) => step.launch_overrides(dir, budget),
            None => Ok(LaunchOverrides::default()),
        }
    }

    /// The signals that wake a check or reach a staying step: a condition or sleep's from its
    /// cases' terms, any other node's from its wakes.
    pub fn subscriptions(&self) -> Result<Vec<Subscription>, SpecErr> {
        if !matches!(self.kind, Some(Kind::Condition(_) | Kind::Sleep(_))) {
            return self
                .wakes
                .iter()
                .map(|wake| wake.subscription.value().cloned().map_err(SpecErr::clone))
                .collect();
        }
        let mut subscriptions = Vec::new();
        for case in self.cases() {
            let expression = case.when.expression().map_err(|error| error.clone())?;
            subscriptions.extend(
                expression
                    .terms()
                    .filter_map(|term| term.wake_subscription()),
            );
        }
        Ok(subscriptions)
    }

    /// Each subscription's selector beside the scope a forward resolves it to, in definition order.
    pub fn subscription_labels(&self) -> Result<Vec<(String, String)>, SpecErr> {
        Ok(self
            .subscriptions()?
            .into_iter()
            .map(|(selector, matches)| {
                let scope = match default_signal_match_key(&selector, &matches) {
                    Some("path") => Some("for this checkout"),
                    Some("instance") if selector.to_string() != "team.stage" => {
                        Some("for this checkout's team")
                    }
                    Some("instance") if !matches.contains_key("board") => {
                        Some("for this checkout's team board")
                    }
                    _ => None,
                };
                let filter = (!matches.is_empty()).then(|| {
                    let pairs = matches
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>();
                    format!("where {}", pairs.join(", "))
                });
                let label = match (scope, filter) {
                    (Some(scope), Some(filter)) => format!("{scope} {filter}"),
                    (Some(scope), None) => scope.into(),
                    (None, Some(filter)) => filter,
                    (None, None) => "anywhere in the room".into(),
                };
                (selector.to_string(), label)
            })
            .collect())
    }

    /// Compile a check a test built alone, as a load compiles each check.
    #[cfg(test)]
    pub(super) fn of_check(check: &Check) -> Self {
        check_node(check)
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
        let limit = limit(authored.workflow.limit.as_ref());
        let on_deadline = authored.workflow.on.deadline.as_deref().map(Target::parse);
        let mut definition = Self {
            authored,
            checks,
            steps,
            limit,
            on_deadline,
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
            // A name both tables carry gets the edges the check resolves on both nodes, as
            // every target lookup reads the check first; `Node::routes` reads the step's own
            // `on` table.
            if let Some(node) = definition.checks.get_mut(&name) {
                node.edges = edges.clone();
            }
            if let Some(node) = definition.steps.get_mut(&name) {
                node.edges = edges;
            }
        }
        definition
    }

    pub fn checks(&self) -> impl Iterator<Item = (&String, &Node)> {
        self.checks.iter()
    }

    pub fn steps(&self) -> impl Iterator<Item = (&String, &Node)> {
        self.steps.iter()
    }

    /// The authored tables, for admission validation and the compile.
    pub(super) fn authored(&self) -> &WorkflowSpec {
        &self.authored
    }

    pub(super) fn is_break(&self, node: &str) -> bool {
        node == BREAK
    }

    /// The fields cleanup ignored, each with its path.
    pub fn ignored(&self) -> &[Issue] {
        &self.authored.ignored
    }

    pub fn summary(&self) -> Option<&str> {
        self.authored.workflow.summary.as_deref()
    }

    pub fn scope(&self) -> ScopeKind {
        self.authored.workflow.scope
    }

    pub fn start(&self) -> &str {
        &self.authored.workflow.start
    }

    /// The header deadline as written; publish resolves it against the clock.
    pub fn deadline(&self) -> Option<&str> {
        self.authored.workflow.deadline.as_deref()
    }

    /// The workflow-wide start limit.
    pub fn limit(&self) -> Option<&Limit> {
        self.limit.as_ref()
    }

    /// The header's `on.deadline` target, when written.
    pub fn on_deadline(&self) -> Option<&Target> {
        self.on_deadline.as_ref()
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

    /// The step's kind, for a step the engine reads only by kind.
    pub(super) fn step_kind(&self, name: &str) -> Option<&Kind> {
        self.step(name)?.kind()
    }

    pub(super) fn check_kind(&self, name: &str) -> Option<&Kind> {
        self.check(name)?.kind()
    }

    /// `Some(empty)` demands checkout facts; `None` means no condition, wake or subscription needs
    /// them.
    pub(crate) fn forge_branches(&self) -> Result<Option<BTreeSet<String>>, SpecErr> {
        let mut branches = None;
        for step in self.steps.values() {
            for (selector, matches) in step.subscriptions()? {
                if default_signal_match_key(&selector, &matches) == Some("path") {
                    branches.get_or_insert_with(BTreeSet::new);
                }
            }
        }
        for check in self.checks.values() {
            if !matches!(check.kind(), Some(Kind::Condition(_) | Kind::Sleep(_))) {
                for wake in &check.wakes {
                    let (selector, matches) = wake.subscription.value().map_err(SpecErr::clone)?;
                    if default_signal_match_key(selector, matches) == Some("path") {
                        branches.get_or_insert_with(BTreeSet::new);
                    }
                }
                continue;
            }
            for case in check.cases() {
                let expression = case.when.expression().map_err(|error| error.clone())?;
                if expression.reads_forge() {
                    branches
                        .get_or_insert_with(BTreeSet::new)
                        .extend(expression.forge_branches().map(str::to_owned));
                }
            }
        }
        Ok(branches)
    }

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

fn limit(limit: Option<&crate::harness::throttle::Limit>) -> Option<Limit> {
    limit.map(|limit| Limit {
        max_parallel: limit.max_parallel,
        stagger: limit.stagger.as_deref().map(duration),
    })
}

pub(super) fn window(text: &str) -> Parsed<WhenExpr, SpecErr> {
    Parsed::new(text, spec::parse_window)
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
    let stray_cases = if check.kind().is_none() {
        cases(&check.cases)
    } else {
        Vec::new()
    };
    let stray_when = (!check.is_sleep())
        .then(|| check.when.as_deref().map(Condition::new))
        .flatten();
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
        window: check.window.as_deref().map(window),
        wakes: subscribed(check.wake.as_ref(), Wake::subscription),
        limit: None,
        budget: None,
        fallback: check.fallback_outcome().map(str::to_owned),
        stray_cases,
        stray_when,
        authored: Authored::Check(Box::new(check.clone())),
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
        window: step.window.as_deref().map(window),
        wakes: subscribed(step.subscribe.as_ref(), Wake::step_subscription),
        limit: limit(step.limit.as_ref()),
        budget: step
            .budget
            .as_deref()
            .map(|text| Parsed::new(text, str::parse)),
        fallback: None,
        stray_cases: Vec::new(),
        stray_when: None,
        authored: Authored::Step(Box::new(step.clone())),
    }
}
