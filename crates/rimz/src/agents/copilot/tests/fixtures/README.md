# Copilot CLI 1.0.70 fixtures

`events.jsonl` comes from a clean prompt-mode turn with installed hooks, plus observed system and hook noise. `otel.jsonl` comes from the same metadata-only scenario with `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false`.

Prompts, replies, paths, repositories, session/request/interaction IDs, encrypted fields, hook bodies, tool definitions, and provider-generated identifiers are replaced. The malformed, unknown, and torn records are synthetic additions after the captured structures.

