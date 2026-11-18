# `rimz mcp`

`rimz mcp serve` is the MCP server a Claude or Codex launch starts over stdio. It gives the agent RimZ's own commands as tools. You do not run it yourself: the command is hidden from `rimz --help`.

```sh
rimz mcp serve [--only <capability>]... [--parent-env]
```

`--only` repeats. Its values are the four capability names below, and without it the server serves every capability. Any other value is a usage error (exit 2). The server is named `rimz`, so Claude spells a tool `mcp__rimz__<tool>`.

`--parent-env` gives each tool's command every variable of the parent process's environment that the server's own environment lacks; a name both hold keeps the server's value. It reads the parent's environment on Linux and macOS. Where that read fails, or on another platform, the server writes one line to stderr and the commands run with the server's own environment.

## Tools

Each tool call runs one `rimz` command as the agent that started the server, in that agent's home and room, and returns the command's output. A capability shares its name with the skill that teaches the same commands.

| Capability | Tool | Arguments | Command |
| --- | --- | --- | --- |
| `rimz-lsp` | `lsp` | `verb` (`def`, `hover`, `refs`, `callers`, `callees`, `impl`, `symbols`, `find`, `show`, `check`), `targets[]`; optional `all`, `no_tests`, `external`, `limit`, `full`, `fix`, `hints`, `server` (ignored by `check`), `cwd` | `rimz lsp <verb> [flags] -- <targets>` |
| `rimz-wait` | `wait` | one trigger of `run`, `check`, `file`, `pid`, `when[]`, `in`, `at`; optional `grep`, `every`, `on` (`fail`, `success`, `any`), `label`, `check_in`, `cwd` | `rimz wait --<trigger>=<value> ...` |
| `rimz-wait` | `wait_list` | none | `rimz wait list` |
| `rimz-wait` | `wait_cancel` | `name`, or `all` | `rimz wait cancel -- <name>`, or `rimz wait cancel --all` |
| `rimz-control` | `self_stop` | optional `when_idle` (a duration, or `off`) | `rimz agents stop --when-idle[=<value>] -- @me` |
| `rimz-control` | `self_compact` | optional `instruction` | `rimz agents compact -- @me [instruction]` |
| `rimz-control` | `self_tune` | optional `setting` (`keep-warm`, `idle-compact`, `smart-compact`), optional `value` (only with `setting`) | `rimz agents tune -- @me [setting] [value]` |
| `rimz-notes` | `notes_add` | `kind`, `title`, and `body` or `file`, not both; optional `fence`, `meta{}` (keys without `=`), `cwd` | `rimz notes add [flags] -- <kind> <title>`, with `body` on stdin |
| `rimz-notes` | `notes_list` | optional `filter[]`, `limit` | `rimz notes list [flags]` |

Every command a tool runs is on the [routine list](../agent-support.md) an installed agent runs without asking. The commands themselves are documented on their own pages: [Language servers](./lsp.md), [Wait](./wait.md), [Agents](./agents.md), and [Notes](./notes.md).

## Calls

- Each call runs `rimz --color=never` with the command's words, flag values joined as `--flag=value`, and every positional after `--`, so no argument an agent passes is read as a flag. The command never gets `--root` or `--json`.
- `cwd` sets the command's working directory; without it the command runs in the server's directory, the directory the agent launched in. For `notes_add`, an agent's room pin names the note's project ahead of `cwd`.
- The command's stdin is empty, except for `notes_add`, whose `body` reaches it byte for byte.
- The result's text is the command's stdout followed by its stderr. A command that exits nonzero returns an error result whose last line is `exit code <N>`, since the [LSP exit codes](./lsp.md#exit-codes) carry meaning.
- The server checks the shape of the arguments (their types, required fields, and enum values) and refuses four combinations the command would misread: a `wait` with no trigger, which the command reads as a listing; a `self_tune` `value` without a `setting`; a `notes_add` with both `body` and `file`; and a `notes_add` `meta` key containing `=`. Such arguments return an error result without running anything. Which flag fits which verb, and the single trigger a `wait` takes, are the command's own rules, and its usage error comes back as the error result.
- Calls run at the same time, so a later call can answer first. A cancelled call's command is killed and the call gets no reply.
- The server adds no timeout and no output cap.

## Protocol

The server speaks newline-delimited JSON-RPC 2.0 on stdin and stdout, and writes its logs to stderr.

| Method | Reply |
| --- | --- |
| `initialize` | The tools capability, `serverInfo` named `rimz`, a one-line `instructions`, and the client's protocol version when it is `2025-06-18` or `2025-11-25`, else `2025-11-25` |
| `ping` | `{}` |
| `tools/list` | The served tools, each with its JSON Schema |
| `tools/call` | The call's result; an unknown or unserved tool is error `-32602` |
| `notifications/cancelled` | No reply; the named call's command is killed |
| Any other request, `server/discover` included | Error `-32601` |

A line that is not JSON gets error `-32700`, and the server goes on reading. The server exits 0 when stdin ends or on SIGINT or SIGTERM, after killing the commands still running. A watcher that a `wait` call detached keeps running.

## Launch registration

RimZ hands its server to a Claude or Codex launch when `[agents] mcp-tools` is on (the default, set in the [machine config](../../guide/configuration.md#rimz-tools-over-mcp)) and the profile's `mcps` list is absent or names at least one capability. The server's command is the room's `rimz` link, so it survives `rimz update`. Every spawn path does the same, and `rimz agents explain <profile>` prints the arguments in the provider argv.

- **Claude** gets one `--mcp-config` holding the `rimz` server, after every other provider argument and before the `--` that precedes a prompt, since the flag takes several values. With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) on, `mcp__rimz__*` joins `permissions.allow` in the launch's `--settings`, so the tools run without asking; with it off, Claude asks before each call.
- **Codex** gets `-c mcp_servers.rimz.*` overrides: the command and its arguments, `--parent-env` among them, `env_vars` naming every variable RimZ spawns Codex with (Codex clears a server's environment to a few names otherwise), `tool_timeout_sec=600`, and `default_tools_approval_mode` set to `"approve"` with `allow-routine-rimz` on and `"prompt"` with it off. The `env_vars` names are collected before the user's login shell runs its startup files, so on Linux and macOS `--parent-env` takes what those files export from Codex's own environment; elsewhere the commands see the `env_vars` set alone.

### The `mcps` profile key

A profile's [`mcps`](../definitions.md) list chooses both RimZ's capabilities and the user's own MCP servers:

| `mcps` | RimZ's server | The user's servers |
| --- | --- | --- |
| Absent | Every capability | As without RimZ. A Claude profile with a `tools` list carries `--strict-mcp-config`, so it loads none. |
| A list | Only the named capabilities, as `--only` arguments; none when no capability is named | Only the named servers, within each provider's reach below |
| `[]` | Not handed over | None, within each provider's reach below |

A list's entries are capability names (`rimz-lsp`, `rimz-wait`, `rimz-control`, `rimz-notes`) and the names of the user's servers. A name that matches no configured server is ignored, since accounts and projects define different servers. An empty entry, `rimz`, and any other name starting `rimz-` refuse the definition at load. RimZ never switches a server on: a listed server the user's config disables stays off.

- **Claude**: a list sets `allowedMcpServers` in the launch's `--settings` to exactly `rimz`, when the server is handed over, plus each other entry, and drops `--strict-mcp-config`. Claude applies it to servers from every source (`--mcp-config`, a project's `.mcp.json`, claude.ai connectors) and prints `Warning: MCP server blocked by enterprise policy: <name>` for each one it blocks.
- **Codex** has no allowlist. A list sets `enabled = false` for each server defined under `mcp_servers` in the account's `config.toml` or the profile file that `--profile`, `-p`, or `-p<name>` selects that the list does not name. Servers from Codex plugins, connectors, trusted project config, managed config, `-c` arguments, and Codex's built-ins stay on. A config file that cannot be read or parsed switches nothing off, and the launch warns.
- **Other kinds** get no server. A list warns at launch that it is not applied, and the launch proceeds.

### Cases RimZ does not handle

In each case, `[agents] mcp-tools = false` keeps RimZ's server out of every launch; a profile's `mcps` list still filters the user's servers.

- A user's own server already named `rimz`.
- A user's own Codex `-c mcp_servers={...}` table in a profile's or launch's arguments: it replaces the table RimZ places first, so the switch-offs a `mcps` list adds are lost. The `rimz` server stays: its dotted keys follow every user argument, and Codex merges dotted keys given after a table.
- A user's Codex `shell_environment_policy`: it filters what Codex's shell commands see, not what the tools' commands see, so a `wait` command armed through the tool can read a variable the policy withholds from the shell.
- Variables Codex sets for itself after it starts, such as those in the `.env` file of its home: a tool's command gets one only when the `env_vars` list names it, since `--parent-env` reads the environment Codex was started with.
- Claude run with `CLAUDE_CODE_MCP_ALLOWLIST_ENV=1`, which keeps the room pin out of the server's environment.
- Claude under a managed MCP policy.
- A user's own Claude settings file that sets `allowedMcpServers`: how Claude combines it with the launch's list is untested.
- A Codex launch forced to a never-ask approval policy with a sandbox while `allow-routine-rimz` is off: Codex refuses every tool call that needs approval, so every call is refused.
