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
rimz transcript @codex#auth-refresh      # one member's turn history
rimz transcript #auth-refresh            # the channel timeline
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
rimz agents <SPEC> [PROMPT] [-w|--worktree[=<NAME>]] [--name <PETNAME>] [--new-pane|--new-tab] [--bg] [--ask|--yolo] [--system-prompt-file <PATH>] [--effort <LEVEL>] [-- PASSTHROUGH...]
rimz agents <SPEC> [PROMPT] -p|--print [--system-prompt-file <PATH>] [--effort <LEVEL>] [--timeout <DURATION>] [--detach] [--output-format <text|json|stream-json>] [--input-format <text|stream-json>] [--keep]
rimz transcript [TARGET] [-w|--worktree <WORKTREE>] [-n|--last <N>] [--details] [--json]
```

### Listing and inspecting

Bare `rimz agents` lists live root-agent cards in attention order. The lead `AGENT` column is the agent's canonical address — its role (`@coder#auth-refresh`) when a unique team role names it, then its profile (`@planner#auth-refresh`), else `@<kind>#<channel>`, growing an ordinal (`@claude-2#auth-refresh`) only when two of a kind share one worktree — so the column reads as the address you would type back. `list --all` adds audit rollup rows, `--worktree` filters by branch, worktree name, or directory basename, and `--json` emits the filtered `AgentState` records. `show` prints one card (its handle, kind, petname, session, status, model, context, worktree, pane) and its newest attached run record when present. When the agent is waiting on a native ask, `show` adds an `ask` line with the question and options; `show --json` includes the same projection as `ask`. `--json` selects JSON for `list` and bare `agents` card output — not for `-p`, which has its own `--output-format`.

### Inspect transcripts

`rimz transcript` reads the local transcript or rollout JSONL for a running agent and renders the conversation Rimz can inspect without joining the agent process.

```sh
rimz transcript @swift-otter
rimz transcript @codex#cli-docs --last 4
rimz transcript #cli-docs
rimz transcript @all#cli-docs --details
rimz transcript --json
```

A single-agent target prints turns: the user prompt and that turn's final assistant message. `--details` prints every normalized user and assistant message in order, and `--last <N>` keeps the last N turns. A pending ask attached to the agent prints at the bottom with its options so you can resolve the blocker before typing over it.

A channel target (`#worktree`, `@all`, or no target for the current channel) fuses every root agent's messages into one timestamp-ordered timeline labelled by handle. User prompts render as `you→@handle:` and assistant messages as `@handle:`. `--details` fuses every message; the default fuses turn summaries. `--last <N>` keeps the last N timeline entries. `--json` emits `{agent, turns, ask}` for one agent and `{channel, timeline, asks}` for a channel.

### The launch spec

`<SPEC>` is a named `[agents.teams]` team or an inline layout grammar: commas split columns, plus signs stack rows, and each inline cell is `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` / `<kind>-ping` cell, a configured `[agents.profiles]` entry, or a configured `[agents.commands]` entry. The built-in `peer` team is the roleless `claude,codex`. The full grammar and how cells compile to panes are in [harness.md → The layout IR](../../internals/agents/harness.md#the-layout-ir).

Permission-mode suffixes (`-auto`, `-ask`, `-plan`, `-yolo`) are the official virtual mode variants; availability depends on what the adapter supports — `claude-plan` passes `--permission-mode plan`, while `codex-plan` has no plan-mode equivalent and falls back to the default posture. The built-in `-ping` suffix opens the agent at lowest effort with a `"ping"` initial prompt (Claude `--effort low`; Codex `-c model_reasoning_effort=low`) and keeps the 5-hour session window alive. Built-in virtual cells: `claude-auto`, `claude-ask`, `claude-plan`, `claude-yolo`, `claude-ping`, `codex-auto`, `codex-ask`, `codex-plan`, `codex-yolo`, `codex-ping`, `pi-ask`, `pi-plan`.

`PROMPT` is the optional second positional, broadcast to every agent cell. Interactive launches pass no approval override by default, so each provider keeps its native prompts; `--ask` keeps or returns to native prompts where supported, and `--yolo` passes the adapter's bypass flags. `-- PASSTHROUGH...` appends raw agent argv to every agent cell after profile/role preset args and any explicit permission args. A second positional that is itself a known cell or team is rejected with a `rimz agents a,b` hint, so the removed space-separated fan-out form never silently becomes a prompt.

### Shared launch params

`--system-prompt-file <PATH>` and `--effort <LEVEL>` broadcast to every agent cell like `PROMPT`, and each adapter renders them into its native flags: `--system-prompt-file` replaces the agent's base system prompt (Claude `--system-prompt-file <PATH>`; Codex `-c model_instructions_file=<PATH>`), and `--effort` sets reasoning effort (Claude `--effort <LEVEL>`; Codex `-c model_reasoning_effort=<LEVEL>`). The launcher resolves the prompt file to an absolute path and refuses a missing file before launch. Levels are provider-specific — Claude takes `low|medium|high|xhigh|max`, Codex takes `minimal|low|medium|high|xhigh` — and an agent with no native flag for a param (Pi today) refuses the launch with the offending flag named. A configured profile preset renders first, so an explicit `--effort` on the command line wins.

### Worktree and placement

`-w`/`--worktree` takes a value as `--worktree=docs` or space-separated `--worktree docs` (both reuse or create that worktree), while bare `--worktree` creates a fresh generated worktree. A worktree launch names the backend tab `#<NAME>`, matching the channel suffix used in agent addresses; a launch without a worktree names the tab `<kind>:<dir>`. A single-agent launch into a fresh generated worktree uses the generated name as a pet-name candidate unless `--name` is set; named shared worktrees keep independent agent names.

Placement follows intent. Under the default `auto` policy a worktree launch, named team, or multi-cell inline spec opens its own tab, while a single non-worktree agent takes over the current pane and returns to the shell when the agent exits. `--new-pane` forces a split for a single agent cell — including a single worktree launch — run from inside the room, and is rejected for a multi-cell spec; `--new-tab` forces a tab. The per-machine `[agents] placement` default sets the policy when neither flag is given, and `placement = "pane"` splits a single non-worktree agent while worktrees and teams keep their tab. `--bg` downgrades an in-place launch to a split so focus can stay on the launching pane. The split-versus-tab mechanics are in [harness.md → Backend shape and placement](../../internals/agents/harness.md#backend-shape-and-placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the final assistant message, and exits with the run status code: `0` completed, `1` failed, `124` timed out, `130` canceled. `--detach` prints the pet name and returns immediately; use that name with `steer`, `agents wait`, `agents show`, or `agents stop`.

`--output-format` selects how `-p` renders the run: `text` (default) prints the final assistant message, `json` prints the full run record, and `stream-json` emits run events as NDJSON while the turn runs. `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT`, while `stream-json` reads user messages from stdin until EOF and refuses a positional `PROMPT`. `stream-json` output cannot combine with `--detach`.

Supervised `-p` runs require installed and trusted hooks, because hooks provide the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [harness.md → Supervised runs](../../internals/agents/harness.md#supervised-runs).

### Loop tasks

`rimz loop` schedules one supervised turn on this machine's OS scheduler. A task's `--spec` must resolve to one agent cell: a kind, profile, or virtual cell. Teams, multi-cell layouts, and command cells are rejected because the scheduled run owns one transient supervised pane.

```sh
rimz loop add morning --spec claude-ping --at 07:00 --days weekdays
rimz loop add pr-watch --spec codex --prompt "check CI on the release PR" --every 15m --mode auto --root .
rimz loop list
rimz loop install pr-watch --scheduler cron
rimz loop uninstall pr-watch
rimz loop remove pr-watch
```

```sh
rimz loop add <NAME> --spec <SPEC> [--prompt <TEXT>|--prompt-file <PATH>] [--at <HH:MM> [--days <MASK>]|--every <DURATION>|--cron <EXPR>|--in <DURATION>] [--once] [--root <PATH>] [--worktree <NAME>] [--mode <auto|ask|yolo>] [--effort <LEVEL>] [--system-prompt-file <PATH>] [--timeout <DURATION>]
rimz loop list
rimz loop install [NAME] [--scheduler <auto|systemd|cron>] [-y|--yes]
rimz loop uninstall [NAME] [--scheduler <auto|systemd|cron>] [-y|--yes]
rimz loop remove <NAME>
```

Schedule forms are calendar (`--at` plus optional `--days`), interval (`--every 15m`), raw cron (`--cron`), and one-shot (`--once`, or `--in 30m`). A `<kind>-ping` spec is the window-primer: `add` defaults the prompt to `ping`, `run` checks the provider's account window and skips when it is already counting down, and install/run preflight still requires installed and trusted hooks. Scheduler artifacts and config shape are in [loop.md](../../internals/agents/loop.md).

### Focus, wait, and stop

`focus` jumps to an agent's pane. `wait` waits for a supervised run by run id or pet name, or for an interactive agent to reach an idle/success gate; `--stream` tails the transcript (`--from-start` replays from the top). `stop` cancels a supervised run when the ref names one, otherwise it closes the agent pane.

`<REF>` accepts a pane id (`tmux:%1`, `zellij:terminal_3`) or an `@`-mention: `@swift-otter` (pet name), `@claude-2` (kind ordinal), `@claude` (a kind), `@planner` (a profile), or `@<session-prefix>`. Append `#<worktree>` to scope the lookup; it narrows by branch, generated worktree name, or directory basename, and defaults to the current worktree. These management commands resolve to one agent, so a fan-out mention (`@claude` matching several, or `@all`) is an ambiguity here — name one. They also accept a bare selector (`swift-otter`), and `wait`/`stop`/`show` accept a run id: the `@` sigil is optional here because a run id carries none. `steer` and `queue` require the `@` sigil and fan out instead — see below.

## Steer Live Agents

`rimz steer` sends text to live agent panes immediately, addressed through the [agent-address grammar](../../internals/agents/harness.md#the-address): `@<handle>` names who, `#<channel>` names the worktree.

```sh
rimz steer @swift-otter -- "Please inspect the failing test and propose the smallest fix."
rimz steer @claude-2#cli-docs --no-enter -- "Use the docs branch only."   # paste, don't submit yet
rimz steer @planner -- "Rebase on main when the run lands."                # address a profile
rimz steer @codex --all -y -- "Pause and report status."                  # fan out to every codex
rimz steer @planner#feat/x --create -- "Draft the new endpoint."          # launch it if not running
rimz steer tmux:%12 --force -- "Answer the pending prompt with option 2."  # override a pending ask
rimz steer @codex --auto-compact 70% -- "Continue the refactor."          # /compact first past 70% full
rimz steer @claude -- "Step 1: read the spec.\nStep 2: list the gaps."    # \n is a soft composer newline
rimz steer @claude --file ./review-notes.md                               # send a file's contents verbatim
rimz steer @codex --no-from -- "exact text"                               # suppress agent sender attribution
```

```sh
rimz steer [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--no-enter] [--force] [--all] [--create] [--auto-compact <PCT|TOKENS>] [--yes] [--file <PATH>] [--no-from] -- <TEXT...>
```

`<TARGET>` is an `@`-mention or a pane id. `@swift-otter` (pet name), `@claude-2` (kind ordinal), and a session-id prefix name one agent; `@codex` (a kind) and `@planner` (a profile) name a type, and `@all` is the broadcast handle. The channel is the current worktree unless you append `#<worktree>` or pass `--worktree`; a pane id (`tmux:%12`) is a precise, channel-agnostic address. A bare selector without `@` is rejected with a `did you mean @…?` hint. A bare `@<kind>`, `@<profile>`, or `@all` also reaches a codex you started in a fresh pane before its first turn: `steer` addresses the pane it types into, so a just-launched agent is steerable without waiting for it to register a session.

A selector that matches several agents is an ambiguity that lists the handles to pick one; `--all` (or the explicit `@all`) opts into the fan-out, which confirms past the first match unless `--yes` (`-y`) skips it, and off a TTY refuses without it. `--create` launches a missing agent from a kind or profile address — opening the worktree when the channel is new — with the text as its first prompt; an instance handle (pet name, ordinal, session id) cannot create. `steer` types the text as a bracketed paste and then presses Enter as a discrete keystroke outside the paste, so the submit lands as a keystroke while any `\n` inside the text stays a soft composer newline — a multi-line prompt lands multi-line (write `\\` for a literal backslash). A Rimz-launched agent sends with `@sender: ` prepended, adding `#channel` when it crosses channels; `--no-from` keeps the delivered bytes exact. `--no-enter` pastes without submitting. `--file <PATH>` reads the prompt from a file instead of the `-- text` argv and sends it verbatim — real newlines stay soft breaks and every backslash stays literal, so code and regex paste unchanged — and conflicts with inline text, refusing an empty or unreadable file. A pending ask attached to an agent reserves the next input for that ask and skips that agent; `--force` records the override and sends anyway. `--auto-compact <PCT|TOKENS>` submits the agent's `/compact` ahead of the text when its context window has reached the threshold — a percentage (`70%`) or an occupied-token count (`120000`) — so the prompt lands against a fresh window instead of racing the agent's own auto-compaction; a window below the threshold (or an unbound pane with no fill reading) sends untouched. `steer` delivers to every reachable agent and prints which it reached and which it skipped, so a blocked or paneless agent skips while the rest still send. The audit event records metadata, sender address when present, and text length, not message content.

Target resolution, the bracketed-paste mechanism, and pane-answering resolver behavior are covered in [harness.md → Talk and queue](../../internals/agents/harness.md#talk-and-queue) and [resolver internals](../../internals/agents/resolvers.md).

## Queue The Next Message

`rimz queue` stores text for agents and delivers it after each reaches a safe turn boundary. It mirrors `steer` — the same address grammar and the same `--worktree`, `--no-enter`, `--force`, `--all`, `--create`, `--yes`, `--auto-compact`, `--file`, and `--no-from` flags, and the same `\n` soft-newline text — and adds only `--on`, the delivery-timing gate that is the whole difference between sending now and sending at a boundary. Because a queued message is durable and keyed on a session, `queue` addresses bound agents; a freshly started pane with no session yet is refused with a pointer to `steer`, which reaches the pane directly.

```sh
rimz queue @swift-otter -- "After this turn, add focused tests for the parser."
rimz queue add @codex#cli-docs --on any -- "If the run failed, capture the error first."
rimz queue @all --yes -- "When you reach a boundary, summarize what changed."
rimz queue @claude --force -- "Answer the pending prompt, then continue."   # deliver past a pending ask
rimz queue @codex#cli-docs --file ./follow-up.md                            # queue a file's contents verbatim
rimz queue @claude --no-from -- "exact text"                                # suppress agent sender attribution
rimz queue list --json
rimz queue remove msg_01J...
rimz queue clear @claude-2#cli-docs
```

```sh
rimz queue [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--on done|any] [--no-enter] [--force] [--all] [--create] [--auto-compact <PCT|TOKENS>] [--yes] [--file <PATH>] [--no-from] -- <TEXT...>
rimz queue add [OPTIONS] <TARGET> [--worktree <WORKTREE>] [--on done|any] [--no-enter] [--force] [--all] [--create] [--auto-compact <PCT|TOKENS>] [--yes] [--file <PATH>] [--no-from] -- <TEXT...>
rimz queue list [--json] [REF]
rimz queue remove <MESSAGE_ID>
rimz queue clear [--worktree <WORKTREE>] <REF>
```

The bare form and `queue add` do the same work and take an `@`-mention. A selector matching several agents is an ambiguity until `--all` (or `@all`) opts into queuing one message per matched agent, confirming past the first unless `--yes` (`-y`) skips it. `--create` launches a missing kind or profile with the text as its first prompt instead of queuing. `--on done` is the default gate and delivers after the agent is `idle` or `success`; `--on any` also delivers after `failed`; `running`, `waiting`, and `paused` keep the message pending. Delivered text rides as a bracketed paste with a discrete submit Enter, the same path as `steer`, so a `\n` in the text lands as a soft composer newline; `--no-enter` stores the text without the submit. A Rimz-launched agent captures its sender at enqueue time, `queue list` shows it in the `FROM` column, and delivery computes whether to include `#channel` against the target's current channel; `--no-from` stores and delivers a human-style message. `--force` marks the message to deliver past a pending ask at the boundary instead of deferring, mirroring `steer --force`. The whole `queue` family takes the `@`-mention grammar — `list <ref>` and `clear <ref>` require the sigil and resolve a single agent.

Delivery is FIFO per agent, and one message is attempted per unparked root turn end. Rimz waits briefly for the pane composer to settle, re-checks the ledger snapshot, defers delivery while a pending ask is attached unless the message was queued with `--force`, claims the queue head, sends through the pane primitive, and marks the message delivered. Failed sends return to `pending` with an attempt count and become `abandoned` after the retry cap. `--auto-compact <PCT|TOKENS>` is evaluated at this delivery boundary, not at enqueue: when the agent's context fill has reached the threshold — a percentage (`70%`) or an occupied-token count (`120000`) — Rimz submits the agent's `/compact` ahead of the text in the same delivery, so a long-idle queue still compacts against the window the turn actually left behind.

Queued delivery requires installed and trusted hooks, because turn-end hooks trigger the delivery helper. The record layout, gates, delivery walk, and hazards are in [harness.md → Talk and queue](../../internals/agents/harness.md#talk-and-queue).

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

`list` is the room seen as panes: every pane grouped under its native tab, each row labelled with the agent-colleague that lives in it (`@kind#worktree`) or `process` for a plain pane, alongside its status and working directory. Rimz's own sidebar pane is omitted. A `●` marks the active pane in each tab.

```text
#auth-refresh
 ●  @claude#auth-refresh   running   ~/code/qe-wt/auth-refresh   zellij:terminal_3
    @codex#auth-refresh    idle      ~/code/qe-wt/auth-refresh   zellij:terminal_4
    process                -         ~/code/qe-wt/auth-refresh   zellij:terminal_5

claude:query-engine
 ●  @claude#main           waiting   ~/code/query-engine         zellij:terminal_8
    process                -         ~/code/query-engine         zellij:terminal_9
```

The agent annotations are a best-effort overlay, folded from the workspace snapshot the same way the sidebar reads it: a pane binds to an agent by the same stamped-id-plus-process-start rule the sidebar's cards use, so a pane the multiplexer has since handed back to a shell reads `process`. The tab grouping always works, and when no snapshot is reachable (no ledger, or a foreign `--session-name`) the panes still list, just labelled `process` rather than carrying a `@handle`. The default session is the cwd's workspace session; `--json` emits the tab tree with a per-pane `kind` (`agent` or `process`), the pane's `command`, `cwd`, and `pid`, and — for agent panes — an `agent` object (`kind`, `handle`, `status`, `worktree`). `capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention to a pane. Named keys are `enter`, `escape`, `tab`, `backspace`, `up`, `down`, `left`, `right`, `ctrl-c`, `ctrl-d`, and `ctrl-u`; aliases include `return`, `esc`, `bs`, `control-c`, `control-d`, and `control-u`.

Pane capture is untrusted terminal text. Scripts and resolvers match bounded patterns before sending text back, and `pane send` is the same explicit input path as `steer` and queued delivery. Resolver patterns and pane-send discipline live in [resolver internals](../../internals/agents/resolvers.md).

## Manage Rimz-Owned Worktrees

`rimz worktree` creates, lists, and removes Rimz-owned git worktrees — the isolated checkouts `rimz agents --worktree` launches agents into.

```sh
rimz worktree new cli-docs --base head                          # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree list --json
rimz worktree remove cli-docs                                   # refuses if dirty or not landed
rimz worktree remove experiment --force                         # remove anyway
```

```sh
rimz worktree new [NAME] [--base <head|fresh|REF>] [--branch <NAME>]
rimz worktree list [--json]
rimz worktree remove <NAME> [--force]
```

`new` creates a marked worktree under the configured worktree directory. `--base head` branches from the current `HEAD`, `--base fresh` branches from the configured fresh base, and any other value is used as a git ref. `--branch <NAME>` creates that branch instead of using the worktree name.

`list` shows Rimz-owned worktrees for the current repo as the channels they are: each row carries the worktree name, branch, the `@kind` handles of the agent-colleagues working there, a dirty marker, the content-landed signal, and the path; `--json` emits structured entries. `remove` refuses dirty worktrees or worktrees whose content is not proven landed on their base; `--force` removes anyway and keeps a branch when Git still rejects safe deletion.

`rimz worktree` requires a git repository. Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary user checkouts. The marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, the cleanup decision, and the `rimz gc` sweep are in [worktree.md](../../internals/agents/worktree.md).
