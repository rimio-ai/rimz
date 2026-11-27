//! The seven blocks a workflow script calls: one Rust type, one schema, and one parser per
//! block, each block's result, and the one failure a call that did not end as asked gives in
//! place of a result. Beside them, the two field grammars a script also reads by hand: a
//! duration (`ms`) and a condition's truth over readings (`reads`).
//!
//! The parser knows its caller: a script awaits its calls, so the fields that tell RimZ when to
//! wake a calling agent are refused there. A field grammar this module cannot reach without
//! the loop scheduler (a `schedule` block's `cron` value) is checked by the booking.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::harness::when::WhenExpr;
use crate::utils::time::CalendarAt;

mod template;
mod typescript;

pub use template::template;
pub use typescript::typescript;

/// Who made a call: an agent through its MCP tool, or a workflow script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    Tool,
    Script,
}

/// The name of a block, which is also the name of its script function and, for every block but
/// `now`, of its tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BlockName {
    Agent,
    Team,
    Shell,
    Watch,
    Workflow,
    Schedule,
    Now,
}

impl BlockName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Shell => "shell",
            Self::Watch => "watch",
            Self::Workflow => "workflow",
            Self::Schedule => "schedule",
            Self::Now => "now",
        }
    }
}

/// One block: its object, the fields a script may not pass, and its result.
pub(crate) trait Block: DeserializeOwned + JsonSchema {
    const NAME: BlockName;
    /// Fields that tell RimZ when to wake a calling agent, or that belong to a script's own run;
    /// a script awaits its calls, so the parser refuses these from one.
    const SCRIPT_REFUSED: &'static [&'static str] = &[];
    /// Values of a field the block's `refusal` always refuses from a script, as (field, value).
    const SCRIPT_REFUSED_VALUES: &'static [(&'static str, &'static str)] = &[];
    /// A block with no fields: a script calls it bare, as `now()`, and its call's object is `{}`.
    const BARE: bool = false;
    /// What an awaited call gives.
    type Result: JsonSchema;

    /// Why an object that fits the block's shape still cannot run.
    fn refusal(&self, caller: Caller) -> Option<String>;
}

/// Parse a block's object for `caller`: the typed block, or the sentence that refuses it.
pub(crate) fn parse<B: Block>(object: Value, caller: Caller) -> Result<B, String> {
    let Value::Object(fields) = &object else {
        return Err(format!(
            "a `{}` block is one object of named fields",
            B::NAME.as_str()
        ));
    };
    if caller == Caller::Script
        && let Some(field) = B::SCRIPT_REFUSED
            .iter()
            .find(|field| fields.get(**field).is_some_and(|value| !value.is_null()))
    {
        return Err(format!(
            "a script awaits its calls and owns its run, so its `{}` does not take `{field}`",
            B::NAME.as_str()
        ));
    }
    let block = serde_json::from_value::<B>(object).map_err(|error| error.to_string())?;
    match block.refusal(caller) {
        Some(reason) => Err(reason),
        None => Ok(block),
    }
}

/// Lowercase hex SHA-256 over the block's name, a newline, and the canonical JSON of the
/// object it was called with (keys sorted at every depth), so a call's digest covers everything
/// it passed, a prompt's text included.
pub(in crate::harness) fn digest(block: BlockName, object: &Value) -> String {
    let mut text = format!("{}\n", block.as_str());
    write_canonical(&mut text, object);
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn write_canonical(out: &mut String, value: &Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(out, value);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The draft-07 schema of `T`, inlined, with an optional field's `null` dropped: a caller
/// omits a field rather than passing `null`. A value that holds `null` (a map's) keeps it.
/// Every MCP tool's input schema is made here too, a tool's arguments that are no block's
/// included.
pub(crate) fn schema<T: JsonSchema>() -> Value {
    let mut schema = schemars::generate::SchemaSettings::draft07()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    if let Some(root) = schema.as_object_mut() {
        root.remove("title");
    }
    drop_null(&mut schema);
    schema
}

fn drop_null(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(fields)) = map.get_mut("properties") {
                fields.values_mut().for_each(drop_field_null);
            }
            map.values_mut().for_each(drop_null);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_null),
        _ => {}
    }
}

fn drop_field_null(field: &mut Value) {
    let Value::Object(map) = field else {
        return;
    };
    if let Some(Value::Array(types)) = map.get_mut("type") {
        types.retain(|kind| kind != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            map.insert("type".to_owned(), only);
        }
    }
    if let Some(Value::Array(values)) = map.get_mut("enum") {
        values.retain(|value| !value.is_null());
    }
}

/// Launches one agent: one seat, never a team. Without a place (`worktree`, `from_pr`,
/// `cwd`) it works in its caller's checkout. A script's call resolves when a staying agent's
/// session has closed, or when a `stay: false` agent's turn has ended.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Agent profile (from `rimz agents profiles`) or bare model; one seat, never a team.
    pub profile: String,
    /// The brief: the agent's first turn.
    pub prompt: String,
    /// Label for `tasks` and the sidebar, 3 to 5 words.
    pub description: Option<String>,
    /// Distinct handle when the channel already holds this profile's, the caller's own included.
    pub name: Option<String>,
    /// Inherit this profile or provider kind below the launched profile; omit unless the user
    /// asks.
    pub agent: Option<String>,
    /// Kebab-case worktree name (at most two words); at most one of `worktree`, `from_pr`, and
    /// `cwd`. A new one branches from `agents.worktree.base` when set, else from the `HEAD` of the
    /// main checkout; a `team.role` branches as its team does.
    pub worktree: Option<String>,
    /// `worktree` only: start a new session in a closed worktree (otherwise refused).
    #[serde(default)]
    pub fresh: bool,
    /// Worktree for this open pull request, by number or URL.
    pub from_pr: Option<String>,
    /// Existing directory to work in.
    pub cwd: Option<PathBuf>,
    /// `true` (the default): the agent stays in the room after its first turn until it is
    /// closed. `false`: one task, and it closes once its turn ends and its report is out.
    #[serde(default = "stays")]
    pub stay: bool,
    /// JSON Schema the agent's answer must fit, over `type`, `required`, `properties`,
    /// `enum`, and `items` only. The agent is told to hand it in with `rimz submit`, and the
    /// answer stands in for its last message as its report and the call's result.
    pub schema: Option<Map<String, Value>>,
    /// Stop the agent this long after its launch (`90m`, `2h`, `3d`), staying or not: its
    /// report, and a script's call, read timed out. None by default.
    pub timeout: Option<String>,
}

fn stays() -> bool {
    true
}

/// What an agent's call gives without a `schema`; with one, the answer it handed in.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentResult {
    /// The agent's last message.
    pub report: String,
    /// The agent's handle.
    pub handle: String,
}

/// Launches a configured team on a brief, which goes to its leader; the call resolves the first
/// time the team's board is done.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Team {
    /// Configured team to launch (from `rimz teams list`).
    pub team: String,
    /// The brief, delivered to the team's leader.
    pub prompt: String,
    /// Kebab-case worktree name (at most two words); give exactly one of `worktree` and
    /// `from_pr`. A new one branches from `agents.worktree.base` when set, else from origin's
    /// default branch fetched at launch (or `HEAD` with no origin).
    pub worktree: Option<String>,
    /// Open pull request, by number or URL, to create or reuse a worktree for.
    pub from_pr: Option<String>,
    /// Label each seat carries in the sidebar, a few words; `tasks` shows the team's goal.
    pub description: Option<String>,
    /// Stop every seat this launch opened this long after it (`90m`, `2h`, `3d`): its
    /// report, and a script's call, read timed out. None by default.
    pub timeout: Option<String>,
}

/// What a team's call gives.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TeamResult {
    /// The leader's last message.
    pub report: String,
}

/// Which ending of a command wakes the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum On {
    Fail,
    Success,
}

/// Runs one command; a nonzero exit fails the call with cause `exit`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shell {
    /// The whole command as one string, run once through POSIX `sh -c` at the checkout's root
    /// with stdin closed, stdout and stderr going to the shell's output file.
    pub command: String,
    /// What you will do on waking, 1 to 60 characters.
    pub label: Option<String>,
    /// A JSON object the command reads as `input` from the `context.json` in the directory
    /// `RIMZ_WORKFLOW_DIR` names to it.
    pub input: Option<Map<String, Value>>,
    /// Wake only on this outcome (default either; `fail` for a server left running).
    pub on: Option<On>,
    /// One still-running notice after this long, whatever `on` says (never a stop); 30m when
    /// absent, or none while cache keepalive pings you. Set one longer than the command should
    /// take when it could hang.
    pub check_in: Option<String>,
    /// Checkout to run in; defaults to the caller's (an agent's session start directory).
    pub cwd: Option<PathBuf>,
    /// Kill the command and every process it started after this long (`90m`, `2h`, `3d`):
    /// its outcome reads timed out, and a script's call fails with `timeout`. None by default.
    pub timeout: Option<String>,
}

/// What a command's call gives.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellResult {
    /// The exit code, `0`.
    pub code: i32,
    /// What the command printed, up to a cap.
    pub stdout: String,
    /// The path of the log that holds everything it printed.
    pub output: String,
}

/// Waits until a condition reads true, already true when called included.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    /// A condition, firing once it reads true, already true when set included.
    pub when: String,
    /// What you will do on waking, 1 to 60 characters; the wake carries only what fired.
    pub label: Option<String>,
    /// One still-not-met notice after this long (never a stop); 30m when absent, or none while
    /// cache keepalive pings you.
    pub check_in: Option<String>,
    /// Checkout to arm in, whose branch bare forge terms read; defaults to the caller's (an
    /// agent's session start directory).
    pub cwd: Option<PathBuf>,
    /// Stop watching after this long (`90m`, `2h`, `3d`): one wake names the timeout, and a
    /// script's call fails with `timeout`. None by default.
    pub timeout: Option<String>,
}

/// What a watch's call gives.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WatchResult {
    /// The reading of each key the condition names, at the look that read it true: `null` for a
    /// key that look had not read.
    pub readings: BTreeMap<String, Option<String>>,
}

/// What a workflow's `value` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(in crate::harness) enum WorkflowSource {
    Name,
    Path,
    Script,
    Run,
}

/// Runs a workflow; the call gives what its script returned, and fails with what it threw.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    dead_code,
    reason = "the tools and the run steps of later changes read the fields no refusal reads"
)]
pub(in crate::harness) struct Workflow {
    /// What `value` is: an installed workflow's `name`, a workflow directory or file's
    /// `path`, a `script`'s text, or an ended `run`'s id to run again.
    pub source: WorkflowSource,
    /// The workflow's name, path, script text, or run id.
    pub value: String,
    /// Any JSON value, which the script reads as the global `args`.
    pub args: Option<Value>,
    /// RimZ-owned worktree to run in, created when missing.
    pub worktree: Option<String>,
    /// How long the run may take (`8h`, `5d`); none by default.
    pub timeout: Option<String>,
    /// The run's launch cap.
    pub max_launches: Option<u32>,
    /// When to give up capacity waits.
    pub give_up: Option<String>,
    /// Launch agent steps past every capacity hold.
    #[serde(default)]
    pub skip_capacity: bool,
    /// Send no report when the run ends and do not hold the calling session open.
    #[serde(default)]
    pub detach: bool,
}

/// What fires a booking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(in crate::harness) enum ScheduleTrigger {
    At,
    In,
    Every,
    Cron,
    Signal,
    When,
}

/// The block a booking launches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(in crate::harness) enum ScheduleTool {
    Agent,
    Team,
    Workflow,
}

/// What a repeating booking does about a trigger during a live fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(in crate::harness) enum ScheduleOverlap {
    Skip,
    Coalesce,
    Queue,
    Replace,
    Allow,
}

/// Books a launch for later; the call gives the booking's id at once, and nothing follows.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    dead_code,
    reason = "the tools and the run steps of later changes read the fields no refusal reads"
)]
pub(in crate::harness) struct Schedule {
    /// Booking name; reusing a name you hold replaces it.
    pub name: String,
    /// What fires the booking.
    pub trigger: ScheduleTrigger,
    /// The trigger's argument: a clock time, delay, interval, cron expression, signal name, or
    /// condition.
    pub value: String,
    /// The block each fire launches.
    pub tool: ScheduleTool,
    /// That block's own object, without `worktree` (pass top-level `worktree`).
    pub arguments: Map<String, Value>,
    /// `signal` only: payload fields that must equal these values (`*` matches any value).
    #[serde(rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    /// `when` only: how long the condition must hold before firing.
    #[serde(rename = "for")]
    pub hold: Option<String>,
    /// `signal` or `when` only: fire on every match rather than once (default once).
    #[serde(default)]
    pub repeat: bool,
    /// RimZ-owned worktree for every fire, created at the first fire when missing.
    pub worktree: Option<String>,
    /// Repeating triggers only: what a trigger during a live fire does (`skip` default).
    pub overlap: Option<ScheduleOverlap>,
    /// Disable after this many failed fires in a row (default `3`; `0` never).
    pub max_strikes: Option<u32>,
}

/// What a booking's call gives.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct ScheduleResult {
    /// The booking's id.
    pub id: String,
}

/// The current time, as Unix epoch milliseconds: RimZ reads it and the run's log keeps it as the
/// call's result, so a replay and a resume read the time the first pass read.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct Now {}

/// Why a call did not end as asked, or why a run ended without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Cause {
    // A command's nonzero exit.
    Exit,
    // An agent or a team gone with its task unfinished.
    Died,
    Timeout,
    // A launch whose wait for headroom was given up.
    Capacity,
    // An agent whose answers never fit its schema, a call whose object does not fit its block,
    // or a script over a cap.
    Invalid,
    // A replayed call whose block or object differs from the one its log answers.
    Replay,
    // A script that can run no further while no call of it is open.
    Stuck,
}

/// A call that did not end as asked, given in place of its result. In a script every block's
/// failure, a refused object's included, arrives one way: the awaited call throws an `Error`
/// whose own fields are these, so a script tells one failure from another by `cause`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    /// The block whose call failed; none for a run that ended without a call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockName>,
    /// `exit`, `died`, `timeout`, `capacity`, or `invalid` for a call; `replay`, `stuck`,
    /// or `invalid` (a cap) for a run that ended without one.
    pub cause: Cause,
    /// What happened, in a sentence.
    pub reason: String,
    /// The call's number in the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    /// A command's exit code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// What a command printed, up to a cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// The path of a command's log.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// An agent's handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Died => "died",
            Self::Timeout => "timeout",
            Self::Capacity => "capacity",
            Self::Invalid => "invalid",
            Self::Replay => "replay",
            Self::Stuck => "stuck",
        }
    }
}

impl Block for Agent {
    const NAME: BlockName = BlockName::Agent;
    type Result = AgentResult;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        if let Some(blank) = blank_field(&[("profile", &self.profile), ("prompt", &self.prompt)]) {
            return Some(blank);
        }
        let places = [
            self.worktree.is_some(),
            self.from_pr.is_some(),
            self.cwd.is_some(),
        ];
        if places.into_iter().filter(|given| *given).count() > 1 {
            return Some("give at most one of `worktree`, `from_pr`, and `cwd`".to_owned());
        }
        if self.fresh && self.worktree.is_none() {
            return Some("`fresh` needs `worktree`".to_owned());
        }
        blank_place(self.worktree.as_deref(), self.from_pr.as_deref())
            .or_else(|| {
                self.schema
                    .as_ref()
                    .and_then(|schema| schema_refusal("schema", schema))
            })
            .or_else(|| duration_refusal("timeout", self.timeout.as_deref()))
    }
}

impl Block for Team {
    const NAME: BlockName = BlockName::Team;
    type Result = TeamResult;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        if let Some(blank) = blank_field(&[("team", &self.team), ("prompt", &self.prompt)]) {
            return Some(blank);
        }
        if self.worktree.is_some() == self.from_pr.is_some() {
            return Some("give exactly one of `worktree` and `from_pr`".to_owned());
        }
        blank_place(self.worktree.as_deref(), self.from_pr.as_deref())
            .or_else(|| duration_refusal("timeout", self.timeout.as_deref()))
    }
}

impl Block for Shell {
    const NAME: BlockName = BlockName::Shell;
    const SCRIPT_REFUSED: &'static [&'static str] = &["on", "check_in"];
    type Result = ShellResult;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        blank_field(&[("command", &self.command)])
            .or_else(|| interval_refusal("check_in", self.check_in.as_deref()))
            .or_else(|| duration_refusal("timeout", self.timeout.as_deref()))
    }
}

impl Block for Watch {
    const NAME: BlockName = BlockName::Watch;
    const SCRIPT_REFUSED: &'static [&'static str] = &["check_in"];
    type Result = WatchResult;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        blank_field(&[("when", &self.when)])
            .or_else(|| when_refusal("when", &self.when))
            .or_else(|| interval_refusal("check_in", self.check_in.as_deref()))
            .or_else(|| duration_refusal("timeout", self.timeout.as_deref()))
    }
}

impl Block for Workflow {
    const NAME: BlockName = BlockName::Workflow;
    const SCRIPT_REFUSED: &'static [&'static str] = &["worktree", "max_launches", "detach"];
    const SCRIPT_REFUSED_VALUES: &'static [(&'static str, &'static str)] = &[("source", "run")];
    type Result = Value;

    fn refusal(&self, caller: Caller) -> Option<String> {
        if let Some(blank) = blank_field(&[("value", &self.value)]) {
            return Some(blank);
        }
        if self.source == WorkflowSource::Name {
            let mut components = std::path::Path::new(&self.value).components();
            if !matches!(
                (components.next(), components.next()),
                (Some(std::path::Component::Normal(_)), None)
            ) {
                return Some(
                    "`value` is an installed workflow's name, not a path; give `source` `path`"
                        .to_owned(),
                );
            }
        }
        if caller == Caller::Script && self.source == WorkflowSource::Run {
            return Some(
                "a script runs no ended run again: `source` `run` belongs to a start of its own"
                    .to_owned(),
            );
        }
        blank_place(self.worktree.as_deref(), None)
            .or_else(|| duration_refusal("timeout", self.timeout.as_deref()))
    }
}

impl Block for Schedule {
    const NAME: BlockName = BlockName::Schedule;
    type Result = ScheduleResult;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        use ScheduleTrigger::{At, Cron, Every, In, Signal, When};
        if let Some(blank) = blank_field(&[("name", &self.name)]) {
            return Some(blank);
        }
        if self.matches.is_some() && self.trigger != Signal {
            return Some("`match` takes a `signal` trigger".to_owned());
        }
        if self
            .matches
            .as_ref()
            .is_some_and(|matches| matches.contains_key(""))
        {
            return Some("a `match` key is empty".to_owned());
        }
        if self.hold.is_some() && self.trigger != When {
            return Some("`for` takes a `when` trigger".to_owned());
        }
        let by_clock = matches!(self.trigger, Every | Cron);
        if self.repeat && !matches!(self.trigger, Signal | When) {
            return Some("`repeat` takes a `signal` or `when` trigger".to_owned());
        }
        if self.overlap.is_some() && !by_clock && !self.repeat {
            return Some("`overlap` takes a repeating trigger".to_owned());
        }
        if let Some(name) = self.worktree.as_deref() {
            if name.trim().is_empty() {
                return Some("`worktree` is blank".to_owned());
            }
            if let Some(reason) =
                flag_refusal("worktree", name).or_else(|| nul_refusal("worktree", Some(name)))
            {
                return Some(reason);
            }
        }
        if let Some(reason) = nul_refusal("value", Some(&self.value))
            .or_else(|| nul_refusal("for", self.hold.as_deref()))
        {
            return Some(reason);
        }
        if let Some(reason) = self.matches.iter().flatten().find_map(|(key, value)| {
            nul_refusal("match", Some(key)).or_else(|| nul_refusal("match", Some(value)))
        }) {
            return Some(reason);
        }
        if self.arguments.contains_key("worktree") {
            return Some("`arguments.worktree`: pass top-level `worktree` instead".to_owned());
        }
        let value_refusal = match self.trigger {
            At => at_refusal("value", &self.value),
            In | Every => interval_refusal("value", Some(&self.value)),
            When => when_refusal("value", &self.value),
            Signal => blank_field(&[("value", &self.value)]),
            // The cron grammar lives with the loop scheduler, which the booking reaches.
            Cron => blank_field(&[("value", &self.value)]),
        };
        value_refusal
            .or_else(|| duration_refusal("for", self.hold.as_deref()))
            .or_else(|| self.carried_refusal())
    }
}

impl Block for Now {
    const NAME: BlockName = BlockName::Now;
    const BARE: bool = true;
    type Result = u64;

    fn refusal(&self, _caller: Caller) -> Option<String> {
        None
    }
}

impl Schedule {
    /// Why `arguments` does not parse as the booked block's own object. A fire starts the block
    /// afresh, outside any script, so it parses as a tool's call.
    fn carried_refusal(&self) -> Option<String> {
        let mut carried = self.arguments.clone();
        if let Some(worktree) = &self.worktree {
            carried.insert("worktree".to_owned(), Value::String(worktree.clone()));
        }
        let carried = Value::Object(carried);
        let parsed = match self.tool {
            ScheduleTool::Agent => parse::<Agent>(carried, Caller::Tool).map(drop),
            ScheduleTool::Team => parse::<Team>(carried, Caller::Tool).map(drop),
            ScheduleTool::Workflow => parse::<Workflow>(carried, Caller::Tool).map(drop),
        };
        parsed.err().map(|reason| format!("`arguments`: {reason}"))
    }
}

fn blank_field(fields: &[(&str, &String)]) -> Option<String> {
    fields
        .iter()
        .find(|(_, value)| value.trim().is_empty())
        .map(|(field, _)| format!("`{field}` is blank"))
}

fn blank_place(worktree: Option<&str>, from_pr: Option<&str>) -> Option<String> {
    [("worktree", worktree), ("from_pr", from_pr)]
        .into_iter()
        .find(|(_, value)| value.is_some_and(|value| value.trim().is_empty()))
        .map(|(field, _)| format!("`{field}` is blank"))
}

/// Why a value a fire's launch passes as a word of its own would read as a flag there.
fn flag_refusal(field: &str, value: &str) -> Option<String> {
    value
        .starts_with('-')
        .then(|| format!("`{field}` starts with `-`, which a fire's launch reads as a flag"))
}

fn nul_refusal(field: &str, value: Option<&str>) -> Option<String> {
    value
        .is_some_and(|value| value.contains('\0'))
        .then(|| format!("`{field}` holds a NUL character, which a fire's launch cannot pass"))
}

/// A duration in the grammar `timeout` and `for` take, in milliseconds: what a script's `ms`
/// gives.
pub(in crate::harness) fn ms(raw: &str) -> Result<u64, String> {
    let duration = crate::harness::duration::duration(raw).map_err(|error| error.to_string())?;
    u64::try_from(duration.as_millis()).map_err(|error| error.to_string())
}

/// The truth of condition `when` over `readings` on the one walk a `watch` fires on, three-valued:
/// none for unknown, and a key absent or `None` is not read. What a script's `reads` gives.
pub(in crate::harness) fn reads(
    when: &str,
    readings: &BTreeMap<String, Option<String>>,
) -> Result<Option<bool>, String> {
    let condition = WhenExpr::parse(&[when.to_owned()]).map_err(|error| error.to_string())?;
    Ok(condition.truth(readings))
}

fn duration_refusal(field: &str, raw: Option<&str>) -> Option<String> {
    let error = crate::harness::duration::duration(raw?).err()?;
    Some(format!("`{field}`: {error}"))
}

/// A duration a timer repeats or fires after: more than zero, and under a day for a poll or a
/// notice.
fn interval_refusal(field: &str, raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let interval = match crate::harness::duration::duration(raw) {
        Ok(interval) => interval,
        Err(error) => return Some(format!("`{field}`: {error}")),
    };
    if interval.is_zero() {
        return Some(format!("`{field}`: must be greater than zero"));
    }
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    (matches!(field, "every" | "check_in") && interval >= day)
        .then(|| format!("`{field}`: must be less than 24h"))
}

fn when_refusal(field: &str, condition: &str) -> Option<String> {
    let error = WhenExpr::parse(&[condition.to_owned()]).err()?;
    Some(format!("`{field}`: {error}"))
}

fn at_refusal(field: &str, clock: &str) -> Option<String> {
    CalendarAt::parse(clock).is_none().then(|| {
        format!("`{field}`: `{clock}` is not `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`")
    })
}

/// Why `answer` does not fit `schema`, a schema `schema_refusal` passed: the path of the first
/// misfit (`answer` for the whole, then `.field` and `[index]`) and its reason.
pub(crate) fn answer_misfit(schema: &Map<String, Value>, answer: &Value) -> Option<String> {
    misfit_at("answer", schema, answer)
}

fn misfit_at(at: &str, schema: &Map<String, Value>, value: &Value) -> Option<String> {
    if let Some(expected) = schema.get("type") {
        let types: Vec<&str> = match expected {
            Value::Array(types) => types.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect(),
        };
        if !types.iter().any(|kind| is_type(kind, value)) {
            return Some(format!("`{at}` is not of type {}", types.join(" or ")));
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.iter().any(|allowed| same_value(allowed, value))
    {
        return Some(format!("`{at}` is not one of the schema's `enum` values"));
    }
    if let Value::Object(fields) = value {
        if let Some(Value::Array(required)) = schema.get("required")
            && let Some(missing) = required
                .iter()
                .filter_map(Value::as_str)
                .find(|name| !fields.contains_key(*name))
        {
            return Some(format!("`{at}` lacks the required `{missing}`"));
        }
        if let Some(Value::Object(properties)) = schema.get("properties")
            && let Some(misfit) = properties.iter().find_map(|(name, property)| {
                let (Value::Object(property), Some(field)) = (property, fields.get(name)) else {
                    return None;
                };
                misfit_at(&format!("{at}.{name}"), property, field)
            })
        {
            return Some(misfit);
        }
    }
    if let (Value::Array(items), Some(Value::Object(item))) = (value, schema.get("items")) {
        return items
            .iter()
            .enumerate()
            .find_map(|(index, value)| misfit_at(&format!("{at}[{index}]"), item, value));
    }
    None
}

/// Whether a schema's `type` is a JSON type name or a non-empty array of them, so some answer
/// can fit it.
fn names_json_types(kind: &Value) -> bool {
    let named = |kind: &Value| {
        kind.as_str().is_some_and(|kind| {
            [
                "object", "array", "string", "number", "integer", "boolean", "null",
            ]
            .contains(&kind)
        })
    };
    match kind {
        Value::Array(kinds) => !kinds.is_empty() && kinds.iter().all(named),
        kind => named(kind),
    }
}

/// Whether `left` and `right` are the same JSON value, numbers compared by value (`1` and
/// `1.0` alike), as JSON Schema compares an `enum` member.
fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            left == right
                || left
                    .as_f64()
                    .is_some_and(|left| Some(left) == right.as_f64())
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_value(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| same_value(left, right)))
        }
        _ => left == right,
    }
}

fn is_type(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        // A number with no fractional part is an integer, `1.0` included.
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|number| number.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

/// The keywords an agent's answer schema may use.
const SCHEMA_KEYWORDS: [&str; 5] = ["type", "required", "properties", "enum", "items"];

fn schema_refusal(at: &str, schema: &Map<String, Value>) -> Option<String> {
    schema.iter().find_map(|(keyword, value)| {
        if !SCHEMA_KEYWORDS.contains(&keyword.as_str()) {
            return Some(format!(
                "`{at}` uses `{keyword}`; a schema takes only `type`, `required`, `properties`, `enum`, and `items`"
            ));
        }
        match (keyword.as_str(), value) {
            ("items", Value::Object(items)) => schema_refusal(&format!("{at}.items"), items),
            ("properties", Value::Object(properties)) => {
                properties.iter().find_map(|(name, property)| match property {
                    Value::Object(property) => {
                        schema_refusal(&format!("{at}.properties.{name}"), property)
                    }
                    _ => Some(format!("`{at}.properties.{name}` is not a schema object")),
                })
            }
            ("items" | "properties", _) => Some(format!("`{at}.{keyword}` is not an object")),
            ("enum", Value::Array(values)) if !values.is_empty() => None,
            ("enum", _) => Some(format!("`{at}.enum` is not a non-empty array of values")),
            ("required", Value::Array(names)) if names.iter().all(Value::is_string) => None,
            ("required", _) => Some(format!("`{at}.required` is not an array of field names")),
            ("type", kind) if !names_json_types(kind) => Some(format!(
                "`{at}.type` names no JSON type: one of `object`, `array`, `string`, `number`, `integer`, `boolean`, and `null`, or an array of them"
            )),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests;
