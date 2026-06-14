# Agent Control CLI

Agent commands list the room's agent cards, launch laid-out agent panes, run supervised script turns, type into live panes, queue follow-up text, and manage Rimz-owned worktrees.

Run them from inside the Rimz room or anywhere that resolves to the same workspace. Every command also accepts the global `--mux <name>` backend override and `--root <path>` workspace-root override.

The launch, run, and messaging machinery these commands drive — the layout IR, tab/split placement, supervised-run records, worktree cleanup, and the pane-send path — lives in [harness.md](../../internals/agents/harness.md). A typical session threads the whole surface:

```sh
rimz agents claude,codex --worktree=auth-refresh "Refactor token refresh; keep the public API stable."
rimz steer @claude#auth-refresh -- "Start with the refresh-token rotation path."
rimz queue @codex#auth-refresh -- "After your turn, add coverage for the expiry edge cases."
rimz agents focus @claude#auth-refresh        # jump to the pane when it needs you
```

## Agents

`rimz agents` is the card surface and the single agent launcher: list the room, launch a layout, run a supervised turn, then focus, wait on, or stop what you started.

**Glance at the room.** Bare `rimz agents` prints the live root-agent cards; widen or narrow from there.

```sh
rimz agents                              # live root-agent cards
rimz agents list --all                   # include audit rollup rows
rimz agents list --worktree auth-refresh # filter to one branch / worktree / dir
rimz agents show swift-otter             # one card plus its newest run record
rimz agents show swift-otter --json      # the AgentState record
```

**Launch a layout.** A spec is a shape; a prompt broadcasts to every agent cell.

```sh
rimz agents peer                                       # built-in claude,codex side by side
rimz agents claude,codex+term                          # Claude | Codex stacked over a shell
rimz agents claude,codex --worktree=cli-docs "Review the CLI docs."
rimz agents 'vim,codex+term' "Review the CLI docs."    # raw command cells beside an agent
rimz agents claude --worktree "Take one approach."     # parallel attempts, each in a fresh worktree
rimz agents claude --worktree "Take another approach."
```

**Run one supervised turn from a script.** `-p` drives a single agent turn, prints its result, and exits with a status code you can branch on.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --detach   # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
```

**Drive what you launched.** Focus jumps to the pane; wait blocks until it lands; stop cancels a run or closes a pane.

```sh
rimz agents focus @claude-2#cli-docs
rimz agents wait swift-otter --stream --from-start
rimz agents stop run_0123456789abcdef0123456789abcdef
```

```sh
rimz agents [--json]
rimz agents list|ls [--json] [--all] [--worktree <WORKTREE>]
rimz agents show <REF> [--json]
rimz agents focus <REF>
rimz agents wait <REF> [--timeout <DURATION>] [--stream [--from-start]] [--json]
rimz agents stop <REF>
rimz agents <SPEC> [PROMPT] [-w|--worktree[=<NAME>]] [--name <PETNAME>] [--same-tab|--new-tab] [--no-focus] [--ask|--yolo] [--system-prompt-file <PATH>] [--effort <LEVEL>] [-- PASSTHROUGH...]
rimz agents <SPEC> [PROMPT] -p|--print [--system-prompt-file <PATH>] [--effort <LEVEL>] [--timeout <DURATION>] [--detach] [--output-format <text|json|stream-json>] [--input-format <text|stream-json>] [--keep]
```

### Listing and inspecting

Bare `rimz agents` lists live root-agent cards, grouped by worktree channel. The lead `AGENT` column is the agent's canonical handle — `@<kind>` within its channel, growing an ordinal (`@claude-2`) only when two of a kind share one worktree — so the column reads as the address you would type back. `list --all` adds audit rollup rows, `--worktree` filters by branch, worktree name, or directory basename, and `--json` emits the filtered `AgentState` records. `show` prints one card (its handle, kind, petname, and session) and its newest attached run record when present. `--json` selects JSON for `list` and bare `agents` card output — not for `-p`, which has its own `--output-format`.

### The launch spec

`<SPEC>` is the layout grammar: commas split columns, plus signs stack rows, and each cell is `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` cell, or a configured `[agents.aliases]` entry. Named layouts come from `[agents.layouts]`; the built-in `peer` layout is `claude,codex`. The full grammar and how cells compile to panes are in [harness.md → The layout IR](../../internals/agents/harness.md#the-layout-ir).

Permission-mode suffixes (`-auto`, `-ask`, `-plan`, `-yolo`) are the official virtual mode variants; availability depends on what the adapter supports — `claude-plan` passes `--permission-mode plan`, while `codex-plan` has no plan-mode equivalent and falls back to the default posture. The built-in `-ping` suffix opens the agent at lowest effort with a `"ping"` initial prompt (Claude `--effort low`; Codex `-c model_reasoning_effort=low`) and keeps the 5-hour session window alive. Built-in virtual cells: `claude-auto`, `claude-ask`, `claude-plan`, `claude-yolo`, `claude-ping`, `codex-auto`, `codex-ask`, `codex-plan`, `codex-yolo`, `codex-ping`, `pi-ask`, `pi-plan`.

`PROMPT` is the optional second positional, broadcast to every agent cell. Interactive launches pass no approval override by default, so each provider keeps its native prompts; `--ask` keeps or returns to native prompts where supported, and `--yolo` passes the adapter's bypass flags. `-- PASSTHROUGH...` appends raw agent argv to every agent cell after alias preset args and any explicit permission args. A second positional that is itself a known cell or layout is rejected with a `rimz agents a,b` hint, so the removed space-separated fan-out form never silently becomes a prompt.

### Shared launch params

`--system-prompt-file <PATH>` and `--effort <LEVEL>` broadcast to every agent cell like `PROMPT`, and each adapter renders them into its native flags: `--system-prompt-file` replaces the agent's base system prompt (Claude `--system-prompt-file <PATH>`; Codex `-c model_instructions_file=<PATH>`), and `--effort` sets reasoning effort (Claude `--effort <LEVEL>`; Codex `-c model_reasoning_effort=<LEVEL>`). The launcher resolves the prompt file to an absolute path and refuses a missing file before launch. Levels are provider-specific — Claude takes `low|medium|high|xhigh|max`, Codex takes `minimal|low|medium|high|xhigh` — and an agent with no native flag for a param (Pi today) refuses the launch with the offending flag named. A configured alias preset renders first, so an explicit `--effort` on the command line wins.

### Worktree and placement

`-w`/`--worktree` takes a value as `--worktree=docs` or space-separated `--worktree docs` (both reuse or create that worktree), while bare `--worktree` creates a fresh generated worktree. A worktree launch names the backend tab `⑂ <NAME>` (the worktree name behind the worktree glyph); a launch without a worktree names the tab `<kind>:<dir>`. A single-agent launch into a fresh generated worktree uses the generated name as a pet-name candidate unless `--name` is set; named shared worktrees keep independent agent names.

Placement follows intent. Under the default `auto` policy a worktree launch or a multi-cell layout opens its own tab, while a single non-worktree agent splits the current view beside the launching pane. `--new-tab` forces a new tab; `--same-tab` forces the split for a single agent cell — including a single worktree launch — run from inside the room, and is rejected for a multi-cell layout. The per-machine `[agents] tab` default sets the policy when neither flag is given, and `tab = "same"` likewise splits a single worktree launch ([configuration.md](../configuration.md#agent-aliases-and-layouts)). `--no-focus` keeps focus on the launching pane in either case. The split-versus-tab mechanics are in [harness.md → Backend shape and placement](../../internals/agents/harness.md#backend-shape-and-placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the final assistant message, and exits with the run status code: `0` completed, `1` failed, `124` timed out, `130` canceled. `--detach` prints the pet name and returns immediately; use that name with `steer`, `agents wait`, `agents show`, or `agents stop`.

`--output-format` selects how `-p` renders the run: `text` (default) prints the final assistant message, `json` prints the full run record, and `stream-json` emits run events as NDJSON while the turn runs. `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT`, while `stream-json` reads user messages from stdin until EOF and refuses a positional `PROMPT`. `stream-json` output cannot combine with `--detach`.

Supervised `-p` runs require installed and trusted hooks, because hooks provide the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [harness.md → Supervised runs](../../internals/agents/harness.md#supervised-runs).

### Focus, wait, and stop

`focus` jumps to an agent's pane. `wait` waits for a supervised run by run id or pet name, or for an interactive agent to reach an idle/success gate; `--stream` tails the transcript (`--from-start` replays from the top). `stop` cancels a supervised run when the ref names one, otherwise it closes the agent pane.

`<REF>` accepts a pane id (`tmux:%1`, `zellij:terminal_3`) or an `@`-mention: `@swift-otter` (pet name), `@claude-2` (kind ordinal), `@claude` (a kind), or `@<session-prefix>`. Append `#<worktree>` to scope the lookup; it narrows by branch, generated worktree name, or directory basename, and defaults to the current worktree. These management commands resolve to one agent, so a fan-out mention (`@claude` matching several, or `@all`) is an ambiguity here — name one. They also accept a bare selector (`swift-otter`), and `wait`/`stop`/`show` accept a run id: the `@` sigil is optional here because a run id carries none. `steer` and `queue` require the `@` sigil and fan out instead — see below.

## Steer Live Agents

`rimz steer` sends human-authored text to live agent panes immediately, addressed like Slack: `@<agent>` names who, `#<worktree>` names the channel.

```sh
rimz steer @swift-otter -- "Please inspect the failing test and propose the smallest fix."
rimz steer @claude-2#cli-docs --no-enter -- "Use the docs branch only."   # paste, don't submit yet
rimz steer @codex -- "Rebase on main when the run lands."
rimz steer @all --yes -- "Pause and report status."                       # broadcast, skip the prompt
rimz steer tmux:%12 --force -- "Answer the pending prompt with option 2."  # override a pending ask
```

```sh
rimz steer [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--no-enter] [--force] [--yes] -- <TEXT...>
```

`<TARGET>` is an `@`-mention or a pane id. `@swift-otter` (pet name), `@claude-2` (kind ordinal), and a session-id prefix name one agent; `@codex` (a kind) and `@all` fan out to every match in the channel. The channel is the current worktree unless you append `#<worktree>` or pass `--worktree`; a pane id (`tmux:%12`) is a precise, channel-agnostic address. A bare selector without `@` is rejected with a `did you mean @…?` hint. A bare `@<kind>` or `@all` also reaches a codex you started in a fresh pane before its first turn: `steer` addresses the pane it types into, so a just-launched agent is steerable without waiting for it to register a session.

A fan-out to more than one agent asks for confirmation; `--yes` (`-y`) skips the prompt, and off a TTY the broadcast refuses without it. `steer` types the text as a bracketed paste and then presses Enter as a discrete keystroke outside the paste, so every agent submits the message instead of taking a newline; `--no-enter` pastes without submitting. A pending ask attached to an agent reserves the next input for that ask and skips that agent; `--force` records the override and sends anyway. One blocked or paneless agent never aborts the rest — `steer` prints which agents it reached and which it skipped. The audit event records metadata and text length, not message content.

Target resolution, the bracketed-paste mechanism, and pane-answering resolver behavior are covered in [harness.md → Steering and queuing live agents](../../internals/agents/harness.md#steering-and-queuing-live-agents) and [resolver internals](../../internals/agents/resolvers.md).

## Queue The Next Message

`rimz queue` stores text for agents and delivers it after each reaches a safe turn boundary. It uses the same `@<agent>#<worktree>` grammar as `steer`. Because a queued message is durable and keyed on a session, `queue` addresses bound agents; a freshly started pane with no session yet is refused with a pointer to `steer`, which reaches the pane directly.

```sh
rimz queue @swift-otter -- "After this turn, add focused tests for the parser."
rimz queue add @codex#cli-docs --on any -- "If the run failed, capture the error first."
rimz queue @all --yes -- "When you reach a boundary, summarize what changed."
rimz queue list --json
rimz queue remove msg_01J...
rimz queue clear @claude-2#cli-docs
```

```sh
rimz queue [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--on done|any] [--no-enter] [--yes] -- <TEXT...>
rimz queue add [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--on done|any] [--no-enter] [--yes] -- <TEXT...>
rimz queue list [--json] [REF]
rimz queue remove <MESSAGE_ID>
rimz queue clear [--worktree <WORKTREE>] <REF>
```

The bare form and `queue add` do the same work and take an `@`-mention. A mention that fans out (`@codex`, `@all`) queues one message per matched agent and asks for confirmation past the first; `--yes` (`-y`) skips the prompt. `--on done` is the default gate and delivers after the agent is `idle` or `success`; `--on any` also delivers after `failed`; `running`, `waiting`, and `paused` keep the message pending. Delivered text rides as a bracketed paste with a discrete submit Enter, the same path as `steer`; `--no-enter` stores the text without it. The whole `queue` family takes the `@`-mention grammar — `list <ref>` and `clear <ref>` require the sigil and resolve a single agent.

Delivery is FIFO per agent, and one message is attempted per unparked root turn end. Rimz waits briefly for the pane composer to settle, re-checks the ledger snapshot, skips delivery while a pending ask is attached, claims the queue head, sends through the pane primitive, and marks the message delivered. Failed sends return to `pending` with an attempt count and become `abandoned` after the retry cap.

Queued delivery requires installed and trusted hooks, because turn-end hooks trigger the delivery helper. The record layout, gates, delivery walk, and hazards are in [harness.md → Steering and queuing live agents](../../internals/agents/harness.md#steering-and-queuing-live-agents).

## Drive Panes

`rimz pane` exposes the public pane primitives that humans, resolvers, and scripts share — see the room as panes, read what is on screen, type into one, and move focus.

```sh
rimz pane list
rimz pane capture zellij:terminal_4 --lines 80                                # read the visible buffer
rimz pane send zellij:terminal_4 --key ctrl-u --enter -- "cargo xtask test"   # clear line, type, run
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

`list` is the room seen as panes: every pane grouped under its native tab, each row carrying the agent-colleague that lives in it (`@kind#worktree`), its status, the foreground command, and the working directory. A `●` marks the active pane in each tab.

```text
⑂ auth-refresh
 ●  @claude#auth-refresh   running   claude   ~/code/qe-wt/auth-refresh   zellij:terminal_3
    @codex#auth-refresh    idle      codex    ~/code/qe-wt/auth-refresh   zellij:terminal_4
    -                      -         zsh      ~/code/qe-wt/auth-refresh   zellij:terminal_5

claude:query-engine
 ●  @claude#main           waiting   claude   ~/code/query-engine         zellij:terminal_8
    -                      -         vim      ~/code/query-engine         zellij:terminal_9
```

The agent annotations are a best-effort overlay, folded from the workspace snapshot the same way the sidebar reads it: a pane binds to an agent by the same stamped-id-plus-process-start rule the sidebar's cards use, so a pane the multiplexer has since handed back to a shell wears no handle. The tab grouping always works, and when no snapshot is reachable (no ledger, or a foreign `--session-name`) the panes still list, just without the `@handle`. The default session is the cwd's workspace session; `--json` emits the tab tree with a per-pane `agent` object (`kind`, `handle`, `status`, `worktree`). `capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention to a pane. Named keys are `enter`, `escape`, `tab`, `backspace`, `up`, `down`, `left`, `right`, `ctrl-c`, `ctrl-d`, and `ctrl-u`; aliases include `return`, `esc`, `bs`, `control-c`, `control-d`, and `control-u`.

Pane capture is untrusted terminal text. Scripts and resolvers match bounded patterns before sending text back, and `pane send` is the same explicit input path as `steer` and queued delivery. Resolver patterns and pane-send discipline live in [resolver internals](../../internals/agents/resolvers.md).

## Manage Rimz-Owned Worktrees

`rimz worktree` creates, lists, and removes Rimz-owned git worktrees — the isolated checkouts `rimz agents --worktree` launches agents into.

```sh
rimz worktree new cli-docs --base head                          # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree list --json
rimz worktree remove cli-docs                                   # refuses if dirty or unmerged
rimz worktree remove experiment --force                         # remove anyway
```

```sh
rimz worktree new [NAME] [--base <head|fresh|REF>] [--branch <NAME>]
rimz worktree list [--json]
rimz worktree remove <NAME> [--force]
```

`new` creates a marked worktree under the configured worktree directory. `--base head` branches from the current `HEAD`, `--base fresh` branches from the configured fresh base, and any other value is used as a git ref. `--branch <NAME>` creates that branch instead of using the worktree name.

`list` shows Rimz-owned worktrees for the current repo as the channels they are: each row carries the worktree name, branch, the `@kind` handles of the agent-colleagues working there, a dirty marker, the unmerged-count signal, and the path; `--json` emits structured entries. `remove` refuses dirty worktrees or worktrees with commits not proven merged into their base; `--force` removes anyway and keeps an unmerged branch when needed.

`rimz worktree` requires a git repository. Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary user checkouts. The marker, `.worktreeinclude` seeding, the supervised cleanup decision, and the `rimz gc` sweep are in [harness.md → Rimz-owned worktrees](../../internals/agents/harness.md#rimz-owned-worktrees) and [Cleanup](../../internals/agents/harness.md#cleanup).
