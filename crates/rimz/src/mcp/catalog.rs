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
    Subagents,
    Agents,
    Teams,
    Workflow,
    Loop,
    Message,
    Control,
    Notes,
}

impl Capability {
    pub const ALL: [Capability; 10] = [
        Self::Lsp,
        Self::Wait,
        Self::Subagents,
        Self::Agents,
        Self::Teams,
        Self::Workflow,
        Self::Loop,
        Self::Message,
        Self::Control,
        Self::Notes,
    ];

    /// Whether the group's tools start things, which `tasks`, `join`, and `stop` list, await,
    /// and end.
    pub fn starts_things(self) -> bool {
        matches!(
            self,
            Self::Wait | Self::Subagents | Self::Agents | Self::Teams | Self::Workflow | Self::Loop
        )
    }

    /// Whether the group's tools launch agents themselves, which a launch that cannot delegate
    /// is denied, tools and skill alike.
    pub fn launches_agents(self) -> bool {
        matches!(self, Self::Subagents | Self::Agents | Self::Teams)
    }

    /// The capability's name, which is also the name of the skill it matches.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lsp => "rimz-lsp",
            Self::Wait => "rimz-wait",
            Self::Subagents => "rimz-subagents",
            Self::Agents => "rimz-agents",
            Self::Teams => "rimz-teams",
            Self::Workflow => "rimz-workflow",
            Self::Loop => "rimz-loop",
            Self::Message => "rimz-message",
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
    Subagent(SubagentArgs),
    Agent(AgentArgs),
    Team(TeamArgs),
    Workflow(WorkflowArgs),
    Schedule(Box<ScheduleArgs>),
    Message(MessageArgs),
    Tasks(TasksArgs),
    Join(JoinArgs),
    Stop(StopArgs),
    SelfStop(SelfStopArgs),
    SelfCompact(SelfCompactArgs),
    SelfTune(SelfTuneArgs),
    SelfMolt(SelfMoltArgs),
    Notes(NotesArgs),
    Guide(GuideArgs),
}

pub struct Tool {
    pub name: &'static str,
    /// The group the tool belongs to; `None` for a tool of no group: `guide`,
    /// served beside any group that has a guide, and `tasks`, `join`, and
    /// `stop`, served beside any group that starts things.
    pub capability: Option<Capability>,
    /// What the tool is for, which the instructions' first line groups the
    /// served tools by.
    pub purpose: Purpose,
    read_only: bool,
    /// The command words after `rimz` of the command the tool stands for; with
    /// `verbs`, one command per verb. A tool of no group stands for each
    /// command of `declared` instead, and `schedule`, whose booking no routine
    /// command makes, leaves this empty and stands for none.
    command: &'static str,
    declared: &'static [&'static str],
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

    /// The name of the tool's group, or `-` for a tool of no group.
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
    /// so an agent may do through the tool only what it may run unasked: a
    /// tool of no group declares its commands, `guide`, which the server
    /// answers itself, declares none, and `schedule` is the named exception.
    pub fn commands(&self) -> Vec<String> {
        if self.capability.is_none() {
            return self
                .declared
                .iter()
                .map(|command| format!("rimz {command}"))
                .collect();
        }
        if self.command.is_empty() {
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

pub static TOOLS: [Tool; 17] = [
    Tool {
        name: "lsp",
        capability: Some(Capability::Lsp),
        purpose: Purpose::Code,
        read_only: false,
        command: "lsp",
        declared: &[],
        verbs: &LspVerb::ALL,
        schema: schema::<LspArgs>,
        output: None,
        parse: |arguments| parse::<LspArgs>(arguments).map(Call::Lsp),
    },
    Tool {
        name: "wait",
        capability: Some(Capability::Wait),
        purpose: Purpose::Start,
        read_only: false,
        command: "wait",
        declared: &[],
        verbs: &[],
        schema: schema::<WaitArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<WaitArgs>(arguments).map(Call::Wait),
    },
    Tool {
        name: "subagent",
        capability: Some(Capability::Subagents),
        purpose: Purpose::Delegate,
        read_only: false,
        command: "subagents",
        declared: &[],
        verbs: &[],
        schema: schema::<SubagentArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<SubagentArgs>(arguments).map(Call::Subagent),
    },
    Tool {
        name: "agent",
        capability: Some(Capability::Agents),
        purpose: Purpose::Delegate,
        read_only: false,
        command: "agents",
        declared: &[],
        verbs: &[],
        schema: schema::<AgentArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<AgentArgs>(arguments).map(Call::Agent),
    },
    Tool {
        name: "team",
        capability: Some(Capability::Teams),
        purpose: Purpose::Delegate,
        read_only: false,
        command: "teams launch",
        declared: &[],
        verbs: &[],
        schema: schema::<TeamArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<TeamArgs>(arguments).map(Call::Team),
    },
    Tool {
        name: "workflow",
        capability: Some(Capability::Workflow),
        purpose: Purpose::Workflows,
        read_only: false,
        command: "workflow run",
        declared: &[],
        verbs: &[],
        schema: schema::<WorkflowArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<WorkflowArgs>(arguments).map(Call::Workflow),
    },
    Tool {
        name: "schedule",
        capability: Some(Capability::Loop),
        purpose: Purpose::Schedule,
        read_only: false,
        command: "",
        declared: &[],
        verbs: &[],
        schema: schema::<ScheduleArgs>,
        output: Some(start_schema),
        parse: |arguments| {
            parse::<ScheduleArgs>(arguments).map(|args| Call::Schedule(Box::new(args)))
        },
    },
    Tool {
        name: "message",
        capability: Some(Capability::Message),
        purpose: Purpose::Message,
        read_only: false,
        command: "message",
        declared: &[],
        verbs: &[],
        schema: schema::<MessageArgs>,
        output: Some(message_schema),
        parse: |arguments| parse::<MessageArgs>(arguments).map(Call::Message),
    },
    Tool {
        name: "tasks",
        capability: None,
        purpose: Purpose::Started,
        read_only: true,
        command: "",
        declared: &["agents show"],
        verbs: &[],
        schema: schema::<TasksArgs>,
        output: Some(tasks_schema),
        parse: |arguments| parse::<TasksArgs>(arguments).map(Call::Tasks),
    },
    Tool {
        name: "join",
        capability: None,
        purpose: Purpose::Started,
        read_only: false,
        command: "",
        declared: &[
            "agents wait",
            "subagents wait",
            "teams wait",
            "workflow wait",
        ],
        verbs: &[],
        schema: schema::<JoinArgs>,
        output: Some(join_schema),
        parse: |arguments| parse::<JoinArgs>(arguments).map(Call::Join),
    },
    Tool {
        name: "stop",
        capability: None,
        purpose: Purpose::Started,
        read_only: false,
        command: "",
        declared: &[
            "agents stop",
            "subagents stop",
            "teams stop",
            "wait cancel",
            "workflow cancel",
        ],
        verbs: &[],
        schema: schema::<StopArgs>,
        output: Some(stop_schema),
        parse: |arguments| parse::<StopArgs>(arguments).map(Call::Stop),
    },
    Tool {
        name: "self_stop",
        capability: Some(Capability::Control),
        purpose: Purpose::Yourself,
        read_only: false,
        command: "agents stop",
        declared: &[],
        verbs: &[],
        schema: schema::<SelfStopArgs>,
        output: Some(self_stop_schema),
        parse: |arguments| parse::<SelfStopArgs>(arguments).map(Call::SelfStop),
    },
    Tool {
        name: "self_compact",
        capability: Some(Capability::Control),
        purpose: Purpose::Yourself,
        read_only: false,
        command: "agents compact",
        declared: &[],
        verbs: &[],
        schema: schema::<SelfCompactArgs>,
        output: Some(self_compact_schema),
        parse: |arguments| parse::<SelfCompactArgs>(arguments).map(Call::SelfCompact),
    },
    Tool {
        name: "self_tune",
        capability: Some(Capability::Control),
        purpose: Purpose::Yourself,
        read_only: false,
        command: "agents tune",
        declared: &[],
        verbs: &[],
        schema: schema::<SelfTuneArgs>,
        output: Some(self_tune_schema),
        parse: |arguments| parse::<SelfTuneArgs>(arguments).map(Call::SelfTune),
    },
    Tool {
        name: "self_molt",
        capability: Some(Capability::Control),
        purpose: Purpose::Yourself,
        read_only: false,
        command: "agents molt",
        declared: &[],
        verbs: &[],
        schema: schema::<SelfMoltArgs>,
        output: Some(self_molt_schema),
        parse: |arguments| parse::<SelfMoltArgs>(arguments).map(Call::SelfMolt),
    },
    Tool {
        name: "notes",
        capability: Some(Capability::Notes),
        purpose: Purpose::Notes,
        read_only: false,
        command: "notes add",
        declared: &[],
        verbs: &[],
        schema: schema::<NotesArgs>,
        output: Some(notes_schema),
        parse: |arguments| parse::<NotesArgs>(arguments).map(Call::Notes),
    },
    Tool {
        name: GUIDE,
        capability: None,
        purpose: Purpose::Guides,
        read_only: true,
        command: "",
        declared: &[],
        verbs: &[],
        schema: schema::<GuideArgs>,
        output: None,
        parse: |arguments| parse::<GuideArgs>(arguments).map(Call::Guide),
    },
];

/// What a tool is for, in the order the instructions' first line names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Purpose {
    Code,
    Start,
    Delegate,
    Workflows,
    Schedule,
    Message,
    Started,
    Yourself,
    Notes,
    Guides,
}

impl Purpose {
    /// What the tools of this purpose let an agent do, as the first line lists it.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Code => "read code",
            Self::Start => "wait outside your turn",
            Self::Delegate => "delegate",
            Self::Workflows => "run workflows",
            Self::Schedule => "schedule launches",
            Self::Message => "message other agents",
            Self::Started => "track what you started",
            Self::Yourself => "manage yourself",
            Self::Notes => "keep notes",
            Self::Guides => "load usage guides",
        }
    }
}

/// The tool that loads a guide, answered by the server from the wording it
/// resolved at its start.
pub const GUIDE: &str = "guide";

/// The tools of `only`'s capabilities, in table order, with `guide` when one
/// of them has a guide and `tasks`, `join`, and `stop` when one of them
/// starts things; empty means all.
pub fn tools_for(only: &[Capability]) -> impl Iterator<Item = &'static Tool> {
    let groups = served_groups(only);
    let guided = groups
        .iter()
        .any(|group| super::wording::has_guides(*group));
    let starting = groups.iter().any(|group| group.starts_things());
    TOOLS.iter().filter(move |tool| match tool.capability {
        Some(capability) => groups.contains(&capability),
        None if tool.name == GUIDE => guided,
        None => starting,
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
pub struct SubagentArgs {
    /// The subagent profile to launch.
    pub profile: String,
    /// The brief: the whole task, its scope, and the result to report.
    pub prompt: String,
    /// A label for `tasks` and the sidebar, a few words.
    pub description: Option<String>,
    /// Where the child works; defaults to your own checkout.
    pub cwd: Option<PathBuf>,
    /// End the child after this long (`90m`, `2h`); the profile's timeout when absent, 30m by
    /// default.
    pub timeout: Option<String>,
    /// Override the profile's model.
    pub model: Option<String>,
    /// Override the profile's reasoning effort.
    pub effort: Option<String>,
    /// Override the profile's agent kind.
    pub agent: Option<String>,
}

impl ToolArgs for SubagentArgs {
    fn refusal(&self) -> Option<String> {
        [("profile", &self.profile), ("prompt", &self.prompt)]
            .into_iter()
            .find(|(_, value)| value.trim().is_empty())
            .map(|(field, _)| format!("`{field}` is blank"))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    /// The agent profile to launch, or a bare model; one seat, never a team.
    pub profile: String,
    /// The brief: the agent's first turn.
    pub prompt: String,
    /// A label for `tasks` and the sidebar, a few words.
    pub description: Option<String>,
    /// A distinct handle, when the channel already holds this profile's.
    pub name: Option<String>,
    /// Re-base the profile onto this model alias for this launch.
    pub agent: Option<String>,
    /// Work in this worktree. A new one branches from `agents.worktree.base` when set, else from
    /// the `HEAD` of your repository's main checkout; a `team.role` branches as its team does.
    pub worktree: Option<String>,
    /// Start a new session in an existing `worktree`.
    #[serde(default)]
    pub fresh: bool,
    /// Work in a worktree of this open pull request, by number or URL.
    pub from_pr: Option<String>,
    /// Work in this existing directory.
    pub cwd: Option<PathBuf>,
}

impl ToolArgs for AgentArgs {
    fn refusal(&self) -> Option<String> {
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
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeamArgs {
    /// The configured team to launch.
    pub team: String,
    /// The brief, which goes to the team's leader.
    pub prompt: String,
    /// The team's worktree. A new one branches from `agents.worktree.base` when set, else from
    /// origin's default branch, fetched at launch.
    pub worktree: Option<String>,
    /// Work in a worktree of this open pull request, by number or URL.
    pub from_pr: Option<String>,
    /// A label each seat carries in the sidebar, a few words; `tasks` shows the team's goal.
    pub description: Option<String>,
}

impl ToolArgs for TeamArgs {
    fn refusal(&self) -> Option<String> {
        if let Some(blank) = blank_field(&[("team", &self.team), ("prompt", &self.prompt)]) {
            return Some(blank);
        }
        if self.worktree.is_some() == self.from_pr.is_some() {
            return Some("give exactly one of `worktree` and `from_pr`".to_owned());
        }
        blank_place(self.worktree.as_deref(), self.from_pr.as_deref())
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowArgs {
    /// The deployed workflow's name; a path is refused.
    pub name: String,
    /// Run in this RimZ-owned worktree, created when missing; refused by a project-scoped
    /// workflow.
    pub worktree: Option<String>,
    /// Text added to every agent and message step's prompt in this run.
    pub prompt: Option<String>,
    /// The start node's input: an object with a string `outcome`, and a `key`, when given,
    /// that is a string or an integer and becomes the run's key.
    pub input: Option<serde_json::Map<String, serde_json::Value>>,
    /// Replace the definition's deadline: a duration, `HH:MM`, `<weekday> HH:MM`, or a date.
    pub deadline: Option<String>,
    /// Give up capacity waits at this time; takes what `deadline` takes.
    pub give_up: Option<String>,
    /// Launch agent steps past every capacity hold.
    #[serde(default)]
    pub skip_capacity: bool,
    /// Send no report when the run ends, for a run you will not be there for.
    #[serde(default)]
    pub detach: bool,
}

impl ToolArgs for WorkflowArgs {
    fn refusal(&self) -> Option<String> {
        let mut components = std::path::Path::new(&self.name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) {
            return Some("`name` is a deployed workflow's name, not a path".to_owned());
        }
        self.worktree
            .as_deref()
            .is_some_and(|worktree| worktree.trim().is_empty())
            .then(|| "`worktree` is blank".to_owned())
    }
}

/// When a booking fires: exactly one of `at`, `in`, `every`, `cron`, `signal`, or `when`. A
/// provider keeps no `oneOf`, so `refusal` holds the choice.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleTrigger {
    /// Once, at this clock time.
    pub at: Option<String>,
    /// Once, after this delay.
    #[serde(rename = "in")]
    pub after: Option<String>,
    /// Repeating, at this interval.
    pub every: Option<String>,
    /// Repeating, on this cron schedule.
    pub cron: Option<String>,
    /// On this signal.
    pub signal: Option<String>,
    /// `signal` only: payload fields that must equal these values.
    #[serde(rename = "match")]
    pub matches: Option<std::collections::BTreeMap<String, String>>,
    /// When this condition reads true.
    pub when: Option<String>,
    /// `when` only: how long it must hold.
    #[serde(rename = "for")]
    pub hold: Option<String>,
}

impl ScheduleTrigger {
    fn refusal(&self) -> Option<String> {
        let forms = [
            self.at.is_some(),
            self.after.is_some(),
            self.every.is_some(),
            self.cron.is_some(),
            self.signal.is_some(),
            self.when.is_some(),
        ];
        if forms.into_iter().filter(|set| *set).count() != 1 {
            return Some(
                "`trigger` takes exactly one of `at`, `in`, `every`, `cron`, `signal`, or `when`"
                    .to_owned(),
            );
        }
        if self.matches.is_some() && self.signal.is_none() {
            return Some("`match` takes a `signal` trigger".to_owned());
        }
        if self
            .matches
            .as_ref()
            .is_some_and(|matches| matches.contains_key(""))
        {
            return Some("a `match` key is empty".to_owned());
        }
        (self.hold.is_some() && self.when.is_none())
            .then(|| "`for` takes a `when` trigger".to_owned())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleAgent {
    /// The agent profile each fire launches.
    pub profile: String,
    /// The task for each fire: its goal, and where the result goes.
    pub prompt: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleTeam {
    /// The team each fire launches.
    pub team: String,
    /// The goal for the team's leader.
    pub goal: String,
}

/// What a fire launches: exactly one of `agent`, `team`, or `workflow`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRun {
    /// Launch a fresh agent.
    pub agent: Option<ScheduleAgent>,
    /// Launch a team.
    pub team: Option<ScheduleTeam>,
    /// Start a run of a deployed workflow.
    pub workflow: Option<WorkflowArgs>,
}

/// Why a value a fire's launch passes as a word of its own would read as a flag there.
fn flag_refusal(field: &str, value: &str) -> Option<String> {
    value
        .starts_with('-')
        .then(|| format!("`{field}` starts with `-`, which a fire's launch reads as a flag"))
}

impl ScheduleRun {
    /// Why this target could never launch from a fire: each refusal is one the check of
    /// `rimz loop add` would make of the stored launch, or one its fire's spawn would make.
    fn refusal(&self) -> Option<String> {
        let blank = |field: &str, value: &str| {
            value
                .trim()
                .is_empty()
                .then(|| format!("`{field}` is blank"))
        };
        // A free-text positional takes a leading `- `, as a bulleted prompt has; any other
        // leading `-` parses as an option.
        let text_flag = |field: &str, value: &str| {
            let rest = value.strip_prefix('-')?;
            if rest.starts_with(char::is_whitespace) {
                return None;
            }
            flag_refusal(field, value)
        };
        let nul = |field: &str, value: Option<&str>| {
            value.is_some_and(|value| value.contains('\0')).then(|| {
                format!("`{field}` holds a NUL character, which a fire's launch cannot pass")
            })
        };
        match (&self.agent, &self.team, &self.workflow) {
            (Some(agent), None, None) => blank("run.agent.profile", &agent.profile)
                .or_else(|| blank("run.agent.prompt", &agent.prompt))
                .or_else(|| flag_refusal("run.agent.profile", &agent.profile))
                .or_else(|| text_flag("run.agent.prompt", &agent.prompt))
                .or_else(|| nul("run.agent.prompt", Some(&agent.prompt)))
                .or_else(|| {
                    agent.profile.starts_with(['@', '#']).then(|| {
                        "`run.agent.profile` is a profile, not an address or scope".to_owned()
                    })
                })
                .or_else(|| {
                    // `launch` is the verb a launch spec may follow, so a spec named that is a
                    // verb too.
                    let launches = agent.profile != "launch"
                        && crate::pane::command_is_launch_chrome(&format!(
                            "rimz agents {} prompt",
                            agent.profile
                        ));
                    (!launches).then(|| {
                        format!(
                            "`run.agent.profile` `{}` is a `rimz agents` verb, which a fire's \
                             launch reads in place of a profile",
                            agent.profile
                        )
                    })
                }),
            (None, Some(team), None) => blank("run.team.team", &team.team)
                .or_else(|| flag_refusal("run.team.team", &team.team))
                .or_else(|| blank("run.team.goal", &team.goal))
                .or_else(|| text_flag("run.team.goal", &team.goal))
                .or_else(|| nul("run.team.goal", Some(&team.goal))),
            (None, None, Some(workflow)) => workflow.refusal().or_else(|| {
                if let Some(reason) = flag_refusal("run.workflow.name", &workflow.name)
                    .or_else(|| {
                        workflow
                            .worktree
                            .as_deref()
                            .and_then(|name| flag_refusal("run.workflow.worktree", name))
                    })
                    .or_else(|| nul("run.workflow.prompt", workflow.prompt.as_deref()))
                {
                    return Some(reason);
                }
                if workflow.input.is_some() {
                    return Some(
                        "`run.workflow.input`: a fire starts a run with no input".to_owned(),
                    );
                }
                workflow.detach.then(|| {
                    "`run.workflow.detach`: a fire's run owes its task a report".to_owned()
                })
            }),
            _ => Some("`run` takes exactly one of `agent`, `team`, or `workflow`".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleOverlap {
    Skip,
    Coalesce,
    Queue,
    Replace,
    Allow,
}

/// How a message reaches its receiver, as `rimz message` sends it bare, with `--steer`, or
/// with `--interrupt`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MessageMode {
    #[default]
    Park,
    Steer,
    Interrupt,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleArgs {
    /// The booking's name; booking a name you hold replaces it.
    pub name: String,
    /// When it fires: one of `at`, `in`, `every`, `cron`, `signal` with optional `match`, or
    /// `when` with optional `for`.
    pub trigger: ScheduleTrigger,
    /// `signal` or `when` only: fire on every match rather than once.
    #[serde(default)]
    pub repeat: bool,
    /// What each fire launches: `agent`, `team`, or `workflow`.
    pub run: ScheduleRun,
    /// Run every fire in this RimZ-owned worktree, created at the first fire when missing.
    pub worktree: Option<String>,
    /// A repeating booking's answer to a trigger during a live fire; `skip` by default.
    pub overlap: Option<ScheduleOverlap>,
    /// Disable after this many failed fires in a row; 3 by default, 0 never.
    pub max_strikes: Option<u32>,
}

impl ToolArgs for ScheduleArgs {
    fn refusal(&self) -> Option<String> {
        if let Some(reason) = self.trigger.refusal() {
            return Some(reason);
        }
        let by_clock = self.trigger.every.is_some() || self.trigger.cron.is_some();
        let by_event = self.trigger.signal.is_some() || self.trigger.when.is_some();
        if self.repeat && !by_event {
            return Some("`repeat` takes a `signal` or `when` trigger".to_owned());
        }
        if self.overlap.is_some() && !by_clock && !self.repeat {
            return Some("`overlap` takes a repeating trigger".to_owned());
        }
        if let Some(name) = self.worktree.as_deref() {
            if name.trim().is_empty() {
                return Some("`worktree` is blank".to_owned());
            }
            if let Some(reason) = flag_refusal("worktree", name) {
                return Some(reason);
            }
        }
        self.run.refusal()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageArgs {
    /// The receiver: `@handle`, or `@handle#channel` in another channel.
    pub to: String,
    /// The message, delivered as written.
    pub text: String,
    /// `park` (default) lands it now or at the next turn boundary, `steer` into the running
    /// turn, `interrupt` stops the turn and then types it.
    #[serde(default)]
    pub mode: MessageMode,
    /// Send to each agent `to` matches, where it matches several.
    #[serde(default)]
    pub all: bool,
    /// Mark the receiver's header as wanting a message back.
    #[serde(default)]
    pub reply_wanted: bool,
}

impl ToolArgs for MessageArgs {
    fn refusal(&self) -> Option<String> {
        [("to", &self.to), ("text", &self.text)]
            .into_iter()
            .find(|(_, value)| value.trim().is_empty())
            .map(|(field, _)| format!("`{field}` is blank"))
    }
}

/// A kind of thing a tool starts, as an id names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StartedKind {
    Subagent,
    Agent,
    Team,
    Workflow,
    Schedule,
    Wait,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TasksArgs {
    /// List only things of this kind.
    pub kind: Option<StartedKind>,
}

impl ToolArgs for TasksArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinArgs {
    /// The id `tasks` or a start answered, as `subagent:warm-haven`.
    pub id: String,
    /// How long to block (`90s`, `4m`); 4m when absent, at most 9m.
    pub timeout: Option<String>,
}

impl ToolArgs for JoinArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopArgs {
    /// The id `tasks` or a start answered, as `subagent:warm-haven`.
    pub id: String,
}

impl ToolArgs for StopArgs {}

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
pub struct SelfMoltArgs {
    /// The new session's first prompt: the path of your molt note, then the task it starts on.
    pub prompt: String,
    /// The profile, model alias, or kind that takes over; your own profile when absent.
    pub agent: Option<String>,
    /// How long this session rests after this turn before it closes (`s`/`m`/`h`/`d`); it
    /// closes when this turn ends when absent.
    pub when_idle: Option<String>,
}

impl ToolArgs for SelfMoltArgs {
    fn refusal(&self) -> Option<String> {
        self.prompt
            .trim()
            .is_empty()
            .then(|| "`prompt` is empty".to_owned())
    }
}

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
pub struct NotesArgs {
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

impl ToolArgs for NotesArgs {
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
/// exact call that stops it, how its result reaches the caller, and any
/// `warnings` from the start.
fn start_schema() -> Value {
    object(
        serde_json::json!({
            "id": string(), "status": string(), "stop": string(), "report": string(),
            "warnings": array_of(string()),
        }),
        &["id", "status", "stop", "report"],
    )
}

/// One row per message `message` recorded, with the agents compacted first and the warnings
/// of the send.
fn message_schema() -> Value {
    let row = object(
        serde_json::json!({
            "to": string(),
            "message_id": string(),
            "status": { "type": "string", "enum": ["sent", "queued", "not_sent"] },
            "detail": string(),
        }),
        &["to", "message_id", "status"],
    );
    object(
        serde_json::json!({
            "messages": array_of(row),
            "compacted": array_of(string()),
            "warnings": array_of(string()),
        }),
        &["messages"],
    )
}

/// One row per thing the caller started, by `tasks`.
fn tasks_schema() -> Value {
    let row = object(
        serde_json::json!({
            "id": string(),
            "kind": string(),
            "status": { "type": "string", "enum": ["running", "done", "disabled"] },
            "on_your_stop": { "type": "string", "enum": ["holds", "closes", "outlives"] },
            "age": string(),
            "about": string(),
            "detail": string(),
        }),
        &["id", "kind", "status", "on_your_stop"],
    );
    object(serde_json::json!({ "tasks": array_of(row) }), &["tasks"])
}

/// What `join` answers: the result of a thing that is done, that one still
/// runs, or for a schedule or a wait how its outcome comes.
fn join_schema() -> Value {
    object(
        serde_json::json!({
            "id": string(),
            "status": { "type": "string", "enum": ["done", "running", "ended", "disabled"] },
            "result": { "type": ["string", "null"] },
            "detail": string(),
            "report": string(),
            "hint": string(),
        }),
        &["id", "status"],
    )
}

/// What `stop` answers: that the thing stopped, or had already ended, and
/// what it left running.
fn stop_schema() -> Value {
    object(
        serde_json::json!({
            "id": string(),
            "status": { "type": "string", "enum": ["stopped", "ended"] },
            "still_running": array_of(string()),
            "hint": string(),
        }),
        &["id", "status"],
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
            "status": { "type": "string", "enum": ["sent", "queued", "joined"] },
            "continues": { "type": "boolean" },
        }),
        &["message_id", "status"],
    )
}

fn self_molt_schema() -> Value {
    object(
        serde_json::json!({
            "agent": string(),
            "successor_kind": string(),
            "closes_after_secs": { "type": "integer" },
            "notices": array_of(string()),
        }),
        &["agent", "successor_kind", "closes_after_secs", "notices"],
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

fn notes_schema() -> Value {
    object(
        serde_json::json!({
            "path": string(),
            "dropped_meta": array_of(string()),
            "unresolved_project": { "type": ["string", "null"] },
        }),
        &["path", "dropped_meta"],
    )
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
