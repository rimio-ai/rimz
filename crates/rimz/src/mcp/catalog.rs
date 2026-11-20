//! The one table of RimZ MCP tools: each tool's capability, description,
//! argument schema, and the `rimz` command its call runs. `rimz mcp serve`,
//! `rimz mcp tools`, their `--only` values, the profile `mcps` capability
//! names, the reference page's tool table, and the routine-command check all
//! read this table.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The name a provider registers RimZ's server under; Claude spells a tool
/// `mcp__rimz__<tool>`.
pub const SERVER_NAME: &str = "rimz";

/// The server's `instructions`, which a provider shows beside its tools.
pub const INSTRUCTIONS: &str = "Use `lsp` for what code means where grep cannot tell a call from a \
                                mention, `wait` over a sleep, poll, or background command, \
                                `notes_*` to keep or find notes across sessions, and `self_*` when \
                                the user asks you to stop, compact, or tune yourself.\nEach tool \
                                runs one `rimz` command as this agent, in its room, and returns \
                                that command's output: its stdout, then a `stderr:` line and its \
                                stderr when it wrote any. A command that exits nonzero returns an \
                                error result whose last line is `exit code N`. Arguments that do \
                                not fit a tool's schema return `invalid arguments: ...`, and \
                                nothing runs. A launch may serve only some of these tools.";

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

    /// The listing entry's estimated size in tokens, as `rimz mcp tools` and
    /// `rimz agents explain --mcps` print it.
    pub fn tokens(&self) -> u64 {
        crate::utils::tokens::estimate(&self.listing().to_string())
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
        description: "Ask the checkout's shared language server what code means. It resolves a \
                      name as the compiler does, through imports, re-exports, and macros, where \
                      grep also matches comments, strings, and same-named methods of other types; \
                      keep grep for text (a log message, a string literal, a config \
                      key).\n\nVerbs: `def` (definition and line span), `hover` (type and docs), \
                      `refs` (every use site), `callers` (functions whose bodies call it), \
                      `callees` (functions it calls), `impl` (implementations of a trait), \
                      `symbols` (a file's outline), `find` (workspace symbols by fuzzy name), \
                      `show` (numbered source), `check` (the code anchors in Markdown \
                      files).\n\nEvery verb takes several targets: pass all of a question's \
                      targets in one call. A server still starting or indexing holds each call up \
                      to two minutes, a wait a loop of calls pays every time.\n\nA target of \
                      `def`, `hover`, `refs`, `callers`, `callees`, or `impl` is one of:\n- a name \
                      as written in code, `Parser::parse`; any segment but the last may be \
                      dropped, from the front or the middle, and a wrong segment fails;\n- an \
                      anchor, `path::Symbol`;\n- `path:line:name`, a grep hit's `path:line` plus \
                      one identifier on that line; a name that occurs several times there fails, \
                      listing each occurrence as a position;\n- a position, `path:line:col`, \
                      1-based, the column anywhere on the name's last segment.\nThe last two reach \
                      a local binding, or an item of a dependency or the standard library, where \
                      your code declares or uses it. Every name and position a result prints is \
                      itself a valid target. A path is absolute or relative to the checkout \
                      root.\n\n`show` prints numbered source for a name, an anchor, \
                      `path:line:col`, `path:line`, or a line range `path:N-N`. A position or a \
                      `path:line` prints the item enclosing it, the way to read the function \
                      around a `refs` hit. An item over 200 lines prints as an outline unless \
                      `full`. One target `show` cannot parse fails the whole call.\n\n`refs` on a \
                      trait method also lists every implementing method; `refs` on one \
                      implementing method lists neither the trait method nor the other \
                      implementations, so before a signature change run `refs` on the trait \
                      method.\n\nA list stops at 50 results: pass `all` on the first call when the \
                      question is about every site, or `limit` for another cap. `refs`, `callers`, \
                      and `find` list test files and modules last, under a `tests:` line, so the \
                      cap cuts them first; `no_tests` drops them from `refs` and `callers`. \
                      `external` adds dependencies and the standard library to `refs`, `callers`, \
                      `callees`, `impl`, and `find` (on `find`, only path dependencies); `def` and \
                      `hover` follow a position into dependencies without it.\n\n`check` reads the \
                      anchors in a Markdown file's code spans: a symbol anchor, `path::Symbol`, \
                      passes when that file holds the symbol, and a line anchor, `path:~N`, when \
                      the file has that line. Any number right after a symbol anchor (`(N)`, `~N`, \
                      `:N`) is read as its line-span hint, so put other numbers elsewhere in the \
                      sentence. `fix` completes a short path and refreshes stale hints; `fix` with \
                      `hints` also writes a hint after every anchor that names one item. An \
                      `owner/repo@ref:` prefix marks an external anchor, left unchecked. A pass \
                      means the item exists, not that it holds the code you describe; verdicts are \
                      a recommendation.\n\nOtherwise each target of a multi-target call gets its \
                      own block and outcome, and the exit code is the most severe: rerun only the \
                      targets whose block failed. With no language server, use grep. `server` \
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
        description: "Arm a watcher that runs outside your turn and wakes you with a message: go \
                      on working or end your turn, and the outcome arrives mid-turn or as your \
                      next prompt. Use it where you would background a command, sleep, write an \
                      until or watch loop, tail a log, or poll CI or a pull request; a wait of a \
                      few seconds stays a plain sleep in your shell.\n\nPick the `trigger` by the \
                      habit it replaces; `value` is its argument:\n- `run`: a command you would \
                      background or that could outrun your tool timeout, run once; wakes on its \
                      exit, either outcome unless `on` names one.\n- `check`: an until-sleep loop \
                      on a state (a port open, a lock gone); reruns it every `every` until it \
                      succeeds (or fails, with `on: fail`). Use `every: 30s` against another \
                      host.\n- `file`: a path, firing when it is created, written, deleted, or \
                      replaced; with `grep`, on the first new complete line holding that literal, \
                      case-sensitive text. A relative path resolves from `cwd`, not the checkout \
                      root. Remove a log before rerunning a command that truncates and rewrites \
                      it.\n- `pid`: a running process, waking when it is gone, without its exit \
                      status.\n- `when`: a condition, firing once when it reads true, at the arm \
                      included.\n- `in`: a delay, one number and unit (`90s`, `2h`, `1d`), rounded \
                      up to a whole minute.\n- `at`: the next such clock time in the configured \
                      time zone: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.\n\n`run` and \
                      `check` take the whole command as one string, run through `sh -c` at the \
                      checkout root with stdin closed and stdout and stderr going to the wait's \
                      output file, whose path the wake gives.\n\nA `when`, `in`, or `at` wake \
                      carries only what fired, so give it a `label` saying what you will do on \
                      waking. When a `run`, `file`, or `pid` watch could hang, give it a \
                      `check_in` longer than it should take: one still-running notice, never a \
                      stop. A `check` probe that hangs sends no notice, so give its command a \
                      timeout of its own. A `when` takes no `check_in`; bound it with an `in` wait \
                      beside it and cancel whichever is left.\n\n`when` terms: `key=a,b` matches \
                      either value, and `key!=a,b` a known reading outside them; terms join with \
                      `&&`, `||`, `!`, and parentheses. Keys:\n- `ci`: passed, failed, pending, \
                      none\n- `pr`: open, merged, closed, none\n- `pr.queue`: queued, dequeued \
                      (left the queue unmerged), armed (auto-merge on, not yet queued), none\n- \
                      `pr.review`: draft, changes_requested, required (a required review not yet \
                      given), approved, none\n- `pr.mergeable`: mergeable, conflicting, none\n- \
                      `team.stage`: a stage your team defines (case-sensitive), or Done\n- \
                      `window.5h.left`, `window.7d.left`: the percent left in the room account's \
                      usage window, compared with `>=`, `<=`, `>`, or `<`, never `=`; it fires at \
                      the window's reset when spent. Use 50 to 80, not 100, which a spend before \
                      the check can miss.\nA bare `ci`, `pr`, `pr.<field>`, or `team.stage` reads \
                      the checkout you arm from; `[branch]` after the key reads that branch and \
                      takes whatever PR it finds there, so a reused branch name can read an old PR \
                      as merged. `none` means nothing to read: no PR or CI, an open PR outside the \
                      queue, a closed PR for a `pr.<field>` term, or no review decision. A PR in \
                      the merge queue still reads `pr=open`, so pair a queue term with it. \
                      `pr.queue`, `pr.review`, and `pr.mergeable` are GitHub only: elsewhere \
                      `pr.queue` reads `none`, and `pr.review` and `pr.mergeable` stay unread \
                      until the PR closes. Where the base requires no review, GitHub may report no \
                      decision, so `pr.review=approved` may never fire. Forge terms trail the \
                      forge by up to five minutes. For a state no term reads, poll the forge's own \
                      CLI with `check`.",
        read_only: false,
        command: "wait",
        verbs: &[],
        schema: schema::<WaitArgs>,
        invoke: invoke::<WaitArgs>,
    },
    Tool {
        name: "wait_list",
        capability: Capability::Wait,
        description: "List the waits you have armed, each with the name `wait_cancel` takes.",
        read_only: true,
        command: "wait list",
        verbs: &[],
        schema: schema::<WaitListArgs>,
        invoke: invoke::<WaitListArgs>,
    },
    Tool {
        name: "wait_cancel",
        capability: Capability::Wait,
        description: "Cancel one of your waits by name, as `wait_list` prints it, or every wait \
                      you armed with `all`. A command a `run` or `check` wait started stops; a \
                      `pid` process keeps running. Cancel the waits finished work no longer needs: \
                      an armed wait holds a booked `self_stop`.",
        read_only: false,
        command: "wait cancel",
        verbs: &[],
        schema: schema::<WaitCancelArgs>,
        invoke: invoke::<WaitCancelArgs>,
    },
    Tool {
        name: "self_stop",
        capability: Capability::Control,
        description: "When the user wants your pane closed, book your own stop: the pane closes \
                      once you have rested `when_idle` (3m by default, longer when the user needs \
                      time to read your final reply), counted from the end of your last turn, with \
                      nothing owed to you; `off` withdraws the booking. Book it in the turn that \
                      finishes the work, even when the user says now, and say in that reply that \
                      the pane closes after the rest. Withdraw it with `off` when more work turns \
                      up. A prompt from the user withdraws the booking, so book again in your \
                      answer when the user still wants the pane closed. A delivery from rimz (a \
                      report, a wait, a stage flip) restarts the rest, and the stop waits while \
                      something is owed to you: a subagent or agent you launched that is mid-turn, \
                      an armed wait, a background shell, an open question to the user. Cancel the \
                      waits (`wait_cancel`) and stop the background shells the finished work no \
                      longer needs.",
        read_only: false,
        command: "agents stop",
        verbs: &[],
        schema: schema::<SelfStopArgs>,
        invoke: invoke::<SelfStopArgs>,
    },
    Tool {
        name: "self_compact",
        capability: Capability::Control,
        description: "When the user asks, compact your own context with the configured brief, or \
                      with the `instruction` you give in its place, which Codex refuses. \
                      Called mid-turn, the compaction runs when this turn ends, and you are then \
                      prompted to go on with your work; `no_continue` leaves you at rest instead.",
        read_only: false,
        command: "agents compact",
        verbs: &[],
        schema: schema::<SelfCompactArgs>,
        invoke: invoke::<SelfCompactArgs>,
    },
    Tool {
        name: "self_tune",
        capability: Capability::Control,
        description: "When the user asks, report or change your own settings for this session; \
                      give no setting to report each one and where its value comes from. \
                      `keep-warm` (`2h`) keeps the prompt cache warm for up to that long of rest \
                      after each turn. `idle-compact` (`120k`) compacts at rest, before the cache \
                      expires, once context reaches that size. `smart-compact` (`70%`, or a token \
                      count such as `200k`) wraps up and compacts at that share of the window. \
                      Each also takes `off`, and `reset` drops the override, back to the \
                      configured value. The override holds through this session's restarts and \
                      resumes; a molt or fork starts without it. No configuration changes.",
        read_only: false,
        command: "agents tune",
        verbs: &[],
        schema: schema::<SelfTuneArgs>,
        invoke: invoke::<SelfTuneArgs>,
    },
    Tool {
        name: "notes_add",
        capability: Capability::Notes,
        description: "Save a Markdown note that outlives this session in the home-wide note store \
                      and return its path: a later session or another agent, in any room or \
                      checkout, reads it by that path, so cite the returned path word for word \
                      wherever you point at the note. RimZ stamps its header with who wrote it and \
                      where. `kind` names what the note is: `handoff`, `molt`, `debug`, \
                      `reflection`, or one word of your own. Give the body inline, or a `file` you \
                      already have (a report, a subagent's output, a log) and from then on cite \
                      the note's path in place of the file's. `fence` wraps text that is not \
                      Markdown (a log, command output) in one code block. A `meta` key is a fact \
                      to find the note by later (`{\"pr\": \"482\"}`), which a `notes_list` filter \
                      names.",
        read_only: false,
        command: "notes add",
        verbs: &[],
        schema: schema::<NotesAddArgs>,
        invoke: invoke::<NotesAddArgs>,
    },
    Tool {
        name: "notes_list",
        capability: Capability::Notes,
        description: "Search every note in the RimZ home, whatever room or checkout wrote it, and \
                      list the newest matches oldest first, so the newest is the last row (20 \
                      unless `limit` says otherwise). Every `filter` term must match. A term is \
                      `<field><op><value>[,<value>...]`: under `=` any value matches and under \
                      `!=` none may; `age` and `written` also take `>`, `>=`, `<`, and `<=`. \
                      Fields: `kind`, `title` (words, any case), `author` (an agent name, `@` \
                      optional), `written` (RFC 3339), `age` (`30s`, `5m`, `1h`, `2d`), `project`, \
                      `worktree`, `profile`, `team`, `role`, `model`, `channel`, `loop-task`, \
                      `loop-fire`, `workflow-run`; any other field is a `meta` key, matched as \
                      written. `*` matches any present value and `none` a missing one; quote a \
                      value that holds spaces or commas. Examples: `kind=handoff`, `author=otter`, \
                      `age<2d`, `pr=482`. The filter `help` prints every field and operator.",
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

/// The `tools/list` result the server answers with for `tools`; a tool whose
/// capability `modes` names carries Claude's load mode in its `_meta`.
pub fn tools_list(tools: &[&Tool], modes: &[(Capability, LoadMode)]) -> Value {
    let listing: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut entry = tool.listing();
            let mode = modes
                .iter()
                .find(|(capability, _)| *capability == tool.capability);
            if let Some((_, mode)) = mode {
                entry["_meta"] =
                    serde_json::json!({ "anthropic/alwaysLoad": *mode == LoadMode::Always });
            }
            entry
        })
        .collect();
    serde_json::json!({ "tools": listing })
}

/// The instructions, then each tool's description and input schema under a
/// `## <tool>` heading with its size, capability, and load mode: what the
/// server sends for `tools` when `modes` gives their load modes.
pub fn render_listing(tools: &[&Tool], modes: &[(Capability, LoadMode)]) -> String {
    let listing = tools_list(tools, modes);
    let entries = listing["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut text = format!(
        "Server instructions (~{} tokens)\n\n{INSTRUCTIONS}\n",
        crate::utils::tokens::estimate(INSTRUCTIONS)
    );
    for (tool, entry) in tools.iter().zip(entries) {
        let load = modes
            .iter()
            .find(|(capability, _)| *capability == tool.capability)
            .map_or("default", |(_, mode)| mode.name());
        let schema = serde_json::to_string_pretty(&entry["inputSchema"]).unwrap_or_default();
        text.push_str(&format!(
            "\n## {}\n~{} tokens · {} · load {load}\n\n{}\n\n```json\n{schema}\n```\n",
            tool.name,
            tool.tokens(),
            tool.capability.name(),
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
    /// The query; the description says what each verb does.
    verb: LspVerb,
    /// Every target of the question; the description gives their forms.
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
    /// `run`: wake only on this outcome. `check`: `fail` polls until it fails.
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
