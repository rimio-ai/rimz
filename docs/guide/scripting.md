# Scripting agents

> `rimz agents -p` is `claude -p` for every agent Rimz supports: one prompt, one supervised turn, one exit code. This page is how you drop an agent into a shell script, a `Makefile`, a cron line, or a CI job. The run records, wakeup socket, and pane cleanup underneath it are [harness.md → Supervised runs](../internals/harness/harness.md#supervised-runs).

A supervised run gives you the ergonomics of a CLI over an agent you can still see and steer. `rimz agents <kind> "<prompt>" -p` opens a real pane in the room, runs exactly one root turn, prints the final answer, and exits with the run's status. Your script blocks on it like any other command and branches on the result, while the same turn stays visible in the sidebar — so a run that stops to ask a question routes to your keyboard instead of hanging silently.

## One turn, one exit code

`-p` (`--print`) is the whole contract: run once, print the answer to stdout, exit with the status code.

```sh
rimz agents codex "Prepare the release checklist." -p               # prints the final message, exits 0/1/124/130
rimz agents claude "Summarize what changed on this branch." -p
```

The exit code carries the outcome, so a script reads it directly:

| Code | Meaning |
| --- | --- |
| `0` | The run completed. |
| `1` | The run failed. |
| `124` | The run hit its `--timeout`. |
| `130` | The run was canceled (Ctrl+C on a blocking `-p`). |

```sh
if rimz agents claude "Run the migration audit; reply PASS or FAIL." -p | grep -q PASS; then
  echo "audit clean"
else
  echo "audit needs a human" && exit 1
fi
```

On success, stdout is the final assistant answer and nothing else, so it pipes cleanly. A failed, timed-out, or canceled run prints its status, the captured pane tail, and the transcript path on stderr, keeping stdout uncontaminated for the happy path.

**Cap the wall clock.** `--timeout` bounds the run and turns a wedged turn into exit `124` your wrapper can handle.

```sh
rimz agents codex "Update dependencies and run the test suite." -p --timeout 30m
```

## Feed the prompt in

The positional prompt is the base, and piped stdin appends to it — so build output, a diff, or a log becomes context without a temp file.

```sh
cat build-error.txt | rimz agents claude -p 'explain the root cause'          # stdin folds in after the prompt
git diff --staged | rimz agents codex -p 'review this diff; reply SHIP or HOLD'
```

When both a prompt and stdin are present, Rimz wraps the piped bytes in `<stdin>…</stdin>` so the agent reads them as attached material. For a fully programmatic feed, `--input-format stream-json` reads user messages from stdin until EOF instead of taking a positional prompt.

## Shape the output

`--output-format` chooses what `-p` prints, so the same run serves a human, a `jq` filter, or a live UI.

```sh
rimz agents codex "Prepare the release checklist." -p --output-format json     # full run record as JSON
rimz agents claude "Refactor the parser." -p --output-format stream-json        # NDJSON run events as they land
```

| Format | Prints | Use it for |
| --- | --- | --- |
| `text` (default) | the final assistant message | humans, and simple `grep`/`case` gates |
| `json` | the full run record | parsing `run_id` (feeds `rimz transcript <run_id>`) and `transcript_path` (the provider-native session file) |
| `stream-json` | run events as newline-delimited JSON while the turn runs | a pipeline that wants progress rather than a final blob |

Where the adapter exposes a native cap, `--max-turns <N>` bounds the agentic turn count (Claude today); an agent without one refuses the flag rather than running unbounded.

## A run you can still watch

The turn runs in a real pane, so it is never a black box. It appears in the sidebar as one more card, ranks with the fleet, and — crucially — a run that stops to ask a permission or a question takes the room's normal waiting path: the row flips to `? waiting`, the cockpit counts it, and a notification fires. You answer from anywhere while the script is still blocked on the exit code.

That is what makes unattended runs safe to leave alone. A failing migration at 3 a.m. becomes a push notification on your phone, a one-line answer typed over SSH, and a green pipeline by morning — the script never had to guess. The same shape works while you watch: a CI job that runs a review turn joins your room as a row and asks its design question in your workspace.

The posture that decides whether a run stops to ask or runs straight through — the agent's own bypass flag versus keeping provider prompts — is a choice you make per run; it lives in [Loops and hands-off operation → the permission posture for unattended runs](./loops.md#the-permission-posture-for-unattended-runs) alongside the safety model in [security.md](./security.md).

## Fire now, collect later

For orchestration, decouple starting a run from waiting on it. `--bg` launches the supervised run and prints its agent name, returning immediately; `rimz agents wait` blocks on that name whenever you are ready.

```sh
name=$(rimz agents claude "Run the migration audit." -p --bg)   # returns now, prints e.g. swift-otter
# ... kick off other work ...
rimz agents wait "$name" --stream                               # block on it, tailing the transcript
```

- `rimz agents wait <ref>` blocks on a run; `--stream` tails the answer as it lands.
- `rimz agents show <ref>` reports a run's activity, context, and recent transcript.
- `rimz agents stop <ref>` cancels a live run or closes its pane.

A reference is the printed name, a run id, or any [agent address](./messaging.md#address-an-agent), so the same handle threads through `wait`, `show`, `stop`, and `message`. Every flag on these verbs is in the [agent-control reference](../reference/cli/agents.md).

## Drive the room from a script

The commands you type interactively are the same primitives a script calls, so anything you do at the keyboard runs on a schedule or in CI unchanged.

- `rimz pane capture <ref>` reads a pane's visible text — untrusted terminal output your script matches against bounded patterns before acting on it.
- `rimz pane send <ref>` types literal text and named keys into the agent's own UI, the same explicit input path as `rimz message --steer`.
- `rimz message --steer @<agent> "continue"` is the first-class nudge for wrapper scripts; `rimz message @<agent> --on done "open a PR summary"` hands follow-up work to a running agent at its next turn boundary. The full delivery model is [Messaging](./messaging.md).
- `rimz transcript <ref>` reads back what happened as a timestamped log.

Because these are public CLI, a wrapper composes them freely: read a prompt with `pane capture`, decide with a bounded matcher, answer with `pane send`, and escalate anything it does not recognize by leaving the row `? waiting` for you.

## In a pipeline

**Cron — refresh dependencies overnight and open a PR.** The worktree isolates the work; the exit code gates the notification.

```sh
# 02:00 nightly
rimz agents codex --worktree=deps --timeout 4h -p \
  "Update dependencies, run the full test suite, and open a PR. Stop and ask if a major version bumps." \
  || notify-send "deps run needs a human"
```

**CI — a review gate that reads SHIP or HOLD.** The turn joins whatever room the runner attaches to, so a design question still reaches a human.

```sh
verdict=$(rimz agents claude -p "Review the canary metrics in ./metrics.json and reply SHIP or HOLD")
case "$verdict" in
  SHIP*) exit 0 ;;
  *)     echo "held: $verdict" >&2; exit 1 ;;
esac
```

**Fan out, then join.** Start several background runs, collect them by name.

```sh
for pkg in api web worker; do
  rimz agents codex --worktree="audit-$pkg" -p --bg "Audit $pkg for the CVE and reply with the fix." >> runs.txt
done
while read -r name; do rimz agents wait "$name" --stream; done < runs.txt
```

## Prerequisites

Supervised runs need installed and trusted hooks, because hooks are the completion signal — a run with no hooks has no way to know its turn ended. `rimz doctor` confirms the hook state, and [Troubleshooting](./troubleshooting.md) covers a run that never completes. Installing hooks is a one-time consent step, walked in the [Quickstart](./quickstart.md).

## See also

- [Loops and hands-off operation](./loops.md) — put these runs on a clock: schedules, watchdogs, and notification handlers.
- [Messaging](./messaging.md) — the `--steer` / `--on done` delivery model wrappers lean on.
- [Agents, worktrees, and teams](./agents.md) — the handle and worktree vocabulary these examples use.
- [Agent control CLI](../reference/cli/agents.md#supervised-runs--p) — every flag on `-p`, `wait`, `show`, `stop`, and `pane`.
- [harness.md → Supervised runs](../internals/harness/harness.md#supervised-runs) — run records, the wakeup socket, streaming, and pane cleanup.
