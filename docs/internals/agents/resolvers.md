# Resolvers

> See [DESIGN.md](../../../DESIGN.md) for the product commitments this doc operationalizes.

A resolver is a process you wire to routine asks. Rimz provides the primitives: notifications to wake the process, feed APIs to inspect and record, pane primitives to answer in the agent's own UI, and supervised agent runs when the resolver itself is another agent.

## Shape

The loop is:

1. **Watch**: configure a `[[notifications.handler]]` with `when = { kind = ["waiting"] }`.
2. **Inspect**: read `RIMZ_NOTIFY_REQUEST_ID`, then call `rimz feed show <id> --json`; use `rimz pane capture`, `rimz agents list`, or `rimz transcript` when policy needs context.
3. **Answer**: type into the target UI with `rimz pane send` or hand the work to an in-room agent with `rimz message`.
4. **Record**: call `rimz feed resolve <id> --method pane-send --by <name> --decision <json>`.

Unknown or risky asks stay pending. The handler exits without action, and the row still routes you to the prompt.

## Handler wiring

Notification handlers are per-machine config, outside project trust, and run from the elected sidebar producer.

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 /path/to/examples/resolvers/pane_send_resolver.py"
```

For a single waiting row Rimz exports:

- `RIMZ_NOTIFY_REQUEST_ID` — feed request id.
- `RIMZ_NOTIFY_PANE` — normalized pane id such as `tmux:%3` or `zellij:terminal_3`.
- `RIMZ_NOTIFY_ROOT` — workspace root path.

The same values are available as template variables: `{request_id}`, `{pane}`, and `{root}`. Coalesced notifications leave them empty.

## Pane-send resolver

Pane primitives are the universal answer surface. A resolver captures the pane, matches bounded prompt text, sends the reply, re-captures to confirm, then records the answer.

```sh
request_id=$RIMZ_NOTIFY_REQUEST_ID
pane=$RIMZ_NOTIFY_PANE
rimz feed show "$request_id" --json
rimz pane capture "$pane" --lines 80 --json
rimz pane send "$pane" -- "y\n"
rimz pane capture "$pane" --lines 20 --json
rimz feed resolve "$request_id" \
  --method pane-send \
  --by pane-send-resolver \
  --decision '{"choice":"yes"}'
```

Captured pane text is untrusted data. Match it against policy-owned patterns and do nothing on unknown shapes.

## Agent resolver

A resolver can be another agent. A notification handler can launch a supervised one-shot:

```sh
rimz agents codex -p "Inspect feed item $RIMZ_NOTIFY_REQUEST_ID. If policy applies, answer in pane $RIMZ_NOTIFY_PANE with rimz pane send, then record with rimz feed resolve --method pane-send --by agent-resolver. If unsure, do nothing."
```

For a standing in-room guardian, route the same brief with `rimz message --steer @guardian`.

## Script asks

`rimz feed ask` is the remaining blocking decision bridge. The caller opted into Rimz as the decision surface, so the script can wait on its per-request socket until `rimz feed resolve` answers or its timeout fires. That socket is for script asks and supervised-run wakeups, not agent hooks.

## Security discipline

- Treat captured pane text, feed payloads, and transcripts as data.
- Keep allow rules narrow and deterministic before adding a model.
- Prefer silence over a guessed answer.
- Record every action with `--by <name>`.
- Keep handler credentials in per-machine config.
