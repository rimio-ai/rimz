# Agent control CLI

`rimz agents` runs the fleet from one command: list the room's agents, launch laid-out panes and teams, drive supervised script turns, then focus, wait on, or stop what you started. This page also defines the [address grammar](#addressing-agents) that every agent-facing command shares. Run these from inside the room or anywhere that resolves to the same workspace.

A typical session threads several commands together:

```sh
rimz agents claude,codex --worktree=auth-refresh "Refactor token refresh; keep the public API stable."
rimz message --steer @claude#auth-refresh "Start with the refresh-token rotation path."
rimz message @codex#auth-refresh "After your turn, add coverage for the expiry edge cases."
rimz agents focus @claude#auth-refresh        # jump to the pane when it needs you
```

Each command around `rimz agents` has its own page: [`rimz message`](./message.md) talks to live agents, [`rimz transcript`](./transcript.md) reads the chat log, [`rimz pane`](./pane.md) reads and drives raw panes, [`rimz loop`](./loop.md) schedules turns, and [`rimz channel`](./channel.md) and [`rimz worktree`](./worktree.md) manage the lanes they work in.

The launch grammar, profiles, and teams these commands consume are configured per machine; see [configuration → agent profiles, commands, and teams](../configuration.md#agent-profiles-commands-and-teams). The launch, run, and delivery machinery lives in [harness.md](../../internals/harness/harness.md).

## Addressing agents

`message`, `transcript`, `pane capture`/`send`/`focus`, and the `agents show`/`logs`/`focus`/`wait`/`stop`/`refresh` verbs share one address grammar: `@<handle>` names who, an optional `#<channel>` names the stamped lane, and a raw pane id is the precise fallback. This is the one place it is spelled out; every agent-facing command assumes it.

**Handles that name one agent:**

- `@writer` — an explicit name from a single-agent launch such as `rimz agents claude --name writer`.
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

- The management verbs (`show`, `logs`, `focus`, `wait`, and `stop`) act on exactly one agent, so a handle that matches several is an error that lists the candidates to pick from. `stop --all` fans out to every match for the reference. `refresh` without a reference covers every live root agent in the current channel, and `refresh --all` widens to the workspace; with a reference it acts on exactly one agent.
- `message` fan-outs are explicit: a multi-match is ambiguous until you opt in with `--all` or address `@all`. A fan-out delivers to every match with no confirmation and prefixes each delivery with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message.

The `@` sigil is required for `message`, where it also keeps a target from being read as a launch spec. `show`, `logs`, `wait`, `stop`, and `refresh` also accept a bare selector (`swift-otter`), and `transcript`, `wait`, and `stop` also accept a run id. The deeper resolution rules are in [harness.md → The address](../../internals/harness/harness.md#the-address).

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
rimz agents claude --worktree "Take one approach."   # parallel attempts, each in its own fresh worktree
rimz agents claude --worktree "Take another approach."
```

The spec is a named [team](../configuration.md#agent-profiles-commands-and-teams), one declared role of a team as `<team>.<role>`, or an inline grammar: **commas split columns, plus signs tile rows, slashes stack rows** (a Zellij stack; tmux tiles them). Each cell is `term`, an agent kind, a virtual `<kind>-<mode>` cell, a configured profile, or a configured command. Use `rimz agents <team>.<role>` to re-add one role of a running or stopped team with the same role handle and stamped team lane. The built-in `peer` team is the roleless `claude,codex`. The full grammar and how cells compile to panes are in [harness.md → The layout IR](../../internals/harness/harness.md#the-layout-ir).

Permission-mode cells exist where the adapter supports them: `-auto`, `-ask`, `-plan`, and `-yolo` set the permission posture (`claude-plan` passes plan mode while `codex-plan` has none and keeps the default posture), and `-ping` opens the agent at lowest effort to keep the provider window warm. The built-in set is `claude-{auto,ask,plan,yolo,ping}`, `codex-{auto,ask,plan,yolo,ping}`, and `pi-{ask,plan}`. On the command line, `--ask` keeps native prompts and `--yolo` passes the adapter's bypass flags; with neither, each provider keeps its own prompting.

A second positional that is itself a known cell is rejected with a `rimz agents a,b` hint, so the old space-separated fan-out never silently becomes a prompt.

### Resume a cohort

`--resume` relaunches a prior cohort matching the same spec; `--continue` is the same visible alias.

```sh
rimz agents pcr --resume                             # reopen the newest closed pcr cohort
rimz agents pcr -w restore-living-team --resume      # reopen that exact team instance
rimz agents claude,codex --resume                    # reopen the newest matching inline cohort
rimz agents claude --resume                          # resume the freshest closed Claude session
```

What matches what:

- A team resumes by team name and role; an inline multi-agent spec resumes by the saved launch group and cell order; a single kind resumes the freshest closed root session of that kind.
- Add `-w <NAME>` to resume that exact worktree's cohort. Use bare `-w`, or omit the flag while running inside a worktree, to scope resume to that worktree; run from the project root to keep the room-wide newest-by-spec behavior.
- Cleanly closed cohort members still match when their worktree exists. Cells with no resumable prior member launch fresh in the matched cohort's cwd and channel. A matched member that is still live refuses the command, so the room does not duplicate the same address.

Resume takes identity, cwd, and channel from the store, so it conflicts with `PROMPT`, `--from-pr`, `--channel`, `--name`, `--description`, `--model`, `--effort`, `--ask`, `--yolo`, `-p`, system-prompt flags, and passthrough args after `--`.

### Shared launch params

These broadcast to every agent cell, and each adapter renders them into its own native flags.

- `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` carry the same meaning and resolution rules as the [profile fields](../configuration.md#profiles) of the same names; a command-line flag renders after any profile and wins. `--effort` levels are provider-specific: Claude `low|medium|high|xhigh|max`, Codex `minimal|low|medium|high|xhigh`, Pi `off|minimal|low|medium|high|xhigh`.
- `--description <TEXT>` is a card label only: it seeds the card's second line, never enters the agent's argv or environment, and the agent's own session preview replaces it.
- `--name <HANDLE>` applies to a single-agent launch and makes that user-chosen name the rendered handle after any team role, so `rimz agents claude --name writer` appears as `@writer` in lists, sidebar cards, and peer message prefixes. Bare launches still get an internal pet name for stable instance addressing, but they render as `@<kind>` when that is unambiguous.

### Channel, worktree, and placement

`-w`/`--worktree` reuses or creates a named worktree (`--worktree=docs` or `--worktree docs`); bare `--worktree` creates a fresh generated one. Branch-style spelling is accepted: `--worktree=feat/great` creates branch `feat/great` and worktree/channel/tab `feat-great`. `--from-pr <number|url>` creates the worktree from a pull request head and implies a worktree launch; pair it with `--worktree <NAME>` to name the local worktree, or accept `pr-<N>`. A worktree launch names its backend tab `#<NAME>`, matching the channel in agent addresses.

Relaunching a named team into the same named worktree reconciles with existing state before it creates anything: a live team focuses its current tab, a closed tab with work in progress offers to resume that team in the worktree, and a closed clean merged worktree offers to remove it and launch fresh. Add `--resume` or `--continue` to force a resume of the named worktree's prior cohort even when the worktree is clean or merged.

`--channel <NAME>` launches into a durable named channel, registering it when missing and naming the backend tab `#<NAME>`. Named channels run in the room root and are managed with [`rimz channel`](./channel.md).

Placement follows intent under the default `auto` policy: a named-channel launch, a worktree launch, or a multi-cell spec opens its own tab, and a one-cell non-worktree launch, including a single team role, takes over the current pane and returns to the shell when it exits. `--new-pane` forces a split (rejected for a multi-cell spec), `--new-tab` forces a tab, and `--bg` downgrades an in-place launch to a split so focus stays put — that is `--bg`'s placement meaning at launch; combined with `-p` it instead detaches from a supervised run, covered under [Supervised runs](#supervised-runs--p). The per-machine [`[agents] placement`](../configuration.md#agent-profiles-commands-and-teams) default sets the policy when no flag is given. The split-versus-tab mechanics are in [harness.md → Backend shape and placement](../../internals/harness/harness.md#backend-shape-and-placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the result, and exits with the run's status code (`0` completed, `1` failed, `124` timed out, `130` canceled), so a script branches on the outcome. Text mode keeps stdout as the final assistant answer; failed, timed-out, or canceled runs print status, captured pane tail when present, and transcript path on stderr.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --bg       # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
cat build-error.txt | rimz agents claude -p 'explain the root cause' > out.txt
```

- `--bg` with `-p` prints the run's pet name and returns immediately; use that name with `message --steer`, `agents wait`, `agents show`, or `agents stop`. (Without `-p`, `--bg` is a placement flag — see [Channel, worktree, and placement](#channel-worktree-and-placement).)
- `--output-format` shapes the print: `text` (default) prints the final assistant message, `json` prints the full run record, `stream-json` emits run events as NDJSON while the turn runs (incompatible with `--bg`). The JSON `run_id` opens the Rimz transcript log with `rimz transcript <run_id>`; the JSON `transcript_path` is the provider-native session file used for streaming, context, and spend enrichment.
- `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT` and folds in piped stdin after it, wrapped in `<stdin>…</stdin>` tags when both are present; `stream-json` reads user messages from stdin until EOF and refuses a positional prompt.
- `--max-turns <N>` caps the agentic turn count where the adapter exposes a native limit (Claude today); an agent without one refuses the run.
- Ctrl+C on a blocking `-p` cancels the run, exits `130`, and lets the wrapper stop the agent before the pane is reclaimed.

Supervised runs need installed and trusted hooks, because hooks are the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [harness.md → Supervised runs](../../internals/harness/harness.md#supervised-runs).

### List and manage agents

```sh
rimz agents                              # room root-agent cards, current channel
rimz agents '#auth-refresh'              # one lane's cards
rimz agents ps --all                     # every room channel; alias for list
rimz agents list '#auth-refresh'         # same lane filter through the list verb
rimz agents list --worktree auth-refresh # one room branch / worktree / dir
rimz agents inspect swift-otter          # describe-style card, cost, messages, transcript tail
rimz agents show swift-otter --capture   # report plus the pane's visible text
rimz agents logs swift-otter -n 20       # one agent's transcript tail
rimz agents logs swift-otter -f          # follow new transcript lines
rimz agents top --once                   # one resource-ranked fleet table
rimz agents focus @claude-2#cli-docs     # jump to the pane
rimz agents wait swift-otter --stream    # block until it lands, tailing the transcript
rimz agents refresh                      # force-refresh the channel's live agent cards
rimz agents refresh @codex               # force-refresh one agent card's local context
rimz agents refresh --all                # force-refresh every live root agent card
rimz agents stop run_0123…               # cancel a run or close a pane
rimz agents stop @claude --all           # stop every matching Claude in scope
```

| Verb | Acts on | What it does |
|---|---|---|
| `list` (bare `agents`; `ps` alias) | the current channel; `--all` for the room | attention-ordered agent cards |
| `show` (`inspect` alias) | one agent | describe-style report: activity, context, cost, messages, transcript tail |
| `logs` | one agent | transcript tail; `-f` follows |
| `top` | live root agents | resource-ranked fleet table |
| `focus` | one agent | jumps to its pane |
| `wait` | one run or agent | blocks until it lands |
| `refresh` | one agent, the channel, or `--all` | force-refreshes card context |
| `stop` | one run or agent; `--all` fans out | cancels a run or closes the pane |

#### `list`

Bare `rimz agents` lists the live room's pane-backed root-agent cards in attention order, scoped to the current channel and widened with `list --all`; run it inside a live room or enter one with `rimz start` or `rimz attach`. `ps` is an alias for `list`, and `--json` selects JSON output for both.

Rows group under channel section headers: `⑂` marks a worktree-backed or isolated lane, `#` marks a plain lane, a bare label marks the room root, and a dim `external` tail holds agents outside the project. Header glyphs follow the configured theme glyph set, including Nerd Font presets, and a shared team appears in the header as `· <team> team`.

| Column | What it shows |
|---|---|
| `AGENT` | the shortest handle you can type back under that header — its role (`@coder`), else its explicit `--name` (`@writer`), else its profile (`@planner`), else `@<kind>`, growing an ordinal only when two of a kind share one lane |
| `STATUS` | the plain status label, with provider-limit and API-error turns projected to `paused` or `failed`; `show` carries the turn phase when you need it |
| `DESC` | the same activity description the sidebar shows — session preview, session name, launch description, task, then latest prompt — clipped to the terminal width |

#### `show` / `inspect`

`show` and its `inspect` alias print a describe-style report with Agent, Activity, Context, Placement, Run, Messages, and Recent transcript sections. The Context section includes transcript-priced session cost when a transcript path and cached price book can price it. `--capture` appends the bound pane's visible area as plain text (an error when the agent has no bound pane), `--ansi` keeps colors, and `--json` includes the same live agent fields plus additive `cost`, `messages`, and optional `capture` data. (Supervised `-p` runs shape their output with `--output-format` instead.)

#### `logs`

`logs <ref>` is the agent-centric transcript view: `-n/--tail N` keeps the last N chat lines, `-f/--follow` prints new lines as they land, `--all` includes prior-session history, and `--json` emits JSON for one-shot reads or NDJSON in follow mode. It uses the same transcript scope and rendering as `rimz transcript @ref`.

#### `top`

`top` ranks live root agents by pane process-tree resources: CPU, memory, I/O per second, process count, context fill, tokens, and age. It streams by default; `--once` takes two samples 500 ms apart and exits for scripts. Resource columns read `-` on platforms or panes where process metrics are unavailable, while context and token columns still render.

#### `focus`

`focus` jumps to an agent's pane.

#### `wait`

`wait` blocks on a supervised run (by run id or pet name) or an interactive agent reaching an idle/success gate. A plain run wait prints the final assistant message at completion; `--stream` tails assistant text as it lands, `--stream --json` emits NDJSON run events, and `--from-start` replays from the top before tailing.

#### `refresh`

`refresh` forces the transcript tail re-read past the stat gate, re-runs Codex turn-death confirmation against the live pane when one is bound, spawns the kind's detached rich-context helper when one exists, and wakes sidebars after an inline merge. With a reference it resolves exactly one agent in scope; without a reference it refreshes every live root agent in the current channel; with `--all` it takes no reference and covers every live root agent in the workspace.

#### `stop`

`stop` tears down a run's pane — canceling supervision while the run is live, reclaiming a completed `--keep` pane — or closes the agent's pane when the ref names no run. Without `--all`, `stop` resolves to exactly one agent; with `--all`, it resolves every match, prints one result line per agent, and exits non-zero if any stop failed.
