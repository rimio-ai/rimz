# Agent control CLI

`rimz agents` launches agents into panes and reads and drives the agents already running in the room. `rimz agents list` lists the current channel's agent cards; `rimz agents <SPEC> [PROMPT]` launches; bare `rimz agents`, with no verb and no spec, refuses with exit 2 and prints the `list` command to run; each subcommand below acts on launches, lanes, or one agent. Run these commands from inside the room, or from any directory that resolves to the same workspace ([which room a command reaches](../cli.md#which-room-a-command-reaches)). `rimz agents <subcommand> --help` prints every flag.

Why you would launch through RimZ instead of the bare CLI is the [agents guide](../../guide/fleet.md). Profiles, commands, and teams are configured in the [configuration guide](../../guide/configuration.md#agent-profiles-commands-and-teams), and the launch machinery is [fleet.md](../../internals/harness/fleet.md).

| Task | Commands | Section |
| --- | --- | --- |
| Launch agents, a layout, or a team role | `rimz agents <SPEC>`, `launch` | [Launch agents](#launch-agents) |
| Run one scripted, exit-coded turn | `rimz agents <SPEC> <PROMPT> -p` | [Supervised runs](#supervised-runs--p) |
| Reopen closed agents | `--resume`, `resume` | [Resume agents](#resume-agents) |
| See what a launch would run | `profiles`, `explain` | [Inspect profiles and launch plans](#inspect-profiles-and-launch-plans) |
| Check machine definitions | `validate` | [Validate definitions](#validate-definitions) |
| Add a third-party agent kind | `register`, `check` | [Register a third-party kind](#register-a-third-party-kind) |
| Read and drive running agents | `list`, `show`, `logs`, `history`, `attribution`, `top`, `focus`, `fork`, `wait`, `refresh`, `stop`, `restart`, `compact` | [List and manage agents](#list-and-manage-agents) |
| Cap one agent's spend | `budget` | [Budget CLI](./budget.md#cap-one-agent) |

Neighbouring commands have their own pages: [`rimz teams`](./teams.md) launches and drives configured teams, [`rimz subagents`](./subagents.md) launches supervised children from an agent, [`rimz message`](./message.md) talks to live agents, [`rimz transcript`](./transcript.md) reads the chat log, [`rimz pane`](./pane.md) reads and types into raw panes, and [`rimz worktree`](./worktree.md) manages worktree lanes.

## Addressing agents

Every command that names an agent uses one address grammar: `@<handle>` names who, an optional `#<channel>` names the lane, and a raw pane id names one pane exactly. This section is the one definition; `message`, `transcript`, `pane capture`/`send`/`focus`, and the `agents` management verbs all assume it. The resolution rules behind it are in [fleet.md → The address](../../internals/harness/fleet.md#the-address).

These handles name one agent:

| Handle | Names |
| --- | --- |
| `@coder` | A team or ad-hoc role. It follows the launch's current conversation; earlier conversations in the same seat stay readable through `show`, `history`, and `fork` but never receive messages. |
| `@writer` | An explicit name from a single-agent launch, such as `rimz agents claude --name writer`. |
| `@swift-otter` | The pet name RimZ gives every launch. |
| `@claude-2` | A kind plus ordinal. Still accepted as a selector; listings show the pet name instead. |
| `@<session-prefix>` | A leading slice of the provider session id. |
| `@me` | The calling agent, from an agent pane: accepted by single-agent commands, direct messages, and loop delivery targets. `me` is reserved and cannot name an agent, profile, command, or team. |

These handles name a type and can match several agents:

| Handle | Names |
| --- | --- |
| `@claude` | Every agent of that kind in the channel. |
| `@planner` | Every agent launched under that [profile](../../guide/configuration.md#agent-profiles-commands-and-teams). |
| `@all` | Every agent in the channel at resolution time. |

A channel scopes the lookup. Without one, the channel is the `#name` tab or worktree you run the command in; a team member launched in place carries `RIMZ_CHANNEL=<dir>/<team>`, so its own commands default to that lane.

| Form | Matches |
| --- | --- |
| `#design` | A lane agents were launched into with `--channel design`. |
| `#auth-refresh` | A worktree lane, by branch, generated worktree name, or directory basename. The flag spelling is `--worktree auth-refresh`. |
| `#query-engine/forge` | The team `forge` launched in place from the `query-engine` directory. |
| `tmux:%12`, `zellij:terminal_3` | One pane by id, ignoring channels. The [`pane` commands](./pane.md) also accept the literal `sidebar`. |

How many matches a command accepts:

| Commands | Rule |
| --- | --- |
| `show`, `logs`, `history`, `focus`, `fork`, `restart`, `compact`, `budget` | Exactly one agent. An address that matches several fails and lists the candidates. |
| `stop` | One agent; `--all` acts on every match. |
| `wait` | One or more references, each resolved independently. |
| `refresh` | One agent with a reference; every live root agent in the channel without one; the whole workspace with `--all`. |
| `message` | One agent unless you opt into fan-out with `--all` or address `@all`. |

A fan-out message is prefixed with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message. A human-sent `@all` reaches every match. An agent-sent `@all` skips the sender and fails when no other agent remains; an explicit `--all @claude` from an agent keeps every match. The [message reference](./message.md) owns delivery.

`message` requires the `@` sigil, which also keeps a target from being read as a launch spec. `show`, `logs`, `history`, `fork`, `wait`, `stop`, `restart`, and `refresh` also accept a bare selector (`swift-otter`), and `wait` and `stop` (like `rimz transcript`) also accept a supervised run id. `rimz agents @coder` is refused with a hint to use `rimz agents show @coder`.

## Launch agents

### Launch a layout

`rimz agents <SPEC> [PROMPT]` opens the panes a spec describes and starts each agent's stock CLI in one of them. `rimz agents launch <SPEC>` is the same command with an explicit verb.

```sh
rimz agents peer                                    # built-in team: claude,codex side by side
rimz agents claude,codex+term                       # Claude | Codex, tiled over a shell
rimz agents claude/codex/term                       # one Zellij stack; tmux tiles the rows
rimz agents claude:planner,codex:coder -w feat-x    # ad-hoc role handles in a worktree
rimz agents claude,codex --channel=design "Draft the API shape."   # prompt goes to Claude
rimz agents 'vim,codex+term' "Review the CLI docs."  # a raw command cell beside an agent
rimz agents codex --from-pr 42 "Review this pull request."
rimz agents forge.planner                            # re-add one role of team forge
rimz agents claude --worktree "Take one approach."   # each run gets its own fresh worktree
```

A spec is one of three things:

| Spec | Launches |
| --- | --- |
| A team name | The [team](../../guide/configuration.md#agent-profiles-commands-and-teams)'s layout. [`rimz teams`](./teams.md#launch-a-team) is the doorway for configured cohorts. The built-in `peer` team is the roleless `claude,codex`. |
| `<team>.<role>` | One declared role of a running or stopped team, with the same role handle and team lane. Inside that team's lane the bare role is enough: `rimz agents planner` in `#forge` means `rimz agents forge.planner`. A bare role that also names a profile or command resolving to a different agent is ambiguous and refused. |
| An inline layout | Cells joined by `,` (columns), `+` (tiled rows), or `/` (a stacked row group; Zellij stacks, tmux tiles). |

Each inline cell is `term` (a plain shell), an agent kind, a [permission-mode cell](#permission-mode-cells) such as `claude-plan`, a configured profile, a configured command, or any executable on your `PATH`. Add `:<role>` to an agent cell for an ad-hoc role handle. Quote a spec that contains `+`, a space, or anything else your shell expands. The full grammar and how cells compile to panes are in [fleet.md → The layout IR](../../internals/harness/fleet.md#the-layout-ir).

The optional `PROMPT` goes to exactly one leader: the team's configured `leader` role, else its first declared role, else the first agent cell. When the first cell repeats (`claude,claude`), give it a role so the target is unambiguous. To send every agent the same text, launch without a prompt and use `rimz message @all`. A second positional that is itself a cell (`rimz agents claude codex`) is refused with a `rimz agents a,b` hint, and a scope such as `rimz agents '#auth'` in the SPEC slot is refused with exit 2 and the `rimz agents list '#auth'` command, prompt or not.

A launch that opens a new pane or tab prints a receipt: the checkout path, then each member's handle, provider, and resolved model (`-` when unset). When you gave a prompt, a shortened echo names its recipient, and non-team launches add copy-ready `Reach` and `Wait` commands for the leader. A launch that creates a launch-prompt run (below) prints the peer receipt in their place. The receipt records what was launched; the agents start asynchronously. A named team prints the [team receipt](./teams.md#launch-a-team) instead, and a launch that takes over the current pane prints none.

A freshly launched agent whose CLI exits with an error before its session opens is relaunched in the same pane. RimZ prints `rimz: codex exited with code 1 before its session opened; relaunching in 3s (1 of 3, Ctrl-C cancels)`, waits, and starts the same command again, up to three times by default ([startup relaunches](../../guide/configuration.md#startup-relaunches)). When the last relaunch dies the same way, RimZ says it is not relaunching again and reports the failure as it would have the first. A session opened with `--resume` or [`fork`](#fork) is never relaunched.

An agent-launched interactive peer with a nonempty prompt also gets a per-turn run before its pane opens. Only the prompt recipient gets that run in a layout. A team launch instead reports its leader once per `Done` ([team reports](./teams.md#team-reports)). Its receipt names the `AGENT_REPORT` that returns when the launcher's fleet settles, the launcher-visible response file, and the peer's handle (`@<name>#<channel>`, or `@<name>` outside a channel) in a `rimz agents wait` command and a `rimz message` follow-up. Later launcher messages can open new reportable turns; user-typed turns do not. User launches, promptless launches, and `--resume` create no launch-prompt run. An adapter without native root turn-start and turn-end hooks creates none and its receipt says it will not report back. The peer stays interactive after reporting.

`--detach` launches that prompt without the report: the run and its response file are still written and `rimz agents wait <RUN_ID>` still reads the answer, but no `AGENT_REPORT` returns for the launch, the launcher is not held open or shown as waiting, and the receipt says so and names the wait by the peer's handle. A launcher message sent after the launch turn settles still opens a reported turn; one that lands while the launch turn is open becomes part of the unreported answer. On a team launch the flag drops the `TEAM_REPORT` instead ([team reports](./teams.md#team-reports)). `--detach` is refused without a prompt, with `--resume`, and on `-p` without `--bg`, where the run prints its answer inline.

Launches refuse before they create any pane or record when:

- the final prompt exceeds 120 KiB (122880 bytes), counting `--stdin` content and any reminder text appended to it, because the provider receives the prompt as one argument (move detail into a file the agent reads);
- an agent-launched command would exceed `[agents] max-chain-length` successive agent-to-agent launches (default `3`);
- a Markdown definition has invalid syntax, an unknown key, or a resolution error (the error names the source; run `rimz agents validate`).

When an agent runs `rimz agents`, each new agent is an independent top-level peer with its own sidebar row. A parented child with one prompt comes only from the agent-only [`rimz subagents`](./subagents.md) command, and a subagent cannot launch agents or subagents.

`rimz agents` shapes individual agents: model, effort, prompts, permission posture, name, placement, and supervised runs. [`rimz teams`](./teams.md) owns where a configured cohort runs, whether it resumes, and what each member may spend.

`PROMPT` may start with `-`, except that prompts starting with `-w` or `-n` are refused to avoid mistaking attached short-option values for prompts; write `-w NAME` or `-n NAME` with a space. An interactive launch cannot take a prompt that is itself one of the command's flags (`--model`, `--model=…`, `-h`, `--help`, etc.); a print run can read it from `-p --stdin`, since `--` introduces provider passthrough arguments, not an escaped prompt.

### Permission-mode cells

A `<kind>-<mode>` cell launches that kind with one permission posture. RimZ renders the posture into the adapter's own flags; a mode the adapter expresses through no flag still launches and adds no arguments.

| Cell | Available for | Launch posture |
| --- | --- | --- |
| `<kind>-ask` | every kind | the agent asks before tool use |
| `<kind>-plan` | every kind | native plan mode where the CLI has a flag for it (Claude, Antigravity, Copilot, Cursor, Droid, Kimi, OpenCode, Qwen); the default posture otherwise (Codex, Grok, Pi, Amp, Kiro) |
| `<kind>-auto` | every kind except `opencode`, `amp`, `kiro`, and `pi` | the adapter's automatic-approval flags |
| `<kind>-yolo` | every kind except `droid`, `amp`, `kiro`, and `pi` | the adapter's bypass flags |

On the command line, `--ask` keeps native permission prompts and `--yolo` passes the adapter's bypass flags. Either flag replaces a profile's declared mode or a permission-mode cell's posture for that run; a restart returns to the profile's mode. With neither, the declared mode stays, or the CLI keeps its own default (supervised `-p` defaults an unset mode to `auto`). Replacement recognizes the adapter's rendered flag vectors, not alternate spellings in raw profile `args` such as `--permission-mode=plan`. Each agent's exact flags per mode are in [agent support](../agent-support.md#permission-modes).

### Shared launch params

These flags apply to every agent cell in the launch, and each adapter renders them into its own native flags.

| Flag | Effect |
| --- | --- |
| `--model <MODEL>` | Model for every agent cell, replacing the profile's. Droid refuses it; the per-agent flags are in [agent support](../agent-support.md#model-and-effort). |
| `--tier <TIER>` | Select `intern`, `junior`, `senior`, or `principal` for the launched Markdown profile. Conflicts with `--model`. |
| `--effort <LEVEL>` | Reasoning effort, passed to the provider's effort flag without validation, so levels are whatever the provider accepts. Kinds with no effort flag refuse the launch; the per-agent flags are in [agent support](../agent-support.md#model-and-effort). |
| `--budget <AMOUNT[/day]>` | Dollar cap per agent: a bare amount caps the session, `/day` resets at the local day boundary. Inspect or change it later with [`rimz agents budget`](./budget.md#cap-one-agent). |
| `--system-prompt-file <PATH>` | Replace each agent's base system prompt with the file. |
| `--append-system-prompt-file <PATH>` | Repeatable. Replaces the inherited profile and role fragment list with these files, in command-line order; the team layer stays intact. |
| `--ask`, `--yolo` | Replace the permission posture for this run, including a profile's declared mode; see [Permission-mode cells](#permission-mode-cells). The two conflict. |
| `--agent <PROFILE\|KIND>` | Re-base every agent cell onto another profile or provider (below). |
| `--isolation host\|sandbox` | Isolation for this launch instead of its profile default or the machine's `agents.isolation` (below). |
| `-n`, `--name <NAME>` | Handle for a single-agent launch: `rimz agents claude --name writer` appears as `@writer`. Without it, after any role or unique profile, the agent renders as `@<kind>` when unambiguous, else its pet name. |
| `--description <TEXT>` | Seeds the card's second line until the agent names its own session. It never reaches the agent's argv or environment. |
| `-- <ARGS>...` | Raw arguments appended to every agent cell's command. |

The profile fields these flags override are described in the [configuration guide](../../guide/configuration.md#profiles).

`--model` takes a concrete model; a tier name is refused with the fix `--tier <tier>`. `--tier` and `--model <listed-model>` use the [ordered tier lists](../../guide/configuration.md#model-tiers), preferring the exact model or the `--agent <kind>` family before the remaining entries, then higher tiers, never lower. With only `--tier`, the definition's family preference stays. A family preference does not pin the provider. A team override routes each Markdown role separately. Project TOML roles take `--model` exactly; a team containing one refuses `--tier`, naming the role. Project TOML profiles cannot use `--tier`. The definition's absolute effort is retained unless `--effort` overrides it; otherwise the chosen model's default applies.

For a routed profile that can render the requested kind, `--agent <kind>` selects that family's complete definition, including its tool arguments. Otherwise `--agent` uses the re-base behavior below.

`--agent` swaps the engine and keeps the job. The replacement profile or kind supplies the provider, and supplies model and effort whenever it sets them, so `rimz agents fixer --agent opus` runs the fixer prompt on the `opus` profile's model and effort. When the replacement leaves model or effort unset, a same-provider re-base keeps the cell's values, while a provider change drops the cell's model and keeps its effort. Permission mode, isolation default, budget, `auto-compact`, skills, and prompt files carry over from the original profile, and the replacement fills only what the original left unset. Raw profile `args` carry over on a same-provider re-base; on a provider change they come from the replacement instead, so a named Codex definition can supply its model and generated tool flags when selected as the replacement. Command-line flags still win, and typed fields the new adapter cannot express fail before any pane opens. `--agent` applies to this launch only: `--resume` refuses it, and a later `restart` refuses when the profile resolves back to a different provider.

Isolation resolves as recorded `--isolation` flag, then the profile's `isolation` default, then machine `agents.isolation`. Sandbox isolation runs the bubblewrap preflight and refuses before any pane opens when it fails, including when selected by a profile. The override is recorded on each agent, so `restart`, `fork`, resume, room rebirth, and the agent's `rimz subagents` children keep it. A cohort resume with `--isolation host|sandbox` replaces that recorded override while keeping the conversation. Omitting the flag preserves any recorded override; an agent with none re-reads its profile default on every relaunch. The profile default itself is never recorded as a flag.

### Channel, worktree, and placement

A launch runs in the current directory's checkout by default. These flags choose its directory and channel:

| Flag | Effect |
| --- | --- |
| `--cwd <DIR>` | Launch in an existing directory without changing rooms or taking worktree ownership. Relative paths resolve against the caller's current directory, in its sandbox view when sandboxed. Missing directories are refused, never created. The channel follows the directory basename unless `--channel` names one. Conflicts with `--worktree`, `--from-pr`, `--resume`, and `--fresh`. |
| `-w`, `--worktree [NAME]` | Reuse or create the RimZ-owned worktree `NAME`, on channel `#NAME` in a tab named `#NAME`. Bare `-w` creates a fresh worktree with a generated name. |
| `--from-pr <NUMBER\|URL>` | Reuse the marked tree holding a same-repository PR's head branch, or create a tree named `pr-<N>` unless `-w NAME` names it. An explicit name must match an existing holder. |
| `--channel <NAME>` | Launch into the lane `#NAME` in the selected directory, in a tab named `#NAME`. The lane exists while agents run in it. `NAME` is bare ASCII letters, digits, `_`, or `-` (`--channel '#design'` is refused with `invalid channel name`), and a name a RimZ-owned worktree holds is refused with ``channel `NAME` is backed by a worktree``. |

Fresh worktree launches print the same [stale-base warning](./worktree.md#create-a-worktree) as `worktree new`, with the remedy to fast-forward the base first or set `agents.worktree.base` to `fresh`.

Spell the worktree name without the channel's `#`. `-w feat-a` gives channel `#feat-a`; `-w '#feat-a'` fails with `invalid worktree name`; and an unquoted `-w #feat-a` in a shell with comments enabled reaches RimZ as a bare `-w` and generates a name. A branch-style name is accepted: `-w feat/great` creates branch `feat/great` with worktree, channel, and tab `feat-great`.

A `--from-pr` URL must match the `origin` remote. With `gh` or `tea` available, RimZ configures the source branch's push destination; on an unsupported forge the checkout is review-only, with pushes unconfigured. A reused tree may have no recorded PR number, which RimZ fills in, but one recording another PR refuses. Differing local and PR tips use the [PR checkout choice and non-interactive rules](./worktree.md#check-out-a-pull-request). Worktrees RimZ creates carry a marker and are cleaned up with [`rimz worktree remove`](./worktree.md) or `rimz gc`; the [worktrees guide](../../guide/worktrees.md) covers the workflow.

RimZ creates the worktree from the current directory's main Git repository, including when you run from one of its linked worktrees, and falls back to the room root outside any Git repository. Two cases ask first:

- When that repository differs from the room root, RimZ shows both paths and asks. A non-interactive launch refuses; pass `--root <git-root>` or run from the room's checkout. The room's `worktree list`, `worktree remove`, and `gc` do not reach worktrees of the other repository, so remove those from their own repository or with `git worktree remove`.
- When `-w NAME` names an existing linked worktree RimZ did not create, in the configured worktree directory and of the same repository, RimZ asks before entering it (default no). It never adopts or seeds that worktree, and `remove` and `gc` leave it alone. A non-interactive launch refuses. `--from-pr` instead reuses the marked tree holding the PR's branch; a non-RimZ checkout holding it refuses.

Placement decides whether the launch takes over your pane, splits it, or opens a tab. Under the default `auto` policy, a worktree launch, a named-channel launch, or a multi-cell spec opens its own tab, and any other single-cell launch (including a single team role) takes over the current pane and returns to the shell when the agent exits. The [`[agents] placement`](../../guide/configuration.md#placement) setting changes the default; these flags override it for one launch:

| Flag | Placement |
| --- | --- |
| `--new-pane` | Split the current tab. Single agent cell only. |
| `--new-tab` | Open a new tab (tmux window). |
| `--bg` | Keep focus where it is: an in-place launch becomes a split, and an already-live cohort or lane does not get a focus jump. Agent callers always get this focus behavior, including fork, resume, and restart, without passing the flag. With `-p`, `--bg` instead returns from a supervised run at once; see [Supervised runs](#supervised-runs--p). |
| `--detach` | Not placement: launch the prompt without a report and without holding the launcher, as described under [Launch a layout](#launch-a-layout). |

The split-versus-tab mechanics are in [fleet.md → Placement](../../internals/harness/fleet.md#placement).

User-shell launches take focus unless `--bg` is passed. Agent callers cannot override focus preservation; explicit jump commands such as `agents focus` remain available. On Zellij 0.44.x, opening a background pane or tab can briefly move focus before returning it to the prior pane; tmux and Zellij 0.45+ open without that transient switch.

### Relaunch into a named worktree

A checkout holding another team (a live member, or team history with a board stage other than `Done`) [refuses a fresh launch](../../internals/harness/fleet.md#one-team-per-checkout); setting that board to `Stage: Done` releases a stopped run. A channel held at a different checkout [also refuses a fresh team launch](../../internals/harness/fleet.md#one-team-per-channel), even for the same team, whether the channel is explicit or derived from the checkout.

Launching a named team, or an inline spec of two or more cells, into a named worktree that already has a cohort reconciles with that cohort before creating anything:

| Existing cohort | Result |
| --- | --- |
| live | Focuses its tab and exits, in any worktree, unless `--bg` or an agent caller keeps focus where it is. |
| closed, RimZ-owned worktree with work in progress | Prompts `(resume/fresh/cancel)`, default `resume`. |
| closed, RimZ-owned worktree that is clean and merged | Prompts `(remove/fresh/cancel)`, default `cancel`. `remove` deletes the worktree and its branch, then launches. |
| closed, worktree RimZ did not create | Asks to enter the worktree, as above; add `--resume` to resume the cohort. |

The same table applies to `--from-pr` when it reuses the tree holding the PR head branch. If its tips differ, the PR tip choice comes after the reconciliation prompt and before launching or resuming. A live cohort exits without moving the tip. Choosing `remove` resolves a fresh PR checkout instead. `--resume` and `--fresh` are rejected alongside `--from-pr`; to answer in advance, run the printed `-w <holder>` command instead.

Enter takes the default, and an unrecognized answer or end of input prints `canceled; nothing launched`. Without a terminal on stdin, RimZ prints the resume-or-remove command and the `--fresh` command instead of prompting. A single-agent spec launches fresh without reconciling.

Two flags answer the prompt in advance. `--resume` (or `--continue`) resumes the prior cohort even when the worktree is clean or merged; see [Resume a cohort](#resume-a-cohort). `--fresh` starts new sessions in the existing checkout:

```sh
rimz agents forge -w restore-living-team --fresh   # new sessions, same checkout and files
```

`--fresh` removes nothing. The worktree, its branch, and every file survive, so the previous run's team scratch files still reach each member: with its first prompt on Claude and Codex, and in the launch reminder where another adapter takes one. The closed members keep their store rows, the new members get new handles in the same channel, and messages still queued for the old members are archived by the next `rimz gc`. `--fresh` needs a named worktree (`-w NAME`), conflicts with `--resume` and `--from-pr`, is refused with `-p`, and never duplicates a live cohort: that case reports the running cohort and follows the [focus rule](#channel-worktree-and-placement). The decision logic is in [fleet.md → Cohort relaunch reconciliation](../../internals/harness/fleet.md#cohort-relaunch-reconciliation).

### Supervised runs (`-p`)

`-p` (`--print`) launches exactly one agent in a real pane, waits until the work ends, prints the result, and exits with the run's status, so a script branches on the outcome. The pane stays watchable and steerable while the script waits. The [scripting guide](../../guide/scripting.md) teaches the workflow.

A clean turn end keeps the run `running` while a one-shot wait, a harness wake in flight, or a live or unreported child result is owed. `agents show` adds the parked age; run JSON and live NDJSON status include `parked_at`. The next turn resumes the same run. If nothing remains to wake a parked run, it fails with exit `1` and the reason `parked on a wake that never arrived: no armed wait, no wake in flight, no live subagents` (or an earlier captured failure). `--timeout` still applies while parked; background receipts and `agents wait` joins are unchanged.

```sh
rimz agents codex "Prepare the release checklist." -p --timeout 30m --output-format json
rimz agents claude "Run the long migration audit." -p --bg       # prints a pet name, returns now
rimz agents claude "Review the diff." -p --effort high --system-prompt-file ./review-prompt.md
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause' > out.txt
```

| Exit | Run status |
| --- | --- |
| `0` | completed |
| `1` | failed |
| `123` | verify failed: `--verify` still red after `--max-attempts` |
| `124` | timed out |
| `125` | budget exceeded, including a room or account cap with no headroom at launch |
| `130` | canceled, including Ctrl+C on a blocking run |

In text mode, stdout carries only the final assistant answer, rendered by the shared [agent-prose rule](../cli.md#agent-prose). Any status other than completed prints the status, captured evidence when present, and the transcript path on stderr. Ctrl+C on a blocking run cancels it and lets the wrapper stop the agent before the pane is reclaimed.

| Flag | Effect |
| --- | --- |
| `--output-format text\|json\|stream-json` | `text` (default) prints the final assistant message; `json` prints the full run record; `stream-json` prints run events as NDJSON while the turn runs. The JSON `run_id` opens the RimZ transcript with `rimz transcript <run_id>`; `transcript_path` is the provider's own session file. |
| `--input-format text\|stream-json` | `text` (default) uses the positional `PROMPT` plus `--stdin` content; `stream-json` reads user messages from stdin until EOF and refuses a positional prompt or `--stdin`. |
| `--stdin` | Read stdin to EOF as prompt content. With a positional prompt, the prompt comes first and stdin follows inside `<stdin>…</stdin>` tags. |
| `--timeout <DURATION>` | Cap the run; exceeding it exits `124`. |
| `--bg` | Print the run's pet name on stdout and return immediately, with a receipt on stderr. Use that name with `agents wait`, `agents show`, `agents stop`, or `message --steer`. Launched from an agent, the run joins that agent's [fleet report](./subagents.md#the-fleet-report), and the receipt says so. Refuses `--output-format stream-json`. |
| `--keep` | Leave the pane open after the run completes; `rimz agents stop` reclaims it. |
| `--detach` | With `--bg`, from an agent: keep the run out of that agent's fleet report and do not hold the agent for it. `agents wait` still reads the answer. Refused without `--bg`. |
| `--max-turns <N>` | Cap agentic turns through the CLI's native limit (Claude and Grok `--max-turns`, Qwen `--max-session-turns`). Other kinds refuse the run with `<agent> does not support --max-turns`. |
| `--retries <N>` | Rerun a failed (exit `1`) run up to `N` more times, appending the previous failure tail to the prompt. `--timeout` and `--budget` apply per attempt, timeout, budget, and cancel never retry, and the last attempt sets the exit code. |
| `--verify <CMD>` | After each completed turn, run `CMD` in the run's directory; while it fails, re-prompt the same session with the failure evidence. The command's timeout is `--timeout`, or five minutes; a timed-out verify counts as failed. |
| `--max-attempts <N>` | Total agent turns allowed while making `--verify` pass. Default `3`, minimum `1`; requires `--verify`. Exhaustion exits `123`. |

`--retries` and `--verify` need a blocking `text` or `json` run: both refuse `--bg` and `--output-format stream-json`.

A supervised run refuses at entry, before it writes a run record or opens a pane, when:

- RimZ's hooks for the agent are not installed and trusted, because hooks are the completion signal;
- a Codex run's launch directory has no recorded Codex trust decision, because Codex would open its directory-trust screen and never read the prompt (run `rimz trust grant --agents codex` there, or answer that screen once in an interactive `codex` session);
- the room or provider-account daily cap has no headroom (exit `125`; see the [Budget CLI](./budget.md#what-a-cap-blocks));
- a fresh Qwen run's exact Alibaba account has an exhausted quota window (exit `125`); missing or mismatched readings allow the launch.

None of these writes a run record, so `agents wait` and the JSON have nothing to read afterwards. The two cap refusals exit `125` as listed; the hook and Codex-trust refusals exit `2`, and a flag combination the command rejects exits `1`, the same code a run that started and failed uses.

Run records, the completion wakeup, streaming, and pane cleanup are described in [scripting.md](../../internals/harness/scripting.md).

### The account check at launch

Every launch RimZ performs checks the account it launches under, `default` included. An interactive agent or team launch, a cohort or lane resume, a [`restart`](#restart), a [`fork`](#fork), and a parent's message that [resumes an ended child](./subagents.md#follow-up-messages) refuse when that account's home is not a directory or RimZ's hooks there are not installed and trusted. The refusal comes before any pane opens or closes and before any record is written, and names the fix: `rimz hooks install <agent>` for `default`, `rimz accounts add <agent> <name>` for a named account, and the provider's own `/hooks` screen for hooks it has not trusted. Only the launched provider's account is judged. A plugin kind declares no home for RimZ to check, so it passes. [`rimz start`](./getting-started.md#start-the-room) is the one lenient entry: it refuses a broken named account and lets a room open on a broken `default`.

The same launches warn, but continue, when the selected account has no folder-trust decision. Answer the provider's prompt in its pane or grant with `rimz trust grant --agents`. Claude's supervised runs do not require folder trust because its headless mode skips that dialog.

## Resume agents

A closed agent's session files stay where its CLI wrote them, so RimZ can reopen it with the provider's own resume. There are two entry points: `--resume` when you know the spec you launched, and `resume` when you know the lane.

### Resume a cohort

`--resume` (alias `--continue`) relaunches the prior cohort that matches the spec, taking identity, working directory, and channel from the store:

```sh
rimz agents forge --resume                           # the newest closed forge cohort
rimz agents forge -w restore-living-team --resume    # that worktree's forge cohort
rimz agents claude,codex --resume                    # the newest matching inline cohort
rimz agents claude --resume                          # the freshest closed Claude session
rimz agents astra --resume                           # the freshest closed session of profile astra
```

| Spec | Matches |
| --- | --- |
| a team | the cohort by team name, member by role |
| an inline multi-agent spec | the saved launch group, member by cell order |
| a single kind | the freshest closed root session of that kind |
| a profile | the freshest closed root session launched from that profile |

Scope follows the worktree. `-w NAME` resumes that worktree's cohort; bare `-w`, or no flag while you stand in a worktree, scopes to the current worktree; from the project root, the newest match anywhere in the room wins.

A matched member that is still live refuses the command, so an address is never duplicated. Cleanly closed members still match while their worktree exists, and a cell with no resumable member launches fresh in the cohort's directory and channel. A spec that matches nothing fails and names the specs the same scope can resume.

A single-cell resume run from the cohort's own directory takes over the current pane, so the hint an exiting team member leaves (`resume with rimz agents forge.coder --resume`) works from the shell it dropped into. An agent caller or `--bg` makes that a split instead, under the [focus rule](#channel-worktree-and-placement). Run from anywhere else, a lane-scoped resume opens its own tab.

`--resume` and `--continue` accept `--isolation host|sandbox`, `--ask` or `--yolo`, `--model`, `--effort`, `--agent`, and `--tier`. Overrides become the saved launch record for later resumes, restart, and rebirth. Omitting them replays the saved model, effort, and agent base, including model suffixes such as `opus[1m]`; in-session `/model` switches are not carried, so restate one with `--model` to keep it. Sessions predating launch records retain their legacy profile and tier behavior. Isolation replaces the recorded override and also survives child launches. A member with no resumable session starts fresh and records its launch settings normally.

`--agent` must name a profile or kind matching every resumed member's provider. `--tier` requires Markdown definitions and selects only same-provider entries in the requested tier, never a higher tier; a missing entry refuses the whole resume and names the tiers that carry that provider. `--model` and `--tier` conflict. A positional `PROMPT` is queued to the layout's leader as a user message for its first available turn after reopening, not as a new-session prompt.

Resume refuses `--system-prompt-file`, `--append-system-prompt-file` (update the profile instead), and passthrough arguments after `--` (use supported profile settings or launch fresh). The same overrides and refusals apply when you choose resume at the named-worktree prompt: rerun without a refused input, or choose fresh. Identity and cohort options `--from-pr`, `--channel`, `--cwd`, `--name`, `--description`, `--budget`, `-p`, and `--fresh` still conflict with resume.

### Resume a lane by place

`rimz agents resume [SCOPE]` makes one lane whole again from its recorded agents, without retyping the team or layout:

```sh
rimz agents resume '#docs'        # the docs lane
rimz agents resume '#docs' --fresh # new team sessions in the lane's saved shape
rimz agents resume pr-69          # by worktree name
rimz agents resume -w pr-69       # flag spelling of the same scope
rimz agents resume --from-pr 69   # the local worktree created from PR 69
rimz agents resume                # in a worktree: that lane; at the project root: list resumable lanes
```

`SCOPE` takes a `#channel`, worktree name, branch, directory name, or path, as `agents list` does. `--from-pr <NUMBER|URL>` finds the worktree by its recorded pull-request provenance, then by the name `pr-<N>`. Resolution is local: no network request, no worktree creation.

Members are root sessions: subagent children never come back through resume; their parent relaunches what it still needs. The durable lane listing's `MEMBERS` and `LIVE` counts exclude children.

| Lane state | Result |
| --- | --- |
| every member live | Focuses the freshest member's pane and exits `0`, or reports it already live without jumping for an agent caller or `--bg`. |
| some members live | Splits only the closed members back into the live tab and reports each live handle it skipped. |
| every member closed | Rebuilds team layouts in declared order and restores other agents as flat panes. |

Each restored agent keeps its session id, role, team, channel, and working directory from the store, so a lane comes back under the same handles after a soft reset. Profiles and team layouts render from the current Markdown definitions. `--bg` and agent callers keep focus where it is, under the [focus rule](#channel-worktree-and-placement).

`--fresh` starts new sessions for the lane's closed named teams, with new handles and the same roles, channel, and working directory, using current team layouts. No team name is needed. Members pick up from `blackboard.md`, the `<stage>-notes.md` files, and the code rather than old conversations. Like [fresh cohort relaunch](#relaunch-into-a-named-worktree), it preserves the checkout and files. Standalone roots are skipped with deduplicated relaunch commands: `-w <worktree>` for a worktree lane, `--channel <channel>` for a project-root channel lane, and no place flag otherwise. A lane with live root members, only provider-discovered sessions and no saved shape, or no restorable team is refused. On this verb `--fresh` works with every scope spelling and `--bg`; it does not inherit the cohort launch flag's conflicts.

When RimZ's own records for a lane are gone, Claude and Codex sessions are recovered from the providers' local session stores: the newest concurrent set of sessions comes back, with exact session ids, as flat panes without roles or teams (those exist only in RimZ). Older, non-overlapping sessions stay closed and are reported by kind and session id. At the project root, the bare listing includes worktree lanes found only in those provider stores.

| Error | Meaning |
| --- | --- |
| `no lane '#docs' in this workspace` | The scope matches no lane. |
| `worktree for '#docs' was removed; recreate it with rimz agents <spec> -w docs` | The lane's checkout is gone. |
| `PR 69 has no local worktree; start one with rimz agents <spec> --from-pr 69` | No local worktree came from that pull request. |
| `nothing to resume in '#docs'` | Neither RimZ nor the Claude and Codex session stores hold a resumable session for the lane. |
| `cannot relaunch '#docs' fresh with live members @coder; stop them with rimz agents stop, or resume without --fresh` | Fresh relaunch would duplicate live members; the error names their handles. |
| `no saved team shape in '#docs'; resume without --fresh, or launch with rimz agents <spec> -w docs` | Fresh relaunch needs durable lane records; provider session files alone cannot reconstruct its shape. |
| `no saved team in '#docs' to relaunch fresh; launch with:` | No saved team can be restored; the error includes the standalone relaunch commands. |
| ``RimZ hooks are missing for claude account `default` at `<home>`; run `rimz hooks install claude` `` | An account a member would launch under fails the [account check](#the-account-check-at-launch); nothing is resumed. |

Resume planning is described in [fleet.md → Resume and rebirth](../../internals/harness/fleet.md#resume-and-rebirth).

## Inspect profiles and launch plans

### Validate definitions

```sh
rimz agents validate
rimz agents validate --json
```

Reads the machine Markdown definition trees without launching agents or generating files. Human output groups agents, subagents, and teams with their resolved seats, then prints source-attributed errors and warnings. JSON emits `{rows, errors, warnings}`; warnings carry `path` and `message`. Any error exits nonzero; warnings do not. Host definitions with a skill list and no provider switch warn that the list is unenforced. Listed skills are checked against the provider's skill root, then the shared skill library, even with host isolation. See [the format and failure classes](../definitions.md#validation-and-failures).

### Discover agent profiles

`rimz agents profiles` lists the configured direct definitions and launch commands as compact cards, with each profile's description. A nonempty permission-rule list adds `  allowed-tools: Bash(git *), Read` after the description; JSON includes `"allowed_tools": ["Bash(git *)", "Read"]`, omitted when unset or empty. Subagent and team profile listings show the same detail. Built-in and plugin agent kinds are launchable but not listed, and teams are listed by [`rimz teams list`](./teams.md#list-teams). Team role profiles, named `<team>.<role>` for a configured team, appear only with `--teams`; [`rimz teams profiles`](./teams.md#list-team-role-profiles) lists them alone.

```sh
rimz agents profiles
rimz agents profiles --path          # add each entry's defining file
rimz agents profiles --teams         # include team role profiles
rimz agents profiles --json --path
```

JSON includes `source` (`profile` or `command`) and, only with `--path`, the absolute `path`.

Tiered profiles show `model (tier)`, or `model (tier, fallback)` when resolution used a higher tier. Changing families within a tier is not a load-time fallback. JSON adds `tier` and `tier_fallback` only for tiered profiles; `model` and `effort` stay concrete. Validation rows use the same tier fields and fallback marker. `agents validate` warns once per routed definition and excluded family, naming the listed model and why that family cannot run it; warnings do not fail validation.

### Explain a launch

`rimz agents explain` prints the launch plan for one agent without launching it. It opens no pane, mints no name or launch id, writes no store record, and creates no prompt files, temp units, or skill copies.

```sh
rimz agents explain coder --model gpt-6-astra --effort high
rimz agents explain coder --tier senior
rimz agents explain forge.coder --json
rimz agents explain @coder
rimz agents explain coder --prompt > prompt.md
```

| Target | Describes |
| --- | --- |
| a profile or `<team>.<role>` | A fresh launch, resolved exactly as a launch would be. Works without a room. A multi-cell layout is refused. |
| an `@handle` | What `restart` would run for that recorded agent under the current profile configuration: a resume of the recorded conversation when supported, otherwise a fresh launch with the reason. A fresh-restart plan shows the recorded name and launch id, although a real restart allocates a new id and may pick a new name. Needs existing room state. |

With a profile target, `explain` accepts the launch overrides `--ask`, `--yolo`, `--model`, `--agent`, `--effort`, `--isolation`, `--system-prompt-file`, `--append-system-prompt-file`, `--budget`, and passthrough arguments after `--`, with the same precedence as a launch: `--ask` and `--yolo` replace even a profile's declared permission mode. The report's `overrides` list records the flags you passed, not that each one won. Passthrough arguments are repeated only when preserved in the provider argv; otherwise the list points to that argv instead of exposing replaced values, such as inline Claude settings. An `@handle` target refuses every override.

The default report shows:

- a routed profile's tier and chosen model, including a `--tier` override;
- the profile chain, including a replacement from `--agent` (`writer ← claude` becomes `writer ← codex`, or `writer ← fast ← codex` when the replacement is profile `fast`);
- the action, working directory, and effective settings;
- the provider argv and the wrapped argv;
- the launch's environment overrides and unset keys (not the whole ambient environment);
- prompt sources, the composed prompt text, and how the RimZ reminder is delivered;
- sandbox mounts, pins, skill copies, and omissions;
- the skill policy: sandbox view, host provider switch with unlisted keys, or an unenforced-list warning.

`--json` emits the same plan with the fields `isolation` (`host` or `sandbox`), `isolation_source` (`--isolation`, `recorded --isolation` for an `@handle` target, `profile default <name>`, or `machine policy`), `target`, `kind`, `action`, `action_note`, `name`, `launch_id`, `account`, `cwd`, `profile`, `overrides`, `mode`, `model`, `effort`, `budget`, `skills`, `program`, `provider_argv`, `argv`, `env`, `unset`, `redacted_keys`, `prompt`, `sandbox`, and `warnings`.

`prompt.sources` lists sources in composition order. File sources have `path` and `bytes`; the embedded team consensus has `builtin: "team consensus"` and `bytes`, with no `path`. A configured `consensus-file` is an ordinary file source. The human report labels the embedded source `source: built-in team consensus (N bytes)`, and adds `; read-only copy at <path>` when the [inspection copy](../definitions.md#the-built-in-consensus-copy) exists; the JSON shape does not carry that path.

`--prompt` prints only the composed configured system prompt, then two newlines and the RimZ reminder. It cannot show a provider's built-in instructions or prompt options passed as raw provider arguments (those appear in the argv report). When the provider has no channel for appended system text, it still prints the reminder and notes on stderr that the reminder is not delivered. `--prompt` and `--json` conflict; warnings go to stderr.

Explain applies the same checks as a launch. System-prompt files must be readable UTF-8, and configured sandbox and skill capabilities must pass their preflights: a failure is an error, never a partial plan. The launch-chain check is the exception: when a real launch from here would fail it (a subagent caller, the chain limit, or a caller identity RimZ cannot resolve), `explain` still prints the plan, without launch ancestry, and warns that a real launch from there would be refused. A profile whose permission posture is missing or incompatible is shown without it, with a warning.

Secrets from trusted project `[[agents]]` `env` values print as `<redacted>` in the environment map, the matching argv environment tokens, and sandbox pins. Redaction goes by environment key, not by value, so paths derived from a secret value stay visible in mounts, skill copies, warnings, and errors. Everything else prints in full, and there is no flag to reveal secrets. Sandbox planning uses the environment `explain` runs in plus the launch overrides, not the target pane's environment, so a different home, XDG, provider-home, or multiplexer variable can change the planned view.

### Register a third-party kind

`rimz agents register` scaffolds a machine-tier agent plugin, and `rimz agents check` validates one:

```sh
rimz agents register mybot                       # scaffold ~/.rimz/agents.d/mybot
rimz agents register --check                     # validate every configured plugin, creating nothing
rimz agents check mybot --replay events.jsonl    # validate one plugin and replay canonical envelopes
rimz agents check mybot --spend-file session.jsonl   # also run the spend probe on a transcript
```

The scaffold holds the manifest, a setup guide, the canonical forwarding shim, and stub probes. A valid plugin kind works anywhere a built-in kind does: inline layouts, profiles, teams, supervised runs, coverage, and messaging. The [agent plugin reference](../agent-plugins.md) defines the bundle and wire contracts.

## List and manage agents

```sh
rimz agents list                         # root-agent cards, current channel
rimz agents list '#auth-refresh'         # one lane's cards
rimz agents list --all                   # every channel in the room
rimz agents show swift-otter --capture   # full report plus the pane's visible text
rimz agents logs swift-otter -f          # follow the transcript
rimz agents history swift-otter -n 10    # per-turn tokens, cost, and outcome
rimz agents attribution --md             # lane credit for a pull request
rimz agents top --once --all             # one resource-ranked table of the room
rimz agents focus @claude-2#cli-docs     # jump to the pane
rimz agents fork @coder --name twin      # branch a conversation into a new agent
rimz agents wait otter fox --any         # race two agents; print the first finisher
rimz agents stop @claude --all           # stop every matching Claude in scope
rimz agents stop @coder --when-idle      # stop it once it has rested 3m with nothing owed
rimz agents restart @claude-2#cli-docs   # replace the pane and resume the session
rimz agents compact @coder               # compact context at the next turn boundary
```

| Verb | Acts on | Does |
| --- | --- | --- |
| `list` (aliases `ls`, `ps`) | the current channel; a scope; `--all` for the room | attention-ordered agent cards |
| `show` (alias `inspect`) | one agent, live or stopped | full report: activity, context, placement, run, messages, transcript tail |
| `logs` | one agent | transcript tail; `-f` follows |
| `history` | one agent, live or stopped | per-turn duration, tokens, cost, and outcome |
| `attribution` | the current lane; a scope; `--all` for the room | agent, model, time, token, and cost credit |
| `top` | live agents in the current channel; `-w` or `--all` | resource-ranked table |
| `focus` | one agent | jumps to its pane |
| `fork` | one root agent, live or stopped | opens a copy of its conversation as a new agent |
| `wait` | one or more runs or agents | blocks until all finish; `--any` returns on the first |
| `refresh` | one agent, the channel, or `--all` | re-reads card context now |
| `stop` | one run or agent; `--all` for every match | cancels the run or closes the pane |
| `restart` | one live agent | replaces its pane and resumes its session |
| `compact` | one agent with a bound pane | sends its native compaction command, with the brief for its seat, at a turn boundary |

`list`, `show`, `logs`, `history`, `attribution`, `top`, `focus`, `wait`, and `refresh` change no agent. `fork` starts a new agent and leaves its source untouched; `stop`, `restart`, and `compact` act on the agent. [Resume a lane by place](#resume-a-lane-by-place) covers `resume`, and the [Budget CLI](./budget.md#cap-one-agent) covers `budget`.

#### `list`

`rimz agents list [SCOPE]` prints the room's pane-backed root agents as cards in attention order, scoped to the current channel. `SCOPE` (`#channel`, worktree, branch, or directory name) or `-w, --worktree` selects one lane, and `--all` covers every channel; the three conflict. Listing needs the verb: bare `rimz agents` and `rimz agents '#auth'` refuse with exit 2 and print the `list` command that would run, carrying over `--all`, `--json`, `-w NAME`, and the scope. It needs a live room: enter one with `rimz start` or `rimz attach`.

```console
$ rimz agents list
HANDLE    PROFILE  AGENT   STATUS   MODEL         CTX  TOKENS  AGE

⑂ auth-refresh · forge team
@planner  planner  claude  waiting  opus@high     42%     78k   2m
  which rotation strategy should we use?

@coder    coder    codex   running  gpt-5.5@high  31%     54k   0s
  wire up the refresh-token path
```

Cards group under lane headers. `⑂` marks a worktree-backed lane, `#` a plain channel, and a bare label the room root; agents outside the project collect under a dim `external` header. A header adds `· <team> team` for a shared team and the lane's pull-request number and CI glyph when one is known. Glyphs follow the configured theme glyph set.

| Column | Shows |
| --- | --- |
| `HANDLE` | The shortest handle you can type under that header: the role (`@coder`), else the `--name` (`@writer`), else the profile when unique (`@planner`), else `@<kind>` when unique, else the pet name. Launched agents show the pet name rather than an ordinal. |
| `PROFILE` | The launch profile, or a faint `-` when absent. |
| `AGENT` | The provider kind, such as `claude` or `codex`; a named account includes its name, such as `claude@work`. |
| `STATUS` | The card status (below). |
| `MODEL` | Model and effort, as `model@effort`. |
| `CTX` | Context-window fill. |
| `TOKENS` | Session tokens. |
| `AGE` | Time since the agent was last seen active. |

Under each row, the agent's activity description (the line the sidebar card shows) wraps to at most three indented lines, ending in an ellipsis when cut.

| Status | Meaning |
| --- | --- |
| `running` | A turn is in progress. |
| `waiting` | The agent is waiting on you: a question, a permission prompt, or a plan to approve. |
| `idle` | At rest, including after an interrupted turn. |
| `success` | The last turn completed. |
| `failed` | The last turn failed. |
| `paused` | Parked by a provider limit or a budget cap. |
| `sleeping` | At rest with a one-shot delivery armed for its session in this workspace: a timer, PID, watched command, polled check, file watch, or a one-shot signal. Standing subscriptions do not count, and running, waiting, failed, paused, or a live child take precedence. |

The status is derived for display. [`rimz message --when`](./message.md) matches the raw lifecycle statuses (`running`, `waiting`, `idle`, `success`, `failed`) and never `paused` or `sleeping`. `show` adds the turn phase.

`--json` emits `{"schema":1,"agents":[...]}`. Each entry is the card the table renders, not the store's provider-shaped record. Except for `login`, every key is always present: an unknown scalar or object is `null`, a count is `0`, and a collection is empty.

| Field | Contents |
| --- | --- |
| `id`, `kind`, `handle`, `name`, `name_explicit`, `profile`, `role`, `team`, `mode` | Identity. |
| `login` | The agent's recorded named account as `kind@name`, such as `claude@work`; omitted for the default account. Switching the room default does not change this field for an existing agent. |
| `me` | `true` on at most one entry: the caller. RimZ matches `TMUX_PANE` or `ZELLIJ_PANE_ID` to the pane binding first, then the `RIMZ_AGENT_KIND`, `RIMZ_AGENT_NAME`, `RIMZ_AGENT_PROFILE`, and `RIMZ_AGENT_ROLE` launch identity. Outside an agent pane every entry is `false`. |
| `status`, `phase`, `turn_error`, `ask`, `unread`, `attention_score`, `description` | Projected activity. |
| `pending_waits` | Armed one-shot deliveries, each with its name, trigger, and optional arm timestamp. Trigger kinds include `check` with `command` and `file` with `path` and `grep`, alongside timers, PIDs, commands, and signals. Timers come first by due time, then watches and signals; names break ties. |
| `idle_stop` | The pending [`stop --when-idle`](#stop) request: `after_secs`, `requested_at`, `requested_by` (the asking agent's handle, omitted from a user shell), and `due_at` while the agent is resting and its clock runs. Omitted when none is pending. |
| `launch_warnings` | Warnings the launch wrapper printed for the current launch, in print order; empty after a clean launch. |
| `background_shells` | Background shells the session is running, each with its `id`, optional `command` and `description`, and `started_at`, when RimZ first saw it. Only Claude reports them; other agents leave the list empty. |
| `model` | `id`, `effort`, and the rendered `label`. |
| `context` | `fill_pct`, `used_tokens`, `window`, `severity`, `compactions` (completed), and `compacting` (in progress). |
| `stats` | The token split; `cost_usd` (the live session plus the pane-backed children it launched); `active_secs`, RimZ's estimate of active time across the seat's sessions and the pane-backed children it launched, matching `agents show`; `tool_calls`, a map of tool name to count; and `tool_repeat`, the open run of identical tool calls. |
| `timeline` | Registration, turn-start, activity, and observation timestamps. |
| `placement` | `channel`, `worktree`, `branch`, `pane`, and `pr`. A `pr` has `number`, `state` (`open`, `closed`, or `merged`), and `ci` (`pending`, `passing`, or `failing`). |
| `budget` | `cap` (the effective cap), `spent_usd`, `parked`, and the `park` label. |
| `sub_agents` | Nested children. Each child's optional `tokens` is `{"window": N}` for current context occupancy or `{"total": N}` for whole-run consumption; absent when neither is known. |

#### `show` / `inspect`

`rimz agents show <REF>` prints one agent's report in the sections Agent, Activity, Context, Placement, Run, Messages, and Recent transcript. It works for live agents and for stopped ones still in the audit record. To see what an agent would launch or restart with, use [`explain`](#explain-a-launch).

The Agent section includes an `account` line for a named account, as `kind@name`; the default account has no account line. It includes `launch_warnings` when the current launch printed warnings, one per line. A clean relaunch clears the entry; the warnings still print to stderr at launch.

Context cost, token split, and active time cover the agent's seat for its whole life: every resumed session plus the pane-backed children it launched. Addressing a child directly reports that child alone. Context fill, window, and tool activity describe the current session. An open run of identical tool calls appears once it reaches the configured warning threshold, as `Bash ×23, 4m`.

The Messages section lists your messages and agents' attributed messages. When it hides system messages, a faint line gives their count and the command that lists them, `rimz message list --all --system @<agent>`.

When the agent's temp directory exists, `show` prints its host path (`tmp:`) and notes that sandboxed panes see it at `/tmp`. A subagent's line names its parent's directory, which the two share. The directory survives a resumed restart, while a fresh restart mints a new handle and a new directory; its lifecycle is in [sandbox.md → Temp units](../../internals/sandbox.md#temp-units).

| Flag | Effect |
| --- | --- |
| `--capture` | Append a Capture section framing the pane's visible area, with the pane id in the top border. An agent without a bound pane is an error. |
| `--ansi` | Keep colors inside the capture frame. |
| `--json` | Emit the report: the [`list` entry](#list) under `agent`, plus `stale`, the full `ask`, `run`, `messages` (the same filtered list, without the hidden count), `capture`, and `tmp_dir` when they apply. A stopped agent keeps the same entry shape, including `login` for a named account, with live-only fields such as context severity `null`. Active time includes its seat and launched children while their records are retained; it is `null` only when none remain. A child addressed directly reports its own seat alone. |

#### `logs`

`rimz agents logs <REF>` prints one agent's transcript as a chat log, with the same scope and rendering as [`rimz transcript @ref`](./transcript.md). Human output hides subagent digests, waits, and `rimz`-authored prompts along with the output of the turns they open; JSON keeps them.

| Flag | Effect |
| --- | --- |
| `-n, --tail <N>` | Keep the last `N` chat lines. |
| `-f, --follow` | Print new lines as they land. Conflicts with `--all`. |
| `--all` | Include earlier sessions. |
| `--json` | Emit `{"entries": [...]}` with the [transcript entry fields](./transcript.md#json-output) and no `channel`, `focus`, or `archived_count`; with `--follow`, one entry per line. |

#### `history`

`rimz agents history <REF>` splits the session's provider transcript into turns, one per user message, and assigns each API response to its turn.

```console
$ rimz agents history @coder -n 3
START             DUR  TOKENS       COST     OUTCOME  PROMPT
2026-07-08 15:12   8m  ↘4k ↗510     $0.4210  done     implement refresh-token rotation
2026-07-08 15:38   3m  ↘1k ↗284     $0.2870  done     add coverage for expiry edge cases
2026-07-08 15:44  12s  ↘320 ↗0      $0.0310  open     run the focused integration tests
3 turns · 54k tokens · $0.7390
```

`TOKENS` shows fresh input and output. `OUTCOME` is best-effort: `done` means an assistant reply closed the turn, `open` is the live final turn, and `cut` means the turn or session ended without a reply. A response repeated under the same message and request id counts once. `-n, --tail <N>` keeps the newest `N` turns, and `--json` emits full records, adding cache-read tokens, cache-write tokens, and the API-call count.

A stopped agent's history stays readable while its provider transcript exists. Per-turn history needs an adapter with normalized transcript and spend coverage; [agent support](../agent-support.md) lists which have it.

#### `attribution`

`rimz agents attribution [SCOPE]` credits the root agents that worked a lane, including agents whose panes have already exited. Use it to credit a pull request: `--md` prints a block for the PR body. It reads the store and provider transcripts and needs no live multiplexer. How records are selected and folded is in [attribution.md](../../internals/agents/attribution.md).

| Flag | Effect |
| --- | --- |
| `SCOPE` | A `#channel`, worktree, path, or directory name, as for `list`. Default: the caller's current lane. A slash spelling names a lane, not a branch. |
| `--all` | Every lane in the room. |
| `--branch <BRANCH>` | Credit only agents observed on `BRANCH`. Default: the selected checkout's current branch. With `--all`, detached HEAD, or a scope spanning several checkouts, no branch filter applies unless you pass this flag. |
| `--md` | Emit a collapsed `<details>` block for a pull-request body. |
| `--json` | Emit the schema 7 document. Conflicts with `--md`. |

Which agents count:

- An agent counts in a lane while its checkout still exists. Removing a worktree removes its agents from the report; nothing is deleted, and `rimz agents show` still reports the session.
- A worktree RimZ created counts only agents registered at or after its creation, so a worktree removed and recreated under the same name credits only its current life. The header then shows `since <YYYY-MM-DD HH:MM>` in the configured time zone. A checkout RimZ did not create has no such boundary.
- Under a branch filter, a root agent counts only with recorded evidence of being on that branch; the children it launched follow it. Figures cover each counted agent's whole effort, so an agent seen on several branches reports its full figures under each.
- A child launched from a pane counts in its parent's lane, wherever its own pane ran, and drops out once its parent leaves the audit record.
- An agent that never opened a turn and has no recorded activity, messages, tokens, or cost is omitted. The panel and JSON keep an agent that opened a turn with no statistics; `--md` omits it and recomputes totals.
- An unreadable checkout or malformed worktree marker leaves its agents out and prints a warning on stderr naming the path.
- Several sessions of one contributor (after compaction or `/clear`) fold into one member when they share a team role, launch cell, explicit name, or pane; `sessions` counts them.

The panel names the applied `branch <name>` and any `since` boundary above the groups, then groups members by team. Each member line shows handle, provider, and model with effort, followed by labelled lines in the order `effort`, `subagents`, `activity`, `messages`, `tokens`; an unavailable figure's line or part is omitted.

- `effort` is active time and cost. Active time includes the seat and the pane-backed children it launched. Cost and tokens are all-in: the member plus every subagent it spawned, the same figure `agents show`, `teams show`, and the sidebar report.
- `subagents` breaks that spend down by task as `{count} × {task}, … · {cost}`; the cost is already inside `effort`. Tasks come from provider-reported types and launch profiles, and missing or description-like types group as `other`.
- `activity` leads with asks, then tool calls.
- `messages` reads `{n} from you · {n} from teammates · {n} to teammates`, excluding RimZ automation. Sent messages are credited by handle, so a message sent by an excluded session still credits the handle a counted session holds.

A `Models` section follows the members: one row per model id the transcripts named (`unknown` where none), each `{cost} · {token split}`. It splits the same all-in spend by model, subagents included, so a member's heading model (its latest session's) can differ from the rows. Rows sort by cost, highest first (unpriced last), then by tokens, then by model id. A `Total` line ends the panel, except that a lone team group omits it because its caption already carries the totals.

```console
$ rimz agents attribution
since 2026-09-06 21:26
forge team · 3 agents · 59m active · $42.73 · 10 messages (1 from you)

  @planner · Claude · claude-fable-5-1@high
      effort:    17m active · $9.66
      subagents: 2 × explorer, 1 × designer · $3.51
      activity:  46 tool calls
      messages:  1 from you · 3 from teammates · 7 to teammates
      tokens:    469.5k input, 83.9k output, 142.1k cache write, 7.6m cache read
[...]
```

`--md` emits the panel's figures and wording as a collapsed `<details>` block: a totals summary that links RimZ to its repository, member bullets under `**Agents**` as `` - **`{role or @handle}`** — {provider} `{model}@{effort}` ``, and model bullets under `**Models**` as `` - `{model id}` — {cost} · {token split} `` (with `unknown` outside the code span). Multiple groups have captions as `` **`{team}` team** `` or `**Other agents**`. Identities, team names, and model ids sit in code spans so those names never mention a GitHub user in a pasted footnote. It omits the `branch` and `since` labels, and an empty scope emits nothing.

The `--json` document has the keys `schema` (`7`), `generated_at`, `rimz_version`, `scope`, `groups`, `models`, and `totals`. A missing active-time or cost figure is `null`, never zero or wall-clock time. Lists arrive in rendered order.

| Object | Fields |
| --- | --- |
| `scope` | `selector`, `channel`, `branch` (the applied filter or `null`), `worktree`, `since` (RFC 3339 boundary or `null`) |
| group | `team` (`name` and launch-ordered `roles`, or `null`), `totals`, `members` |
| member | `handle`, `role`, `name`, `kind`, `provider`, `model`, `effort`, `presence` (`live` or `exited`), `me`, `launch_ordinal`, `sessions`, `registered_at`, `last_activity`, `active_secs`, `asks`, `asks_answered`, `tool_calls`, `compactions`, `messages`, `tokens`, `cost_usd`, `subagents`, `models` |
| `messages` | `from_user`, `from_teammates`, `to_teammates` |
| `tokens` | `input`, `output`, `cache_write`, `cache_read` |
| subagent group | `task`, `count`, `cost_usd` |
| model row | `model` (`null` when unnamed), `tokens`, `cost_usd` |
| `totals` (group and document) | `agents`, `active_secs`, `wall_clock_secs`, `cost_usd`, `asks`, `asks_answered`, `tool_calls`, `compactions`, `messages`, `tokens` |

`cost_usd`, `tokens`, and `active_secs` are all-in at every level; asks, tool calls, compactions, and messages count the member's own seat. `sessions` counts the seat's own sessions, never children. A member's `models` splits that member's figures; the document's `models` is the list the panel and `--md` render, and groups carry none. Active time comes from per-session sidecars that `rimz gc` removes after its runtime retention (`gc.older_than`, 7 days by default); a seat with no retained active-time records keeps its other figures with `active_secs` set to `null`.

#### `top`

`rimz agents top` ranks live pane-backed agents, with launched children nested under their parent, by the resources their pane's process tree uses.

```console
$ rimz agents top --once
4 agents · 2 running · 143% CPU · 1.9G MEM · 248k tokens
AGENT         STATUS   CPU   MEM  IO/S  PROCS  CTX  TOKENS  AGE
@coder        running  96%  892M  4M/s     12  31%     54k  0s
@planner      waiting   2%  410M     -      6  42%     78k  2m
```

| Flag | Effect |
| --- | --- |
| `--once` | Take two samples 500 ms apart, print one table, and exit. Without it, `top` refreshes until you quit. |
| `--interval <DURATION>` | Refresh interval (`2s`, `500ms`, or bare seconds). Default 2 seconds. |
| `--all` | Every channel in the room instead of the current channel. |
| `-w, --worktree <NAME>` | One worktree or channel. Conflicts with `--all`. |

Resource columns read `-` where process metrics are unavailable for the platform or pane; context and token columns still render.

#### `focus`

`rimz agents focus <REF>` moves your multiplexer focus to the agent's pane.

#### `fork`

`rimz agents fork <REF>` opens a new agent holding a copy of another agent's full conversation, under a new session id, and leaves the source untouched. It resolves a live agent in the current channel first, then a stopped agent in the audit record.

```sh
rimz agents fork @coder --name twin
```

The fork reuses the source profile's launch posture (system prompt files, model, effort, permission flags, and profile arguments), rendered from current configuration as `restart` does. A profile that no longer resolves forks without it and warns; a profile that now names a different provider refuses. The fork keeps the source's channel but not its team or role, so the role handle stays unique. Prompt caching is up to the provider and may start cold. A fork refuses when the account fails the [account check](#the-account-check-at-launch).

The fork always opens in the source agent's worktree. From a user shell it takes over the current pane by default; `--new-pane` splits the current tab, `--new-tab` opens a new one, and `--bg` splits and keeps focus where it is. Agent callers split by default and preserve focus under the [focus rule](#channel-worktree-and-placement). `-n, --name <NAME>` sets its handle. It keeps the source's recorded `--isolation`, and `--isolation host|sandbox` replaces it. A fork takes no prompt: send the first instruction with `rimz message` once it opens.

| Agent | Native fork command |
| --- | --- |
| Claude | `claude --resume <id> --fork-session` |
| Codex | `codex fork <id>` |
| Droid | `droid --fork <id>` |
| Grok | `grok --resume <id> --fork-session` |
| OpenCode | `opencode --session <id> --fork` |
| Pi | `pi --fork <id>` |
| Qwen | `qwen --resume <id> --fork-session` |

Other kinds, including plugins, cannot fork.

#### `wait`

Immediately after a prompted peer launch, `rimz agents wait @peer` resolves the pending launch run even before provider registration. After a later launcher message, it retains the ordinary turn wait until a prompt-correlated turn-start hook enrolls the new run. Use `rimz message --wait @peer '<text>'` for a message-correlated reply wait. A turn-start hook without prompt text acknowledges delivery but opens no reportable turn.

`rimz agents wait <REF>...` blocks until supervised runs finish or interactive agents finish a turn: `idle`, `success`, or `failed` after a turn has opened. A sleeping agent (one with an armed one-shot wait) and a freshly registered agent that never opened a turn keep the wait blocked. A reference is a run id, a pet name, or any address.

With one reference, `wait` prints the final assistant message: a run's answer, or the message that closed an interactive agent's turn. When a completed turn left no message, stderr says so and stdout stays empty. `--json` prints a run's full record, or for an interactive agent one `{status, exit, cost, transcript_path, last_message}` entry, omitting fields that have no value. `--stream` tails assistant text as it lands, `--stream --json` emits NDJSON run events, and `--from-start` replays the transcript from the top before tailing.

Several references form a join. Text mode prints each final answer in completion order under a `--- <name> ---` header, rendered by the [agent-prose rule](../cli.md#agent-prose), and adds a status suffix only to abnormal results; stderr diagnostics carry the same header. `--json` prints one map after every target settles, `{name: {status, exit, cost, transcript_path, last_message, error}}`, omitting `cost`, `transcript_path`, `last_message`, and `error` when they have no value. `--stream` accepts one reference.

| Flag | Effect |
| --- | --- |
| `--any` | Return when the first target finishes, successfully or not, printing its answer block (or a one-entry map with `--json`). The other targets keep running. Conflicts with `--stream`. |
| `--timeout <DURATION>` | Cap the whole wait. On expiry `wait` exits `124` and leaves the targets running; text mode names each unfinished target on stderr as `--- <name> (timed out) ---`, and JSON mode marks them `timed_out`. A single reference reports the same way, its `--json` being the one-entry result map. |
| `--stream` | Tail the transcript while waiting. One reference only. |
| `--from-start` | With `--stream`, replay from the top first. |
| `--json` | Labeled result map; with `--stream`, NDJSON run events. |

The exit code is `0` when every target completes; otherwise it is the status code of the first target, in argument order, that did not complete ([exit codes](#supervised-runs--p)). With `--any`, it is the winner's code. [`rimz subagents wait`](./subagents.md) runs the same join restricted to the caller's own children.

#### `refresh`

`rimz agents refresh [REF]` re-reads card context now instead of waiting for the next change. It re-reads the transcript tail, re-checks a Codex turn that looks dead against its live pane, starts the kind's rich-context helper when one exists, and wakes the sidebars. With a reference it refreshes exactly one agent; without one, every live root agent in the current channel; with `--all` (no reference), every live root agent in the workspace.

#### `stop`

`rimz agents stop <REF>` ends an agent's CLI process the way Ctrl+C would. For a live supervised run it cancels the run; for a completed run kept open with `--keep` it reclaims the pane; for an agent it closes the pane. The provider's session files stay on disk, so a stopped agent is one [`--resume`](#resume-agents) away.

A stop succeeds only when the agent's own pane is gone. A pane the multiplexer still lists after its close failed, or one whose state could not be read and whose close failed, fails the stop with exit `1` and `<handle> was not stopped: <failures>`; the session keeps its loop rows and any pending idle stop, and rerunning the stop retries the close. When the agent's pane closed but something else failed (a child's pane stayed open, or the session's rows could not be retired), the stop exits `1` with `<handle> stopped, but: <failures>`. Each failure starts with the handle it concerns.

An interactive peer still takes the pane-close path when it has a launcher-opened run, whether that run is open or already terminal. An open turn reports failure to its launcher when session-end or the backstop observes the stop; an idle peer with no open turn reports nothing.

Stopping a parent first stops its live children launched with `rimz subagents`, including when [`rimz teams stop`](./teams.md#drive-a-live-team) reaches the parent. Without `--all`, the reference must match one agent. With `--all`, `stop` acts on every match and prints `stopped <handle>` for each agent whose pane closed and `error <handle>: <failures>` for each one with a failure; an agent that closed while a child or its retirement failed gets both lines. It exits `1` if any agent had a failure.

`rimz agents stop <REF> --when-idle [DURATION|off]` asks for the same stop later, once the agent is finished rather than now. It records the request, prints one line, and returns at once with exit `0`; the room stops the agent after it has rested for the duration with nothing owed to it. An agent can pass `@me` to end its own session when its work is done.

| Form | Does |
| --- | --- |
| `--when-idle` | Stop after `3m` of rest. |
| `--when-idle 10m`, `--when-idle=10m` | Stop after the given rest: a whole number with `s`, `m`, `h`, or `d`. `0s` stops at the first check that finds nothing owed. |
| `--when-idle off` | Withdraw the pending request; with none pending it says so and still exits `0`. |

The reference comes before the flag: in `rimz agents stop --when-idle @me` the flag takes `@me` as its duration and the command fails with that fix. `--when-idle` conflicts with `--all` and needs one live agent; an ended one refuses with `rimz agents resume` as the fix. A reference that resolves only to a run refuses and names the plain `stop`, and a provider's own subagent refuses because it ends with its parent. A second request for the same agent replaces the first, and its clock starts over.

The agent stops only when all of this holds at one check:

- its turn has ended cleanly, and it is not waiting on your input, parked on a budget or provider limit, compacting, or running background work;
- nothing is owed to it: no armed wait, no subagent or launched team still to report, no message queued or scheduled for it, and no open supervised run or peer turn, so a soft stop never cancels a run;
- every live `rimz subagents` child the stop would close with it, at any depth, passes those same two checks. A child needs no rest of its own, but a message queued for a child you kept holds its parent until the message lands. A provider's own subagent is not checked: its row can read as working after it finished;
- stopping it and those children would not cancel a run that is still open;
- for a team seat, the team's board reads `Done`. A seat whose checkout has no board is held;
- it has rested for the whole duration, never a fraction of a second less, measured from the latest of the request, its last turn end or interruption, and the session's last recorded event. A new turn before then restarts the clock, and so does a compaction that failed.

Timing is best effort. The check runs on the room's refresh, which a running sidebar drives, so the stop comes after the duration, never at a wall-clock instant. While pending, the request shows as `idle_stop` in `rimz agents show`, in the `show` and `list` JSON, and as a `stop` entry under the card's `⧖ waits`. It ends with the session: a plain `stop`, any `restart` (one that resumes the same session included), or the agent exiting removes it, [`rimz reset`](./maintenance.md#reset-a-wedged-room) drops every pending request so a resumed agent never inherits one, and a stop that leaves the agent's pane open keeps it armed for the next check. A stop that closed the agent but failed elsewhere, on a child's pane for one, removes it with the session. Each attempt is recorded as an `idle_stop` assist in [`rimz stats`](./stats.md#the-assist-timeline).

A soft-stopped agent comes back like any stopped one: `rimz agents resume` reopens its place and resumes the provider session. Nothing else revives it: a message or a team stage flip addressed to a stopped solo agent or team seat does not reopen it. A stopped `rimz subagents` child is the exception, since [a message from its parent](./subagents.md#follow-up-messages) resumes its session where the provider supports it.

#### `restart`

`rimz agents restart <REF>` replaces one live agent's pane and resumes its provider session. From a user shell, it focuses the pane, opens the replacement in the same layout position, and closes the old pane; focus follows the replacement on Zellij and tmux. An agent caller opens the replacement beside the old pane without jumping to it, under the [focus rule](#channel-worktree-and-placement).

A peer's open launcher-requested turn is failed before the replacement pane opens and becomes eligible for its launcher's fleet report. The replacement does not continue that turn's run.

The replacement renders the agent's profile from current configuration, including its isolation default, then replays its saved model, effort, and `--agent` base. It keeps its role, team, channel, permission mode, and recorded `--isolation`; passthrough arguments do not carry over. A profile that now resolves to a different provider without a saved base restoring the original kind makes restart refuse and point to an explicit fresh launch. Restart also refuses, leaving the old pane and its session as they were, when the account fails the [account check](#the-account-check-at-launch).

When the provider has no resume command or no recorded conversation, restart launches fresh and prints `restarted fresh as @<name> — <reason>`. The fresh agent may get a new name, because the old card still holds its handle while the replacement starts.

Closing the old pane ends the replaced session, so its session-pinned loop rows retire with it. A restart that resumes keeps the same session id and is a continuation, so its declared [team bindings](../../internals/harness/loops.md#team-bindings) carry straight over to the replacement; a restart that launches fresh leaves the old identity behind and retires its rows whole, and the replacement's registration re-arms the role's bindings under the new session. Either way, waits the agent armed for itself are gone, and the result line ends with `; <n> armed wait(s) dropped` when any were, also when the retirement then reports an error on stderr. A restart that fails before the old pane closes leaves every row intact.

#### `compact`

`rimz agents compact <REF> [INSTRUCTION]` queues the agent's native context-compaction command and delivers it at a turn boundary: at once for an idle agent, or when a running or waiting agent's turn ends. There is no way to force it into a live turn.

| Output | Meaning |
| --- | --- |
| `compacting @handle (msg_...)` | Sent. The command was submitted; compaction may still be running. |
| `queued compaction for @handle (msg_...) — @handle is <status>; delivers at its next turn boundary` | Queued until the turn ends. |

Without `INSTRUCTION`, the command uses the [`[harness] compact_instruction`](../../guide/configuration.md#smart-compaction) brief. An instruction replaces that brief on adapters that accept trailing text (Claude, Grok, Droid), and `""` sends the bare command. Other adapters refuse any instruction, including `""`; rerun without one.

`INSTRUCTION` may start with `-`; a value that is itself one of the command's flags (`-h`, `--help`, or a global flag such as `--color`) needs `--` before it.

The command refuses, printing the reason on stderr with empty stdout and exit `1`, when the agent:

- is compacting now, already has a compaction queued, or has taken no user turn since its last compaction;
- has no bound pane or no native compaction command;
- has no native turn-start hook (a plugin that declares no `turn_start` event), because RimZ could not guarantee a compaction never follows a compaction; use the native command in the agent's pane.

Each compaction is recorded as a message and an audit event, as an operator action rather than an automation assist.
