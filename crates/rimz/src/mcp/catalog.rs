//! The one table of RimZ MCP tools: each tool's capability, description,
//! argument schema, and the `rimz` command its call runs. `rimz mcp serve`,
//! its `--only` values, the profile `mcps` capability names, the reference
//! page's tool table, and the routine-command check all read this table.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The name a provider registers RimZ's server under; Claude spells a tool
/// `mcp__rimz__<tool>`.
pub const SERVER_NAME: &str = "rimz";

/// One group of tools a profile's `mcps` list and `--only` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Lsp,
    Wait,
    Control,
    Notes,
}

impl Capability {
    pub const ALL: [Capability; 4] = [Self::Lsp, Self::Wait, Self::Control, Self::Notes];

    /// The capability's name, which is also the name of the skill it matches.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lsp => "rimz-lsp",
            Self::Wait => "rimz-wait",
            Self::Control => "rimz-control",
            Self::Notes => "rimz-notes",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.name() == name)
    }
}

/// The arguments after the program that start `rimz mcp serve` with `only`'s
/// tools; an empty `only` serves every capability.
pub fn serve_args(only: &[Capability]) -> Vec<String> {
    let mut args = vec!["mcp".to_owned(), "serve".to_owned()];
    for capability in only {
        args.extend(["--only".to_owned(), capability.name().to_owned()]);
    }
    args
}

/// One `rimz` one-shot: its arguments after the program and the global
/// `--color` flag, the directory it runs in, and the bytes its stdin carries.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
}

pub struct Tool {
    pub name: &'static str,
    pub capability: Capability,
    pub description: &'static str,
    read_only: bool,
    /// The command words after `rimz`; with `verbs`, one command per verb.
    command: &'static str,
    verbs: &'static [LspVerb],
    schema: fn() -> Value,
    invoke: fn(Value) -> Result<Invocation, serde_json::Error>,
}

impl Tool {
    /// The tool's `tools/list` entry.
    pub fn listing(&self) -> Value {
        let mut entry = serde_json::json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.schema)(),
        });
        if self.read_only {
            entry["annotations"] = serde_json::json!({ "readOnlyHint": true });
        }
        entry
    }

    /// The one-shot a call runs, or the error naming the argument that does
    /// not fit the schema's shape.
    pub fn invocation(&self, arguments: Value) -> Result<Invocation, serde_json::Error> {
        (self.invoke)(arguments)
    }

    /// Every `rimz ...` command the tool can run, without flags or values.
    pub fn commands(&self) -> Vec<String> {
        if self.verbs.is_empty() {
            return vec![format!("rimz {}", self.command)];
        }
        self.verbs
            .iter()
            .map(|verb| format!("rimz {} {}", self.command, verb.as_str()))
            .collect()
    }
}

pub static TOOLS: [Tool; 9] = [
    Tool {
        name: "lsp",
        capability: Capability::Lsp,
        description: "Ask the checkout's shared language server about code: definitions, \
                      references, callers, callees, implementations, outlines, symbol search, \
                      numbered source, or a check of the code anchors in Markdown files. `server` \
                      picks one of several language servers for the checkout; `check` ignores it.",
        read_only: false,
        command: "lsp",
        verbs: &LspVerb::ALL,
        schema: schema::<LspArgs>,
        invoke: invoke::<LspArgs>,
    },
    Tool {
        name: "wait",
        capability: Capability::Wait,
        description: "Arm one wait that wakes you with a message: after a delay, at a time, when \
                      a state expression reads true, when a command or process finishes, when a \
                      check passes, or when a file changes. Give exactly one trigger.",
        read_only: false,
        command: "wait",
        verbs: &[],
        schema: schema::<WaitArgs>,
        invoke: invoke::<WaitArgs>,
    },
    Tool {
        name: "wait_list",
        capability: Capability::Wait,
        description: "List the waits you have armed.",
        read_only: true,
        command: "wait list",
        verbs: &[],
        schema: schema::<WaitListArgs>,
        invoke: invoke::<WaitListArgs>,
    },
    Tool {
        name: "wait_cancel",
        capability: Capability::Wait,
        description: "Cancel one of your waits by name, or all of them; a watched command stops.",
        read_only: false,
        command: "wait cancel",
        verbs: &[],
        schema: schema::<WaitCancelArgs>,
        invoke: invoke::<WaitCancelArgs>,
    },
    Tool {
        name: "self_stop",
        capability: Capability::Control,
        description: "Book your own stop: your pane closes once you have rested this long \
                      (3m by default) with nothing owed to you; `off` withdraws the booking.",
        read_only: false,
        command: "agents stop",
        verbs: &[],
        schema: schema::<SelfStopArgs>,
        invoke: invoke::<SelfStopArgs>,
    },
    Tool {
        name: "self_compact",
        capability: Capability::Control,
        description: "Compact your own context once this turn ends, with the configured brief \
                      or the instruction you give.",
        read_only: false,
        command: "agents compact",
        verbs: &[],
        schema: schema::<SelfCompactArgs>,
        invoke: invoke::<SelfCompactArgs>,
    },
    Tool {
        name: "self_tune",
        capability: Capability::Control,
        description: "Report or change your own keep-warm, idle compaction, or smart compaction \
                      for this session; give no setting to report all of them.",
        read_only: false,
        command: "agents tune",
        verbs: &[],
        schema: schema::<SelfTuneArgs>,
        invoke: invoke::<SelfTuneArgs>,
    },
    Tool {
        name: "notes_add",
        capability: Capability::Notes,
        description: "Save a Markdown note in the home-wide note store and return its path; \
                      RimZ stamps its header with who wrote it and where.",
        read_only: false,
        command: "notes add",
        verbs: &[],
        schema: schema::<NotesAddArgs>,
        invoke: invoke::<NotesAddArgs>,
    },
    Tool {
        name: "notes_list",
        capability: Capability::Notes,
        description: "List the newest notes in the home-wide note store, oldest first.",
        read_only: true,
        command: "notes list",
        verbs: &[],
        schema: schema::<NotesListArgs>,
        invoke: invoke::<NotesListArgs>,
    },
];

/// The tools of `only`'s capabilities, in table order; empty means all.
pub fn tools_for(only: &[Capability]) -> impl Iterator<Item = &'static Tool> {
    TOOLS
        .iter()
        .filter(move |tool| only.is_empty() || only.contains(&tool.capability))
}

trait ToolArgs: DeserializeOwned + JsonSchema {
    /// Why arguments that fit the schema's shape still cannot run.
    fn refusal(&self) -> Option<String> {
        None
    }

    fn invocation(self) -> Invocation;
}

fn invoke<T: ToolArgs>(arguments: Value) -> Result<Invocation, serde_json::Error> {
    let arguments = serde_json::from_value::<T>(arguments)?;
    match arguments.refusal() {
        Some(reason) => Err(serde::de::Error::custom(reason)),
        None => Ok(arguments.invocation()),
    }
}

/// The argument schema of `T`, inlined, with an optional field's `null`
/// dropped: a client omits an argument rather than sending `null`.
fn schema<T: JsonSchema>() -> Value {
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
            map.values_mut().for_each(drop_null);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_null),
        _ => {}
    }
}

/// Arguments for a one-shot: flags first, each value joined to its flag with
/// `=`, then `--`, then positionals, so no value is read as a flag.
struct Argv {
    flags: Vec<String>,
    positionals: Vec<String>,
}

impl Argv {
    fn new(command: &str) -> Self {
        Self {
            flags: command.split(' ').map(ToOwned::to_owned).collect(),
            positionals: Vec::new(),
        }
    }

    fn switch(mut self, flag: &str, on: bool) -> Self {
        if on {
            self.flags.push(format!("--{flag}"));
        }
        self
    }

    fn value(mut self, flag: &str, value: Option<impl std::fmt::Display>) -> Self {
        if let Some(value) = value {
            self.flags.push(format!("--{flag}={value}"));
        }
        self
    }

    fn values<T: std::fmt::Display>(self, flag: &str, values: impl IntoIterator<Item = T>) -> Self {
        values
            .into_iter()
            .fold(self, |argv, value| argv.value(flag, Some(value)))
    }

    fn positional(mut self, value: impl Into<String>) -> Self {
        self.positionals.push(value.into());
        self
    }

    fn positionals(mut self, values: Vec<String>) -> Self {
        self.positionals.extend(values);
        self
    }

    fn invocation(self, cwd: Option<PathBuf>) -> Invocation {
        let mut args = self.flags;
        if !self.positionals.is_empty() {
            args.push("--".to_owned());
            args.extend(self.positionals);
        }
        Invocation {
            args,
            cwd,
            stdin: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LspVerb {
    Def,
    Hover,
    Refs,
    Callers,
    Callees,
    Impl,
    Symbols,
    Find,
    Show,
    Check,
}

impl LspVerb {
    const ALL: [LspVerb; 10] = [
        Self::Def,
        Self::Hover,
        Self::Refs,
        Self::Callers,
        Self::Callees,
        Self::Impl,
        Self::Symbols,
        Self::Find,
        Self::Show,
        Self::Check,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Def => "def",
            Self::Hover => "hover",
            Self::Refs => "refs",
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Impl => "impl",
            Self::Symbols => "symbols",
            Self::Find => "find",
            Self::Show => "show",
            Self::Check => "check",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LspArgs {
    /// The query: `def`, `hover`, `refs`, `callers`, `callees`, `impl`, `symbols`, `find`,
    /// `show`, or `check`.
    verb: LspVerb,
    /// Positions (`path:line:col`), `path::Symbol` anchors, symbol names, files, or (for
    /// `check`) Markdown files.
    targets: Vec<String>,
    /// Show every location in scope.
    #[serde(default)]
    all: bool,
    /// Hide results in test files and test modules.
    #[serde(default)]
    no_tests: bool,
    /// Include results outside the checkout.
    #[serde(default)]
    external: bool,
    /// The most locations to show.
    limit: Option<u32>,
    /// `show`: print the full body of a large item.
    #[serde(default)]
    full: bool,
    /// `check`: complete and rewrite the anchors it can resolve.
    #[serde(default)]
    fix: bool,
    /// `check` with `fix`: add a hint to every anchor that names one item.
    #[serde(default)]
    hints: bool,
    /// The language server to ask, where the checkout has several; `check` ignores it.
    server: Option<String>,
    /// The directory to run in; defaults to where the session started.
    cwd: Option<PathBuf>,
}

impl ToolArgs for LspArgs {
    fn invocation(self) -> Invocation {
        let server = self.server.filter(|_| !matches!(self.verb, LspVerb::Check));
        Argv::new(&format!("lsp {}", self.verb.as_str()))
            .value("server", server)
            .switch("all", self.all)
            .switch("no-tests", self.no_tests)
            .switch("external", self.external)
            .value("limit", self.limit)
            .switch("full", self.full)
            .switch("fix", self.fix)
            .switch("hints", self.hints)
            .positionals(self.targets)
            .invocation(self.cwd)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum WaitOn {
    Fail,
    Success,
    Any,
}

impl std::fmt::Display for WaitOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fail => "fail",
            Self::Success => "success",
            Self::Any => "any",
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    /// Trigger: run this shell command once through `sh -c` and wake with its outcome.
    run: Option<String>,
    /// Trigger: run this shell command repeatedly until it succeeds (or fails, with `on`).
    check: Option<String>,
    /// Trigger: wake when this file is created, deleted, written, or replaced.
    file: Option<String>,
    /// Trigger: wake when this existing process disappears.
    pid: Option<u32>,
    /// Trigger: wake once every state expression, such as `pr=merged`, reads true.
    #[serde(default)]
    when: Vec<String>,
    /// Trigger: wake once after this delay, such as `30m`.
    #[serde(rename = "in")]
    delay: Option<String>,
    /// Trigger: wake once at this time: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.
    at: Option<String>,
    /// With `file`: wait for a new line containing this literal text.
    grep: Option<String>,
    /// With `check`: sleep this long between runs.
    every: Option<String>,
    /// With `run` or `check`: deliver for a failed, successful, or any outcome.
    on: Option<WaitOn>,
    /// A short description of one line.
    label: Option<String>,
    /// For a watch: send one still-running message after this long.
    check_in: Option<String>,
    /// The directory to run in; defaults to where the session started.
    cwd: Option<PathBuf>,
}

impl ToolArgs for WaitArgs {
    fn refusal(&self) -> Option<String> {
        let armed = self.run.is_some()
            || self.check.is_some()
            || self.file.is_some()
            || self.pid.is_some()
            || !self.when.is_empty()
            || self.delay.is_some()
            || self.at.is_some();
        (!armed).then(|| {
            "give a trigger: run, check, file, pid, when, in, or at; nothing was armed".to_owned()
        })
    }

    fn invocation(self) -> Invocation {
        Argv::new("wait")
            .value("run", self.run)
            .value("check", self.check)
            .value("file", self.file)
            .value("pid", self.pid)
            .values("when", self.when)
            .value("in", self.delay)
            .value("at", self.at)
            .value("grep", self.grep)
            .value("every", self.every)
            .value("on", self.on)
            .value("label", self.label)
            .value("check-in", self.check_in)
            .invocation(self.cwd)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitListArgs {}

impl ToolArgs for WaitListArgs {
    fn invocation(self) -> Invocation {
        Argv::new("wait list").invocation(None)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitCancelArgs {
    /// The wait's name, as `wait_list` prints it.
    name: Option<String>,
    /// Cancel every wait you armed, leaving subscriptions intact.
    #[serde(default)]
    all: bool,
}

impl ToolArgs for WaitCancelArgs {
    fn invocation(self) -> Invocation {
        let argv = Argv::new("wait cancel").switch("all", self.all);
        match self.name {
            Some(name) => argv.positional(name),
            None => argv,
        }
        .invocation(None)
    }
}

/// The address every self-control tool acts on: the calling agent.
const SELF: &str = "@me";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SelfStopArgs {
    /// How long to rest before the pane closes (`s`/`m`/`h`/`d`), or `off` to withdraw the
    /// booking; 3m when absent.
    when_idle: Option<String>,
}

impl ToolArgs for SelfStopArgs {
    fn invocation(self) -> Invocation {
        let argv = Argv::new("agents stop");
        match self.when_idle {
            Some(rest) => argv.value("when-idle", Some(rest)),
            None => argv.switch("when-idle", true),
        }
        .positional(SELF)
        .invocation(None)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SelfCompactArgs {
    /// Replaces the configured compaction brief; an empty string sends the bare command.
    instruction: Option<String>,
}

impl ToolArgs for SelfCompactArgs {
    fn invocation(self) -> Invocation {
        let argv = Argv::new("agents compact").positional(SELF);
        match self.instruction {
            Some(instruction) => argv.positional(instruction),
            None => argv,
        }
        .invocation(None)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum TuneSetting {
    KeepWarm,
    IdleCompact,
    SmartCompact,
}

impl TuneSetting {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeepWarm => "keep-warm",
            Self::IdleCompact => "idle-compact",
            Self::SmartCompact => "smart-compact",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SelfTuneArgs {
    /// The setting to report or change; absent reports every setting.
    setting: Option<TuneSetting>,
    /// The new value (`2h`, `120k`, `70%`), `off`, or `reset`; absent reports the setting.
    value: Option<String>,
}

impl ToolArgs for SelfTuneArgs {
    fn refusal(&self) -> Option<String> {
        (self.setting.is_none() && self.value.is_some())
            .then(|| "a value needs the setting it changes".to_owned())
    }

    fn invocation(self) -> Invocation {
        let argv = Argv::new("agents tune").positional(SELF);
        let argv = match self.setting {
            Some(setting) => argv.positional(setting.as_str()),
            None => argv,
        };
        match self.value {
            Some(value) => argv.positional(value),
            None => argv,
        }
        .invocation(None)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotesAddArgs {
    /// The note's kind, which names its folder (handoff, reflection, ...).
    kind: String,
    /// The note's title, which names its file.
    title: String,
    /// The note's Markdown body.
    body: Option<String>,
    /// Read the body from this file instead.
    file: Option<String>,
    /// Wrap the body in one fenced code block, for a log or other text.
    #[serde(default)]
    fence: bool,
    /// Header keys of your own; RimZ's own keys win.
    #[serde(default)]
    meta: BTreeMap<String, String>,
    /// The directory to run in, which names the note's project unless you run in a room, whose
    /// project wins; defaults to where the session started.
    cwd: Option<PathBuf>,
}

impl ToolArgs for NotesAddArgs {
    fn refusal(&self) -> Option<String> {
        if self.body.is_some() && self.file.is_some() {
            return Some("give the body or a file, not both".to_owned());
        }
        self.meta
            .keys()
            .find(|key| key.contains('='))
            .map(|key| format!("meta key {key:?} contains `=`"))
    }

    fn invocation(self) -> Invocation {
        let mut invocation = Argv::new("notes add")
            .value("file", self.file)
            .switch("fence", self.fence)
            .values(
                "meta",
                self.meta
                    .into_iter()
                    .map(|(key, value)| format!("{key}={value}")),
            )
            .positional(self.kind)
            .positional(self.title)
            .invocation(self.cwd);
        invocation.stdin = self.body.map(String::into_bytes);
        invocation
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotesListArgs {
    /// Keep notes matching every `field=value` term; `help` lists the fields.
    #[serde(default)]
    filter: Vec<String>,
    /// Show the newest N matches; 0 shows every match.
    limit: Option<u32>,
}

impl ToolArgs for NotesListArgs {
    fn invocation(self) -> Invocation {
        Argv::new("notes list")
            .values("filter", self.filter)
            .value("limit", self.limit)
            .invocation(None)
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
