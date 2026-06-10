# Public CLI

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The CLI is the one public API every participant speaks — humans, scripts, sidebars, hooks, and resolvers. Command behaviour is stable and additive.

Commands group by intent below. Each cluster opens with its common flow, so you read by what you want to do. Internal commands that hooks and the sidebar invoke for you come last.

## Your first session

One repo maps to one room: a multiplexer session with a sidebar, backed by one ledger. Every command writes to or reads from that room.

```sh
cd ~/code/query-engine
rimz                                                # open or reattach the room

rimz event emit --kind build.started --title web   # any script can post
rimz feed ask --title "Promote staging → prod?" \
              --options yes,no --timeout 1h         # blocks until answered

rimz remote connect dev-box:query-engine          # reattach from anywhere over ssh
rimz agents claude codex --worktree                 # launch agents in fresh worktree tabs
```

That is the whole loop. For the five-minute tour and the why, see [the product guide](../guide/product.md); for the model that shapes the surface, see [DESIGN.md](../../DESIGN.md).

## Start and attach a workspace

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [PATH]
rimz start [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [PATH]
rimz attach [--attach|--no-attach|--print] [--no-resume] [--refresh-ms <ms>] [SESSION]
rimz list [--all] [--json]        # running + recently-active workspaces; --all adds dormant ones
rimz setup [--yes] [--force]      # first-run environment report and default config bootstrap
rimz doctor [--audit]             # diagnose backend, hooks, trust, resolvers
```

`rimz` (or `rimz start`) resolves the workspace root, finds or creates the multiplexer session, launches the native sidebar pane, and enters the session. On an interactive TTY it attaches; otherwise it prints the attach command. `--attach` forces attaching; `--no-attach` and `--print` force printing — the rule is [interactive attach is opportunistic](../../DESIGN.md#commitments). Run from inside a session of the selected backend, `rimz` reports the directory's room and exits without nesting — a same-mux room can't be nested; detach or run from outside to (re)launch. The first interactive run on a machine can offer to install detected agent hooks through an inline consent gate; Space toggles agents, `d` shows the real unified config diff, Enter installs the selected agents, and `s`/Esc skips. Non-interactive starts install nothing.

The root resolves through one ladder, richest tier first: an explicit `--root`, the enclosing git repo (worktrees collapse to the repo, so every worktree shares the repo's room), a project-marker directory (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `.rimz/config.toml`, …), and finally the directory itself — a first-class **directory workspace**, the room a headless box of agents gets with no source control. A non-repo root names itself with one stderr line at start, and the directory tier refuses exactly two roots as almost-certainly accidental — `$HOME` and `/` — with `--root` as the deliberate override. Starting a room whose root nests inside (or contains) another *live* room adds a one-line overlap notice: overlap is legal — an agent belongs to the room its pane lives in — and `rimz doctor` shows the standing room tree.

Inside a room, identity is pinned: session birth stamps `RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT` into the session environment, and participating commands — agent hooks, `rimz tab`/`agents`, `rimz event`/`feed`, the statusline helpers — resolve through the verified pin before the static ladder (an explicit `--root` beats both), so an agent working in a nested repo inside a directory room writes to the room it lives in ([hooks.md](../internals/hooks.md#hooks-resolve-the-room-they-live-in)). Room-choosing commands (`rimz start`/`attach`) resolve fresh, keeping a deliberate per-repo room one `rimz start` away.

When a session is reborn — after a reboot, a multiplexer crash, or a reset — Rimz re-seeds the prior agents it remembers, each restored idle in its own pane (`claude --resume`, `codex resume`); the conversation is back, no tokens spent until you type. `--no-resume` comes up empty for a deliberately fresh start (`resume.on_rebirth` in [configuration.md](./configuration.md) is the persistent switch). `--refresh-ms <ms>` overrides the sidebar render grid for sidebars spawned by that launch; persistent cadence stays in `[sidebar].refresh_ms`. Details: [resume-on-rebirth](../internals/sidebar.md#resume-on-rebirth).

`rimz attach <session>` reattaches by exact session name; `rimz attach` with no name uses the cwd's workspace. `rimz list` joins each known workspace against the live Zellij and tmux sessions so you see which mux currently hosts it, running first.

Remote rooms have their own command group:

```sh
rimz remote add <name> <target> [--no-reconnect] [--no-resume] [--mux <name>]
rimz remote connect <alias|target> [--reset] [--no-reconnect] [--attach|--print]
rimz remote reset <alias|target> [--no-reconnect] [--attach|--print]
rimz remote del <name>      # alias: rm
rimz remote rename <old> <new>
rimz remote list [--json]   # alias: ls
```

`rimz remote connect [user@]host:<target>` attaches a room on another machine: rimz builds the guarded `ssh -t` invocation, the host's own `rimz` starts or reattaches the room, and it renders in your terminal — sidebar, feed, and all. A `<target>` containing `/` (or starting with `~`) is a path the host starts a room for (`dev-box:~/code/query-engine`); a bare word is a session name to reattach (`dev-box:query-engine`); IPv6 hosts keep their brackets (`user@[::1]:…`). The same attach rule applies — an interactive TTY connects, anything else prints the full ssh command (`--print` needs no local ssh) — and `~/.ssh/config` aliases, ports, keys, and jump hosts apply as-is because rimz runs your `ssh`. The remote snippet repairs a non-login shell's PATH and, when the host has no `rimz`, fails with the install command instead of a bare `command not found`.

`rimz remote add prod agent@prod-box:query-engine` saves a named alias in `~/.config/rimz/remote.toml`; `rimz remote connect prod` resolves it. `--mux <name>` on `remote add` or on `remote` before `add` pins that alias's remote backend, while a top-level `rimz --mux <name> remote add …` is only this invocation's backend override and is not saved. A connect positional containing `:` is always a raw target, and every other value is an alias. Alias defaults (`reconnect`, `no_resume`, `mux`) are overlaid by connect flags: `--no-reconnect` hands the link to a single ssh run, and `--reset` / `rimz remote reset` passes `--no-resume` to the remote `rimz` for a fresh remote room.

A dropped link reconnects by itself when reconnect is enabled. Keepalives (`ServerAliveInterval=5`, three strikes) detect a dead link in about fifteen seconds, and rimz reattaches with capped exponential backoff — the remote room survives the drop by design, so pickup is where you left it. While supervised, rimz also opens a ControlMaster probe stream over the same SSH connection and publishes a sidebar footer badge (`⇅ 42ms 0%`, or `⇅ ?` when stale), local lost/restored notifications for confirmed dead links, terminal-local stalled-link banners for probe blackouts, and remote degraded/recovered notifications while the stream is still alive; `RIMZ_REMOTE_PROBE_MS=0` disables that enrichment. A clean detach ends the session, a first connection that fails (auth, unknown host) surfaces immediately rather than looping a password prompt, and a remote failure that isn't a link drop reports the remote's own error. Details live in [remote.md](../internals/remote.md).

`rimz doctor` reports the backend, installed hooks, trust state, enrolled resolvers, recent sidebar diagnostics, and the machine's room tree — every recorded workspace with its root, root class, and liveness, the current directory's room starred and nesting live rooms flagged — and names the fix for anything misconfigured. Run it first when something looks wrong.

## Configure your machine

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
```

`rimz setup` detects the active multiplexer, current workspace, agent binaries, hook status, trust state, and per-machine config path. In a terminal it offers to write the default per-machine config. `--yes` is the non-interactive path: it writes the default config only, with no hook installs or trust grants.

`rimz config init --print` prints the authoritative commented config template. `rimz config init` writes it to `~/.config/rimz/config.toml`, refusing to replace an existing file unless `--force` is present. `path` prints the resolved file path.

`get` loads the effective per-machine config over defaults. With no key it prints the whole config; with a dotted key such as `sidebar.max_cols` it prints that value. `--json` emits machine-readable JSON. `set` edits one dotted key while preserving comments, rejects unknown keys, validates the resulting TOML against `MachineConfig`, and writes atomically.

## Run agents in tabs and worktrees

```sh
rimz worktree new [NAME] [--base <head|fresh|ref>] [--branch <name>]
rimz worktree list [--json]
rimz worktree remove <name> [--force]

rimz tab [--layout <name|spec>] [--worktree [NAME]] [--name <title>] [--prompt <text>] [--no-focus]
rimz agents <KIND>... [--worktree [NAME]] [--prompt <text>] [--no-focus]
rimz run [--agent <KIND>] [--worktree [NAME]] [--ask|--yolo] [--timeout <duration>] [--keep] [--detach|--json|--stream] <prompt>
rimz run status <run-id> [--json]
rimz run list [--json]
rimz run stop <run-id>
rimz run send <run-id> [--enter] -- <text>
rimz run stream <run-id> [--from-start] [--timeout <duration>]
rimz steer <target> [--worktree <name>] [--no-enter] [--force] -- <text>
rimz queue <target> [--worktree <name>] [--on done|any] [--no-enter] -- <text>
rimz queue list [--json] [target]
rimz queue remove <message-id>
rimz queue clear <target> [--worktree <name>]
```

`rimz worktree` manages only Rimz-marked Git worktrees. New worktrees live under `[worktree] dir` (default `../{repo}-worktrees/<name>`), branch from `[worktree] base`, and carry their marker in the worktree's Git admin directory so the checkout stays untouched. `remove` refuses dirty worktrees or commits not yet merged into their base unless `--force` is explicit.

`rimz tab` opens one Zellij tab or tmux window in the current room. `--layout` accepts a named `[agents.layouts]` entry or an inline spec: commas split columns, plus signs stack rows, and cells are agent kinds (`claude`, `codex`, `pi`) or `term`; the built-in `peer` layout is `claude,codex`, named layouts may attach per-agent launch flags, and no layout is one terminal.

`--worktree` creates or reuses a Rimz-owned worktree and runs every cell in it. A bare `--worktree` creates a fresh generated name; `--worktree demo` reuses `demo` when marked or creates it. New worktree branches use the worktree name directly unless `rimz worktree new --branch` overrides it. `rimz tab` defaults to the cwd basename as the view title; `rimz tab --worktree demo` defaults to `⑂ demo`; `--name` overrides the title. `--prompt` is passed to agent cells; `term` cells run your shell.

Worktree launchers require a repo-backed room. In a marker or directory room, `rimz tab --worktree <name>` and `rimz agents ... --worktree <name>` fail at launch with `--worktree requires a git repository-backed room`; plain `rimz tab` and `rimz agents` run in the room root. Because `rimz tab` and `rimz agents` are participating commands, running them from `/tmp` inside a repo room still targets the pinned repo room and creates or reuses the named worktree there; running them from `/tmp` outside any room resolves `/tmp` as a directory workspace, so `--worktree` fails.

`rimz agents` is launcher sugar: each positional kind opens its own single-agent tab. Repeating a kind opens a fleet. Bare `--worktree` creates one fresh worktree per agent; a named worktree is shared by all launched agents. Details and cleanup state machine: [internals/worktrees.md](../internals/worktrees.md).

`rimz run` launches one interactive agent in the room, waits for that agent's root turn to finish, prints the final assistant message, and exits with the run status code (`0` completed, `1` failed, `124` timed out, `130` canceled). The command requires the selected agent's Rimz hooks to be installed and trusted, because hooks are the completion signal; it refuses an unwired explicit agent instead of guessing from pane exit. Omitting `--agent` selects the first registered agent whose hooks are installed, trusted, and whose binary is on PATH; `--agent <kind>` pins the choice. Default permissions accept edits where the adapter has that mode, `--ask` leaves the provider's prompts in place, and `--yolo` passes the adapter's explicit bypass flag. `--detach` prints the run id and returns; unless `--keep` is set, the launched wrapper closes its pane after the run record reaches a terminal status. `--json` on a blocking run prints the terminal run record instead of only `last_message`; `--stream` prints NDJSON events until the run ends. `status` and `list` read the current workspace's retained run records; `status` joins live agent phase and pending ask from the cached sidebar snapshot when the run is still active. A run that stops on a question stays a live agent in the room: the item takes the normal resolver-then-human path, and the blocked `rimz run` resumes the moment it is answered. `--timeout` accepts durations like `30s`, `5m`, `1h`, `1d`; without it the command waits as long as the run takes. The unattended pattern — cron launch, resolver chain, human fallback — is in [the product guide](../guide/product.md#unattended-runs). Detail: [internals/run.md](../internals/run.md).

`rimz run stop <run-id>` marks an active run `canceled`, wakes any blocked waiter, and closes the run pane when it can, including runs launched with `--keep`; stopping an already-terminal run exits successfully and reports that prior status on stderr.

`rimz run send <run-id> [--enter] -- <text>` sends text to the run's pane through the public pane-send primitive and appends Enter when `--enter` is present. It fails fast for terminal runs and for runs whose pane has not bound yet. For scripts that inspect before they type, use the capture-before-send discipline from [resolvers.md](../internals/resolvers.md#pane-send-resolver).

`rimz run --stream "<prompt>"` and `rimz run stream <run-id>` emit NDJSON: `message` events for assistant progress, `status` events when the live state changes, and one `end` event with the terminal status and `last_message`. Message events are interim progress; `end.last_message` is the deliverable, and it may duplicate the final message event. Message events come from Claude and Codex transcript shapes today; adapters without a stream parser still emit status and terminal end events. `rimz run stream <run-id>` attaches by polling the retained run record and transcript file, so it does not steal a blocked producer's wakeup socket. Its `--timeout` stops watching and exits `124` without writing `timed_out` to the run record.

## Steer and queue agent messages

`TARGET` is a normalized pane id (`tmux:%1`, `zellij:terminal_3`), a known agent kind (`claude`, `codex`, `pi`), or an agent session id or unique session-id prefix. Kind and session targets must resolve to one root agent; ambiguity and misses print candidate sessions. `--worktree <name>` filters kind/session matches by worktree branch, basename, or path.

`rimz steer` types human-authored text into a live agent pane immediately and appends Enter unless `--no-enter` is present. It refuses when a pending ask is attached to the agent, because the next input belongs to that ask; `--force` records the override and sends anyway. The audit event records kind, session id, pane id, force flag, and text length, never message content.

`rimz queue` stores text durably for one agent and delivers FIFO when the agent reaches the gate. `--on done` delivers after `idle` or `success`; `--on any` also delivers after `failed`; `running`, `waiting`, and `paused` keep the message pending. Hooks are the delivery signal, so `queue add` requires that agent's hooks to be installed and trusted before accepting a message.

Delivery happens one message per unparked turn end. The helper waits briefly for the pane composer to settle, re-checks the ledger snapshot, skips delivery while a pending ask is attached, claims the pending head, then sends through the pane primitive and marks the message delivered. Failed sends return to `pending` with an attempt count and become `abandoned` after the retry cap; a helper crash leaves a visible `claimed` record rather than auto-redelivering. `queue list` shows durable records, `remove` moves one open record to `removed`, and `clear` removes every open record for the resolved agent. Detail: [internals/messages.md](../internals/messages.md).

## Publish events and ask questions

```sh
rimz event emit --kind <kind> [--title <s>] [--body <s>] [--json <payload>]
rimz feed push --kind <kind> --title <s> [--body <s>]
rimz feed ask  --title <s> --options <a,b,c> [--timeout <duration>] [--no-block]
rimz feed list [--json] [--audit]        # alias: ls
rimz feed show <request-id> [--json]
```

`event emit` posts a fire-and-forget signal to the event log. `feed push` posts a non-blocking item to the feed. `feed ask` posts a question and blocks until someone answers or the timeout fires; while it waits, the question shows in the sidebar with its options as answer buttons.

`feed list` is a runtime view that drops items whose owner process is gone; `--audit` shows the durable history as written. `feed show` is always an exact audit lookup.

A deploy gate, the canonical script flow:

```sh
rimz feed ask \
  --title "Promote build 2026.05.18-rc.4 to prod?" \
  --options yes,no,abort \
  --timeout 4h
```

## Answer and triage

`feed resolve` is the only verb that delivers a decision. The other two are non-answers with distinct meanings.

| Verb | Valid surfaces | Meaning |
| --- | --- | --- |
| `feed resolve` | `bridge`, `script` | Deliver a decision through Rimz to a waiting hook or script. |
| `feed dismiss` | `native_ui` | Local acknowledgement only. The agent is unanswered — focus its pane and answer there. |
| `feed abstain` | `bridge`, `script` | The active resolver declines; the chain advances to the next link. |

```sh
rimz feed resolve <request-id> --decision '{"choice":"yes"}' \
                  [--resolver-id <id>] [--method <method>] [--override-chain]
rimz feed dismiss  <request-id> [--reason <text>]
rimz feed abstain  <request-id> --resolver-id <id> [--reason <text>]
```

Each resolution records its `--method` in the ledger: `hook_bridge` (a resolver returned decision JSON the hook printed), `pane_send` (a resolver typed the answer into the pane), `cli` (a human ran `feed resolve`), or `sidebar` (a human clicked through the UI). The three surfaces are defined in [the three operating paths](../../DESIGN.md#the-three-operating-paths).

## Drive panes

```sh
rimz pane split                                  # split the current view; inherits the workspace env
rimz pane focus <pane-id> [--session-name <name>] [--pane-process-start <ts>]
rimz pane list [--json] [--session-name <name>]
rimz pane capture <pane-id> [--lines N] [--json] [--ansi]
rimz pane send <pane-id> [--enter] [--key <key>]... [--] [text]
rimz pane detach [--session-name <name>]
```

`capture` and `send` are the universal answer surface: resolvers use them to answer prompts on tools that expose no hook protocol, and humans use them for direct pane control. `--key` accepts `enter`, `escape`, `tab`, `backspace`, `up`, `down`, `left`, `right`, `ctrl-c`, `ctrl-d`, and `ctrl-u`; repeat it to press several keys. `--enter` appends an Enter key after text and explicit keys. Captured pane text is untrusted data — a resolver matches it against its own bounded patterns, never replaying it as an instruction. Detail in [resolvers.md](../internals/resolvers.md). `detach` drops the attached client and leaves the session running; client semantics differ per backend ([multiplexers.md](../internals/multiplexers.md)).

## Enrol resolvers

```sh
rimz resolver add <id> [--order <n>] [--budget <duration>] \
                       [--binary <path>] [--display-name <name>]
rimz resolver remove   <id>
rimz resolver list     [--json]            # alias: ls
rimz resolver reorder  <id> [--before <other-id> | --after <other-id>]
```

Resolvers form an ordered chain that ends with you. Each entry carries its own `--budget`, so a fast LLM policy, a Slack-to-human bot, and a PagerDuty escalator chain naturally. Full protocol in [resolvers.md](../internals/resolvers.md); trust model in [security.md](../guide/security.md).

## Maintain the room

```sh
rimz reset [--yes] [--no-start] [PATH]   # destroy a wedged room and rebuild it clean
rimz reload                              # converge every running sidebar to a healthy set
rimz gc [--older-than <duration>]        # sweep stale liveness hints, dead-owner items, orphan queued messages, landed marked worktrees
rimz workspace migrate <old-root> <new-root>
rimz workspace rotate-events [--max-bytes <size>] [--archive-older-than <duration>]
```

`reset` tears a stuck room down — the session, its resurrection cache, and orphaned processes — then rebuilds and reattaches it; `--no-start` stops after teardown, `--yes` skips the confirmation. `reload` runs from anywhere and reconciles sidebars across all of your workspaces: it signals each to pick up the freshly-installed build, verifies build-stamped heartbeats, and re-adds any sidebar pane that cannot reload in place, never rebirthing a session ([internals/sidebar.md](../internals/sidebar.md)). `gc` is the global janitor: it removes stale resolver/sidebar heartbeats, sockets, and read-mark receipts whose owner heartbeats have expired, abandons pending items whose owner process has exited, abandons queued messages for sessions no longer in the rollup, reaps provably-dead workspace ledgers, and sweeps clean Rimz-marked worktrees whose work has landed on their base in the current repo when no live user pane is inside them.

`workspace migrate` rewires the ledger after a repo moves on disk, rewriting every feed item, queued message record, event, and snapshot to the new workspace ID. `workspace rotate-events` archives the active event log past `--max-bytes` (default `64MiB`), preserving the agent rollup, and prunes archives older than `--archive-older-than`. The durability rules behind both live in [internals/ledger.md](../internals/ledger.md).

## Manage trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`status` re-hashes the project's executable surface and reports one of `trusted`, `stale`, `untrusted`, or `no_config`; `grant` pins the current hash; `revoke` drops it. The states, the hash, and stale auto-revoke are in [internals/trust.md](../internals/trust.md); the threat model is in [security.md](../guide/security.md).

## Install agent hooks

```sh
rimz hooks install <agent>      # claude | codex | pi
rimz hooks uninstall <agent>
```

`install` wires Rimz into an agent's own per-user config — the event set plus a statusline (Claude) — additively and reversibly. For pi it writes the Rimz-authored extension whole-file to `~/.pi/agent/extensions/rimz.ts` (the extension needs `rimz` on `PATH`, and takes effect on the next `pi` launch or a `/reload`). This is the real hook mechanism; it is distinct from the project config's `[[hooks]]` table (see [configuration.md](./configuration.md)). What gets wired and how payload content is gated live in [internals/hooks.md](../internals/hooks.md).

## Commands Rimz calls for you

Hooks, the sidebar, the statusline, and other Rimz processes invoke these. You rarely run them by hand.

```sh
rimz ping                                          # liveness check; prints `ok`
rimz sidebar snapshot --workspace-id <id> [--json] # the view-model JSON (inspection; the plugin rail's read)
rimz sidebar serve ...                             # the terminal sidebar renderer
rimz sidebar wake --reason <r> [--workspace-id <id>] # Zellij presence-plugin poke (stamp + eldest nudge)
rimz statusline feed --source <agent>              # captures statusline context
rimz hooks feed --source <agent> [--event <e>]     # routes a hook payload (--event is a debug override)
rimz queue deliver --message-id <id>               # hook-spawned queued-message delivery helper
rimz agents exec <agent> [--run-id <id>] [--worktree-path <p>] [--prompt <text>] # supervised agent pane wrapper
rimz worktree cleanup <path> [--non-interactive]   # marked-worktree cleanup helper
rimz codex ...                                     # Codex enrichment helpers
rimz workspace resolve [PATH]                      # print the resolved workspace as JSON
```

The installed hook command passes only `--source`; the event is read from the payload on stdin. `agents exec` is what `rimz tab`, `rimz agents`, and `rimz run` run inside agent panes: it launches the adapter's CLI, forwards `RIMZ_RUN_ID` when a supervised run owns the pane, and delegates marked-worktree cleanup after exit to `rimz worktree cleanup`. The Codex helpers and the daemon broker they back are documented in [internals/hooks.md](../internals/hooks.md), [internals/transcript.md](../internals/transcript.md), and [internals/worktrees.md](../internals/worktrees.md).
