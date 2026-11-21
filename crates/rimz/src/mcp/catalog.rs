//! The one table of RimZ MCP tools: each tool's capability, argument schema,
//! and the `rimz` command its call runs; `super::wording` holds what each
//! tool and the server's instructions say. `rimz mcp serve`, `rimz mcp tools`,
//! their `--only` values, the profile `mcps` capability names, the reference
//! page's tool table, and the routine-command check all read this table.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::wording::Wording;

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

/// How a provider loads a capability's tools: in the agent's context from the
/// start, or deferred until its tool search finds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoadMode {
    Always,
    Deferred,
}

impl LoadMode {
    /// The mark's name in an `mcps` entry and the `serve` flag it becomes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Deferred => "deferred",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Always, Self::Deferred]
            .into_iter()
            .find(|mode| mode.name() == name)
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
    /// The group the tool belongs to; `None` for `guide`, which the server
    /// answers itself and serves beside any group that has a guide.
    pub capability: Option<Capability>,
    read_only: bool,
    /// The command words after `rimz`; with `verbs`, one command per verb.
    command: &'static str,
    verbs: &'static [LspVerb],
    schema: fn() -> Value,
    invoke: fn(Value) -> Result<Invocation, serde_json::Error>,
}

impl Tool {
    /// The tool's `tools/list` entry, with its description as `wording` resolves it.
    pub fn listing(&self, wording: &Wording) -> Value {
        let mut entry = serde_json::json!({
            "name": self.name,
            "description": wording.description(self).text(),
            "inputSchema": (self.schema)(),
        });
        if self.read_only {
            entry["annotations"] = serde_json::json!({ "readOnlyHint": true });
        }
        entry
    }

    /// The listing entry's estimated size in tokens, as `rimz mcp tools` and
    /// `rimz agents explain --mcps` print it.
    pub fn tokens(&self, wording: &Wording) -> u64 {
        crate::utils::tokens::estimate(&self.listing(wording).to_string())
    }

    /// The name of the tool's group, or `-` for `guide`, which has none.
    pub fn group_name(&self) -> &'static str {
        self.capability.map_or("-", Capability::name)
    }

    /// The path of the wording file holding the tool's description, relative to
    /// the wording root.
    pub(super) fn wording_path(&self) -> String {
        match self.capability {
            Some(capability) => format!("{}/{}.md", capability.name(), self.name),
            None => format!("{}.md", self.name),
        }
    }

    /// The characters of the descriptions of the tool's arguments, which a
    /// provider may count with the tool's own description.
    pub(super) fn argument_text_len(&self) -> usize {
        (self.schema)()["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, property)| property["description"].as_str())
            .map(|text| text.chars().count())
            .sum()
    }

    /// The one-shot a call runs, or the error naming the argument that does
    /// not fit the schema's shape.
    pub fn invocation(&self, arguments: Value) -> Result<Invocation, serde_json::Error> {
        (self.invoke)(arguments)
    }

    /// Every `rimz ...` command the tool can run, without flags or values;
    /// none for `guide`, which the server answers itself.
    pub fn commands(&self) -> Vec<String> {
        if self.capability.is_none() {
            return Vec::new();
        }
        if self.verbs.is_empty() {
            return vec![format!("rimz {}", self.command)];
        }
        self.verbs
            .iter()
            .map(|verb| format!("rimz {} {}", self.command, verb.as_str()))
            .collect()
    }
}

pub static TOOLS: [Tool; 10] = [
    Tool {
        name: "lsp",
        capability: Some(Capability::Lsp),
        read_only: false,
        command: "lsp",
        verbs: &LspVerb::ALL,
        schema: schema::<LspArgs>,
        invoke: invoke::<LspArgs>,
    },
    Tool {
        name: "wait",
        capability: Some(Capability::Wait),
        read_only: false,
        command: "wait",
        verbs: &[],
        schema: schema::<WaitArgs>,
        invoke: invoke::<WaitArgs>,
    },
    Tool {
        name: "wait_list",
        capability: Some(Capability::Wait),
        read_only: true,
        command: "wait list",
        verbs: &[],
        schema: schema::<WaitListArgs>,
        invoke: invoke::<WaitListArgs>,
    },
    Tool {
        name: "wait_cancel",
        capability: Some(Capability::Wait),
        read_only: false,
        command: "wait cancel",
        verbs: &[],
        schema: schema::<WaitCancelArgs>,
        invoke: invoke::<WaitCancelArgs>,
    },
    Tool {
        name: "self_stop",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents stop",
        verbs: &[],
        schema: schema::<SelfStopArgs>,
        invoke: invoke::<SelfStopArgs>,
    },
    Tool {
        name: "self_compact",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents compact",
        verbs: &[],
        schema: schema::<SelfCompactArgs>,
        invoke: invoke::<SelfCompactArgs>,
    },
    Tool {
        name: "self_tune",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents tune",
        verbs: &[],
        schema: schema::<SelfTuneArgs>,
        invoke: invoke::<SelfTuneArgs>,
    },
    Tool {
        name: "notes_add",
        capability: Some(Capability::Notes),
        read_only: false,
        command: "notes add",
        verbs: &[],
        schema: schema::<NotesAddArgs>,
        invoke: invoke::<NotesAddArgs>,
    },
    Tool {
        name: "notes_list",
        capability: Some(Capability::Notes),
        read_only: true,
        command: "notes list",
        verbs: &[],
        schema: schema::<NotesListArgs>,
        invoke: invoke::<NotesListArgs>,
    },
    Tool {
        name: GUIDE,
        capability: None,
        read_only: true,
        command: "",
        verbs: &[],
        schema: schema::<GuideArgs>,
        invoke: invoke::<GuideArgs>,
    },
];

/// The tool that loads a guide, answered by the server from the wording it
/// resolved at its start.
pub const GUIDE: &str = "guide";

/// The tools of `only`'s capabilities, in table order, and `guide` when one
/// of them has a guide; empty means all.
pub fn tools_for(only: &[Capability]) -> impl Iterator<Item = &'static Tool> {
    let groups = served_groups(only);
    let guided = groups
        .iter()
        .any(|group| super::wording::has_guides(*group));
    TOOLS.iter().filter(move |tool| {
        tool.capability
            .map_or(guided, |capability| groups.contains(&capability))
    })
}

/// The capabilities `only` serves, in group order; empty means all.
pub fn served_groups(only: &[Capability]) -> Vec<Capability> {
    Capability::ALL
        .into_iter()
        .filter(|capability| only.is_empty() || only.contains(capability))
        .collect()
}

/// The `tools/list` result the server answers with for `tools`; a tool whose
/// capability `modes` names carries its load mode in its `_meta`, under Claude's key
/// and Antigravity's.
pub fn tools_list(tools: &[&Tool], modes: &[(Capability, LoadMode)], wording: &Wording) -> Value {
    let listing: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut entry = tool.listing(wording);
            let mode = modes
                .iter()
                .find(|(capability, _)| Some(*capability) == tool.capability);
            if let Some((_, mode)) = mode {
                let always = *mode == LoadMode::Always;
                entry["_meta"] = serde_json::json!({
                    "anthropic/alwaysLoad": always,
                    "antigravity/alwaysLoad": always,
                });
            }
            entry
        })
        .collect();
    serde_json::json!({ "tools": listing })
}

/// The instructions, then each tool's description and input schema under a
/// `## <tool>` heading with its size, capability, and load mode: what the
/// server sends for `tools` of `groups` when `modes` gives their load modes
/// and `server_wide` the mode of a tool no group's mode covers.
/// The instructions' heading and a tool's line end with `text <path>` for each
/// part whose text is the user's file.
pub fn render_listing(
    tools: &[&Tool],
    groups: &[Capability],
    modes: &[(Capability, LoadMode)],
    server_wide: Option<LoadMode>,
    wording: &Wording,
) -> String {
    let listing = tools_list(tools, modes, wording);
    let entries = listing["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let instructions = wording.instructions(groups);
    let paths: Vec<String> = wording
        .instruction_paths(groups)
        .map(|path| path.display().to_string())
        .collect();
    let mark = if paths.is_empty() {
        String::new()
    } else {
        format!(" · text {}", paths.join(", "))
    };
    let mut text = format!(
        "Server instructions (~{} tokens{mark})\n\n{instructions}\n",
        crate::utils::tokens::estimate(&instructions)
    );
    for (tool, entry) in tools.iter().zip(entries) {
        let load = modes
            .iter()
            .find(|(capability, _)| Some(*capability) == tool.capability)
            .map(|(_, mode)| *mode)
            .or(server_wide)
            .map_or("default", LoadMode::name);
        let mark = wording
            .description(tool)
            .path()
            .map(|path| format!(" · text {}", path.display()))
            .unwrap_or_default();
        let schema = serde_json::to_string_pretty(&entry["inputSchema"]).unwrap_or_default();
        text.push_str(&format!(
            "\n## {}\n~{} tokens · {} · load {load}{mark}\n\n{}\n\n```json\n{schema}\n```\n",
            tool.name,
            tool.tokens(wording),
            tool.group_name(),
            entry["description"].as_str().unwrap_or_default(),
        ));
    }
    text
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
    /// The query.
    verb: LspVerb,
    /// Every target of the question.
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
    /// Which language server to ask, where the checkout has several.
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
}

impl std::fmt::Display for WaitOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fail => "fail",
            Self::Success => "success",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum WaitTrigger {
    Run,
    Check,
    File,
    Pid,
    When,
    In,
    At,
}

impl WaitTrigger {
    fn flag(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Check => "check",
            Self::File => "file",
            Self::Pid => "pid",
            Self::When => "when",
            Self::In => "in",
            Self::At => "at",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    /// What fires the wait.
    trigger: WaitTrigger,
    /// The trigger's argument: a command, path, PID, expression, duration, or time.
    value: String,
    /// `file` only: fire on a new line holding this literal text.
    grep: Option<String>,
    /// `check` only: the sleep between runs (default `1s`).
    every: Option<String>,
    /// `run`: wake only on this outcome, not either. `check`: `fail` polls until it fails.
    on: Option<WaitOn>,
    /// What you will do on waking, 1 to 60 characters.
    label: Option<String>,
    /// `run`, `check`, `file`, or `pid`: one still-running notice after this long.
    check_in: Option<String>,
    /// Where to run; defaults to the session's start directory.
    cwd: Option<PathBuf>,
}

impl ToolArgs for WaitArgs {
    fn invocation(self) -> Invocation {
        Argv::new("wait")
            .value(self.trigger.flag(), Some(self.value))
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
    /// Leave yourself at rest after a compaction that waited for this turn to end.
    #[serde(default)]
    no_continue: bool,
}

impl ToolArgs for SelfCompactArgs {
    fn invocation(self) -> Invocation {
        let argv = Argv::new("agents compact")
            .switch("no-continue", self.no_continue)
            .positional(SELF);
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
            .invocation(None);
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
}

impl ToolArgs for NotesListArgs {
    fn invocation(self) -> Invocation {
        Argv::new("notes list")
            .values("filter", self.filter)
            .invocation(None)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuideArgs {
    /// The guide, as `<group>/<guide>`.
    name: String,
}

impl ToolArgs for GuideArgs {
    /// The guide's name alone: the server answers a `guide` call itself, and
    /// runs no command.
    fn invocation(self) -> Invocation {
        Invocation {
            args: vec![self.name],
            cwd: None,
            stdin: None,
        }
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
