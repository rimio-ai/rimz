# Copilot CLI 1.0.70 fixtures

`events.jsonl` comes from a clean prompt-mode turn with installed hooks, plus observed system and hook noise. `otel.jsonl` comes from the same metadata-only scenario with `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false`.

`otel-interleaved.jsonl` minimizes a shared-file capture from two simultaneous direct Copilot processes. Three overlapping turns per session produced 84 complete JSON lines (93,721 bytes) with no truncation or interleaving; the last 64 KiB retained two complete `chat` spans for each exact conversation ID. The fixture keeps the captured record/resource shape and adds sanitized duplicate/out-of-order timestamps, malformed noise, and a torn suffix to pin selection and recovery behavior.

The 1.0.70 exporter wrote the completed `chat` span before the `agentStop` hook returned (2,743 bytes visible at both `agentStop` and `sessionEnd`) and appended `invoke_agent` plus metrics during shutdown (15,357 bytes at process exit). The stat-gated reader therefore sees the usable span at stop and sees any later exporter flush on Tick/Watch without another turn. Input tokens include the cache-read slice; the reader preserves fresh input as the saturating `input - cache_read` difference.

Captured `github.copilot.cost` was a finite per-chat `0.0`, while `invoke_agent` carried no session-cumulative dollar. That does not prove a positive live-session dollar mapping, so Copilot realtime and historical spend remain unsupported. A separate statusline probe bound `session_id` and reported `current_context_tokens`, `displayed_context_limit`, and `current_context_used_percentage` after a turn, but the nominal `context_window_size` and `used_percentage` fields stayed null and the wrapper/concurrency matrix is incomplete; RimZ therefore leaves the user statusline untouched and keeps context-window fill unsupported.

Prompts, replies, paths, repositories, session/request/interaction IDs, encrypted fields, hook bodies, tool definitions, and provider-generated identifiers are replaced. The malformed, unknown, and torn records are synthetic additions after the captured structures.
