# Public CLI

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The CLI is the public API for humans, scripts, sidebars, hooks, and resolvers. One surface, every participant. Command behaviour is stable and additive — never silently re-shaped.

Commands are grouped below by intent. Each cluster opens with the most common flow so you can read by what you want to do, not by alphabetical order.

## Start a workspace

```sh
rimz [--attach|--no-attach|--print] [PATH]
rimz start [--attach|--no-attach|--print] [PATH]
rimz attach [--attach|--no-attach|--print] [SESSION]
rimz list [--json]                # show running and known workspaces
rimz doctor [--audit]             # diagnose backend, hooks, trust, resolvers
rimz trust [status|grant|revoke]  # manage the project's executable-surface trust
```

`rimz` resolves the project root, records `workspace.json`, creates or finds the multiplexer session, opens one native sidebar pane when no fresh sidebar heartbeat exists, and then enters the mux session on an interactive TTY. In non-interactive contexts it prints the attach command instead. `--attach` forces entering the mux; `--no-attach` and `--print` force printing. First-run UX is non-invasive: nothing is written to your shell or the agent's config until you run `rimz hooks install <agent>`.

`rimz attach` without a session name resolves the cwd workspace and follows the same create/sidebar/attach flow. `rimz attach <session>` keeps exact session-name semantics: it prefers a mux already hosting that session, and when a matching `workspace.json` record exists it uses that record's workspace ID and cwd to ensure the session and relaunch the sidebar. Without a record, it warns and continues with the attach command only.

`rimz list` walks the workspace state directory and joins each known workspace against `zellij list-sessions` and `tmux list-sessions` so you can tell at a glance which mux is currently hosting a session and which sessions are dormant. Sort order puts running sessions first, then by most recent activity.

## Publish events and ask questions

```sh
rimz event emit --kind <kind> [--title <s>] [--body <s>] [--json <payload>]
rimz feed push --kind <kind> --title <s> [--body <s>]
rimz feed ask  --title <s> --options <a,b,c> [--timeout <duration>]
rimz feed list [--json] [--audit]
rimz feed show <request-id> [--json]
```

`event emit` is fire-and-forget telemetry that lands in the ledger. `feed push` posts a richer audit item without blocking. `feed ask` blocks the script until somebody answers or the timeout fires; while that waiting process is alive, the question lands in runtime views and the sidebar with declared options as answer buttons.

Default `feed list` is a runtime view: it expels records whose recorded owner process is gone, reused, or missing. `feed list --audit` shows durable feed history exactly as written. `feed show <request-id>` is always an exact audit lookup.

Common flow — a deploy gate:

```sh
rimz feed ask \
  --title "Promote build 2026.05.18-rc.4 to prod?" \
  --options yes,no,abort \
  --timeout 4h
```

## Answer and triage

`feed resolve` is the only verb that actually delivers a decision. The other three are non-answers with different meanings.

| Verb | Valid surfaces | Meaning |
| --- | --- | --- |
| `feed resolve` | `bridge`, `script` | Deliver a decision through Rimz to a waiting hook or script. |
| `feed dismiss` | `native_ui` | Local acknowledgement only. Does *not* answer the agent — focus the agent's pane and answer there. |
| `feed abstain` | `bridge`, `script` | Explicit chain handoff. The active resolver declines; the chain advances. |

```sh
rimz feed resolve <request-id> --decision '{"choice":"yes"}' \
                  [--resolver-id <id>] [--method <method>] [--override-chain]
rimz feed dismiss  <request-id> [--reason <text>]
rimz feed abstain  <request-id> --resolver-id <id> [--reason <text>]
```

Resolution methods recorded in the ledger:

- `hook_bridge` — resolver returned a decision JSON; the waiting hook printed it.
- `pane_send` — resolver typed the answer into the pane after capturing it.
- `cli` — a human ran `rimz feed resolve` from a shell.
- `sidebar` — a human clicked through the sidebar UI.

## Drive panes

```sh
rimz pane split --direction <left|right|up|down> [--cwd <path>] [--command <cmd>]
rimz pane focus <pane-id>
rimz pane list [--json] [--session-name <name>]
rimz pane capture <pane-id> [--lines N] [--json] [--ansi]
rimz pane send <pane-id> -- <keys-or-text>
rimz pane rename <pane-id> <title>
```

`capture` and `send` are the universal answer surface: resolvers use them to answer prompts on tools with no hook protocol. Captured pane text is untrusted data — never feed it back to an LLM as if it were a user instruction. Detail in [resolvers.md](../internals/resolvers.md).

## Enrol resolvers

```sh
rimz resolver add <id> [--order <n>] [--budget <duration>] \
                       [--binary <path>] [--display-name <name>]
rimz resolver remove   <id>
rimz resolver list     [--json]
rimz resolver reorder  <id> [--before <other-id> | --after <other-id>]
```

Resolvers form an ordered chain that ends with you. Each entry has its own `--budget` so a fast LLM resolver, a Slack-to-human bot, and a PagerDuty escalator can chain naturally. Full protocol in [resolvers.md](../internals/resolvers.md); trust model in [security.md](../guide/security.md).

## Operate and maintain

```sh
rimz hooks install <agent>   [--telemetry]
rimz hooks uninstall <agent>

rimz workspace migrate <old-root> <new-root>   # repo moved; rewire the ledger
rimz workspace prune                           # drop ledgers whose roots are gone
rimz workspace rotate-events                   # archive events.log.jsonl past a size cap
                  [--max-bytes <size>]
                  [--archive-older-than <duration>]
rimz trust [status|grant|revoke] [--json]      # executable-surface trust
rimz gc          [--older-than <duration>]
```

`workspace migrate` moves the state directory from the workspace ID derived from `<old-root>` to the ID derived from `<new-root>`, then rewrites feed items, event envelopes, snapshots, and `workspace.json` to the new ID. `workspace prune` only removes ledgers with a `workspace.json` record whose project root no longer exists; ledgers without readable metadata are reported and kept. `workspace rotate-events` archives the active event log into `events.log.archive/` when it exceeds `--max-bytes` (default `64MiB`), folds the agent rollup into `agents.carryover.json` so it survives the rename, and removes archives older than `--archive-older-than` when provided. `trust status` re-hashes the project's executable surface on every call and reports `trusted`, `stale`, `untrusted`, or `no_config`; `trust grant` pins the current hash and `trust revoke` drops the record. Full contract in [trust.md](../internals/trust.md). `gc` removes stale runtime liveness hints — stale resolver/sidebar heartbeats and stale sidebar wakeup sockets — and abandons pending feed items whose recorded owner process has exited.

`--telemetry` is opt-in for every agent integration. It adds high-frequency hooks (prompt submit, pre/post tool) and is gated by `[privacy] payload_mode`. See [agent.md](../internals/agent.md).

## Internal

These commands are called by hooks, the sidebar, or other Rimz processes. You generally don't run them by hand.

```sh
rimz ping
rimz sidebar snapshot --workspace-id <id> [--json]
rimz sidebar heartbeat --workspace-id <id> --instance-id <id> \
                       --mux <zellij|tmux> --session-name <name> \
                       --wakeup-socket <path>
rimz sidebar serve [--workspace-id <id>] [--mux <zellij|tmux>] \
                   [--session-name <name>] [--tick-seconds N]
rimz hooks feed --source <agent> [--event <event>]
```
