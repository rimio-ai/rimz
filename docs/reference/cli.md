# Public CLI

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

`rimz` is the command line for one project room: start the room, attach locally or over SSH, launch and steer agents, publish feed items, and maintain the ledger-backed workspace.

This page is the map. Detailed examples and full command notes live in the command-group references:

- [Getting started](./cli/getting-started.md) — `rimz`, `start`, `attach`, `remote`, `list`, `setup`, `doctor`.
- [Agent control](./cli/agents.md) — `agents`, `transcript`, `steer`, `queue`, `pane`, `worktree`, `loop`.
- [Feed, resolvers, hooks, and trust](./cli/feed.md) — `feed`, `event`, `resolver`, `hooks`, `trust`.
- [Maintenance](./cli/maintenance.md) — `config`, `workspace`, `reload`, `reset`, `gc`, `ping`.

## Fast path

Start in a project directory and run `rimz`.

```sh
cd ~/code/query-engine
rimz
```

`rimz` resolves the workspace, creates or reattaches the Zellij or tmux room, opens the sidebar, and enters the session when the terminal is interactive. From there, launch agents in panes or tabs; the sidebar keeps the fleet visible and takes you to the pane that needs input.

Use `remote connect` when the room lives on another machine. A raw target connects immediately; an alias saves the target and reconnect defaults for later.

```sh
rimz remote connect dev-box:~/code/query-engine
rimz remote add dev dev-box:~/code/query-engine
rimz remote connect dev
```

Use `agents -p` for one supervised agent turn from a script, `steer` for immediate text into a live agent, and `queue` for the next instruction after a safe turn boundary.

```sh
rimz agents codex --worktree=docs --timeout 2h -p "update the CLI reference and run markdown checks"
rimz steer @claude -- "focus on the failing parser test"
rimz queue @codex --on done -- "open a PR summary when the tests pass"
```

## Start and attach a workspace

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [PATH]
rimz start [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [PATH]
rimz attach [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [SESSION]
```

`rimz` and `rimz start` are the same entry point. They choose a room from the current path, preferring an explicit `--root`, then the enclosing git repository, then a project marker, then the directory itself. The session pins `RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, so participant commands inside the room write to the same ledger even when panes move through nested directories.

Interactive terminals attach by default. Non-interactive callers print the attach command. `--attach`, `--no-attach`, and `--print` force that decision. `--no-resume` starts an empty reborn room instead of re-seeding remembered agents, and `--refresh-ms` overrides the sidebar render cadence for sidebars spawned by that launch.

`rimz attach <session>` attaches by exact session name. With no session argument, it resolves the current directory's room.

Full bootstrap, directory-workspace, remote, list, setup, and doctor examples live in [Getting started](./cli/getting-started.md).

## Remote rooms

```sh
rimz remote connect [user@]host:<session-or-path>
rimz remote add <name> <target> [--no-reconnect] [--no-resume] [--mux <name>]
rimz remote connect <alias|target> [--reset] [--no-reconnect] [--attach|--print]
rimz remote reset <alias|target> [--no-reconnect] [--attach|--print]
rimz remote del <name>      # alias: rm
rimz remote rename <old> <new>
rimz remote list [--json]   # alias: ls
```

`remote connect` builds a guarded `ssh -t` command and runs the remote host's own `rimz`. A target after `:` is either a path (`dev-box:~/code/query-engine`) or a session name (`dev-box:query-engine`). Saved aliases live in `~/.config/rimz/remote.toml`; reconnect supervision is on by default and `--no-reconnect` hands the link to a single SSH process.

Full remote examples and target grammar live in [Getting started](./cli/getting-started.md#remote-rooms). Link-health mechanics live in [remote.md](../internals/reach/remote.md).

## Launch and control agents

```sh
rimz agents [--json]
rimz agents list|ls [--json] [--worktree <name>]
rimz agents show <ref> [--json]
rimz agents focus <ref>
rimz agents wait <ref> [--timeout <duration>] [--stream [--from-start]] [--json]
rimz agents stop <ref>
rimz agents <spec> [prompt] [-w|--worktree[=<name>]] [--name <name>] [--new-pane|--new-tab] [--bg] [--ask|--yolo] [--system-prompt-file <path>] [--effort <level>] [-- passthrough...]
rimz agents <spec> [prompt] -p|--print [--system-prompt-file <path>] [--effort <level>] [--timeout <duration>] [--detach] [--output-format <text|json|stream-json>] [--input-format <text|stream-json>] [--keep]
rimz transcript [target] [-w|--worktree <name>] [-n|--last <n>] [--details] [--json]
```

`rimz agents` lists live agent cards by default. A launch spec is either a named `[agents.teams]` team or the inline layout grammar from `[agents.profiles]` and `[agents.commands]`: commas split columns, plus signs stack rows, and inline cells are `term`, agent kinds, virtual `<kind>-<mode>` cells such as `codex-yolo`, configured profiles, or configured commands. `-p` launches one supervised agent pane, waits for the root turn, prints the final assistant message, and exits with `0` for success, `1` for failure, `124` for timeout, and `130` for cancellation. Hooks are the completion signal, so the selected agent's Rimz hooks must be installed and trusted.

`--system-prompt-file` and `--effort` are shared launch params that each adapter renders into its native flags, so one flag works across providers; `--output-format` and `--input-format` shape how `-p` prints the run and reads the prompt. Both are detailed in [agents.md](./cli/agents.md).

Use `transcript` to read a single agent's turn history or a channel's fused timeline. Use `steer` for immediate text into a live agent pane, and `queue` for durable delivery after the agent reaches a safe gate.

```sh
rimz steer <target> [--worktree <name>] [--no-enter] [--force] [--no-from] [--yes] -- <text>
rimz queue <target> [--worktree <name>] [--on done|any] [--no-enter] [--force] [--no-from] [--yes] -- <text>
rimz queue add <target> [--worktree <name>] [--on done|any] [--no-enter] [--force] [--no-from] [--yes] -- <text>
rimz queue list [--json] [target]
rimz queue remove <message-id>
rimz queue clear [--worktree <name>] <target>
```

`TARGET` is an `@`-mention or a pane id (`tmux:%1`, `zellij:terminal_3`). `@swift-otter` (pet name), `@claude-2` (kind ordinal), and a session-id prefix name one agent; `@codex` (an agent kind) and `@all` fan out to every match in the channel. The channel is the current worktree unless you append `#<worktree>` or pass `--worktree`, both narrowing by branch, worktree name, or path. A fan-out past one agent confirms first (`--yes` skips the prompt). Agent-authored sends arrive as `@sender: text`, with `#channel` added across channels; `--no-from` sends byte-for-byte. `steer` refuses to type over a pending ask unless `--force` is explicit; `queue` waits for hooks to report a safe delivery moment. A bare `@<kind>` or `@all` from `steer` also reaches a codex started in a fresh pane before its first turn — it addresses the pane directly — while `queue` needs a bound session and points such a pane back at `steer`.

Full agent, pane, and worktree examples live in [Agent control](./cli/agents.md).

## Publish events and ask questions

```sh
rimz feed ask --title <s> [--options <a,b,c>] [--timeout <duration>] [--no-block]
rimz feed push --kind <kind> --title <s> [--body <s>]
rimz feed list [--json] [--audit]        # alias: ls
rimz feed show <request-id> [--json]
rimz feed resolve --decision <json> <request-id> [--resolver-id <id>] [--method <method>] [--override-chain]
rimz feed dismiss <request-id> [--reason <text>]
rimz feed abstain --resolver-id <id> <request-id> [--reason <text>]
```

`feed ask` is the script decision primitive: it posts a question to the same sidebar feed as agent prompts and blocks until a human or resolver answers. `feed resolve` delivers a decision for `bridge` and `script` requests, `feed dismiss` acknowledges `native_ui` items locally, and `feed abstain` advances an active resolver chain.

Full feed, resolver, hook, event, and trust examples live in [Feed, resolvers, hooks, and trust](./cli/feed.md).

## Command index

| Command | Use it for | Reference |
| --- | --- | --- |
| `rimz`, `start` | Open or create the current project room. | [Getting started](./cli/getting-started.md#start-the-room) |
| `remote` | Connect to rooms over SSH and manage remote aliases. | [Getting started](./cli/getting-started.md#remote-rooms) |
| `steer` | Type into live agent panes immediately. | [Agent control](./cli/agents.md#steer-live-agents) |
| `queue` | Deliver the next instruction when an agent finishes a turn. | [Agent control](./cli/agents.md#queue-the-next-message) |
| `transcript` | Read an agent turn history or a channel timeline from local transcripts. | [Agent control](./cli/agents.md#inspect-transcripts) |
| `pane` | See the room as panes (grouped by tab, agent-aware), capture, send to, focus, split, or detach. | [Agent control](./cli/agents.md#drive-panes) |
| `feed` | Post feed items, ask script questions, and resolve decisions. | [Feed](./cli/feed.md#feed-items-and-decisions) |
| `agents` | List, launch, focus, wait for, and stop agent cards. | [Agent control](./cli/agents.md#agents) |
| `worktree` | Create, list, and remove Rimz-owned git worktrees. | [Agent control](./cli/agents.md#manage-rimz-owned-worktrees) |
| `loop` | Schedule one supervised agent turn on this machine's OS scheduler. | [Loop tasks](../internals/agents/loop.md) |
| `list` | Show known rooms and their live backend. | [Getting started](./cli/getting-started.md#list-rooms) |
| `stats` | Token-activity heatmap, model breakdown, and usage insights, account-global. | [The Lobby](../internals/reach/welcome.md#rimz-stats) |
| `list-themes` | Print the bundled sidebar theme names. | [Maintenance](./cli/maintenance.md#list-themes) |
| `doctor` | Diagnose backend, hook, trust, resolver, and room-tree state. | [Getting started](./cli/getting-started.md#setup-and-doctor) |
| `setup` | Print first-run environment state and write default config. | [Getting started](./cli/getting-started.md#setup-and-doctor) |
| `hooks` | Install or remove agent hooks. | [Feed](./cli/feed.md#agent-hooks) |
| `trust` | Grant, revoke, or inspect project executable-surface trust. | [Feed](./cli/feed.md#project-trust) |
| `config` | Initialize, read, and edit per-machine config. | [Maintenance](./cli/maintenance.md#configure-the-machine) |
| `resolver` | Manage the per-machine resolver allowlist. | [Feed](./cli/feed.md#resolver-chain) |
| `event` | Append generic structured events to the workspace ledger. | [Feed](./cli/feed.md#events) |
| `attach` | Attach to a room by session name. | [Getting started](./cli/getting-started.md#start-the-room) |
| `workspace` | Resolve, migrate, and rotate workspace ledger state. | [Maintenance](./cli/maintenance.md#workspace-ledger-tools) |
| `reload` | Converge running sidebars onto the installed build. | [Maintenance](./cli/maintenance.md#reload-reset-and-gc) |
| `reset` | Archive and rebuild a wedged room. | [Maintenance](./cli/maintenance.md#reload-reset-and-gc) |
| `gc` | Sweep stale runtime liveness and cleanup records. | [Maintenance](./cli/maintenance.md#reload-reset-and-gc) |
| `ping` | Print `ok` for machine-readable liveness checks. | [Maintenance](./cli/maintenance.md#ping) |

## Global flags

`--mux <name>` overrides backend selection for the current invocation. Use it when both Zellij and tmux are installed and you need a specific backend.

`--root <path>` overrides workspace root resolution. Use it for monorepo escape hatches or deliberate directory rooms.

Attach-capable commands accept `--attach`, `--no-attach`, and `--print`. `--print` is an alias for `--no-attach`.

Many read commands accept `--json`; those outputs are the scripting surface. Human tables stay compact and may change to improve readability.

## Commands Rimz calls for you

Hidden helper commands are machinery for hooks, sidebars, statuslines, and agent wrappers. They are omitted from `rimz --help` and are not the user-facing CLI contract.

Examples include `rimz sidebar snapshot`, `rimz sidebar serve`, `rimz statusline feed`, `rimz hooks feed`, `rimz queue deliver`, `rimz agents exec`, `rimz agents auto-continue` (the producer's rate-limit-reset nudge), `rimz agents refresh-usage --kind <kind>` (the uniform per-provider account-usage refresh), `rimz loop run`, `rimz worktree cleanup`, and `rimz codex ...` (the `refresh-context` / `app-server` session-enrichment helpers). The owning internals docs describe the protocols: [ledger](../internals/sidebar/ledger.md), [state](../internals/sidebar/state.md), [agent](../internals/agents/agent.md), [provider](../internals/agents/provider.md), and [harness](../internals/agents/harness.md).
