# `rimz mcp`

`rimz mcp serve` is the MCP server a Claude, Codex, or Antigravity launch starts over stdio. It gives the agent RimZ's own commands as tools. You do not run it yourself: started with a terminal on stdin, it prints a short note to stderr naming the commands that show what it serves and what a launch passes it, and exits 0 without serving. `rimz mcp tools` lists what it serves.

```sh
rimz mcp serve [--only <capability>]... [--always <capability>]... [--deferred <capability>]... [--parent-env]
rimz mcp serve --launch-env [--parent-env]
```

`--only` repeats. Its values are the five capability names below, and without it the server serves every capability. Any other value is a usage error (exit 2). The server is named `rimz`, so Claude spells a tool `mcp__rimz__<tool>`.

`--always` and `--deferred` repeat and take the same values as `--only`. Each adds `_meta: {"anthropic/alwaysLoad": true, "antigravity/alwaysLoad": true}` (`--always`), or both keys `false` (`--deferred`), to the `tools/list` entry of every tool of that capability. The first key tells Claude to load the tool into the agent's context up front or to leave it behind its tool search; the second tells Antigravity to bind the tool directly or to leave it behind its `call_mcp_tool` tool. A capability given to both refuses with exit 1.

`--launch-env` serves what the launch that started the provider names in two variables of the server's environment, for a provider that starts one fixed server entry for every session of an account (Antigravity, through the rimz plugin). `RIMZ_MCP_SERVE` lists the served capabilities, comma-separated, each with an optional `:always` or `:deferred` that works as `--always` or `--deferred` does (`rimz-lsp:always,rimz-wait`). `RIMZ_MCP_CWD` is the directory the agent launched in: a tool acts there, and a relative `cwd` argument is read from there. An entry that is not a capability, carries any other mark, or names a capability a second time exits 1 with an error naming the entry. With `RIMZ_MCP_SERVE` unset or empty, the server writes `rimz mcp serve: RimZ serves no tools in this session, as expected in an Antigravity session RimZ did not launch or a launch whose profile serves none` to stderr and exits 0. `--launch-env` conflicts with `--only`, `--always`, and `--deferred`.

`--parent-env` restarts the server once, at its start, with every variable of the parent process's environment that its own environment lacks; a name both hold keeps the server's value. Its tools then act as the agent, in the home and room, that this environment names, and the server reads the [replacement files](#replace-the-wording) from that home. It reads the parent's environment on Linux and macOS. Where that read or the restart fails, or on another platform, the server writes one line to stderr and runs with its own environment.

## Tools

Each tool call but `guide`'s does the job of one `rimz` command, inside the server, as the agent that started the server, in that agent's home and room, and answers with what the job returns ([calls](#calls)). A capability shares its name with the skill that teaches the same commands, a launch that serves the capability withholds that skill from the model, on Antigravity only under scoped isolation ([configuration](../../guide/configuration.md#rimz-tools-over-mcp)), and its tools' descriptions carry that skill's usage knowledge for the options the tools expose: the `lsp` target forms and result cap, the `wait` triggers, what a `subagent` brief carries and how its result comes back, the `tasks`, `join`, and `stop` id forms, and when to book a stop. Material few calls need is a guide the agent loads on demand: the `wait` description points to `rimz-wait/conditions`, the `when` condition grammar, and the `subagent` description to `rimz-subagents/delegating`, what to delegate and how to brief and read a subagent. The server's `instructions` open with a short line saying what the served tools are for, which RimZ builds from the served tools and no file replaces; then one line per served capability says when to reach for it, then how a call runs. `rimz agents explain <profile> --mcps` prints all of that text as one launch is served it.

`guide` belongs to no capability. The server lists it when it serves a capability that has a guide (`rimz-wait` and `rimz-subagents`), gives it no load mode of its own, and answers a call itself: the text of the named guide, or, for a name of a guide the launch does not serve, an error result whose `error` is `no guide "<name>"` and whose `hint` is `this launch has: <names>`.

`tasks`, `join`, and `stop` belong to no capability either. The server lists them when it serves `rimz-wait` or `rimz-subagents`, the capabilities whose tools start things, gives them no load mode of their own, and they act on what the agent started through RimZ, each named by an id `<kind>:<name>`: `subagent:` or `agent:` and the handle, `team:` and the team instance (`team#lane`), `workflow:` and the `wf_` run id, `schedule:` or `wait:` and the task name. `tasks` lists those ids. `stop` and `join` act on a thing the agent started, or that an agent it started started; any other id is refused. Cancelling a workflow run is the user's call, and the `stop` description says so.

| Capability | Tool | Arguments | Command |
| --- | --- | --- | --- |
| `rimz-lsp` | `lsp` | `verb` (`def`, `hover`, `refs`, `callers`, `callees`, `impl`, `symbols`, `find`, `show`, `check`), `targets[]`; optional `all`, `no_tests`, `external`, `limit`, `full`, `fix`, `hints`, `server` (ignored by `check`), `cwd` | `rimz lsp <verb> [flags] -- <targets>` |
| `rimz-wait` | `wait` | `trigger` (`run`, `check`, `file`, `pid`, `when`, `in`, `at`) and its `value`, a string; optional `grep`, `every`, `on` (`fail`, `success`), `label`, `check_in`, `cwd` | `rimz wait --<trigger>=<value> ...` |
| `rimz-subagents` | `subagent` | `profile` and `prompt`, neither blank; optional `description`, `cwd`, `timeout` (a duration), `model`, `effort`, `agent` | `rimz subagents <profile> <prompt> [flags]` |
| - | `tasks` | optional `kind` (`subagent`, `agent`, `team`, `workflow`, `schedule`, `wait`) | The rows `rimz agents show -- @me` prints under what it started |
| - | `join` | `id`; optional `timeout` (a duration, default `4m`, at most `9m`) | `rimz agents wait`, `rimz subagents wait`, `rimz teams wait`, or `rimz workflow wait`, by kind |
| - | `stop` | `id` | `rimz agents stop`, `rimz subagents stop`, `rimz teams stop`, `rimz workflow cancel`, or `rimz wait cancel`, by kind; for a schedule, the removal of its booking |
| `rimz-control` | `self_stop` | optional `when_idle` (a duration, or `off`) | `rimz agents stop --when-idle[=<value>] -- @me` |
| `rimz-control` | `self_compact` | optional `instruction`, `no_continue` | `rimz agents compact [--no-continue] -- @me [instruction]` |
| `rimz-control` | `self_tune` | optional `setting` (`keep-warm`, `idle-compact`, `smart-compact`), optional `value` (only with `setting`) | `rimz agents tune -- @me [setting] [value]` |
| `rimz-notes` | `notes` | `kind`, `title`, and `body` or `file`, not both; optional `fence`, `meta{}` (keys without `=`) | `rimz notes add [flags] -- <kind> <title>` |
| - | `guide` | `name`, `<capability>/<guide>` | None: the server answers from the guide files it read at its start |

## List the tools

```sh
rimz mcp tools [--only <capability>]... [--json]
```

`rimz mcp tools` prints each tool the server offers with its capability (`-` for `guide`, `tasks`, `join`, and `stop`) and the estimated tokens of its `tools/list` entry (compact JSON, counted with the `o200k_base` tokenizer, so Claude's own count runs somewhat higher), then the total. That entry is what a provider puts in the agent's context for the tool when it loads it up front. `--only` takes the same values as `serve --only` and refuses any other the same way. `--json` prints the server's `tools/list` result for the same `--only`, unchanged. Both read the [replacement files](#replace-the-wording) as the server does, and print a warning on stderr for each one that is not used.

```console
$ rimz mcp tools
CAPABILITY      TOOL          TOKENS
rimz-lsp        lsp            ~1223
rimz-wait       wait           ~1095
rimz-subagents  subagent        ~600
-               tasks           ~371
-               join            ~419
-               stop            ~364
rimz-control    self_stop       ~430
rimz-control    self_compact    ~208
rimz-control    self_tune       ~310
rimz-notes      notes           ~464
-               guide           ~120
total: ~5604 tokens in 11 tools
```

Every command a tool stands for is on the [routine list](../agent-support.md) an installed agent runs without asking, so a tool does nothing such an agent may not do unasked, with one exception: a `stop` of a schedule removes its booking, which no routine command does, and runs unasked all the same. The commands themselves are documented on their own pages: [Language servers](./lsp.md), [Wait](./wait.md), [Subagents](./subagents.md), [Agents](./agents.md), and [Notes](./notes.md).

## Replace the wording

Each text the server sends is a Markdown file built into `rimz`, and a file at the same path under `mcps/` in the RimZ home replaces it:

| Path under `~/.rimz/mcps/` | Text |
| --- | --- |
| `instructions.md` | The instructions' last part: how a call runs and what its result holds |
| `guide.md` | The `guide` description |
| `<capability>/instructions.md` | The capability's line in the instructions, below the first line |
| `<capability>/<tool>.md` | The tool's description (`rimz-wait/wait.md`) |
| `<capability>/guides/<guide>.md` | A guide `guide` loads (`rimz-wait/guides/conditions.md`) |

The built-in files are under `crates/rimz/src/mcp/tools/` in the source tree. The server reads the folder once, when it starts, so a running session keeps the text it started with and an agent launched afterwards gets the new file; a provider that restarts the server inside a session (a resume, a server it respawns) reads the folder again. An argument's description is part of the tool's schema and has no file.

A replacement never switches a tool off. RimZ serves the built-in text in its place, and warns with the file's path and the reason, when the file cannot be read, is not UTF-8, is empty, or breaks a limit a provider keeps:

- a tool's description and its arguments' descriptions over 4,096 characters;
- a capability's sentence on more than one line;
- the instructions with every capability over 4,096 characters: a replaced `instructions.md` falls back.

The limits are checked with every capability, so a file passes or fails the same way in every launch. A `.md` file at any other path is ignored with a warning, which names the path it belongs at when exactly one built-in file has its name (`rimz-lsp/wait.md` belongs at `rimz-wait/wait.md`); a file cannot add a tool or a guide. Files that do not end in `.md` are not read.

A launch that hands the server over carries the warnings of the capabilities it serves, of the shared files, and of every misplaced file: they print in the agent's pane before the provider starts, stay on the agent's record for `rimz agents show`, and print on stderr under `rimz agents explain`. `rimz mcp tools` prints every warning on stderr, and `rimz agents explain <profile> --mcps` ends the line of each tool, and the instructions' heading, with `text <path>` for each part whose text is a replacement.

## Calls

- Each call but `guide`'s runs inside the server, on a thread of its own, through the library function its command calls. No `rimz` process starts for a call, except the watcher a `wait` detaches, as `rimz wait` does, the language-server broker (`rimz lsp serve`) an `lsp` call starts again when the checkout's has died, as `rimz lsp` does, the pane a `subagent` opens for its child, as `rimz subagents` does, and what a `stop` of a workflow run starts as `rimz workflow cancel` does: the loop runs subscribed to the run's signals, the advance of the run whose `runs` step started it, and the next waiting fire of the schedule that fired it. Every argument reaches that function as a value, so no string an agent passes is read as a flag.
- `cwd`, on `lsp`, `wait`, and `subagent`, names the directory the call acts on, relative to the directory the agent launched in unless it is absolute; without it the call acts on that directory: the server's own directory, or on Antigravity, whose server runs in the plugin directory, `RIMZ_MCP_CWD`. A `subagent` call always acts in the room of that directory: its `cwd` names the child's working directory, and without it the child starts where `rimz subagents` without `--cwd` does: the caller's own checkout, or the profile's empty directory. A `cwd` that is not a directory returns an error result.
- `lsp` answers the text `rimz lsp` prints, and `guide` the guide's text. Every other tool answers a JSON object in the shape of its `outputSchema`, as the result's `structuredContent` and, the same JSON, as its one text item: `wait` `{id, status, stop, report}` (`id` is `wait:<name>`, `status` is `running`, `stop` is the `stop` call that cancels it, and `report` says how its result arrives); `subagent` the same four fields, with `id` `subagent:<petname>` and a `report` naming the `SUBAGENT_REPORT` message, plus `warnings`, a list of the notices `rimz subagents` would print for the launch, left out when there are none; `tasks` `{tasks}`, a row per thing with `id`, `kind`, `status` (`running`, `done`, or `disabled`), `on_your_stop` (`holds`, `closes`, or `outlives`), and when known `age`, `about`, and `detail`; `join` `{id, status}` with `status` `done` and the `result` and any `detail` (for a workflow run, `result` is its notes path, absent when it has none, and `detail` its end state and checkout), `running` at its timeout or with a `report` for a schedule or a wait, whose outcome comes as a message, `disabled` for a schedule switched off, or `ended` with a `hint` for a wait, schedule, or team RimZ holds no record of; `stop` `{id, status}` with `status` `stopped`, plus `still_running`, a list that may be empty, of the ids a stopped schedule or workflow run leaves running, or `ended` for a thing that had ended, with a `hint` when RimZ holds no record of it; `self_stop` `{booking}` (`withdrawn`, `none_pending`, or `booked` with `after_secs`, `replaces_after_secs`, and `started`, which holds the rows the command's receipt prints under `holding`, `closing`, and `outliving`: what holds the stop, what closes with the agent, and what outlives it, or `unread` with why they could not be read); `self_compact` `{message_id, status, continues}` (`status` is `sent`, `queued`, or `joined` when the call joined the compaction already queued for the agent, which `message_id` names); `self_tune` `{agent, settings}`, with `keep_warm_while` when one is set and `budget` when no setting is named; `notes` `{path, dropped_meta, unresolved_project}`.
- A call that cannot do its job returns an error result whose text is a JSON object `{"error": ..., "hint": ...}`, with `hint` only where RimZ knows the fix, and which carries the same object as `structuredContent`: arguments that do not fit, no room at the call's directory, a refusal of the job (an id that is not `<kind>:<name>`, a thing another agent started, a duration that does not parse, a `wait` `label` that is not one line of 1 to 60 characters, a `when` condition that cannot be read in the checkout, a `subagent` launch the launcher refuses, such as one from a subagent, of a profile the caller may not launch, or over budget), or an `lsp` call on one target that fails. A failed `subagent` launch adds `warnings`, the notices collected before the failure, left out when there are none. A verdict is an answer, not a failure: an `lsp check` that flags anchors, `no results`, and a failed target among several `lsp` targets come back as text. `show` is stricter on one target: any outcome that shows no code is an error result, an unknown or ambiguous name included, which `def` answers as text; the candidates are in `error`.
- The server checks the shape of the arguments (their types, required fields, and enum values), so a `wait` without its `trigger` or its `value` never runs. It also refuses what `rimz` would refuse in the command's flags: a `self_tune` `value` without a `setting`, a `notes` with both `body` and `file`, a `notes` `meta` key containing `=`, a `subagent` with a blank `profile` or `prompt`, and an `lsp` call with no `targets`, `hints` without `fix`, `limit` with `all`, a `limit` of 0, or a field the verb's command has no flag for (`all`, `limit`, and `external` off `refs`, `callers`, `callees`, `impl`, and `find`; `no_tests` off `refs` and `callers`; `full` off `show`; `fix` and `hints` off `check`). Such arguments return an error result whose `error` starts `invalid arguments:`, and nothing runs.
- Calls run at the same time, so a later call can answer first. A cancelled call gets no reply, and its work runs to its end, except a `join`, which stops waiting within half a second. A `join` that finds a message queued for the agent answers `running` with a hint to read it. A call that panics returns an error result, and the server goes on answering.
- The server adds no timeout and no output cap beyond `join`'s own.

## Protocol

The server speaks newline-delimited JSON-RPC 2.0 on stdin and stdout, and writes its logs to stderr. Anything else written to its stdout, by a tool's path, lands on stderr, so the protocol carries only its own lines.

| Method | Reply |
| --- | --- |
| `initialize` | The tools capability with `listChanged`, `serverInfo` named `rimz`, `instructions` whose first line says what the served tools are for, then a line per served capability saying when to reach for it, then how a call runs and what its result holds, and the client's protocol version when it is `2025-06-18` or `2025-11-25`, else `2025-11-25` |
| `ping` | `{}` |
| `tools/list` | The served tools, each with its JSON Schema, an `outputSchema` on each tool that answers data, and `_meta` on the tools of a capability `--always` or `--deferred` names |
| `tools/call` | The call's result, which for `guide` the server writes itself; an unknown or unserved tool is error `-32602` |
| `notifications/cancelled` | No reply; the named call gets no reply, and its work runs to its end, except a `join`'s, which stops waiting |
| Any other request, `server/discover` included | Error `-32601` |

A line that is not JSON gets error `-32700`, and the server goes on reading. The server exits 0 when stdin ends or on SIGINT or SIGTERM, and a call still running ends with it. A watcher that a `wait` call detached keeps running.

A server started with a room pin (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, which every room pane exports) follows `rimz update`: once the room records another build, the server replaces itself with that build at its first moment with no call in flight, keeping its process and its pipes, and the new build first sends `notifications/tools/list_changed`, so the client reads the tool list again. Codex rereads it only with its `mcp_tool_list_changed` feature on. A server with no room pin stays on the build it started with.

## Launch registration

RimZ hands its server to a Claude, Codex, or Antigravity launch when `[agents] rimz-mcp-server` is on (the default, set in the [machine config](../../guide/configuration.md#rimz-tools-over-mcp)) and the profile's `mcps` list is absent or holds `*`, `rimz`, or at least one capability. On Claude and Codex the server's command is the room's `rimz` link, so it survives `rimz update`, and a running server in a room follows the update ([protocol](#protocol)). Every spawn path does the same, and `rimz agents explain <profile> --mcps` prints the servers, tools, load modes, and arguments one launch gets, then the instructions and each tool's description and schema as the server sends them ([explain a launch](./agents.md#explain-a-launch)).

- **Claude** gets one `--mcp-config` holding the `rimz` server, after every other provider argument and before the `--` that precedes a prompt, since the flag takes several values. With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) on, `mcp__rimz__*` joins `permissions.allow` in the launch's `--settings`, so the tools run without asking; with it off, Claude asks before each call.
- **Codex** gets `-c mcp_servers.rimz.*` overrides: the command and its arguments, `--parent-env` among them, `env_vars` naming every variable RimZ spawns Codex with (Codex clears a server's environment to a few names otherwise), `tool_timeout_sec=600`, and `default_tools_approval_mode` set to `"approve"` with `allow-routine-rimz` on and `"prompt"` with it off. The `env_vars` names are collected before the user's login shell runs its startup files, so on Linux and macOS `--parent-env` takes what those files export from Codex's own environment; elsewhere the server has the `env_vars` set alone.

- **Antigravity** starts the server itself, from the `tools` entry of the rimz plugin's `mcp_config.json` that `rimz hooks install antigravity` writes, which agy shows as `rimz_tools` (it names a plugin's server `<plugin directory>_<server>`): `rimz mcp serve --launch-env`, never in argv or the agent file. Every launch that hands the server over, a resume included, sets `RIMZ_MCP_SERVE` to the served capabilities with their marks and `RIMZ_MCP_CWD` to the launch directory in agy's environment, and every other launch of any kind removes both, so a session RimZ did not launch serves nothing. agy binds the tools from a conversation's first turn, so a scoped Antigravity launch withholds the `rimz-*` skill of each capability it serves, as Claude and Codex do; a host launch keeps the skills callable, since agy has no per-launch skill switch on the host. With `allow-routine-rimz` on, install saves `mcp(rimz_tools/*)` in `permissions.allow` of `~/.gemini/antigravity-cli/settings.json`, so the tools run without asking, and a definition's [`allowed-tools`](../definitions.md#tools-to-provider-arguments) entry `mcp__rimz` or one of its tools reaches agy as an `mcp(rimz_tools/...)` rule; with the switch off, install removes that one rule and agy asks before each call. An account whose plugin lacks the current `mcp_config.json` reads as missing its hooks, so its launches refuse until `rimz hooks install antigravity` runs again.

### The `mcps` profile key

A profile's [`mcps`](../definitions.md) list chooses both RimZ's capabilities and the user's own MCP servers:

| `mcps` | RimZ's server | The user's servers |
| --- | --- | --- |
| Absent | Every capability | As without RimZ |
| A list holding `*` | Every capability | As without RimZ |
| Any other list | Every capability when it names `rimz`, else only the named capabilities, as `--only` arguments; none when it names neither | Only the named servers, within each provider's reach below |
| `[]` | Not handed over | None, within each provider's reach below |

A subagent child and a workflow judge are never served `rimz-subagents`, whatever the list says: every capability is the other four there, and a list that names only `rimz-subagents` hands such a launch no RimZ server.

A list's entries are `*` (every server), `rimz` (every capability), the capability names (`rimz-lsp`, `rimz-wait`, `rimz-subagents`, `rimz-control`, `rimz-notes`), and the names of the user's servers. Every entry but `*` takes an optional mark, `:always` or `:deferred`, written without a space (`rimz-lsp:always`); a Markdown definition quotes `'*'`, since YAML reads a bare `*` as an alias. A name that matches no configured server is ignored, since accounts and projects define different servers. An empty entry, a mark on `*`, any other mark, any other name starting `rimz-`, and one name listed with two different marks refuse the definition at load. The same entry listed twice refuses in TOML and counts once in a Markdown definition. RimZ never switches a server on: a listed server the user's config disables stays off.

A mark sets how the provider loads a server's tools: `always` puts them in the agent's context up front, `deferred` leaves them behind the provider's tool search until the agent looks for them. A mark the launch cannot apply warns once, naming the entry, and the launch proceeds without it. On Antigravity, a mark on a capability or on `rimz` reaches the server through `RIMZ_MCP_SERVE` on every launch that hands it over.

- **Claude**: a list without `*` sets `allowedMcpServers` in the launch's `--settings` to exactly `rimz`, when the server is handed over, plus each of the user's servers it names. Claude applies it to servers from every source (`--mcp-config`, a project's `.mcp.json`, claude.ai connectors) and prints `Warning: MCP server blocked by enterprise policy: <name>` for each one it blocks. A profile's `tools` list limits no MCP server. A mark on `rimz` sets the server entry's `alwaysLoad`: `true` for `rimz:always`, and `false` for `rimz:deferred` unless a capability is marked `always`. A mark on a capability becomes `--always <capability>` or `--deferred <capability>` in the server's arguments. A mark on one of the user's servers warns: Claude reads that server's load mode from the user's own config.
- **Codex** has no allowlist. A list without `*` sets `enabled = false` for each server defined under `mcp_servers` in the account's `config.toml` or the profile file that `--profile`, `-p`, or `-p<name>` selects that the list does not name. Servers from Codex plugins, connectors, trusted project config, managed config, `-c` arguments, and Codex's built-ins stay on. A config file that cannot be read or parsed switches nothing off, and the launch warns. A mark on `rimz` sets `mcp_servers.rimz.omit_tools_from`: `["deferred"]` for `always`, which declares the tools to the model directly, and `[]` for `deferred`, which leaves them to Codex's default (behind tool search on a model that has it). A mark on one of the user's servers passes the same value as `-c mcp_servers.<name>.omit_tools_from=...` when the account's `config.toml` or the selected profile file defines that server and its name holds neither `.` nor `=`, and warns otherwise. A mark on a capability warns: Codex sets a load mode per server, so the capability loads as the `rimz` server does. Under Codex's strict Code Mode Only (`features.code_mode_only` with `code_mode_only_strict_3p_tools`), Codex ignores `omit_tools_from`, so marks do nothing.
- **Antigravity** applies the list to the user's servers only through its agent file: `inheritMcp: false` and a name-only `mcpServers` item for each of the user's servers the list names. agy refuses a call to a tool of a server the agent does not list, though that server's process still starts. A list that limits nothing (absent, or holding `*`) writes no MCP key, so the account's servers load as configured. A fresh launch with no prompt sources writes no file, so a limiting list refuses there before any pane opens (a layout launch, a molt, or a restart whose conversation cannot resume), naming the fix: add the kind base `agents/antigravity.md` or another prompt source, or remove `mcps`. A resume keeps the user's servers its conversation started with. RimZ's capabilities and their marks reach the server through `RIMZ_MCP_SERVE`, and a mark on `rimz` applies to every served capability that has no mark of its own. A mark on one of the user's servers warns and keeps its name-only item, since agy takes a load mode only for RimZ's capabilities.
- **Other kinds** get no server. A list warns at launch that it is not applied, unless it holds only `*`, and the launch proceeds.

### Cases RimZ does not handle

In each case, `[agents] rimz-mcp-server = false` keeps RimZ's server out of every launch (agy still starts the plugin's `rimz_tools`, which serves nothing, and a resumed Antigravity conversation whose agent file named an inline `rimz` server still starts that one); a profile's `mcps` list still filters the user's servers.

- A user's own server already named `rimz`.
- A user's own server whose name holds a `:`: an entry naming it reads the text after the last colon as a mark, and refuses the entry unless that text is `always` or `deferred`.
- A `--strict-mcp-config` in a profile's or launch's arguments: RimZ passes it to Claude, which then ignores the account's and project's servers whatever the `mcps` list names.
- A user's own Codex `-c mcp_servers={...}` table in a profile's or launch's arguments: it replaces the table RimZ places first, so the switch-offs a `mcps` list adds are lost. The `rimz` server stays: its dotted keys follow every user argument, and Codex merges dotted keys given after a table.
- A user's Codex `shell_environment_policy`: it filters what Codex's shell commands see, not what the server sees, so a `wait` command armed through the tool can read a variable the policy withholds from the shell.
- Variables Codex sets for itself after it starts, such as those in the `.env` file of its home: the server gets one only when the `env_vars` list names it, since `--parent-env` reads the environment Codex was started with.
- Claude run with `CLAUDE_CODE_MCP_ALLOWLIST_ENV=1`, which keeps the room pin out of the server's environment.
- Claude under a managed MCP policy.
- A user's own Claude settings file that sets `allowedMcpServers`: how Claude combines it with the launch's list is untested.
- Two Antigravity sessions of one account at once: agy writes each server's tool schemas to one folder per server name under `~/.gemini/antigravity-cli/mcp/`, so two sessions serving different capabilities share the `rimz_tools` folder.
- An Antigravity session RimZ did not launch: the rimz plugin still starts `rimz_tools`, which serves nothing and shows in agy's `/mcp` as a red `✗ rimz_tools` row with the server's stderr line as its error.
- A Codex launch forced to a never-ask approval policy with a sandbox while `allow-routine-rimz` is off: Codex refuses every tool call that needs approval, so every call is refused.
