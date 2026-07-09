# Asks and answers

`rimz asks` reads the blocking prompt that currently owns an agent's input, and `rimz answer` answers a supported question through the agent's native terminal interface.

## List open asks

```sh
rimz asks
rimz asks --json
rimz asks list --all
rimz asks show @planner --json
rimz asks show ask_0123456789abcdef
```

The default list follows the current channel. `--all` includes every channel. `show` accepts the current ask id or one agent address.

JSON rows have this shape:

```json
{
  "ask_id": "ask_0123456789abcdef",
  "agent": { "handle": "@planner#auth", "kind": "claude", "channel": "auth" },
  "kind": "question",
  "since": "2026-07-09T12:00:00Z",
  "detail": "Choose a rollout",
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

Permission asks expose the semantic choices `allow` and `deny` for presentation, and plan approvals expose `approve` and `keep-planning` with the auto-accept caution. Both kinds are read-only in Rimz and stay answerable in the Claude pane; their Escape rejection paths release the TUI but emit no lifecycle confirmation, so a remote answer could not meet the durable confirmation contract.

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

Rimz validates every question answer before sending a keystroke. An ask-id target also acts as a compare-and-swap token: a prompt answered or superseded in the pane is stale and receives no input. Claude permission and plan-approval targets exit `3` before pane delivery and direct you to the Claude pane.

Confirmation waits 30 seconds by default. `--wait 5m` changes the deadline; `--no-wait` returns after the pane write. Exit `0` means confirmed or intentionally not waited, `2` means the ask was stale or its pane unavailable, `3` means validation or adapter capability failed, and `4` means the agent did not confirm before the deadline.

Claude questions support single picks, multi-select picks, and free text. Permission and plan-approval menus remain pane-only, including one-time approval, rejection, persistent grants, auto-accept approval, and refinement feedback.
