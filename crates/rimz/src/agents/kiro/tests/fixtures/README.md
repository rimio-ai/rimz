# Kiro CLI 2.12.1 transcript evidence

The stock interactive captures launched `kiro-cli chat --v3` in disposable directories. `stock_ping/` preserves a ping/pong turn, context percentage, credit-only usage, and the physically late `session_start`; `stock_approval/` preserves native approval, `fs_write`, resolution, and settlement ordering.

The UI reported a root session identity shaped as `sess_<uuid>`. Kiro CLI's stock structured store pairs `session.json` and `messages.jsonl` under the workspace-hash bucket. The older `root/*.history` fixture remains exclusion evidence: readline history alone carries no assistant, lifecycle, context, or tool result.

The attempted hook files produced no command invocation or stdin payload. Pulled store evidence therefore supports transcript and live display without claiming executable hook coverage or structured Ask/Answer routing.

The paired UUID-only JSON and JSONL samples came from an ACP-hosted non-interactive session whose metadata says `session_created_reason: "subagent"`. They demonstrate a distinct session class and do not define the stock interactive adapter contract.

Redaction replaces session IDs, message IDs, cwd, prompt text, reply text, tool arguments, and timestamps while preserving the observed field names, JSON types, record order, and root-versus-ACP identity shapes. The fixtures contain no account, email, token, or authenticated home-directory data.
