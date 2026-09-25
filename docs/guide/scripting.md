# Scripting

You already script agents. `claude -p "explain this diff"` in a pipeline, `codex exec` in a Makefile: a prompt goes in, an answer comes out on stdout, and an exit code tells the script what happened. That contract is right, and `rimz agents -p` keeps it. Same flag, same shape, nothing to relearn.

What headless mode gives up is everything around the run. The turn is an invisible process, so when it stalls, wanders, or stops to ask a permission question, the pipeline hangs with nothing to look at and nowhere to type an answer. And every CLI spells the mode its own way, which costs you one wrapper script per provider.

`rimz agents -p` runs the same turn in a real pane in your room, with a card in the sidebar you can watch, answer, and steer while the script blocks. One grammar covers every agent RimZ supports, so swapping the model behind a pipeline is a one-word change.

```sh
rimz agents claude -p "Summarize what changed on this branch."   # the claude -p you know
rimz agents codex -p "Prepare the release checklist."            # the same grammar, any agent
```

Two things have to be true before the first run. RimZ's reporting hooks must be installed and trusted for that agent, because the hook is how a run learns its turn ended; `rimz doctor` reports their state, and [set up your machine](./setup.md#install-agent-hooks) walks the one-time consent. And a Codex run needs a recorded directory-trust decision for the checkout: open `codex` there once and answer its trust prompt. RimZ refuses an unattended launch that would stop at that screen rather than granting the trust for you.

## What a run does on your machine

`-p` adds no engine of its own. It sequences pieces RimZ already has, in this order:

1. It checks the preconditions: the agent's hooks installed and trusted, Codex's directory trust, and the provider's command resolving on your `PATH` after your shell starts. A failure here stops the command before the multiplexer is touched.
2. It checks your dollar caps and the provider account's quota window, then writes a durable run record, a JSON file under `~/.rimz/ws/<workspace-dir>/owned/runs/`.
3. It opens one pane in your Zellij or tmux running the agent's own CLI with your prompt: a split beside you when you run it inside the room, a new tab when the caller is outside it. This is the same launch as an interactive `rimz agents <kind>`.
4. It blocks until the work ends, then prints the answer, exits with the run's code, and closes the pane.

Nothing else moves: no daemon, no forked agent, no RimZ-private copy of the session. The agent CLI writes its own session file where it always does, and `rimz agents show` and `rimz transcript` read the run back from the record after the pane is gone.

Because it is the same launch, the flags you already use still apply. `--worktree` isolates the run's changes on their own branch, a [profile](./fleet.md#profiles-shape-an-agent-for-one-job) or `--model`, `--effort`, and `--system-prompt-file` shape the turn, and [`rimz loop`](./loops.md) fires the same path on a clock.

Ctrl+C on a blocking run cancels it cleanly: exit `130`, the agent stopped, the pane reclaimed. `rimz agents stop <name>` does the same from any other pane. `--keep` leaves the finished pane open for inspection, and the same `rimz agents stop` closes it when you have read enough.

## One run, one exit code

`-p` (`--print`) is the whole contract: run until the work ends, print the answer to stdout, exit with the status.

| Code | Meaning |
| --- | --- |
| `0` | The run completed. |
| `1` | The run failed. |
| `2` | The run never started: RimZ rejected the command line, or a precondition failed. |
| `123` | The run exhausted `--max-attempts` with its `--verify` command still red. |
| `124` | The run hit its `--timeout`. |
| `125` | A dollar cap or a spent provider quota window stopped it. |
| `130` | The run was canceled, by Ctrl+C or `rimz agents stop`. |

A few flag-combination refusals exit `1` rather than `2`, but either way nothing ran, no run record was written, and the reason is on stderr. Caps are the exception to the whole split: a cap exits `125` whether it refused the launch or stopped a turn already under way.

On success, stdout carries the final assistant answer and nothing else, so it pipes cleanly. A failed, timed-out, or canceled run prints its status, the captured pane tail, and the transcript path on stderr, leaving stdout uncontaminated. Branch on the code first, then on the text:

```sh
if answer=$(rimz agents claude -p "Run the migration audit; reply PASS or FAIL."); then
  case "$answer" in
    PASS*) echo "audit clean" ;;
    *)     echo "audit needs a human"; exit 1 ;;
  esac
else
  echo "the run itself failed; see stderr"; exit 1
fi
```

**Cap the wall clock.** `--timeout` bounds the run and turns a wedged turn into exit `124` your wrapper can handle. Kiro needs one: it fires no hook for a cancelled or errored turn, so a `rimz agents kiro -p` run that goes wrong waits out its deadline instead of failing at `1`.

```sh
rimz agents codex -p --timeout 30m "Update dependencies and run the test suite."
```

**Cap the dollars.** `--budget 2` records the run as `budget_exceeded` and exits `125`, distinct from a timeout or an agent failure. A provider subscription whose quota window is already spent exits `125` the same way, before the run record or the pane exists, so a script reads one code for "this run was not affordable" whichever limit stopped it. Both caps are the [budgets guide](./budget.md).

**Cap the turns.** `--max-turns <N>` bounds how many agentic turns the prompt gets, where the agent's CLI has a native limit for it. Claude, Grok, and Qwen have one today; any other agent refuses the flag rather than running unbounded.

Two more flags, `--verify` and `--retries`, decide what counts as done and rerun what failed; they have [their own section below](#verify-and-retry).

## Feed the prompt in

The positional prompt is the base, and `--stdin` appends stdin to it, so build output, a diff, or a log becomes context without a temp file.

```sh
cat build-error.txt | rimz agents claude -p --stdin 'explain the root cause'
git diff --staged | rimz agents codex -p --stdin 'review this diff; reply SHIP or HOLD'
```

`--stdin` reads to EOF. When both a prompt and stdin are present, RimZ puts the prompt first and wraps the stdin bytes in `<stdin>…</stdin>` so the agent reads them as attached material. For a fully programmatic feed, `--input-format stream-json` reads user messages from stdin until EOF. It names stdin as its prompt source, so it refuses both `--stdin` and a positional prompt alongside it.

For a long brief, redirect a file: `rimz agents codex -p --stdin < brief.md`. The complete launch prompt, including attached stdin and any instructions RimZ adds for the agent, must fit within 120 KiB (122,880 bytes). A larger prompt is refused before the run record is written, so keep the brief short and tell the agent to read the larger file itself.

## Shape the output

`--output-format` chooses what `-p` prints, so the same run serves a human, a `jq` filter, or a live UI.

```sh
rimz agents codex -p --output-format json "Prepare the release checklist."
rimz agents claude -p --output-format stream-json "Refactor the parser."
```

| Format | Prints | Use it for |
| --- | --- | --- |
| `text` (default) | the final assistant message | humans, and simple `grep`/`case` gates |
| `json` | the full run record | reading `run_id` (which `rimz transcript <run_id>` opens) and `transcript_path` (the provider's own session file) |
| `stream-json` | run events as newline-delimited JSON while the turn runs | a pipeline that wants progress rather than a final blob |

## Verify and retry

An exit code tells you the turn ended. These two flags let the run prove the work and repair itself before your script ever sees a failure.

**Verify the work.** `--verify <CMD>` runs a shell command in the run's working directory after each completed agent turn. A non-zero exit, a signal, or a timeout re-prompts the same live session with the command, its status, and an output tail, then waits for that session's next turn. `--max-attempts <N>` counts total agent turns and defaults to `3`; exhausting it records `verify_failed` and exits `123`. The verify command runs under `--timeout` when you set one and under a five-minute cap otherwise, and a timed-out verify counts as red.

```sh
rimz agents codex -p --verify "cargo xtask test auth" --max-attempts 3 "Fix the failing auth test."
```

**Retry on failure.** `--retries N` reruns a failed (exit `1`) turn up to `N` more times in a fresh session, appending the previous attempt's captured pane tail to the original prompt inside a `<previous-attempt-failure>` block. Timeout and budget caps apply to each attempt; timeouts, budget stops, and cancels stay terminal, and the last attempt decides the command's exit code.

```sh
rimz agents codex -p --retries 1 --timeout 30m "Fix the failing checks."
```

The two compose. A verification repair stays in the same session after a completed turn, while a turn that fails with exit `1` starts a fresh-session retry and resets the verify attempt count. Both need a blocking run, so both refuse `--bg` and `--output-format stream-json`; use `text` or `json` output. To verify work you started in the background, join it first and run the check on the result.

## Answer and steer a run in flight

The pane `-p` opens runs the agent's own CLI in your room, as a fleet member with a card and a handle. So everything the room does with an agent, it does with a scripted run, and who started the turn stops mattering the moment it starts.

The permission mode decides how often the run stops to ask, and it is a per-run choice rendered into the agent's own flags. `-p` defaults to the provider's automatic-approval mode for routine actions, `--ask` keeps the native prompts so each one reaches you as a waiting row, and `--yolo` passes the bypass flag. An agent whose CLI has no flag for a mode launches unchanged: Pi and Amp take neither switch. The tradeoffs for an unattended run are in [Loops](./loops.md#permissions-for-an-unattended-run) and [Security](./security.md).

**It asks, you answer.** A run that stops on a permission prompt or a real design question takes the room's normal waiting path: the row flips to `? waiting`, the cockpit counts it, a [notification handler](./notifications.md) fires. You answer in the agent's own UI, from the room or over SSH from your phone, while the script stays blocked on the exit code. A failing migration at 3 a.m. becomes a push notification, a one-line answer, and a green pipeline by morning; the run never had to guess.

**You steer it mid-turn.** The run answers to a handle like any agent, so `rimz message --steer @<name>` injects new instructions into a turn a cron job started, and `rimz agents show` or `rimz agents logs -f` reads its progress from any pane. A drifting unattended run is a one-line correction rather than a kill and a rerun.

**It can wait without finishing.** An agent can arm a [`rimz wait`](./loops.md#wake-a-running-agent) or launch children of its own and end its turn while they run. The run stays open, its pane stays alive, and your script keeps blocking; `rimz agents show` reports how long it has been parked. If nothing is left that could wake it, the run fails with exit `1` and a reason beginning `parked on a wake that never arrived`, which `--retries` can retry. `--timeout` bounds the run either way, parked time included.

## Fire now, collect later

For orchestration, split starting a run from waiting on it. `--bg` launches the run, prints its agent name on stdout, and returns immediately; `rimz agents wait` blocks on one or several names whenever you are ready.

```sh
name=$(rimz agents claude -p --bg "Run the migration audit.")   # returns now, prints e.g. swift-otter
# ... kick off other work ...
rimz agents wait "$name" --stream                               # block on it, tailing the transcript
```

A `<ref>` below is the printed name, a run id, or any [agent address](./messaging.md#address-an-agent), so one identity threads through every verb here; `rimz message` wants it as a handle, `@swift-otter`.

- `rimz agents wait <ref>...` blocks until every named run lands, printing each answer under a `--- <name> ---` header. `--stream` tails one run's answer as it lands, and `--any` returns as soon as the first one finishes.
- `rimz agents wait a b c --json` prints one result map, `{name: {status, exit, cost, transcript_path, last_message}}`, once the join settles.
- `rimz agents show <ref>` reports a run's activity, context, and recent transcript.
- `rimz agents stop <ref>` cancels a live run or closes its pane.

`wait --timeout` stops the waiting, not the run. It exits `124` with the run still going, which is the code a timed-out run exits with too, so a script that has to tell the two apart rereads the record with `rimz agents show`. Ending the run itself takes `rimz agents stop`. Every flag on these verbs is in the [agent control reference](../reference/cli/agents.md#supervised-runs--p).

## In a pipeline

**Refresh dependencies overnight from cron.** The worktree isolates the work; the exit code gates the notification.

```sh
# 02:00 nightly
rimz agents codex -p --worktree=deps --timeout 4h \
  "Update dependencies, run the full test suite, and open a PR. Stop and ask if a major version bumps." \
  || notify-send "deps run needs a human"
```

**Gate a CI job on SHIP or HOLD.** The turn joins whatever room the runner attaches to, so a design question still reaches a human.

```sh
verdict=$(rimz agents claude -p "Review the canary metrics in ./metrics.json and reply SHIP or HOLD") || exit 1
case "$verdict" in
  SHIP*) exit 0 ;;
  *)     echo "held: $verdict" >&2; exit 1 ;;
esac
```

**Fan out, then join.** Start several background runs, each in its own worktree, and collect them by name.

```sh
for pkg in api web worker; do
  rimz agents codex -p --bg --worktree="audit-$pkg" "Audit $pkg for the CVE and reply with the fix." >> runs.txt
done
rimz agents wait $(cat runs.txt)
```

**Race two providers.** Give each one its own worktree and let whichever finishes first win. `--any` prints the winner's answer under its header and leaves the loser running, so read the name from the JSON map, which holds only the run that won.

```sh
a=$(rimz agents claude -p --bg --worktree=race-claude "Fix the failing parser test.")
b=$(rimz agents codex  -p --bg --worktree=race-codex  "Fix the failing parser test.")
winner=$(rimz agents wait "$a" "$b" --any --json | jq -r 'keys[0]')
if [ "$winner" = "$a" ]; then rimz agents stop "$b"; else rimz agents stop "$a"; fi
```

## Drive the room from a script

A wrapper sometimes has to read or nudge a pane itself, and the commands that do it are the ones you already type by hand.

- `rimz pane capture <ref>` reads a pane's visible text. Treat it as untrusted: match it against patterns your script controls, never act on whatever the terminal happened to print.
- `rimz pane send <ref>` types literal text and named keys into the agent's own UI as a keyboard would, without the bracketed-paste wrapper a terminal paste adds, and it waits for any in-flight `rimz message` write to the same pane ([send text and keys](../reference/cli/pane.md#send-text-and-keys)).
- `rimz message --steer @<name> "continue"` is the first-class nudge for wrapper scripts, and a plain `rimz message @<name> "open a PR summary"` hands follow-up work to a running agent at its next turn boundary. The full delivery model is [Messaging](./messaging.md).
- `rimz transcript <ref>` reads back what happened as a timestamped log.

Because these are public CLI, a wrapper composes them freely: read a prompt with `pane capture`, decide with a bounded matcher, answer with `pane send`, and escalate anything it does not recognize by leaving the row `? waiting` for you.

A few of these commands, `rimz message --wait` and `rimz providers` among them, animate a status line while they work. It only ever reaches an interactive stderr terminal, so a redirected or piped stderr never sees it; `RIMZ_NO_PROGRESS=1` turns it off everywhere.

## Agents scripting agents

`rimz agents -p` is a plain shell command and agents have shell tools, so the caller does not have to be you. An agent can start a run exactly as your script does, and `rimz subagents` packages that path for delegation: one command launches a child that runs in its own pane under the parent's card, and the parent gets one report once its children settle. Setting up the children an agent may launch, watching and stopping them, and choosing between a child, a peer run, and a team are the [subagents guide](./subagents.md).

## See also

- [Loops and schedules](./loops.md): put these runs on a clock: schedules, watchdogs, and self-waking agents.
- [Notifications](./notifications.md): the push routes and handlers that reach you when a run needs an answer.
- [Messaging](./messaging.md): the steer and park delivery model wrapper scripts lean on.
- [Agents](./fleet.md): the profile, handle, and layout vocabulary these examples use.
- [Worktrees](./worktrees.md): the isolated branches behind `--worktree` runs.
- [Agent control CLI](../reference/cli/agents.md#supervised-runs--p): every flag on `-p`, `wait`, `show`, and `stop`.
- [Subagents](./subagents.md): let your agents delegate to supervised children, and watch and stop them.
- [Supervised runs (internals)](../internals/harness/scripting.md): run records, the wake socket, streaming, and pane cleanup.
