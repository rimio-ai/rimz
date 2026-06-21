# Feed, events, resolvers, hooks, and trust

These commands publish items to the room's feed, answer them, enrol resolvers, wire agent hooks, and grant project trust. The model behind them is [one feed, one CLI](../../../DESIGN.md#one-feed-one-cli); the three actionable surfaces are [the three operating paths](../../../DESIGN.md#the-three-operating-paths).

## Common script flows

Block a deployment gate until a human or resolver answers, then read the decision JSON:

```sh
decision=$(rimz feed ask --title "Promote build 2026.06.10-rc.4 to prod?" --options yes,no,abort --timeout 4h)
```

The caller owns the decision schema — a button answer is commonly `{"choice":"yes"}`, while a resolver can return whatever JSON the caller expects. Post progress without blocking, or create a question and resolve it from another process:

```sh
rimz event emit --kind deploy.started --title "Deploy started" --json '{"env":"prod"}'
request_id=$(rimz feed ask --title "Continue migration?" --options yes,no --no-block)
rimz feed resolve --decision '{"choice":"yes"}' "$request_id"
```

Wire hooks and grant trust so an enrolled resolver can answer agent prompts:

```sh
rimz hooks install claude
rimz trust grant
rimz resolver add readonly-policy --order 10 --budget 30s --binary ~/.local/bin/rimz-readonly-policy
```

## Feed items and decisions

Every actionable feed item carries a `surface` that decides which action is meaningful:

| Surface | Created by | Answer path |
| --- | --- | --- |
| `native_ui` | An agent hook when no fresh enrolled resolver is active. | The agent's own UI asks the human; `rimz feed dismiss` records local acknowledgement after the pane handles it. |
| `bridge` | An agent hook when a fresh enrolled resolver is active. | `rimz feed resolve` delivers a decision to the waiting hook; `rimz feed abstain` advances the chain. |
| `script` | `rimz feed ask`. | `rimz feed resolve` delivers JSON to the blocked script; `rimz feed abstain` lets the next resolver try. |

```sh
rimz feed ask --title <TEXT> [--options <a,b,c>] [--timeout <DURATION>] [--no-block]
rimz feed push --kind <KIND> --title <TEXT> [--body <TEXT>]
rimz feed list|ls [--json] [--audit]
rimz feed show <REQUEST_ID> [--json]
rimz feed resolve --decision <JSON> <REQUEST_ID> [--resolver-id <ID>] [--method <METHOD>] [--override-chain]
rimz feed dismiss <REQUEST_ID> [--reason <TEXT>]
rimz feed abstain --resolver-id <ID> <REQUEST_ID> [--reason <TEXT>]
```

- **`ask`** creates a `script` item, prints the request id, and (without `--no-block`) waits for the decision JSON and prints it. `--options` supplies button labels; `--timeout` bounds the wait.
- **`push`** posts a non-blocking `native_ui` notice — an operator-visible item that needs no decision — and prints the request id.
- **`list`** prints items newest first (`request_id`, `status`, `surface`, `title`); the runtime view hides records whose owner process is gone, and `--audit` reads durable history. **`show`** is always an exact audit lookup by id.
- **`resolve`** records a decision for a `bridge` or `script` item: it stores the `--decision` JSON, wakes the waiting hook or script, and prints `<request-id> effective=<bool> late=<bool>`. `--resolver-id` attributes it; `--method` records how it was answered (`hook-bridge`, `pane-send`, `cli` (default), or `sidebar`); `--override-chain` bypasses the active-chain check for a deliberate, audited human override.
- **`dismiss`** acknowledges a `native_ui` item locally without answering the agent. **`abstain`** records that a resolver declines and advances the chain, printing the next resolver id or `(none)` at human fallback.

The ledger owns the socket, nonce, compare-and-swap, late-answer, and audit rules; the wire contract is in [ledger and bridge](../../internals/sidebar/ledger.md).

## Events

```sh
rimz event emit --kind <KIND> [--title <TEXT>] [--body <TEXT>] [--json <PAYLOAD>]
```

`event emit` appends a fire-and-forget workspace event and prints the event id. Unlike `feed push`, an event is a ledger record rather than a feed item — use it for structured progress that tooling reads, not for something a person needs to act on. `--kind` is a free-form tag (agent integrations prefer `<source>.<verb>`) and `--json` is stored as a structured payload.

```sh
rimz event emit --kind build.started --title web --json '{"sha":"abc123"}'
rimz event emit --kind deploy.finished --title prod --body "Canary passed."
```

## Resolver chain

Resolvers are trusted per machine: the allowlist decides which heartbeating resolver ids may engage the bridge, so a same-UID process writing heartbeat files is not enough on its own. The protocol and reference patterns are in [resolvers](../../internals/agents/resolvers.md), and the threat model in [security](../../guide/security.md).

```sh
rimz resolver add <ID> [--order <N>] [--budget <DURATION>] [--binary <PATH>] [--display-name <NAME>]
rimz resolver remove <ID>
rimz resolver list|ls [--json]
rimz resolver reorder <ID> [--before <OTHER> | --after <OTHER>]
```

`add` enrols one resolver id. `--order` defaults to `10` (lower runs earlier), `--budget` defaults to `30s`, `--binary` pins the executable Rimz expects for that resolver's heartbeat, and `--display-name` is the label shown in UI and reports. `list` prints entries in chain order (`--json` emits `id`, `order`, `budget_seconds`, `binary`, `display_name`), and `reorder` moves an id before or after another.

```sh
rimz resolver add readonly-policy --order 10 --budget 30s --binary ~/.local/bin/rimz-readonly-policy
rimz resolver add slack-on-call --order 20 --budget 5m --display-name "Slack on-call"
rimz resolver reorder slack-on-call --after readonly-policy
```

Project config can launch resolvers only after project trust allows its command-running fields — and the resolver still has to be enrolled here and heartbeating freshly before it can answer.

## Agent hooks

```sh
rimz hooks install [AGENT]
rimz hooks uninstall [AGENT]
```

`hooks install` writes Rimz-managed hook entries into the agent's per-user config. With no `AGENT` it installs every detected supported agent on PATH and prints a JSON array of reports; with an explicit kind (`claude`, `codex`, `pi`, …) it prints the single report. `hooks uninstall` removes only Rimz-managed hook blocks — with no `AGENT` it removes every installed set, prints `[]` when nothing is installed, and exits successfully without needing the binary on PATH.

Installed hooks call Rimz's hidden hook entrypoint for lifecycle and blocking feed events. Hook stdout is the agent decision channel, so installed hooks keep diagnostics off stdout and route decisions through the bridge ([the adapter boundary](../../internals/agents/agent.md#the-adapter-boundary)). Some agents add their own hook trust gate; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`trust status` (the default) re-hashes the project's executable surface and prints `no project config`, `untrusted`, `trusted`, or `stale`. `trust grant` pins the current hash on this machine; `trust revoke` removes the grant. A later edit to a command-running project field makes the state `stale`, which behaves like untrusted until the grant is refreshed. `--json` emits the state, ids, paths, hashes, and grant timestamp.

Project trust covers project-supplied command surfaces — hook commands, resolver launch commands, agent launch commands, layout commands, and other executable fields. The hash and record format are in [project trust](../../internals/sidebar/trust.md); the operator-facing safety model is in [security and trust](../../guide/security.md).
