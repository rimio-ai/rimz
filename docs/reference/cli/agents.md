# Agent Control CLI

Agent commands list the room's agent cards, launch laid-out agent panes, run supervised script turns, type into live panes, queue follow-up text, and manage Rimz-owned worktrees.

Run these commands from inside the Rimz room or anywhere that resolves to the same workspace. Every command also accepts the global `--mux <name>` backend override and `--root <path>` workspace-root override.

## Agents

`rimz agents` is the card surface and the single agent launcher.

```sh
rimz agents
rimz agents list --json
rimz agents show swift-otter
rimz agents focus claude-2@cli-docs
rimz agents wait swift-otter --stream --from-start
rimz agents stop run_0123456789abcdef0123456789abcdef
rimz agents claude,codex --worktree=cli-docs
rimz agents 'vim,codex+term' "review the CLI docs"
rimz agents codex "prepare the release checklist" -p --timeout 30m --json
rimz agents claude "run the long migration audit" -p --detach
```

```sh
rimz agents [--json]
rimz agents list|ls [--json] [--all] [--worktree <WORKTREE>]
rimz agents show <REF> [--json]
rimz agents focus <REF>
rimz agents wait <REF> [--timeout <DURATION>] [--stream [--from-start]] [--json]
rimz agents stop <REF>
rimz agents <SPEC> [PROMPT] [-w|--worktree[=<NAME>]] [--name <PETNAME>] [--no-focus] [--ask|--yolo] [-- PASSTHROUGH...]
rimz agents <SPEC> [PROMPT] -p|--print [--timeout <DURATION>] [--detach] [--stream] [--json] [--keep]
```

Bare `rimz agents` lists live root-agent cards. `list --all` includes audit rollup rows, `--worktree` filters by branch, worktree name, or directory basename, and `--json` emits the filtered `AgentState` records.

`<SPEC>` is the layout grammar: commas split columns, plus signs stack rows, and each cell is `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` cell, or a configured `[agents.aliases]` entry. Named layouts come from `[agents.layouts]`; the built-in `peer` layout is `claude,codex`.

Permission-mode suffixes (`-auto`, `-ask`, `-plan`, `-yolo`) are the official virtual mode variants; availability depends on what the adapter supports — for example, `claude-plan` passes `--permission-mode plan`, while `codex-plan` has no plan-mode equivalent and falls back to the default posture. The built-in `-ping` suffix opens the agent at lowest effort with a `"ping"` initial prompt (Claude: `--effort low`; Codex: `-c model_reasoning_effort=low`) and is useful for keeping the 5-hour session window alive. Built-in virtual cells: `claude-auto`, `claude-ask`, `claude-plan`, `claude-yolo`, `claude-ping`, `codex-auto`, `codex-ask`, `codex-plan`, `codex-yolo`, `codex-ping`, `pi-ask`, `pi-plan`.

`PROMPT` is the optional second positional and is broadcast to every agent cell. Interactive launches pass no approval override by default, so each provider keeps its native prompts; `--ask` keeps/returns to native prompts where supported, and `--yolo` passes the adapter's bypass flags. `-- PASSTHROUGH...` appends raw agent argv to every agent cell after alias preset args and any explicit permission args. A second positional that is itself a known cell or layout is rejected with a `rimz agents a,b` hint so the removed space-separated fan-out form does not silently become a prompt.

`-w`/`--worktree` takes a value as `--worktree=docs` or a space-separated `--worktree docs` (both reuse or create that worktree), while bare `--worktree` creates a fresh generated worktree. A worktree launch names the backend tab `⑂ <NAME>` (the worktree name behind the worktree glyph); launches without a worktree name the tab `<kind>:<dir>`. A single-agent launch into a fresh generated worktree uses the generated worktree name as a pet-name candidate unless `--name` is set; named shared worktrees keep independent agent names.

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the final assistant message, and exits with the run status code: `0` completed, `1` failed, `124` timed out, `130` canceled. `--detach` prints the pet name and returns immediately; use that name with `steer`, `agents wait`, `agents show`, or `agents stop`.

`show` prints one card and its newest attached run record when present. `wait` waits for a supervised run by run id or pet name, or for an interactive agent to reach an idle/success gate. `stop` cancels a supervised run when the ref names one, otherwise it closes the agent pane.

`<REF>` accepts a pane id (`tmux:%1`, `zellij:terminal_3`), an exact pet name, a kind ordinal (`claude-2`), a unique kind in scope, or a unique session-id prefix. Add `@<worktree>` to the selector or pass `--worktree` where available; both forms narrow by branch, generated worktree name, or directory basename.

Supervised `-p` runs require installed and trusted hooks because hooks provide the completion signal. Details live in [run internals](../../internals/agents/run.md).

## Steer A Live Agent

`rimz steer` sends human-authored text to one live agent pane immediately.

```sh
rimz steer swift-otter -- "Please inspect the failing test and propose the smallest fix."
rimz steer claude-2@cli-docs --no-enter -- "Use the docs branch only."
rimz steer tmux:%12 --force -- "Answer the pending prompt with option 2."
```

```sh
rimz steer [OPTIONS] <REF> [--worktree <WORKTREE>] [--no-enter] [--force] -- <TEXT...>
```

Targets use the same card ref grammar as `agents show`. By default, `steer` presses Enter as a discrete keystroke after the text, so the agent submits instead of taking a newline. `--no-enter` types without submitting. A pending ask attached to the agent reserves the next input for that ask; `--force` records the override and sends anyway. The audit event records metadata and text length, not message content.

Target resolution and pane-answering resolver behavior are covered in [resolver internals](../../internals/agents/resolvers.md).

## Queue The Next Message

`rimz queue` stores text for one agent and delivers it after the agent reaches a safe turn boundary.

```sh
rimz queue swift-otter -- "After this turn, add focused tests for the parser."
rimz queue add codex@cli-docs --on any -- "If the run failed, capture the error first."
rimz queue list --json
rimz queue remove msg_01J...
rimz queue clear claude-2@cli-docs
```

```sh
rimz queue [OPTIONS] <REF> [--worktree <WORKTREE>] [--on done|any] [--no-enter] -- <TEXT...>
rimz queue add [OPTIONS] <REF> [--worktree <WORKTREE>] [--on done|any] [--no-enter] -- <TEXT...>
rimz queue list [--json] [REF]
rimz queue remove <MESSAGE_ID>
rimz queue clear [--worktree <WORKTREE>] <REF>
```

The bare form and `queue add` do the same work. `--on done` is the default gate and delivers after the agent is `idle` or `success`. `--on any` also delivers after `failed`. `running`, `waiting`, and `paused` keep the message pending. `--no-enter` stores the text without the final Enter.

Delivery is FIFO per agent and one message is attempted per unparked root turn end. Rimz waits briefly for the pane composer to settle, re-checks the ledger snapshot, skips delivery while a pending ask is attached, claims the queue head, sends through the pane primitive, and marks the message delivered. Failed sends return to `pending` with an attempt count and become `abandoned` after the retry cap.

Queued delivery requires installed and trusted hooks because turn-end hooks trigger the delivery helper. Details live in [message internals](../../internals/agents/messages.md).

## Drive Panes

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

`list` shows panes in the selected session; the default session is the cwd's workspace session. `capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention to a pane. Named keys are `enter`, `escape`, `tab`, `backspace`, `up`, `down`, `left`, `right`, `ctrl-c`, `ctrl-d`, and `ctrl-u`; aliases include `return`, `esc`, `bs`, `control-c`, `control-d`, and `control-u`.

Pane capture is untrusted terminal text. Scripts and resolvers match bounded patterns before sending text back, and `pane send` is treated as the same explicit input path as `steer` and queued delivery. Resolver patterns and pane-send discipline live in [resolver internals](../../internals/agents/resolvers.md).

## Manage Rimz-Owned Worktrees

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
