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
use crate::harness::block::schema;

/// The name a provider registers RimZ's server under; Claude spells a tool
/// `mcp__rimz__<tool>`.
pub const SERVER_NAME: &str = "rimz";

/// One group of tools a profile's `mcps` list and `--only` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Lsp,
    Shell,
    Watch,
    Subagents,
    Agents,
    Teams,
    Workflow,
    Loop,
    Message,
    Ask,
    Control,
    Notes,
    Blackboard,
}

impl Capability {
    pub const ALL: [Capability; 13] = [
        Self::Lsp,
        Self::Shell,
        Self::Watch,
        Self::Subagents,
        Self::Agents,
        Self::Teams,
        Self::Workflow,
        Self::Loop,
        Self::Message,
        Self::Ask,
        Self::Control,
        Self::Notes,
        Self::Blackboard,
    ];

    /// Whether the group's tools start things, which `tasks`, `join`, and `stop` list, await,
    /// and end.
    pub fn starts_things(self) -> bool {
        matches!(
            self,
            Self::Shell
                | Self::Watch
                | Self::Subagents
                | Self::Agents
                | Self::Teams
                | Self::Workflow
                | Self::Loop
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
            Self::Shell => "rimz-shell",
            Self::Watch => "rimz-watch",
            Self::Subagents => "rimz-subagents",
            Self::Agents => "rimz-agents",
            Self::Teams => "rimz-teams",
            Self::Workflow => "rimz-workflow",
            Self::Loop => "rimz-loop",
            Self::Message => "rimz-message",
            Self::Ask => "rimz-ask",
            Self::Control => "rimz-control",
            Self::Notes => "rimz-notes",
            Self::Blackboard => "rimz-blackboard",
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
    Shell(ShellArgs),
    Watch(WatchArgs),
    Subagent(SubagentArgs),
    Agent(AgentArgs),
    Team(TeamArgs),
    Workflow(WorkflowArgs),
    Schedule(Box<ScheduleArgs>),
    Message(MessageArgs),
    Ask(AskArgs),
    Tasks(TasksArgs),
    Join(JoinArgs),
    Stop(StopArgs),
    SelfStop(SelfStopArgs),
    SelfCompact(SelfCompactArgs),
    SelfTune(SelfTuneArgs),
    SelfMolt(SelfMoltArgs),
    Notes(NotesArgs),
    BlackboardFlip(BlackboardFlipArgs),
    BlackboardRecord(BlackboardRecordArgs),
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

    /// The schema every data answer of the tool fits; `None` for `lsp`,
    /// `blackboard_record`, and `guide`, which answer text.
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

    /// The characters of every description the tool's input schema carries,
    /// top-level and nested, which a provider may count with the tool's own
    /// description.
    pub(super) fn argument_text_len(&self) -> usize {
        schema_description_len(&(self.schema)())
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

pub static TOOLS: [Tool; 21] = [
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
        name: "shell",
        capability: Some(Capability::Shell),
        purpose: Purpose::Start,
        read_only: false,
        command: "shell",
        declared: &[],
        verbs: &[],
        schema: schema::<ShellArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<ShellArgs>(arguments).map(Call::Shell),
    },
    Tool {
        name: "watch",
        capability: Some(Capability::Watch),
        purpose: Purpose::Start,
        read_only: false,
        command: "watch",
        declared: &[],
        verbs: &[],
        schema: schema::<WatchArgs>,
        output: Some(start_schema),
        parse: |arguments| parse::<WatchArgs>(arguments).map(Call::Watch),
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
        schema: schema::<ScheduleInput>,
        output: Some(start_schema),
        parse: parse_schedule,
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
        name: "ask",
        capability: Some(Capability::Ask),
        purpose: Purpose::Ask,
        read_only: false,
        command: "asks raise",
        declared: &[],
        verbs: &[],
        schema: schema::<AskArgs>,
        output: Some(ask_schema),
        parse: |arguments| parse::<AskArgs>(arguments).map(Call::Ask),
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
            "shell cancel",
            "watch cancel",
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
        name: "blackboard_flip",
        capability: Some(Capability::Blackboard),
        purpose: Purpose::Board,
        read_only: false,
        command: "teams flip",
        declared: &[],
        verbs: &[],
        schema: schema::<BlackboardFlipArgs>,
        output: Some(blackboard_flip_schema),
        parse: |arguments| parse::<BlackboardFlipArgs>(arguments).map(Call::BlackboardFlip),
    },
    Tool {
        name: "blackboard_record",
        capability: Some(Capability::Blackboard),
        purpose: Purpose::Board,
        read_only: false,
        command: "teams record",
        declared: &[],
        verbs: &[],
        schema: schema::<BlackboardRecordArgs>,
        output: None,
        parse: |arguments| parse::<BlackboardRecordArgs>(arguments).map(Call::BlackboardRecord),
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
    Ask,
    Started,
    Yourself,
    Notes,
    Board,
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
            Self::Ask => "ask the user",
            Self::Started => "track what you started",
            Self::Yourself => "manage yourself",
            Self::Notes => "keep notes",
            Self::Board => "write your team's board",
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
/// `## <tool>` heading with its size, capability, and the load `load` names
/// for it: what the server sends for `tools` of `groups` when `modes` gives
/// their load modes.
/// The instructions' heading and a tool's line end with `text <path>` for each
/// part whose text is the user's file.
pub fn render_listing(
    tools: &[&Tool],
    groups: &[Capability],
    modes: &[(Capability, LoadMode)],
    load: impl Fn(&Tool) -> &'static str,
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
        let load = load(tool);
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

fn schema_description_len(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, child)| match child {
                Value::String(text) if key == "description" => text.chars().count(),
                _ => schema_description_len(child),
            })
            .sum(),
        Value::Array(items) => items.iter().map(schema_description_len).sum(),
        _ => 0,
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
pub enum ShellOn {
    Fail,
    Success,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellArgs {
    /// The whole command as one string, run once through POSIX `sh -c` at the checkout's root
    /// with stdin closed, stdout and stderr going to the shell's output file.
    pub command: String,
    /// Wake only on this outcome (default either; `fail` for a server left running).
    pub on: Option<ShellOn>,
    /// What you will do on waking, 1 to 60 characters.
    pub label: Option<String>,
    /// One still-running notice after this long, whatever `on` says (never a stop); 30m when
    /// absent, or none while cache keepalive pings you. Set one longer than the command should
    /// take when it could hang.
    pub check_in: Option<String>,
    /// Checkout to run in; defaults to the session's start directory.
    pub cwd: Option<PathBuf>,
}

impl ToolArgs for ShellArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchArgs {
    /// A condition, firing once it reads true, already true when set included.
    pub when: String,
    /// What you will do on waking, 1 to 60 characters; the wake carries only what fired.
    pub label: Option<String>,
    /// One still-not-met notice after this long (never a stop); 30m when absent, or none while
    /// cache keepalive pings you.
    pub check_in: Option<String>,
    /// Checkout to arm in, whose branch bare forge terms read; defaults to the session's start
    /// directory.
    pub cwd: Option<PathBuf>,
}

impl ToolArgs for WatchArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentArgs {
    /// Subagent profile from your Subagents reminder.
    pub profile: String,
    /// The brief, the only context the child gets.
    pub prompt: String,
    /// Short label for `tasks` and the sidebar.
    pub description: Option<String>,
    /// Checkout where the child works; defaults to yours.
    pub cwd: Option<PathBuf>,
    /// Cut the child off after this long (`90m`, `2h`); defaults to the profile's timeout, else
    /// `30m`.
    pub timeout: Option<String>,
    /// Override the profile's model; omit unless the user asks.
    pub model: Option<String>,
    /// Override the profile's reasoning effort; omit unless the user asks.
    pub effort: Option<String>,
    /// Inherit this profile (subagent or agent) or provider kind below the child profile; omit
    /// unless the user asks.
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
    /// Agent profile (from `rimz agents profiles`) or bare model; one seat, never a team.
    pub profile: String,
    /// The brief: the agent's first turn.
    pub prompt: String,
    /// Label for `tasks` and the sidebar, 3 to 5 words.
    pub description: Option<String>,
    /// Distinct handle when the channel already holds this profile's (including yours).
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
    /// Configured team to launch (from `rimz teams list`).
    pub team: String,
    /// The brief, delivered to the team's leader.
    pub prompt: String,
    /// Kebab-case worktree name (at most two words); give exactly one of `worktree` and `from_pr`.
    /// A new one branches from `agents.worktree.base` when set, else from origin's default branch
    /// fetched at launch (or `HEAD` with no origin).
    pub worktree: Option<String>,
    /// Open pull request, by number or URL, to create or reuse a worktree for.
    pub from_pr: Option<String>,
    /// Label each seat carries in the sidebar, a few words; `tasks` shows the team's goal.
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
    /// Deployed workflow name (not a path).
    pub name: String,
    /// RimZ-owned worktree to run in, created when missing; refused by a `project`-scoped
    /// workflow.
    pub worktree: Option<String>,
    /// Text added to every agent and message step's prompt in this run.
    pub prompt: Option<String>,
    /// Start node input: an object with a string `outcome`, and optional string or integer `key`
    /// (becomes the run's key).
    pub input: Option<serde_json::Map<String, serde_json::Value>>,
    /// Replace the definition's deadline: a duration (`8h`), `HH:MM`, `<weekday> HH:MM`
    /// (`fri 18:00`), or date (`2026-11-21`, midnight at its start).
    pub deadline: Option<String>,
    /// When to give up capacity waits; accepts the same formats as `deadline`.
    pub give_up: Option<String>,
    /// Launch agent steps past every capacity hold.
    #[serde(default)]
    pub skip_capacity: bool,
    /// Send no report when the run ends and do not hold this session open.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleTrigger {
    At,
    In,
    Every,
    Cron,
    Signal,
    When,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ScheduleTool {
    Agent,
    Team,
    Workflow,
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

/// What a fire launches, parsed through the named tool's own argument parser and `refusal`.
#[derive(Debug)]
pub enum ScheduleTarget {
    Agent(AgentArgs),
    Team(TeamArgs),
    Workflow(WorkflowArgs),
}

// Wire schema for `schedule`; `parse_schedule` turns it into a validated `ScheduleArgs`.
// `tool` and `arguments` carry the launch tool's own call, which `parse_schedule` hands to that
// tool's parser: a field of the carried tool is never copied into this struct.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScheduleInput {
    /// Booking name; reusing a name you hold replaces it (canceling unstarted live runs and
    /// clearing strikes and history), while another starter's name is refused.
    name: String,
    /// What fires the booking.
    trigger: ScheduleTrigger,
    /// The trigger's argument: a clock time, delay, interval, cron expression, signal name, or
    /// condition.
    value: String,
    /// Launch tool each fire invokes.
    tool: ScheduleTool,
    /// Arguments for `tool`, in that tool's shape without `worktree` (pass top-level `worktree`).
    arguments: serde_json::Map<String, Value>,
    /// `signal` only: payload fields that must equal these values (`*` matches any value).
    #[serde(rename = "match")]
    matches: Option<BTreeMap<String, String>>,
    /// `when` only: duration the condition must hold before firing.
    #[serde(rename = "for")]
    hold: Option<String>,
    /// `signal` or `when` only: fire on every match rather than once (default once).
    #[serde(default)]
    repeat: bool,
    /// RimZ-owned worktree for every fire, created at the first fire when missing.
    worktree: Option<String>,
    /// Repeating triggers only: answer to a trigger during a live fire (`skip` default;
    /// `coalesce` or `queue` hold it, `replace` stops the live fire first, `allow` launches beside
    /// it for `agent` only).
    overlap: Option<ScheduleOverlap>,
    /// Disable after this many failed fires in a row (default `3`; `0` never).
    max_strikes: Option<u32>,
}

/// A validated `schedule` call, with `arguments` parsed into its typed launch target.
#[derive(Debug)]
pub struct ScheduleArgs {
    pub name: String,
    pub trigger: ScheduleTrigger,
    pub value: String,
    pub matches: Option<BTreeMap<String, String>>,
    pub hold: Option<String>,
    pub repeat: bool,
    pub target: ScheduleTarget,
    pub worktree: Option<String>,
    pub overlap: Option<ScheduleOverlap>,
    pub max_strikes: Option<u32>,
}

impl ScheduleArgs {
    /// Whether the booking's trigger can fire more than once.
    pub fn repeats(&self) -> bool {
        matches!(self.trigger, ScheduleTrigger::Every | ScheduleTrigger::Cron) || self.repeat
    }
}

/// Why a value a fire's launch passes as a word of its own would read as a flag there.
fn flag_refusal(field: &str, value: &str) -> Option<String> {
    value
        .starts_with('-')
        .then(|| format!("`{field}` starts with `-`, which a fire's launch reads as a flag"))
}

/// A free-text positional takes a leading `- `, as a bulleted prompt has; any other leading `-`
/// parses as an option.
fn text_flag_refusal(field: &str, value: &str) -> Option<String> {
    let rest = value.strip_prefix('-')?;
    if rest.starts_with(char::is_whitespace) {
        return None;
    }
    flag_refusal(field, value)
}

fn nul_refusal(field: &str, value: Option<&str>) -> Option<String> {
    value
        .is_some_and(|value| value.contains('\0'))
        .then(|| format!("`{field}` holds a NUL character, which a fire's launch cannot pass"))
}

fn parse_carried<T: ToolArgs>(arguments: Value) -> Result<T, serde_json::Error> {
    parse::<T>(arguments).map_err(|err| {
        let mut reason = err.to_string();
        if let Some((head, tail)) = reason.split_once("expected one of ") {
            let tail = tail.replace(", `worktree`", "").replace("`worktree`, ", "");
            reason = format!("{head}expected one of {tail}");
        }
        let formatted = if reason.contains("`worktree`") {
            format!("`arguments` (with top-level `worktree`): {reason}")
        } else {
            format!("`arguments`: {reason}")
        };
        serde::de::Error::custom(formatted)
    })
}

fn parse_schedule(raw: Value) -> Result<Call, serde_json::Error> {
    let input = serde_json::from_value::<ScheduleInput>(raw)?;
    if let Some(reason) = schedule_input_refusal(&input) {
        return Err(serde::de::Error::custom(reason));
    }
    if input.arguments.contains_key("worktree") {
        return Err(serde::de::Error::custom(
            "`arguments.worktree`: pass top-level `worktree` instead",
        ));
    }
    let mut forwarded = input.arguments;
    if let Some(worktree) = &input.worktree {
        forwarded.insert("worktree".to_owned(), Value::String(worktree.clone()));
    }
    let forwarded = Value::Object(forwarded);
    let target = match input.tool {
        ScheduleTool::Agent => ScheduleTarget::Agent(parse_carried::<AgentArgs>(forwarded)?),
        ScheduleTool::Team => ScheduleTarget::Team(parse_carried::<TeamArgs>(forwarded)?),
        ScheduleTool::Workflow => {
            ScheduleTarget::Workflow(parse_carried::<WorkflowArgs>(forwarded)?)
        }
    };
    let args = ScheduleArgs {
        name: input.name,
        trigger: input.trigger,
        value: input.value,
        matches: input.matches,
        hold: input.hold,
        repeat: input.repeat,
        target,
        worktree: input.worktree,
        overlap: input.overlap,
        max_strikes: input.max_strikes,
    };
    if let Some(reason) = schedule_target_refusal(&args) {
        return Err(serde::de::Error::custom(reason));
    }
    Ok(Call::Schedule(Box::new(args)))
}

fn schedule_input_refusal(input: &ScheduleInput) -> Option<String> {
    if input.matches.is_some() && input.trigger != ScheduleTrigger::Signal {
        return Some("`match` takes a `signal` trigger".to_owned());
    }
    if input
        .matches
        .as_ref()
        .is_some_and(|matches| matches.contains_key(""))
    {
        return Some("a `match` key is empty".to_owned());
    }
    if input.hold.is_some() && input.trigger != ScheduleTrigger::When {
        return Some("`for` takes a `when` trigger".to_owned());
    }
    let by_clock = matches!(
        input.trigger,
        ScheduleTrigger::Every | ScheduleTrigger::Cron
    );
    let by_event = matches!(
        input.trigger,
        ScheduleTrigger::Signal | ScheduleTrigger::When
    );
    if input.repeat && !by_event {
        return Some("`repeat` takes a `signal` or `when` trigger".to_owned());
    }
    if input.overlap.is_some() && !by_clock && !input.repeat {
        return Some("`overlap` takes a repeating trigger".to_owned());
    }
    if let Some(name) = input.worktree.as_deref() {
        if name.trim().is_empty() {
            return Some("`worktree` is blank".to_owned());
        }
        if let Some(reason) =
            flag_refusal("worktree", name).or_else(|| nul_refusal("worktree", Some(name)))
        {
            return Some(reason);
        }
    }
    if let Some(reason) = nul_refusal("value", Some(&input.value))
        .or_else(|| nul_refusal("for", input.hold.as_deref()))
    {
        return Some(reason);
    }
    if let Some(matches) = &input.matches {
        for (key, value) in matches {
            if let Some(reason) =
                nul_refusal("match", Some(key)).or_else(|| nul_refusal("match", Some(value)))
            {
                return Some(reason);
            }
        }
    }
    None
}

/// Refusals particular to a fire beyond what the carried tool's own `ToolArgs::refusal` holds:
/// each is one `rimz loop add`'s check or a fire's child launch would refuse.
fn schedule_target_refusal(args: &ScheduleArgs) -> Option<String> {
    match &args.target {
        ScheduleTarget::Agent(agent) => {
            let profile = agent.profile.trim();
            nul_refusal("arguments.profile", Some(&agent.profile))
                .or_else(|| flag_refusal("arguments.profile", profile))
                .or_else(|| {
                    profile.starts_with(['@', '#']).then(|| {
                        "`arguments.profile` is a profile, not an address or scope".to_owned()
                    })
                })
                .or_else(|| {
                    // `launch` is the verb a launch spec may follow, so a spec named that is a
                    // verb too.
                    let launches = profile != "launch"
                        && crate::pane::command_is_launch_chrome(&format!(
                            "rimz agents {profile} prompt"
                        ));
                    (!launches).then(|| {
                        format!(
                            "`arguments.profile` `{profile}` is a `rimz agents` verb, which a \
                             fire's launch reads in place of a profile"
                        )
                    })
                })
                .or_else(|| text_flag_refusal("arguments.prompt", &agent.prompt))
                .or_else(|| nul_refusal("arguments.prompt", Some(&agent.prompt)))
                .or_else(|| nul_refusal("arguments.description", agent.description.as_deref()))
                .or_else(|| nul_refusal("arguments.name", agent.name.as_deref()))
                .or_else(|| nul_refusal("arguments.agent", agent.agent.as_deref()))
                .or_else(|| nul_refusal("arguments.from_pr", agent.from_pr.as_deref()))
                .or_else(|| {
                    let cwd = agent.cwd.as_deref().map(|path| path.to_string_lossy());
                    nul_refusal("arguments.cwd", cwd.as_deref())
                })
        }
        ScheduleTarget::Team(team) => {
            let team_name = team.team.trim();
            nul_refusal("arguments.team", Some(&team.team))
                .or_else(|| flag_refusal("arguments.team", team_name))
                .or_else(|| {
                    team_name.contains('#').then(|| {
                        "`arguments.team` cannot include `#lane`; pass top-level `worktree` \
                         instead"
                            .to_owned()
                    })
                })
                .or_else(|| text_flag_refusal("arguments.prompt", &team.prompt))
                .or_else(|| nul_refusal("arguments.prompt", Some(&team.prompt)))
                .or_else(|| nul_refusal("arguments.description", team.description.as_deref()))
                .or_else(|| nul_refusal("arguments.from_pr", team.from_pr.as_deref()))
                .or_else(|| {
                    (args.repeats() && team.from_pr.is_some()).then(|| {
                        "`arguments.from_pr`: a repeating team booking cannot use `from_pr`; set \
                         top-level `worktree` or use a one-shot trigger"
                            .to_owned()
                    })
                })
        }
        ScheduleTarget::Workflow(workflow) => {
            if workflow.name.trim().is_empty() {
                return Some("`arguments.name` is blank".to_owned());
            }
            nul_refusal("arguments.name", Some(&workflow.name))
                .or_else(|| flag_refusal("arguments.name", &workflow.name))
                .or_else(|| nul_refusal("arguments.prompt", workflow.prompt.as_deref()))
                .or_else(|| nul_refusal("arguments.deadline", workflow.deadline.as_deref()))
                .or_else(|| nul_refusal("arguments.give_up", workflow.give_up.as_deref()))
                .or_else(|| {
                    workflow
                        .input
                        .is_some()
                        .then(|| "`arguments.input`: a fire starts a run with no input".to_owned())
                })
                .or_else(|| {
                    workflow.detach.then(|| {
                        "`arguments.detach`: a fire's run owes its task a report".to_owned()
                    })
                })
        }
    }
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
    Shell,
    Watch,
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
pub struct AskArgs {
    /// The question in one sentence for your card, which cuts it at 160 characters.
    pub brief: String,
}

impl ToolArgs for AskArgs {
    fn refusal(&self) -> Option<String> {
        self.brief
            .trim()
            .is_empty()
            .then(|| "`brief` is empty".to_owned())
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
pub struct BlackboardFlipArgs {
    /// The next stage by its exact name, or `Done`.
    pub stage: String,
    /// What is done; the reasoning stays in your stage file.
    pub note: String,
}

impl ToolArgs for BlackboardFlipArgs {
    fn refusal(&self) -> Option<String> {
        blank_field(&[("stage", &self.stage), ("note", &self.note)])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RecordSection {
    Goal,
    Decisions,
    Result,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlackboardRecordArgs {
    pub section: RecordSection,
    /// The entry.
    pub text: String,
}

impl ToolArgs for BlackboardRecordArgs {
    fn refusal(&self) -> Option<String> {
        blank_field(&[("text", &self.text)])
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
/// runs, or for a schedule or a watch how its outcome comes.
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

fn ask_schema() -> Value {
    object(
        serde_json::json!({
            "id": string(),
            "status": { "type": "string", "enum": ["armed"] },
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

/// The stage a flip opened and, short of `Done`, its owner: `@role`, or `you`.
fn blackboard_flip_schema() -> Value {
    object(
        serde_json::json!({ "stage": string(), "owner": string() }),
        &["stage"],
    )
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
