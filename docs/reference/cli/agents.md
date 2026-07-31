# Agent control CLI

`rimz agents` is the single launcher and card surface for the fleet: list the room's agents, launch laid-out panes and teams, drive supervised script turns, then focus, wait on, or stop what you started. What it does on your machine is thin — it renders a profile into the stock CLI's own flags (`claude --model … --allowed-tools …`, nothing you couldn't type) and runs that command in your Zellij or tmux, in the pane you stand in for one agent or a fresh tab for a layout or worktree. The agent process is the official CLI; its session files land where that CLI always puts them, so `claude --resume` and the provider's own apps keep working. `agents stop` ends a pane the way Ctrl+C would, and `--resume` reopens a closed cohort. Why you reach for a profile or a team instead of a bare CLI is the [agents guide](../../guide/fleet.md).

This page also defines the [address grammar](#addressing-agents) that every agent-facing command shares. Run these from inside the room or anywhere that resolves to the same workspace.

A typical session threads several commands together:

```sh
rimz agents claude,codex --worktree=auth-refresh "Refactor token refresh; keep the public API stable."
rimz message --steer @claude#auth-refresh "Start with the refresh-token rotation path."
rimz message @codex#auth-refresh "After your turn, add coverage for the expiry edge cases."
rimz agents focus @claude#auth-refresh        # jump to the pane when it needs you
```

Each command around `rimz agents` has its own page: [`rimz message`](./message.md) talks to live agents, [`rimz transcript`](./transcript.md) reads the chat log, [`rimz pane`](./pane.md) reads and drives raw panes, [`rimz loop`](./loop.md) schedules turns and exposes the live `rimz loop watch` dashboard, and [`rimz channel`](./channel.md) and [`rimz worktree`](./worktree.md) manage the lanes they work in. The profiles, teams, and launch grammar these commands consume are configured per machine in the [configuration guide](../../guide/configuration.md#agent-profiles-commands-and-teams); the launch, run, and delivery machinery lives in [fleet.md](../../internals/harness/fleet.md).

## Addressing agents

`message`, `transcript`, `pane capture`/`send`/`focus`, and the `agents show`/`logs`/`history`/`focus`/`fork`/`wait`/`stop`/`restart`/`refresh` verbs share one address grammar: `@<handle>` names who, an optional `#<channel>` names the stamped lane, and a raw pane id is the precise fallback. This is the one place it is spelled out; every agent-facing command assumes it.

The [`pane` commands](./pane.md) additionally accept the literal `sidebar` for the session's sidebar pane.

**Handles that name one agent:**

- `@writer` — an explicit name from a single-agent launch such as `rimz agents claude --name writer`.
- `@swift-otter` — a pet name.
- `@claude-2` — a kind plus ordinal (the ordinal appears only when two of a kind share one worktree).
- `@<session-prefix>` — a leading slice of the session id.

**Handles that name a type and fan out:**

- `@claude` — an agent kind; every Claude in the channel.
- `@planner` — a [profile](../../guide/configuration.md#agent-profiles-commands-and-teams) you defined; every agent launched under it.
- `@all` — everyone in the channel at resolution time; `message` excludes its RimZ-launched caller before dispatch.

**Channels** scope the lookup to a named lane, worktree, or in-place team lane stamped at launch:

- `#design` matches a named channel created by [`rimz channel`](./channel.md); `--channel design` is the flag spelling.
- `#auth-refresh` matches by branch, generated worktree name, or directory basename; `--worktree auth-refresh` is the worktree flag spelling.
- `#query-engine/forge` matches the named team `forge` launched in-place from the `query-engine` directory.
- The default channel is the named-channel tab or worktree you run the command in.
- A team member pane launched in-place carries `RIMZ_CHANNEL=<dir>/<team>`, so its own `rimz` commands default to that stamped lane.
- A pane id (`tmux:%12`, `zellij:terminal_3`) addresses one pane directly and ignores channels.

**One agent or many:**

- The management verbs (`show`, `logs`, `history`, `focus`, `fork`, `stop`, and `restart`) act on exactly one agent, so a handle that matches several is an error that lists the candidates to pick from. `wait` accepts one or more independently resolved references. `stop --all` fans out to every match for the reference. `refresh` without a reference covers every live root agent in the current channel, and `refresh --all` widens to the workspace; with a reference it acts on exactly one agent.
- `message` fan-outs are explicit: a multi-match is ambiguous until you opt in with `--all` or address `@all`. A human-authored `@all` delivers to every match; an agent-authored `@all` excludes that caller and errors when no peers remain. Explicit selector fan-outs such as `--all @claude` keep every match. Each delivery is prefixed with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message.

The `@` sigil is required for `message`, where it also keeps a target from being read as a launch spec. `show`, `logs`, `history`, `fork`, `wait`, `stop`, `restart`, and `refresh` also accept a bare selector (`swift-otter`), and `transcript`, `wait`, and `stop` also accept a run id. The deeper resolution rules are in [fleet.md → The address](../../internals/harness/fleet.md#the-address).

## Agents

`rimz agents` is the card surface and the single launcher. The subsections below cover the forms worth knowing; run `rimz agents --help` (and `--help` on each subcommand) for the full flag list.

Agent launches validate every discovered `~/.agents` fragment before resolving the requested spec. A syntax error or invalid fragment fails at entry with its source path and fix; unknown fields instead print a warning, are ignored, and can be removed with `rimz setup`.

### Discover agent specs

```sh
rimz agents specs
rimz agents types --json
```

`specs` lists `[agents.profiles]` profiles and configured launch commands; `types` is an alias. Profile rows include their optional descriptions, and the final path column identifies the file that defines each row. Built-in and registered agent kinds remain directly launchable but are omitted from this configured-spec catalog. Teams are excluded because the catalog describes reusable cell types rather than cohort layouts.

### Register a third-party kind

```sh
rimz agents register mybot           # scaffold $XDG_CONFIG_HOME/rimz/agents.d/mybot
rimz agents register --check         # validate every machine-tier plugin
rimz agents check mybot --replay events.jsonl # validate one plugin and replay canonical envelopes
```

The scaffold contains the manifest, setup guide, canonical forwarding shim, and stub probes. The [agent plugin reference](../agent-plugins.md) defines the bundle and wire contracts. A valid plugin kind works anywhere a built-in kind does, including inline layouts, profiles, teams, supervised runs, coverage, and messaging.

### Launch a layout

A `<SPEC>` is a shape, and the optional `PROMPT` goes to exactly one leader: a named team's configured `leader` role, its first declared role by default, or otherwise the first agent cell. A repeated first cell must have an inline role to make the target unambiguous; use `rimz message @all` after launch for a broadcast.

```sh
rimz agents peer                                    # built-in claude,codex side by side
rimz agents launch peer                             # explicit launch verb, same payload
rimz agents claude,codex+term                       # Claude | Codex tiled over a shell
rimz agents claude/codex/term                       # one Zellij stack; tmux tiles rows
rimz agents claude,codex --channel=design "Draft the API shape."  # prompt Claude, the first cell
rimz agents claude,codex --worktree=cli-docs "Review the CLI docs." # prompt Claude, the first cell
rimz agents codex --from-pr 42 "Review this pull request."
rimz agents 'vim,codex+term' "Review the CLI docs."  # a raw command cell beside an agent
rimz agents forge.planner                            # re-add one role of team forge
rimz agents planner                                  # same, from a pane in the forge team's channel
rimz agents claude --worktree "Take one approach."   # parallel attempts, each in its own fresh worktree
rimz agents claude --worktree "Take another approach."
```

The bare spec and `launch` verb are equivalent: use whichever reads better in a command chain.

When a RimZ-launched agent runs this command, each new agent is an independent top-level peer with its own sidebar row. `[agents] max-chain-length` bounds successive agent-to-agent launches (three by default); an over-limit command refuses before it creates launch state. Agents that want a parented, one-prompt child with the safe flags implied use the agent-only [`rimz subagents`](./subagents.md) doorway instead. Only that doorway creates a subagent relationship, and a subagent cannot launch agents or subagents.

The spec is a named [team](../../guide/configuration.md#agent-profiles-commands-and-teams), one declared role of a team as `<team>.<role>`, or an inline grammar: **commas split columns, plus signs tile rows, slashes stack rows** (a Zellij stack; tmux tiles them). Each cell is `term`, an agent kind, a virtual `<kind>-<mode>` cell, a configured profile, or a configured command; an agent cell may use `<cell>:<role>` for an ad-hoc role handle. Use `rimz agents <team>.<role>` to re-add one role of a running or stopped team with the same role handle and stamped team lane. Inside that team's channel the bare role is enough — `rimz agents planner` in `#forge` means `rimz agents forge.planner`, and the role joins the lane it resolved from. RimZ reads the lane's team from the stamps its agents carry, so the shorthand works in a worktree lane and an in-place `<dir>/<team>` lane alike. A bare role that also names a profile or command resolving to a different agent is ambiguous and refuses; launch `<team>.<role>` or rename one of them. The built-in `peer` team is the roleless `claude,codex`. The full grammar and how cells compile to panes are in [fleet.md → The layout IR](../../internals/harness/fleet.md#the-layout-ir).

`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend.
`rimz agents` sets what an agent is — model, effort, prompts, permission posture, name, pane placement, supervised runs.
The configured-team doorway and team lifecycle verbs are in [`rimz teams`](./teams.md).

Permission-mode cells set the launch posture: `-ask`, `-plan`, `-auto`, and `-yolo`. Every registered kind has `-ask` and `-plan`. `-auto` and `-yolo` exist wherever the adapter declares argv for them, which is every kind except `droid` (no `-yolo`), `opencode` (no `-auto`), and `amp`, `kiro`, and `pi`, which carry `-ask` and `-plan` alone. A posture the agent expresses through no launch flag still resolves as a cell and simply adds no argv, so `codex-plan` and `grok-plan` keep the default posture while `claude-plan` and `antigravity-plan` pass native plan mode. Grok Ask maps to `--permission-mode default`, Auto to `--permission-mode auto`, and Yolo to `--yolo`. On the command line, `--ask` keeps native prompts and `--yolo` passes the adapter's bypass flags; with neither, each provider keeps its own prompting.

A second positional that is itself a known cell is rejected with a `rimz agents a,b` hint, so the old space-separated fan-out never silently becomes a prompt.

### Resume a cohort

`--resume` relaunches a prior cohort matching the same spec; `--continue` is the same visible alias. It reads identity, cwd, and channel from the store, so a closed cohort comes back where it was. Use the [place-first `resume` verb](#resume-a-lane-by-place) when the lane is known and the original spec is not.

```sh
rimz agents forge --resume                           # reopen the newest closed forge cohort
rimz agents forge -w restore-living-team --resume    # reopen that exact team instance
rimz agents claude,codex --resume                    # reopen the newest matching inline cohort
rimz agents claude --resume                          # resume the freshest closed Claude session
```

What matches what:

- A team resumes by team name and role; an inline multi-agent spec resumes by the saved launch group and cell order; a single kind resumes the freshest closed root session of that kind.
- Add `-w <NAME>` to resume that exact worktree's cohort. Use bare `-w`, or omit the flag while running inside a worktree, to scope resume to that worktree; run from the project root to keep the room-wide newest-by-spec behavior.
- Cleanly closed cohort members still match when their worktree exists. Cells with no resumable prior member launch fresh in the matched cohort's cwd and channel. A matched member that is still live refuses the command, so the room does not duplicate the same address.

A single-cell resume run from the cohort's own directory takes over the launching pane, so an exited team member comes back in its origin pane — the exit hint an agent leaves behind (`resume with rimz agents forge.coder --resume`) works from the very shell it dropped into. Run from anywhere else, a lane-scoped resume opens its own tab. A spec that matches nothing fails naming the specs the same scope can still resume.

Because resume takes identity from the store, it conflicts with `PROMPT`, `--from-pr`, `--channel`, `--name`, `--description`, `--model`, `--effort`, `--ask`, `--yolo`, `-p`, system-prompt flags, and passthrough args after `--`.

### Resume a lane by place

`resume [SCOPE]` makes one lane whole from its durable agent records without retyping the team or layout. Scope accepts the same `#channel`, worktree name, branch, directory name, and path spellings as `agents list`; `--from-pr <number|url>` resolves a RimZ worktree's recorded PR provenance first and the legacy `pr-<N>` name second. Resolution is local and performs no network request or worktree creation.

```sh
rimz agents resume '#docs'        # resume the docs lane
rimz agents resume pr-69          # resume by worktree name
rimz agents resume -w pr-69       # flag spelling of the same worktree scope
rimz agents resume --from-pr 69   # resume the local worktree created from PR 69
rimz agents resume                # inside a worktree: that lane; at project root: list lanes
```

| Lane state | Result |
|---|---|
| every member live | focuses the freshest member's pane and exits successfully |
| some members live | splits only the closed members back into the live tab and reports each skipped live handle |
| every member closed | rebuilds team layouts in declared order and restores stray agents as flat panes |

Soft reset preserves the lane's durable session identity, including exact provider ids, roles, teams, and placement, so a reset resumes with the same handles. When those RimZ records are genuinely gone, Claude and Codex fall back to their provider-owned local session stores and restore the newest concurrent working set with exact session ids. Provider-only recovery is flat because role and team identity exists only in RimZ; the resumed hooks record the recovered session again on first activity. Older disjoint runs stay closed and are reported by kind and session id.

At the project root, the bare listing includes worktree lanes found only in the Claude or Codex session store as closed lanes. Each provider store is scanned once per local worktree.

`--bg` leaves focus where it is when panes or tabs open. Profiles and team layouts render from the current `agents.toml`, while session identity, role, team, channel, and working directory come from the durable records. This is place-first recovery; [spec-first `--resume`](#resume-a-cohort) remains the form for choosing a prior cohort by team or layout.

Failures name the fix: an unknown scope reports `no lane '#docs' in this workspace`; a removed checkout reports `worktree for '#docs' was removed; recreate it with rimz agents <spec> -w docs`; a PR with no local worktree reports `PR 69 has no local worktree; start one with rimz agents <spec> --from-pr 69`; and `nothing to resume in '#docs'` means neither the RimZ store nor the supported provider stores contain a resumable session for that lane.

### Shared launch params

These broadcast to every agent cell, and each adapter renders them into its own native flags.

- `--agent <PROFILE|KIND>` re-bases every agent cell onto that profile or registered provider while retaining the cell's profile and team identity. Mode, effort, budget, and prompt files carry from the original profile; the replacement base fills their gaps. Model and raw `args` are provider-specific: they carry on a same-provider re-base, but a provider change silently takes them from the replacement base instead. A profile such as `[agents.profiles.codex]` can therefore hold the Codex model and raw flags used whenever `--agent codex` swaps a launch to Codex. Later command-line flags still win, and adapter-incompatible typed fields fail before RimZ creates a pane. This is a fresh-launch override: it conflicts with `--resume`, is not recorded as a profile edit, and a later `restart` refuses when the profile resolves back to a different provider.
- `--model`, `--effort`, `--budget <AMOUNT[/day]>`, and `--system-prompt-file` carry the same meaning and resolution rules as the [profile fields](../../guide/configuration.md#profiles) of the same names. Repeat `--append-system-prompt-file <PATH>` to replace the inherited fragment list in command-line order. A bare budget caps the session; `/day` resets at the configured local day boundary. `--effort` levels are provider-specific: Claude `low|medium|high|xhigh|max`, Codex `minimal|low|medium|high|xhigh`, Pi `off|minimal|low|medium|high|xhigh`.
- `--description <TEXT>` is a card label only: it seeds the card's second line, never enters the agent's argv or environment, and the agent's own session preview replaces it.
- `--name <HANDLE>` applies to a single-agent launch and makes that user-chosen name the rendered handle after any team role, so `rimz agents claude --name writer` appears as `@writer` in lists, sidebar cards, and peer message prefixes. Bare launches still get an internal pet name for stable instance addressing, but they render as `@<kind>` when that is unambiguous.

### Channel, worktree, and placement

`-w`/`--worktree` reuses or creates a named worktree (`--worktree=docs` or `--worktree docs`); bare `--worktree` creates a fresh generated one. Branch-style spelling is accepted: `--worktree=feat/great` creates branch `feat/great` and worktree/channel/tab `feat-great`. `--from-pr <number|url>` creates the worktree from a pull request head and implies a worktree launch; pair it with `--worktree <NAME>` to name the worktree, or accept the `pr-<N>` worktree name. A PR URL must match `origin`; `gh` or `tea` configures the source branch's push destination, while an unsupported forge creates a review-only checkout with pushes unconfigured. A worktree launch names its backend tab `#<NAME>`, matching the channel in agent addresses. Within the room's repository, worktrees RimZ creates are marked and cleaned up with [`rimz worktree remove`](./worktree.md) or the `rimz gc` sweep.

Creation uses the current directory's main Git repository, including when the command runs from one of its linked worktrees. If that repository differs from the room root, RimZ shows both paths and asks before proceeding. The room's `worktree list`, `worktree remove`, and `gc` commands do not cross that repository boundary: run removal from the worktree's own repository, or use plain `git worktree remove`. A non-interactive launch refuses the ambiguity; pass `--root <current-git-root>` to name the intended repository explicitly, or run the command from the room's checkout. When the current directory is not in a Git repository, the room root remains the creation root.

Relaunching a named team into the same named worktree reconciles with existing state before it creates anything: a live team focuses its current tab, a closed tab with work in progress offers to resume that team in the worktree, and a closed clean merged worktree offers to remove it and launch fresh. Add `--resume` or `--continue` to force a resume of the named worktree's prior cohort even when the worktree is clean or merged.

`--channel <NAME>` launches into a durable named channel, registering it when missing and naming the backend tab `#<NAME>`. Named channels run in the room root and are managed with [`rimz channel`](./channel.md).

Placement follows intent under the default `auto` policy: a named-channel launch, a worktree launch, or a multi-cell spec opens its own tab, and a one-cell non-worktree launch, including a single team role, takes over the current pane and returns to the shell when it exits. `--new-pane` forces a split (rejected for a multi-cell spec), `--new-tab` forces a tab, and `--bg` downgrades an in-place launch to a split so focus stays put — that is `--bg`'s placement meaning at launch; combined with `-p` it instead detaches from a supervised run, covered under [Supervised runs](#supervised-runs--p). The per-machine [`[agents] placement`](../../guide/configuration.md#agent-profiles-commands-and-teams) default sets the policy when no flag is given. The split-versus-tab mechanics are in [fleet.md → Placement](../../internals/harness/fleet.md#placement).

### Supervised runs (`-p`)

`-p` launches exactly one supervised agent pane, waits for the root turn, prints the result, and exits with the run's status code (`0` completed, `1` failed, `123` verify failed, `124` timed out, `125` budget exceeded, `130` canceled), so a script branches on the outcome. A fresh Qwen supervised launch also exits `125` before opening a pane when a matching exact-account Alibaba window is exhausted; missing or mismatched readings leave an ordinary launch available. The turn still runs in a real pane you can watch and steer while the pipeline waits. Text mode keeps stdout as the final assistant answer; failed, verify-failed, timed-out, budget-exceeded, or canceled runs print status, captured evidence when present, and transcript path on stderr.

### Inspect and change a budget

Why you cap spend, and what a park means, is the [budgets guide](../../guide/budget.md); this is the command surface.

`rimz agents budget @coder` prints current spend, cap, window, and park state. Set a new cap with `rimz agents budget @coder 10`, add headroom with `+5`, or remove the cap with `clear`. Raising or clearing a parked cap queues the configured continue prompt by default; pass `--no-continue` to leave the agent at rest.

`rimz budget` owns the two broader daily scopes. With no value it prints this room's fleet cap, source, local-day spend, and park state plus every configured provider-account cap. Config is the on-switch: `harness.budget` arms the room cap, and `[accounts.budget].<kind>` arms an account cap only when that adapter exposes durable account-spend history. Unknown or ineligible kinds are rejected by config validation, room start, and `rimz budget --account` before a ledger is written; Cursor's live local price remains available to per-agent and room caps but not an account-day cap. `rimz budget 20/day`, `+10`, or `off` adjusts, raises, or disables the armed room cap; `clear` aliases `off`, and `--account <kind>` applies the same operation to an eligible login across rooms. Daily caps require `/day`, while relative raises stay bare (`+10`). A change nudges affected parked agents in the current room unless `--no-continue` is set.

Room and account caps gate automation before it launches: `agents -p` exits `125`, and loop fires record `budget skipped`. Matching exact-account Alibaba quota applies the same outcome to fresh managed Qwen launches without turning provider quota into a configurable dollar cap. Interactive launches remain available, and one human message after a park waives that agent's next turn.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --bg       # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause' > out.txt
```

- `--bg` with `-p` prints the run's pet name and returns immediately; use that name with `message --steer`, `agents wait`, `agents show`, or `agents stop`. (Without `-p`, `--bg` is a placement flag — see [Channel, worktree, and placement](#channel-worktree-and-placement).)
- `--output-format` shapes the print: `text` (default) prints the final assistant message, `json` prints the full run record, `stream-json` emits run events as NDJSON while the turn runs (incompatible with `--bg`). The JSON `run_id` opens the RimZ transcript log with `rimz transcript <run_id>`; the JSON `transcript_path` is the provider-native session file used for streaming, context, and spend enrichment.
- `--stdin` adds stdin to the text prompt and reads it to EOF, wrapping it in `<stdin>…</stdin>` tags after a positional `PROMPT` when both are present.
- `--input-format` selects the prompt source: `text` (default) uses the positional `PROMPT` plus explicit `--stdin` content; `stream-json` reads user messages from stdin until EOF and refuses a positional prompt or `--stdin`.
- `--max-turns <N>` caps the agentic turn count where the adapter exposes a native limit (Claude today); an agent without one refuses the run.
- `--retries <N>` reruns only failed (exit `1`) turns, up to `N` more attempts, with the previous failure tail appended to the original prompt. `--timeout` and `--budget` apply per attempt; timeout, budget, and cancel results never retry; the final attempt decides the exit code. Retries require a blocking text or JSON run and refuse `--bg` and `--output-format stream-json`.
- `--verify <CMD>` runs the command in the run cwd after every completed turn and re-prompts the same session with failure evidence until it passes. `--max-attempts <N>` is the total agent-turn cap, defaults to `3`, and must be at least `1`; exhaustion exits `123`. The verify command uses `--timeout` or a five-minute default, a timed-out verify is red, and both flags refuse `--bg` and `--output-format stream-json`.
- Ctrl+C on a blocking `-p` cancels the run, exits `130`, and lets the wrapper stop the agent before the pane is reclaimed.

Supervised runs need installed and trusted hooks, because hooks are the completion signal. The run records, wakeup socket, streaming, and pane cleanup are in [scripting.md](../../internals/harness/scripting.md).

### List and manage agents

```sh
rimz agents                              # room root-agent cards, current channel
rimz agents '#auth-refresh'              # one lane's cards
rimz agents ps --all                     # every room channel; alias for list
rimz agents list '#auth-refresh'         # same lane filter through the list verb
rimz agents list -w auth-refresh         # one room branch / worktree / dir
rimz agents inspect swift-otter          # describe-style card, cost, messages, transcript tail
rimz agents show swift-otter --capture   # report plus the pane's visible text
rimz agents logs swift-otter -n 20       # one agent's transcript tail
rimz agents logs swift-otter -f          # follow new transcript lines
rimz agents history swift-otter -n 10    # per-turn tokens, cost, and outcome
rimz agents attribution --md             # durable lane credit for a pull request
rimz agents top --once -w auth-refresh   # one lane's resource-ranked fleet table
rimz agents focus @claude-2#cli-docs     # jump to the pane
rimz agents fork @coder --name twin      # branch a conversation into a new agent
rimz agents restart @claude-2#cli-docs   # replace its pane and resume it
rimz agents resume '#cli-docs'           # fill every closed place in one lane
rimz agents wait swift-otter --stream    # block until it lands, tailing the transcript
rimz agents wait otter fox --any         # race agents; print the first finisher
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
| `history` | one live or stopped agent | per-turn duration, tokens, cost, and outcome |
| `attribution` | the current lane; `--all` for the room | durable agent, model, time, token, and cost credit |
| `top` | live root agents | resource-ranked fleet table |
| `focus` | one agent | jumps to its pane |
| `fork` | one live or stopped root agent | branches its full conversation into a new agent |
| `wait` | one or more runs or agents | blocks until all land; `--any` returns on the first |
| `refresh` | one agent, the channel, or `--all` | force-refreshes card context |
| `stop` | one run or agent; `--all` fans out | cancels a run or closes the pane |
| `restart` | one live agent | replaces its pane and resumes its provider session |
| `resume` | one lane | focuses a whole live lane or restores its closed members |

`list`, `show`, `logs`, `history`, `attribution`, `top`, `focus`, `wait`, and `refresh` read state and change no agent. `fork` starts a new agent without changing its source, `stop` ends an agent, `restart` deliberately ends and replaces one, and `resume` restores the closed portion of a lane.

#### `list`

Bare `rimz agents` lists the live room's pane-backed root-agent cards in attention order, scoped to the current channel and widened with `list --all`; run it inside a live room or enter one with `rimz start` or `rimz attach`. `ps` is an alias for `list`, `-w/--worktree` selects one lane, and `--json` selects JSON output for both.

Rows group under channel section headers: `⑂` marks a worktree-backed or isolated lane, `#` marks a plain lane, a bare label marks the room root, and a dim `external` tail holds agents outside the project. Header glyphs follow the configured theme glyph set, including Nerd Font presets, and a shared team appears in the header as `· <team> team`.

`--json` emits the versioned projection `{"schema":1,"agents":[...]}`. Each entry is the same card the table renders rather than the provider-shaped durable rollup: `id`, `kind`, `handle`, `name`, `name_explicit`, `profile`, `role`, `team`, `mode`, and `me` identify it; `status`, `phase`, `turn_error`, `ask`, `unread`, `attention_score`, and `description` describe its projected activity; and `model`, `context`, `stats`, `timeline`, `placement`, `budget`, and `sub_agents` carry the normalized detail.

Nested fields are stable too. `model` carries `id`, `effort`, and the rendered `label`; `context` carries `fill_pct`, occupied `used_tokens`, `window`, `severity`, completed `compactions`, and current `compacting`; `stats` carries the token split, `cost_usd`, RimZ's provider-neutral `active_secs` estimate, `tool_calls`, and the optional open `tool_repeat` run; `timeline` carries registration, turn-start, activity, and observation timestamps; `placement` carries `channel`, `worktree`, `branch`, `pane`, and `pr`; and `budget` carries the effective ledger-resolved `cap`, live `spent_usd`, `parked`, and the current `park` label. A PR carries `number`, `state` (`open`, `closed`, or `merged`), and `ci` (`pending`, `passing`, or `failing`).

Every report key is present: an unknown scalar or object is `null`, a count is `0`, and a collection is empty. The raw provider `AgentContext` is outside this schema.

`me` marks at most one entry. RimZ first matches the caller's normalized `TMUX_PANE` or `ZELLIJ_PANE_ID` to the published pane binding, then falls back to the `RIMZ_AGENT_KIND`, `RIMZ_AGENT_NAME`, `RIMZ_AGENT_PROFILE`, and `RIMZ_AGENT_ROLE` launch identity; calls outside a recognized agent leave every entry false.

| Column | What it shows |
|---|---|
| `AGENT` | the shortest handle you can type back under that header — its role (`@coder`), else its explicit `--name` (`@writer`), else its profile (`@planner`), else `@<kind>`, growing an ordinal only when two of a kind share one lane |
| `STATUS` | the plain status label, with provider-limit and API-error turns projected to `paused` or `failed`; `show` carries the turn phase when you need it |

The activity description — the same field the sidebar card shows — renders under each row, whitespace-collapsed and wrapped to at most three indented lines with an ellipsis when truncated; agents without one omit the description block.

#### `show` / `inspect`

`show` and its `inspect` alias print a describe-style report with Agent, Activity, Context, Placement, Run, Messages, and Recent transcript sections. The Context section's cost, token split, and active time cover the durable agent seat's lifetime across resumed sessions through the same fold attribution uses; live context fill, window, tool activity, and the no-transcript fallback remain session-scoped. An open identical-tool run appears once it reaches the configured warning threshold (`Bash ×23, 4m`). `--capture` appends a Capture section that frames the bound pane's visible area with its pane id in the top border (an error when the agent has no bound pane), and `--ansi` keeps colors inside that frame.

`show --json` places the same projected agent entry under `agent`, with `stale`, rich `ask`, `run`, `messages`, and raw `capture` data as show-only siblings when applicable. A stopped audit agent keeps the full stable entry shape, with published-row fields such as context severity and active time set to `null`. Supervised `-p` runs shape their output with `--output-format` instead.

#### `logs`

`logs <ref>` is the agent-centric transcript view: `-n/--tail N` keeps the last N chat lines, `-f/--follow` prints new lines as they land, `--all` includes prior-session history, and `--json` emits JSON for one-shot reads or NDJSON in follow mode. It uses the same transcript scope and rendering as [`rimz transcript @ref`](./transcript.md).

#### `history`

`history <ref>` groups the provider transcript at each user message and assigns the session's API-call spend rows to those time spans. The table reports local start time, duration, fresh-input and output tokens, price, best-effort outcome, and prompt preview; `-n/--tail N` keeps the newest turns and `--json` emits the full records including cache-read tokens, cache-write tokens, and API-call count. `done` means an assistant reply closed the turn, `open` is the live in-flight final turn, and `cut` means the turn or session ended without an assistant reply. Live resolution falls back to the audit rollup, so stopped sessions remain readable while their provider transcript exists. Per-turn grouping requires an adapter with normalized transcript and spend coverage; see [agent support](../agent-support.md) for the current per-adapter surface.

#### `attribution`

`attribution [SCOPE]` credits the root agents that worked the selected lane even after their panes and processes exit. `SCOPE` accepts the same `#channel`, worktree, branch, directory name, and path spellings as `list`; the default is the caller's current lane, and `--all` covers every lane in the room. Attribution covers the lane's retained history rather than inferring a commit or time window; JSON timestamps let callers apply their own window. It reads durable and local provider state without requiring a live multiplexer.

Agents that never opened a turn and have no recorded active time, asks, messages, tool calls, compactions, subagents, tokens, or cost are omitted from the listing and agent counts. The panel and JSON retain an agent that durably opened a turn even when those statistics are unavailable; `--md` omits that stat-less row and recomputes its groups and totals.

The default panel groups members by team and shows provider, model and effort, estimated active time, calls, messages, transcript-priced cost, token detail, and subagents as labelled lines. Asks lead the calls line. Messages read `{n} from you · {n} from teammates · {n} to teammates`; the Total line counts received messages once and excludes RimZ-authored automation. The final subagents line groups children by provider-reported type as `{count} × {type} ({cost})`, credits each type's durable transcript cost, and folds description-like or missing types into `other`. A member cost that includes child spend says `incl. subagents`; the subagent line is a breakdown, not an amount to add. Any unavailable or empty labelled figure is omitted rather than printed as an unknown placeholder.

`--md` emits the panel's figures and wording as a collapsed `<details>` receipt for a pull-request body, while omitting opened-turn-only members and recomputing the totals. The summary links RimZ to its repository. An empty scope emits no Markdown. `--json` emits a schema 3 document, and `--json` conflicts with `--md`.

The JSON document carries `schema`, `generated_at`, `rimz_version`, `scope`, `groups`, and `totals`. `scope` has `selector`, `channel`, `branch`, and `worktree`; each group has `team`, `totals`, and `members`; a team has `name` and launch-ordered `roles`. A member has `handle`, `role`, `name`, `kind`, `provider`, `model`, `effort`, `presence`, `me`, `launch_ordinal`, `sessions`, `registered_at`, `last_activity`, `active_secs`, `asks`, `asks_answered`, `tool_calls`, `compactions`, `messages`, `tokens`, `cost_usd`, and `subagents`. `asks_answered` joins durable answer records to their ask id. `presence` is `live` or `exited`; `messages` has `from_user`, `from_teammates`, and `to_teammates`; `tokens` has `input`, `output`, `cache_write`, and `cache_read`; each subagent group has `task`, `count`, and `cost_usd`. Group and document totals have `agents`, `active_secs`, `wall_clock_secs`, `cost_usd`, `asks`, `asks_answered`, `tool_calls`, `compactions`, the same message counts, and the same token split. A missing active-time or cost figure is `null`, never a wall-clock or zero substitute.

One contributor can own several provider sessions: compaction continuation and `/clear` start fresh session ids while keeping the same team role, launch cohort cell, explicit name, or pane seat. Attribution folds those records into one member in that order of identity strength, keeps provider kind in the key, and publishes `sessions` so the fold stays auditable.

Identity, presence, tool calls, compactions, subagent identity, and clocks come from the store's audit rollup, which retains ended sessions. Prompts, agent messages, and asks come from RimZ's append-only conversation transcript; matched system nudges carry RimZ's sender identity and do not count as prompts from you. Transcript entries written before this distinction can still count historical system nudges as user prompts. Sent-message credit joins the sender's rendered handle because message records do not carry its session id. Tokens and dollars come together from the adapter's historical spend parser and the shared price book; companion child transcripts split the subagent portion by child before it is grouped by type. Estimated active time comes from the per-session runtime sidecar; `rimz gc` removes stale sidecars after its runtime retention (24 hours by default), so old credit keeps its agent and transcript figures while `active_secs` becomes `null`.

#### `top`

`top` ranks live pane-backed agents, including launched children nested in a parent card, by process-tree resources: CPU, memory, I/O per second, process count, context fill, tokens, and age. It streams by default; `--once` takes two samples 500 ms apart and exits for scripts, while `-w/--worktree` selects one lane. Resource columns read `-` on platforms or panes where process metrics are unavailable, while context and token columns still render.

#### `focus`

`focus` jumps to an agent's pane.

#### `fork`

`fork <ref>` resolves one live agent in the current channel first, then falls back to the stopped-agent audit rollup, and opens a provider-native copy with the full conversation history under a new session id. The source stays untouched. The fork replays the source profile's rendered launch posture — system prompt files, model, effort, permission argv, and profile args — through the same seam as restart, preserving profile-declared prompts and tool configuration instead of reverting to provider defaults. A profile that no longer resolves forks bare with a warning, while a profile that now names a different provider refuses. Prompt-cache lineage remains provider-controlled and may start cold under the fork's new session identity. The fork carries the source channel and drops team and role identity so the original role handle stays unique.

The fork always opens in the source agent's recorded worktree. A plain fork takes over the launching pane; `--new-pane` splits it into the current view, `--new-tab` opens a separate view, and `--bg` implies a background split. `--name/-n <name>` pins its handle. Cross-worktree forks and a first prompt are outside this command; send the first new instruction with `rimz message` after the fork opens.

| Agent | Native fork argv |
| --- | --- |
| Claude | `claude --resume <id> --fork-session` |
| Codex | `codex fork <id>` |
| Pi | `pi --fork <id>` |
| OpenCode | `opencode --session <id> --fork` |

#### `wait`

`wait` blocks on supervised runs (by run id or pet name) and interactive agents reaching an idle/success gate. One reference keeps the answer-oriented behavior: a plain run wait prints the final assistant message, `--stream` tails assistant text as it lands, `--stream --json` emits NDJSON run events, and `--from-start` replays from the top before tailing.

Several references form a join. Text mode prints each final answer in completion order under a `--- <name> ---` header; only abnormal results add a status suffix. Diagnostics on stderr carry the same header. `--json` prints one labeled map `{name: {status, exit, cost, transcript_path, last_message}}` after every target settles. The command succeeds when every target completes; otherwise it exits with the first non-completed target's status code in argument order. `--stream` accepts one target because one stdout stream has one transcript.

`--any` returns on the first terminal target regardless of success or failure, prints the same labeled answer block for the winner, and exits with that target's status code; JSON mode prints the labeled map with only the winner. The other targets keep running. `--timeout` caps the whole wait and exits `124` without changing pending targets; text mode names each unfinished target on stderr as `--- <name> (timed out) ---`, while JSON mode stamps unfinished targets `timed_out` in the result map before exiting.

`rimz subagents wait` delegates to this join after restricting references to the calling agent's own children. With no names it supplies every live child automatically.

#### `refresh`

`refresh` forces the transcript tail re-read past the stat gate, re-runs Codex turn-death confirmation against the live pane when one is bound, spawns the kind's detached rich-context helper when one exists, and wakes sidebars after an inline merge. With a reference it resolves exactly one agent in scope; without a reference it refreshes every live root agent in the current channel; with `--all` it takes no reference and covers every live root agent in the workspace.

#### `stop`

`stop` tears down a run's pane — canceling supervision while the run is live, reclaiming a completed `--keep` pane — or closes the agent's pane when the ref names no run. It ends the CLI process the way Ctrl+C would; the provider's session files stay on disk, so a stopped agent is one `--resume` away. A parent stop first stops its live RimZ-launched subagents; this also applies when `rimz teams stop` reaches that parent. Without `--all`, `stop` resolves to exactly one agent; with `--all`, it resolves every match, prints one result line per agent, and exits non-zero if any stop failed.

#### `restart`

`restart <ref>` acts on one live pane. It focuses that pane, opens its replacement in the same layout position, then closes the old pane; focus follows the replacement on both Zellij and tmux. The replacement re-renders the stamped profile from current configuration, preserves role, team, channel, and permission mode, and uses the provider's native session resume. One-off model, `--agent`, and passthrough flags are not durable and are not replayed. If the profile now resolves to a different provider — including a session launched through `--agent` — restart refuses and points back to `rimz agents <profile> --agent <kind>` for an explicit fresh launch. When no resume command or recorded conversation exists, restart launches fresh and prints `restarted fresh as @<allocated-name> — <reason>`; the allocator may choose a new name while the old live card still owns its handle, so the output makes that degraded rename explicit.
