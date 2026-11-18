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
