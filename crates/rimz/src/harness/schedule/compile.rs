//! Loop task compiler: the workflow a stored task row runs.
//!
//! An unguarded `wait` row delivers to its session and a `workflow` row names a deployed
//! definition; every other row compiles here to an inline definition: TOML text that the
//! workflow loader accepts, plus the assets it names by confined relative path. Compiling
//! reads no file and writes nothing; [`InlineTask::definition`] reads the assets when a fire
//! materializes the definition, from the same paths a supervised loop fire reads them.

use std::time::Duration;

use anyhow::Result;
use toml::{Table, Value};

use super::catalog::LoadedTask;
use super::runner::{self, CHECK_DEFAULT_TIMEOUT};
use super::{TaskAction, TaskActionErr};
use crate::config::{CheckOn, Judge, MachineConfig, TaskEntry, TaskTarget};

/// What a fire of one task does, compiled from its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompiledAction {
    Deliver(TaskTarget),
    Workflow(TaskWorkflow),
}

/// The workflow a task's fire starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskWorkflow {
    Named(String),
    Inline(InlineTask),
}

/// A task's compiled graph; its assets are read only by [`InlineTask::definition`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTask {
    text: String,
    /// The agent step that takes `loop.default-timeout` when the row sets no timeout.
    timed_step: Option<&'static str>,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Asset {
    Prompt,
    Judge(usize),
    SystemPrompt,
}

impl Asset {
    fn path(&self) -> String {
        match self {
            Self::Prompt => PROMPT.into(),
            Self::Judge(tier) => judge_prompt(*tier),
            Self::SystemPrompt => SYSTEM_PROMPT.into(),
        }
    }
}

/// The task prompt asset, which a fire rewrites with its trigger's evidence.
pub(super) const PROMPT: &str = "prompts/prompt.md";
const SYSTEM_PROMPT: &str = "prompts/system.md";
const AGENT: &str = "agent";
/// A wait with no recheck ends after this long; the workflow bound on a check timeout.
const FOREVER: &str = "365d";

/// The prefix of the node where a judge decline with no `recheck` holds its run; a manual fire
/// supersedes a run waiting there.
pub(super) const DECLINE_HOLD: &str = "hold-";

fn judge_prompt(tier: usize) -> String {
    format!("prompts/judge-{tier}.md")
}

pub(super) fn compile(
    name: &str,
    entry: &TaskEntry,
    action: Result<&TaskAction, &TaskActionErr>,
) -> Result<CompiledAction, TaskActionErr> {
    let act = match action.map_err(Clone::clone)? {
        TaskAction::Workflow(workflow) => {
            return Ok(CompiledAction::Workflow(TaskWorkflow::Named(
                workflow.clone(),
            )));
        }
        TaskAction::Deliver(target) if !guarded(entry) => {
            return Ok(CompiledAction::Deliver(target.clone()));
        }
        TaskAction::Deliver(target) => Act::Deliver(target),
        TaskAction::Spawn(agent) => Act::Agent(agent),
        TaskAction::CheckOnly => Act::CheckOnly,
    };
    Ok(CompiledAction::Workflow(TaskWorkflow::Inline(
        Graph::new(entry).build(name, act),
    )))
}

fn guarded(entry: &TaskEntry) -> bool {
    let guard = entry.guard();
    guard.shell.is_some() || !guard.judges.is_empty()
}

impl InlineTask {
    /// The definition text before a fire fills the default timeout.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Read the assets and fill the default timeout: the definition one fire starts from.
    pub fn definition(
        &self,
        name: &str,
        task: &LoadedTask,
        config: &MachineConfig,
    ) -> Result<crate::harness::workflow::InlineDefinition> {
        let entry = task.entry();
        runner::task_permission_mode(entry)?;
        let mut text = self.text.clone();
        if let Some(step) = self.timed_step {
            let timeout = runner::configured_timeout(config)?
                .unwrap_or(runner::SCHEDULED_RUN_DEFAULT_TIMEOUT);
            let mut table: Table = toml::from_str(&text)?;
            if let Some(Value::Table(step)) = table
                .get_mut("agent")
                .and_then(|agents| agents.get_mut(step))
            {
                step.insert("timeout".into(), seconds(timeout));
            }
            text = toml::to_string(&table)?;
        }
        let mut assets = std::collections::BTreeMap::new();
        for asset in &self.assets {
            let body = match asset {
                Asset::Prompt => runner::resolve_task_prompt(name, entry)?,
                Asset::Judge(tier) => read(&judge_text(entry, *tier, task)?)?,
                Asset::SystemPrompt => {
                    let path = entry
                        .system_prompt_file
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("system-prompt-file is unset"))?;
                    read(&JudgeText::File(runner::resolve_config_path(path)?))?
                }
            };
            assets.insert(asset.path(), body.into_bytes());
        }
        Ok(crate::harness::workflow::InlineDefinition { text, assets })
    }
}

/// Keep the agent step's session when its run ends, as a `--keep` fire asks, on either
/// lifecycle.
pub(super) fn keep_agent(text: &str) -> Result<String> {
    let mut table: Table = toml::from_str(text)?;
    let Some(Value::Table(step)) = table
        .get_mut("agent")
        .and_then(|agents| agents.get_mut(AGENT))
    else {
        return Ok(text.to_owned());
    };
    step.insert("keep".into(), true.into());
    Ok(toml::to_string(&table)?)
}

enum JudgeText {
    Inline(String),
    File(std::path::PathBuf),
}

fn judge_text(entry: &TaskEntry, tier: usize, task: &LoadedTask) -> Result<JudgeText> {
    let judge = entry
        .guard()
        .judges
        .get(tier - 1)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("judge {tier} is gone from the task"))?;
    Ok(match (&judge.prompt, &judge.prompt_file) {
        (Some(prompt), _) => JudgeText::Inline(prompt.clone()),
        (None, Some(path)) => JudgeText::File(judge_prompt_path(path, task)?),
        (None, None) => anyhow::bail!("judge {tier} has no prompt"),
    })
}

/// A judge's prompt file, read from the origin of the task's own prompt file: the machine
/// config directory, or a project task's `.rimz` directory.
pub(super) fn judge_prompt_path(
    path: &std::path::Path,
    task: &LoadedTask,
) -> Result<std::path::PathBuf> {
    if !matches!(task.source(), super::catalog::TaskSource::Project { .. }) {
        return runner::resolve_config_path(path);
    }
    let expanded = runner::expand_tilde(path);
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let config = super::catalog::project_config_path(&task.entry().root);
    Ok(config
        .parent()
        .ok_or_else(|| anyhow::anyhow!("project config has no parent"))?
        .join(expanded))
}

fn read(text: &JudgeText) -> Result<String> {
    match text {
        JudgeText::Inline(text) => Ok(text.clone()),
        JudgeText::File(path) => std::fs::read_to_string(path)
            .map_err(|err| anyhow::anyhow!("reading `{}`: {err}", path.display())),
    }
}

fn seconds(duration: Duration) -> Value {
    Value::from(format!("{}s", duration.as_secs()))
}

enum Act<'a> {
    Agent(&'a str),
    Deliver(&'a TaskTarget),
    CheckOnly,
}

struct Graph<'a> {
    entry: &'a TaskEntry,
    /// The compiled nodes, one table per kind (`agent`, `test`, `condition`, ...).
    nodes: Table,
    assets: Vec<Asset>,
}

impl<'a> Graph<'a> {
    fn new(entry: &'a TaskEntry) -> Self {
        Self {
            entry,
            nodes: Table::new(),
            assets: Vec::new(),
        }
    }

    fn build(mut self, name: &str, act: Act<'_>) -> InlineTask {
        let guard = self.entry.guard();
        let mut guards = guard
            .shell
            .map(|_| "check".to_owned())
            .into_iter()
            .collect::<Vec<_>>();
        guards.extend((1..=guard.judges.len()).map(|tier| format!("judge-{tier}")));
        let act_node = match act {
            Act::Agent(_) => Some(AGENT),
            Act::Deliver(_) => Some("deliver"),
            Act::CheckOnly => None,
        };
        let start = guards
            .first()
            .map_or_else(|| act_node.unwrap_or(AGENT).to_owned(), Clone::clone);
        let polarity = self.entry.on.unwrap_or_default();
        if let Some(shell) = guard.shell {
            let next = guards
                .get(1)
                .map_or(act_node.unwrap_or("@done"), String::as_str);
            let mut check = table([
                ("summary", "the task's shell check".into()),
                ("shell", shell.into()),
                (
                    "timeout",
                    guard
                        .shell_timeout
                        .map_or_else(|| seconds(CHECK_DEFAULT_TIMEOUT), Value::from),
                ),
            ]);
            let mut on = if act_node.is_none() {
                routes([("pass", "@done"), ("fail", "@failed")])
            } else {
                let retry = format!("{}s", super::fire::RESIDENT_RETRY_SECS);
                let decline = self.decline(&start, "check", Some(&retry));
                verdicts(polarity, next, &decline)
            };
            on.insert("timeout".into(), "@failed".into());
            on.insert("invalid".into(), "@failed".into());
            check.insert("on".into(), on.into());
            self.insert("test", "check", check);
        }
        let offset = usize::from(guard.shell.is_some());
        for (index, judge) in guard.judges.iter().enumerate() {
            let tier = index + 1;
            let next = guards
                .get(offset + tier)
                .map_or(act_node.unwrap_or("@done"), String::as_str);
            let guard = format!("judge-{tier}");
            let decline = self.decline(&start, &guard, judge.recheck.as_deref().map(str::trim));
            self.judge(tier, judge, polarity, next, &decline);
        }
        let timed_step = match act {
            Act::Agent(agent) => self.agent(agent),
            Act::Deliver(target) => {
                self.deliver(target);
                None
            }
            Act::CheckOnly => None,
        };
        let workflow = table([
            ("summary", format!("loop task {name}").into()),
            ("scope", "worktree".into()),
            ("start", start.into()),
        ]);
        let mut spec = table([("workflow", workflow.into())]);
        spec.extend(self.nodes);
        InlineTask {
            // A table of strings, booleans, integers, and tables always serializes.
            text: toml::to_string(&spec).expect("compiled definition serializes"),
            timed_step,
            assets: self.assets,
        }
    }

    fn insert(&mut self, kind: &str, name: &str, node: Table) {
        if let Value::Table(nodes) = self
            .nodes
            .entry(kind)
            .or_insert_with(|| Table::new().into())
        {
            nodes.insert(name.into(), node.into());
        }
    }

    fn has(&self, kind: &str, name: &str) -> bool {
        self.nodes
            .get(kind)
            .and_then(Value::as_table)
            .is_some_and(|nodes| nodes.contains_key(name))
    }

    /// Where a declining `guard` goes. A deduping task holds its run and re-enters `first`
    /// once `wait` elapses (a judge's own `recheck`, or the resident retry after a shell
    /// decline), or once its condition stretch ends and a new one begins. A judge with no
    /// `recheck` on a task without `when` holds its run untimed: the live run is the hold, which
    /// a redefinition, `loop remove`, or a manual fire ends.
    fn decline(&mut self, first: &str, guard: &str, wait: Option<&str>) -> String {
        if !self.entry.dedupes_checkout() || wait == Some("0") {
            return "@done".into();
        }
        let Some(when) = &self.entry.when else {
            let (node, sleep) = match wait {
                Some(wait) => (
                    format!("recheck-{guard}"),
                    table([
                        ("summary", format!("wait out the {guard} decline").into()),
                        ("sleep", wait.into()),
                        ("then", first.into()),
                        ("on", routes([("invalid", "@failed")]).into()),
                    ]),
                ),
                None => (
                    format!("{DECLINE_HOLD}{guard}"),
                    table([
                        ("summary", format!("hold the {guard} decline").into()),
                        ("sleep", FOREVER.into()),
                        ("then", "@done".into()),
                        ("on", routes([("invalid", "@failed")]).into()),
                    ]),
                ),
            };
            self.insert("sleep", &node, sleep);
            return node;
        };
        let when = Value::from(
            when.iter()
                .map(String::as_str)
                .map(Value::from)
                .collect::<Vec<_>>(),
        );
        let mut held = table([
            (
                "summary",
                format!("hold the {guard} decline while the condition holds").into(),
            ),
            (
                "cases",
                vec![Value::from(table([
                    ("when", when.clone()),
                    ("outcome", "holds".into()),
                ]))]
                .into(),
            ),
            ("else", "ended".into()),
            ("timeout", wait.unwrap_or(FOREVER).into()),
        ]);
        held.insert(
            "on".into(),
            routes([
                ("holds", "@wait"),
                ("unknown", "@wait"),
                ("ended", "rearm"),
                ("invalid", "@failed"),
                ("timeout", if wait.is_some() { first } else { "@done" }),
            ])
            .into(),
        );
        if let Some(provider) = &self.entry.provider {
            held.insert("provider".into(), provider.as_str().into());
        }
        let node = format!("held-{guard}");
        self.insert("condition", &node, held);
        if !self.has("condition", "rearm") {
            let mut rearm_case = table([("when", when), ("outcome", "holds".into())]);
            if let Some(hold) = &self.entry.hold {
                rearm_case.insert("for".into(), hold.as_str().into());
            }
            let mut rearm = table([
                ("summary", "wait for the condition to hold again".into()),
                ("cases", vec![Value::from(rearm_case)].into()),
                ("else", "ended".into()),
                ("timeout", FOREVER.into()),
                (
                    "on",
                    routes([
                        ("holds", first),
                        ("ended", "@wait"),
                        ("unknown", "@wait"),
                        ("invalid", "@failed"),
                        ("timeout", "@done"),
                    ])
                    .into(),
                ),
            ]);
            if let Some(provider) = &self.entry.provider {
                rearm.insert("provider".into(), provider.as_str().into());
            }
            self.insert("condition", "rearm", rearm);
        }
        node
    }

    fn judge(&mut self, tier: usize, judge: &Judge, polarity: CheckOn, next: &str, decline: &str) {
        let mut on = verdicts(polarity, next, decline);
        // A tier with no answer after its retry runs the agent anyway.
        for outcome in ["invalid", "timeout"] {
            on.insert(outcome.into(), next.into());
        }
        let check = table([
            ("summary", format!("the task's judge {tier}").into()),
            ("agent", judge.agent.as_str().into()),
            ("prompt-file", judge_prompt(tier).into()),
            (
                "timeout",
                judge
                    .timeout
                    .as_deref()
                    .map_or_else(|| seconds(CHECK_DEFAULT_TIMEOUT), Value::from),
            ),
            ("on", on.into()),
        ]);
        self.assets.push(Asset::Judge(tier));
        self.insert("judge", &format!("judge-{tier}"), check);
    }

    fn agent(&mut self, agent: &str) -> Option<&'static str> {
        let entry = self.entry;
        let mut step = table([("agent", agent.into()), ("prompt-file", PROMPT.into())]);
        self.assets.push(Asset::Prompt);
        let text = [
            ("effort", entry.effort.as_deref()),
            ("budget", entry.budget.as_deref()),
            ("worktree", entry.worktree.as_deref()),
            (
                "mode",
                entry
                    .mode
                    .as_deref()
                    .map(str::trim)
                    .filter(|mode| !mode.is_empty()),
            ),
            (
                "account",
                entry.account.as_ref().map(|account| account.as_str()),
            ),
        ];
        for (key, value) in text {
            if let Some(value) = value {
                step.insert(key.into(), value.into());
            }
        }
        for (key, value) in [("fast", entry.fast), ("notes", entry.notes)] {
            if let Some(value) = value {
                step.insert(key.into(), value.into());
            }
        }
        let mut admission = Table::new();
        for (key, value) in [
            ("throttle", entry.throttle.map(Value::try_from)),
            ("give-up", entry.capacity_wait.map(Value::try_from)),
        ] {
            if let Some(Ok(value)) = value {
                admission.insert(key.into(), value);
            }
        }
        if entry.system_prompt_file.is_some() {
            step.insert("system-prompt-file".into(), SYSTEM_PROMPT.into());
            self.assets.push(Asset::SystemPrompt);
        }
        let mut on = routes([("failed", "@failed"), ("invalid", "@failed")]);
        let timed = if entry.stay {
            step.insert("lifecycle".into(), "session".into());
            // A resident session launches beside its checkout's occupants unless it takes them over.
            admission.insert(
                "occupancy".into(),
                if entry.takeover { "takeover" } else { "share" }.into(),
            );
            if !entry.subscribe.is_empty() {
                let wakes = entry.subscribe.iter().map(|binding| {
                    if binding.matches.is_empty() && binding.prompt.is_none() {
                        return Value::from(binding.signal.as_str());
                    }
                    let mut wake = table([("signal", binding.signal.as_str().into())]);
                    if !binding.matches.is_empty() {
                        let matches = binding
                            .matches
                            .iter()
                            .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
                            .collect::<Table>();
                        wake.insert("match".into(), matches.into());
                    }
                    if let Some(prompt) = &binding.prompt {
                        wake.insert("prompt".into(), prompt.as_str().into());
                    }
                    wake.into()
                });
                step.insert("subscribe".into(), wakes.collect::<Vec<_>>().into());
            }
            step.insert("then".into(), "@done".into());
            false
        } else if let Some(verify) = &entry.verify {
            step.insert("lifecycle".into(), "session".into());
            step.insert("rest-check".into(), vec![Value::from("verify")].into());
            step.insert("then".into(), "verify".into());
            let attempts = entry
                .max_attempts
                .unwrap_or(crate::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT);
            step.insert("max-visits".into(), i64::from(attempts).into());
            on.insert("cap".into(), "@failed".into());
            let check = table([
                ("summary", "the task's verify command".into()),
                ("shell", verify.as_str().into()),
                (
                    "timeout",
                    entry
                        .timeout
                        .as_deref()
                        .map_or_else(|| seconds(CHECK_DEFAULT_TIMEOUT), Value::from),
                ),
                (
                    "on",
                    routes([
                        ("pass", "@done"),
                        ("fail", AGENT),
                        ("timeout", "@failed"),
                        ("invalid", "@failed"),
                    ])
                    .into(),
                ),
            ]);
            self.insert("test", "verify", check);
            true
        } else {
            step.insert("lifecycle".into(), "turn".into());
            step.insert("then".into(), "@done".into());
            true
        };
        step.insert("on".into(), on.into());
        if !admission.is_empty() {
            step.insert("admission".into(), admission.into());
        }
        let timed = match (&entry.timeout, timed) {
            (Some(timeout), true) => {
                step.insert("timeout".into(), timeout.as_str().into());
                None
            }
            (None, true) => Some(AGENT),
            (_, false) => None,
        };
        self.insert("agent", AGENT, step);
        timed
    }

    fn deliver(&mut self, target: &TaskTarget) {
        let handle = if target.handle.starts_with('@') {
            target.handle.clone()
        } else {
            format!("@{}", target.handle)
        };
        self.assets.push(Asset::Prompt);
        let step = table([
            ("message", handle.into()),
            ("session", target.session.as_str().into()),
            ("prompt-file", PROMPT.into()),
            ("then", "@done".into()),
            ("on", routes([("undelivered", "@failed")]).into()),
        ]);
        self.insert("message", "deliver", step);
    }
}

/// The guard's verdict routes: the action runs on the verdict `on` names.
fn verdicts(polarity: CheckOn, next: &str, decline: &str) -> Table {
    let (pass, fail) = match polarity {
        CheckOn::Fail => (decline, next),
        CheckOn::Success => (next, decline),
        CheckOn::Any => (next, next),
    };
    routes([("pass", pass), ("fail", fail)])
}

fn routes<'b>(pairs: impl IntoIterator<Item = (&'b str, &'b str)>) -> Table {
    pairs
        .into_iter()
        .map(|(outcome, target)| (outcome.to_owned(), Value::from(target)))
        .collect()
}

fn table<'b>(pairs: impl IntoIterator<Item = (&'b str, Value)>) -> Table {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

#[cfg(test)]
mod tests;
