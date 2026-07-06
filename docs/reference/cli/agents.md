# Agent control CLI

These commands are how you run the fleet: list the room's agents, launch laid-out panes and supervised script turns, talk to live agents by name, read their transcripts, and manage the channels they work in. Run them from inside the room or anywhere that resolves to the same workspace.

A typical session threads the whole surface together:

```sh
rimz agents claude,codex --worktree=auth-refresh "Refactor token refresh; keep the public API stable."
rimz message --steer @claude#auth-refresh "Start with the refresh-token rotation path."
rimz message @codex#auth-refresh "After your turn, add coverage for the expiry edge cases."
rimz agents focus @claude#auth-refresh        # jump to the pane when it needs you
```

The launch grammar, profiles, and teams these commands consume are configured per machine — see [configuration → agent profiles, commands, and teams](../configuration.md#agent-profiles-commands-and-teams). The launch, run, and delivery machinery lives in [harness.md](../../internals/agents/harness.md).

## Addressing agents

`message`, `transcript`, and the `agents show`/`logs`/`focus`/`wait`/`stop` verbs share one address grammar: **`@<handle>` names who, an optional `#<channel>` names the stamped lane,** and a raw pane id is the precise fallback. This is the one place it is spelled out; every command below assumes it.

**Handles that name one agent:**

- `@swift-otter` — a pet name.
- `@claude-2` — a kind plus ordinal (the ordinal appears only when two of a kind share one worktree).
- `@<session-prefix>` — a leading slice of the session id.

**Handles that name a type and fan out:**

- `@claude` — an agent kind; every Claude in the channel.
- `@planner` — a [profile](../configuration.md#agent-profiles-commands-and-teams) you defined; every agent launched under it.
- `@all` — everyone in the channel.

**Channels** scope the lookup to a named lane, worktree, or in-place team lane stamped at launch:

- `#design` matches a named channel created by [`rimz channel`](./channel.md); `--channel design` is the flag spelling.
- `#auth-refresh` matches by branch, generated worktree name, or directory basename; `--worktree auth-refresh` is the worktree flag spelling.
- `#query-engine/pcr` matches the named team `pcr` launched in-place from the `query-engine` directory.
- The default channel is the named-channel tab or worktree you run the command in.
- A team member pane launched in-place carries `RIMZ_CHANNEL=<dir>/<team>`, so its own `rimz` commands default to that stamped lane.
- A pane id (`tmux:%12`, `zellij:terminal_3`) addresses one pane directly and ignores channels.

**One agent or many:**

- The management verbs (`show`, `logs`, `focus`, `wait`, and `stop` by default) act on exactly one agent, so a handle that matches several is an error that lists the candidates to pick from. `stop --all` is the explicit fan-out exception.
- `message` fan-outs are explicit: a multi-match is ambiguous until you opt in with `--all` or address `@all`; a fan-out delivers to every match with no confirmation and prefixes each delivery with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message.

The `@` sigil is required for `message` (it also keeps a target from being read as a launch spec); `show`, `logs`, `wait`, and `stop` also accept a bare selector (`swift-otter`) or a run id. The deeper resolution rules are in [harness.md → The address](../../internals/agents/harness.md#the-address).

## Agents

`rimz agents` is the card surface and the single launcher: list the room, launch a layout, run a supervised turn, then focus, wait on, or stop what you started. The subsections below cover the forms worth knowing; run `rimz agents --help` (and `--help` on each subcommand) for the full flag list.

### Launch a layout

A `<SPEC>` is a shape, and the optional `PROMPT` broadcasts to every agent cell in it.

```sh
rimz agents peer                                    # built-in claude,codex side by side
rimz agents claude,codex+term                       # Claude | Codex tiled over a shell
rimz agents claude/codex/term                       # one Zellij stack; tmux tiles rows
rimz agents claude,codex --channel=design "Draft the API shape."
rimz agents claude,codex --worktree=cli-docs "Review the CLI docs."
rimz agents codex --from-pr 42 "Review this pull request."
rimz agents 'vim,codex+term' "Review the CLI docs."  # a raw command cell beside an agent
rimz agents pcr.planner                              # re-add one role of team pcr
rimz agents pcr --resume                             # reopen the newest closed pcr cohort
rimz agents pcr --continue                           # same as --resume
rimz agents pcr -w restore-living-team --resume      # reopen that exact team instance
rimz agents claude,codex --resume                    # reopen the newest matching inline cohort
rimz agents claude --resume                          # resume the freshest closed Claude session
rimz agents claude --worktree "Take one approach."   # parallel attempts, each in its own fresh worktree
rimz agents claude --worktree "Take another approach."
```

The spec is a named [team](../configuration.md#agent-profiles-commands-and-teams), one declared role of a team as `<team>.<role>`, or an inline grammar: **commas split columns, plus signs tile rows, slashes stack rows** as a Zellij stack while tmux tiles them, and each cell is `term`, an agent kind, a virtual `<kind>-<mode>` cell, a configured profile, or a configured command. Use `rimz agents <team>.<role>` to re-add one role of a running or stopped team with the same role handle and stamped team lane. The built-in `peer` team is the roleless `claude,codex`. The full grammar and how cells compile to panes are in [harness.md → The layout IR](../../internals/agents/harness.md#the-layout-ir).

`--resume` relaunches a prior cohort matching the same spec, and `--continue` is the same visible alias: a team resumes by team name and role, an inline multi-agent spec resumes by the saved launch group and cell order, and a single kind resumes the freshest closed root session of that kind. Add `-w <NAME>` to resume that exact worktree's cohort; use bare `-w` or omit the flag while running inside a worktree to scope resume to that worktree; run from the project root to keep the room-wide newest-by-spec behavior. Cleanly closed cohort members still match when their worktree exists. Cells with no resumable prior member launch fresh in the matched cohort's cwd and channel, while a matched member that is still live refuses the command so the room does not duplicate the same address. Resume takes identity, cwd, and channel from the ledger, so it conflicts with `PROMPT`, `--from-pr`, `--channel`, `--name`, `--description`, `--model`, `--effort`, `--ask`, `--yolo`, `-p`, system-prompt flags, and passthrough args after `--`.

Permission-mode cells exist where the adapter supports them: `-auto`, `-ask`, `-plan`, and `-yolo` (so `claude-plan` passes plan mode while `codex-plan` has none and keeps the default posture), and `-ping` opens the agent at lowest effort with a `"ping"` prompt to keep the provider window warm. The built-in set is `claude-{auto,ask,plan,yolo,ping}`, `codex-{auto,ask,plan,yolo,ping}`, and `pi-{ask,plan}`. On the command line, `--ask` keeps native prompts and `--yolo` passes the adapter's bypass flags; with neither, each provider keeps its own prompting. A second positional that is itself a known cell is rejected with a `rimz agents a,b` hint, so the old space-separated fan-out never silently becomes a prompt.

### Shared launch params

These broadcast to every agent cell, and each adapter renders them into its own native flags. `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` carry the same meaning and resolution rules as the [profile fields](../configuration.md#profiles) of the same names — a command-line flag renders after any profile and wins. `--effort` levels are provider-specific: Claude `low|medium|high|xhigh|max`, Codex `minimal|low|medium|high|xhigh`, Pi `off|minimal|low|medium|high|xhigh`. `--description <TEXT>` is a card label only: it seeds the card's second line, never enters the agent's argv or environment, and the agent's own session preview replaces it.

### Channel, worktree, and placement

`-w`/`--worktree` reuses or creates a named worktree (`--worktree=docs` or `--worktree docs`); bare `--worktree` creates a fresh generated one. `--from-pr <number|url>` creates the worktree from a pull request head and implies a worktree launch — pair it with `--worktree <NAME>` to name the local worktree, or accept `pr-<N>`. A worktree launch names its backend tab `#<NAME>`, matching the channel in agent addresses.

Relaunching a named team into the same named worktree reconciles with existing state before it creates anything: a live team focuses its current tab, a closed tab with work in progress offers to resume that team in the worktree, and a closed clean merged worktree offers to remove it and launch fresh. Add `--resume` or `--continue` to force a resume of the named worktree's prior cohort even when the worktree is clean or merged.

`--channel <NAME>` launches into a durable named channel, registering it when missing and naming the backend tab `#<NAME>`. Named channels run in the room root and are managed with [`rimz channel`](./channel.md).

Placement follows intent under the default `auto` policy: a named-channel launch, a worktree launch, or a multi-cell spec opens its own tab, and a one-cell non-worktree launch, including a single team role, takes over the current pane and returns to the shell when it exits. `--new-pane` forces a split (rejected for a multi-cell spec), `--new-tab` forces a tab, and `--bg` downgrades an in-place launch to a split so focus stays put. The per-machine [`[agents] placement`](../configuration.md#agent-profiles-commands-and-teams) default sets the policy when no flag is given. The split-versus-tab mechanics are in [harness.md → Backend shape and placement](../../internals/agents/harness.md#backend-shape-and-placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the result, and exits with the run's status code (`0` completed, `1` failed, `124` timed out, `130` canceled) — so a script branches on the outcome. Text mode keeps stdout as the final assistant answer; failed, timed-out, or canceled runs print status, captured pane tail when present, and transcript path on stderr.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --detach   # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
cat build-error.txt | rimz agents claude -p 'explain the root cause' > out.txt
```

- `--detach` prints the pet name and returns immediately; use that name with `message --steer`, `agents wait`, `agents show`, or `agents stop`.
- `--output-format` shapes the print: `text` (default) prints the final assistant message, `json` prints the full run record, `stream-json` emits run events as NDJSON while the turn runs (incompatible with `--detach`).
- `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT` and folds in piped stdin after it; `stream-json` reads user messages from stdin until EOF and refuses a positional prompt.
- `--max-turns <N>` caps the agentic turn count where the adapter exposes a native limit (Claude today); an agent without one refuses the run.

Supervised runs need installed and trusted hooks, because hooks are the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [harness.md → Supervised runs](../../internals/agents/harness.md#supervised-runs).

### List, inspect, logs, top, focus, wait, and stop

```sh
rimz agents                              # room root-agent cards, current channel
rimz agents ps --all                     # every room channel; alias for list
rimz agents list --worktree auth-refresh # one room branch / worktree / dir
rimz agents inspect swift-otter          # describe-style card, cost, messages, transcript tail
rimz agents show swift-otter --capture   # report plus the pane's visible text
rimz agents logs swift-otter -n 20       # one agent's transcript tail
rimz agents logs swift-otter -f          # follow new transcript lines
rimz agents top --once                   # one resource-ranked fleet table
rimz agents focus @claude-2#cli-docs     # jump to the pane
rimz agents wait swift-otter --stream    # block until it lands, tailing the transcript
rimz agents stop run_0123…               # cancel a run or close a pane
rimz agents stop @claude --all           # stop every matching Claude in scope
```

Bare `rimz agents` lists the live room's pane-backed root-agent cards in attention order, scoped to the current channel and widened with `list --all`; run it inside a live room or enter one with `rimz start` or `rimz attach`. The `AGENT` column is the shortest handle you can type back — its role (`@coder`), else its profile (`@planner`), else `@<kind>`, growing an ordinal only when two of a kind share one worktree. `DESC` is the same activity description the sidebar shows: session preview, session name, launch description, task, then latest prompt, clipped to the terminal width. `ps` is an alias for `list`.

`show` and its `inspect` alias print a describe-style report with Agent, Activity, Context, Placement, Run, Messages, and Recent transcript sections. The Context section includes transcript-priced session cost when a transcript path and cached price book can price it. `--capture` appends the bound pane's visible area as plain text, `--ansi` keeps colors, and `--json` includes the same live agent fields plus additive `cost`, `messages`, and optional `capture` data. Capture errors when the agent has no bound pane. `--json` selects JSON for `list` and bare `agents` (supervised `-p` uses `--output-format` instead).

`logs <ref>` is the agent-centric transcript view: `-n/--tail N` keeps the last N chat lines, `-f/--follow` prints new lines as they land, `--all` includes prior-session history, and `--json` emits JSON for one-shot reads or NDJSON in follow mode. It uses the same transcript scope and rendering as `rimz transcript @ref`.

`top` ranks live root agents by pane process-tree resources: CPU, memory, I/O per second, process count, context fill, tokens, and age. It streams by default; `--once` takes two samples 500 ms apart and exits for scripts. Resource columns read `-` on platforms or panes where `/proc` metrics are unavailable, while context and token columns still render.

`focus` jumps to an agent's pane. `wait` blocks on a supervised run (by run id or pet name) or an interactive agent reaching an idle/success gate; `--stream` tails the transcript and `--from-start` replays from the top. `stop` tears down a run's pane — canceling supervision while the run is live, reclaiming a completed `--keep` pane — or closes the agent's pane when the ref names no run. Without `--all`, `stop` resolves to exactly one agent; with `--all`, it resolves every match, prints one result line per agent, and exits non-zero if any stop failed.

## Message an agent

`rimz message` is the teammate chat surface. The default parks text for the next safe turn boundary, sending immediately only when the agent is already open to receive; `--steer` interrupts the live pane now; `--schedule` sets the earliest delivery time before the usual `--on` gate opens.

```sh
rimz message @swift-otter "Add focused tests for the parser."                   # park or send now if open
rimz message --on any @codex#cli-docs "If the run failed, capture the error first."
rimz message --schedule 60m @claude "Run the smoke test after lunch."
rimz message --schedule 14:30 --on any @planner "Restart the review."
rimz message --steer @claude "Inspect the failing test now."
rimz message --steer @codex --no-enter "Use the docs branch only."              # paste, don't submit
rimz message --steer @planner --create "Draft the new endpoint."                # launch if missing
rimz message @all "When you reach a boundary, summarize what changed."
rimz message                                                                  # inbox for the current lane
rimz message list --json
rimz message list --channel cli-docs --status queued
rimz message show msg_01k…                                                    # status alias kept
rimz message remove msg_01k… msg_01k…
rimz message clear                                                            # clear open messages in the current lane
rimz message clear @claude-2#cli-docs
```

The message is one bare quoted argument, so no `--` separates ordinary prose from flags. A message that starts with `-` still uses clap's universal terminator (`--`) before the text. Value-optional flags such as bare `--wait` belong after the message, or use `--wait=<duration>`, so the flag does not capture the next token.

Address the target with the [agent-address grammar](#addressing-agents). `message --steer` delivers to live panes immediately, writes a durable prompt record, and prints `sent to @handle (msg_...)`; smart compaction adds a durable command record and `compacted @handle` before the prompt line. Broadcasts summarize sent and skipped agents with handles and message ids, so one blocked agent never stops the rest. A fan-out tags each delivery with the addressed handle, and an unmatched address prints the live-agent list. The `message.sent` audit event records message id, receiver, pane, force flag, sender, body, and text length; message content stays in the message record.

The default mode uses the same live path when the addressed agent can receive now: a live pane exists, the `--on` gate is open, no pending ask reserves input unless `--force`, and no older ready message owns that card's FIFO head. Otherwise it parks a `queued` prompt record until `--on done` (idle or success) or `--on any` (idle, success, or failed) opens. `--schedule <DUR|HH:MM>` always parks and sets a `not_before` time floor; examples include `90s`, `60m`, `2h`, `1d`, and configured-timezone 24-hour times such as `14:30`. A scheduled message becomes eligible only after that floor, then the normal gate and pending-ask checks still apply.

The flags worth knowing tune delivery (run `rimz message --help` for the full surface):

- `--steer` interrupts the live pane now and conflicts with `--schedule` and `--on`, because it has no later boundary.
- `--schedule <DUR|HH:MM>` sets the earliest delivery time for parked records; the room must be open so the sidebar elder can spawn `message sweep` when the wake stamp comes due.
- `--on done|any` chooses which turn-boundary statuses release parked records; `done` is the default.
- `--no-enter` pastes the text without submitting; otherwise the text rides as a bracketed paste and Enter lands as a discrete keystroke, so a `\n` in the text stays a soft composer newline and a multi-line prompt lands multi-line (write `\\` for a literal backslash).
- `--file <PATH>` reads the prompt from a file and sends it byte-for-byte — real newlines stay soft breaks and backslashes stay literal, so code and regex paste unchanged. It conflicts with inline text.
- `--channel <NAME>` scopes the target to a named channel; inline `#NAME` is the address form. `--worktree <NAME>` scopes to a worktree name or path.
- `--create` launches a missing agent from a kind or profile address with the text as its first prompt; inline `#NAME` or `--channel NAME` registers a named channel, while `--worktree NAME` creates or reuses Git backing.
- `--force` sends over a pending native ask; without it the ask keeps the next input reserved.
- `--smart-compact <PCT|TOKENS>` sends a tracked `/compact` command first when the agent's context window has reached the threshold (a percentage like `70%` or an occupied-token count like `120000`), then sends the prompt one message interval later so it lands against a fresh window. Unset, [`[harness] smart_compact`](../configuration.md#smart-compaction) supplies the threshold; a window below it sends untouched.
- `--no-from` sends the bytes exactly. By default a Rimz-launched agent's send arrives as `from @sender: text`, gaining `#channel` when it crosses channels.
- `--wait[=DURATION]` waits after send-now delivery until the agent's next `TurnStarted` hook confirms the prompt, the delivery window elapses, or the send errors. Bare `--wait` uses `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` or the default window. It conflicts with `--no-enter`, because an unsubmitted paste cannot be confirmed.

A bare `@<kind>`, `@<profile>`, or `@all` in `--steer` mode also reaches an agent you just started in a fresh pane, before its first turn, because the live-pane side addresses the pane it types into. Parked records key on the bound session or launch placeholder card so FIFO survives registration. Message statuses are `queued`, `claimed`, `sent`, `delivered`, `timed_out`, `errored`, `removed`, `abandoned`, and `archived`. `sent` means Rimz wrote the bytes to the pane; `delivered` means the agent acknowledged a prompt through `TurnStarted` or a command through `Compacting`; `archived` means the receiver or channel context ended. Bare `rimz message` renders the inbox. `message list` defaults to the current channel lane, hides archived records, sorts newest first, caps at 200 rows (`--limit N`, `--limit 0` for all), and renders `ID FROM TO STATUS CREATED DELIVERED MESSAGE`; `--all` widens the view to every channel and adds a `CHANNEL` column after `TO`, `--channel <NAME>` selects one lane, `--status <STATUS>` filters exactly, and `--json` keeps the full record including attempts. Handles omit `#channel` when the row already sits inside that scoped lane. Terminal rows read their preserved text from `messages/history.jsonl`; older event-only rows show the terminal reason in `MESSAGE`. `message show <id>` (`status` alias) prints the full record text, a `message.*` event timeline, and a delivery check for open records that names the first blocker: schedule floor, FIFO head, receiver presence, gate, pending ask, or live pane. `message remove` accepts one or more ids and keeps processing after misses, then exits non-zero if any id was not open. `message clear` with a target removes that agent's open messages; without a target it removes open messages in the scoped lane from `--channel`, `--worktree`, or the ambient room channel, and prints the ids it removed.

Parked delivery needs installed and trusted hooks, because turn-end hooks trigger the hidden `message deliver` helper. Scheduled wakeups use `message-wake.json` in the runtime cache and the hidden `message sweep` helper; the wake path needs an open room so an elder is keeping time. The record layout, gates, and delivery walk are in [message.md](../../internals/agents/message.md).

## Inspect transcripts

`rimz transcript` reads Rimz's durable transcript log and renders the channel as a timestamped chat log, including ended agents whose native transcript files have rotated away. The log model (entry kinds, JSONL buckets, retention) is [message.md → Transcript](../../internals/agents/message.md#transcript).

```sh
rimz transcript @swift-otter            # one agent's channel messages
rimz transcript @codex#cli-docs --last 4
rimz transcript #cli-docs               # the channel chat log
rimz transcript @all#cli-docs --last 12
rimz transcript --all                   # include the dated history archive
rimz transcript --json
```

A channel target (`#worktree`, `@all`, or no target for the current channel) projects every root agent's transcript-log entry in that exact lane into one timestamp-ordered chat log. The default view starts at the current live cohort for that scope, so a same-name team or worktree relaunch opens on its living conversation instead of replaying the prior one; older lines remain in the append-only log, `--all` shows them under a dated history archive, and an empty scope exits successfully with a short note. A single-agent transcript target is deliberately non-ambiguous: an exact session id wins across channels, otherwise a handle picks a live session in the current room when one exists, then the latest transcript activity. Headers put the sender first, add the receiver with `→` when one exists, and show `HH:MM`; consecutive messages from the same sender-to-receiver pair group under one header. Message bodies highlight `@agent` and `#channel` mentions, and provider API error entries render as error-styled agent lines. Blocking asks render as cards with a left spine, option lists, each option's description under its label, folded answers, and `◌ unanswered` when no answer exists in the log. Peer-opened turns include the receiver's assistant reply, and `--last <N>` keeps the last N chat lines.

A single-agent target builds that same channel log and filters it to messages the focal agent sent or received, so sent messages appear from peers' logs as well as received messages from the focal log. Sends made with `--no-from` look like human prompts, and cross-channel sends involving agents outside the focal channel are outside this view. `--json` emits `{channel, focus, entries}` for both channel and agent targets, with `archived_count` when prior-session lines are hidden.

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

`list` is the room seen as panes: every pane grouped under its native tab, each row labelled with the agent that lives in it (`@kind#worktree`) or `process` for a plain pane, with status and working directory. Rimz's own sidebar pane is omitted, and a `●` marks the active pane in each tab. On Zellij, listing a named session requires a known Rimz workspace record because the pane roster comes from Rimz's presence-plugin topology cache.

`split` opens a shell beside the current pane along its longer visual edge, matching the room's native new-pane behavior.

```text
#auth-refresh
 ●  @claude#auth-refresh   running   ~/code/qe-wt/auth-refresh   zellij:terminal_3
    @codex#auth-refresh    idle      ~/code/qe-wt/auth-refresh   zellij:terminal_4
    process                -         ~/code/qe-wt/auth-refresh   zellij:terminal_5
```

The agent labels are a best-effort overlay folded from the workspace snapshot, so a pane the multiplexer has handed back to a shell reads `process`; the tab grouping always works, even with no snapshot reachable. `--json` emits the tab tree with a per-pane `kind`, `command`, `cwd`, and `pid`, and an `agent` object for agent panes. `capture` prints visible pane text, `send` types literal text and named keys in order, and `focus` moves attention. Named keys are `enter`, `escape`, `tab`, `backspace`, the four arrows, `ctrl-c`, `ctrl-d`, and `ctrl-u`, with aliases like `return`, `esc`, and `bs`.

Pane capture is untrusted terminal text — scripts and resolvers match bounded patterns before sending anything back, and `pane send` is the same explicit input path as `message --steer`. Resolver patterns and pane-send discipline are in [resolver internals](../../internals/agents/resolvers.md).

## Schedule turns with loop

`rimz loop` schedules work from the room's sidebar elder while a room for the task's project is open. A task uses `--spec` to spawn one supervised transient pane, `--bind` to deliver a prompt to one live agent session through the message path, `--check` to run a scheduled command, or `--check` as a guard before an agent action.

```sh
rimz loop add morning --spec claude-ping --at 07:00 --days weekdays
rimz loop add weekly-prime --spec claude-ping --at-reset
rimz loop add pr-watch --spec codex --prompt "check CI on the release PR" --every 15m --mode auto --root .
rimz loop add self-wake --bind @planner --prompt "resume the review and fix the next blocking comment" --in 30m --root .
rimz loop add watchdog --check "cargo test" --on fail --spec codex --prompt "fix the failing test" --every 15m
rimz loop add ci-green --check "gh run watch --exit-status" --on success --until 30m --every 2m --bind @planner --prompt "CI is green; merge"
rimz loop fire pr-watch
rimz loop fire pr-watch --keep
rimz loop rename pr-watch ci-watch
rimz loop list
rimz loop show pr-watch
rimz loop remove pr-watch
```

Schedules come in six shapes: calendar (`--at` plus optional `--days`), interval (`--every 15m`), raw cron (`--cron`), window-reset (`--at-reset` on a `<kind>-ping` spec), one-shot (`--once` or `--in 30m`), and poll-until (`--every`, `--check`, `--on`, `--until`, plus an agent action). Calendar, cron, `--in`, and `--until` resolution use the top-level `timezone`, falling back to the system zone when unset. A `<kind>-ping` spec is the window-primer — `add` defaults its prompt to `ping`, and the run skips when the provider's window is already counting down. `--at-reset` fires that ping one minute after the provider's longest observed budget window resets, then uses the ping turn's own cache refresh as the next occurrence. `--bind @<handle>` resolves the address immediately and pins the exact session id; if that session is gone when the task fires, Rimz skips delivery and removes the schedule. `--check` runs at the project root; `--on fail` wakes on non-zero exit or timeout, while `--on success` wakes on zero exit. Rimz-generated `--in`, `--once`, and `--until` tasks persist as state, not `loop.toml` config. `loop fire` runs the task now in the foreground with the same check guard, window skip, overlap guard, and run-log record as a scheduled fire, streams the check's live output, prints the outcome, and keeps one-shot entries and bind schedules in place; `--keep` leaves the transient supervised pane open for inspection. A task that is already running records `overlapped` and skips instead of stacking another run. `loop rename` moves the task key in its store; the task then re-arms, so an interval task next fires one interval later. `loop list` groups tasks by project root with room state in the section header, then shows name, task, schedule, last-run age, status, and next fire. `loop show <name>` opens with one task's schedule, next fire, task, check, root, and source, then prints recent runs plus stored details such as check output, error chains, run ids, captured pane output tails, and transcript links. The task model and config shape are in [harness.md → Scheduled turns](../../internals/agents/harness.md#scheduled-turns-loop).

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

`new` creates a marked worktree under the configured [`[agents.worktree] dir`](../configuration.md#worktrees). `--base head` branches from `HEAD`, `--base fresh` from the configured fresh base, and any other value is a git ref. `--from-pr <number|url>` fetches the pull request head through `origin` and creates a `pr-<N>` branch unless `--branch` names it (GitHub/Gitea/Forgejo use `refs/pull/<N>/head`, GitLab `refs/merge-requests/<N>/head`). `list` shows Rimz-owned worktrees as the channels they are — name, display branch, the `@kind` handles working there, a dirty marker, the landed signal, and the path. `remove` refuses a dirty worktree or one whose content is not proven landed on its base; `--force` removes anyway.

Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary checkouts. Named channels are covered in [channel.md](./channel.md); the marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, and the `rimz gc` sweep are in [worktree.md](../../internals/agents/worktree.md).
