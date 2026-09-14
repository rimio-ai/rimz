# Kiro CLI 2.12.1 transcript evidence

The stock interactive captures launched `kiro-cli chat --v3` in disposable directories. `stock_empty/` preserves the statusless metadata and zero-byte transcript written before the first prompt; `stock_ping/` preserves a ping/pong turn, context percentage, credit-only usage, and the physically late `session_start`; `stock_approval/` preserves native approval, `fs_write`, resolution, and settlement ordering.

The UI reported a root session identity shaped as `sess_<uuid>`. Kiro CLI's stock structured store pairs `session.json` and `messages.jsonl` under the workspace-hash bucket. The older `root/*.history` fixture remains exclusion evidence: readline history alone carries no assistant, lifecycle, context, or tool result.

`stock_shell_2_21_4/` is a Kiro CLI 2.21.4 capture of the same launch: two turns, the second approving one `execute_bash` call. It preserves the `fetch_cloud_config` tool call written before `turn_start`, the `pending_interaction` and `interaction_resolved` pair, the tool call recorded as `completed` rather than `approved`, and `in_progress` metadata status.

Kiro CLI 2.12.1 ran none of the attempted hook files. On 2.21.4 the same captures fired workspace and global hooks, and the hook samples in the adapter catalog follow those payloads, with the session id redacted. No hook reports a pending approval or question, so the store remains the evidence for waiting cards.

The paired UUID-only JSON and JSONL samples came from an ACP-hosted non-interactive session whose metadata says `session_created_reason: "subagent"`. They demonstrate a distinct session class and do not define the stock interactive adapter contract.

Redaction replaces session IDs, message IDs, cwd, prompt text, reply text, tool arguments, and timestamps while preserving the observed field names, JSON types, record order, and root-versus-ACP identity shapes. The fixtures contain no account, email, token, or authenticated home-directory data.
