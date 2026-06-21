# Agent control CLI

These commands are how you run the fleet: list the room's agents, launch laid-out panes and supervised script turns, talk to live agents by name, read their transcripts, and manage the worktrees they work in. Run them from inside the room or anywhere that resolves to the same workspace.

A typical session threads the whole surface together:

```sh
rimz agents claude,codex --worktree=auth-refresh "Refactor token refresh; keep the public API stable."
rimz steer @claude#auth-refresh -- "Start with the refresh-token rotation path."
rimz queue @codex#auth-refresh -- "After your turn, add coverage for the expiry edge cases."
rimz agents focus @claude#auth-refresh        # jump to the pane when it needs you
```

The launch grammar, profiles, and teams these commands consume are configured per machine — see [configuration → agent profiles, commands, and teams](../configuration.md#agent-profiles-commands-and-teams). The launch, run, and delivery machinery lives in [harness.md](../../internals/agents/harness.md).

## Addressing agents

`steer`, `queue`, `transcript`, and the `agents show`/`focus`/`wait`/`stop` verbs share one address grammar: **`@<handle>` names who, an optional `#<channel>` names the worktree or in-place team channel,** and a raw pane id is the precise fallback. This is the one place it is spelled out; every command below assumes it.

**Handles that name one agent:**

- `@swift-otter` — a pet name.
- `@claude-2` — a kind plus ordinal (the ordinal appears only when two of a kind share one worktree).
- `@<session-prefix>` — a leading slice of the session id.

**Handles that name a type and fan out:**

- `@claude` — an agent kind; every Claude in the channel.
- `@planner` — a [profile](../configuration.md#agent-profiles-commands-and-teams) you defined; every agent launched under it.
- `@all` — everyone in the channel.

**Channels** scope the lookup to a worktree or in-place team:

- `#auth-refresh` matches by branch, generated worktree name, or directory basename; `--worktree auth-refresh` is the flag spelling.
- `#query-engine/pcr` matches the named team `pcr` launched in-place from the `query-engine` directory.
- The default channel is the worktree you run the command in.
- A team member pane launched in-place carries its team channel, so its own `rimz` commands default to `<dir>/<team>`.
- A pane id (`tmux:%12`, `zellij:terminal_3`) addresses one pane directly and ignores channels.

**One agent or many:**

- The management verbs (`show`, `focus`, `wait`, `stop`) act on exactly one agent, so a handle that matches several is an error that lists the candidates to pick from.
- `steer` and `queue` fan out: a multi-match is ambiguous until you opt in with `--all` (or address `@all`), and a fan-out confirms past the first match unless `--yes` — off a TTY it refuses without it.

The `@` sigil is required for `steer` and `queue` (it also keeps a target from being read as a launch spec); `show`, `wait`, and `stop` also accept a bare selector (`swift-otter`) or a run id. The deeper resolution rules are in [harness.md → The address](../../internals/agents/harness.md#the-address).

## Agents

`rimz agents` is the card surface and the single launcher: list the room, launch a layout, run a supervised turn, then focus, wait on, or stop what you started. The subsections below cover the forms worth knowing; run `rimz agents --help` (and `--help` on each subcommand) for the full flag list.

### Launch a layout

A `<SPEC>` is a shape, and the optional `PROMPT` broadcasts to every agent cell in it.

```sh
rimz agents peer                                    # built-in claude,codex side by side
rimz agents claude,codex+term                       # Claude | Codex stacked over a shell
rimz agents claude,codex --worktree=cli-docs "Review the CLI docs."
rimz agents codex --from-pr 42 "Review this pull request."
rimz agents 'vim,codex+term' "Review the CLI docs."  # a raw command cell beside an agent
rimz agents claude --worktree "Take one approach."   # parallel attempts, each in its own fresh worktree
rimz agents claude --worktree "Take another approach."
```

The spec is a named [team](../configuration.md#agent-profiles-commands-and-teams) or an inline grammar: **commas split columns, plus signs stack rows,** and each cell is `term`, an agent kind, a virtual `<kind>-<mode>` cell, a configured profile, or a configured command. The built-in `peer` team is the roleless `claude,codex`. The full grammar and how cells compile to panes are in [harness.md → The layout IR](../../internals/agents/harness.md#the-layout-ir).

Permission-mode cells exist where the adapter supports them: `-auto`, `-ask`, `-plan`, and `-yolo` (so `claude-plan` passes plan mode while `codex-plan` has none and keeps the default posture), and `-ping` opens the agent at lowest effort with a `"ping"` prompt to keep the provider window warm. The built-in set is `claude-{auto,ask,plan,yolo,ping}`, `codex-{auto,ask,plan,yolo,ping}`, and `pi-{ask,plan}`. On the command line, `--ask` keeps native prompts and `--yolo` passes the adapter's bypass flags; with neither, each provider keeps its own prompting. A second positional that is itself a known cell is rejected with a `rimz agents a,b` hint, so the old space-separated fan-out never silently becomes a prompt.

### Shared launch params

These broadcast to every agent cell and each adapter renders them into its own native flags, so one flag works across providers:

- `--model <MODEL>` selects the provider model.
- `--effort <LEVEL>` sets reasoning effort where the agent exposes it. Levels are provider-specific (Claude `low|medium|high|xhigh|max`, Codex `minimal|low|medium|high|xhigh`, Pi `off|minimal|low|medium|high|xhigh`).
- `--system-prompt-file <PATH>` replaces the agent's base system prompt; `--append-system-prompt-file <PATH>` keeps the base and appends rules where supported.
- `--description <TEXT>` is a card label only — it seeds the card's second line and never enters the agent's argv or environment, and the agent's own session preview replaces it.

A configured profile renders first, so an explicit flag on the command line wins. The launcher resolves prompt files to absolute paths and refuses a missing one before launch; a param the chosen adapter has no flag for fails the launch and names the offending flag.

### Worktree and placement

`-w`/`--worktree` reuses or creates a named worktree (`--worktree=docs` or `--worktree docs`); bare `--worktree` creates a fresh generated one. `--from-pr <number|url>` creates the worktree from a pull request head and implies a worktree launch — pair it with `--worktree <NAME>` to name the local worktree, or accept `pr-<N>`. A worktree launch names its backend tab `#<NAME>`, matching the channel in agent addresses.

Placement follows intent under the default `auto` policy: a worktree launch, a named team, or a multi-cell spec opens its own tab, while a single non-worktree agent takes over the current pane and returns to the shell when it exits. `--new-pane` forces a split (rejected for a multi-cell spec), `--new-tab` forces a tab, and `--bg` downgrades an in-place launch to a split so focus stays put. The per-machine [`[agents] placement`](../configuration.md#agent-profiles-commands-and-teams) default sets the policy when no flag is given. The split-versus-tab mechanics are in [harness.md → Backend shape and placement](../../internals/agents/harness.md#backend-shape-and-placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the result, and exits with the run's status code (`0` completed, `1` failed, `124` timed out, `130` canceled) — so a script branches on the outcome.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --detach   # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
cat build-error.txt | rimz agents claude -p 'explain the root cause' > out.txt
```

- `--detach` prints the pet name and returns immediately; use that name with `steer`, `agents wait`, `agents show`, or `agents stop`.
- `--output-format` shapes the print: `text` (default) prints the final assistant message, `json` prints the full run record, `stream-json` emits run events as NDJSON while the turn runs (incompatible with `--detach`).
- `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT` and folds in piped stdin after it; `stream-json` reads user messages from stdin until EOF and refuses a positional prompt.
- `--max-turns <N>` caps the agentic turn count where the adapter exposes a native limit (Claude today); an agent without one refuses the run.

Supervised runs need installed and trusted hooks, because hooks are the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [harness.md → Supervised runs](../../internals/agents/harness.md#supervised-runs).

### List, inspect, focus, wait, and stop

```sh
rimz agents                              # live root-agent cards, current channel
rimz agents list --all                   # every channel
rimz agents list --worktree auth-refresh # one branch / worktree / dir
rimz agents show swift-otter             # one card plus its newest run record
rimz agents focus @claude-2#cli-docs     # jump to the pane
rimz agents wait swift-otter --stream    # block until it lands, tailing the transcript
rimz agents stop run_0123…               # cancel a run or close a pane
```

Bare `rimz agents` lists live root-agent cards in attention order, scoped to the current channel and widened with `list --all`. The `AGENT` column is the shortest handle you can type back — its role (`@coder`), else its profile (`@planner`), else `@<kind>`, growing an ordinal only when two of a kind share one worktree. `show` prints one card and its newest attached run record, plus an `ask` line when the agent is waiting on a native prompt. `--json` selects JSON for `list` and bare `agents` (supervised `-p` uses `--output-format` instead).

`focus` jumps to an agent's pane. `wait` blocks on a supervised run (by run id or pet name) or an interactive agent reaching an idle/success gate; `--stream` tails the transcript and `--from-start` replays from the top. `stop` tears down a run's pane — canceling supervision while the run is live, reclaiming a completed `--keep` pane — or closes the agent's pane when the ref names no run. All four resolve to exactly one agent, so a fan-out match is an error here (see [Addressing agents](#addressing-agents)).

## Steer live agents

`rimz steer` sends text to live agent panes **right now.**

```sh
rimz steer @swift-otter -- "Inspect the failing test and propose the smallest fix."
rimz steer @claude-2#cli-docs --no-enter -- "Use the docs branch only."   # paste, don't submit
rimz steer @planner -- "Rebase on main when the run lands."                # address a profile
rimz steer @codex --all -y -- "Pause and report status."                  # fan out to every codex
rimz steer @planner#feat/x --create -- "Draft the new endpoint."          # launch it if not running
rimz steer @codex --smart-compact 70% -- "Continue the refactor."         # /compact first past 70% full
rimz steer @claude --file ./review-notes.md                               # send a file verbatim
```

Address the target with the [agent-address grammar](#addressing-agents). Steer delivers to every reachable agent and prints which it reached and which it skipped, so one blocked agent never stops the rest. The audit event records metadata and text length, never the message content.

The flags worth knowing tune delivery (run `rimz steer --help` for the rest):

- `--no-enter` pastes the text without submitting; otherwise the text rides as a bracketed paste and Enter lands as a discrete keystroke, so a `\n` in the text stays a soft composer newline and a multi-line prompt lands multi-line (write `\\` for a literal backslash).
- `--file <PATH>` reads the prompt from a file and sends it byte-for-byte — real newlines stay soft breaks and backslashes stay literal, so code and regex paste unchanged. It conflicts with inline text.
- `--create` launches a missing agent from a kind or profile address (opening the worktree when the channel is new) with the text as its first prompt; an instance handle like a pet name cannot create.
- `--force` types over a pending native ask, which `steer` otherwise skips to avoid clobbering the reserved input.
- `--smart-compact <PCT|TOKENS>` submits the agent's `/compact` first when its context window has reached the threshold (a percentage like `70%` or an occupied-token count like `120000`), so the prompt lands against a fresh window. Unset, [`[harness] smart_compact`](../configuration.md#smart-compaction) supplies the threshold; a window below it sends untouched.
- `--no-from` sends the bytes exactly. By default a Rimz-launched agent's send arrives as `from @sender: text`, gaining `#channel` when it crosses channels.

A bare `@<kind>`, `@<profile>`, or `@all` also reaches an agent you just started in a fresh pane, before its first turn — `steer` addresses the pane it types into, so a just-launched agent is steerable without waiting for it to register a session. The bracketed-paste mechanism and pane-answering resolver behavior are in [harness.md → Talk and queue](../../internals/agents/harness.md#talk-and-queue).

## Queue the next message

`rimz queue` stores text and delivers it after each addressed agent reaches a safe turn boundary. It mirrors `steer` — same address grammar, same `--worktree`, `--no-enter`, `--force`, `--all`, `--create`, `--yes`, `--smart-compact`, `--file`, and `--no-from` — and adds `--on`, the delivery gate that is the whole difference between sending now and sending at a boundary.

```sh
rimz queue @swift-otter -- "After this turn, add focused tests for the parser."
rimz queue add @codex#cli-docs --on any -- "If the run failed, capture the error first."
rimz queue @all --yes -- "When you reach a boundary, summarize what changed."
rimz queue list --json
rimz queue remove msg_01J…
rimz queue clear @claude-2#cli-docs
```

The bare form and `queue add` do the same work. `--on done` (the default) delivers once the agent is `idle` or `success`; `--on any` also delivers after `failed`; `running`, `waiting`, and `paused` keep the message pending. Delivery is FIFO per agent, one message per unparked turn end; a failed send returns to pending and is abandoned after the retry cap. A queued message is durable and keyed on a session, so `queue` addresses bound agents — a freshly started pane with no session yet is refused with a pointer to `steer`, which reaches the pane directly.

Queued delivery needs installed and trusted hooks, because turn-end hooks trigger the delivery helper. The record layout, gates, and delivery walk are in [harness.md → Talk and queue](../../internals/agents/harness.md#talk-and-queue).

## Inspect transcripts

`rimz transcript` reads a running agent's local transcript and renders the conversation Rimz can see without joining the agent process.

```sh
rimz transcript @swift-otter            # one agent's turns
rimz transcript @codex#cli-docs --last 4
rimz transcript #cli-docs               # the channel timeline
rimz transcript @all#cli-docs --details
rimz transcript --json
```

A single-agent target prints turns — the user prompt and that turn's final assistant message — while `--details` prints every normalized message and `--last <N>` keeps the last N turns. A pending ask prints at the bottom with its options, so you can resolve a blocker before typing over it. A channel target (`#worktree`, `@all`, or no target for the current channel) fuses every root agent's messages into one timestamp-ordered timeline labelled by handle: prompts render as `you→@handle:` and replies as `@handle:`. `--json` emits `{agent, turns, ask}` for one agent and `{channel, timeline, asks}` for a channel.

## Drive panes

`rimz pane` exposes the public pane primitives that humans, resolvers, and scripts share: see the room as panes, read what is on screen, type into one, and move focus.

```sh
rimz pane list
rimz pane capture zellij:terminal_4 --lines 80                                # read the visible buffer
rimz pane send zellij:terminal_4 --key ctrl-u --enter -- "cargo xtask test"   # clear line, type, run
rimz pane focus tmux:%3
rimz pane split
rimz pane detach
```

`list` is the room seen as panes: every pane grouped under its native tab, each row labelled with the agent that lives in it (`@kind#worktree`) or `process` for a plain pane, with status and working directory. Rimz's own sidebar pane is omitted, and a `●` marks the active pane in each tab.

```text
#auth-refresh
 ●  @claude#auth-refresh   running   ~/code/qe-wt/auth-refresh   zellij:terminal_3
    @codex#auth-refresh    idle      ~/code/qe-wt/auth-refresh   zellij:terminal_4
    process                -         ~/code/qe-wt/auth-refresh   zellij:terminal_5
```

The agent labels are a best-effort overlay folded from the workspace snapshot, so a pane the multiplexer has handed back to a shell reads `process`; the tab grouping always works, even with no snapshot reachable. `--json` emits the tab tree with a per-pane `kind`, `command`, `cwd`, and `pid`, and an `agent` object for agent panes. `capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention. Named keys are `enter`, `escape`, `tab`, `backspace`, the four arrows, `ctrl-c`, `ctrl-d`, and `ctrl-u`, with aliases like `return`, `esc`, and `bs`.

Pane capture is untrusted terminal text — scripts and resolvers match bounded patterns before sending anything back, and `pane send` is the same explicit input path as `steer`. Resolver patterns and pane-send discipline are in [resolver internals](../../internals/agents/resolvers.md).

## Schedule turns with loop

`rimz loop` schedules one supervised turn on this machine's OS scheduler. A task's `--spec` must resolve to a single agent cell — a kind, profile, or virtual cell — because the scheduled run owns one transient supervised pane; teams, multi-cell layouts, and command cells are rejected.

```sh
rimz loop add morning --spec claude-ping --at 07:00 --days weekdays
rimz loop add pr-watch --spec codex --prompt "check CI on the release PR" --every 15m --mode auto --root .
rimz loop list
rimz loop install pr-watch --scheduler cron
rimz loop uninstall pr-watch
rimz loop remove pr-watch
```

Schedules come in four shapes: calendar (`--at` plus optional `--days`), interval (`--every 15m`), raw cron (`--cron`), and one-shot (`--once` or `--in 30m`). A `<kind>-ping` spec is the window-primer — `add` defaults its prompt to `ping`, and the run skips when the provider's window is already counting down. `loop add` records the intent; `loop install` applies it to the scheduler after a consent preview. The task model and config shape are in [loop.md](../../internals/agents/loop.md).

## Manage Rimz-owned worktrees

`rimz worktree` creates, lists, and removes the isolated git checkouts that `rimz agents --worktree` launches agents into.

```sh
rimz worktree new cli-docs --base head                  # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree new --from-pr 42                           # branch pr-42 from the PR head
rimz worktree list --json
rimz worktree remove cli-docs                            # refuses if dirty or not landed
rimz worktree remove experiment --force                  # remove anyway
```

`new` creates a marked worktree under the configured [`[agents.worktree] dir`](../configuration.md#worktrees). `--base head` branches from `HEAD`, `--base fresh` from the configured fresh base, and any other value is a git ref. `--from-pr <number|url>` fetches the pull request head through `origin` and creates a `pr-<N>` branch unless `--branch` names it (GitHub/Gitea/Forgejo use `refs/pull/<N>/head`, GitLab `refs/merge-requests/<N>/head`). `list` shows Rimz-owned worktrees as the channels they are — name, branch, the `@kind` handles working there, a dirty marker, the landed signal, and the path. `remove` refuses a dirty worktree or one whose content is not proven landed on its base; `--force` removes anyway.

Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary checkouts. The marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, and the `rimz gc` sweep are in [worktree.md](../../internals/agents/worktree.md).
