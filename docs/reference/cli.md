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

rimz attach --remote dev-box:query-engine          # reattach from anywhere over ssh
rimz pane split && claude                           # run an agent in a new pane
```

That is the whole loop. For the five-minute tour and the why, see [the product guide](../guide/product.md); for the model that shapes the surface, see [DESIGN.md](../../DESIGN.md).

## Start and attach a workspace

```sh
rimz [--attach|--no-attach|--print] [--no-resume] [PATH]
rimz start [--attach|--no-attach|--print] [--no-resume] [PATH]
rimz attach [--attach|--no-attach|--print] [--no-resume] [SESSION]
rimz attach --remote [user@]host:<session-or-path> [--no-reconnect]
rimz list [--all] [--json]        # running + recently-active workspaces; --all adds dormant ones
rimz doctor [--audit]             # diagnose backend, hooks, trust, resolvers
```

`rimz` (or `rimz start`) resolves the workspace root, finds or creates the multiplexer session, launches the native sidebar pane, and enters the session. On an interactive TTY it attaches; otherwise it prints the attach command. `--attach` forces attaching; `--no-attach` and `--print` force printing — the rule is [interactive attach is opportunistic](../../DESIGN.md#commitments). Run from inside a session of the selected backend, `rimz` reports the directory's room and exits without nesting — a same-mux room can't be nested; detach or run from outside to (re)launch. Nothing is written to your shell or an agent's config until you run `rimz hooks install`.

The root resolves through one ladder, richest tier first: an explicit `--root`, the enclosing git repo (worktrees collapse to the repo, so every worktree shares the repo's room), a project-marker directory (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `.rimz/config.toml`, …), and finally the directory itself — a first-class **directory workspace**, the room a headless box of agents gets with no source control. A non-repo root names itself with one stderr line at start, and the directory tier refuses exactly two roots as almost-certainly accidental — `$HOME` and `/` — with `--root` as the deliberate override. Starting a room whose root nests inside (or contains) another *live* room adds a one-line overlap notice: overlap is legal — an agent belongs to the room its pane lives in — and `rimz doctor` shows the standing room tree.

Inside a room, identity is pinned: session birth stamps `RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT` into the session environment, and participating commands — agent hooks, `rimz event`/`feed`, the statusline helpers — resolve through the verified pin before the static ladder (an explicit `--root` beats both), so an agent working in a nested repo inside a directory room writes to the room it lives in ([hooks.md](../internals/hooks.md#hooks-resolve-the-room-they-live-in)). Room-choosing commands (`rimz start`/`attach`) resolve fresh, keeping a deliberate per-repo room one `rimz start` away.

When a session is reborn — after a reboot, a multiplexer crash, or a reset — Rimz re-seeds the prior agents it remembers, each restored idle in its own pane (`claude --resume`, `codex resume`); the conversation is back, no tokens spent until you type. `--no-resume` comes up empty for a deliberately fresh start (`resume.on_rebirth` in [configuration.md](./configuration.md) is the persistent switch). Details: [resume-on-rebirth](../internals/sidebar.md#resume-on-rebirth).

`rimz attach <session>` reattaches by exact session name; `rimz attach` with no name uses the cwd's workspace. `rimz list` joins each known workspace against the live Zellij and tmux sessions so you see which mux currently hosts it, running first.

`rimz attach --remote [user@]host:<target>` attaches a room on another machine: rimz builds the guarded `ssh -t` invocation, the host's own `rimz` starts or reattaches the room, and it renders in your terminal — sidebar, feed, and all. A `<target>` containing `/` (or starting with `~`) is a path the host starts a room for (`dev-box:~/code/query-engine`); a bare word is a session name to reattach (`dev-box:query-engine`); IPv6 hosts keep their brackets (`user@[::1]:…`). The same attach rule applies — an interactive TTY connects, anything else prints the full ssh command (`--print` needs no local ssh) — and `~/.ssh/config` aliases, ports, keys, and jump hosts apply as-is because rimz runs your `ssh`. The remote snippet repairs a non-login shell's PATH and, when the host has no `rimz`, fails with the install command instead of a bare `command not found`.

A dropped link reconnects by itself. Keepalives (`ServerAliveInterval=5`, three strikes) detect a dead link in about fifteen seconds, and rimz reattaches with capped exponential backoff — the remote room survives the drop by design, so pickup is where you left it. A clean detach ends the session, a first connection that fails (auth, unknown host) surfaces immediately rather than looping a password prompt, and a remote failure that isn't a link drop reports the remote's own error. `--no-reconnect` hands the link to a single ssh run. `--no-resume` and `--mux` ride into the remote `rimz`.

`rimz doctor` reports the backend, installed hooks, trust state, enrolled resolvers, and the machine's room tree — every recorded workspace with its root, root class, and liveness, the current directory's room starred and nesting live rooms flagged — and names the fix for anything misconfigured. Run it first when something looks wrong.

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
rimz pane send <pane-id> -- <keys-or-text>
rimz pane detach [--session-name <name>]
```

`capture` and `send` are the universal answer surface: resolvers use them to answer prompts on tools that expose no hook protocol. Captured pane text is untrusted data — a resolver matches it against its own bounded patterns, never replaying it as an instruction. Detail in [resolvers.md](../internals/resolvers.md). `detach` drops the attached client and leaves the session running; client semantics differ per backend ([multiplexers.md](../internals/multiplexers.md)).

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
rimz gc [--older-than <duration>]        # sweep stale liveness hints and dead-owner items
rimz workspace migrate <old-root> <new-root>
rimz workspace rotate-events [--max-bytes <size>] [--archive-older-than <duration>]
```

`reset` tears a stuck room down — the session, its resurrection cache, and orphaned processes — then rebuilds and reattaches it; `--no-start` stops after teardown, `--yes` skips the confirmation. `reload` runs from anywhere and reconciles sidebars across all of your workspaces: it re-execs each to a freshly-installed build and re-adds any view that lost its sidebar, never rebirthing a session ([internals/sidebar.md](../internals/sidebar.md)). `gc` is the global janitor: it removes stale resolver/sidebar heartbeats and sockets, abandons pending items whose owner process has exited, and reaps provably-dead workspace ledgers.

`workspace migrate` rewires the ledger after a repo moves on disk, rewriting every feed item, event, and snapshot to the new workspace ID. `workspace rotate-events` archives the active event log past `--max-bytes` (default `64MiB`), preserving the agent rollup, and prunes archives older than `--archive-older-than`. The durability rules behind both live in [internals/ledger.md](../internals/ledger.md).

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
rimz sidebar snapshot --workspace-id <id> [--json] # the shared view-model JSON
rimz sidebar serve ...                             # the terminal sidebar renderer
rimz statusline feed --source <agent>              # captures statusline context
rimz hooks feed --source <agent> [--event <e>]     # routes a hook payload (--event is a debug override)
rimz codex ...                                     # Codex enrichment helpers
rimz workspace resolve [PATH]                      # print the resolved workspace as JSON
```

The installed hook command passes only `--source`; the event is read from the payload on stdin. The Codex helpers and the daemon broker they back are documented in [internals/hooks.md](../internals/hooks.md) and [internals/transcript.md](../internals/transcript.md).
