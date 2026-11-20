# `rimz mcp`

`rimz mcp serve` is the MCP server a Claude or Codex launch starts over stdio. It gives the agent RimZ's own commands as tools. You do not run it yourself: started with a terminal on stdin, it prints a short note to stderr naming the commands that show what it serves and what a launch passes it, and exits 0 without serving. `rimz mcp tools` lists what it serves.

```sh
rimz mcp serve [--only <capability>]... [--always <capability>]... [--deferred <capability>]... [--parent-env]
```

`--only` repeats. Its values are the four capability names below, and without it the server serves every capability. Any other value is a usage error (exit 2). The server is named `rimz`, so Claude spells a tool `mcp__rimz__<tool>`.

`--always` and `--deferred` repeat and take the same values as `--only`. Each adds `_meta: {"anthropic/alwaysLoad": true}` (`--always`) or `false` (`--deferred`) to the `tools/list` entry of every tool of that capability, which tells Claude to load the tool into the agent's context up front or to leave it behind its tool search. A capability given to both refuses with exit 1.

`--parent-env` gives each tool's command every variable of the parent process's environment that the server's own environment lacks; a name both hold keeps the server's value. It reads the parent's environment on Linux and macOS. Where that read fails, or on another platform, the server writes one line to stderr and the commands run with the server's own environment.

## Tools

Each tool call runs one `rimz` command as the agent that started the server, in that agent's home and room, and returns the command's output. A capability shares its name with the skill that teaches the same commands, and its tools' descriptions carry that skill's usage knowledge for the options the tools expose: the `lsp` target forms and result cap, the `wait` triggers and `when` grammar, the `notes_list` filter fields, and when to book a stop. The server's `instructions` say when to reach for each group of tools and how a call runs. `rimz agents explain <profile> --mcps` prints all of that text as one launch is served it.

| Capability | Tool | Arguments | Command |
| --- | --- | --- | --- |
| `rimz-lsp` | `lsp` | `verb` (`def`, `hover`, `refs`, `callers`, `callees`, `impl`, `symbols`, `find`, `show`, `check`), `targets[]`; optional `all`, `no_tests`, `external`, `limit`, `full`, `fix`, `hints`, `server` (ignored by `check`), `cwd` | `rimz lsp <verb> [flags] -- <targets>` |
| `rimz-wait` | `wait` | `trigger` (`run`, `check`, `file`, `pid`, `when`, `in`, `at`) and its `value`, a string; optional `grep`, `every`, `on` (`fail`, `success`, `any`), `label`, `check_in`, `cwd` | `rimz wait --<trigger>=<value> ...` |
| `rimz-wait` | `wait_list` | none | `rimz wait list` |
| `rimz-wait` | `wait_cancel` | `name`, or `all` | `rimz wait cancel -- <name>`, or `rimz wait cancel --all` |
| `rimz-control` | `self_stop` | optional `when_idle` (a duration, or `off`) | `rimz agents stop --when-idle[=<value>] -- @me` |
| `rimz-control` | `self_compact` | optional `instruction`, `no_continue` | `rimz agents compact [--no-continue] -- @me [instruction]` |
| `rimz-control` | `self_tune` | optional `setting` (`keep-warm`, `idle-compact`, `smart-compact`), optional `value` (only with `setting`) | `rimz agents tune -- @me [setting] [value]` |
| `rimz-notes` | `notes_add` | `kind`, `title`, and `body` or `file`, not both; optional `fence`, `meta{}` (keys without `=`), `cwd` | `rimz notes add [flags] -- <kind> <title>`, with `body` on stdin |
| `rimz-notes` | `notes_list` | optional `filter[]`, `limit` | `rimz notes list [flags]` |

## List the tools

```sh
rimz mcp tools [--only <capability>]... [--json]
```

`rimz mcp tools` prints each tool the server offers with its capability and the estimated tokens of its `tools/list` entry (compact JSON, counted with the `o200k_base` tokenizer, so Claude's own count runs somewhat higher), then the total. That entry is what a provider puts in the agent's context for the tool when it loads it up front. `--only` takes the same values as `serve --only` and refuses any other the same way. `--json` prints the server's `tools/list` result for the same `--only`, unchanged.

```console
$ rimz mcp tools
CAPABILITY    TOOL          TOKENS
rimz-lsp      lsp            ~1204
rimz-wait     wait           ~1227
rimz-wait     wait_list        ~46
rimz-wait     wait_cancel     ~135
rimz-control  self_stop       ~283
rimz-control  self_compact    ~140
rimz-control  self_tune       ~251
rimz-notes    notes_add       ~405
rimz-notes    notes_list      ~359
total: ~4050 tokens in 9 tools
```

Every command a tool runs is on the [routine list](../agent-support.md) an installed agent runs without asking. The commands themselves are documented on their own pages: [Language servers](./lsp.md), [Wait](./wait.md), [Agents](./agents.md), and [Notes](./notes.md).

## Calls

- Each call runs `rimz --color=never` with the command's words, flag values joined as `--flag=value`, and every positional after `--`, so no argument an agent passes is read as a flag. The command never gets `--root` or `--json`.
- `cwd` sets the command's working directory; without it the command runs in the server's directory, the directory the agent launched in. For `notes_add`, an agent's room pin names the note's project ahead of `cwd`.
- The command's stdin is empty, except for `notes_add`, whose `body` reaches it byte for byte.
- The result's text is the command's stdout, then, when the command wrote to stderr, a line `stderr:` and its stderr. A command that exits nonzero returns an error result whose last line is `exit code <N>`, since the [LSP exit codes](./lsp.md#exit-codes) carry meaning.
- The server checks the shape of the arguments (their types, required fields, and enum values), so a `wait` without its `trigger` or its `value` never runs (without a trigger, the command would read the call as a listing). It also refuses three combinations the command would misread: a `self_tune` `value` without a `setting`, a `notes_add` with both `body` and `file`, and a `notes_add` `meta` key containing `=`. Such arguments return an error result starting `invalid arguments:` without running anything. Which flag fits which verb or trigger, and what a `value` must hold (a `pid` value that is not a number, say), are the command's own rules, and its usage error comes back as the error result.
- Calls run at the same time, so a later call can answer first. A cancelled call's command is killed and the call gets no reply.
- The server adds no timeout and no output cap.

## Protocol

The server speaks newline-delimited JSON-RPC 2.0 on stdin and stdout, and writes its logs to stderr.

| Method | Reply |
| --- | --- |
| `initialize` | The tools capability, `serverInfo` named `rimz`, `instructions` whose first line says when to reach for each group of tools and whose other lines say how a call runs and what its result holds, and the client's protocol version when it is `2025-06-18` or `2025-11-25`, else `2025-11-25` |
| `ping` | `{}` |
| `tools/list` | The served tools, each with its JSON Schema, and `_meta` on the tools of a capability `--always` or `--deferred` names |
| `tools/call` | The call's result; an unknown or unserved tool is error `-32602` |
| `notifications/cancelled` | No reply; the named call's command is killed |
| Any other request, `server/discover` included | Error `-32601` |

A line that is not JSON gets error `-32700`, and the server goes on reading. The server exits 0 when stdin ends or on SIGINT or SIGTERM, after killing the commands still running. A watcher that a `wait` call detached keeps running.

## Launch registration

RimZ hands its server to a Claude or Codex launch when `[agents] rimz-mcp-server` is on (the default, set in the [machine config](../../guide/configuration.md#rimz-tools-over-mcp)) and the profile's `mcps` list is absent or holds `*`, `rimz`, or at least one capability. The server's command is the room's `rimz` link, so it survives `rimz update`. Every spawn path does the same, and `rimz agents explain <profile> --mcps` prints the servers, tools, load modes, and arguments one launch gets, then the instructions and each tool's description and schema as the server sends them ([explain a launch](./agents.md#explain-a-launch)).

- **Claude** gets one `--mcp-config` holding the `rimz` server, after every other provider argument and before the `--` that precedes a prompt, since the flag takes several values. With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) on, `mcp__rimz__*` joins `permissions.allow` in the launch's `--settings`, so the tools run without asking; with it off, Claude asks before each call.
- **Codex** gets `-c mcp_servers.rimz.*` overrides: the command and its arguments, `--parent-env` among them, `env_vars` naming every variable RimZ spawns Codex with (Codex clears a server's environment to a few names otherwise), `tool_timeout_sec=600`, and `default_tools_approval_mode` set to `"approve"` with `allow-routine-rimz` on and `"prompt"` with it off. The `env_vars` names are collected before the user's login shell runs its startup files, so on Linux and macOS `--parent-env` takes what those files export from Codex's own environment; elsewhere the commands see the `env_vars` set alone.

### The `mcps` profile key

A profile's [`mcps`](../definitions.md) list chooses both RimZ's capabilities and the user's own MCP servers:

| `mcps` | RimZ's server | The user's servers |
| --- | --- | --- |
| Absent | Every capability | As without RimZ |
| A list holding `*` | Every capability | As without RimZ |
| Any other list | Every capability when it names `rimz`, else only the named capabilities, as `--only` arguments; none when it names neither | Only the named servers, within each provider's reach below |
| `[]` | Not handed over | None, within each provider's reach below |

A list's entries are `*` (every server), `rimz` (every capability), the capability names (`rimz-lsp`, `rimz-wait`, `rimz-control`, `rimz-notes`), and the names of the user's servers. Every entry but `*` takes an optional mark, `:always` or `:deferred`, written without a space (`rimz-lsp:always`); a Markdown definition quotes `'*'`, since YAML reads a bare `*` as an alias. A name that matches no configured server is ignored, since accounts and projects define different servers. An empty entry, a mark on `*`, any other mark, any other name starting `rimz-`, and one name listed with two different marks refuse the definition at load. The same entry listed twice refuses in TOML and counts once in a Markdown definition. RimZ never switches a server on: a listed server the user's config disables stays off.

A mark sets how the provider loads a server's tools: `always` puts them in the agent's context up front, `deferred` leaves them behind the provider's tool search until the agent looks for them. A mark the launch cannot apply warns once, naming the entry, and the launch proceeds without it.

- **Claude**: a list without `*` sets `allowedMcpServers` in the launch's `--settings` to exactly `rimz`, when the server is handed over, plus each of the user's servers it names. Claude applies it to servers from every source (`--mcp-config`, a project's `.mcp.json`, claude.ai connectors) and prints `Warning: MCP server blocked by enterprise policy: <name>` for each one it blocks. A profile's `tools` list limits no MCP server. A mark on `rimz` sets the server entry's `alwaysLoad`: `true` for `rimz:always`, and `false` for `rimz:deferred` unless a capability is marked `always`. A mark on a capability becomes `--always <capability>` or `--deferred <capability>` in the server's arguments. A mark on one of the user's servers warns: Claude reads that server's load mode from the user's own config.
- **Codex** has no allowlist. A list without `*` sets `enabled = false` for each server defined under `mcp_servers` in the account's `config.toml` or the profile file that `--profile`, `-p`, or `-p<name>` selects that the list does not name. Servers from Codex plugins, connectors, trusted project config, managed config, `-c` arguments, and Codex's built-ins stay on. A config file that cannot be read or parsed switches nothing off, and the launch warns. A mark on `rimz` sets `mcp_servers.rimz.omit_tools_from`: `["deferred"]` for `always`, which declares the tools to the model directly, and `[]` for `deferred`, which leaves them to Codex's default (behind tool search on a model that has it). A mark on one of the user's servers passes the same value as `-c mcp_servers.<name>.omit_tools_from=...` when the account's `config.toml` or the selected profile file defines that server and its name holds neither `.` nor `=`, and warns otherwise. A mark on a capability warns: Codex sets a load mode per server, so the capability loads as the `rimz` server does. Under Codex's strict Code Mode Only (`features.code_mode_only` with `code_mode_only_strict_3p_tools`), Codex ignores `omit_tools_from`, so marks do nothing.
- **Other kinds** get no server. A list warns at launch that it is not applied, unless it holds only `*`, and the launch proceeds.

### Cases RimZ does not handle

In each case, `[agents] rimz-mcp-server = false` keeps RimZ's server out of every launch; a profile's `mcps` list still filters the user's servers.

- A user's own server already named `rimz`.
- A user's own server whose name holds a `:`: an entry naming it reads the text after the last colon as a mark, and refuses the entry unless that text is `always` or `deferred`.
- A `--strict-mcp-config` in a profile's or launch's arguments: RimZ passes it to Claude, which then ignores the account's and project's servers whatever the `mcps` list names.
- A user's own Codex `-c mcp_servers={...}` table in a profile's or launch's arguments: it replaces the table RimZ places first, so the switch-offs a `mcps` list adds are lost. The `rimz` server stays: its dotted keys follow every user argument, and Codex merges dotted keys given after a table.
- A user's Codex `shell_environment_policy`: it filters what Codex's shell commands see, not what the tools' commands see, so a `wait` command armed through the tool can read a variable the policy withholds from the shell.
- Variables Codex sets for itself after it starts, such as those in the `.env` file of its home: a tool's command gets one only when the `env_vars` list names it, since `--parent-env` reads the environment Codex was started with.
- Claude run with `CLAUDE_CODE_MCP_ALLOWLIST_ENV=1`, which keeps the room pin out of the server's environment.
- Claude under a managed MCP policy.
- A user's own Claude settings file that sets `allowedMcpServers`: how Claude combines it with the launch's list is untested.
- A Codex launch forced to a never-ask approval policy with a sandbox while `allow-routine-rimz` is off: Codex refuses every tool call that needs approval, so every call is refused.
