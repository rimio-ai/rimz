# Agent Control CLI

Agent commands launch supervised work, type into live panes, queue follow-up text, and manage the worktrees those agents use.

Run these commands from inside the Rimz room or anywhere that resolves to the same workspace. Every command also accepts the global `--mux <name>` backend override and `--root <path>` workspace-root override.

## Run one supervised agent turn

`rimz run` starts one supervised agent turn and returns the answer to the shell.

```sh
rimz run "review the latest diff and summarize the risks"
rimz run --agent codex --worktree cli-docs "rewrite the CLI docs for run and queue"
rimz run --ask --timeout 30m --json "prepare the release checklist"
rimz run --detach --agent claude "run the long migration audit"
```

Use prompt mode for one new agent turn, or a child command to inspect and control an existing run.

```sh
rimz run [OPTIONS] [PROMPT]
rimz run status [--json] <RUN_ID>
rimz run list [--json]
rimz run stop <RUN_ID>
rimz run send <RUN_ID> [--enter] -- <TEXT>
rimz run stream <RUN_ID> [--from-start] [--timeout <DURATION>]
```

Prompt mode launches the selected agent in the current room, waits for the root turn to finish, prints the final assistant message, and exits with the run status code. `--agent <kind>` pins an adapter such as `claude` or `codex`; without it, Rimz selects the first installed, trusted, launchable agent. `--worktree [NAME]` runs the prompt in a Rimz-owned worktree; a bare flag creates a fresh worktree and a value reuses or creates that named worktree.

Permissions default to the adapter's normal editable mode. `--ask` leaves provider permission prompts in place, and `--yolo` passes the adapter's explicit bypass mode where supported. Use one permission mode per run.

`--timeout <DURATION>` accepts values such as `30s`, `5m`, `1h`, and `1d`; omitted means the command waits as long as the run takes. `--keep` leaves the agent pane open after a terminal run. `--detach` prints the run id and returns immediately. `--json` prints the terminal run record instead of only the final message. `--stream` prints NDJSON progress for a blocking run.

`status` shows one run in the current workspace, with `--json` returning the run record plus live status when the run is still active. `list` prints retained runs newest first, and `--json` emits the records. `stop` marks an active run `canceled`, wakes blocked waiters, and closes the run pane when it can; an already-terminal run reports its prior status and exits successfully.

`send` types into the run pane through the public pane-send primitive and appends Enter only with `--enter`. It fails for terminal runs and for runs whose pane has not bound yet. `stream` attaches to an existing run and emits NDJSON until the run reaches a terminal status; `--from-start` replays available assistant messages from the beginning, and `--timeout` stops watching without changing the run record.

Streaming output is newline-delimited JSON. Consumers read `message` events for assistant progress, `status` events for live-state changes, and one `end` event with the terminal status and `last_message`; `end.last_message` is the deliverable.

Exit codes are `0` for `completed`, `1` for `failed`, `124` for `timed_out` or a stream watch timeout, and `130` for `canceled`.

Supervised runs require installed and trusted hooks because hooks provide the completion signal. Details live in [run internals](../../internals/agents/run.md).

## Steer a live agent

`rimz steer` sends human-authored text to one live agent pane immediately.

```sh
rimz steer claude -- "Please inspect the failing test and propose the smallest fix."
rimz steer codex --worktree cli-docs --no-enter -- "Use the docs branch only."
rimz steer tmux:%12 --force -- "Answer the pending prompt with option 2."
```

```sh
rimz steer [OPTIONS] <TARGET> -- <TEXT>
```

`<TARGET>` is one of three forms:

- `tmux:%1` or `zellij:terminal_3`: a normalized pane id.
- `claude`, `codex`, or another known agent kind: exactly one live root agent of that kind must match.
- An agent session id or unique session-id prefix.

`--worktree <name-or-path>` filters kind and session targets by worktree branch, basename, or full path. Ambiguous and missing targets print candidate agents.

By default, `steer` appends Enter after the text. `--no-enter` types without submitting. A pending ask attached to the agent reserves the next input for that ask; `--force` records the override and sends anyway. The audit event records metadata and text length, not message content.

Target resolution and pane-answering resolver behavior are covered in [resolver internals](../../internals/agents/resolvers.md).

## Queue the next message

`rimz queue` stores text for one agent and delivers it after the agent reaches a safe turn boundary.

```sh
rimz queue claude -- "After this turn, add focused tests for the parser."
rimz queue add codex --worktree cli-docs --on any -- "If the run failed, capture the error first."
rimz queue list --json
rimz queue remove msg_01J...
rimz queue clear claude --worktree cli-docs
```

Use the bare form for the common add path, or spell the same action as `queue add` when a script prefers explicit subcommands.

```sh
rimz queue [OPTIONS] <TARGET> -- <TEXT>
rimz queue add [OPTIONS] <TARGET> -- <TEXT>
rimz queue list [--json] [TARGET]
rimz queue remove <MESSAGE_ID>
rimz queue clear [--worktree <WORKTREE>] <TARGET>
```

The bare form and `queue add` do the same work. Targets use the same pane, kind, and session grammar as `steer`. `--on done` is the default gate and delivers after the agent is `idle` or `success`. `--on any` also delivers after `failed`. `running`, `waiting`, and `paused` keep the message pending. `--no-enter` stores the text without the final Enter. `--worktree <name-or-path>` filters the target the same way `steer` does.

Delivery is FIFO per agent and one message is attempted per unparked root turn end. Rimz waits briefly for the pane composer to settle, re-checks the ledger snapshot, skips delivery while a pending ask is attached, claims the queue head, sends through the pane primitive, and marks the message delivered. Failed sends return to `pending` with an attempt count and become `abandoned` after the retry cap.

`queue list` prints durable records, with `--json` for automation. `queue remove` removes one open message. `queue clear` removes every open message for the resolved agent and prints the removal count.

Queued delivery requires installed and trusted hooks because turn-end hooks trigger the delivery helper. Details live in [message internals](../../internals/agents/messages.md).

## Drive panes

`rimz pane` exposes the public pane primitives that humans, resolvers, and scripts share.

```sh
rimz pane list
rimz pane capture zellij:terminal_4 --lines 80
rimz pane send zellij:terminal_4 --key ctrl-u --enter -- "cargo xtask test"
rimz pane focus tmux:%3
rimz pane split
rimz pane detach
```

```sh
rimz pane list [--json] [--session-name <NAME>]
rimz pane capture <PANE_ID> [--lines <N>] [--json] [--ansi]
rimz pane send <PANE_ID> [--key <KEY>]... [--enter] [TEXT]
rimz pane focus <PANE_ID> [--session-name <NAME>] [--pane-process-start <TIMESTAMP>]
rimz pane split
rimz pane detach [--session-name <NAME>]
```

`list` shows panes in the selected session; the default session is the cwd's workspace session. `--json` returns structured pane records. `capture` prints visible pane text, `--lines <N>` limits the capture, `--ansi` preserves ANSI styling, and `--json` returns the capture object.

`send` types literal text and named keys in order. Use `--` before text that starts with `-`. `--enter` appends Enter after text and explicit keys. Named keys are `enter`, `escape`, `tab`, `backspace`, `up`, `down`, `left`, `right`, `ctrl-c`, `ctrl-d`, and `ctrl-u`; aliases include `return`, `esc`, `bs`, `control-c`, `control-d`, and `control-u`.

`focus` moves attention to a pane. `--pane-process-start <TIMESTAMP>` refuses focus when a reused pane id no longer matches the sidebar snapshot; pair it with `--session-name` when focusing from cached UI state. `split` opens a new pane in the current view with Rimz workspace environment variables. `detach` detaches the attached client while leaving the session running; tmux and Zellij differ in exact client scope.

Pane capture is untrusted terminal text. Scripts and resolvers match bounded patterns before sending text back, and `pane send` is treated as the same explicit input path as `steer` and queued delivery. Resolver patterns and pane-send discipline live in [resolver internals](../../internals/agents/resolvers.md).

## Open agent tabs

`rimz tab` opens one laid-out tab or window in the current room.

```sh
rimz tab
rimz tab --layout peer --worktree cli-docs --prompt "Work on the CLI docs."
rimz tab --layout 'claude,codex+term' --name "docs review" --no-focus
```

```sh
rimz tab [--layout <NAME|SPEC>] [--worktree [NAME]] [--name <TITLE>] [--prompt <TEXT>] [--no-focus]
```

`--layout` accepts a named `[tab.layouts]` entry or an inline spec. Commas split columns, plus signs stack rows, and cells are keywords: `term`, agent kinds, adapter-supported `<kind>-<mode>` variants, or entries from `[tab.keywords]`; for example, `claude,codex+term` opens one Claude column and one stacked Codex plus shell column. With no layout, Rimz opens one terminal.

`--worktree [NAME]` creates or reuses a Rimz-owned worktree and runs every cell in it. A bare flag creates a generated worktree name. `--name` sets the tab or window title, `--prompt` passes text to agent cells, and `--no-focus` leaves focus where it is.

`rimz agents` is launcher sugar for one or more single-agent tabs.

```sh
rimz agents claude codex
rimz agents claude claude --worktree --prompt "Take separate approaches and report back."
rimz agents codex --worktree cli-docs --no-focus
```

```sh
rimz agents [KIND]... [--worktree [NAME]] [--prompt <TEXT>] [--no-focus]
```

Each positional `KIND` opens in its own tab or window. Repeating a kind opens a small fleet. A bare `--worktree` creates one fresh worktree per launched agent; a named worktree is shared by all launched agents. `--prompt` broadcasts to every launched agent, and `--no-focus` leaves focus unchanged.

Worktree launchers require a git repository-backed room. Plain `tab` and `agents` run in the room root; `--worktree` fails in directory or marker rooms. Worktree launch and cleanup details live in [worktree internals](../../internals/agents/worktrees.md).

## Manage Rimz-owned worktrees

`rimz worktree` creates, lists, and removes Rimz-owned git worktrees.

```sh
rimz worktree new cli-docs --base head
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree list --json
rimz worktree remove cli-docs
rimz worktree remove experiment --force
```

```sh
rimz worktree new [NAME] [--base <head|fresh|REF>] [--branch <NAME>]
rimz worktree list [--json]
rimz worktree remove <NAME> [--force]
```

`new` creates a marked worktree under the configured worktree directory. `--base head` branches from the current `HEAD`, `--base fresh` branches from the configured fresh base, and any other value is used as a git ref. `--branch <NAME>` creates that branch instead of using the worktree name.

`list` shows Rimz-owned worktrees for the current repo, including path, branch, unmerged-count signal, and dirty marker; `--json` emits structured entries. `remove` refuses dirty worktrees or worktrees with commits not proven merged into their base. `--force` removes anyway and keeps an unmerged branch when needed.

`rimz worktree` requires a git repository. Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary user checkouts.
