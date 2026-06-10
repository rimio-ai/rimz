# Feed, events, resolvers, hooks, and trust

This page covers the CLI surfaces that publish feed items, answer them, enrol resolvers, wire agent hooks, and grant project trust. The model behind the commands is [one feed, one CLI](../../../DESIGN.md#one-feed-one-cli); the three actionable surfaces are summarized in [the three operating paths](../../../DESIGN.md#the-three-operating-paths).

## Common script flows

A deployment gate blocks until a human or resolver answers, then prints the decision JSON to stdout:

```sh
decision=$(
  rimz feed ask \
    --title "Promote build 2026.06.10-rc.4 to prod?" \
    --options yes,no,abort \
    --timeout 4h
)
```

The caller owns the decision schema. A simple button answer commonly resolves as `{"choice":"yes"}`, while a richer resolver can return the agent-native or script-native JSON the caller expects.

A script can post progress without blocking:

```sh
rimz event emit --kind deploy.started --title "Deploy started" --json '{"env":"prod"}'
rimz feed push --kind deploy --title "Deploy waiting for canary" --body "Watch the prod room before rollout."
```

A headless script can create a question, return immediately, and let another process resolve it later:

```sh
request_id=$(rimz feed ask --title "Continue migration?" --options yes,no --no-block)
rimz feed show "$request_id" --json
```

A resolver that answers an agent hook uses `feed resolve`:

```sh
rimz feed resolve \
  --decision '{"behavior":"allow"}' \
  --resolver-id readonly-policy \
  --method hook-bridge \
  "$request_id"
```

A resolver that types into a pane still closes the ledger item with `feed resolve` after it verifies the answer landed:

```sh
rimz pane capture "$pane_id" --lines 80 --json
rimz pane send "$pane_id" --enter -- "yes"
rimz feed resolve \
  --decision '{"choice":"yes"}' \
  --resolver-id prompt-policy \
  --method pane-send \
  "$request_id"
```

Enrol the resolver before it can answer bridge items:

```sh
rimz resolver add readonly-policy \
  --order 10 \
  --budget 30s \
  --binary ~/.local/bin/rimz-readonly-policy \
  --display-name "Read-only policy"
rimz resolver ls --json
```

Wire agent hooks and grant project command trust explicitly:

```sh
rimz hooks install claude
rimz trust status
rimz trust grant
```

## Feed items and decisions

Every actionable feed item carries a `surface` that decides which CLI action is meaningful:

| Surface | Created by | Answer path |
| --- | --- | --- |
| `native_ui` | An agent hook when no fresh enrolled resolver is active. | The agent's own UI asks the human. `rimz feed dismiss` records local acknowledgement after the prompt is handled in the pane. |
| `bridge` | An agent hook when a fresh enrolled resolver is active. | `rimz feed resolve` delivers a decision to the waiting hook. `rimz feed abstain` advances the resolver chain. |
| `script` | `rimz feed ask`. | `rimz feed resolve` delivers JSON to the blocked script. `rimz feed abstain` lets the next resolver try. |

The ledger owns the socket, nonce, compare-and-swap, late-answer, and audit rules; see [ledger and bridge](../../internals/ledger.md) for the wire contract.

`rimz feed list` reads the runtime view by default. Runtime views hide pending records whose owner process is gone, while `rimz feed list --audit` reads durable history. `rimz feed show <request-id>` is always an exact audit lookup.

## Events

```sh
rimz event emit --kind <kind> [--title <text>] [--body <text>] [--json <payload>]
```

`event emit` appends a fire-and-forget workspace event and prints the event id. `--kind` is a free-form tag; agent integrations prefer `<source>.<verb>` names. `--json` is parsed as a JSON literal and stored as structured payload.

Examples:

```sh
rimz event emit --kind build.started --title web --json '{"sha":"abc123"}'
rimz event emit --kind deploy.finished --title prod --body "Canary passed."
```

## `rimz feed push`

```sh
rimz feed push --kind <kind> --title <text> [--body <text>]
```

`feed push` posts a non-blocking `native_ui` feed item and prints the request id. Use it for operator-visible notices that need to sit in the room but do not need a decision.

Options:

- `--kind <kind>` names the item category.
- `--title <text>` is the one-line feed title.
- `--body <text>` adds longer context.

Example:

```sh
rimz feed push --kind deploy --title "Manual verification needed" --body "Check canary metrics before rollout."
```

## `rimz feed ask`

```sh
rimz feed ask --title <text> [--options <a,b,c>] [--timeout <duration>] [--no-block]
```

`feed ask` creates a `script` item and prints the request id. By default it then waits until the item resolves and prints the decision JSON. With `--no-block`, it prints only the request id and exits after publishing the item.

Options:

- `--title <text>` is the question shown in the feed and sidebar.
- `--options <a,b,c>` supplies comma-separated answer labels for UI buttons and simple resolvers.
- `--timeout <duration>` bounds the wait with units `s`, `m`, `h`, or `d`, such as `30s`, `5m`, `1h`, or `1d`.
- `--no-block` publishes the question and returns without waiting.

Examples:

```sh
rimz feed ask --title "Restart API workers?" --options yes,no --timeout 10m
request_id=$(rimz feed ask --title "Continue data repair?" --options yes,no --no-block)
```

## `rimz feed list` and `rimz feed ls`

```sh
rimz feed list [--json] [--audit]
rimz feed ls [--json] [--audit]
```

`feed list` prints feed items newest first. Human output is tab-separated as `request_id`, `status`, `surface`, and `title`. `--json` emits the item array. `--audit` includes terminal and dead-owner history that the runtime view intentionally hides.

Examples:

```sh
rimz feed ls
rimz feed list --json
rimz feed list --json --audit
```

## `rimz feed show`

```sh
rimz feed show <request-id> [--json]
```

`feed show` loads one item by id from durable history. Human output prints `<request-id> [<status>/<surface>] <title>` plus the body when present. `--json` emits the full item record.

Example:

```sh
rimz feed show "$request_id" --json
```

## `rimz feed resolve`

```sh
rimz feed resolve --decision <json> <request-id> [--resolver-id <id>] [--method hook-bridge|pane-send|cli|sidebar] [--override-chain]
```

`feed resolve` records a decision for a `bridge` or `script` item. It parses `--decision` as JSON, stores it in the ledger, wakes the waiting hook or script when one exists, and prints `<request-id> effective=<true|false> late=<true|false>`.

Options:

- `--decision <json>` is the decision payload delivered to the waiter.
- `--resolver-id <id>` attributes the decision to an enrolled resolver or to a resolver-compatible tool.
- `--method hook-bridge` records an answer returned through a blocking hook resolver.
- `--method pane-send` records an answer that a resolver typed into a pane.
- `--method cli` records a human or script answer from the command line; this is the default.
- `--method sidebar` records an answer from the sidebar UI.
- `--override-chain` bypasses the active-chain compare-and-swap check for a human override.

Examples:

```sh
rimz feed resolve --decision '{"choice":"yes"}' "$request_id"
rimz feed resolve --decision '{"behavior":"allow"}' --resolver-id readonly-policy --method hook-bridge "$request_id"
rimz feed resolve --decision '{"choice":"yes"}' --resolver-id prompt-policy --method pane-send "$request_id"
```

Use `--override-chain` sparingly. It is for deliberate human intervention when the active resolver chain is stuck or stale, and the override is recorded in the audit trail.

## `rimz feed dismiss`

```sh
rimz feed dismiss <request-id> [--reason <text>]
```

`feed dismiss` acknowledges a `native_ui` item without sending any answer to the agent. Use it after the prompt has already been handled in the agent pane, or when the item is only a local notice.

Options:

- `--reason <text>` records why the item was dismissed.

Example:

```sh
rimz feed dismiss "$request_id" --reason "answered in agent UI"
```

## `rimz feed abstain`

```sh
rimz feed abstain --resolver-id <id> <request-id> [--reason <text>]
```

`feed abstain` records that the active resolver declines to answer and advances the chain. It prints `<request-id> next_resolver=<id>` when another resolver is available, or `<request-id> next_resolver=(none)` when the chain has reached human fallback.

Options:

- `--resolver-id <id>` identifies the resolver that is passing.
- `--reason <text>` records why the resolver passed.

Example:

```sh
rimz feed abstain --resolver-id readonly-policy "$request_id" --reason "write command requested"
```

## Resolver chain

Resolvers are trusted per machine. The allowlist decides which heartbeating resolver ids may engage the bridge; a same-UID process writing heartbeat files is not enough. The resolver protocol and reference resolver patterns live in [resolvers](../../internals/resolvers.md), and the user-facing threat model lives in [security and trust](../../guide/security.md).

```sh
rimz resolver add <id> [--order <n>] [--budget <duration>] [--binary <path>] [--display-name <name>]
rimz resolver remove <id>
rimz resolver list [--json]
rimz resolver ls [--json]
rimz resolver reorder <id> [--before <other-id> | --after <other-id>]
```

`resolver add` enrols one resolver id. `--order` defaults to `10`; lower numbers run earlier. `--budget` defaults to `30s` and accepts `s`, `m`, and `h`. `--binary <path>` pins the executable path Rimz expects for that resolver's heartbeat process. `--display-name <name>` gives UI and reports a human label.

`resolver remove` drops an id from the allowlist. `resolver list` and alias `resolver ls` print entries sorted by chain order; `--json` emits `{ "resolvers": [...] }` with `id`, `order`, `budget_seconds`, `binary`, and `display_name`. `resolver reorder` moves an id immediately before or after another id.

Examples:

```sh
rimz resolver add readonly-policy --order 10 --budget 30s --binary ~/.local/bin/rimz-readonly-policy
rimz resolver add slack-on-call --order 20 --budget 5m --display-name "Slack on-call"
rimz resolver reorder slack-on-call --after readonly-policy
rimz resolver ls --json
rimz resolver remove readonly-policy
```

Project config can launch resolvers only after project trust allows its command-running fields. The resolver still has to be enrolled here and heartbeating freshly before it can answer.

## Agent hooks

```sh
rimz hooks install <agent>
rimz hooks uninstall <agent>
```

`hooks install` writes Rimz-managed hook entries into the agent's per-user config. `hooks uninstall` removes only the Rimz-managed hook block. `<agent>` is an agent kind such as `claude`, `codex`, or `pi`.

Examples:

```sh
rimz hooks install claude
rimz hooks install codex
rimz hooks uninstall claude
```

Installed hooks call Rimz's hidden hook entrypoint for lifecycle events and blocking feed events. Hook stdout is the agent decision channel, so installed hooks keep diagnostics off stdout and route blocking decisions through the bridge; see [agent hooks](../../internals/hooks.md) for the adapter contract.

Some agents add their own hook trust gate after installation. When an agent reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix, such as trusting Rimz hooks inside that agent's own hook UI.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`trust status` is the default. It re-hashes the current project's executable surface and prints `no project config`, `untrusted`, `trusted`, or `stale`. `--json` emits the state, workspace id, project root, config path, record path, current hash, granted hash, and grant timestamp when available.

`trust grant` pins the current executable-surface hash on this machine. `trust revoke` removes the grant. A later edit to a command-running project config field makes the state `stale`, which behaves like untrusted until the grant is refreshed.

Examples:

```sh
rimz trust
rimz trust status --json
rimz trust grant
rimz trust revoke
```

Project trust covers project-supplied command surfaces such as hook commands, resolver launch commands, agent launch commands, layout commands, and other executable fields. The hash and record format live in [project trust](../../internals/trust.md); the operator-facing safety model lives in [security and trust](../../guide/security.md).
