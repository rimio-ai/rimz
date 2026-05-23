# Public CLI

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The CLI is the public API for humans, scripts, sidebars, hooks, and resolvers. One surface, every participant. Command behaviour is stable and additive — never silently re-shaped.

Commands are grouped below by intent. Each cluster opens with the most common flow so you can read by what you want to do, not by alphabetical order.

## Start a workspace

```sh
rimz                              # open or reattach the project's room
rimz attach billing-service       # reattach a specific workspace from anywhere
rimz list [--json]                # show running and known workspaces
rimz setup [--yes]                # dry-run plugin + hook install, prompt to apply
rimz doctor                       # diagnose backend, hooks, trust, resolvers
rimz trust [status|grant|revoke]  # manage the project's executable-surface trust
```

`rimz` resolves the project root, picks the multiplexer backend, opens (or attaches to) the session, and lands you in a pane. First-run UX is non-invasive: nothing is written to your shell or the agent's config until you `rimz setup`.

## Publish events and ask questions

```sh
rimz event emit --kind <kind> [--title <s>] [--body <s>] [--json <payload>]
rimz feed push --kind <kind> --title <s> [--body <s>]
rimz feed ask  --title <s> --options <a,b,c> [--timeout <duration>]
rimz feed list [--json]
rimz feed show <request-id> [--json]
```

`event emit` is fire-and-forget telemetry that lands in **Recent activity**. `feed push` posts a richer item to **Needs your attention** without blocking. `feed ask` blocks the script until somebody answers or the timeout fires; the question lands in the sidebar with declared options as answer buttons.

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
| `feed annotate` | any | Append an audit note. No state-machine effect. |
| `feed abstain` | `bridge`, `script` | Explicit chain handoff. The active resolver declines; the chain advances. |

```sh
rimz feed resolve <request-id> --decision '{"choice":"yes"}' \
                  [--resolver-id <id>] [--method <method>] [--override-chain]
rimz feed dismiss  <request-id> [--reason <text>]
rimz feed annotate <request-id> --note <text>
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
rimz pane list [--json]
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

## Manage worktrees

```sh
rimz worktree list    [--json]                # branch, path, last activity, agent count
rimz worktree suggest [--branch <name>] [--path <path>]   # prints a git worktree add command
rimz worktree focus   <branch-or-path>        # switch the active view in the session
```

Worktree creation is a user gesture (`git worktree add`); `suggest` just prints the command, ready to copy.

## Operate and maintain

```sh
rimz hooks setup            [--agent <name>] [--yes]
rimz hooks <agent> install  [--yes] [--telemetry|--no-telemetry]
rimz hooks <agent> uninstall

rimz workspace migrate <old-root> <new-root>   # repo moved; rewire the ledger
rimz workspace prune                           # drop ledgers whose roots are gone
rimz state export [--json]                     # honors the active payload_mode
rimz state wipe   <workspace>                  # destructive; prompts
rimz gc          [--older-than <duration>]
```

`--telemetry` is opt-in for every agent integration. It adds high-frequency hooks (prompt submit, pre/post tool) and is gated by `[privacy] payload_mode`. See [agent.md](../internals/agent.md).

## Internal

These commands are called by hooks, the sidebar, or other Rimz processes. You generally don't run them by hand.

```sh
rimz plugin install [--yes]
rimz ping
rimz sidebar snapshot --workspace-id <id> [--json]
rimz sidebar heartbeat --workspace-id <id> --instance-id <id> \
                       --mux <zellij|tmux> --session-name <name> \
                       --wakeup-socket <path>
rimz sidebar serve [--workspace-id <id>] [--mux <zellij|tmux>] \
                   [--session-name <name>] [--tick-seconds N]
rimz hooks <agent> <subcommand>
rimz hooks feed --source <agent> [--event <event>]
rimz feed wait    <request-id> [--timeout <duration>]
rimz feed claim   <request-id> --resolver-id <id> [--ttl <duration>]
rimz feed unclaim <request-id> --resolver-id <id>
rimz feed expire  <request-id>
rimz wait agent-status <agent> <state> [--timeout <duration>]
```
