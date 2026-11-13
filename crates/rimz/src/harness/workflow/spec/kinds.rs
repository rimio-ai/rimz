//! The authored definition file: one strict table per node kind, each denying every field the
//! kind does not take, lowered into the checks and steps the definition compiles.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_path_to_error::Segment;

use super::{
    Case, Check, CheckTable, Header, OccupancyPolicy, SpecErr, Step, StepEdges, StepTable, Then,
    Wake, WorkflowSpec, nonempty_cases, then_checks,
};
use crate::harness::throttle::Limit;
use crate::store::workflow::StepLifecycle;

/// Every node kind, by the table word a definition writes it under.
pub(super) const KINDS: [&str; 8] = [
    "agent",
    "command",
    "message",
    "test",
    "script",
    "judge",
    "condition",
    "sleep",
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct WorkflowFile {
    workflow: Header,
    #[serde(default)]
    agent: BTreeMap<String, AgentNode>,
    #[serde(default)]
    command: BTreeMap<String, CommandNode>,
    #[serde(default)]
    message: BTreeMap<String, MessageNode>,
    #[serde(default)]
    test: BTreeMap<String, TestNode>,
    #[serde(default)]
    script: BTreeMap<String, ScriptNode>,
    #[serde(default)]
    judge: BTreeMap<String, JudgeNode>,
    #[serde(default)]
    condition: BTreeMap<String, ConditionNode>,
    #[serde(default)]
    sleep: BTreeMap<String, SleepNode>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct AgentNode {
    summary: Option<String>,
    agent: String,
    lifecycle: Option<StepLifecycle>,
    #[serde(default, deserialize_with = "then_checks")]
    #[schemars(required, with = "Then", !default)]
    then: Vec<String>,
    prompt_file: Option<String>,
    keep: Option<bool>,
    notes: Option<bool>,
    subscribe: Option<Vec<Wake>>,
    #[serde(default)]
    on: AgentEdges,
    #[schemars(range(min = 1))]
    max_visits: Option<u64>,
    timeout: Option<String>,
    #[serde(default)]
    admission: AgentAdmission,
    #[schemars(with = "Option<String>")]
    account: Option<crate::ids::LoginName>,
    #[schemars(with = "Option<String>")]
    mode: Option<crate::agents::PermissionMode>,
    effort: Option<String>,
    fast: Option<bool>,
    budget: Option<String>,
    system_prompt_file: Option<String>,
    worktree: Option<String>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct AgentEdges {
    asked: Option<String>,
    cap: Option<String>,
    capacity: Option<String>,
    invalid: Option<String>,
    /// An ending that died, timed out, or was limited; without it, `then` takes it.
    failed: Option<String>,
}

/// What admits an agent node's start: its usage window, its checkout's occupants, its own
/// start limit, the machine throttle, and how long it waits for capacity.
#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct AgentAdmission {
    window: Option<String>,
    occupancy: Option<OccupancyPolicy>,
    #[schemars(range(min = 1))]
    max_parallel: Option<u32>,
    stagger: Option<String>,
    #[schemars(with = "Option<String>")]
    throttle: Option<crate::config::ThrottleSwitch>,
    #[serde(default, deserialize_with = "give_up")]
    #[schemars(with = "Option<String>")]
    give_up: Option<crate::config::CapacityWait>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct JudgeAdmission {
    window: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct CommandNode {
    summary: Option<String>,
    run: Option<String>,
    shell: Option<String>,
    timeout: Option<String>,
    then: Option<String>,
    #[serde(default)]
    on: CapEdges,
    #[schemars(range(min = 1))]
    max_visits: Option<u64>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct CapEdges {
    cap: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct MessageNode {
    summary: Option<String>,
    message: String,
    then: Option<String>,
    prompt_file: Option<String>,
    #[schemars(with = "Option<String>")]
    session: Option<crate::ids::AgentSessionId>,
    #[serde(default)]
    on: MessageEdges,
    #[schemars(range(min = 1))]
    max_visits: Option<u64>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct MessageEdges {
    cap: Option<String>,
    undelivered: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct TestNode {
    summary: Option<String>,
    run: Option<String>,
    shell: Option<String>,
    timeout: Option<String>,
    wake: Option<Vec<Wake>>,
    #[serde(default)]
    on: TestEdges,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct TestEdges {
    pass: Option<String>,
    fail: Option<String>,
    timeout: Option<String>,
    invalid: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct ScriptNode {
    summary: Option<String>,
    run: Option<String>,
    shell: Option<String>,
    timeout: Option<String>,
    wake: Option<Vec<Wake>>,
    #[serde(default)]
    on: BTreeMap<String, String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct JudgeNode {
    summary: Option<String>,
    agent: String,
    prompt_file: Option<String>,
    timeout: Option<String>,
    wake: Option<Vec<Wake>>,
    #[serde(default)]
    on: BTreeMap<String, String>,
    #[serde(default)]
    admission: JudgeAdmission,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct ConditionNode {
    summary: Option<String>,
    #[serde(deserialize_with = "nonempty_cases")]
    #[schemars(length(min = 1))]
    cases: Vec<Case>,
    #[serde(rename = "else")]
    else_outcome: Option<String>,
    timeout: Option<String>,
    #[serde(default)]
    on: BTreeMap<String, String>,
    #[schemars(with = "Option<String>")]
    provider: Option<crate::ids::AgentKind>,
    #[schemars(with = "Option<String>")]
    account: Option<crate::ids::LoginName>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct SleepNode {
    summary: Option<String>,
    sleep: String,
    then: Option<String>,
    when: Option<Vec<String>>,
    #[serde(rename = "for")]
    hold: Option<String>,
    #[schemars(with = "Option<String>")]
    provider: Option<crate::ids::AgentKind>,
    #[schemars(with = "Option<String>")]
    account: Option<crate::ids::LoginName>,
    #[serde(default)]
    on: SleepEdges,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct SleepEdges {
    invalid: Option<String>,
}

fn give_up<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::config::CapacityWait>, D::Error> {
    let raw = String::deserialize(deserializer)?;
    raw.parse().map(Some).map_err(|error| match error {
        crate::config::CapacityWaitErr::Zero => {
            serde::de::Error::custom("give-up must be greater than zero, or forever")
        }
        error => serde::de::Error::custom(error),
    })
}

/// Deserialize the file, an error naming the dotted path of the field it is about.
pub(super) fn deserialize(value: toml::Value) -> Result<WorkflowFile, SpecErr> {
    serde_path_to_error::deserialize::<_, WorkflowFile>(value).map_err(|error| {
        let mut path = error
            .path()
            .iter()
            .filter_map(|segment| match segment {
                Segment::Map { key } => Some(key.clone()),
                Segment::Seq { index } => Some(index.to_string()),
                Segment::Enum { variant } => Some(variant.clone()),
                Segment::Unknown => None,
            })
            .collect::<Vec<_>>();
        let source = error.into_inner();
        let message = source.message();
        // serde reports an unknown key at the table that holds it; name the key itself.
        if let Some(key) = message
            .strip_prefix("unknown field `")
            .and_then(|rest| rest.split_once('`'))
            .map(|(key, _)| key)
            .filter(|key| path.last().is_none_or(|last| last != key))
        {
            path.push(key.into());
        }
        // A required field is missing from the table the path names.
        if let Some(key) = message
            .strip_prefix("missing field `")
            .and_then(|rest| rest.split_once('`'))
            .map(|(key, _)| key)
        {
            path.push(key.into());
        }
        SpecErr::Toml {
            path: if path.is_empty() {
                "workflow".into()
            } else {
                path.join(".")
            },
            source,
        }
    })
}

fn optional_limit(max_parallel: Option<u32>, stagger: Option<String>) -> Option<Limit> {
    (max_parallel.is_some() || stagger.is_some()).then_some(Limit {
        max_parallel,
        stagger,
    })
}

fn routes(
    pairs: impl IntoIterator<Item = (&'static str, Option<String>)>,
) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .filter_map(|(outcome, target)| Some((outcome.to_owned(), target?)))
        .collect()
}

fn check(table: CheckTable) -> Check {
    Check {
        table,
        summary: None,
        run: None,
        shell: None,
        agent: None,
        sleep: None,
        when: None,
        hold: None,
        then: None,
        prompt_file: None,
        window: None,
        wake: None,
        cases: Vec::new(),
        else_outcome: None,
        provider: None,
        account: None,
        on: BTreeMap::new(),
        timeout: None,
    }
}

fn step(table: StepTable) -> Step {
    Step {
        table,
        summary: None,
        agent: None,
        run: None,
        shell: None,
        message: None,
        lifecycle: None,
        keep: None,
        notes: None,
        prompt_file: None,
        window: None,
        occupancy: None,
        subscribe: None,
        throttle: None,
        capacity_wait: None,
        limit: None,
        then: Vec::new(),
        on: StepEdges::default(),
        max_visits: None,
        timeout: None,
        account: None,
        mode: None,
        effort: None,
        fast: None,
        budget: None,
        system_prompt_file: None,
        worktree: None,
        session: None,
    }
}

impl WorkflowFile {
    /// Lower every kind table into the checks and steps the definition compiles, refusing a
    /// name two kinds share.
    pub(in super::super) fn lower(self) -> Result<WorkflowSpec, SpecErr> {
        let mut spec = WorkflowSpec {
            workflow: self.workflow,
            checks: BTreeMap::new(),
            steps: BTreeMap::new(),
        };
        let mut seen = BTreeMap::<String, &'static str>::new();
        let mut claim = |kind: &'static str, name: &str| match seen.insert(name.into(), kind) {
            Some(other) => Err(SpecErr::DuplicateNode {
                path: format!("{kind}.{name}"),
                name: name.into(),
                other,
            }),
            None => Ok(()),
        };
        for (name, node) in self.agent {
            claim("agent", &name)?;
            let admission = node.admission;
            spec.steps.insert(
                name,
                Step {
                    summary: node.summary,
                    agent: Some(node.agent),
                    lifecycle: node.lifecycle,
                    then: node.then,
                    prompt_file: node.prompt_file,
                    keep: node.keep,
                    notes: node.notes,
                    subscribe: node.subscribe,
                    on: StepEdges {
                        asked: node.on.asked,
                        cap: node.on.cap,
                        capacity: node.on.capacity,
                        invalid: node.on.invalid,
                        failed: node.on.failed,
                        undelivered: None,
                    },
                    max_visits: node.max_visits,
                    timeout: node.timeout,
                    window: admission.window,
                    occupancy: admission.occupancy,
                    limit: optional_limit(admission.max_parallel, admission.stagger),
                    throttle: admission.throttle,
                    capacity_wait: admission.give_up,
                    account: node.account,
                    mode: node.mode,
                    effort: node.effort,
                    fast: node.fast,
                    budget: node.budget,
                    system_prompt_file: node.system_prompt_file,
                    worktree: node.worktree,
                    ..step(StepTable::Agent)
                },
            );
        }
        for (name, node) in self.command {
            claim("command", &name)?;
            spec.steps.insert(
                name,
                Step {
                    summary: node.summary,
                    run: node.run,
                    shell: node.shell,
                    timeout: node.timeout,
                    then: node.then.into_iter().collect(),
                    on: StepEdges {
                        cap: node.on.cap,
                        ..StepEdges::default()
                    },
                    max_visits: node.max_visits,
                    ..step(StepTable::Command)
                },
            );
        }
        for (name, node) in self.message {
            claim("message", &name)?;
            spec.steps.insert(
                name,
                Step {
                    summary: node.summary,
                    message: Some(node.message),
                    then: node.then.into_iter().collect(),
                    prompt_file: node.prompt_file,
                    session: node.session,
                    on: StepEdges {
                        cap: node.on.cap,
                        undelivered: node.on.undelivered,
                        ..StepEdges::default()
                    },
                    max_visits: node.max_visits,
                    ..step(StepTable::Message)
                },
            );
        }
        for (name, node) in self.test {
            claim("test", &name)?;
            spec.checks.insert(
                name,
                Check {
                    summary: node.summary,
                    run: node.run,
                    shell: node.shell,
                    timeout: node.timeout,
                    wake: node.wake,
                    on: routes([
                        ("pass", node.on.pass),
                        ("fail", node.on.fail),
                        ("timeout", node.on.timeout),
                        ("invalid", node.on.invalid),
                    ]),
                    ..check(CheckTable::Test)
                },
            );
        }
        for (name, node) in self.script {
            claim("script", &name)?;
            spec.checks.insert(
                name,
                Check {
                    summary: node.summary,
                    run: node.run,
                    shell: node.shell,
                    timeout: node.timeout,
                    wake: node.wake,
                    on: node.on,
                    ..check(CheckTable::Script)
                },
            );
        }
        for (name, node) in self.judge {
            claim("judge", &name)?;
            spec.checks.insert(
                name,
                Check {
                    summary: node.summary,
                    agent: Some(node.agent),
                    prompt_file: node.prompt_file,
                    timeout: node.timeout,
                    wake: node.wake,
                    on: node.on,
                    window: node.admission.window,
                    ..check(CheckTable::Judge)
                },
            );
        }
        for (name, node) in self.condition {
            claim("condition", &name)?;
            spec.checks.insert(
                name,
                Check {
                    summary: node.summary,
                    cases: node.cases,
                    else_outcome: node.else_outcome,
                    timeout: node.timeout,
                    on: node.on,
                    provider: node.provider,
                    account: node.account,
                    ..check(CheckTable::Condition)
                },
            );
        }
        for (name, node) in self.sleep {
            claim("sleep", &name)?;
            spec.checks.insert(
                name,
                Check {
                    summary: node.summary,
                    sleep: Some(node.sleep),
                    then: node.then,
                    when: node.when,
                    hold: node.hold,
                    provider: node.provider,
                    account: node.account,
                    on: routes([("invalid", node.on.invalid)]),
                    ..check(CheckTable::Sleep)
                },
            );
        }
        Ok(spec)
    }
}
