//! The one table of RimZ MCP tools: each tool's capability, argument schema,
//! answer schema, and the `rimz` command it stands for; `super::wording` holds
//! what each tool and the server's instructions say. `rimz mcp serve`, `rimz mcp tools`,
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

/// A tool call whose arguments fit its tool's schema, as the runner `serve`
/// is handed receives it.
#[derive(Debug)]
pub enum Call {
    Lsp(LspArgs),
    Wait(WaitArgs),
    WaitList(WaitListArgs),
    WaitCancel(WaitCancelArgs),
    SelfStop(SelfStopArgs),
    SelfCompact(SelfCompactArgs),
    SelfTune(SelfTuneArgs),
    NotesAdd(NotesAddArgs),
    NotesList(NotesListArgs),
    Guide(GuideArgs),
}

pub struct Tool {
    pub name: &'static str,
    /// The group the tool belongs to; `None` for `guide`, which the server
    /// answers itself and serves beside any group that has a guide.
    pub capability: Option<Capability>,
    read_only: bool,
    /// The command words after `rimz` of the command the tool stands for; with
    /// `verbs`, one command per verb.
    command: &'static str,
    verbs: &'static [LspVerb],
    schema: fn() -> Value,
    /// The schema of a data answer; `None` for a tool that answers text.
    output: Option<fn() -> Value>,
    parse: fn(Value) -> Result<Call, serde_json::Error>,
}

impl Tool {
    /// The tool's `tools/list` entry, with its description as `wording` resolves it.
    pub fn listing(&self, wording: &Wording) -> Value {
        let mut entry = serde_json::json!({
            "name": self.name,
            "description": wording.description(self).text(),
            "inputSchema": (self.schema)(),
        });
        if let Some(output) = self.output_schema() {
            entry["outputSchema"] = output;
        }
        if self.read_only {
            entry["annotations"] = serde_json::json!({ "readOnlyHint": true });
        }
        entry
    }

    /// The schema every data answer of the tool fits; `None` for `lsp` and
    /// `guide`, which answer text.
    pub fn output_schema(&self) -> Option<Value> {
        self.output.map(|output| output())
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

    /// The call the arguments make, or the error naming the argument that
    /// does not fit the schema's shape.
    pub fn call(&self, arguments: Value) -> Result<Call, serde_json::Error> {
        (self.parse)(arguments)
    }

    /// Every `rimz ...` command the tool stands for, without flags or values,
    /// so an agent may do through the tool only what it may run unasked; none
    /// for `guide`, which the server answers itself.
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
        output: None,
        parse: |arguments| parse::<LspArgs>(arguments).map(Call::Lsp),
    },
    Tool {
        name: "wait",
        capability: Some(Capability::Wait),
        read_only: false,
        command: "wait",
        verbs: &[],
        schema: schema::<WaitArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<WaitArgs>(arguments).map(Call::Wait),
    },
    Tool {
        name: "wait_list",
        capability: Some(Capability::Wait),
        read_only: true,
        command: "wait list",
        verbs: &[],
        schema: schema::<WaitListArgs>,
        output: Some(wait_list_schema),
        parse: |arguments| parse::<WaitListArgs>(arguments).map(Call::WaitList),
    },
    Tool {
        name: "wait_cancel",
        capability: Some(Capability::Wait),
        read_only: false,
        command: "wait cancel",
        verbs: &[],
        schema: schema::<WaitCancelArgs>,
        output: Some(wait_cancel_schema),
        parse: |arguments| parse::<WaitCancelArgs>(arguments).map(Call::WaitCancel),
    },
    Tool {
        name: "self_stop",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents stop",
        verbs: &[],
        schema: schema::<SelfStopArgs>,
        output: Some(self_stop_schema),
        parse: |arguments| parse::<SelfStopArgs>(arguments).map(Call::SelfStop),
    },
    Tool {
        name: "self_compact",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents compact",
        verbs: &[],
        schema: schema::<SelfCompactArgs>,
        output: Some(self_compact_schema),
        parse: |arguments| parse::<SelfCompactArgs>(arguments).map(Call::SelfCompact),
    },
    Tool {
        name: "self_tune",
        capability: Some(Capability::Control),
        read_only: false,
        command: "agents tune",
        verbs: &[],
        schema: schema::<SelfTuneArgs>,
        output: Some(self_tune_schema),
        parse: |arguments| parse::<SelfTuneArgs>(arguments).map(Call::SelfTune),
    },
    Tool {
        name: "notes_add",
        capability: Some(Capability::Notes),
        read_only: false,
        command: "notes add",
        verbs: &[],
        schema: schema::<NotesAddArgs>,
        output: Some(notes_add_schema),
        parse: |arguments| parse::<NotesAddArgs>(arguments).map(Call::NotesAdd),
    },
    Tool {
        name: "notes_list",
        capability: Some(Capability::Notes),
        read_only: true,
        command: "notes list",
        verbs: &[],
        schema: schema::<NotesListArgs>,
        output: Some(notes_list_schema),
        parse: |arguments| parse::<NotesListArgs>(arguments).map(Call::NotesList),
    },
    Tool {
        name: GUIDE,
        capability: None,
        read_only: true,
        command: "",
        verbs: &[],
        schema: schema::<GuideArgs>,
        output: None,
        parse: |arguments| parse::<GuideArgs>(arguments).map(Call::Guide),
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
}

fn parse<T: ToolArgs>(arguments: Value) -> Result<T, serde_json::Error> {
    let arguments = serde_json::from_value::<T>(arguments)?;
    match arguments.refusal() {
        Some(reason) => Err(serde::de::Error::custom(reason)),
        None => Ok(arguments),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LspArgs {
    /// The query.
    pub verb: LspVerb,
    /// Every target of the question.
    #[schemars(length(min = 1))]
    pub targets: Vec<String>,
    /// Show every location in scope.
    #[serde(default)]
    pub all: bool,
    /// Hide results in test files and test modules.
    #[serde(default)]
    pub no_tests: bool,
    /// Include results outside the checkout.
    #[serde(default)]
    pub external: bool,
    /// The most locations to show.
    #[schemars(range(min = 1))]
    pub limit: Option<u32>,
    /// `show`: print the full body of a large item.
    #[serde(default)]
    pub full: bool,
    /// `check`: complete and rewrite the anchors it can resolve.
    #[serde(default)]
    pub fix: bool,
    /// `check` with `fix`: add a hint to every anchor that names one item.
    #[serde(default)]
    pub hints: bool,
    /// Which language server to ask, where the checkout has several.
    server: Option<String>,
    /// The directory to run in; defaults to where the session started.
    pub cwd: Option<PathBuf>,
}

impl ToolArgs for LspArgs {
    /// The rules `rimz lsp` holds in its flags: a target, `hints` only with
    /// `fix`, `limit` of at least 1 and not with `all`, and each field only on
    /// the verbs whose command takes its flag.
    fn refusal(&self) -> Option<String> {
        if self.targets.is_empty() {
            return Some("give at least one target".to_owned());
        }
        if self.hints && !self.fix {
            return Some("`hints` needs `fix`".to_owned());
        }
        if self.all && self.limit.is_some() {
            return Some("give `limit` or `all`, not both".to_owned());
        }
        if self.limit == Some(0) {
            return Some("`limit` is at least 1".to_owned());
        }
        let verb = self.verb;
        let listing = matches!(
            verb,
            LspVerb::Refs | LspVerb::Callers | LspVerb::Impl | LspVerb::Callees | LspVerb::Find
        );
        let references = matches!(verb, LspVerb::Refs | LspVerb::Callers);
        [
            ("all", self.all, listing),
            ("limit", self.limit.is_some(), listing),
            ("external", self.external, listing),
            ("no_tests", self.no_tests, references),
            ("full", self.full, verb == LspVerb::Show),
            ("fix", self.fix, verb == LspVerb::Check),
            ("hints", self.hints, verb == LspVerb::Check),
        ]
        .into_iter()
        .find(|(_, given, taken)| *given && !*taken)
        .map(|(field, ..)| format!("`{}` does not take `{field}`", verb.as_str()))
    }
}

impl LspArgs {
    /// The language server the call names, which `check` ignores.
    pub fn server(&self) -> Option<&str> {
        self.server
            .as_deref()
            .filter(|_| self.verb != LspVerb::Check)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WaitOn {
    Fail,
    Success,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WaitTrigger {
    Run,
    Check,
    File,
    Pid,
    When,
    In,
    At,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    /// What fires the wait.
    pub trigger: WaitTrigger,
    /// The trigger's argument: a command, path, PID, expression, duration, or time.
    pub value: String,
    /// `file` only: fire on a new line holding this literal text.
    pub grep: Option<String>,
    /// `check` only: the sleep between runs (default `1s`).
    pub every: Option<String>,
    /// `run`: wake only on this outcome, not either. `check`: `fail` polls until it fails.
    pub on: Option<WaitOn>,
    /// What you will do on waking, 1 to 60 characters.
    pub label: Option<String>,
    /// `run`, `check`, `file`, or `pid`: one still-running notice after this long.
    pub check_in: Option<String>,
    /// Where to run; defaults to the session's start directory.
    pub cwd: Option<PathBuf>,
}

impl ToolArgs for WaitArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitListArgs {}

impl ToolArgs for WaitListArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitCancelArgs {
    /// The wait's name, as `wait_list` prints it.
    pub name: Option<String>,
    /// Cancel every wait you armed, leaving subscriptions intact.
    #[serde(default)]
    pub all: bool,
}

impl ToolArgs for WaitCancelArgs {
    fn refusal(&self) -> Option<String> {
        (self.name.is_some() == self.all)
            .then(|| "give a wait's name or `all`, and only one of them".to_owned())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelfStopArgs {
    /// How long to rest before the pane closes (`s`/`m`/`h`/`d`), or `off` to withdraw the
    /// booking; 3m when absent.
    pub when_idle: Option<String>,
}

impl ToolArgs for SelfStopArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelfCompactArgs {
    /// Replaces the configured compaction brief; an empty string sends the bare command.
    pub instruction: Option<String>,
    /// Leave yourself at rest after a compaction that waited for this turn to end.
    #[serde(default)]
    pub no_continue: bool,
}

impl ToolArgs for SelfCompactArgs {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TuneSetting {
    KeepWarm,
    IdleCompact,
    SmartCompact,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelfTuneArgs {
    /// The setting to report or change; absent reports every setting.
    pub setting: Option<TuneSetting>,
    /// The new value (`2h`, `120k`, `70%`), `off`, or `reset`; absent reports the setting.
    pub value: Option<String>,
}

impl ToolArgs for SelfTuneArgs {
    fn refusal(&self) -> Option<String> {
        (self.setting.is_none() && self.value.is_some())
            .then(|| "a value needs the setting it changes".to_owned())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotesAddArgs {
    /// The note's kind, which names its folder (handoff, reflection, ...).
    pub kind: String,
    /// The note's title, which names its file.
    pub title: String,
    /// The note's Markdown body.
    pub body: Option<String>,
    /// Read the body from this file instead.
    pub file: Option<String>,
    /// Wrap the body in one fenced code block, for a log or other text.
    #[serde(default)]
    pub fence: bool,
    /// Header keys of your own; RimZ's own keys win.
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotesListArgs {
    /// Keep notes matching every `field=value` term; `help` lists the fields.
    #[serde(default)]
    pub filter: Vec<String>,
}

impl ToolArgs for NotesListArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideArgs {
    /// The guide, as `<group>/<guide>`.
    pub name: String,
}

impl ToolArgs for GuideArgs {}

/// An object schema holding `properties`, of which `required` must be present.
fn object(properties: Value, required: &[&str]) -> Value {
    serde_json::json!({ "type": "object", "properties": properties, "required": required })
}

fn array_of(items: Value) -> Value {
    serde_json::json!({ "type": "array", "items": items })
}

fn string() -> Value {
    serde_json::json!({ "type": "string" })
}

fn any_object() -> Value {
    serde_json::json!({ "type": "object" })
}

/// The answer of a tool that starts something: its `id`, `status`, the
/// exact call that stops it, and how its result reaches the caller.
fn start_schema() -> Value {
    object(
        serde_json::json!({
            "id": string(), "status": string(), "stop": string(), "report": string(),
        }),
        &["id", "status", "stop", "report"],
    )
}

fn wait_list_schema() -> Value {
    object(
        serde_json::json!({ "waits": array_of(any_object()) }),
        &["waits"],
    )
}

fn wait_cancel_schema() -> Value {
    object(
        serde_json::json!({
            "canceled": array_of(string()), "pending": array_of(any_object()),
        }),
        &["canceled", "pending"],
    )
}

fn self_stop_schema() -> Value {
    object(
        serde_json::json!({
            "booking": { "type": "string", "enum": ["withdrawn", "none_pending", "booked"] },
            "after_secs": { "type": "integer" },
            "replaces_after_secs": { "type": ["integer", "null"] },
            "started": object(
                serde_json::json!({
                    "holding": array_of(any_object()),
                    "closing": array_of(any_object()),
                    "outliving": array_of(any_object()),
                    "unread": string(),
                }),
                &[],
            ),
        }),
        &["booking"],
    )
}

fn self_compact_schema() -> Value {
    object(
        serde_json::json!({
            "message_id": string(),
            "status": { "type": "string", "enum": ["sent", "queued"] },
            "continues": { "type": "boolean" },
        }),
        &["message_id", "status"],
    )
}

fn self_tune_schema() -> Value {
    object(
        serde_json::json!({
            "agent": string(),
            "settings": array_of(any_object()),
            "keep_warm_while": string(),
            "budget": any_object(),
        }),
        &["agent", "settings"],
    )
}

fn notes_add_schema() -> Value {
    object(
        serde_json::json!({
            "path": string(),
            "dropped_meta": array_of(string()),
            "unresolved_project": { "type": ["string", "null"] },
        }),
        &["path", "dropped_meta"],
    )
}

/// A listing, or for the filter `help` the fields a filter names and its grammar.
fn notes_list_schema() -> Value {
    object(
        serde_json::json!({
            "notes": array_of(any_object()),
            "hidden": { "type": "integer" },
            "unknown_keys": array_of(string()),
            "skipped": array_of(serde_json::json!({ "type": "array" })),
            "fields": array_of(any_object()),
            "grammar": array_of(string()),
        }),
        &[],
    )
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
