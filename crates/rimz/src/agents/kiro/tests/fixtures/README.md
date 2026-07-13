# Kiro CLI 2.12.1 negative transcript evidence

The stock interactive capture launched `kiro-cli chat --v3` in a disposable directory and completed two turns, including one approved file write.

The UI reported a root session identity shaped as `sess_<uuid>`. The session directory contained only the matching `.history` file after the observation; its lines contain submitted prompt and slash-command text, with no assistant reply, timestamp, model, context, usage, or tool-result fields.

The attempted user-level canonical hook file, an auxiliary user hook file, a replacement canonical command, and a project `.kiro/hooks` file produced no command invocation or stdin payload. These fixtures make no claim about native hook keys, event ordering, history append-versus-rewrite timing, or resume persistence.

The paired UUID-only JSON and JSONL samples came from an ACP-hosted non-interactive session whose metadata says `session_created_reason: "subagent"`. They demonstrate a distinct session class and do not define the stock interactive adapter contract.

Redaction replaces session IDs, message IDs, cwd, prompt text, reply text, tool arguments, and timestamps while preserving the observed field names, JSON types, record order, and root-versus-ACP identity shapes. The fixtures contain no account, email, token, or authenticated home-directory data.
