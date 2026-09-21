# Asks and answers

An ask is a blocking prompt that owns an agent's input: a permission prompt, a plan approval, or a question. `rimz asks` reads the open asks as structured data, and `rimz answer` answers one by typing into the agent's own prompt UI, the same keys you would press in the pane. Every agent whose questions reach RimZ shows up in `rimz asks`; `rimz answer` works for Claude, Codex, and Pi, within the limits in [What each agent accepts](#what-each-agent-accepts). The workflow is taught in [the messaging guide](../../guide/messaging.md#asks-and-answers), and which agents surface their asks at all is in [agent support](../agent-support.md).

## List open asks

```sh
rimz asks                 # open asks in the current channel
rimz asks --all           # every channel
rimz asks --json          # the same rows as JSON
```

`rimz asks` and `rimz asks list` are the same command and take the same flags.

| Flag | Effect |
| --- | --- |
| `--all` | Include asks from every channel. Outside a channel the list already covers every channel. |
| `--json` | Print a JSON array of [ask rows](#ask-json), `[]` when nothing is open. |

The table has the columns `ASK`, `AGENT`, `KIND`, `AGE`, and `QUESTION`, oldest ask first. `KIND` reads `permission`, `plan approval`, or `question`, and `QUESTION` is the first line of the first question. With no open asks the command prints `no agent in #<channel> is asking anything — rimz asks --all shows every channel` and exits `0`; where the list already covers every channel (with `--all`, or outside a channel) the line is `no agent is asking anything`.

A provider subagent (a child the agent spawned inside its own session) that raises a prompt appears as `child-name (via @root)`, because the prompt is drawn in the root agent's pane. In JSON, `agent.handle` is the root's address and `agent.name` is the child. A child whose root agent is no longer live is left out of the list. Target a child's ask by its `ask_id`.

## Show one ask

```sh
rimz asks show @planner
rimz asks show ask_0123456789abcdef --json
```

The target is an agent address or an ask id. An address resolves in the current channel, as everywhere else ([addressing agents](./agents.md#addressing-agents)); an ask id matches in any channel.

The human form prints a header line, the agent's context message, and each question with its numbered options:

```console
$ rimz asks show @planner
ask_0123456789abcdef  @planner#auth  question · 4m

▌ A staged rollout limits the blast radius.

Choose a rollout
  1. safe
     Stage the rollout
  2. fast
```

The context lines render as Markdown under the [agent prose](../cli.md#agent-prose) rule. Questions are numbered only when an ask has more than one, and an option with a caution prints it after the label, as in `1. approve [caution: enables auto-accept for subsequent edits]`.

`show` exits `1` when the address resolves to an agent that is not asking, when the ask id is no longer open, or when a subagent's root agent is gone.

## Ask JSON

`rimz asks --json` prints an array of rows and `rimz asks show --json` prints one:

```json
{
  "ask_id": "ask_0123456789abcdef",
  "agent": { "handle": "@planner#auth", "kind": "claude", "channel": "auth" },
  "kind": "question",
  "since": "2026-07-09T12:00:00Z",
  "detail": "Choose a rollout",
  "context": "A staged rollout limits the blast radius.",
  "questions": [
    {
      "question": "Choose a rollout",
      "options": [
        { "label": "safe", "description": "Stage the rollout", "mutates_trust": false, "caution": null },
        { "label": "fast", "description": null, "mutates_trust": false, "caution": null }
      ],
      "multi_select": false
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `ask_id` | The ask's id. Pass it to `answer` to answer this prompt and no later one. |
| `agent.handle` | The agent's address; for a provider subagent, the root agent's address. |
| `agent.name` | The subagent's name. Present only on a provider subagent's ask. |
| `agent.kind` | The agent kind, such as `claude`. |
| `agent.channel` | The agent's channel. Absent when it has none. |
| `kind` | `permission`, `plan_approval`, or `question`. |
| `since` | When the prompt opened. |
| `detail` | A one-line summary of the prompt, such as a permission ask's tool call. `null` when the agent recorded none. |
| `context` | The message the agent wrote just before raising the prompt. Absent when it wrote none, and always absent on permission asks. |
| `questions[].question` | The question text. |
| `questions[].options[].label` | The option label, which `answer` accepts as a selector. |
| `questions[].options[].description` | The option's description, or `null`. |
| `questions[].options[].mutates_trust` | `true` exactly when `caution` is set. |
| `questions[].options[].caution` | What choosing the option changes beyond this prompt, such as Claude's `approve` turning on auto-accept edits. `null` otherwise. |
| `questions[].multi_select` | Whether the question takes several options. |

Permission and plan asks carry one question whose options are only the actions `answer` can deliver for that agent. A Codex permission ask carries no options, because none can be answered from outside the pane.

## Answer an ask

```sh
rimz answer @planner 2
rimz answer @planner safe
rimz answer @planner 1,3
rimz answer @planner --text "Use the staged route"
rimz answer ask_0123456789abcdef --json answers.json
rimz answer ask_0123456789abcdef --json < answers.json
```

The target is an agent address or an ask id, resolved as for [`show`](#show-one-ask). One command answers the whole ask, every question at once.

| Flag | Effect |
| --- | --- |
| `SELECTORS...` | One selector per question, in order. |
| `--text <TEXT>` | A free-text answer. Only for an ask with exactly one question, and not together with selectors. |
| `--json [FILE]` | Read [structured answers](#structured-answers) from `FILE`, or from stdin when `FILE` is omitted. Not together with selectors or `--text`. |
| `--wait <DURATION>` | How long to wait for the agent to confirm. Default `30s`; takes `s`, `m`, or `h`. |
| `--no-wait` | Return once the keys are sent, without waiting for confirmation. |

A selector is a one-based option number or an option label. Labels match case-insensitively (ASCII only), and a selector made only of digits is always read as a number. Separate several options with commas (`1,3`) on a multi-select question. The number of selectors must equal the number of questions, and an option may be chosen only once.

### Structured answers

Structured input is a JSON array with one object per question, in order. Use it for an ask with several questions that needs free text, or to combine picks and text on one question:

```json
[
  { "pick": ["safe"] },
  { "pick": [1, 3] },
  { "text": "Notify the release channel" }
]
```

`pick` takes option numbers or labels, and `text` takes free text. An object needs at least one of them, and unknown keys are ignored. Whether an agent accepts `text`, or `pick` and `text` together, is in [What each agent accepts](#what-each-agent-accepts).

### How an answer is delivered

`answer` checks everything it can before it touches the pane, then sends the whole answer as one uninterrupted write:

1. Validate every answer against the ask's questions and the agent's supported actions. A failure exits `3` and sends nothing.
2. Wait up to 30 seconds for any other RimZ write to the same pane (a message, another answer) to finish, then hold the pane until the answer is sent. `--no-wait` does not skip this wait.
3. Check that the ask read in step 1 is still the agent's open ask. A prompt answered or replaced in the meantime gets no keys, and the command exits `2`. This check runs for an address target too.
4. Send the keys. A provider subagent's answer goes to its root agent's pane.
5. Wait for confirmation: the ask closes, or the agent records an answer for it. `--no-wait` stops before this step.

On success `answer` prints `answered <ask_id> for <handle>`, or `sent answer for <ask_id> to <handle>` with `--no-wait`. Errors print `error: <message>` on stderr. An ask that closes during step 5 for any reason, including being dismissed in the pane, counts as confirmed.

### Exit codes

`answer` exits with its own codes, so a script can tell a stale ask from a rejected answer:

| Code | Meaning |
| --- | --- |
| `0` | The agent confirmed the answer, or `--no-wait` sent it. |
| `1` | No room, or the store could not be read or written. |
| `2` | The target is not an agent or not asking, the ask is no longer current, the agent has no live pane, the pane stayed busy for 30 seconds, or sending failed. A send that fails partway may leave some keys in the pane. An invalid command line (such as `--wait 5d`) also exits `2`. |
| `3` | The answer is invalid for the ask, the `--json` input cannot be read or parsed, or the agent does not support the answer. |
| `4` | The keys were sent, but the agent did not confirm before the `--wait` deadline. |

`rimz asks` follows the [CLI-wide exit codes](../cli.md#exit-codes).

## What each agent accepts

`rimz answer` answers only what an agent's prompt UI lets it drive and confirm. Every other kind exits `3` with `<kind> does not support structured answers`.

| Agent | Question | Permission | Plan approval |
| --- | --- | --- | --- |
| Claude | Single picks, multi-select picks, and free text. Picks and text together only on a multi-select question, through `--json`. | `allow`, which approves the current tool call once. | `approve`, which approves the plan and turns on auto-accept edits. |
| Codex | One pick per question. Multi-select questions, free text, and `Other` or `None of the above` options are refused. | Refused. | `implement`, which picks "Yes, implement this plan" and switches Codex from Plan mode to Default mode. |
| Pi | Single picks, multi-select picks, and free text, on every question shape. Picks and text together are refused. | Not raised. | Not raised. |

Pi asks come from the [`@juicesharp/rpiv-ask-user-question`](https://github.com/juicesharp/rpiv-mono/tree/v2.7.1/packages/rpiv-ask-user-question) extension's questionnaire; Pi raises no permission or plan prompts of its own. Cancel and "Chat about this" stay in the Pi pane.

Every other action stays in the agent's pane: denying a permission, a persistent grant, keep-planning, refinement text, and Claude's manual-review approval. Dismissing a Claude prompt with Escape emits no lifecycle event, so RimZ could not confirm a deny or keep-planning it sent. A refused action exits `3` and names the pane as the place to take it.

You can always answer in the pane instead. When you dismiss a question or plan approval there and type a new prompt, RimZ records that prompt as the ask's answer.
