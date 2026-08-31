# Asks and answers

`rimz asks` reads the blocking prompt that currently owns an agent's input, and `rimz answer` answers a supported prompt through the agent's native terminal interface.

## List open asks

```sh
rimz asks
rimz asks --json
rimz asks list --all
rimz asks show @planner --json
rimz asks show ask_0123456789abcdef
```

The default list follows the current channel. `--all` includes every channel. `show` accepts the current ask id or one agent address.

A provider subagent waiting on a prompt appears as `child-name (via @root)`. Its JSON `agent.handle` is the root pane address and the additive `agent.name` identifies the child. Show or answer that child ask by `ask_id`; RimZ sends the answer to the root pane where the provider surfaced the prompt.

JSON rows have this shape:

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

`context` is the agent's explanation message written just before it raised the prompt. `show` renders it above the questions using the shared [agent-prose rendering rule](../cli.md#agent-prose), and the field is absent when the agent wrote no explanation, including permission asks.

Permission asks expose only `allow`, which approves the current tool call once. Plan approvals expose only `approve`, whose caution reports that it enables auto-accept for subsequent edits. The JSON options contain only those deliverable actions; denial, persistent grants, keep-planning, refinement text, and manual-review approval remain in the Claude pane.

## Answer an ask

```sh
rimz answer @planner 2
rimz answer @planner safe
rimz answer @planner 1,3
rimz answer @planner --text "Use the staged route"
rimz answer ask_0123456789abcdef --json answers.json
rimz answer ask_0123456789abcdef --json < answers.json
```

One positional selector answers each question in order. A comma selects several options on a multi-select question. Selectors accept one-based indices or case-insensitive full labels. `--text` applies only to a single question; use structured JSON to mix answer forms across several questions.

Structured input is one object per question:

```json
[
  { "pick": ["safe"] },
  { "pick": [1, 3] },
  { "text": "Notify the release channel" }
]
```

RimZ validates every answer before sending a keystroke. An ask-id target also acts as a compare-and-swap token: a prompt answered or superseded in the pane is stale and receives no input. Unsupported permission and plan actions exit `3` before pane delivery, list the valid remote option, and direct you to the Claude pane.

Confirmation waits 30 seconds by default. `--wait 5m` changes the deadline; `--no-wait` returns after the pane write. Exit `0` means confirmed or intentionally not waited, `2` means the ask was stale or its pane unavailable, `3` means validation or adapter capability failed, and `4` means the agent did not confirm before the deadline.

Claude questions support single picks, multi-select picks, and free text. Permission `allow` uses the stable first menu action. Plan `approve` uses Shift-Tab and enables auto-accept edits. If you Escape a question and submit free text in the pane, RimZ records that next human prompt as the open question's answer. The Escape-based deny and keep-planning actions stay pane-only because Claude Code 2.1.205 emits no lifecycle confirmation for them.

Pi questions from `@juicesharp/rpiv-ask-user-question` support single picks, multi-select picks, and free text. Preview-carrying and multi-select questions suppress the native free-text row; cancel and Chat about this stay in the pi pane.
