# Antigravity adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The pinned upstream surface and live 1.1.1 evidence are in [antigravity-reference.md](../../externals/agent-adapter/antigravity-reference.md).

Antigravity support targets `agy` 1.1.1. RimZ owns stock interactive launch, permission-mode flags, model presets, exact conversation resume, process identity, validated local-session discovery, basic text-turn state, transcript history, and assistant streaming.

## Launch, resume, and presence

Fresh sessions run `agy`; a startup prompt uses `agy --prompt-interactive <prompt>`. Profiles map `model` to `--model`; Antigravity publishes no reasoning-effort or system-prompt launch flag, so those profile fields fail before launch. Permission postures map `ask` to the native default, `auto` to `--mode accept-edits`, `plan` to `--mode plan`, and `yolo` to `--dangerously-skip-permissions`.

Exact resume runs `agy --conversation <conversation-id>`, and both split and joined flag forms are recognized on the live process command line. `-c` and `--continue` remain workspace-latest conveniences and do not claim an exact identity. Native `/fork` changes identity inside the current TUI and has no source-ID launch flag, so `rimz agents fork` remains unsupported.

The process matcher recognizes `agy`; it leaves desktop processes named `antigravity` outside the CLI adapter.

## Local store and transcript

Pulled discovery reads `${RIMZ_ANTIGRAVITY_HOME:-$HOME/.gemini/antigravity-cli}`. The test-only override keeps fixtures isolated; stock use reads the provider path. A transcript is accepted only as a regular file at a direct, non-symlink `brain/<conversation-id>/.system_generated/logs/transcript.jsonl` path with a safe opaque path component.

`cache/last_conversations.json` authorizes fresh-session pairing only for the exact absolute pane cwd. Every validated transcript remains a candidate for an exact `--conversation` command-line match, so resuming an older conversation does not depend on the latest-workspace cache. At most the 512 newest local conversations enter one snapshot discovery pass; ambiguous fresh candidates stay process rows.

The 1.1.1 fixture verifies six-field JSONL records: `step_index`, `source`, `type`, `status`, `created_at`, and optional `content`. RimZ maps only the two visible shapes observed in a stock root conversation:

- `USER_EXPLICIT` / `USER_INPUT` becomes user text and running/reasoning.
- `MODEL` / `PLANNER_RESPONSE` becomes assistant text; `status: DONE` settles success.
- `SYSTEM` conversation-history/checkpoint records, malformed lines, unknown sources, and unknown types stay out of visible history.

Reads preserve physical order, tolerate unknown complete records, and retain a torn final JSONL record until it becomes complete. Discovery folds a bounded tail; full history and incremental assistant output use the adapter-normalized transcript path.

This is deliberately partial live truth. The captured text records do not prove permission/question/artifact waits, tool completion, failures, cancellation, background work, compaction, child identity, model, context, quota, or cost.

## Hooks and rich state

Antigravity command hooks put policy decisions on stdout. The documented `PreToolUse` results all affect policy, and 1.1.1 does not publish behavior-preserving observer bytes. RimZ therefore installs no hooks and keeps manual feeds silent. It also leaves the custom statusline untouched until initial callback timing, transition payloads, prior-command wrapping, and uninstall restoration are captured.

`rimz agents antigravity -p` fails before pane or run-record creation because pulled files do not provide the executable completion channel supervised runs require. Native ask routing, structured answers, rich context, spend, quota, compaction, subagent rows, and remote control remain unsupported until their typed live fixtures land.
