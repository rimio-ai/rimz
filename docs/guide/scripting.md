# Scripting

> `rimz agents -p` is `claude -p` for every agent RimZ supports: one prompt, one supervised turn, one exit code — with the turn running in a real pane you can watch, answer, and steer. This page is how you drop an agent into a shell script, a `Makefile`, a cron line, or a CI job. The run records, wakeup socket, and pane cleanup underneath it are [scripting.md](../internals/harness/scripting.md).

## Why `rimz agents -p`

You already script agents. `claude -p "explain this diff"` in a pipeline, `codex exec` in a Makefile: prompt in, answer out, an exit code the script branches on. The contract is right, and RimZ keeps it — same flag, same shape, nothing to relearn.

What headless mode gives up is everything around the run. The turn is an invisible process: when it stalls, wanders, or stops to ask a permission question, the pipeline hangs with nothing to look at and nowhere to type an answer. And every CLI spells the mode its own way — different flags, different output framing, one wrapper script per provider.

`rimz agents -p` is the same contract without those limits:

```sh
rimz agents claude "Summarize what changed on this branch." -p    # the claude -p you know
rimz agents codex "Prepare the release checklist." -p             # the same grammar, any agent
```

- **One grammar for every fully scripted adapter.** The same flags, exit codes, and output formats drive Claude, Codex, Amp, Pi, OpenCode, Droid, and Qwen; swapping the model behind a pipeline is a one-word change. (Kiro's CLI exposes no verified turn-completion signal yet, so `rimz agents kiro -p` refuses before opening a pane rather than hanging.)
- **A pane instead of a headless process.** The turn runs the stock CLI in your room with a live card in the sidebar, so a scripted run is exactly as observable — and as steerable — as one you launched by hand ([a real agent, not a background job](#a-real-agent-not-a-background-job)).

## What a run does on your machine

`-p` adds no engine of its own; it sequences pieces this guide set already covers:

1. It checks pre-launch dollar gates and any exact managed-launch provider quota, then writes a durable run record — a JSON file under `~/.local/state/rimz/workspaces/<id>/runs/` — before anything opens.
2. It opens one pane in your Zellij or tmux (a split beside you when you run it inside the room, a new tab when the caller is outside it) running the official agent CLI with your prompt: the same launch as an interactive `rimz agents <kind>`, one supervised turn instead of a session.
3. It blocks until the agent's own reporting hooks say the root turn ended — hooks are the completion signal, which is why they are the one [prerequisite](#prerequisites).
4. It prints the answer, exits with the run's code, and closes the pane. The agent's session file stays where the CLI always puts it, so `claude --resume` and the provider's own apps keep working, and `rimz agents show` and `rimz transcript` read the run back after it ends.

Nothing else moves: no daemon, no forked agent, no RimZ-private copy of the session. Ctrl+C cancels cleanly — exit `130`, agent stopped, pane reclaimed — and `rimz agents stop <ref>` does the same from any other pane. Add `--keep` to leave the finished pane open for inspection.

## One turn, one exit code

`-p` (`--print`) is the whole contract: run once, print the answer to stdout, exit with the status code.

| Code | Meaning |
| --- | --- |
| `0` | The run completed. |
| `1` | The run failed. |
| `123` | The run exhausted `--max-attempts` while its `--verify` command was still red. |
| `124` | The run hit its `--timeout`. |
| `125` | The run reached its `--budget` or an exact managed-launch provider quota. |
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

**Cap the dollars.** `--budget 2` records `budget_exceeded` and exits `125`, distinct from a timeout or agent failure. A spent provider subscription quota exits `125` the same way, before the run record or the pane exists, so a script sees one code for "this run was not affordable" whichever limit stopped it. Both caps are the [budgets guide](./budget.md).

Two more flags, `--verify` and `--retries`, gate what counts as done and rerun what failed; they have [their own section below](#verify-and-retry).

Long-running commands animate their current phase and elapsed time on an interactive stderr terminal. Set `RIMZ_NO_PROGRESS=1` to disable the status line everywhere; non-TTY stderr and RimZ-launched agent shells carrying `RIMZ_AGENT_KIND` disable it automatically.

## Feed the prompt in

The positional prompt is the base, and `--stdin` appends stdin to it — so build output, a diff, or a log becomes context without a temp file.

```sh
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause'          # stdin folds in after the prompt
git diff --staged | rimz agents codex -p --stdin 'review this diff; reply SHIP or HOLD'
```

`--stdin` reads to EOF. When both a prompt and stdin are present, RimZ wraps the stdin bytes in `<stdin>…</stdin>` so the agent reads them as attached material. For a fully programmatic feed, `--input-format stream-json` reads user messages from stdin until EOF instead of taking a positional prompt; it already declares stdin as its source, so omit `--stdin`.

## Shape the output

`--output-format` chooses what `-p` prints, so the same run serves a human, a `jq` filter, or a live UI.

```sh
rimz agents codex "Prepare the release checklist." -p --output-format json      # full run record as JSON
rimz agents claude "Refactor the parser." -p --output-format stream-json        # NDJSON run events as they land
```

| Format | Prints | Use it for |
| --- | --- | --- |
| `text` (default) | the final assistant message | humans, and simple `grep`/`case` gates |
| `json` | the full run record | parsing `run_id` (feeds `rimz transcript <run_id>`) and `transcript_path` (the provider-native session file) |
| `stream-json` | run events as newline-delimited JSON while the turn runs | a pipeline that wants progress rather than a final blob |

Where the adapter exposes a native cap, `--max-turns <N>` bounds the agentic turn count (Claude today); an agent without one refuses the flag rather than running unbounded.

## Verify and retry

An exit code tells you the turn ended; these two flags let the run prove the work and repair itself before your script ever sees a failure.

**Verify the work.** `--verify <CMD>` runs a shell command in the run's working directory after each completed agent turn. A non-zero exit, signal, or timeout re-prompts the same live session with the command, status, and output tail, then waits for that session's next turn; `--max-attempts <N>` counts total agent turns and defaults to `3`. The verify command and every re-prompted wait use `--timeout` when set; without it, the command has a five-minute cap, and a timed-out verify is red. Exhausting the attempt cap records `verify_failed` and exits `123`.

```sh
rimz agents codex "Fix the failing auth test." -p --verify "cargo xtask test auth" --max-attempts 3
```

**Retry on failure.** `--retries N` reruns a failed (exit `1`) turn up to `N` more times and appends the previous attempt's captured pane tail to the original prompt in a `<previous-attempt-failure>` block. Timeout and budget caps apply to each attempt; timeouts, budget stops, and cancels stay terminal, and the last attempt decides the command's exit code.

```sh
rimz agents codex "Fix the failing checks." -p --retries 1 --timeout 30m
```

The two compose: verification repairs stay in the same session after a completed turn, while an agent turn that fails with exit `1` starts a fresh-session retry and resets the verify attempt count. Both need a blocking run, so both refuse `--bg` and `--output-format stream-json`; use `text` or `json` output and wait on the run. To verify work you started in the background, wait for it first and run the check on the result.

## A real agent, not a background job

The pane `-p` opens runs the stock CLI exactly as if you had launched it yourself and pasted the prompt, and the room cannot tell the difference. So everything the room does with an agent, it does with a scripted run — who started the turn stops mattering the moment it starts.

**It asks, you answer.** A run that stops on a permission prompt or a real design question takes the room's normal waiting path: the row flips to `? waiting`, the cockpit counts it, a [notification handler](./notifications.md) fires. You answer in the agent's own UI — from the room, or over SSH from your phone — while the script stays blocked on the exit code. A failing migration at 3 a.m. becomes a push notification, a one-line answer, and a green pipeline by morning; the run never had to guess.

**You steer it mid-turn.** The run answers to a handle like any agent, so `rimz message --steer` injects new instructions into a turn a cron job started, and `rimz agents show` or `logs -f` reads its progress from any pane. A drifting unattended run is a one-line correction, not a kill-and-retry.

**It works your harness.** `--worktree` isolates the run's changes on their own branch, a [profile](./fleet.md#profiles-shape-an-agent-for-one-job) or `--model`/`--effort`/`--system-prompt-file` shapes the turn, [`rimz loop`](./loops.md) fires the same path on a clock, and the run messages your interactive agents — and is messaged by them — like any teammate.

Permissions are a per-run choice, rendered through the agent's own flags: `-p` defaults to the provider's auto-accept mode for routine actions, `--ask` keeps every native prompt (each one routes to you as a waiting row), and `--yolo` passes the provider's bypass flag. The tradeoffs are [Loops → the permission posture for unattended runs](./loops.md#the-permission-posture-for-unattended-runs) and [security.md](./security.md).

## Fire now, collect later

For orchestration, decouple starting a run from waiting on it. `--bg` launches the supervised run and prints its agent name, returning immediately; `rimz agents wait` blocks on one or several names whenever you are ready.

```sh
name=$(rimz agents claude "Run the migration audit." -p --bg)   # returns now, prints e.g. swift-otter
# ... kick off other work ...
rimz agents wait "$name" --stream                               # block on it, tailing the transcript
```

- `rimz agents wait <ref>...` blocks until every named run lands; `--stream` tails one run's answer as it lands.
- `rimz agents wait a b c --json` returns one labeled result map `{name: {status, exit, cost, transcript_path}}` after the join settles.
- `rimz agents show <ref>` reports a run's activity, context, and recent transcript.
- `rimz agents stop <ref>` cancels a live run or closes its pane.

A reference is the printed name, a run id, or any [agent address](./messaging.md#address-an-agent), so the same handle threads through `wait`, `show`, `stop`, and `message`. Every flag on these verbs is in the [agent-control reference](../reference/cli/agents.md).

## Agents scripting agents

`rimz agents -p` is a plain shell command, and agents have shell tools — so the caller does not have to be you. `rimz subagents` packages that path for a RimZ-launched agent: a single launch waits for its result by default or returns a petname with `--bg`, while `fanout` launches a JSON task array and joins the whole group by default. Both inherit the caller's checkout. Claude can hand its diff to Codex for a second opinion, Codex can hand a stubborn bug to Claude, and a planner can fan an audit across three runs. The calling agent sees only commands and durable answers, so subagents mix providers freely; pairing model strengths this way is the same economics that makes [teams](./teams.md) work.

```sh
# inside a planner turn: launch three bounded audits, then join all three
rimz subagents fanout <<'JSON'
[
  {"spec":"codex","prompt":"review correctness; report concrete findings"},
  {"spec":"claude","prompt":"review the interface; report concrete findings"},
  {"spec":"codex","prompt":"review test coverage; report concrete findings"}
]
JSON
```

Each child gets its own deadline and durable run record. Pane opens serialize safely, but the audits run in parallel; the command reports one labeled outcome per child and exits nonzero if any child fails. Add `--bg` to scatter without joining, then use `rimz subagents wait` when the parent is ready for the results. The full task schema and timeout precedence are in the [`subagents` reference](../reference/cli/subagents.md#launch-and-fan-out-work).

Compare that with an agent's built-in subagents, which run headless inside the parent's harness. A RimZ-launched child is a first-class member of your room: its own pane, a nested sidebar row, a transcript that outlives the turn, and a question that routes to you instead of failing silently. Use `rimz agents -p` directly when the child needs controls the tailored doorway omits, such as `--stdin` or a separate worktree. When the task belongs to a teammate that is already running, hand it over with [`rimz message`](./messaging.md) instead of spawning a fresh turn.

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

**Fan out, then join.** Start several background runs, each in its own worktree, and collect them by name.

```sh
for pkg in api web worker; do
  rimz agents codex --worktree="audit-$pkg" -p --bg "Audit $pkg for the CVE and reply with the fix." >> runs.txt
done
rimz agents wait $(cat runs.txt)
```

Race providers on isolated worktrees when the first finished result wins. `--any` prints that run's name and leaves the loser running for an explicit stop.

```sh
a=$(rimz agents claude --worktree=race-claude -p --bg "Fix the failing parser test.")
b=$(rimz agents codex --worktree=race-codex -p --bg "Fix the failing parser test.")
winner=$(rimz agents wait "$a" "$b" --any)
if [ "$winner" = "$a" ]; then rimz agents stop "$b"; else rimz agents stop "$a"; fi
```

## Drive the room from a script

The commands you type interactively are the same primitives a script calls, so anything you do at the keyboard runs on a schedule or in CI unchanged.

- `rimz pane capture <ref>` reads a pane's visible text — untrusted terminal output your script matches against bounded patterns before acting on it.
- `rimz pane send <ref>` types literal text and named keys into the agent's own UI, the same explicit input path as `rimz message --steer`.
- `rimz message --steer @<agent> "continue"` is the first-class nudge for wrapper scripts; `rimz message @<agent> --on done "open a PR summary"` hands follow-up work to a running agent at its next turn boundary. The full delivery model is [Messaging](./messaging.md).
- `rimz transcript <ref>` reads back what happened as a timestamped log.

Because these are public CLI, a wrapper composes them freely: read a prompt with `pane capture`, decide with a bounded matcher, answer with `pane send`, and escalate anything it does not recognize by leaving the row `? waiting` for you.

## Prerequisites

Supervised runs need installed and trusted hooks, because hooks are the completion signal — a run with no hooks has no way to know its turn ended. `rimz doctor` confirms the hook state, and [Troubleshooting](./troubleshooting.md) covers a run that never completes. Installing hooks is a one-time consent step, walked in [set up your machine](./setup.md#install-agent-hooks).

## See also

- [Loops and schedules](./loops.md) — put these runs on a clock: schedules, watchdogs, and self-waking agents.
- [Notifications](./notifications.md) — the push routes and handlers that reach you when a run needs an answer.
- [Messaging](./messaging.md) — the `--steer` / `--on done` delivery model wrappers lean on.
- [Agents](./fleet.md) — the profile, handle, and layout vocabulary these examples use.
- [Worktrees](./worktrees.md) — the isolated branches behind `--worktree` runs.
- [Agent control CLI](../reference/cli/agents.md#supervised-runs--p) — every flag on `-p`, `wait`, `show`, `stop`, and `pane`.
- [scripting.md](../internals/harness/scripting.md) — run records, the wakeup socket, streaming, and pane cleanup.
